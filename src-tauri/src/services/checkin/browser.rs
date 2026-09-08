//! 用真实 WebView 过 Cloudflare 挑战，取出 `cf_clearance` 供 reqwest 复用
//!
//! CF 的 JS 挑战校验浏览器运行时与 TLS 指纹，伪装请求头无法通过；而非交互式
//! 挑战在真实浏览器里会自行算完并跳转。所以这里开一个真实 WebviewWindow
//! 加载站点，等 cookie 出现再取走。
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

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};
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
    /// 可直接用于 `Cookie` 头的串（含 cf_clearance 及同域其它 cookie）。
    pub cookie: String,
    /// 与 cookie 配对的 UA，必须原样交给 reqwest。
    pub user_agent: String,
    /// 是否经过用户交互才拿到，供前端提示。
    pub needed_interaction: bool,
}

struct ClearanceWindow(tauri::WebviewWindow);

impl Drop for ClearanceWindow {
    fn drop(&mut self) {
        if let Err(error) = self.0.destroy() {
            log::debug!("[Checkin] 关闭验证窗口: {error}");
        }
    }
}

/// 打开 WebView 过闸并取回 cookie。
///
/// 先隐藏窗口尝试自动过（非交互式挑战）；超时仍未拿到 cookie，则显示窗口
/// 让用户完成 Turnstile 再继续等。两段都超时才返回错误。
pub async fn acquire_clearance(app: &AppHandle, url: &str) -> Result<ClearanceOutcome, String> {
    let target = url.trim();
    if !(target.starts_with("http://") || target.starts_with("https://")) {
        return Err(format!(
            "过闸 URL 必须以 http:// 或 https:// 开头: {target}"
        ));
    }
    let parsed: tauri::Url = target
        .parse()
        .map_err(|e| format!("过闸 URL 解析失败: {e}"))?;

    let label = format!("cf-clearance-{}", uuid::Uuid::new_v4().simple());
    if let Some(existing) = app.get_webview_window(&label) {
        let _ = existing.destroy();
    }

    let window = WebviewWindowBuilder::new(app, &label, WebviewUrl::External(parsed.clone()))
        .title("正在通过站点验证…")
        .inner_size(480.0, 640.0)
        .visible(false)
        .focused(false)
        .skip_taskbar(true)
        // 与 reqwest 侧共用同一 UA，cookie 才不会因 UA 不匹配失效。
        .user_agent(CLEARANCE_USER_AGENT)
        .build()
        .map_err(|e| format!("创建验证窗口失败: {e}"))?;

    // RAII also destroys a hidden window if shutdown cancels the awaiting task.
    let window = ClearanceWindow(window);
    run_challenge(&window.0, &parsed).await
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
        let cookies = window
            .cookies_for_url(parsed.clone())
            .map_err(|e| format!("读取 WebView Cookie 失败: {e}"))?;

        if cookies.iter().any(|c| c.name() == CF_CLEARANCE) {
            return Ok(Some(join_cookies(&cookies)));
        }

        if tokio::time::Instant::now() >= deadline {
            return Ok(None);
        }
        sleep(POLL_INTERVAL).await;
    }
}

/// 折成 `a=1; b=2`。
fn join_cookies(cookies: &[tauri::webview::Cookie<'static>]) -> String {
    cookies
        .iter()
        .map(|c| format!("{}={}", c.name(), c.value()))
        .collect::<Vec<_>>()
        .join("; ")
}
