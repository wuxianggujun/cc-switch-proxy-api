//! 网关选线 → Provider 合成
//!
//! 新的 API 接入模型（`api_endpoints` + `api_keys`）与老的 `providers` 表并存。
//! 转发链、协议转换器、各 adapter 全都以 `Provider` 为输入，凭据一律从
//! `settings_config` JSON 里取；所以接线最省的做法是把选线结果**合成**成一个
//! 临时 Provider，而不是给整条链新增一个平行的参数类型。
//!
//! 合成出来的 Provider 只活在单次请求内，从不落库，也不参与 current provider
//! 的语义——轮询到第二条线路在新模型里是常态，不是"供应商切换"。

use serde_json::json;

use crate::database::{RouteCandidate, UpstreamType};
use crate::provider::{Provider, ProviderMeta};

/// 合成 Provider 的 id 前缀。带上它便于在日志与用量记录里区分两条链。
pub const GATEWAY_PROVIDER_PREFIX: &str = "gw:";

/// 这个 Provider 是否来自网关选线（而非 `providers` 表）。
///
/// 用于在 failover 切换、托盘同步等"改写 current provider"的路径上短路：
/// 网关链的线路轮换不该触发那些副作用。
pub fn is_gateway_provider(provider: &Provider) -> bool {
    provider.id.starts_with(GATEWAY_PROVIDER_PREFIX)
}

/// 从合成 Provider 的 id 里取回 key_id，用于记账。
pub fn gateway_key_id(provider: &Provider) -> Option<&str> {
    provider.id.strip_prefix(GATEWAY_PROVIDER_PREFIX)
}

/// 把选线候选合成为 Provider。
///
/// `settings_config` 的形状必须与各 adapter 的读取路径对齐，否则 adapter 会
/// 取不到凭据而报 ConfigError：
/// - Claude 读 `env.ANTHROPIC_BASE_URL` / `env.ANTHROPIC_AUTH_TOKEN`
/// - Codex 读 `base_url` / `env.OPENAI_API_KEY`
/// - Gemini 读 `env.GOOGLE_GEMINI_BASE_URL` / `env.GEMINI_API_KEY`
///
/// 同时冗余写入通用的 `base_url` 与 `api_key`，覆盖 adapter 的回退分支。
pub fn candidate_to_provider(candidate: &RouteCandidate) -> Provider {
    let base_url = candidate.base_url.trim_end_matches('/');
    let key = candidate.api_key.as_str();
    let full_endpoint_suffix = match candidate.upstream_type {
        UpstreamType::Claude => "/messages",
        UpstreamType::Openai | UpstreamType::Deepseek => "/chat/completions",
        UpstreamType::Codex => "/responses",
        UpstreamType::Gemini => "",
    };
    let is_full_url = !full_endpoint_suffix.is_empty()
        && url::Url::parse(base_url).ok().is_some_and(|url| {
            url.path()
                .trim_end_matches('/')
                .ends_with(full_endpoint_suffix)
        });

    let env = match candidate.upstream_type {
        UpstreamType::Claude => json!({
            "ANTHROPIC_BASE_URL": base_url,
            "ANTHROPIC_API_KEY": key,
        }),
        // Codex 与 DeepSeek 都走 OpenAI 兼容那套读取路径
        UpstreamType::Openai | UpstreamType::Codex | UpstreamType::Deepseek => json!({
            "OPENAI_API_KEY": key,
            "OPENAI_BASE_URL": base_url,
        }),
        UpstreamType::Gemini => json!({
            "GOOGLE_GEMINI_BASE_URL": base_url,
            "GEMINI_API_KEY": key,
        }),
    };

    let mut settings_config = json!({
        "env": env,
        "base_url": base_url,
        "api_key": key,
        "api_format": match candidate.upstream_type {
            UpstreamType::Claude => "anthropic",
            UpstreamType::Openai | UpstreamType::Deepseek => "openai_chat",
            UpstreamType::Codex => "openai_responses",
            UpstreamType::Gemini => "gemini_native",
        },
    });

    // The entry may be Anthropic, but OpenAI-family upstreams still require
    // Bearer authentication. Do not let the entry adapter choose x-api-key.
    if matches!(
        candidate.upstream_type,
        UpstreamType::Openai | UpstreamType::Codex | UpstreamType::Deepseek
    ) {
        settings_config["auth_mode"] = json!("bearer_only");
    }

    Provider {
        // key_id 而非 endpoint_id：熔断器、健康度、用量都该落在 key 粒度上，
        // 因为轮询和判罚都发生在这一层。
        id: format!("{GATEWAY_PROVIDER_PREFIX}{}", candidate.key_id),
        name: format!("{} ({})", candidate.endpoint_name, candidate.key_last4),
        settings_config,
        website_url: None,
        // 不能标 "official"——那个分类在老链上有跳过连通检测、跳过 failover 的
        // 特权，会让网关线路绕过判罚。
        category: None,
        created_at: None,
        sort_index: None,
        notes: None,
        meta: (candidate.upstream_type == UpstreamType::Claude || is_full_url).then(|| {
            ProviderMeta {
                api_key_field: (candidate.upstream_type == UpstreamType::Claude)
                    .then(|| "ANTHROPIC_API_KEY".into()),
                is_full_url: is_full_url.then_some(true),
                ..Default::default()
            }
        }),
        icon: None,
        icon_color: None,
        in_failover_queue: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(upstream: UpstreamType) -> RouteCandidate {
        RouteCandidate {
            key_id: "key_abc".to_string(),
            endpoint_id: "ep_1".to_string(),
            endpoint_name: "SotaModel".to_string(),
            base_url: "https://api.example.com/".to_string(),
            upstream_type: upstream,
            key_last4: "3IKa".to_string(),
            priority: 100,
            internal_priority: 50,
            last_used_at: None,
            api_key: "sk-secret-3IKa".to_string(),
        }
    }

    #[test]
    fn claude_credentials_land_where_the_adapter_reads_them() {
        let provider = candidate_to_provider(&candidate(UpstreamType::Claude));
        let env = &provider.settings_config["env"];
        // 尾斜杠必须去掉：adapter 自己会拼路径，重复斜杠会打到 404
        assert_eq!(env["ANTHROPIC_BASE_URL"], "https://api.example.com");
        assert_eq!(env["ANTHROPIC_API_KEY"], "sk-secret-3IKa");
    }

    #[test]
    fn openai_family_shares_one_shape() {
        for upstream in [
            UpstreamType::Openai,
            UpstreamType::Codex,
            UpstreamType::Deepseek,
        ] {
            let provider = candidate_to_provider(&candidate(upstream));
            assert_eq!(
                provider.settings_config["env"]["OPENAI_API_KEY"], "sk-secret-3IKa",
                "{upstream:?} 应写入 OPENAI_API_KEY"
            );
            // Codex adapter 从顶层 base_url 读取
            assert_eq!(
                provider.settings_config["base_url"],
                "https://api.example.com"
            );
        }
    }

    #[test]
    fn gemini_credentials_land_where_the_adapter_reads_them() {
        let provider = candidate_to_provider(&candidate(UpstreamType::Gemini));
        let env = &provider.settings_config["env"];
        assert_eq!(env["GOOGLE_GEMINI_BASE_URL"], "https://api.example.com");
        assert_eq!(env["GEMINI_API_KEY"], "sk-secret-3IKa");
    }

    #[test]
    fn synthetic_provider_is_identifiable_and_carries_key_id() {
        let provider = candidate_to_provider(&candidate(UpstreamType::Claude));
        assert!(is_gateway_provider(&provider));
        assert_eq!(gateway_key_id(&provider), Some("key_abc"));

        // 老链的 provider 不该被误判
        let legacy = Provider::with_id("abc123".to_string(), "Legacy".to_string(), json!({}), None);
        assert!(!is_gateway_provider(&legacy));
        assert_eq!(gateway_key_id(&legacy), None);
    }

    #[test]
    fn synthetic_provider_avoids_official_privileges() {
        let provider = candidate_to_provider(&candidate(UpstreamType::Claude));
        // category=="official" 会跳过连通检测与 failover，网关线路必须参与判罚
        assert_ne!(provider.category.as_deref(), Some("official"));
        assert!(!provider.in_failover_queue);
    }

    #[test]
    fn display_name_masks_the_secret() {
        let provider = candidate_to_provider(&candidate(UpstreamType::Claude));
        assert!(provider.name.contains("SotaModel"));
        assert!(provider.name.contains("3IKa"));
        assert!(!provider.name.contains("sk-secret"));
    }
}
