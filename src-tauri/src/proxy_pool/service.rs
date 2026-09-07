//! 代理池服务：编排池、DB、探测三者
//!
//! 内存池是权威副本，DB 是持久化镜像。所有写操作先改池再落盘。
//! 探测循环由 `start_probe_loop` 起一个后台 task，退出时通过 shutdown 通道停止。

use super::parser::parse_subscription;
use super::pool::NodePool;
use super::probe;
use super::types::{
    now_ms, Lease, NodeView, PoolConfig, PoolStats, ProxyNode, Subscription, SubscriptionSource,
};
use crate::database::Database;
use crate::error::AppError;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{oneshot, RwLock};

/// 刷新订阅的结果，回给 UI 做提示
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshOutcome {
    pub subscription_id: String,
    pub node_count: u32,
    pub skipped_unsupported: u32,
    /// 被跳过的协议名，用于提示用户"这些节点当前不支持"
    pub skipped_protocols: Vec<String>,
}

#[derive(Clone)]
pub struct ProxyPoolService {
    db: Arc<Database>,
    pool: Arc<RwLock<NodePool>>,
    /// 探测循环的停止信号，Some 表示循环在跑
    probe_shutdown: Arc<RwLock<Option<oneshot::Sender<()>>>>,
}

impl ProxyPoolService {
    pub fn new(db: Arc<Database>) -> Self {
        Self {
            db,
            pool: Arc::new(RwLock::new(NodePool::new(PoolConfig::default()))),
            probe_shutdown: Arc::new(RwLock::new(None)),
        }
    }

    /// 启动时从 DB 恢复节点、健康状态与租约
    pub async fn load_from_db(&self) -> Result<(), AppError> {
        let nodes = self.db.pp_load_nodes()?;
        let leases = self.db.pp_load_leases()?;

        let mut pool = self.pool.write().await;
        for (node, health, subscription_ids) in nodes {
            pool.load_entry(node, health, subscription_ids);
        }
        for lease in leases {
            pool.load_lease(lease);
        }

        let stats = pool.stats(0);
        log::info!(
            "[ProxyPool] 已恢复 {} 个节点、{} 条租约",
            stats.total_nodes,
            stats.active_leases
        );
        Ok(())
    }

    // ---- 订阅管理 ----

    pub fn list_subscriptions(&self) -> Result<Vec<Subscription>, AppError> {
        self.db.pp_list_subscriptions()
    }

    /// 新增订阅。立即刷新一次，让用户马上看到节点数。
    pub async fn add_subscription(
        &self,
        name: String,
        source: SubscriptionSource,
        url: String,
        content: String,
        update_interval_secs: u64,
    ) -> Result<RefreshOutcome, AppError> {
        let trimmed_name = name.trim();
        if trimmed_name.is_empty() {
            return Err(AppError::localized(
                "proxyPool.error.nameRequired",
                "订阅名称不能为空",
                "Subscription name is required",
            ));
        }
        if source == SubscriptionSource::Remote && url.trim().is_empty() {
            return Err(AppError::localized(
                "proxyPool.error.urlRequired",
                "远程订阅必须填写 URL",
                "Remote subscription requires a URL",
            ));
        }
        if source == SubscriptionSource::Inline && content.trim().is_empty() {
            return Err(AppError::localized(
                "proxyPool.error.contentRequired",
                "手动订阅内容不能为空",
                "Inline subscription content is required",
            ));
        }

        let now = now_ms();
        let sub = Subscription {
            id: uuid::Uuid::new_v4().to_string(),
            name: trimmed_name.to_string(),
            source,
            url: url.trim().to_string(),
            content,
            enabled: true,
            update_interval_secs,
            created_at_ms: now,
            updated_at_ms: now,
            node_count: 0,
            last_error: None,
        };
        self.db.pp_upsert_subscription(&sub)?;
        self.refresh_subscription(&sub.id).await
    }

    pub async fn update_subscription(&self, sub: Subscription) -> Result<(), AppError> {
        let mut sub = sub;
        sub.updated_at_ms = now_ms();

        // content 标了 skip_serializing，前端拿不到也回传不了，反序列化后必然是空串。
        // 不补回原值就会清空 inline 订阅的节点唯一来源，且不可恢复。
        if sub.content.is_empty() {
            if let Some(existing) = self
                .db
                .pp_list_subscriptions()?
                .into_iter()
                .find(|s| s.id == sub.id)
            {
                sub.content = existing.content;
            }
        }

        self.db.pp_upsert_subscription(&sub)?;

        // 停用的订阅立即撤下其节点，不必等下次刷新
        if !sub.enabled {
            let removed = {
                let mut pool = self.pool.write().await;
                pool.remove_subscription(&sub.id)
            };
            self.db.pp_sync_subscription_nodes(&sub.id, &[])?;
            log::info!(
                "[ProxyPool] 订阅 {} 已停用，撤下 {} 个节点",
                sub.id,
                removed.len()
            );
        }
        Ok(())
    }

    pub async fn delete_subscription(&self, id: &str) -> Result<(), AppError> {
        {
            let mut pool = self.pool.write().await;
            pool.remove_subscription(id);
        }
        self.db.pp_sync_subscription_nodes(id, &[])?;
        self.db.pp_delete_subscription(id)?;
        self.persist_leases().await?;
        Ok(())
    }

    /// 拉取并解析订阅，同步到池和 DB。
    ///
    /// 拉取失败会把错误写进订阅记录供 UI 展示，但不清空既有节点 ——
    /// 网络抖动不应导致代理池瞬间变空。
    pub async fn refresh_subscription(&self, id: &str) -> Result<RefreshOutcome, AppError> {
        let mut sub = self
            .db
            .pp_list_subscriptions()?
            .into_iter()
            .find(|s| s.id == id)
            .ok_or_else(|| {
                AppError::localized(
                    "proxyPool.error.subscriptionNotFound",
                    "订阅不存在",
                    "Subscription not found",
                )
            })?;

        let content = match sub.source {
            SubscriptionSource::Inline => sub.content.clone(),
            SubscriptionSource::Remote => match self.fetch_remote(&sub.url).await {
                Ok(body) => body,
                Err(e) => {
                    sub.last_error = Some(e.clone());
                    sub.updated_at_ms = now_ms();
                    self.db.pp_upsert_subscription(&sub)?;
                    return Err(AppError::InvalidInput(format!("拉取订阅失败: {e}")));
                }
            },
        };

        let outcome = match parse_subscription(&content) {
            Ok(outcome) => outcome,
            Err(e) => {
                sub.last_error = Some(e.clone());
                sub.updated_at_ms = now_ms();
                self.db.pp_upsert_subscription(&sub)?;
                return Err(AppError::InvalidInput(format!("解析订阅失败: {e}")));
            }
        };

        let node_count = outcome.nodes.len() as u32;
        self.db.pp_sync_subscription_nodes(id, &outcome.nodes)?;
        {
            let mut pool = self.pool.write().await;
            pool.sync_subscription(id, outcome.nodes);
        }
        self.persist_leases().await?;

        // 远程订阅缓存内容，下次启动无网也能重建节点
        if sub.source == SubscriptionSource::Remote {
            sub.content = content;
        }
        sub.node_count = node_count;
        sub.last_error = None;
        sub.updated_at_ms = now_ms();
        self.db.pp_upsert_subscription(&sub)?;

        log::info!(
            "[ProxyPool] 订阅 {} 刷新完成：{} 个可用节点，跳过 {} 个不支持的节点",
            id,
            node_count,
            outcome.skipped_unsupported
        );

        Ok(RefreshOutcome {
            subscription_id: id.to_string(),
            node_count,
            skipped_unsupported: outcome.skipped_unsupported as u32,
            skipped_protocols: outcome.skipped_protocols,
        })
    }

    /// 拉取远程订阅。走全局代理客户端 —— 订阅地址本身可能需要代理才能访问。
    async fn fetch_remote(&self, url: &str) -> Result<String, String> {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err("订阅 URL 必须以 http:// 或 https:// 开头".to_string());
        }
        let client = crate::proxy::http_client::get();
        let response = client
            .get(url)
            .timeout(Duration::from_secs(30))
            // 多数订阅服务按 UA 返回不同格式，clash.meta 能拿到最通用的形式
            .header("User-Agent", "clash.meta")
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    "请求超时".to_string()
                } else if e.is_connect() {
                    "连接失败".to_string()
                } else {
                    "请求失败".to_string()
                }
            })?;

        let status = response.status();
        if !status.is_success() {
            return Err(format!("HTTP {}", status.as_u16()));
        }
        response
            .text()
            .await
            .map_err(|_| "读取响应内容失败".to_string())
    }

    // ---- 选路 ----

    /// 给一个业务身份选代理 URL。返回 None 表示池空或全部熔断，调用方应直连。
    ///
    /// 这是 forwarder 的入口。
    pub async fn select_proxy_url(&self, sticky_key: &str) -> Option<String> {
        let mut pool = self.pool.write().await;
        if !pool.config().enabled {
            return None;
        }
        pool.select(sticky_key)
            .map(|selection| selection.node.to_proxy_url())
    }

    /// 记录请求结果，驱动熔断
    pub async fn record_result(&self, proxy_url: &str, success: bool, latency_ms: Option<f64>) {
        // forwarder 只持有 proxy_url，这里反查 hash
        let hash = {
            let pool = self.pool.read().await;
            pool.all_nodes()
                .into_iter()
                .find(|n| n.to_proxy_url() == proxy_url)
                .map(|n| n.hash)
        };
        if let Some(hash) = hash {
            let mut pool = self.pool.write().await;
            pool.record_result(&hash, success, latency_ms);
        }
    }

    // ---- 探测 ----

    /// 探测指定节点；传空则探测所有到期节点
    pub async fn probe_now(&self, node_hashes: Vec<String>) -> Result<usize, AppError> {
        let (targets, config) = {
            let pool = self.pool.read().await;
            let config = pool.config().clone();
            let targets: Vec<ProxyNode> = if node_hashes.is_empty() {
                pool.all_nodes()
            } else {
                pool.all_nodes()
                    .into_iter()
                    .filter(|n| node_hashes.contains(&n.hash))
                    .collect()
            };
            (targets, config)
        };

        if targets.is_empty() {
            return Ok(0);
        }
        self.run_probe_round(&targets, &config).await
    }

    async fn run_probe_round(
        &self,
        targets: &[ProxyNode],
        config: &PoolConfig,
    ) -> Result<usize, AppError> {
        let results = probe::probe_batch(
            targets,
            &config.egress_probe_url,
            Duration::from_secs(config.probe_timeout_secs),
            config.probe_concurrency,
        )
        .await;

        let count = results.len();
        let mut updates = Vec::with_capacity(count);
        {
            let mut pool = self.pool.write().await;
            for result in results {
                if let Some(error) = &result.error {
                    // 节点级失败原因只进日志：探测失败是常态，不值得打扰用户，
                    // 但排查"为什么全部熔断"时必须能查到
                    log::debug!(
                        "[ProxyPool] 节点 {} 探测失败: {error}",
                        &result.node_hash[..8.min(result.node_hash.len())]
                    );
                }
                pool.record_probe(
                    &result.node_hash,
                    result.egress_ip,
                    result.latency_ms,
                    result.success,
                );
                if let Some(health) = pool.health_of(&result.node_hash) {
                    updates.push((result.node_hash, health));
                }
            }
        }
        self.db.pp_save_health(&updates)?;
        Ok(count)
    }

    /// 起后台探测循环。重复调用是幂等的。
    pub async fn start_probe_loop(&self) {
        let mut guard = self.probe_shutdown.write().await;
        if guard.is_some() {
            return;
        }

        let (tx, mut rx) = oneshot::channel();
        *guard = Some(tx);
        drop(guard);

        let service = self.clone();
        tokio::spawn(async move {
            log::info!("[ProxyPool] 探测循环已启动");
            loop {
                let interval = {
                    let pool = service.pool.read().await;
                    pool.config().probe_interval_secs
                };
                // 间隔为 0 表示关闭主动探测，但循环保留以便配置改回来后无需重启
                let sleep_secs = if interval == 0 { 60 } else { interval.min(3600) };

                tokio::select! {
                    _ = &mut rx => {
                        log::info!("[ProxyPool] 探测循环已停止");
                        break;
                    }
                    _ = tokio::time::sleep(Duration::from_secs(sleep_secs)) => {}
                }

                let (targets, config) = {
                    let pool = service.pool.read().await;
                    let config = pool.config().clone();
                    if !config.enabled || config.probe_interval_secs == 0 {
                        (Vec::new(), config)
                    } else {
                        (pool.nodes_due_for_probe(config.probe_concurrency * 4), config)
                    }
                };
                if targets.is_empty() {
                    continue;
                }
                if let Err(e) = service.run_probe_round(&targets, &config).await {
                    log::warn!("[ProxyPool] 探测轮次失败: {e}");
                }
            }
        });
    }

    /// 停探测循环并落盘租约。退出清理时调用。
    pub async fn shutdown(&self) {
        if let Some(tx) = self.probe_shutdown.write().await.take() {
            let _ = tx.send(());
        }
        if let Err(e) = self.persist_leases().await {
            log::warn!("[ProxyPool] 退出时保存租约失败: {e}");
        }
    }

    // ---- 配置与查询 ----

    pub async fn get_config(&self) -> PoolConfig {
        self.pool.read().await.config().clone()
    }

    pub async fn set_config(&self, config: PoolConfig) -> Result<(), AppError> {
        let should_start = config.enabled;
        {
            let mut pool = self.pool.write().await;
            pool.set_config(config);
        }
        if should_start {
            self.start_probe_loop().await;
        }
        Ok(())
    }

    pub async fn node_views(&self) -> Vec<NodeView> {
        self.pool.read().await.node_views()
    }

    pub async fn stats(&self) -> Result<PoolStats, AppError> {
        let subscription_count = self.db.pp_list_subscriptions()?.len() as u32;
        Ok(self.pool.read().await.stats(subscription_count))
    }

    pub async fn leases(&self) -> Vec<Lease> {
        self.pool.read().await.leases()
    }

    pub async fn clear_lease(&self, sticky_key: &str) -> Result<bool, AppError> {
        let removed = {
            let mut pool = self.pool.write().await;
            pool.clear_lease(sticky_key)
        };
        self.persist_leases().await?;
        Ok(removed)
    }

    pub async fn clear_all_leases(&self) -> Result<usize, AppError> {
        let count = {
            let mut pool = self.pool.write().await;
            pool.clear_all_leases()
        };
        self.persist_leases().await?;
        Ok(count)
    }

    pub async fn reset_circuit(&self, node_hash: &str) -> Result<bool, AppError> {
        let reset = {
            let mut pool = self.pool.write().await;
            pool.reset_circuit(node_hash)
        };
        if reset {
            let health = self.pool.read().await.health_of(node_hash);
            if let Some(health) = health {
                self.db.pp_save_health(&[(node_hash.to_string(), health)])?;
            }
        }
        Ok(reset)
    }

    async fn persist_leases(&self) -> Result<(), AppError> {
        let leases = self.pool.read().await.leases();
        self.db.pp_save_leases(&leases)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy_pool::types::NodeProtocol;

    fn service() -> ProxyPoolService {
        let db = Arc::new(Database::memory().expect("memory db"));
        ProxyPoolService::new(db)
    }

    #[tokio::test]
    async fn add_inline_subscription_parses_and_persists() {
        let svc = service();
        let outcome = svc
            .add_subscription(
                "manual".into(),
                SubscriptionSource::Inline,
                String::new(),
                "socks5://1.2.3.4:1080#A\nhttp://5.6.7.8:8080#B".into(),
                0,
            )
            .await
            .expect("add ok");

        assert_eq!(outcome.node_count, 2);
        assert_eq!(outcome.skipped_unsupported, 0);

        // 池里有节点
        assert_eq!(svc.node_views().await.len(), 2);
        // DB 里也有
        let subs = svc.list_subscriptions().expect("list");
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].node_count, 2);
        assert_eq!(subs[0].last_error, None);
    }

    #[tokio::test]
    async fn add_subscription_reports_skipped_protocols() {
        let svc = service();
        let outcome = svc
            .add_subscription(
                "mixed".into(),
                SubscriptionSource::Inline,
                String::new(),
                "vmess://x\nsocks5://1.2.3.4:1080#OK\ntrojan://y".into(),
                0,
            )
            .await
            .expect("add ok");

        assert_eq!(outcome.node_count, 1);
        assert_eq!(outcome.skipped_unsupported, 2);
        assert_eq!(outcome.skipped_protocols.len(), 2);
    }

    #[tokio::test]
    async fn validation_rejects_bad_input() {
        let svc = service();
        assert!(svc
            .add_subscription("".into(), SubscriptionSource::Inline, "".into(), "x".into(), 0)
            .await
            .is_err());
        assert!(svc
            .add_subscription("n".into(), SubscriptionSource::Remote, "".into(), "".into(), 0)
            .await
            .is_err());
        assert!(svc
            .add_subscription("n".into(), SubscriptionSource::Inline, "".into(), "".into(), 0)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn select_returns_none_while_disabled() {
        let svc = service();
        svc.add_subscription(
            "m".into(),
            SubscriptionSource::Inline,
            String::new(),
            "socks5://1.2.3.4:1080#A".into(),
            0,
        )
        .await
        .expect("add");

        // 默认 enabled = false
        assert!(svc.select_proxy_url("provider-1").await.is_none());

        let mut config = svc.get_config().await;
        config.enabled = true;
        svc.set_config(config).await.expect("set config");

        let url = svc.select_proxy_url("provider-1").await.expect("has url");
        assert_eq!(url, "socks5://1.2.3.4:1080");
        // 同一 key 再选应命中租约，拿到同一个节点
        assert_eq!(svc.select_proxy_url("provider-1").await.as_deref(), Some(url.as_str()));

        svc.shutdown().await;
    }

    #[tokio::test]
    async fn delete_subscription_removes_nodes() {
        let svc = service();
        let outcome = svc
            .add_subscription(
                "m".into(),
                SubscriptionSource::Inline,
                String::new(),
                "socks5://1.2.3.4:1080#A".into(),
                0,
            )
            .await
            .expect("add");
        assert_eq!(svc.node_views().await.len(), 1);

        svc.delete_subscription(&outcome.subscription_id)
            .await
            .expect("delete");
        assert!(svc.node_views().await.is_empty());
        assert!(svc.list_subscriptions().expect("list").is_empty());
    }

    #[tokio::test]
    async fn disabling_subscription_withdraws_nodes() {
        let svc = service();
        let outcome = svc
            .add_subscription(
                "m".into(),
                SubscriptionSource::Inline,
                String::new(),
                "socks5://1.2.3.4:1080#A".into(),
                0,
            )
            .await
            .expect("add");

        let mut sub = svc
            .list_subscriptions()
            .expect("list")
            .into_iter()
            .find(|s| s.id == outcome.subscription_id)
            .expect("found");
        sub.enabled = false;
        svc.update_subscription(sub).await.expect("update");

        assert!(svc.node_views().await.is_empty());
    }

    #[tokio::test]
    async fn update_preserves_inline_content_when_absent() {
        // 回归：content 是 skip_serializing，前端回传必然空串。
        // 若不补回原值，inline 订阅的节点来源会被永久清空。
        let svc = service();
        let outcome = svc
            .add_subscription(
                "inline".into(),
                SubscriptionSource::Inline,
                String::new(),
                "socks5://1.2.3.4:1080#A\nsocks5://1.2.3.4:1081#B".into(),
                0,
            )
            .await
            .expect("add");

        // 模拟前端往返：读回来的 sub 不含 content
        let mut sub = svc
            .list_subscriptions()
            .expect("list")
            .into_iter()
            .find(|s| s.id == outcome.subscription_id)
            .expect("found");
        assert!(!sub.content.is_empty(), "DB 里应有正文");
        sub.content = String::new();
        sub.name = "renamed".into();
        svc.update_subscription(sub).await.expect("update");

        // 正文还在，刷新仍能解析出节点
        let refreshed = svc
            .refresh_subscription(&outcome.subscription_id)
            .await
            .expect("refresh after update");
        assert_eq!(refreshed.node_count, 2);
        assert_eq!(svc.node_views().await.len(), 2);
    }

    #[tokio::test]
    async fn update_accepts_explicit_content_replacement() {
        // 显式传新正文时必须覆盖，否则用户改不了 inline 订阅
        let svc = service();
        let outcome = svc
            .add_subscription(
                "inline".into(),
                SubscriptionSource::Inline,
                String::new(),
                "socks5://1.2.3.4:1080#A".into(),
                0,
            )
            .await
            .expect("add");

        let mut sub = svc
            .list_subscriptions()
            .expect("list")
            .into_iter()
            .find(|s| s.id == outcome.subscription_id)
            .expect("found");
        sub.content = "socks5://9.9.9.9:1080#NEW\nsocks5://9.9.9.9:1081#NEW2".into();
        svc.update_subscription(sub).await.expect("update");

        let refreshed = svc
            .refresh_subscription(&outcome.subscription_id)
            .await
            .expect("refresh");
        assert_eq!(refreshed.node_count, 2);
        let views = svc.node_views().await;
        assert!(views.iter().all(|v| v.node.host == "9.9.9.9"));
    }

    #[tokio::test]
    async fn load_from_db_restores_nodes_and_health() {
        let db = Arc::new(Database::memory().expect("db"));
        let first = ProxyPoolService::new(db.clone());
        first
            .add_subscription(
                "m".into(),
                SubscriptionSource::Inline,
                String::new(),
                "socks5://1.2.3.4:1080#A".into(),
                0,
            )
            .await
            .expect("add");

        // 新建 service 共享同一个 DB，模拟重启
        let restarted = ProxyPoolService::new(db);
        assert!(restarted.node_views().await.is_empty());
        restarted.load_from_db().await.expect("load");
        assert_eq!(restarted.node_views().await.len(), 1);
    }

    #[tokio::test]
    async fn refresh_missing_subscription_errors() {
        let svc = service();
        assert!(svc.refresh_subscription("nope").await.is_err());
    }

    #[tokio::test]
    async fn stats_reflect_pool_and_subscription_count() {
        let svc = service();
        svc.add_subscription(
            "m".into(),
            SubscriptionSource::Inline,
            String::new(),
            "socks5://1.2.3.4:1080#A\nsocks5://1.2.3.4:1081#B".into(),
            0,
        )
        .await
        .expect("add");

        let stats = svc.stats().await.expect("stats");
        assert_eq!(stats.total_nodes, 2);
        assert_eq!(stats.healthy_nodes, 2);
        assert_eq!(stats.subscription_count, 1);
    }

    #[tokio::test]
    async fn lease_management_clears_and_persists() {
        let svc = service();
        svc.add_subscription(
            "m".into(),
            SubscriptionSource::Inline,
            String::new(),
            "socks5://1.2.3.4:1080#A".into(),
            0,
        )
        .await
        .expect("add");
        let mut config = svc.get_config().await;
        config.enabled = true;
        svc.set_config(config).await.expect("config");

        svc.select_proxy_url("k1").await.expect("url");
        svc.select_proxy_url("k2").await.expect("url");
        assert_eq!(svc.leases().await.len(), 2);

        assert!(svc.clear_lease("k1").await.expect("clear"));
        assert_eq!(svc.leases().await.len(), 1);

        assert_eq!(svc.clear_all_leases().await.expect("clear all"), 1);
        assert!(svc.leases().await.is_empty());

        svc.shutdown().await;
    }

    #[tokio::test]
    async fn probe_now_on_empty_pool_is_noop() {
        let svc = service();
        assert_eq!(svc.probe_now(vec![]).await.expect("probe"), 0);
    }

    #[tokio::test]
    async fn reset_circuit_reports_missing_node() {
        let svc = service();
        assert!(!svc.reset_circuit("deadbeef").await.expect("reset"));
    }

    #[tokio::test]
    async fn start_probe_loop_is_idempotent() {
        let svc = service();
        svc.start_probe_loop().await;
        svc.start_probe_loop().await;
        svc.shutdown().await;
    }

    #[tokio::test]
    async fn record_result_drives_circuit_by_proxy_url() {
        let svc = service();
        svc.add_subscription(
            "m".into(),
            SubscriptionSource::Inline,
            String::new(),
            "socks5://1.2.3.4:1080#A\nsocks5://1.2.3.4:1081#B".into(),
            0,
        )
        .await
        .expect("add");
        let mut config = svc.get_config().await;
        config.enabled = true;
        svc.set_config(config).await.expect("config");

        let url = svc.select_proxy_url("k").await.expect("url");
        for _ in 0..3 {
            svc.record_result(&url, false, None).await;
        }

        // 熔断后同一 key 应换到另一个节点
        let next = svc.select_proxy_url("k").await.expect("fallback");
        assert_ne!(next, url);

        svc.shutdown().await;
    }

    #[tokio::test]
    async fn node_protocol_survives_db_roundtrip() {
        let db = Arc::new(Database::memory().expect("db"));
        let svc = ProxyPoolService::new(db.clone());
        svc.add_subscription(
            "m".into(),
            SubscriptionSource::Inline,
            String::new(),
            "http://1.1.1.1:8080#H".into(),
            0,
        )
        .await
        .expect("add");

        let restarted = ProxyPoolService::new(db);
        restarted.load_from_db().await.expect("load");
        let views = restarted.node_views().await;
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].node.protocol, NodeProtocol::Http);
    }
}
