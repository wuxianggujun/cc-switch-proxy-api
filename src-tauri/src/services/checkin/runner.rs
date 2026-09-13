//! 签到请求执行
//!
//! reqwest 未启用 `cookie` feature，所以登录态靠手动解析 `Set-Cookie`
//! 再拼到签到请求的 `Cookie` 头上，而不是依赖 cookie store。

use super::browser::{CF_CLEARANCE, CLEARANCE_USER_AGENT};
use super::{
    CheckinAuthKind, CheckinBodyKind, CheckinLogin, CheckinRequest, CheckinResult, CheckinSite,
    CheckinStatus,
};
use reqwest::header::{
    HeaderMap, HeaderName, HeaderValue, CONTENT_TYPE, COOKIE, SET_COOKIE, USER_AGENT,
};
use reqwest::{Client, Method, Response};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(30);
/// 响应体只留前 2KB：签到接口的有效信息都在开头，
/// 失败时整页 HTML 存进数据库没有意义。
const MAX_BODY: usize = 2048;
const MAX_RESPONSE_BODY_BYTES: usize = 256 * 1024;

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 走 `Browser` 认证时，UA 必须与过闸窗口完全一致，否则 cf_clearance 失效，
/// 所以 UA 由调用方传入而非固定。
fn build_client(user_agent: &str) -> Result<Client, String> {
    Client::builder()
        .timeout(TIMEOUT)
        // Keep login Set-Cookie headers on 302/303 responses; following a redirect
        // without a cookie jar loses the authenticated session.
        .redirect(reqwest::redirect::Policy::none())
        // 站点常按 UA 拦非浏览器请求。
        .user_agent(user_agent)
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))
}

/// 过闸凭证：cookie 与 UA 必须成对传入。
#[derive(Debug, Clone)]
pub struct Clearance<'a> {
    pub cookie: &'a str,
    pub user_agent: &'a str,
}

/// 执行一个站点的签到。网络失败与站点判定失败分开表达，
/// 前端要能区分「网断了」和「今天已经签过」。
///
/// `clearance` 仅 `Browser` 认证时提供，由命令层负责过闸后传入。
pub async fn run_checkin_with(
    site: &CheckinSite,
    clearance: Option<Clearance<'_>>,
) -> CheckinResult {
    run_with_credentials(site, clearance, None).await
}

/// Browser account cookies come from the entry's profile, never from the clearance cache.
/// An empty clearance allows ordinary sites to work without waiting for a nonexistent CF challenge.
pub(super) async fn run_browser_checkin(
    site: &CheckinSite,
    clearance: Option<Clearance<'_>>,
    account_cookie: &str,
) -> CheckinResult {
    if site.auth_kind != CheckinAuthKind::Browser {
        return run_checkin_with(site, None).await;
    }
    let clearance = clearance.unwrap_or(Clearance {
        cookie: "",
        user_agent: CLEARANCE_USER_AGENT,
    });
    run_with_credentials(site, Some(clearance), Some(account_cookie)).await
}

async fn run_with_credentials(
    site: &CheckinSite,
    clearance: Option<Clearance<'_>>,
    account_cookie: Option<&str>,
) -> CheckinResult {
    match execute(site, clearance, account_cookie).await {
        Ok(result) => result,
        Err(message) => CheckinResult {
            status: CheckinStatus::Error,
            at: now(),
            http_status: None,
            message,
            needs_login: false,
        },
    }
}

async fn execute(
    site: &CheckinSite,
    clearance: Option<Clearance<'_>>,
    account_cookie: Option<&str>,
) -> Result<CheckinResult, String> {
    // Browser 认证下 UA 必须与过闸窗口一致，否则 cf_clearance 当场失效。
    let user_agent = clearance
        .as_ref()
        .map(|c| c.user_agent)
        .unwrap_or(CLEARANCE_USER_AGENT);
    let client = build_client(user_agent)?;

    let session_cookie = match site.auth_kind {
        CheckinAuthKind::Header => None,
        CheckinAuthKind::Login => {
            let login = site
                .login
                .as_ref()
                .ok_or_else(|| "缺少登录配置".to_string())?;
            Some(login_for_cookie(&client, login).await?)
        }
        // 过闸由命令层完成（需要 AppHandle 开 WebView），这里只消费结果。
        // 拿不到凭证就显式报错，不静默降级成无 cookie 请求——那只会撞上
        // 挑战页并被误判成「签到失败」。
        CheckinAuthKind::Browser => Some(
            clearance
                .as_ref()
                .map(|c| browser_request_cookies(c.cookie, account_cookie))
                .ok_or_else(|| "缺少 Cloudflare 过闸凭证".to_string())?,
        ),
    };

    let response = send_checkin(
        &client,
        &site.request,
        session_cookie.as_deref(),
        clearance.as_ref().map(|clearance| clearance.user_agent),
    )
    .await?;
    let http_status = response.status().as_u16();
    let login_redirect = response
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|location| url::Url::parse(&site.request.url).ok()?.join(location).ok())
        .is_some_and(|url| is_login_path(url.path()));
    let body = read_body(response).await?;

    // 先判是否被 CF 挡下：此时 body 是挑战页而非站点响应，按判定串比对
    // 只会得到「签到失败」，掩盖真实原因，也不会触发重新过闸。
    if is_cloudflare_challenge(http_status, &body) {
        return Ok(CheckinResult {
            status: CheckinStatus::Blocked,
            at: now(),
            http_status: Some(http_status),
            message: "被 Cloudflare 拦截，需要重新通过站点验证".to_string(),
            needs_login: false,
        });
    }

    let needs_login = site.auth_kind == CheckinAuthKind::Browser
        && (is_login_required(http_status, &body)
            || ((300..400).contains(&http_status) && login_redirect));
    let ok = !needs_login && judge(&site.request.success_contains, http_status, &body);

    Ok(CheckinResult {
        status: if ok {
            CheckinStatus::Success
        } else {
            CheckinStatus::Failed
        },
        at: now(),
        http_status: Some(http_status),
        message: extract_message(&body),
        needs_login,
    })
}

/// 识别 Cloudflare 挑战/拦截响应。
///
/// 403/503 是 CF 挑战与 IUAM 的典型状态码，但站点自身也可能用它们表达业务
/// 错误，所以状态码必须与挑战页特征串同时命中才判定为拦截。
fn is_cloudflare_challenge(http_status: u16, body: &str) -> bool {
    if !matches!(http_status, 403 | 429 | 503) {
        return false;
    }
    const MARKERS: [&str; 6] = [
        "cf-browser-verification",
        "cf_chl_opt",
        "challenge-platform",
        "Just a moment",
        "Checking your browser",
        "cf-mitigated",
    ];
    MARKERS.iter().any(|m| body.contains(m))
}

/// 登录并返回可直接用于 `Cookie` 头的串。
async fn login_for_cookie(client: &Client, login: &CheckinLogin) -> Result<String, String> {
    let mut request = client.post(login.url.trim());

    request = match login.body_kind {
        CheckinBodyKind::Form => request.form(&[
            (login.username_field.as_str(), login.username.as_str()),
            (login.password_field.as_str(), login.password.as_str()),
        ]),
        // 登录接口极少用空体，None 也按 JSON 处理。
        CheckinBodyKind::Json | CheckinBodyKind::None => {
            let mut map = serde_json::Map::new();
            map.insert(
                login.username_field.clone(),
                serde_json::Value::String(login.username.clone()),
            );
            map.insert(
                login.password_field.clone(),
                serde_json::Value::String(login.password.clone()),
            );
            request.json(&serde_json::Value::Object(map))
        }
    };

    let response = request
        .send()
        .await
        .map_err(|e| format!("登录请求失败: {}", e.without_url()))?;

    let status = response.status();
    if status.is_client_error() || status.is_server_error() {
        return Err(format!("登录失败（HTTP {}）", status.as_u16()));
    }
    let cookie = collect_cookies(response.headers());

    if cookie.is_empty() {
        return Err(format!(
            "登录未返回 Cookie（HTTP {}），请确认账号密码与字段名是否正确",
            status.as_u16()
        ));
    }

    Ok(cookie)
}

/// 把多个 `Set-Cookie` 折成 `a=1; b=2`，丢掉 Path/HttpOnly 等属性。
fn collect_cookies(headers: &HeaderMap) -> String {
    headers
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .filter_map(|raw| raw.split(';').next())
        .map(str::trim)
        .filter(|pair| !pair.is_empty())
        .collect::<Vec<_>>()
        .join("; ")
}

async fn send_checkin(
    client: &Client,
    request: &CheckinRequest,
    session_cookie: Option<&str>,
    enforced_user_agent: Option<&str>,
) -> Result<Response, String> {
    let method = Method::from_bytes(request.method.trim().to_uppercase().as_bytes())
        .map_err(|_| format!("不支持的请求方法: {}", request.method))?;

    let mut builder = client.request(method, request.url.trim());
    let mut headers = HeaderMap::new();
    let mut cookies = indexmap::IndexMap::new();

    // 自动凭证先合并，用户显式 Cookie 后合并，避免覆盖用户选择的账号。
    if let Some(cookie) = session_cookie {
        merge_cookies(&mut cookies, cookie);
    }

    for header in &request.headers {
        let name = header.name.trim();
        if name.is_empty() {
            continue;
        }
        let header_name =
            HeaderName::from_bytes(name.as_bytes()).map_err(|_| format!("非法请求头名: {name}"))?;
        let header_value = HeaderValue::from_str(header.value.trim())
            .map_err(|_| format!("请求头 {name} 的值含非法字符"))?;
        if header_name == COOKIE {
            merge_cookies(&mut cookies, header.value.trim());
        } else {
            headers.insert(header_name, header_value);
        }
    }

    if !cookies.is_empty() {
        let cookie = cookies
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; ");
        headers.insert(
            COOKIE,
            HeaderValue::from_str(&cookie).map_err(|_| "Cookie 含非法字符".to_string())?,
        );
    }
    if let Some(user_agent) = enforced_user_agent {
        headers.insert(
            USER_AGENT,
            HeaderValue::from_str(user_agent)
                .map_err(|_| "浏览器 User-Agent 含非法字符".to_string())?,
        );
    }
    builder = builder.headers(headers);

    builder = match request.body_kind {
        CheckinBodyKind::None => builder,
        CheckinBodyKind::Json => builder
            .header(CONTENT_TYPE, "application/json")
            .body(request.body.clone()),
        CheckinBodyKind::Form => builder
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(request.body.clone()),
    };

    builder
        .send()
        .await
        .map_err(|e| format!("签到请求失败: {}", e.without_url()))
}

fn merge_cookies(cookies: &mut indexmap::IndexMap<String, String>, header: &str) {
    for pair in header.split(';') {
        if let Some((name, value)) = pair.trim().split_once('=') {
            if !name.is_empty() {
                cookies.insert(name.to_string(), value.to_string());
            }
        }
    }
}

/// Legacy clearance caches are CF-only. Profile account cookies are a separate source;
/// user headers are merged afterwards by send_checkin and therefore still take precedence.
fn browser_request_cookies(clearance: &str, account_cookie: Option<&str>) -> String {
    let mut cookies = indexmap::IndexMap::new();
    merge_cookies(&mut cookies, clearance);
    cookies.retain(|name, _| name == CF_CLEARANCE);
    if let Some(account_cookie) = account_cookie {
        let mut account = indexmap::IndexMap::new();
        merge_cookies(&mut account, account_cookie);
        account.retain(|name, _| super::profile::is_account_cookie(name));
        cookies.extend(account);
    }
    cookies
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("; ")
}

fn is_login_path(path: &str) -> bool {
    matches!(
        path.trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "login" | "signin" | "sign-in"
    )
}

fn is_login_required(status: u16, body: &str) -> bool {
    if status == 401 {
        return true;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return false;
    };
    if value.get("success").and_then(|value| value.as_bool()) == Some(true) {
        return false;
    }
    let message = extract_message(body).to_ascii_lowercase();
    [
        "unauthorized",
        "unauthenticated",
        "not logged in",
        "please log in",
        "login required",
        "session expired",
        "未登录",
        "未登入",
        "请先登录",
        "請先登入",
        "登录已过期",
        "登录状态已失效",
        "登录状态无效",
        "登录失效",
        "登入已過期",
        "登录态已过期",
    ]
    .iter()
    .any(|marker| message.contains(marker))
}

async fn read_body(mut response: Response) -> Result<String, String> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("读取签到响应失败: {}", error.without_url()))?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BODY_BYTES {
            return Err("签到响应超过 256 KiB 限制".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).map_err(|_| "签到响应不是有效的 UTF-8 文本".into())
}

/// 成功判定。配了判定串就以它为准（多数站点签到失败也返回 200），
/// 没配则退回看 HTTP 状态码。
fn judge(success_contains: &str, http_status: u16, body: &str) -> bool {
    if !(200..300).contains(&http_status) {
        return false;
    }
    if serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value.get("success").and_then(|success| success.as_bool()))
        == Some(false)
    {
        return false;
    }
    let needle = success_contains.trim();
    if !needle.is_empty() {
        return body.contains(needle);
    }
    true
}

/// 优先取站点返回的 message 字段，取不到就用截断后的原文。
fn extract_message(body: &str) -> String {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(body) {
        for key in ["message", "msg", "error", "data"] {
            if let Some(text) = value.get(key).and_then(|v| v.as_str()) {
                if !text.trim().is_empty() {
                    return truncate_message(text.trim());
                }
            }
        }
    }
    body.trim().chars().take(200).collect()
}

fn truncate_message(text: &str) -> String {
    let mut end = text.len().min(MAX_BODY);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct CheckinTestServer {
        url: String,
        received_headers: Arc<Mutex<Option<HeaderMap>>>,
        task: tokio::task::JoinHandle<()>,
    }

    impl CheckinTestServer {
        async fn start() -> Self {
            Self::with_response(200, r#"{"success":true}"#).await
        }

        async fn with_response(status: u16, body: &'static str) -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/", listener.local_addr().unwrap());
            let received_headers = Arc::new(Mutex::new(None));
            let captured = Arc::clone(&received_headers);
            let app = axum::Router::new().route(
                "/",
                axum::routing::get(move |headers: HeaderMap| {
                    let captured = Arc::clone(&captured);
                    async move {
                        *captured.lock().unwrap() = Some(headers);
                        (axum::http::StatusCode::from_u16(status).unwrap(), body)
                    }
                }),
            );
            let task = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            Self {
                url,
                received_headers,
                task,
            }
        }

        fn headers(&self) -> HeaderMap {
            self.received_headers.lock().unwrap().clone().unwrap()
        }
    }

    impl Drop for CheckinTestServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    #[tokio::test]
    async fn profile_b_expired_login_is_failed_and_requests_reauthentication() {
        for (http_status, body) in [
            (401, r#"{"message":"Unauthorized"}"#),
            (200, r#"{"success":false,"message":"请先登录"}"#),
        ] {
            let server = CheckinTestServer::with_response(http_status, body).await;
            let site: CheckinSite = serde_json::from_value(serde_json::json!({
                "id": "account-b", "name": "Account B", "authKind": "browser",
                "request": {"url": server.url, "method": "GET"}
            }))
            .unwrap();
            let result = run_checkin_with(
                &site,
                Some(Clearance {
                    cookie: "cf_clearance=passed",
                    user_agent: CLEARANCE_USER_AGENT,
                }),
            )
            .await;
            assert_eq!(result.status, CheckinStatus::Failed);
            assert_eq!(serde_json::to_value(result).unwrap()["needsLogin"], true);
        }
    }

    #[tokio::test]
    async fn profile_b_account_cookie_works_without_a_cloudflare_cookie() {
        let server = CheckinTestServer::start().await;
        let site: CheckinSite = serde_json::from_value(serde_json::json!({
            "id": "account-b", "name": "Account B", "authKind": "browser",
            "request": {"url": server.url, "method": "GET"}
        }))
        .unwrap();
        let result = run_browser_checkin(&site, None, "session=account-b").await;
        assert_eq!(result.status, CheckinStatus::Success);
        assert_eq!(server.headers()[COOKIE], "session=account-b");
        assert_eq!(server.headers()[USER_AGENT], CLEARANCE_USER_AGENT);
    }

    #[tokio::test]
    async fn profile_b_explicit_cookie_wins_over_profile_and_clearance_cache() {
        let server = CheckinTestServer::start().await;
        let site: CheckinSite = serde_json::from_value(serde_json::json!({
            "id": "account-b", "name": "Account B", "authKind": "browser",
            "request": {"url": server.url, "method": "GET", "headers": [
                {"name": "Cookie", "value": "session=manual-account; token=manual-token"}
            ]}
        }))
        .unwrap();
        let result = run_browser_checkin(
            &site,
            Some(Clearance {
                cookie: "cf_clearance=passed; session=legacy-account",
                user_agent: CLEARANCE_USER_AGENT,
            }),
            "session=profile-account; token=profile-token; cf_clearance=must-not-override",
        )
        .await;
        assert_eq!(result.status, CheckinStatus::Success);
        let headers = server.headers();
        assert_eq!(headers.get_all(COOKIE).iter().count(), 1);
        let mut cookies = indexmap::IndexMap::new();
        merge_cookies(&mut cookies, headers[COOKIE].to_str().unwrap());
        assert_eq!(cookies.len(), 3);
        assert_eq!(cookies["session"], "manual-account");
        assert_eq!(cookies["token"], "manual-token");
        assert_eq!(cookies["cf_clearance"], "passed");
    }

    #[tokio::test]
    async fn profile_b_authentication_hints_preserve_cf_and_business_statuses() {
        for (status, body, expected) in [
            (
                403,
                r#"{"message":"Permission denied"}"#,
                CheckinStatus::Failed,
            ),
            (403, "<title>Just a moment</title>", CheckinStatus::Blocked),
            (
                200,
                r#"{"success":false,"message":"今日已签到"}"#,
                CheckinStatus::Failed,
            ),
            (
                200,
                r#"{"success":true,"message":"未登录用户的说明"}"#,
                CheckinStatus::Success,
            ),
        ] {
            let server = CheckinTestServer::with_response(status, body).await;
            let site: CheckinSite = serde_json::from_value(serde_json::json!({
                "id": "account-b", "name": "Account B", "authKind": "browser",
                "request": {"url": server.url, "method": "GET"}
            }))
            .unwrap();
            let result = run_browser_checkin(&site, None, "session=account-b").await;
            assert_eq!(result.status, expected);
            assert!(!result.needs_login);
        }
    }

    #[tokio::test]
    async fn browser_cookie_does_not_override_explicit_account_cookie() {
        let server = CheckinTestServer::start().await;
        let client = Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let request: CheckinRequest = serde_json::from_value(serde_json::json!({
            "url": server.url,
            "method": "GET",
            "headers": [
                {"name": "cOoKiE", "value": "session=account-b; token=manual=="},
                {"name": "User-Agent", "value": "must-not-replace-clearance-ua"}
            ]
        }))
        .unwrap();

        send_checkin(
            &client,
            &request,
            Some("cf_clearance=passed; session=account-a"),
            Some(CLEARANCE_USER_AGENT),
        )
        .await
        .unwrap();

        let headers = server.headers();
        assert_eq!(headers.get_all(COOKIE).iter().count(), 1);
        let cookie_header = headers[COOKIE].to_str().unwrap();
        let mut cookies = indexmap::IndexMap::new();
        merge_cookies(&mut cookies, cookie_header);
        assert_eq!(cookies["session"], "account-b");
        assert_eq!(cookies["cf_clearance"], "passed");
        assert_eq!(cookies["token"], "manual==");
        assert_eq!(cookie_header.split(';').count(), 3);
        assert_eq!(headers[USER_AGENT], CLEARANCE_USER_AGENT);
    }

    #[tokio::test]
    async fn browser_explicit_clearance_cookie_overrides_cached_clearance() {
        let server = CheckinTestServer::start().await;
        let client = Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let request: CheckinRequest = serde_json::from_value(serde_json::json!({
            "url": server.url,
            "method": "GET",
            "headers": [{"name": "Cookie", "value": "cf_clearance=explicit"}]
        }))
        .unwrap();

        send_checkin(
            &client,
            &request,
            Some("cf_clearance=cached"),
            Some(CLEARANCE_USER_AGENT),
        )
        .await
        .unwrap();

        let headers = server.headers();
        assert_eq!(headers.get_all(COOKIE).iter().count(), 1);
        assert_eq!(headers[COOKIE], "cf_clearance=explicit");
        assert_eq!(headers[USER_AGENT], CLEARANCE_USER_AGENT);
    }

    #[tokio::test]
    async fn browser_cached_cookies_never_supply_account_login_state() {
        let server = CheckinTestServer::start().await;
        // Both entries share a site and the same legacy WebView cookie snapshot.
        // Omitting an explicit account must not fall back to the WebView's account.
        for account in [Some("account-a"), Some("account-b"), None] {
            let explicit_headers = account
                .map(|account| {
                    serde_json::json!([{"name": "Cookie", "value": format!("session={account}")}])
                })
                .unwrap_or_else(|| serde_json::json!([]));
            let site: CheckinSite = serde_json::from_value(serde_json::json!({
                "id": account.unwrap_or("no-account"),
                "name": "same-site",
                "authKind": "browser",
                "request": {"url": server.url, "method": "GET", "headers": explicit_headers}
            }))
            .unwrap();

            let result = run_checkin_with(
                &site,
                Some(Clearance {
                    cookie: "cf_clearance=passed; session=account-a; auth_token=account-a-token",
                    user_agent: CLEARANCE_USER_AGENT,
                }),
            )
            .await;

            assert_eq!(result.status, CheckinStatus::Success);
            let headers = server.headers();
            assert_eq!(headers.get_all(COOKIE).iter().count(), 1);
            let mut cookies = indexmap::IndexMap::new();
            merge_cookies(&mut cookies, headers[COOKIE].to_str().unwrap());
            assert_eq!(cookies["cf_clearance"], "passed");
            assert_eq!(cookies.get("session").map(String::as_str), account);
            assert_eq!(cookies.len(), if account.is_some() { 2 } else { 1 });
            assert_eq!(headers[USER_AGENT], CLEARANCE_USER_AGENT);
        }
    }

    #[test]
    fn readiness_http_error_cannot_match_success_keyword() {
        assert!(!judge("success", 500, r#"{"success":false}"#));
        assert!(!judge("", 200, r#"{"success":false}"#));
        assert!(!judge("success", 200, r#"{"success":false}"#));
    }

    #[test]
    fn browser_cookies_merge_with_manual_login_cookie_without_duplicates() {
        let mut cookies = indexmap::IndexMap::new();
        merge_cookies(&mut cookies, "session=old; token=keep");
        merge_cookies(&mut cookies, "session=fresh; cf_clearance=passed");
        assert_eq!(cookies.len(), 3);
        assert_eq!(cookies["session"], "fresh");
        assert_eq!(cookies["token"], "keep");
        assert_eq!(cookies["cf_clearance"], "passed");
    }

    #[tokio::test]
    async fn readiness_truncated_body_is_not_a_successful_checkin() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            socket.read(&mut request).await.unwrap();
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\nshort",
                )
                .await
                .unwrap();
        });
        let site: CheckinSite = serde_json::from_value(serde_json::json!({
            "id": "test", "name": "test", "authKind": "header",
            "request": {"url": format!("http://{address}/"), "method": "GET"}
        }))
        .unwrap();
        let result = run_checkin_with(&site, None).await;
        server.await.unwrap();
        assert_eq!(result.status, CheckinStatus::Error);
    }

    #[test]
    fn judge_prefers_success_needle_over_status() {
        // 站点签到失败也返回 200，此时必须靠判定串识别。
        assert!(!judge("签到成功", 200, r#"{"message":"今日已签到"}"#));
        assert!(judge(
            "签到成功",
            200,
            r#"{"message":"签到成功，获得 5 额度"}"#
        ));
    }

    #[test]
    fn judge_falls_back_to_status_when_needle_absent() {
        assert!(judge("", 200, "whatever"));
        assert!(!judge("", 401, "unauthorized"));
    }

    #[test]
    fn collect_cookies_strips_attributes() {
        let mut headers = HeaderMap::new();
        headers.append(
            SET_COOKIE,
            HeaderValue::from_static("session=abc; Path=/; HttpOnly"),
        );
        headers.append(SET_COOKIE, HeaderValue::from_static("token=xyz; Secure"));
        assert_eq!(collect_cookies(&headers), "session=abc; token=xyz");
    }

    #[test]
    fn extract_message_reads_common_fields() {
        assert_eq!(extract_message(r#"{"ret":1,"msg":"签到成功"}"#), "签到成功");
        assert_eq!(extract_message("plain text"), "plain text");
    }

    #[test]
    fn cloudflare_challenge_needs_both_status_and_marker() {
        let page = r#"<html><head><title>Just a moment...</title></head></html>"#;
        assert!(is_cloudflare_challenge(403, page));
        assert!(is_cloudflare_challenge(503, page));
        assert!(is_cloudflare_challenge(429, page));
        // 200 的正常响应即使含关键词也不算挑战，否则会把成功当拦截。
        assert!(!is_cloudflare_challenge(200, page));
        // 拦截状态码但无挑战特征，是站点自己的 403，应走普通失败路径。
        assert!(!is_cloudflare_challenge(403, r#"{"msg":"未登录"}"#));
    }

    #[test]
    fn cloudflare_challenge_recognizes_script_markers() {
        // CF 的挑战页面不一定带 "Just a moment"，还要认脚本侧特征。
        assert!(is_cloudflare_challenge(
            503,
            r#"<script>window._cf_chl_opt={cvId:"3"}</script>"#
        ));
        assert!(is_cloudflare_challenge(
            403,
            r#"<div id="cf-browser-verification"></div>"#
        ));
        assert!(is_cloudflare_challenge(
            403,
            "/cdn-cgi/challenge-platform/h/b/orchestrate"
        ));
    }

    #[test]
    fn read_body_truncation_respects_char_boundary() {
        // 纯 ASCII 直接过，多字节不能切坏。
        let long = "签".repeat(2000);
        let truncated: String = {
            let end = long
                .char_indices()
                .map(|(i, _)| i)
                .take_while(|i| *i <= MAX_BODY)
                .last()
                .unwrap_or(0);
            long[..end].to_string()
        };
        assert!(truncated.len() <= MAX_BODY);
        assert!(!truncated.is_empty());
    }
}
