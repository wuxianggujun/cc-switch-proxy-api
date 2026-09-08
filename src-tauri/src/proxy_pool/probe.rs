//! 节点探测
//!
//! 两类探测共享单节点超时预算：
//! - 出口 IP：请求回显服务，响应体就是出口 IP，粘性路由靠它做同 IP 迁移
//! - 延迟：访问配置的延迟目标；留空或与回显地址相同时复用回显耗时
//!
//! reqwest 的代理是 client 级配置，所以每个节点都要单独建 client。探测频率低
//! （默认 10 分钟一轮），这个开销可以接受。

use super::types::ProxyNode;
use std::time::{Duration, Instant};

/// 单节点探测结果
#[derive(Debug, Clone)]
pub struct ProbeResult {
    pub node_hash: String,
    pub success: bool,
    pub egress_ip: Option<String>,
    pub latency_ms: Option<f64>,
    pub error: Option<String>,
}

impl ProbeResult {
    fn failed(node_hash: String, error: String) -> Self {
        Self {
            node_hash,
            success: false,
            egress_ip: None,
            latency_ms: None,
            error: Some(error),
        }
    }
}

/// 出口 IP 回显响应体的最大接受长度。
/// 正常响应是一个 IP 字符串（最长 45 字节的 IPv6），给足余量后仍能挡住
/// 代理返回错误页面这类大响应。
const MAX_EGRESS_BODY_BYTES: usize = 256;

/// 探测单个节点。
///
/// IP echo and optional latency target share one overall timeout budget.
/// An empty/equal latency URL reuses the echo request's header latency.
pub async fn probe_node(
    node: &ProxyNode,
    egress_url: &str,
    latency_url: &str,
    timeout: Duration,
) -> ProbeResult {
    let client = match build_probe_client(node, timeout) {
        Ok(client) => client,
        Err(e) => return ProbeResult::failed(node.hash.clone(), e),
    };

    let started = Instant::now();
    let mut response = match client.get(egress_url).send().await {
        Ok(resp) => resp,
        Err(e) => {
            return ProbeResult::failed(node.hash.clone(), summarize_reqwest_error(&e));
        }
    };
    // 响应头到达即计延迟，不含读 body 的时间
    let latency_ms = started.elapsed().as_secs_f64() * 1000.0;

    let status = response.status();
    if !status.is_success() {
        return ProbeResult {
            node_hash: node.hash.clone(),
            success: false,
            egress_ip: None,
            // 状态码非 2xx 说明链路是通的，延迟数据仍然有效
            latency_ms: Some(latency_ms),
            error: Some(format!("HTTP {}", status.as_u16())),
        };
    }

    let mut body = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if body.len().saturating_add(chunk.len()) > MAX_EGRESS_BODY_BYTES {
                    return ProbeResult::failed(node.hash.clone(), "出口探测响应过大".into());
                }
                body.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(error) => {
                return ProbeResult::failed(node.hash.clone(), summarize_reqwest_error(&error))
            }
        }
    }
    let egress_ip = std::str::from_utf8(&body).ok().and_then(parse_egress_ip);
    if egress_ip.is_none() {
        return ProbeResult::failed(node.hash.clone(), "出口探测没有返回有效 IP".into());
    }

    let latency_ms = if latency_url.trim().is_empty() || latency_url.trim() == egress_url.trim() {
        latency_ms
    } else {
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return ProbeResult::failed(node.hash.clone(), "超时".into());
        }
        let latency_started = Instant::now();
        match client
            .get(latency_url.trim())
            .timeout(remaining)
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => {
                latency_started.elapsed().as_secs_f64() * 1000.0
            }
            Ok(response) => {
                return ProbeResult::failed(
                    node.hash.clone(),
                    format!("延迟探测 HTTP {}", response.status().as_u16()),
                )
            }
            Err(error) => {
                return ProbeResult::failed(node.hash.clone(), summarize_reqwest_error(&error))
            }
        }
    };

    ProbeResult {
        node_hash: node.hash.clone(),
        success: true,
        egress_ip,
        latency_ms: Some(latency_ms),
        error: None,
    }
}

/// 并发探测一批节点，按 concurrency 分批避免瞬时打满连接数
pub async fn probe_batch(
    nodes: &[ProxyNode],
    egress_url: &str,
    latency_url: &str,
    timeout: Duration,
    concurrency: usize,
) -> Vec<ProbeResult> {
    let concurrency = concurrency.max(1);
    let mut results = Vec::with_capacity(nodes.len());

    for chunk in nodes.chunks(concurrency) {
        let batch = chunk
            .iter()
            .map(|node| probe_node(node, egress_url, latency_url, timeout));
        results.extend(futures::future::join_all(batch).await);
    }

    results
}

fn build_probe_client(node: &ProxyNode, timeout: Duration) -> Result<reqwest::Client, String> {
    let proxy = reqwest::Proxy::all(node.to_proxy_url())
        .map_err(|e| format!("代理地址无效 ({}): {e}", node.endpoint()))?;

    reqwest::Client::builder()
        .proxy(proxy)
        .timeout(timeout)
        .connect_timeout(timeout)
        // 探测不复用连接，避免坏节点的死连接留在池里影响后续判断
        .pool_max_idle_per_host(0)
        .build()
        .map_err(|e| format!("构建探测客户端失败: {e}"))
}

/// 从回显响应体里取 IP。响应可能带换行或被包在 JSON 里，
/// 这里只接受纯 IP 形式，其余按未知处理。
fn parse_egress_ip(body: &str) -> Option<String> {
    if body.len() > MAX_EGRESS_BODY_BYTES {
        return None;
    }
    let trimmed = body.trim();
    trimmed
        .parse::<std::net::IpAddr>()
        .ok()
        .map(|ip| ip.to_string())
}

/// 把 reqwest 错误压缩成短原因，避免把完整 URL（可能含凭据）写进 UI
fn summarize_reqwest_error(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        return "超时".to_string();
    }
    if error.is_connect() {
        return "连接失败".to_string();
    }
    if error.is_request() {
        return "请求失败".to_string();
    }
    "探测失败".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy_pool::types::NodeProtocol;

    fn node(port: u16) -> ProxyNode {
        ProxyNode::new(
            NodeProtocol::Http,
            "127.0.0.1".into(),
            port,
            None,
            None,
            format!("n{port}"),
        )
    }

    #[test]
    fn parses_ipv4_and_ipv6_echo_bodies() {
        assert_eq!(parse_egress_ip("1.2.3.4"), Some("1.2.3.4".to_string()));
        assert_eq!(parse_egress_ip("  1.2.3.4\n"), Some("1.2.3.4".to_string()));
        assert_eq!(
            parse_egress_ip("2001:db8::1"),
            Some("2001:db8::1".to_string())
        );
    }

    #[test]
    fn rejects_non_ip_and_oversized_bodies() {
        assert_eq!(parse_egress_ip("<html>error</html>"), None);
        assert_eq!(parse_egress_ip(""), None);
        assert_eq!(parse_egress_ip(&"1".repeat(300)), None);
        // JSON 包裹的形式不接受，避免误把片段当 IP
        assert_eq!(parse_egress_ip("{\"ip\":\"1.2.3.4\"}"), None);
    }

    #[test]
    fn builds_client_for_valid_node() {
        let result = build_probe_client(&node(1080), Duration::from_secs(5));
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn probe_fails_fast_on_dead_port() {
        // 9 端口（discard）通常无监听，用于验证失败路径不 panic 且返回错误
        let result = probe_node(
            &node(9),
            "http://example.invalid",
            "",
            Duration::from_millis(300),
        )
        .await;
        assert!(!result.success);
        assert!(result.error.is_some());
        assert_eq!(result.egress_ip, None);
    }

    #[tokio::test]
    async fn probe_batch_returns_one_result_per_node() {
        let nodes = vec![node(9), node(10), node(11)];
        let results = probe_batch(
            &nodes,
            "http://example.invalid",
            "",
            Duration::from_millis(200),
            2,
        )
        .await;
        assert_eq!(results.len(), 3);
        // 全部失败但都有结果，且 hash 一一对应
        let hashes: Vec<&str> = results.iter().map(|r| r.node_hash.as_str()).collect();
        for n in &nodes {
            assert!(hashes.contains(&n.hash.as_str()));
        }
    }

    #[tokio::test]
    async fn probe_batch_handles_empty_input() {
        let results =
            probe_batch(&[], "http://example.invalid", "", Duration::from_secs(1), 4).await;
        assert!(results.is_empty());
    }
}
