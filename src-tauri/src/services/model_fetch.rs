//! 模型列表获取服务
//!
//! 通过 OpenAI 兼容的 GET /v1/models 端点获取供应商可用模型列表。
//! 主要面向第三方聚合站（硅基流动、OpenRouter 等），以及把 Anthropic
//! 协议挂在兼容子路径上的官方供应商（DeepSeek、Kimi、智谱 GLM 等）。

use crate::database::{validate_endpoint_url, UpstreamType};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION, USER_AGENT};
use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;
use url::Url;

/// 获取到的模型信息
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchedModel {
    pub id: String,
    pub owned_by: Option<String>,
}

/// OpenAI 兼容的 /v1/models 响应格式
#[derive(Debug, Deserialize)]
struct ModelsResponse {
    data: Option<Vec<ModelEntry>>,
}

#[derive(Debug, Deserialize)]
struct ModelEntry {
    id: String,
    owned_by: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ClaudeModelsResponse {
    #[serde(default)]
    data: Vec<ModelEntry>,
    #[serde(default)]
    has_more: bool,
    last_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GeminiModelsResponse {
    #[serde(default)]
    models: Vec<GeminiModelEntry>,
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GeminiModelEntry {
    name: String,
}

const FETCH_TIMEOUT_SECS: u64 = 15;
const MAX_GATEWAY_MODEL_PAGES: usize = 20;
const MAX_REQUEST_HEADERS: usize = 64;
const MAX_HEADER_NAME_BYTES: usize = 256;
const MAX_HEADER_VALUE_BYTES: usize = 16 * 1024;

/// 404/405 响应体截断长度：避免把几十 KB HTML 404 页整页保留到错误串里。
const ERROR_BODY_MAX_CHARS: usize = 512;

/// 已知的「Anthropic 协议兼容子路径」后缀；按长度降序，最长前缀优先匹配。
/// baseURL 命中这些后缀时，候选列表会追加「剥离后缀再拼 /v1/models / /models」的版本。
const KNOWN_COMPAT_SUFFIXES: &[&str] = &[
    "/api/claudecode",
    "/api/anthropic",
    "/apps/anthropic",
    "/api/coding",
    "/claudecode",
    "/anthropic",
    "/step_plan",
    "/coding",
    "/claude",
];

/// 获取供应商的可用模型列表
///
/// 使用 OpenAI 兼容的 GET /v1/models 端点，按候选列表顺序尝试。
pub async fn fetch_models(
    base_url: &str,
    api_key: &str,
    is_full_url: bool,
    models_url_override: Option<&str>,
    user_agent: Option<HeaderValue>,
    api_format: Option<&str>,
    request_headers: Option<&BTreeMap<String, String>>,
) -> Result<Vec<FetchedModel>, String> {
    let candidates = build_models_url_candidates(base_url, is_full_url, models_url_override)?;
    let headers =
        build_model_fetch_headers(api_key, api_format, user_agent.as_ref(), request_headers)?;
    let client = crate::proxy::http_client::get();
    let mut last_err: Option<String> = None;
    let mut known_secrets = vec![api_key.to_string()];
    if let Some(request_headers) = request_headers {
        known_secrets.extend(request_headers.values().cloned());
    }

    for url in &candidates {
        log::debug!(
            "[ModelFetch] Trying endpoint: {}",
            crate::url_for_log_with_secrets(url, &known_secrets)
        );
        let request = client
            .get(url)
            .headers(headers.clone())
            .timeout(Duration::from_secs(FETCH_TIMEOUT_SECS));
        let response = match request.send().await {
            Ok(r) => r,
            Err(e) => {
                return Err(format!("Request failed: {e}"));
            }
        };

        let status = response.status();

        if status.is_success() {
            let resp: ModelsResponse = response
                .json()
                .await
                .map_err(|e| format!("Failed to parse response: {e}"))?;

            let mut models: Vec<FetchedModel> = resp
                .data
                .unwrap_or_default()
                .into_iter()
                .map(|m| FetchedModel {
                    id: m.id,
                    owned_by: m.owned_by,
                })
                .collect();

            models.sort_by(|a, b| a.id.cmp(&b.id));
            return Ok(models);
        }

        if status == StatusCode::NOT_FOUND || status == StatusCode::METHOD_NOT_ALLOWED {
            let body = redact_model_fetch_error_body(
                response.text().await.unwrap_or_default(),
                &known_secrets,
            );
            last_err = Some(format!("HTTP {status}: {body}"));
            continue;
        }

        let body = redact_model_fetch_error_body(
            response.text().await.unwrap_or_default(),
            &known_secrets,
        );
        return Err(format!("HTTP {status}: {body}"));
    }

    Err(format!(
        "All candidates failed: {}",
        last_err.unwrap_or_else(|| "no candidates".to_string())
    ))
}

/// 按网关上游协议获取模型列表。认证值只进入请求头，不进入 URL、日志或错误。
pub async fn fetch_gateway_models(
    base_url: &str,
    api_key: &str,
    upstream_type: UpstreamType,
) -> Result<Vec<FetchedModel>, String> {
    let base_url = base_url.trim();
    let api_key = api_key.trim();
    validate_endpoint_url(base_url).map_err(|e| e.to_string())?;
    if api_key.is_empty() {
        return Err("密钥不能为空".to_string());
    }

    let headers = build_gateway_headers(api_key, upstream_type)?;
    let mut models = Vec::new();

    match upstream_type {
        UpstreamType::Claude => {
            let mut url = gateway_models_url(base_url, upstream_type)?;
            let mut seen_page_tokens = BTreeSet::new();
            for page in 0..MAX_GATEWAY_MODEL_PAGES {
                let response: ClaudeModelsResponse = fetch_gateway_page(&url, &headers, api_key)
                    .await
                    .map_err(GatewayPageError::into_message)?;
                models.extend(response.data.into_iter().map(|model| FetchedModel {
                    id: model.id,
                    owned_by: model.owned_by,
                }));
                if !response.has_more {
                    break;
                }
                let last_id = response
                    .last_id
                    .ok_or_else(|| "Claude 模型分页响应缺少 last_id".to_string())?;
                if !seen_page_tokens.insert(last_id.clone()) {
                    return Err("Claude 模型分页游标重复".to_string());
                }
                if page + 1 == MAX_GATEWAY_MODEL_PAGES {
                    return Err(format!("模型分页超过安全上限 {MAX_GATEWAY_MODEL_PAGES}"));
                }
                url.query_pairs_mut()
                    .clear()
                    .append_pair("after_id", &last_id);
            }
        }
        UpstreamType::Gemini => {
            let mut url = gateway_models_url(base_url, upstream_type)?;
            let mut seen_page_tokens = BTreeSet::new();
            for page in 0..MAX_GATEWAY_MODEL_PAGES {
                let response: GeminiModelsResponse = fetch_gateway_page(&url, &headers, api_key)
                    .await
                    .map_err(GatewayPageError::into_message)?;
                models.extend(response.models.into_iter().filter_map(|model| {
                    let id = model.name.strip_prefix("models/").unwrap_or(&model.name);
                    (!id.is_empty()).then(|| FetchedModel {
                        id: id.to_string(),
                        owned_by: Some("google".to_string()),
                    })
                }));
                let Some(next_page_token) = response
                    .next_page_token
                    .filter(|token| !token.trim().is_empty())
                else {
                    break;
                };
                if !seen_page_tokens.insert(next_page_token.clone()) {
                    return Err("Gemini 模型分页游标重复".to_string());
                }
                if page + 1 == MAX_GATEWAY_MODEL_PAGES {
                    return Err(format!("模型分页超过安全上限 {MAX_GATEWAY_MODEL_PAGES}"));
                }
                url.query_pairs_mut()
                    .clear()
                    .append_pair("pageToken", &next_page_token);
            }
        }
        UpstreamType::Openai | UpstreamType::Codex | UpstreamType::Deepseek => {
            let candidates = gateway_openai_models_url_candidates(base_url)?;
            let response: ModelsResponse =
                fetch_gateway_candidate(&candidates, &headers, api_key).await?;
            models.extend(response.data.unwrap_or_default().into_iter().map(|model| {
                FetchedModel {
                    id: model.id,
                    owned_by: model.owned_by,
                }
            }));
        }
    }

    // ID 是前端唯一选择值；排序与去重确保分页重叠不会产生重复项。
    models.sort_by(|left, right| left.id.cmp(&right.id));
    models.dedup_by(|left, right| left.id == right.id);
    Ok(models)
}

fn build_gateway_headers(api_key: &str, upstream_type: UpstreamType) -> Result<HeaderMap, String> {
    let value =
        HeaderValue::from_str(api_key).map_err(|_| "密钥无法用于 HTTP 请求头".to_string())?;
    let mut headers = HeaderMap::new();
    match upstream_type {
        UpstreamType::Claude => {
            headers.insert(HeaderName::from_static("x-api-key"), value);
            headers.insert(
                HeaderName::from_static("anthropic-version"),
                HeaderValue::from_static("2023-06-01"),
            );
        }
        UpstreamType::Gemini => {
            headers.insert(HeaderName::from_static("x-goog-api-key"), value);
        }
        UpstreamType::Openai | UpstreamType::Codex | UpstreamType::Deepseek => {
            let bearer = HeaderValue::from_str(&format!("Bearer {api_key}"))
                .map_err(|_| "密钥无法用于 HTTP 请求头".to_string())?;
            headers.insert(AUTHORIZATION, bearer);
        }
    }
    Ok(headers)
}

fn gateway_models_url(base_url: &str, upstream_type: UpstreamType) -> Result<Url, String> {
    let base_url = base_url.trim_end_matches('/');
    let suffix = match upstream_type {
        UpstreamType::Gemini if base_url.ends_with("/v1beta") => "/models",
        UpstreamType::Gemini => "/v1beta/models",
        UpstreamType::Claude
        | UpstreamType::Openai
        | UpstreamType::Codex
        | UpstreamType::Deepseek
            if ends_with_version_segment(base_url) =>
        {
            "/models"
        }
        UpstreamType::Claude
        | UpstreamType::Openai
        | UpstreamType::Codex
        | UpstreamType::Deepseek => "/v1/models",
    };
    Url::parse(&format!("{base_url}{suffix}")).map_err(|_| "无法构造模型列表 URL".to_string())
}

fn gateway_openai_models_url_candidates(base_url: &str) -> Result<Vec<Url>, String> {
    let base_url = base_url.trim_end_matches('/');
    let mut values = if let Some(root) = base_url.strip_suffix("/v1") {
        vec![format!("{base_url}/models"), format!("{root}/models")]
    } else if ends_with_version_segment(base_url) {
        vec![
            format!("{base_url}/models"),
            format!("{base_url}/v1/models"),
        ]
    } else {
        vec![
            format!("{base_url}/v1/models"),
            format!("{base_url}/models"),
        ]
    };
    values.dedup();
    values
        .into_iter()
        .map(|value| Url::parse(&value).map_err(|_| "无法构造模型列表 URL".to_string()))
        .collect()
}

async fn fetch_gateway_candidate<T: DeserializeOwned>(
    candidates: &[Url],
    headers: &HeaderMap,
    api_key: &str,
) -> Result<T, String> {
    let mut last_err = None;
    for url in candidates {
        match fetch_gateway_page(url, headers, api_key).await {
            Ok(response) => return Ok(response),
            Err(GatewayPageError::NotFound(message)) => last_err = Some(message),
            Err(GatewayPageError::Fatal(message)) => return Err(message),
        }
    }
    Err(format!(
        "All candidates failed: {}",
        last_err.unwrap_or_else(|| "no candidates".to_string())
    ))
}

enum GatewayPageError {
    NotFound(String),
    Fatal(String),
}

impl GatewayPageError {
    fn into_message(self) -> String {
        match self {
            Self::NotFound(message) | Self::Fatal(message) => message,
        }
    }
}

async fn fetch_gateway_page<T: DeserializeOwned>(
    url: &Url,
    headers: &HeaderMap,
    api_key: &str,
) -> Result<T, GatewayPageError> {
    let known_secrets = [api_key.to_string()];
    log::debug!(
        "[GatewayModelFetch] Trying endpoint: {}",
        crate::url_for_log_with_secrets(url.as_str(), &known_secrets)
    );
    let response = crate::proxy::http_client::get()
        .get(url.clone())
        .headers(headers.clone())
        .timeout(Duration::from_secs(FETCH_TIMEOUT_SECS))
        .send()
        .await
        .map_err(|error| {
            GatewayPageError::Fatal(crate::redact_known_secrets_strict(
                &format!("模型请求失败: {error}"),
                &known_secrets,
            ))
        })?;
    let status = response.status();
    if !status.is_success() {
        let body = redact_model_fetch_error_body(
            response.text().await.unwrap_or_default(),
            &known_secrets,
        );
        let message = format!("HTTP {status}: {body}");
        return if status == StatusCode::NOT_FOUND || status == StatusCode::METHOD_NOT_ALLOWED {
            Err(GatewayPageError::NotFound(message))
        } else {
            Err(GatewayPageError::Fatal(message))
        };
    }
    response.json::<T>().await.map_err(|error| {
        GatewayPageError::Fatal(crate::redact_known_secrets_strict(
            &format!("Failed to parse response: {error}"),
            &known_secrets,
        ))
    })
}

fn redact_model_fetch_error_body(body: String, known_secrets: &[String]) -> String {
    truncate_body(crate::redact_known_secrets_strict(&body, known_secrets))
}

fn build_model_fetch_headers(
    api_key: &str,
    api_format: Option<&str>,
    user_agent: Option<&HeaderValue>,
    request_headers: Option<&BTreeMap<String, String>>,
) -> Result<HeaderMap, String> {
    let custom_count = request_headers.map_or(0, BTreeMap::len);
    if api_key.is_empty() && custom_count == 0 {
        return Err("API Key or request headers are required to fetch models".to_string());
    }
    if custom_count > MAX_REQUEST_HEADERS {
        return Err(format!(
            "Too many model-fetch request headers (maximum {MAX_REQUEST_HEADERS})"
        ));
    }

    let mut headers = HeaderMap::new();
    if !api_key.is_empty() {
        let (name, value) = match api_format {
            Some("anthropic-messages") => (
                HeaderName::from_static("x-api-key"),
                HeaderValue::from_str(api_key)
                    .map_err(|error| format!("Invalid API Key header value: {error}"))?,
            ),
            Some("google-generative-ai") => (
                HeaderName::from_static("x-goog-api-key"),
                HeaderValue::from_str(api_key)
                    .map_err(|error| format!("Invalid API Key header value: {error}"))?,
            ),
            _ => (
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {api_key}"))
                    .map_err(|error| format!("Invalid API Key header value: {error}"))?,
            ),
        };
        headers.insert(name, value);
    }

    if let Some(user_agent) = user_agent {
        headers.insert(USER_AGENT, user_agent.clone());
    }

    if let Some(request_headers) = request_headers {
        for (raw_name, raw_value) in request_headers {
            let name = raw_name.trim();
            if name.is_empty() || name.len() > MAX_HEADER_NAME_BYTES {
                return Err(format!("Invalid model-fetch header name: {raw_name}"));
            }
            if raw_value.len() > MAX_HEADER_VALUE_BYTES {
                return Err(format!("Model-fetch header value is too large: {name}"));
            }
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|error| format!("Invalid model-fetch header name {name}: {error}"))?;
            let value = HeaderValue::from_str(raw_value)
                .map_err(|error| format!("Invalid model-fetch header value for {name}: {error}"))?;
            headers.insert(name, value);
        }
    }

    Ok(headers)
}

/// 构造「模型列表端点」的候选 URL 列表
///
/// 候选顺序：
/// 1. `models_url_override` 非空 → 只返回它
/// 2. baseURL 拼 `/v1/models`；若已以版本段 `/v{N}` 结尾（`/v1`、智谱
///    `/api/coding/paas/v4` 等），版本号已在路径里，改拼 `/models`
/// 3. 版本段非 `/v1`（如 `/v4`）时再追加 `/v1/models` 作为兜底次候选
/// 4. 若 baseURL 命中 [`KNOWN_COMPAT_SUFFIXES`]，剥离后缀再拼 `/v1/models`、`/models`
///
/// 结果已去重且保持首次出现顺序。
pub fn build_models_url_candidates(
    base_url: &str,
    is_full_url: bool,
    models_url_override: Option<&str>,
) -> Result<Vec<String>, String> {
    if let Some(raw) = models_url_override {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return Ok(vec![trimmed.to_string()]);
        }
    }

    let trimmed = base_url.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err("Base URL is empty".to_string());
    }

    let mut candidates: Vec<String> = Vec::new();

    if is_full_url {
        if let Some(idx) = trimmed.find("/v1/") {
            candidates.push(format!("{}/v1/models", &trimmed[..idx]));
        } else if let Some(idx) = trimmed.rfind('/') {
            let root = &trimmed[..idx];
            if root.contains("://") && root.len() > root.find("://").unwrap() + 3 {
                candidates.push(format!("{root}/v1/models"));
            }
        }
        if candidates.is_empty() {
            return Err("Cannot derive models endpoint from full URL".to_string());
        }
        return Ok(candidates);
    }

    // baseURL 已以版本段 /v{N} 结尾时（如 `/v1`、智谱 `/api/coding/paas/v4`），
    // OpenAI 惯例的模型端点是 `{base}/models`，不能再补 `/v1`
    // （否则 .../coding/paas/v4/v1/models → 404）。
    if ends_with_version_segment(trimmed) {
        candidates.push(format!("{trimmed}/models"));
        // 版本段非 /v1 时，保留旧的 /v1/models 作为兜底次候选（正确路径已在前）。
        if !trimmed.ends_with("/v1") {
            candidates.push(format!("{trimmed}/v1/models"));
        }
    } else {
        candidates.push(format!("{trimmed}/v1/models"));
    }

    if let Some(stripped) = strip_compat_suffix(trimmed) {
        let root = stripped.trim_end_matches('/');
        if !root.is_empty() && root.contains("://") {
            candidates.push(format!("{root}/v1/models"));
            candidates.push(format!("{root}/models"));
        }
    }

    // 候选最多 3 条，线性去重即可，不值得上 HashSet。
    let mut unique: Vec<String> = Vec::with_capacity(candidates.len());
    for url in candidates {
        if !unique.iter().any(|u| u == &url) {
            unique.push(url);
        }
    }

    Ok(unique)
}

/// 截断响应体到 [`ERROR_BODY_MAX_CHARS`] 字符，避免 HTML 404 页占用错误串。
fn truncate_body(body: String) -> String {
    if body.chars().count() <= ERROR_BODY_MAX_CHARS {
        body
    } else {
        let mut s: String = body.chars().take(ERROR_BODY_MAX_CHARS).collect();
        s.push('…');
        s
    }
}

/// 若 baseURL 以任一已知兼容子路径结尾，返回剥离后的剩余部分；否则 `None`。
///
/// 依赖 [`KNOWN_COMPAT_SUFFIXES`] 按长度降序排列，确保最长前缀优先命中
/// （否则 `/anthropic` 会提前匹配掉 `/api/anthropic` 的场景）。
fn strip_compat_suffix(base_url: &str) -> Option<&str> {
    for suffix in KNOWN_COMPAT_SUFFIXES {
        if base_url.ends_with(*suffix) {
            return Some(&base_url[..base_url.len() - suffix.len()]);
        }
    }
    None
}

/// 判断 baseURL 是否以 OpenAI 风格的版本段 `/v{N}` 结尾（`N` 为一个或多个数字），
/// 例如 `/v1`、`.../paas/v4`。这类 URL 版本号已在路径中，模型端点应为
/// `{base}/models`，不能再补 `/v1`（智谱 Coding Plan 即 `.../coding/paas/v4`）。
fn ends_with_version_segment(url: &str) -> bool {
    let last = url.rsplit('/').next().unwrap_or("");
    last.strip_prefix('v')
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_model_fetch_uses_protocol_specific_urls_and_auth() {
        let claude =
            gateway_models_url("https://claude.example.com/v1", UpstreamType::Claude).unwrap();
        assert_eq!(claude.as_str(), "https://claude.example.com/v1/models");
        let claude_headers = build_gateway_headers("claude-key", UpstreamType::Claude).unwrap();
        assert_eq!(claude_headers["x-api-key"], "claude-key");
        assert_eq!(claude_headers["anthropic-version"], "2023-06-01");
        assert!(!claude_headers.contains_key(AUTHORIZATION));

        let gemini = gateway_models_url(
            "https://generativelanguage.googleapis.com/v1beta",
            UpstreamType::Gemini,
        )
        .unwrap();
        assert_eq!(
            gemini.as_str(),
            "https://generativelanguage.googleapis.com/v1beta/models"
        );
        let gemini_headers = build_gateway_headers("gemini-key", UpstreamType::Gemini).unwrap();
        assert_eq!(gemini_headers["x-goog-api-key"], "gemini-key");
        assert!(!gemini_headers.contains_key(AUTHORIZATION));

        for upstream in [
            UpstreamType::Openai,
            UpstreamType::Codex,
            UpstreamType::Deepseek,
        ] {
            let urls =
                gateway_openai_models_url_candidates("https://relay.example.com/api").unwrap();
            assert_eq!(
                urls.iter().map(Url::as_str).collect::<Vec<_>>(),
                vec![
                    "https://relay.example.com/api/v1/models",
                    "https://relay.example.com/api/models",
                ]
            );
            let versioned =
                gateway_openai_models_url_candidates("https://relay.example.com/api/v1").unwrap();
            assert_eq!(
                versioned.iter().map(Url::as_str).collect::<Vec<_>>(),
                vec![
                    "https://relay.example.com/api/v1/models",
                    "https://relay.example.com/api/models",
                ]
            );
            let nonstandard_version =
                gateway_openai_models_url_candidates("https://relay.example.com/api/v4").unwrap();
            assert_eq!(
                nonstandard_version
                    .iter()
                    .map(Url::as_str)
                    .collect::<Vec<_>>(),
                vec![
                    "https://relay.example.com/api/v4/models",
                    "https://relay.example.com/api/v4/v1/models",
                ]
            );
            let headers = build_gateway_headers("bearer-key", upstream).unwrap();
            assert_eq!(headers[AUTHORIZATION], "Bearer bearer-key");
        }
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn gateway_openai_model_fetch_falls_back_only_for_404_or_405() {
        crate::proxy::http_client::init(None).unwrap();
        let requested = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = requested.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = axum::Router::new()
            .route(
                "/api/v1/models",
                axum::routing::get({
                    let seen = requested.clone();
                    move || {
                        let seen = seen.clone();
                        async move {
                            seen.lock().unwrap().push("/api/v1/models");
                            axum::http::StatusCode::NOT_FOUND
                        }
                    }
                }),
            )
            .route(
                "/api/models",
                axum::routing::get(move |headers: axum::http::HeaderMap| {
                    let seen = seen.clone();
                    async move {
                        seen.lock().unwrap().push("/api/models");
                        assert_eq!(headers[AUTHORIZATION], "Bearer loopback-secret");
                        axum::Json(serde_json::json!({
                            "data": [{"id": "fallback-model", "owned_by": "loopback"}]
                        }))
                    }
                }),
            );
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

        let models = fetch_gateway_models(
            &format!("http://{address}/api"),
            "loopback-secret",
            UpstreamType::Openai,
        )
        .await
        .unwrap();
        assert_eq!(models[0].id, "fallback-model");
        assert_eq!(
            *requested.lock().unwrap(),
            vec!["/api/v1/models", "/api/models"]
        );
        server.abort();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn gateway_model_fetch_parse_error_is_classified_and_redacted() {
        crate::proxy::http_client::init(None).unwrap();
        let fallback_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls = fallback_calls.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = axum::Router::new()
            .route(
                "/api/v1/models",
                axum::routing::get(|| async { "loopback-secret is not json" }),
            )
            .route(
                "/api/models",
                axum::routing::get(move || {
                    calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    async { axum::Json(serde_json::json!({"data": []})) }
                }),
            );
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

        let error = fetch_gateway_models(
            &format!("http://{address}/api"),
            "loopback-secret",
            UpstreamType::Openai,
        )
        .await
        .unwrap_err();
        assert!(error.contains("Failed to parse"), "{error}");
        assert!(!error.contains("loopback-secret"), "{error}");
        assert_eq!(fallback_calls.load(std::sync::atomic::Ordering::Relaxed), 0);
        server.abort();
    }

    #[test]
    fn gateway_model_envelopes_parse_pagination_fields() {
        let claude: ClaudeModelsResponse = serde_json::from_str(
            r#"{"data":[{"id":"claude-model"}],"has_more":true,"last_id":"cursor-a"}"#,
        )
        .unwrap();
        assert!(claude.has_more);
        assert_eq!(claude.last_id.as_deref(), Some("cursor-a"));
        assert_eq!(claude.data[0].id, "claude-model");

        let gemini: GeminiModelsResponse = serde_json::from_str(
            r#"{"models":[{"name":"models/gemini-model"}],"nextPageToken":"cursor-b"}"#,
        )
        .unwrap();
        assert_eq!(gemini.next_page_token.as_deref(), Some("cursor-b"));
        assert_eq!(gemini.models[0].name, "models/gemini-model");
    }

    #[test]
    fn model_fetch_headers_follow_pi_api_format() {
        let anthropic =
            build_model_fetch_headers("anthropic-key", Some("anthropic-messages"), None, None)
                .unwrap();
        assert_eq!(anthropic["x-api-key"], "anthropic-key");
        assert!(!anthropic.contains_key(AUTHORIZATION));

        let google =
            build_model_fetch_headers("google-key", Some("google-generative-ai"), None, None)
                .unwrap();
        assert_eq!(google["x-goog-api-key"], "google-key");
        assert!(!google.contains_key(AUTHORIZATION));

        let openai =
            build_model_fetch_headers("openai-key", Some("openai-responses"), None, None).unwrap();
        assert_eq!(openai[AUTHORIZATION], "Bearer openai-key");
    }

    #[test]
    fn model_fetch_headers_allow_validated_header_only_auth_and_overrides() {
        let custom = BTreeMap::from([
            ("Authorization".to_string(), "Token literal".to_string()),
            ("X-Tenant".to_string(), "tenant-a".to_string()),
        ]);
        let headers =
            build_model_fetch_headers("", Some("openai-completions"), None, Some(&custom)).unwrap();
        assert_eq!(headers[AUTHORIZATION], "Token literal");
        assert_eq!(headers["x-tenant"], "tenant-a");

        let override_default =
            BTreeMap::from([("x-api-key".to_string(), "header-managed-key".to_string())]);
        let headers = build_model_fetch_headers(
            "provider-key",
            Some("anthropic-messages"),
            None,
            Some(&override_default),
        )
        .unwrap();
        assert_eq!(headers["x-api-key"], "header-managed-key");
    }

    #[test]
    fn model_fetch_headers_reject_invalid_or_missing_credentials() {
        assert!(build_model_fetch_headers("", None, None, None).is_err());
        let invalid = BTreeMap::from([("bad header".to_string(), "literal-value".to_string())]);
        assert!(build_model_fetch_headers("", None, None, Some(&invalid)).is_err());
    }

    #[test]
    fn model_fetch_error_body_redacts_known_header_credentials() {
        let secrets = vec![
            "short".to_string(),
            "Bearer literal-header-secret".to_string(),
        ];
        let body = redact_model_fetch_error_body(
            "invalid short / Bearer literal-header-secret".to_string(),
            &secrets,
        );
        assert_eq!(body, "invalid [REDACTED] / [REDACTED]");
    }

    #[test]
    fn test_candidates_plain_root() {
        let c = build_models_url_candidates("https://api.siliconflow.cn", false, None).unwrap();
        assert_eq!(c, vec!["https://api.siliconflow.cn/v1/models"]);
    }

    #[test]
    fn test_candidates_trailing_slash() {
        let c = build_models_url_candidates("https://api.example.com/", false, None).unwrap();
        assert_eq!(c, vec!["https://api.example.com/v1/models"]);
    }

    #[test]
    fn test_candidates_with_v1() {
        let c = build_models_url_candidates("https://api.example.com/v1", false, None).unwrap();
        assert_eq!(c, vec!["https://api.example.com/v1/models"]);
    }

    #[test]
    fn test_candidates_zhipu_coding_paas_v4() {
        // 智谱 Coding Plan 端点以 /v4 版本段结尾：模型端点是 {base}/models，
        // 正确路径必须排在 .../v4/v1/models（404）之前。
        let c =
            build_models_url_candidates("https://open.bigmodel.cn/api/coding/paas/v4", false, None)
                .unwrap();
        assert_eq!(
            c,
            vec![
                "https://open.bigmodel.cn/api/coding/paas/v4/models",
                "https://open.bigmodel.cn/api/coding/paas/v4/v1/models",
            ]
        );
    }

    #[test]
    fn test_candidates_zai_coding_paas_v4() {
        let c = build_models_url_candidates("https://api.z.ai/api/coding/paas/v4", false, None)
            .unwrap();
        assert_eq!(
            c,
            vec![
                "https://api.z.ai/api/coding/paas/v4/models",
                "https://api.z.ai/api/coding/paas/v4/v1/models",
            ]
        );
    }

    #[test]
    fn test_ends_with_version_segment() {
        assert!(ends_with_version_segment("https://x.com/v1"));
        assert!(ends_with_version_segment(
            "https://open.bigmodel.cn/api/coding/paas/v4"
        ));
        assert!(ends_with_version_segment("https://x.com/v10"));
        assert!(!ends_with_version_segment("https://x.com/api"));
        assert!(!ends_with_version_segment("https://x.com/vX"));
        assert!(!ends_with_version_segment("https://x.com/models"));
        assert!(!ends_with_version_segment("https://api.siliconflow.cn"));
    }

    #[test]
    fn test_candidates_full_url() {
        let c = build_models_url_candidates(
            "https://proxy.example.com/v1/chat/completions",
            true,
            None,
        )
        .unwrap();
        assert_eq!(c, vec!["https://proxy.example.com/v1/models"]);
    }

    #[test]
    fn test_candidates_empty() {
        assert!(build_models_url_candidates("", false, None).is_err());
    }

    #[test]
    fn test_candidates_override_returns_single() {
        let c = build_models_url_candidates(
            "https://api.deepseek.com/anthropic",
            false,
            Some("https://api.deepseek.com/models"),
        )
        .unwrap();
        assert_eq!(c, vec!["https://api.deepseek.com/models"]);
    }

    #[test]
    fn test_candidates_override_empty_falls_through() {
        let c =
            build_models_url_candidates("https://api.siliconflow.cn", false, Some("   ")).unwrap();
        assert_eq!(c, vec!["https://api.siliconflow.cn/v1/models"]);
    }

    #[test]
    fn test_candidates_deepseek_strip_anthropic() {
        let c =
            build_models_url_candidates("https://api.deepseek.com/anthropic", false, None).unwrap();
        assert_eq!(
            c,
            vec![
                "https://api.deepseek.com/anthropic/v1/models",
                "https://api.deepseek.com/v1/models",
                "https://api.deepseek.com/models",
            ]
        );
    }

    #[test]
    fn test_candidates_zhipu_strip_api_anthropic() {
        let c = build_models_url_candidates("https://open.bigmodel.cn/api/anthropic", false, None)
            .unwrap();
        assert_eq!(
            c,
            vec![
                "https://open.bigmodel.cn/api/anthropic/v1/models",
                "https://open.bigmodel.cn/v1/models",
                "https://open.bigmodel.cn/models",
            ]
        );
    }

    #[test]
    fn test_candidates_bailian_strip_apps_anthropic() {
        let c = build_models_url_candidates(
            "https://dashscope.aliyuncs.com/apps/anthropic",
            false,
            None,
        )
        .unwrap();
        assert_eq!(
            c,
            vec![
                "https://dashscope.aliyuncs.com/apps/anthropic/v1/models",
                "https://dashscope.aliyuncs.com/v1/models",
                "https://dashscope.aliyuncs.com/models",
            ]
        );
    }

    #[test]
    fn test_candidates_stepfun_strip_step_plan() {
        let c =
            build_models_url_candidates("https://api.stepfun.com/step_plan", false, None).unwrap();
        assert_eq!(
            c,
            vec![
                "https://api.stepfun.com/step_plan/v1/models",
                "https://api.stepfun.com/v1/models",
                "https://api.stepfun.com/models",
            ]
        );
    }

    #[test]
    fn test_candidates_doubao_strip_api_coding() {
        let c = build_models_url_candidates(
            "https://ark.cn-beijing.volces.com/api/coding",
            false,
            None,
        )
        .unwrap();
        assert_eq!(
            c,
            vec![
                "https://ark.cn-beijing.volces.com/api/coding/v1/models",
                "https://ark.cn-beijing.volces.com/v1/models",
                "https://ark.cn-beijing.volces.com/models",
            ]
        );
    }

    #[test]
    fn test_candidates_rightcode_strip_claude() {
        let c = build_models_url_candidates("https://www.right.codes/claude", false, None).unwrap();
        assert_eq!(
            c,
            vec![
                "https://www.right.codes/claude/v1/models",
                "https://www.right.codes/v1/models",
                "https://www.right.codes/models",
            ]
        );
    }

    #[test]
    fn test_candidates_longer_suffix_wins() {
        // baseURL 以 /api/anthropic 结尾时，应剥离整个 /api/anthropic，
        // 而不是只剥离 /anthropic（那样会得到残缺的 https://.../api 根）。
        let c = build_models_url_candidates("https://api.z.ai/api/anthropic", false, None).unwrap();
        assert_eq!(
            c,
            vec![
                "https://api.z.ai/api/anthropic/v1/models",
                "https://api.z.ai/v1/models",
                "https://api.z.ai/models",
            ]
        );
    }

    #[test]
    fn test_candidates_no_suffix_no_strip() {
        let c = build_models_url_candidates("https://openrouter.ai/api", false, None).unwrap();
        assert_eq!(c, vec!["https://openrouter.ai/api/v1/models"]);
    }

    #[test]
    fn test_candidates_deduplicate() {
        // 虚构 case：baseURL 就是 "scheme://host"，剥不出子路径，应只有一个候选。
        let c = build_models_url_candidates("https://host.example.com", false, None).unwrap();
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn test_parse_response() {
        let json = r#"{"object":"list","data":[{"id":"gpt-4","object":"model","owned_by":"openai"},{"id":"claude-3-sonnet","object":"model","owned_by":"anthropic"}]}"#;
        let resp: ModelsResponse = serde_json::from_str(json).unwrap();
        let data = resp.data.unwrap();
        assert_eq!(data.len(), 2);
        assert_eq!(data[0].id, "gpt-4");
        assert_eq!(data[0].owned_by.as_deref(), Some("openai"));
        assert_eq!(data[1].id, "claude-3-sonnet");
    }

    #[test]
    fn test_parse_response_no_owned_by() {
        let json = r#"{"object":"list","data":[{"id":"my-model","object":"model"}]}"#;
        let resp: ModelsResponse = serde_json::from_str(json).unwrap();
        let data = resp.data.unwrap();
        assert_eq!(data[0].id, "my-model");
        assert!(data[0].owned_by.is_none());
    }

    #[test]
    fn test_parse_response_empty_data() {
        let json = r#"{"object":"list","data":[]}"#;
        let resp: ModelsResponse = serde_json::from_str(json).unwrap();
        assert!(resp.data.unwrap().is_empty());
    }
}
