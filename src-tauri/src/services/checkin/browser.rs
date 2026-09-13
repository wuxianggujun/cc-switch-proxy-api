//! 用真实 WebView 过 Cloudflare 挑战，取出 `cf_clearance` 供 reqwest 复用
//!
//! CF 的 JS 挑战校验浏览器运行时与 TLS 指纹，伪装请求头无法通过；而非交互式
//! 挑战在真实浏览器里会自行算完并跳转。所以这里开一个真实 WebviewWindow
//! 加载站点，等 cookie 出现后只取 `cf_clearance`；独立 profile 的账号 Cookie
//! 由 profile 模块单独读取，用户显式请求头仍有最高优先级。
//!
//! 两个硬约束，都来自 CF 的 cookie 绑定策略：
//!
//! - `cf_clearance` 绑定 **UA**。这里不去读 WebView 的默认 UA，而是**主动把
//!   同一个 UA 同时设给 WebView 和 reqwest**（`user_agent()` builder）。
//!   反向读取要靠注入脚本回传，既脆弱又与 WebView2 版本相关。
//! - `cf_clearance` 绑定 **出口 IP**。签到请求不能走代理池，换 IP 即失效。
//!
//! Windows 上 `cookies()` 在同步命令或事件回调里会死锁（wry#583），
//! 所以本模块全部对外 API 都是 async，且不在持锁期间跨 await。

use std::time::Duration;

use super::{parse_browser_url, profile, CheckinSite};
use tauri::{AppHandle, Manager};
use tokio::time::sleep;

/// CF 放行 cookie 名。
pub const CF_CLEARANCE: &str = "cf_clearance";

/// 过闸与签到统一使用的 UA。cf_clearance 绑定 UA，两端必须完全一致，
/// 所以固定成常量而非各处硬编码。
pub const CLEARANCE_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
     AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

/// 轮询间隔。CF 非交互式挑战通常 4~5 秒完成。
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// 隐藏窗口的等待上限：只够跑非交互式挑战。
const HEADLESS_BUDGET: Duration = Duration::from_secs(20);

/// 显示窗口后的等待上限：留给用户点 Turnstile。
const INTERACTIVE_BUDGET: Duration = Duration::from_secs(180);

/// 过闸产物。
#[derive(Debug, Clone)]
pub struct ClearanceOutcome {
    /// 可直接用于 `Cookie` 头的串（仅含 cf_clearance，不包含账号登录态）。
    pub cookie: String,
    /// 与 cookie 配对的 UA，必须原样交给 reqwest。
    pub user_agent: String,
    /// 是否经过用户交互才拿到，供前端提示。
    pub needed_interaction: bool,
}

struct ClearanceWindow(tauri::WebviewWindow);

impl Drop for ClearanceWindow {
    fn drop(&mut self) {
        profile::close_child_windows(self.0.app_handle(), self.0.label());
        if let Err(error) = self.0.destroy() {
            log::debug!("[Checkin] 关闭验证窗口: {error}");
        }
    }
}

/// 打开 WebView 过闸并取回 cookie。
///
/// 先隐藏窗口尝试自动过（非交互式挑战）；超时仍未拿到 cookie，则显示窗口
/// 让用户完成 Turnstile 再继续等。两段都超时才返回错误。
pub async fn acquire_clearance(
    app: &AppHandle,
    site: &CheckinSite,
    force_refresh: bool,
) -> Result<ClearanceOutcome, String> {
    let challenge_url = parse_browser_url(site.resolve_challenge_url())?;
    let request_url = parse_browser_url(&site.request.url)?;
    let browser_profile = profile::BrowserProfile::for_site(site)?;
    // Keep the entry's context alive while temporary verification windows come and go.
    let session_window = profile::session_window(app, &browser_profile)?;
    if force_refresh {
        // Removing only the DB cache is insufficient: polling would immediately pick
        // up the same rejected WebView cookie again. Never delete the account cookies.
        for url in [&request_url, &challenge_url] {
            for cookie in profile::cookies_for_url_when_ready(&session_window, url.clone())
                .await?
                .into_iter()
                .filter(|cookie| cookie.name() == CF_CLEARANCE)
            {
                session_window
                    .delete_cookie(cookie)
                    .map_err(|error| format!("清除失效过闸 Cookie 失败: {error}"))?;
            }
        }
    }
    let label = format!(
        "{}-clearance-{}",
        browser_profile.window_prefix,
        uuid::Uuid::new_v4().simple()
    );
    let window = profile::create_window(app, &browser_profile, &label, challenge_url, false, None)?;

    // RAII also destroys a hidden window if shutdown cancels the awaiting task.
    let window = ClearanceWindow(window);
    // Cookie scope must match the actual API request, not just the verification page.
    run_challenge(&window.0, &request_url).await
}

async fn run_challenge(
    window: &tauri::WebviewWindow,
    parsed: &tauri::Url,
) -> Result<ClearanceOutcome, String> {
    // 阶段一：隐藏窗口自动过非交互式挑战。
    if let Some(cookie) = poll_for_clearance(window, parsed, HEADLESS_BUDGET).await? {
        return Ok(ClearanceOutcome {
            cookie,
            user_agent: CLEARANCE_USER_AGENT.to_string(),
            needed_interaction: false,
        });
    }

    // 阶段二：交互式挑战（Turnstile）需要用户点一下。
    log::info!("[Checkin] 自动过闸超时，显示窗口等待用户完成验证");
    let _ = window.set_skip_taskbar(false);
    let _ = window.show();
    let _ = window.set_focus();

    match poll_for_clearance(window, parsed, INTERACTIVE_BUDGET).await? {
        Some(cookie) => Ok(ClearanceOutcome {
            cookie,
            user_agent: CLEARANCE_USER_AGENT.to_string(),
            needed_interaction: true,
        }),
        None => Err("等待站点验证超时，未获取到 cf_clearance".to_string()),
    }
}

/// 轮询 cookie 存储，直到出现 `cf_clearance` 或超预算。
///
/// `Ok(None)` 表示预算内没等到，由调用方决定下一步；`Err` 只用于真正的读取失败。
async fn poll_for_clearance(
    window: &tauri::WebviewWindow,
    parsed: &tauri::Url,
    budget: Duration,
) -> Result<Option<String>, String> {
    let deadline = tokio::time::Instant::now() + budget;

    loop {
        // cookies_for_url 会带上 HttpOnly，cf_clearance 正是 HttpOnly。
        let cookies = profile::cookies_for_url_when_ready(window, parsed.clone()).await?;

        if cookies
            .iter()
            .any(|c| c.name() == CF_CLEARANCE && !c.value().is_empty())
        {
            return Ok(Some(join_cookies(&cookies)));
        }

        if tokio::time::Instant::now() >= deadline {
            return Ok(None);
        }
        sleep(POLL_INTERVAL).await;
    }
}

/// 只导出过闸凭证，不能把共享 WebView 中其它账号的登录态带入签到请求。
fn join_cookies(cookies: &[tauri::webview::Cookie<'static>]) -> String {
    cookies
        .iter()
        .filter(|c| c.name() == CF_CLEARANCE)
        .map(|c| format!("{}={}", c.name(), c.value()))
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tauri::webview::Cookie;

    #[test]
    fn browser_cookie_export_only_contains_cf_clearance() {
        let cookies = vec![
            Cookie::new("session", "account-a"),
            Cookie::new(CF_CLEARANCE, "passed=="),
            Cookie::new("auth_token", "account-a-token"),
            Cookie::new("__cf_bm", "bot-management"),
            Cookie::new("CF_CLEARANCE", "not-the-clearance-cookie"),
        ];

        assert_eq!(join_cookies(&cookies), "cf_clearance=passed==");
    }

    #[test]
    fn browser_cookie_export_without_clearance_is_empty() {
        assert_eq!(join_cookies(&[]), "");
        assert_eq!(join_cookies(&[Cookie::new("session", "account-a")]), "");
    }
}
