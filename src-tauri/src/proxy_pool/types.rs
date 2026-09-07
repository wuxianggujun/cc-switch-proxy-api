//! 代理池核心类型
//!
//! 节点、订阅、粘性租约的领域模型。序列化字段统一 camelCase 对齐前端。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

/// 出站协议。当前只实现 reqwest 原生支持的两种。
///
/// 新增协议需同时扩展 `ProxyNode::to_proxy_url` 与订阅解析。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeProtocol {
    Http,
    Https,
    Socks5,
}

impl NodeProtocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
            Self::Socks5 => "socks5",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "http" => Some(Self::Http),
            "https" => Some(Self::Https),
            // socks5h 的差别是域名由代理端解析，reqwest 的 socks5h scheme 行为一致
            "socks5" | "socks" | "socks5h" => Some(Self::Socks5),
            _ => None,
        }
    }
}

/// 一个代理节点。`hash` 是跨订阅去重的唯一键。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyNode {
    /// sha256(protocol|host|port|username|password) 前 16 字节 hex
    pub hash: String,
    pub protocol: NodeProtocol,
    pub host: String,
    pub port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// 不下发给前端，避免凭据出现在 webview
    #[serde(skip_serializing)]
    pub password: Option<String>,
    /// 订阅里的节点名
    pub tag: String,
}

impl ProxyNode {
    pub fn compute_hash(
        protocol: NodeProtocol,
        host: &str,
        port: u16,
        username: Option<&str>,
        password: Option<&str>,
    ) -> String {
        let mut hasher = Sha256::new();
        hasher.update(protocol.as_str().as_bytes());
        hasher.update(b"|");
        hasher.update(host.trim().to_ascii_lowercase().as_bytes());
        hasher.update(b"|");
        hasher.update(port.to_string().as_bytes());
        hasher.update(b"|");
        hasher.update(username.unwrap_or("").as_bytes());
        hasher.update(b"|");
        hasher.update(password.unwrap_or("").as_bytes());
        hex_prefix(&hasher.finalize(), 16)
    }

    pub fn new(
        protocol: NodeProtocol,
        host: String,
        port: u16,
        username: Option<String>,
        password: Option<String>,
        tag: String,
    ) -> Self {
        let hash = Self::compute_hash(
            protocol,
            &host,
            port,
            username.as_deref(),
            password.as_deref(),
        );
        Self {
            hash,
            protocol,
            host,
            port,
            username,
            password,
            tag,
        }
    }

    /// 构造 reqwest::Proxy 能吃的 URL，含 userinfo。
    ///
    /// 凭据按 RFC 3986 userinfo 规则百分号编码，否则密码里的 `@` `:` `/`
    /// 会破坏 URL 结构。
    pub fn to_proxy_url(&self) -> String {
        let auth = match (&self.username, &self.password) {
            (Some(u), Some(p)) if !u.is_empty() => {
                format!("{}:{}@", encode_userinfo(u), encode_userinfo(p))
            }
            (Some(u), None) if !u.is_empty() => format!("{}@", encode_userinfo(u)),
            _ => String::new(),
        };
        format!(
            "{}://{}{}:{}",
            self.protocol.as_str(),
            auth,
            self.host,
            self.port
        )
    }

    /// 日志/UI 用的地址，不含凭据
    pub fn endpoint(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// 节点运行时健康状态。持久化后重启可恢复。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeHealth {
    /// 连续失败次数，成功即归零
    pub failure_count: u32,
    /// 熔断起始时间（ms）。0 表示未熔断。
    pub circuit_open_since_ms: i64,
    /// 探测到的出口 IP，粘性路由靠它做同 IP 迁移
    #[serde(skip_serializing_if = "Option::is_none")]
    pub egress_ip: Option<String>,
    /// 延迟 EWMA（ms）。None 表示尚未探测出有效值。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ewma_ms: Option<f64>,
    /// 最后一次探测尝试时间（ms）
    pub last_probe_at_ms: i64,
}

impl NodeHealth {
    /// EWMA 平滑系数。0.3 让新样本占三成，够快反映劣化又不过度抖动。
    const LATENCY_ALPHA: f64 = 0.3;

    pub fn is_circuit_open(&self) -> bool {
        self.circuit_open_since_ms > 0
    }

    /// 熔断是否已过冷却期，可以放行试探
    pub fn is_cooled_down(&self, cooldown_ms: i64, now_ms: i64) -> bool {
        self.circuit_open_since_ms > 0
            && now_ms.saturating_sub(self.circuit_open_since_ms) >= cooldown_ms
    }

    pub fn record_success(&mut self, latency_ms: Option<f64>) {
        self.failure_count = 0;
        self.circuit_open_since_ms = 0;
        if let Some(sample) = latency_ms {
            self.latency_ewma_ms = Some(match self.latency_ewma_ms {
                Some(prev) => prev * (1.0 - Self::LATENCY_ALPHA) + sample * Self::LATENCY_ALPHA,
                None => sample,
            });
        }
    }

    pub fn record_failure(&mut self, max_failures: u32, now_ms: i64) {
        self.failure_count = self.failure_count.saturating_add(1);
        if self.failure_count >= max_failures && self.circuit_open_since_ms == 0 {
            self.circuit_open_since_ms = now_ms;
        }
    }
}

/// 节点 + 健康状态，下发给前端的完整视图
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeView {
    #[serde(flatten)]
    pub node: ProxyNode,
    pub health: NodeHealth,
    /// 所属订阅 id 列表（跨订阅去重后可能多个）
    pub subscription_ids: Vec<String>,
}

/// 订阅来源类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubscriptionSource {
    /// 远程 URL，按 interval 定期拉取
    Remote,
    /// 手工粘贴的内容
    Inline,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Subscription {
    pub id: String,
    pub name: String,
    pub source: SubscriptionSource,
    /// Remote 时是 URL；Inline 时为空
    #[serde(default)]
    pub url: String,
    /// Inline 时是订阅正文；Remote 时缓存上次拉取的内容
    #[serde(default, skip_serializing)]
    pub content: String,
    pub enabled: bool,
    /// 自动刷新间隔（秒）。0 表示不自动刷新。
    pub update_interval_secs: u64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    /// 上次刷新解析出的节点数
    #[serde(default)]
    pub node_count: u32,
    /// 上次刷新的错误信息
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// 粘性租约：把业务身份绑定到具体节点。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lease {
    /// 业务身份。当前用 provider id，后续可扩展成 API key 摘要。
    pub sticky_key: String,
    pub node_hash: String,
    /// 绑定时节点的出口 IP。节点挂掉时优先迁移到同 IP 节点。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub egress_ip: Option<String>,
    pub created_at_ms: i64,
    /// 最后一次命中时间，用于 TTL 淘汰
    pub last_used_at_ms: i64,
}

/// 池的调度参数
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolConfig {
    pub enabled: bool,
    /// 连续失败多少次触发熔断
    pub max_consecutive_failures: u32,
    /// 熔断冷却时长（秒），过后放行试探
    pub circuit_cooldown_secs: u64,
    /// 租约 TTL（秒）。超时未使用则解绑。
    pub lease_ttl_secs: u64,
    /// 主动探测间隔（秒）。0 表示关闭主动探测。
    pub probe_interval_secs: u64,
    /// 探测超时（秒）
    pub probe_timeout_secs: u64,
    /// 探测并发数
    pub probe_concurrency: usize,
    /// 出口 IP 探测地址
    pub egress_probe_url: String,
    /// 延迟探测地址
    pub latency_probe_url: String,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_consecutive_failures: 3,
            circuit_cooldown_secs: 300,
            lease_ttl_secs: 3600,
            probe_interval_secs: 600,
            probe_timeout_secs: 10,
            probe_concurrency: 16,
            egress_probe_url: "https://api.ipify.org".to_string(),
            latency_probe_url: "https://www.gstatic.com/generate_204".to_string(),
        }
    }
}

/// 池的聚合统计，给 UI 顶部状态区
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolStats {
    pub total_nodes: u32,
    pub healthy_nodes: u32,
    pub circuit_open_nodes: u32,
    /// 健康节点里去重后的出口 IP 数
    pub unique_egress_ips: u32,
    pub active_leases: u32,
    pub subscription_count: u32,
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn hex_prefix(bytes: &[u8], take: usize) -> String {
    let mut out = String::with_capacity(take * 2);
    for byte in bytes.iter().take(take) {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// 百分号编码 userinfo。保留 RFC 3986 的 unreserved 集合，其余全部转义。
fn encode_userinfo(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_stable_and_host_case_insensitive() {
        let a = ProxyNode::compute_hash(NodeProtocol::Http, "Example.COM", 8080, None, None);
        let b = ProxyNode::compute_hash(NodeProtocol::Http, "example.com", 8080, None, None);
        assert_eq!(a, b);
        assert_eq!(a.len(), 32);
    }

    #[test]
    fn hash_differs_on_credentials() {
        let a = ProxyNode::compute_hash(NodeProtocol::Http, "h", 1, Some("u"), Some("p1"));
        let b = ProxyNode::compute_hash(NodeProtocol::Http, "h", 1, Some("u"), Some("p2"));
        assert_ne!(a, b);
    }

    #[test]
    fn proxy_url_escapes_credentials() {
        let node = ProxyNode::new(
            NodeProtocol::Socks5,
            "1.2.3.4".into(),
            1080,
            Some("user@corp".into()),
            Some("p:a/s@s".into()),
            "t".into(),
        );
        let url = node.to_proxy_url();
        assert_eq!(url, "socks5://user%40corp:p%3Aa%2Fs%40s@1.2.3.4:1080");
        // 转义后必须仍能被 url crate 正确解析回原值
        let parsed = url::Url::parse(&url).expect("must parse");
        assert_eq!(parsed.username(), "user%40corp");
        assert_eq!(parsed.host_str(), Some("1.2.3.4"));
        assert_eq!(parsed.port(), Some(1080));
    }

    #[test]
    fn proxy_url_without_auth_has_no_at_sign() {
        let node = ProxyNode::new(
            NodeProtocol::Http,
            "example.com".into(),
            3128,
            None,
            None,
            "t".into(),
        );
        assert_eq!(node.to_proxy_url(), "http://example.com:3128");
    }

    #[test]
    fn circuit_opens_at_threshold_and_resets_on_success() {
        let mut health = NodeHealth::default();
        health.record_failure(3, 1_000);
        health.record_failure(3, 1_100);
        assert!(!health.is_circuit_open());
        health.record_failure(3, 1_200);
        assert!(health.is_circuit_open());
        assert_eq!(health.circuit_open_since_ms, 1_200);

        health.record_success(Some(50.0));
        assert!(!health.is_circuit_open());
        assert_eq!(health.failure_count, 0);
    }

    #[test]
    fn ewma_seeds_then_smooths() {
        let mut health = NodeHealth::default();
        health.record_success(Some(100.0));
        assert_eq!(health.latency_ewma_ms, Some(100.0));
        health.record_success(Some(200.0));
        // 100*0.7 + 200*0.3 = 130
        assert_eq!(health.latency_ewma_ms, Some(130.0));
    }

    #[test]
    fn cooldown_requires_full_interval() {
        let mut health = NodeHealth::default();
        health.record_failure(1, 1_000);
        assert!(health.is_circuit_open());
        assert!(!health.is_cooled_down(500, 1_400));
        assert!(health.is_cooled_down(500, 1_500));
    }

    #[test]
    fn protocol_parse_accepts_socks_aliases() {
        assert_eq!(NodeProtocol::parse("SOCKS5H"), Some(NodeProtocol::Socks5));
        assert_eq!(NodeProtocol::parse("socks"), Some(NodeProtocol::Socks5));
        assert_eq!(NodeProtocol::parse("vmess"), None);
    }
}
