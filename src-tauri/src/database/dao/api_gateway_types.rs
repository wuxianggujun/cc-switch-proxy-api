//! API 网关数据类型
//!
//! 序列化统一 camelCase 对齐前端。

use serde::{Deserialize, Serialize};

/// 上游协议族。决定用哪套转换器与请求路径。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpstreamType {
    Claude,
    Openai,
    Gemini,
    Codex,
    Deepseek,
}

impl UpstreamType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Openai => "openai",
            Self::Gemini => "gemini",
            Self::Codex => "codex",
            Self::Deepseek => "deepseek",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "claude" | "anthropic" => Some(Self::Claude),
            "openai" | "openai-compatible" => Some(Self::Openai),
            "gemini" | "google" => Some(Self::Gemini),
            "codex" => Some(Self::Codex),
            "deepseek" => Some(Self::Deepseek),
            _ => None,
        }
    }
}

/// 密钥被踢出候选集的原因。硬状态需外部信号才恢复，冷却到期自动恢复。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HardState {
    /// 额度耗尽。上游明确表示配额用尽，等重置或人工清除。
    QuotaExhausted,
    /// 鉴权失败。key 失效或被吊销。
    AuthInvalid,
    /// 被封禁。
    Banned,
}

impl HardState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::QuotaExhausted => "quota_exhausted",
            Self::AuthInvalid => "auth_invalid",
            Self::Banned => "banned",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "quota_exhausted" => Some(Self::QuotaExhausted),
            "auth_invalid" => Some(Self::AuthInvalid),
            "banned" => Some(Self::Banned),
            _ => None,
        }
    }
}

/// 接入点：一个上游地址 + 它支持的模型集合。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiEndpointRecord {
    pub id: String,
    pub name: String,
    pub upstream_type: UpstreamType,
    pub base_url: String,
    /// 空数组表示不限模型，任何 model 都可命中。
    pub models: Vec<String>,
    /// 层级优先级，越小越优先。与 sort_index（展示序）解耦。
    pub priority: i64,
    pub enabled: bool,
    pub sort_index: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    pub created_at: i64,
    /// 该接入点下的密钥数（列表页展示用，非表字段）。
    #[serde(default)]
    pub key_count: i64,
    /// 该接入点下已启用的密钥数（用于判断是否可以启用接入点）。
    #[serde(default)]
    pub enabled_key_count: i64,
}

/// 新建接入点入参。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewApiEndpoint {
    pub name: String,
    pub upstream_type: UpstreamType,
    pub base_url: String,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default = "default_priority")]
    pub priority: i64,
    #[serde(default)]
    pub notes: Option<String>,
}

fn default_priority() -> i64 {
    100
}

fn default_internal_priority() -> i64 {
    50
}

/// 不含 endpoint_id 的密钥值，供原子创建与换绑复用。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewApiKeyValue {
    pub api_key: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default = "default_internal_priority")]
    pub internal_priority: i64,
}

/// 原子创建接入点及首把密钥的入参。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewApiEndpointWithKey {
    pub endpoint: NewApiEndpoint,
    pub first_key: NewApiKeyValue,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedApiEndpoint {
    pub endpoint_id: String,
    pub key_id: String,
}

/// 接入点编辑入参。是否换绑由后端对比旧目标决定。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateApiEndpointInput {
    pub endpoint_id: String,
    pub name: String,
    pub upstream_type: UpstreamType,
    pub base_url: String,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default = "default_priority")]
    pub priority: i64,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub new_key: Option<NewApiKeyValue>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdatedApiEndpoint {
    pub endpoint_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_id: Option<String>,
    pub rebound: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyMutationOutcome {
    pub endpoint_id: String,
    pub endpoint_auto_disabled: bool,
}

/// 已保存接入点取模型时使用的进程内快照；明文密钥永不序列化或 Debug 输出。
#[derive(Clone)]
pub struct ApiEndpointModelFetchConfig {
    pub base_url: String,
    pub api_key: String,
    pub upstream_type: UpstreamType,
}

/// 密钥记录。明文 api_key 不下发前端，只给末四位。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyRecord {
    pub id: String,
    pub endpoint_id: String,
    /// 脱敏展示用，形如 `3IKa`
    pub key_last4: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 层内优先级，越小越优先。
    pub internal_priority: i64,
    pub enabled: bool,
    /// LRU 轮询依据（单调递增的 Unix 毫秒标记）。旧秒值首次使用后自动升级。
    /// NULL 表示从未使用，排最前。
    pub last_used_at: Option<i64>,
    pub cooldown_until: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cooldown_reason: Option<String>,
    pub hard_state: Option<HardState>,
    pub request_count: i64,
    pub success_count: i64,
    pub error_count: i64,
    pub total_tokens: i64,
    pub total_cost_usd: f64,
    pub last_error_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error_message: Option<String>,
    pub created_at: i64,
}

/// 新建密钥入参。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewApiKey {
    pub endpoint_id: String,
    pub api_key: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default = "default_internal_priority")]
    pub internal_priority: i64,
}

/// 选线候选。明文 key 只在进程内流转，不序列化给前端。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteCandidate {
    pub key_id: String,
    pub endpoint_id: String,
    pub endpoint_name: String,
    pub base_url: String,
    pub upstream_type: UpstreamType,
    pub key_last4: String,
    pub priority: i64,
    pub internal_priority: i64,
    pub last_used_at: Option<i64>,
    #[serde(skip_serializing)]
    pub api_key: String,
}

/// 取末四位用于展示；短密钥不返回任何原文，避免完整密钥下发前端。
pub fn last4(key: &str) -> String {
    let chars: Vec<char> = key.trim().chars().collect();
    if chars.len() <= 4 {
        return "*".repeat(chars.len());
    }
    chars[chars.len() - 4..].iter().collect()
}
