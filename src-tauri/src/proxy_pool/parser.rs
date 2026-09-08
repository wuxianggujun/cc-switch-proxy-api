//! 订阅内容解析
//!
//! 支持三种格式，按内容嗅探自动判定：
//! - URI 行：`http://` `https://` `socks5://` `socks5h://`，可整体 base64 包裹
//! - Clash：YAML 或 JSON 的 `proxies:` 数组
//! - sing-box：JSON 的 `outbounds` 数组或裸数组
//!
//! 只提取 HTTP/SOCKS5 节点。其他协议（vmess/vless/trojan/ss/hysteria2）会被
//! 跳过并计入 `skipped`，因为出站实现依赖 reqwest 原生能力。

use super::types::{NodeProtocol, ProxyNode};
use base64::Engine;
use serde_json::Value;

#[derive(Debug, Default)]
pub struct ParseOutcome {
    pub nodes: Vec<ProxyNode>,
    /// 因协议不支持而跳过的节点数
    pub skipped_unsupported: usize,
    /// 跳过的协议名，去重后用于给用户提示
    pub skipped_protocols: Vec<String>,
}

impl ParseOutcome {
    fn note_skip(&mut self, protocol: &str) {
        self.skipped_unsupported += 1;
        let name = protocol.trim().to_ascii_lowercase();
        if !name.is_empty() && !self.skipped_protocols.contains(&name) {
            self.skipped_protocols.push(name);
        }
    }

    /// 按 hash 去重，保留首次出现的节点
    fn dedup(&mut self) {
        let mut seen = std::collections::HashSet::new();
        self.nodes.retain(|node| seen.insert(node.hash.clone()));
    }
}

/// 解析订阅正文。空内容返回空结果而非报错 —— 订阅可能暂时为空。
pub fn parse_subscription(raw: &str) -> Result<ParseOutcome, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(ParseOutcome::default());
    }

    // base64 整体包裹是 URI 行格式的常见变体，先尝试解开
    let decoded = try_decode_base64(trimmed);
    let content = decoded.as_deref().unwrap_or(trimmed);

    let mut outcome = if looks_like_json(content) {
        parse_json(content)?
    } else if let Some(result) = try_parse_clash_yaml(content) {
        result?
    } else {
        parse_uri_lines(content)
    };

    outcome.dedup();
    Ok(outcome)
}

fn looks_like_json(content: &str) -> bool {
    let first = content.trim_start().chars().next();
    matches!(first, Some('{') | Some('['))
}

/// 整体 base64 解码。只在解码结果看起来像订阅内容时才采用，
/// 避免把恰好符合 base64 字符集的明文误解码成乱码。
fn try_decode_base64(content: &str) -> Option<String> {
    let compact: String = content.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.len() < 8 {
        return None;
    }
    // 宽松引擎：容忍无 padding 与 URL-safe 变体
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&compact)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(&compact))
        .ok()?;
    let text = String::from_utf8(bytes).ok()?;
    let looks_like_subscription = text.contains("://") || looks_like_json(&text);
    looks_like_subscription.then_some(text)
}

fn parse_json(content: &str) -> Result<ParseOutcome, String> {
    let value: Value = serde_json::from_str(content).map_err(|e| format!("JSON 解析失败: {e}"))?;

    let mut outcome = ParseOutcome::default();

    // Clash: {"proxies": [...]}
    if let Some(proxies) = value.get("proxies").and_then(Value::as_array) {
        for item in proxies {
            collect_clash_proxy(item, &mut outcome);
        }
        return Ok(outcome);
    }

    // sing-box: {"outbounds": [...]}
    if let Some(outbounds) = value.get("outbounds").and_then(Value::as_array) {
        for item in outbounds {
            collect_singbox_outbound(item, &mut outcome);
        }
        return Ok(outcome);
    }

    // 裸数组：两种格式都可能，逐个试
    if let Some(items) = value.as_array() {
        for item in items {
            // sing-box 的 server/server_port 与 Clash 的 server/port 靠字段名区分
            if item.get("server_port").is_some() {
                collect_singbox_outbound(item, &mut outcome);
            } else {
                collect_clash_proxy(item, &mut outcome);
            }
        }
        return Ok(outcome);
    }

    Err("JSON 中未找到 proxies 或 outbounds 数组".to_string())
}

/// Clash YAML。返回 None 表示内容不像 Clash 配置，应回退到 URI 行解析。
fn try_parse_clash_yaml(content: &str) -> Option<Result<ParseOutcome, String>> {
    if !content.contains("proxies:") {
        return None;
    }
    let value: serde_yaml::Value = match serde_yaml::from_str(content) {
        Ok(v) => v,
        Err(e) => return Some(Err(format!("YAML 解析失败: {e}"))),
    };
    let proxies = value.get("proxies")?.as_sequence()?;

    let mut outcome = ParseOutcome::default();
    for item in proxies {
        // 转成 serde_json::Value 复用同一套字段提取
        match serde_json::to_value(item) {
            Ok(json) => collect_clash_proxy(&json, &mut outcome),
            Err(_) => continue,
        }
    }
    Some(Ok(outcome))
}

/// Clash 节点：`{type, name, server, port, username, password}`
fn collect_clash_proxy(item: &Value, outcome: &mut ParseOutcome) {
    let raw_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
    let Some(protocol) = NodeProtocol::parse(raw_type) else {
        outcome.note_skip(raw_type);
        return;
    };

    let Some(host) = item.get("server").and_then(Value::as_str) else {
        return;
    };
    let Some(port) = extract_port(item.get("port")) else {
        return;
    };

    // Clash 的 http 节点用 tls: true 表示 https
    let protocol = if protocol == NodeProtocol::Http
        && item.get("tls").and_then(Value::as_bool).unwrap_or(false)
    {
        NodeProtocol::Https
    } else {
        protocol
    };

    let tag = item
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(host)
        .to_string();

    outcome.nodes.push(ProxyNode::new(
        protocol,
        host.to_string(),
        port,
        non_empty(item.get("username").and_then(Value::as_str)),
        non_empty(item.get("password").and_then(Value::as_str)),
        tag,
    ));
}

/// sing-box outbound：`{type, tag, server, server_port, username, password}`
fn collect_singbox_outbound(item: &Value, outcome: &mut ParseOutcome) {
    let raw_type = item.get("type").and_then(Value::as_str).unwrap_or_default();

    // sing-box 的控制类 outbound 不是真实节点，静默跳过不计入 skipped
    if matches!(
        raw_type,
        "direct" | "block" | "dns" | "selector" | "urltest" | ""
    ) {
        return;
    }

    let Some(protocol) = NodeProtocol::parse(raw_type) else {
        outcome.note_skip(raw_type);
        return;
    };

    let Some(host) = item.get("server").and_then(Value::as_str) else {
        return;
    };
    let Some(port) = extract_port(item.get("server_port")) else {
        return;
    };

    // sing-box 用嵌套的 tls.enabled 表示 https
    let tls_on = item
        .get("tls")
        .and_then(|tls| tls.get("enabled"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let protocol = if protocol == NodeProtocol::Http && tls_on {
        NodeProtocol::Https
    } else {
        protocol
    };

    let tag = item
        .get("tag")
        .and_then(Value::as_str)
        .unwrap_or(host)
        .to_string();

    outcome.nodes.push(ProxyNode::new(
        protocol,
        host.to_string(),
        port,
        non_empty(item.get("username").and_then(Value::as_str)),
        non_empty(item.get("password").and_then(Value::as_str)),
        tag,
    ));
}

/// URI 行格式，每行一个节点，`#` 后是 tag
fn parse_uri_lines(content: &str) -> ParseOutcome {
    let mut outcome = ParseOutcome::default();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }
        let Some((scheme, _)) = line.split_once("://") else {
            continue;
        };
        if NodeProtocol::parse(scheme).is_none() {
            outcome.note_skip(scheme);
            continue;
        }
        match parse_proxy_uri(line) {
            Some(node) => outcome.nodes.push(node),
            None => continue,
        }
    }

    outcome
}

/// 解析单条 `scheme://[user:pass@]host:port[#tag]`
fn parse_proxy_uri(raw: &str) -> Option<ProxyNode> {
    let parsed = url::Url::parse(raw).ok()?;
    let protocol = NodeProtocol::parse(parsed.scheme())?;
    let host = parsed.host_str()?.to_string();
    // 无显式端口时按协议取默认值
    let port = parsed.port().or_else(|| match protocol {
        NodeProtocol::Http => Some(80),
        NodeProtocol::Https => Some(443),
        NodeProtocol::Socks5 => Some(1080),
    })?;

    // url crate 已做百分号解码前的原始值，这里还原成明文供 to_proxy_url 重新编码
    let username = percent_decode(parsed.username());
    let password = parsed.password().and_then(percent_decode);

    let tag = parsed
        .fragment()
        .and_then(percent_decode)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("{host}:{port}"));

    Some(ProxyNode::new(
        protocol,
        host,
        port,
        username.filter(|s| !s.is_empty()),
        password.filter(|s| !s.is_empty()),
        tag,
    ))
}

fn percent_decode(raw: &str) -> Option<String> {
    percent_encoding::percent_decode_str(raw)
        .decode_utf8()
        .ok()
        .map(|s| s.to_string())
}

/// 端口可能是数字或字符串，两种都收
fn extract_port(value: Option<&Value>) -> Option<u16> {
    let value = value?;
    if let Some(n) = value.as_u64() {
        return u16::try_from(n).ok().filter(|p| *p > 0);
    }
    value
        .as_str()?
        .trim()
        .parse::<u16>()
        .ok()
        .filter(|p| *p > 0)
}

fn non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_uri_lines_with_auth_and_tag() {
        let raw = "socks5://user:pass@1.2.3.4:1080#HK-01\nhttp://5.6.7.8:3128#US-01";
        let out = parse_subscription(raw).expect("parse ok");
        assert_eq!(out.nodes.len(), 2);

        let hk = &out.nodes[0];
        assert_eq!(hk.protocol, NodeProtocol::Socks5);
        assert_eq!(hk.host, "1.2.3.4");
        assert_eq!(hk.port, 1080);
        assert_eq!(hk.username.as_deref(), Some("user"));
        assert_eq!(hk.password.as_deref(), Some("pass"));
        assert_eq!(hk.tag, "HK-01");

        assert_eq!(out.nodes[1].tag, "US-01");
        assert_eq!(out.nodes[1].username, None);
    }

    #[test]
    fn skips_unsupported_protocols_and_reports_them() {
        let raw = "vmess://abc\nsocks5://1.2.3.4:1080\ntrojan://xyz\nvmess://def";
        let out = parse_subscription(raw).expect("parse ok");
        assert_eq!(out.nodes.len(), 1);
        assert_eq!(out.skipped_unsupported, 3);
        // 去重后只剩两种协议名
        assert_eq!(out.skipped_protocols.len(), 2);
        assert!(out.skipped_protocols.contains(&"vmess".to_string()));
        assert!(out.skipped_protocols.contains(&"trojan".to_string()));
    }

    #[test]
    fn decodes_base64_wrapped_uri_list() {
        let plain = "socks5://1.2.3.4:1080#N1\nhttp://5.6.7.8:8080#N2";
        let encoded = base64::engine::general_purpose::STANDARD.encode(plain);
        let out = parse_subscription(&encoded).expect("parse ok");
        assert_eq!(out.nodes.len(), 2);
        assert_eq!(out.nodes[0].tag, "N1");
    }

    #[test]
    fn parses_clash_yaml() {
        let raw = r#"
proxies:
  - name: "SOCKS-A"
    type: socks5
    server: 10.0.0.1
    port: 1080
    username: u1
    password: p1
  - name: "VMESS-B"
    type: vmess
    server: 10.0.0.2
    port: 443
  - name: "HTTPS-C"
    type: http
    server: 10.0.0.3
    port: 8443
    tls: true
"#;
        let out = parse_subscription(raw).expect("parse ok");
        assert_eq!(out.nodes.len(), 2);
        assert_eq!(out.nodes[0].tag, "SOCKS-A");
        assert_eq!(out.nodes[0].username.as_deref(), Some("u1"));
        // tls: true 把 http 提升为 https
        assert_eq!(out.nodes[1].protocol, NodeProtocol::Https);
        assert_eq!(out.skipped_unsupported, 1);
    }

    #[test]
    fn parses_singbox_json_and_ignores_control_outbounds() {
        let raw = r#"{
          "outbounds": [
            {"type": "direct", "tag": "direct"},
            {"type": "selector", "tag": "auto", "outbounds": ["a"]},
            {"type": "socks", "tag": "S1", "server": "1.1.1.1", "server_port": 1080},
            {"type": "http", "tag": "H1", "server": "2.2.2.2", "server_port": 8080,
             "username": "u", "password": "p", "tls": {"enabled": true}},
            {"type": "vless", "tag": "V1", "server": "3.3.3.3", "server_port": 443}
          ]
        }"#;
        let out = parse_subscription(raw).expect("parse ok");
        assert_eq!(out.nodes.len(), 2);
        assert_eq!(out.nodes[0].tag, "S1");
        assert_eq!(out.nodes[0].protocol, NodeProtocol::Socks5);
        assert_eq!(out.nodes[1].protocol, NodeProtocol::Https);
        // direct/selector 不计入 skipped，只有 vless 计入
        assert_eq!(out.skipped_unsupported, 1);
        assert_eq!(out.skipped_protocols, vec!["vless".to_string()]);
    }

    #[test]
    fn dedups_identical_nodes_across_formats() {
        let raw = "socks5://1.2.3.4:1080#A\nsocks5://1.2.3.4:1080#B";
        let out = parse_subscription(raw).expect("parse ok");
        // 同 host/port/凭据 → 同 hash，只保留第一个
        assert_eq!(out.nodes.len(), 1);
        assert_eq!(out.nodes[0].tag, "A");
    }

    #[test]
    fn accepts_string_port_in_clash() {
        let raw = r#"{"proxies":[{"type":"socks5","name":"S","server":"1.1.1.1","port":"1080"}]}"#;
        let out = parse_subscription(raw).expect("parse ok");
        assert_eq!(out.nodes.len(), 1);
        assert_eq!(out.nodes[0].port, 1080);
    }

    #[test]
    fn empty_content_yields_empty_result() {
        let out = parse_subscription("   \n  ").expect("parse ok");
        assert!(out.nodes.is_empty());
        assert_eq!(out.skipped_unsupported, 0);
    }

    #[test]
    fn uri_without_port_uses_protocol_default() {
        let out = parse_subscription("http://example.com#H").expect("parse ok");
        assert_eq!(out.nodes[0].port, 80);
        let out = parse_subscription("socks5://example.com#S").expect("parse ok");
        assert_eq!(out.nodes[0].port, 1080);
    }

    #[test]
    fn decodes_percent_encoded_credentials() {
        let out = parse_subscription("socks5://u%40corp:p%3As@1.2.3.4:1080#T").expect("parse ok");
        let node = &out.nodes[0];
        assert_eq!(node.username.as_deref(), Some("u@corp"));
        assert_eq!(node.password.as_deref(), Some("p:s"));
        // 往返：重新编码后仍是合法 URL
        assert_eq!(node.to_proxy_url(), "socks5h://u%40corp:p%3As@1.2.3.4:1080");
    }

    #[test]
    fn malformed_json_reports_error() {
        assert!(parse_subscription("{not json").is_err());
    }
}
