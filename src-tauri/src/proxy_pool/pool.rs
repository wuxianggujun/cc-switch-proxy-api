//! 节点池与粘性路由
//!
//! 内存态权威副本，写操作同时标记脏数据供 dao 落盘。
//! 选路策略：先查租约（粘性），未命中或节点不可用则 P2C 按延迟加权选新节点。

use super::types::{now_ms, Lease, NodeHealth, NodeView, PoolConfig, PoolStats, ProxyNode};
use std::collections::{HashMap, HashSet};

/// 池中一个节点的完整运行时状态
#[derive(Debug, Clone)]
struct PoolEntry {
    node: ProxyNode,
    health: NodeHealth,
    /// 引用该节点的订阅集合。为空时节点可被清理。
    subscription_ids: HashSet<String>,
}

impl PoolEntry {
    /// 可被选中：未熔断，或已过冷却期（放行试探）
    fn is_selectable(&self, config: &PoolConfig, now: i64) -> bool {
        if !self.health.is_circuit_open() {
            return true;
        }
        self.health
            .is_cooled_down(config.circuit_cooldown_secs as i64 * 1000, now)
    }

    /// 严格健康：未熔断。用于统计展示，不含试探态。
    fn is_healthy(&self) -> bool {
        !self.health.is_circuit_open()
    }
}

/// 选路结果
#[derive(Debug, Clone)]
pub struct Selection {
    pub node: ProxyNode,
    /// true 表示复用了已有租约
    pub from_lease: bool,
}

#[derive(Debug, Default)]
pub struct NodePool {
    entries: HashMap<String, PoolEntry>,
    /// sticky_key -> 租约
    leases: HashMap<String, Lease>,
    config: PoolConfig,
    /// 自增计数器，替代随机数做 P2C 采样，避免引入 rand 依赖且便于测试
    cursor: u64,
}

impl NodePool {
    pub fn new(config: PoolConfig) -> Self {
        Self {
            entries: HashMap::new(),
            leases: HashMap::new(),
            config,
            cursor: 0,
        }
    }

    pub fn config(&self) -> &PoolConfig {
        &self.config
    }

    pub fn set_config(&mut self, config: PoolConfig) {
        self.config = config;
    }

    /// 用订阅刷新出的节点集替换该订阅的贡献。
    ///
    /// 保留已有节点的健康状态（跨订阅去重 + 重启恢复都靠这个），
    /// 并解除本订阅对已消失节点的引用。返回被彻底移除的节点 hash。
    pub fn sync_subscription(&mut self, subscription_id: &str, nodes: Vec<ProxyNode>) -> Vec<String> {
        let incoming: HashSet<String> = nodes.iter().map(|n| n.hash.clone()).collect();

        for node in nodes {
            let entry = self.entries.entry(node.hash.clone()).or_insert_with(|| PoolEntry {
                node: node.clone(),
                health: NodeHealth::default(),
                subscription_ids: HashSet::new(),
            });
            // 已存在则只更新可变元数据（tag 可能改名），健康状态不动
            entry.node.tag = node.tag;
            entry.subscription_ids.insert(subscription_id.to_string());
        }

        // 解除本订阅对不再包含的节点的引用
        let mut removed = Vec::new();
        for (hash, entry) in self.entries.iter_mut() {
            if !incoming.contains(hash) {
                entry.subscription_ids.remove(subscription_id);
            }
        }
        self.entries.retain(|hash, entry| {
            let keep = !entry.subscription_ids.is_empty();
            if !keep {
                removed.push(hash.clone());
            }
            keep
        });

        // 节点消失后其租约失效
        for hash in &removed {
            self.leases.retain(|_, lease| &lease.node_hash != hash);
        }

        removed
    }

    /// 移除订阅时清掉它的全部贡献
    pub fn remove_subscription(&mut self, subscription_id: &str) -> Vec<String> {
        self.sync_subscription(subscription_id, Vec::new())
    }

    /// 直接载入节点与健康状态，用于启动时从 DB 恢复
    pub fn load_entry(
        &mut self,
        node: ProxyNode,
        health: NodeHealth,
        subscription_ids: Vec<String>,
    ) {
        self.entries.insert(
            node.hash.clone(),
            PoolEntry {
                node,
                health,
                subscription_ids: subscription_ids.into_iter().collect(),
            },
        );
    }

    pub fn load_lease(&mut self, lease: Lease) {
        // 节点已不存在的租约直接丢弃
        if self.entries.contains_key(&lease.node_hash) {
            self.leases.insert(lease.sticky_key.clone(), lease);
        }
    }

    /// 为 sticky_key 选一个节点。
    ///
    /// 1. 有租约且节点可用 → 复用（粘性命中）
    /// 2. 有租约但节点挂了 → 优先迁移到同出口 IP 的节点，保持 IP 不变
    /// 3. 无租约 → P2C 选延迟最优
    pub fn select(&mut self, sticky_key: &str) -> Option<Selection> {
        let now = now_ms();
        self.evict_expired_leases(now);

        if let Some(lease) = self.leases.get(sticky_key).cloned() {
            if let Some(entry) = self.entries.get(&lease.node_hash) {
                if entry.is_selectable(&self.config, now) {
                    let node = entry.node.clone();
                    if let Some(lease) = self.leases.get_mut(sticky_key) {
                        lease.last_used_at_ms = now;
                    }
                    return Some(Selection {
                        node,
                        from_lease: true,
                    });
                }
            }
            // 租约节点不可用：尝试同 IP 迁移
            if let Some(ip) = lease.egress_ip.as_deref() {
                if let Some(node) = self.pick_by_egress_ip(ip, now) {
                    self.bind_lease(sticky_key, &node, Some(ip.to_string()), now);
                    return Some(Selection {
                        node,
                        from_lease: false,
                    });
                }
            }
        }

        let node = self.pick_p2c(now)?;
        let egress = self
            .entries
            .get(&node.hash)
            .and_then(|e| e.health.egress_ip.clone());
        self.bind_lease(sticky_key, &node, egress, now);
        Some(Selection {
            node,
            from_lease: false,
        })
    }

    fn bind_lease(
        &mut self,
        sticky_key: &str,
        node: &ProxyNode,
        egress_ip: Option<String>,
        now: i64,
    ) {
        self.leases.insert(
            sticky_key.to_string(),
            Lease {
                sticky_key: sticky_key.to_string(),
                node_hash: node.hash.clone(),
                egress_ip,
                created_at_ms: now,
                last_used_at_ms: now,
            },
        );
    }

    /// 同出口 IP 的可用节点里挑延迟最优的
    fn pick_by_egress_ip(&self, ip: &str, now: i64) -> Option<ProxyNode> {
        self.entries
            .values()
            .filter(|e| e.is_selectable(&self.config, now))
            .filter(|e| e.health.egress_ip.as_deref() == Some(ip))
            .min_by(|a, b| score(a).total_cmp(&score(b)))
            .map(|e| e.node.clone())
    }

    /// Power of Two Choices：取两个候选，选分数更优的。
    ///
    /// 比全局最优更抗羊群效应 —— 全局最优会让所有请求挤到同一个节点上。
    fn pick_p2c(&mut self, now: i64) -> Option<ProxyNode> {
        let candidates: Vec<&PoolEntry> = self
            .entries
            .values()
            .filter(|e| e.is_selectable(&self.config, now))
            .collect();

        match candidates.len() {
            0 => None,
            1 => Some(candidates[0].node.clone()),
            len => {
                // 用游标取两个不同下标，保证确定性且分散
                self.cursor = self.cursor.wrapping_add(1);
                let i = (self.cursor as usize) % len;
                let j = (self.cursor as usize / len + 1 + i) % len;
                let j = if j == i { (i + 1) % len } else { j };

                let a = candidates[i];
                let b = candidates[j];
                Some(if score(a) <= score(b) {
                    a.node.clone()
                } else {
                    b.node.clone()
                })
            }
        }
    }

    /// 记录一次请求结果，驱动熔断与延迟统计
    pub fn record_result(&mut self, node_hash: &str, success: bool, latency_ms: Option<f64>) {
        let now = now_ms();
        let max_failures = self.config.max_consecutive_failures;
        if let Some(entry) = self.entries.get_mut(node_hash) {
            if success {
                entry.health.record_success(latency_ms);
            } else {
                entry.health.record_failure(max_failures, now);
            }
        }
    }

    /// 探测结果回写
    pub fn record_probe(
        &mut self,
        node_hash: &str,
        egress_ip: Option<String>,
        latency_ms: Option<f64>,
        success: bool,
    ) {
        let now = now_ms();
        let max_failures = self.config.max_consecutive_failures;
        if let Some(entry) = self.entries.get_mut(node_hash) {
            entry.health.last_probe_at_ms = now;
            if success {
                if egress_ip.is_some() {
                    entry.health.egress_ip = egress_ip;
                }
                entry.health.record_success(latency_ms);
            } else {
                entry.health.record_failure(max_failures, now);
            }
        }
    }

    fn evict_expired_leases(&mut self, now: i64) {
        let ttl_ms = self.config.lease_ttl_secs as i64 * 1000;
        if ttl_ms <= 0 {
            return;
        }
        self.leases
            .retain(|_, lease| now.saturating_sub(lease.last_used_at_ms) < ttl_ms);
    }

    /// 待探测节点：超过间隔未探测的，按最久未探测优先
    pub fn nodes_due_for_probe(&self, limit: usize) -> Vec<ProxyNode> {
        let interval_ms = self.config.probe_interval_secs as i64 * 1000;
        if interval_ms <= 0 {
            return Vec::new();
        }
        let now = now_ms();
        let mut due: Vec<&PoolEntry> = self
            .entries
            .values()
            .filter(|e| now.saturating_sub(e.health.last_probe_at_ms) >= interval_ms)
            .collect();
        due.sort_by_key(|e| e.health.last_probe_at_ms);
        due.into_iter().take(limit).map(|e| e.node.clone()).collect()
    }

    pub fn all_nodes(&self) -> Vec<ProxyNode> {
        self.entries.values().map(|e| e.node.clone()).collect()
    }

    /// 下发给前端的节点视图，按 tag 排序保证 UI 稳定
    pub fn node_views(&self) -> Vec<NodeView> {
        let mut views: Vec<NodeView> = self
            .entries
            .values()
            .map(|e| NodeView {
                node: e.node.clone(),
                health: e.health.clone(),
                subscription_ids: e.subscription_ids.iter().cloned().collect(),
            })
            .collect();
        views.sort_by(|a, b| a.node.tag.cmp(&b.node.tag));
        views
    }

    pub fn health_of(&self, node_hash: &str) -> Option<NodeHealth> {
        self.entries.get(node_hash).map(|e| e.health.clone())
    }

    pub fn leases(&self) -> Vec<Lease> {
        self.leases.values().cloned().collect()
    }

    pub fn clear_lease(&mut self, sticky_key: &str) -> bool {
        self.leases.remove(sticky_key).is_some()
    }

    pub fn clear_all_leases(&mut self) -> usize {
        let count = self.leases.len();
        self.leases.clear();
        count
    }

    /// 手动重置熔断，让节点重新参与选路
    pub fn reset_circuit(&mut self, node_hash: &str) -> bool {
        match self.entries.get_mut(node_hash) {
            Some(entry) => {
                entry.health.failure_count = 0;
                entry.health.circuit_open_since_ms = 0;
                true
            }
            None => false,
        }
    }

    pub fn stats(&self, subscription_count: u32) -> PoolStats {
        let mut healthy = 0u32;
        let mut open = 0u32;
        let mut ips = HashSet::new();

        for entry in self.entries.values() {
            if entry.is_healthy() {
                healthy += 1;
                if let Some(ip) = &entry.health.egress_ip {
                    ips.insert(ip.clone());
                }
            } else {
                open += 1;
            }
        }

        PoolStats {
            total_nodes: self.entries.len() as u32,
            healthy_nodes: healthy,
            circuit_open_nodes: open,
            unique_egress_ips: ips.len() as u32,
            active_leases: self.leases.len() as u32,
            subscription_count,
        }
    }
}

/// 选路分数，越小越优。无延迟数据的节点给一个较大的默认值，
/// 让已探测过的节点优先，但仍保留被选中的机会。
fn score(entry: &PoolEntry) -> f64 {
    entry.health.latency_ewma_ms.unwrap_or(5_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy_pool::types::NodeProtocol;

    fn node(tag: &str, port: u16) -> ProxyNode {
        ProxyNode::new(
            NodeProtocol::Socks5,
            "1.2.3.4".into(),
            port,
            None,
            None,
            tag.into(),
        )
    }

    fn pool_with(nodes: Vec<ProxyNode>) -> NodePool {
        let mut pool = NodePool::new(PoolConfig::default());
        pool.sync_subscription("sub1", nodes);
        pool
    }

    #[test]
    fn sync_preserves_health_across_refresh() {
        let mut pool = pool_with(vec![node("A", 1)]);
        let hash = pool.all_nodes()[0].hash.clone();
        pool.record_result(&hash, true, Some(120.0));

        // 同一节点再次出现在刷新结果里
        pool.sync_subscription("sub1", vec![node("A-renamed", 1)]);
        let health = pool.health_of(&hash).expect("node kept");
        assert_eq!(health.latency_ewma_ms, Some(120.0));
        // tag 更新了
        assert_eq!(pool.node_views()[0].node.tag, "A-renamed");
    }

    #[test]
    fn node_survives_while_another_subscription_references_it() {
        let mut pool = pool_with(vec![node("A", 1)]);
        pool.sync_subscription("sub2", vec![node("A", 1)]);
        assert_eq!(pool.all_nodes().len(), 1);

        // sub1 移除后 sub2 仍持有引用
        let removed = pool.remove_subscription("sub1");
        assert!(removed.is_empty());
        assert_eq!(pool.all_nodes().len(), 1);

        let removed = pool.remove_subscription("sub2");
        assert_eq!(removed.len(), 1);
        assert!(pool.all_nodes().is_empty());
    }

    #[test]
    fn lease_makes_selection_sticky() {
        let mut pool = pool_with(vec![node("A", 1), node("B", 2), node("C", 3)]);
        let first = pool.select("provider-1").expect("has node");
        assert!(!first.from_lease);

        for _ in 0..5 {
            let again = pool.select("provider-1").expect("has node");
            assert!(again.from_lease);
            assert_eq!(again.node.hash, first.node.hash);
        }
    }

    #[test]
    fn distinct_sticky_keys_can_get_distinct_nodes() {
        let mut pool = pool_with(vec![node("A", 1), node("B", 2), node("C", 3), node("D", 4)]);
        let a = pool.select("key-a").expect("node");
        let b = pool.select("key-b").expect("node");
        // P2C 游标推进，两个 key 不应绑定到同一节点
        assert_ne!(a.node.hash, b.node.hash);
    }

    #[test]
    fn circuit_open_node_is_skipped_and_lease_rebound() {
        let mut pool = pool_with(vec![node("A", 1), node("B", 2)]);
        let first = pool.select("k").expect("node");

        // 打到熔断阈值
        for _ in 0..3 {
            pool.record_result(&first.node.hash, false, None);
        }
        assert!(pool
            .health_of(&first.node.hash)
            .expect("exists")
            .is_circuit_open());

        let second = pool.select("k").expect("fallback node");
        assert_ne!(second.node.hash, first.node.hash);
        assert!(!second.from_lease);
    }

    #[test]
    fn migrates_to_same_egress_ip_when_bound_node_dies() {
        let mut pool = pool_with(vec![node("A", 1), node("B", 2), node("C", 3)]);
        let nodes = pool.all_nodes();

        // A 和 B 同出口 IP，C 不同
        pool.record_probe(&nodes[0].hash, Some("9.9.9.9".into()), Some(50.0), true);
        pool.record_probe(&nodes[1].hash, Some("9.9.9.9".into()), Some(80.0), true);
        pool.record_probe(&nodes[2].hash, Some("8.8.8.8".into()), Some(10.0), true);

        // 绑定到 9.9.9.9 上的某个节点
        let target_ip = "9.9.9.9";
        let bound = {
            // 直接构造租约，确保绑定在 9.9.9.9
            let same_ip_hash = nodes
                .iter()
                .find(|n| {
                    pool.health_of(&n.hash)
                        .and_then(|h| h.egress_ip)
                        .as_deref()
                        == Some(target_ip)
                })
                .expect("has same-ip node")
                .hash
                .clone();
            pool.load_lease(Lease {
                sticky_key: "k".into(),
                node_hash: same_ip_hash.clone(),
                egress_ip: Some(target_ip.into()),
                created_at_ms: now_ms(),
                last_used_at_ms: now_ms(),
            });
            same_ip_hash
        };

        // 熔断该节点
        for _ in 0..3 {
            pool.record_result(&bound, false, None);
        }

        let next = pool.select("k").expect("migrated");
        assert_ne!(next.node.hash, bound);
        // 迁移后仍在同一出口 IP 上，即使 C 延迟更低
        let ip = pool
            .health_of(&next.node.hash)
            .and_then(|h| h.egress_ip)
            .expect("has ip");
        assert_eq!(ip, target_ip);
    }

    #[test]
    fn cooled_down_circuit_is_selectable_again() {
        let config = PoolConfig {
            circuit_cooldown_secs: 0,
            ..PoolConfig::default()
        };
        let mut pool = NodePool::new(config);
        pool.sync_subscription("s", vec![node("A", 1)]);
        let hash = pool.all_nodes()[0].hash.clone();

        for _ in 0..3 {
            pool.record_result(&hash, false, None);
        }
        // 冷却期为 0，熔断后立即放行试探
        assert!(pool.select("k").is_some());
    }

    #[test]
    fn p2c_prefers_lower_latency() {
        let mut pool = pool_with(vec![node("fast", 1), node("slow", 2)]);
        let nodes = pool.all_nodes();
        let fast = nodes.iter().find(|n| n.tag == "fast").expect("fast").hash.clone();
        let slow = nodes.iter().find(|n| n.tag == "slow").expect("slow").hash.clone();
        pool.record_result(&fast, true, Some(10.0));
        pool.record_result(&slow, true, Some(900.0));

        // 只有两个候选时 P2C 必然比较这两个，应稳定选中 fast
        let mut fast_hits = 0;
        for i in 0..10 {
            let key = format!("k{i}");
            if pool.select(&key).expect("node").node.hash == fast {
                fast_hits += 1;
            }
        }
        assert_eq!(fast_hits, 10);
    }

    #[test]
    fn empty_pool_selects_nothing() {
        let mut pool = NodePool::new(PoolConfig::default());
        assert!(pool.select("k").is_none());
    }

    #[test]
    fn expired_lease_is_evicted() {
        let config = PoolConfig {
            lease_ttl_secs: 1,
            ..PoolConfig::default()
        };
        let mut pool = NodePool::new(config);
        pool.sync_subscription("s", vec![node("A", 1)]);

        let hash = pool.all_nodes()[0].hash.clone();
        pool.load_lease(Lease {
            sticky_key: "k".into(),
            node_hash: hash,
            egress_ip: None,
            created_at_ms: 0,
            // 远早于 TTL
            last_used_at_ms: now_ms() - 10_000,
        });
        let sel = pool.select("k").expect("node");
        // 租约已过期，重新绑定而非复用
        assert!(!sel.from_lease);
    }

    #[test]
    fn stats_counts_healthy_and_unique_ips() {
        let mut pool = pool_with(vec![node("A", 1), node("B", 2), node("C", 3)]);
        let nodes = pool.all_nodes();
        pool.record_probe(&nodes[0].hash, Some("1.1.1.1".into()), Some(10.0), true);
        pool.record_probe(&nodes[1].hash, Some("1.1.1.1".into()), Some(20.0), true);
        pool.record_probe(&nodes[2].hash, Some("2.2.2.2".into()), Some(30.0), true);
        for _ in 0..3 {
            pool.record_result(&nodes[2].hash, false, None);
        }

        let stats = pool.stats(1);
        assert_eq!(stats.total_nodes, 3);
        assert_eq!(stats.healthy_nodes, 2);
        assert_eq!(stats.circuit_open_nodes, 1);
        // 熔断节点的 IP 不计入
        assert_eq!(stats.unique_egress_ips, 1);
    }

    #[test]
    fn reset_circuit_restores_node() {
        let mut pool = pool_with(vec![node("A", 1)]);
        let hash = pool.all_nodes()[0].hash.clone();
        for _ in 0..3 {
            pool.record_result(&hash, false, None);
        }
        assert!(pool.health_of(&hash).expect("e").is_circuit_open());
        assert!(pool.reset_circuit(&hash));
        assert!(!pool.health_of(&hash).expect("e").is_circuit_open());
    }

    #[test]
    fn probe_due_list_respects_interval_and_limit() {
        let mut pool = pool_with(vec![node("A", 1), node("B", 2), node("C", 3)]);
        // 全部从未探测，应全部到期
        assert_eq!(pool.nodes_due_for_probe(10).len(), 3);
        assert_eq!(pool.nodes_due_for_probe(2).len(), 2);

        // 探测过的节点退出到期列表
        let nodes = pool.all_nodes();
        for n in &nodes {
            pool.record_probe(&n.hash, None, Some(10.0), true);
        }
        assert!(pool.nodes_due_for_probe(10).is_empty());
    }
}
