//! Per-entry persistent browser profiles. Account cookies never leave the backend.

use super::browser::{CF_CLEARANCE, CLEARANCE_USER_AGENT};
use super::{parse_browser_url, CheckinBrowserSessionStatus, CheckinSite};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::Duration;
use tauri::webview::{Cookie, NewWindowFeatures, NewWindowResponse};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

pub const BROWSER_UPDATED_EVENT: &str = "checkin-browser-updated";
const MAX_PROFILE_WINDOWS: usize = 8;
const WEBVIEW_READY_RETRIES: usize = 30;
const WEBVIEW_READY_INTERVAL: Duration = Duration::from_millis(100);
static OPEN_LOGIN_WINDOWS: once_cell::sync::Lazy<Mutex<HashSet<String>>> =
    once_cell::sync::Lazy::new(|| Mutex::new(HashSet::new()));
static DISPOSING_LOGIN_WINDOWS: once_cell::sync::Lazy<Mutex<HashSet<String>>> =
    once_cell::sync::Lazy::new(|| Mutex::new(HashSet::new()));

#[derive(Clone)]
pub(super) struct BrowserProfile {
    pub directory: PathBuf,
    pub window_prefix: String,
    site_id: String,
    title: String,
    #[cfg(target_os = "macos")]
    data_store_identifier: [u8; 16],
}

impl BrowserProfile {
    pub fn for_site(site: &CheckinSite) -> Result<Self, String> {
        Self::under_root(&crate::config::get_app_config_dir(), site)
    }

    fn under_root(root: &Path, site: &CheckinSite) -> Result<Self, String> {
        if site.id.trim().is_empty() {
            return Err("请先保存签到条目，再打开独立登录窗口".into());
        }
        if !root.is_absolute() {
            return Err("浏览器 profile 的配置目录必须是绝对路径".into());
        }
        let origin = parse_browser_url(&site.request.url)?
            .origin()
            .ascii_serialization();
        let mut digest = Sha256::new();
        digest.update(b"cc-switch-checkin-profile-v1\0");
        digest.update(site.id.as_bytes());
        digest.update(b"\0");
        digest.update(origin.as_bytes());
        // Never use an imported entry ID as a path segment.
        let storage_key = format!("{:x}", digest.finalize());
        let directory = root.join("checkin").join("profiles").join(storage_key);
        // A config-directory change must not reuse a window backed by the old directory.
        let window_digest = Sha256::digest(directory.to_string_lossy().as_bytes());
        Ok(Self {
            directory,
            window_prefix: format!("checkin-profile-{window_digest:x}"),
            site_id: site.id.clone(),
            title: format!("{} - 独立账号窗口（登录完成后关闭窗口）", site.name),
            #[cfg(target_os = "macos")]
            data_store_identifier: window_digest[..16].try_into().expect("16-byte identifier"),
        })
    }

    pub fn login_label(&self) -> String {
        format!("{}-login", self.window_prefix)
    }

    fn ensure_directory(&self) -> Result<PathBuf, String> {
        ensure_platform_support()?;
        let root = self.directory.parent().ok_or("浏览器 profile 目录无效")?;
        std::fs::create_dir_all(&self.directory)
            .map_err(|error| format!("创建独立浏览器目录失败: {error}"))?;
        let root = root
            .canonicalize()
            .map_err(|error| format!("解析浏览器根目录失败: {error}"))?;
        let directory = self
            .directory
            .canonicalize()
            .map_err(|error| format!("解析浏览器 profile 目录失败: {error}"))?;
        let expected = root.join(
            self.directory
                .file_name()
                .ok_or("浏览器 profile 标识无效")?,
        );
        if directory != expected || !directory.is_dir() {
            return Err("浏览器 profile 目录不能通过链接指向其它账号目录".into());
        }
        // canonicalize is only for containment validation. On Windows its verbatim
        // \\?\ path is not a usable Chromium SQLite user-data path: cookies would
        // remain memory-only even though WebView2 creates the profile directory.
        Ok(self.directory.clone())
    }
}

fn ensure_platform_support() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        static SUPPORTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if !SUPPORTED.get_or_init(|| {
            std::process::Command::new("/usr/bin/sw_vers")
                .arg("-productVersion")
                .output()
                .ok()
                .filter(|output| output.status.success())
                .and_then(|output| String::from_utf8(output.stdout).ok())
                .and_then(|version| version.trim().split('.').next()?.parse::<u32>().ok())
                .is_some_and(|major| major >= 14)
        }) {
            return Err(
                "独立持久化浏览器 profile 需要 macOS 14 或更高版本，不能回退到共享登录态".into(),
            );
        }
    }
    Ok(())
}

fn browser_navigation_allowed(url: &url::Url) -> bool {
    matches!(url.scheme(), "http" | "https") || url.as_str() == "about:blank"
}

fn set_login_window_open(label: &str, open: bool) {
    let mut windows = OPEN_LOGIN_WINDOWS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if open {
        windows.insert(label.to_string());
    } else {
        windows.remove(label);
    }
}

fn set_login_window_disposing(label: &str, disposing: bool) {
    let mut windows = DISPOSING_LOGIN_WINDOWS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if disposing {
        windows.insert(label.to_string());
    } else {
        windows.remove(label);
    }
}

fn login_window_disposing(label: &str) -> bool {
    DISPOSING_LOGIN_WINDOWS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains(label)
}

/// Login, challenge, cookie readers, and popup windows all go through this factory.
/// Called from async commands; Tauri also runs the Windows popup callback off the UI thread.
pub(super) fn create_window(
    app: &AppHandle,
    profile: &BrowserProfile,
    label: &str,
    url: url::Url,
    visible: bool,
    features: Option<NewWindowFeatures>,
) -> Result<WebviewWindow, String> {
    let directory = profile.ensure_directory()?;
    let popup_app = app.clone();
    let popup_profile = profile.clone();
    let parent_label = label.to_string();
    let from_login_window = label.starts_with(&profile.login_label());
    // WebView2 assigns the opener only after on_new_window returns. A popup must
    // survive until its first real navigation, even if its owner closes immediately.
    let is_popup = features.is_some();
    let popup_attached = Arc::new(AtomicBool::new(!is_popup));
    let close_pending = Arc::new(AtomicBool::new(false));
    let attached_on_load = popup_attached.clone();
    let close_on_load = close_pending.clone();
    let mut builder = WebviewWindowBuilder::new(app, label, WebviewUrl::External(url))
        .title(&profile.title)
        .inner_size(900.0, 720.0)
        .visible(visible)
        .focused(visible)
        .skip_taskbar(!visible)
        .data_directory(directory)
        .user_agent(CLEARANCE_USER_AGENT)
        .on_navigation(browser_navigation_allowed)
        .on_page_load(move |window, payload| {
            if payload.url().scheme() != "about" {
                attached_on_load.store(true, Ordering::Release);
                if close_on_load.swap(false, Ordering::AcqRel) {
                    let window = window.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Err(error) = window.close() {
                            log::debug!("[Checkin] 关闭已完成初始化的账号弹窗失败: {error}");
                        }
                    });
                }
            }
        })
        .on_new_window(move |url, features| {
            if !from_login_window || login_window_disposing(&popup_profile.login_label()) {
                return NewWindowResponse::Deny;
            }
            match login_window_open(&popup_app, &popup_profile) {
                Ok(true) => {}
                Ok(false) => return NewWindowResponse::Deny,
                Err(error) => {
                    log::debug!("[Checkin] 登录窗口已不可用，取消弹窗: {error}");
                    return NewWindowResponse::Deny;
                }
            }
            let count = popup_app
                .webview_windows()
                .keys()
                .filter(|label| label.starts_with(&popup_profile.window_prefix))
                .count();
            if !browser_navigation_allowed(&url) || count >= MAX_PROFILE_WINDOWS {
                return NewWindowResponse::Deny;
            }
            let label = format!("{parent_label}-popup-{}", uuid::Uuid::new_v4().simple());
            // window_features preserves the opener; the explicit profile prevents a popup
            // from falling back to the main window's WebContext.
            match create_window(
                &popup_app,
                &popup_profile,
                &label,
                url::Url::parse("about:blank").expect("static URL"),
                true,
                Some(features),
            ) {
                Ok(window) => NewWindowResponse::Create { window },
                Err(error) => {
                    log::warn!("[Checkin] 创建独立账号子窗口失败: {error}");
                    NewWindowResponse::Deny
                }
            }
        });
    #[cfg(target_os = "macos")]
    {
        builder = builder.data_store_identifier(profile.data_store_identifier);
    }
    if let Some(features) = features {
        builder = builder.window_features(features);
    }
    let window = builder
        .build()
        .map_err(|error| format!("创建独立浏览器窗口失败: {error}"))?;
    let event_window = window.clone();
    let site_id = profile.site_id.clone();
    let is_login = label == profile.login_label();
    let login_root_label = profile.login_label();
    window.on_window_event(move |event| match event {
        tauri::WindowEvent::CloseRequested { api, .. }
            if is_popup && !popup_attached.load(Ordering::Acquire) =>
        {
            api.prevent_close();
            close_pending.store(true, Ordering::Release);
            if let Err(error) = event_window.hide() {
                log::debug!("[Checkin] 隐藏尚未完成初始化的账号弹窗失败: {error}");
            }
        }
        tauri::WindowEvent::CloseRequested { api, .. } if is_login => {
            // Keep session cookies alive until the app exits; persistent cookies also
            // survive restart. Reading cookies here would deadlock WebView2.
            api.prevent_close();
            if let Err(error) = event_window.hide() {
                log::warn!("[Checkin] 隐藏账号窗口失败: {error}");
                return;
            }
            set_login_window_open(event_window.label(), false);
            if let Err(error) = event_window.set_skip_taskbar(true) {
                log::debug!("[Checkin] 隐藏账号任务栏窗口失败: {error}");
            }
            // Never tear down an OAuth popup reentrantly inside its opener's native
            // close callback. Queue destruction from a worker after this event returns.
            let app = event_window.app_handle().clone();
            let label = event_window.label().to_string();
            let site_id = site_id.clone();
            tauri::async_runtime::spawn(async move {
                close_child_windows(&app, &label);
                notify_updated(&app, &site_id);
            });
        }
        tauri::WindowEvent::Destroyed => {
            if is_login {
                set_login_window_open(event_window.label(), false);
                set_login_window_disposing(event_window.label(), false);
            }
            notify_updated(event_window.app_handle(), &site_id);
            let app = event_window.app_handle().clone();
            let login_root_label = login_root_label.clone();
            tauri::async_runtime::spawn(async move {
                finalize_disposed_login_window(&app, &login_root_label);
            });
        }
        _ => {}
    });
    Ok(window)
}

pub(super) fn close_child_windows(app: &AppHandle, parent_label: &str) {
    let prefix = format!("{parent_label}-popup-");
    for (label, window) in app.webview_windows() {
        if label.starts_with(&prefix) {
            if let Err(error) = window.close() {
                log::warn!("[Checkin] 关闭账号子窗口失败: {error}");
            }
        }
    }
}

fn finalize_disposed_login_window(app: &AppHandle, login_label: &str) {
    if !login_window_disposing(login_label) {
        return;
    }
    let child_prefix = format!("{login_label}-popup-");
    if app
        .webview_windows()
        .keys()
        .any(|label| label.starts_with(&child_prefix))
    {
        return;
    }
    set_login_window_disposing(login_label, false);
    if let Some(window) = app.get_webview_window(login_label) {
        if let Err(error) = window.destroy() {
            log::warn!("[Checkin] 回收旧账号窗口失败: {error}");
        }
    }
}

pub(super) fn close_login_window(app: &AppHandle, site: &CheckinSite) {
    if let Ok(profile) = BrowserProfile::for_site(site) {
        let label = profile.login_label();
        set_login_window_open(&label, false);
        set_login_window_disposing(&label, true);
        if let Some(window) = app.get_webview_window(&label) {
            if let Err(error) = window.hide() {
                log::warn!("[Checkin] 隐藏待回收账号窗口失败: {error}");
            }
        }
        close_child_windows(app, &label);
        finalize_disposed_login_window(app, &label);
    }
}

fn notify_updated(app: &AppHandle, id: &str) {
    if let Err(error) = app.emit(BROWSER_UPDATED_EVENT, id) {
        log::debug!("[Checkin] 推送浏览器状态失败: {error}");
    }
}

pub(super) fn session_window(
    app: &AppHandle,
    profile: &BrowserProfile,
) -> Result<WebviewWindow, String> {
    let label = profile.login_label();
    if let Some(window) = app.get_webview_window(&label) {
        return Ok(window);
    }
    create_window(
        app,
        profile,
        &label,
        url::Url::parse("about:blank").expect("static URL"),
        false,
        None,
    )
}

pub(super) fn login_window_open(
    _app: &AppHandle,
    profile: &BrowserProfile,
) -> Result<bool, String> {
    let windows = OPEN_LOGIN_WINDOWS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Ok(windows.contains(&profile.login_label()))
}

pub async fn open_login(app: &AppHandle, site: &CheckinSite) -> Result<(), String> {
    let profile = BrowserProfile::for_site(site)?;
    if login_window_disposing(&profile.login_label()) {
        return Err("该条目的旧账号窗口正在关闭，请稍后重试".into());
    }
    let window = session_window(app, &profile)?;
    if !login_window_open(app, &profile)? {
        window
            .navigate(site.resolve_login_url()?)
            .map_err(|error| format!("打开登录页面失败: {error}"))?;
    }
    window
        .set_title(&profile.title)
        .map_err(|error| format!("更新登录窗口标题失败: {error}"))?;
    window
        .set_skip_taskbar(false)
        .map_err(|error| format!("显示登录窗口失败: {error}"))?;
    window
        .show()
        .map_err(|error| format!("显示登录窗口失败: {error}"))?;
    set_login_window_open(&profile.login_label(), true);
    if let Err(error) = window.set_focus() {
        log::debug!("[Checkin] 聚焦登录窗口失败: {error}");
    }
    notify_updated(app, &site.id);
    Ok(())
}

pub(super) struct BrowserSession {
    pub account_cookie: String,
    pub clearance_cookie: Option<String>,
    pub status: CheckinBrowserSessionStatus,
}

pub(super) fn is_account_cookie(name: &str) -> bool {
    !name.is_empty()
        && name != CF_CLEARANCE
        && !name.starts_with("cf_chl_")
        && !matches!(name, "__cf_bm" | "__cflb" | "__cfseq" | "_cfuvid")
}

fn split_session(cookies: &[Cookie<'static>], login_window_open: bool) -> BrowserSession {
    let account_cookies: Vec<_> = cookies
        .iter()
        .filter(|cookie| is_account_cookie(cookie.name()))
        .collect();
    let clearance_cookie = cookies
        .iter()
        .find(|cookie| cookie.name() == CF_CLEARANCE && !cookie.value().is_empty())
        .map(|cookie| format!("{CF_CLEARANCE}={}", cookie.value()));
    BrowserSession {
        account_cookie: account_cookies
            .iter()
            .map(|cookie| format!("{}={}", cookie.name(), cookie.value()))
            .collect::<Vec<_>>()
            .join("; "),
        clearance_cookie,
        status: CheckinBrowserSessionStatus {
            account_cookie_count: account_cookies.len(),
            login_window_open,
        },
    }
}

pub(super) async fn read_session(
    app: &AppHandle,
    site: &CheckinSite,
) -> Result<BrowserSession, String> {
    let profile = BrowserProfile::for_site(site)?;
    // Merely opening a form for a new entry must not create a WebView or a profile.
    if !profile.directory.exists() && app.get_webview_window(&profile.login_label()).is_none() {
        return Ok(split_session(&[], false));
    }
    let window = session_window(app, &profile)?;
    let request_url = parse_browser_url(&site.request.url)?;
    let cookies = cookies_for_url_when_ready(&window, request_url).await?;
    Ok(split_session(&cookies, login_window_open(app, &profile)?))
}

pub(super) async fn cookies_for_url_when_ready(
    window: &WebviewWindow,
    url: url::Url,
) -> Result<Vec<Cookie<'static>>, String> {
    let mut last_error = None;
    for attempt in 0..=WEBVIEW_READY_RETRIES {
        match window.cookies_for_url(url.clone()) {
            Ok(cookies) => return Ok(cookies),
            Err(error) if attempt < WEBVIEW_READY_RETRIES => {
                last_error = Some(error.to_string());
                tokio::time::sleep(WEBVIEW_READY_INTERVAL).await;
            }
            Err(error) => last_error = Some(error.to_string()),
        }
    }
    Err(format!(
        "读取独立账号 Cookie 失败: {}",
        last_error.unwrap_or_else(|| "WebView 尚未就绪".into())
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site(id: &str, url: &str) -> CheckinSite {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": "Account", "authKind": "browser", "request": {"url": url, "method": "GET"}
        }))
        .unwrap()
    }

    #[test]
    fn profile_b_same_origin_accounts_have_different_persistent_directories() {
        let root = tempfile::tempdir().unwrap();
        let a = BrowserProfile::under_root(root.path(), &site("a", "https://same.example/checkin"))
            .unwrap();
        let b = BrowserProfile::under_root(root.path(), &site("b", "https://same.example/checkin"))
            .unwrap();
        assert_ne!(a.directory, b.directory);
        assert_ne!(a.login_label(), b.login_label());
        assert_ne!(a.login_label(), "main");
        let mut renamed = site("a", "https://SAME.example:443/other?query=value");
        renamed.name = "Renamed account".into();
        let restarted = BrowserProfile::under_root(root.path(), &renamed).unwrap();
        assert_eq!(a.directory, restarted.directory);
        assert_eq!(a.login_label(), restarted.login_label());
        let moved =
            BrowserProfile::under_root(root.path(), &site("a", "https://other.example/")).unwrap();
        assert_ne!(a.directory, moved.directory);
    }

    #[test]
    fn profile_b_imported_id_cannot_escape_the_profile_root() {
        let root = tempfile::tempdir().unwrap();
        let profile = BrowserProfile::under_root(
            root.path(),
            &site("../../main\\..\\other", "https://same.example/"),
        )
        .unwrap();
        assert_eq!(
            profile.directory.parent().unwrap(),
            root.path().join("checkin/profiles")
        );
        let name = profile.directory.file_name().unwrap().to_str().unwrap();
        assert_eq!(name.len(), 64);
        assert!(name.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert!(
            BrowserProfile::under_root(root.path(), &site("", "https://same.example/")).is_err()
        );
        assert!(
            BrowserProfile::under_root(root.path(), &site("a", "file:///tmp/account")).is_err()
        );
    }

    #[test]
    fn profile_b_account_cookies_and_clearance_are_separate_channels() {
        let session = split_session(
            &[
                Cookie::new("session", "account-b=="),
                Cookie::new(CF_CLEARANCE, "passed"),
                Cookie::new("__cf_bm", "bot-cookie"),
                Cookie::new("cf_chl_rc_i", "challenge-cookie"),
            ],
            false,
        );
        assert_eq!(session.account_cookie, "session=account-b==");
        assert_eq!(
            session.clearance_cookie.as_deref(),
            Some("cf_clearance=passed")
        );
        assert_eq!(session.status.account_cookie_count, 1);
        let status = serde_json::to_string(&session.status).unwrap();
        assert!(!status.contains("account-b"));
        assert!(!status.contains("passed"));
        assert_eq!(
            split_session(&[Cookie::new(CF_CLEARANCE, "passed")], false)
                .status
                .account_cookie_count,
            0
        );
    }

    #[test]
    fn profile_b_navigation_does_not_open_local_files_or_custom_protocols() {
        for url in [
            "https://same.example/login",
            "http://localhost/login",
            "about:blank",
        ] {
            assert!(browser_navigation_allowed(&url.parse().unwrap()));
        }
        for url in [
            "file:///C:/secret",
            "tauri://localhost",
            "ccswitch://import",
            "data:text/html,test",
        ] {
            assert!(!browser_navigation_allowed(&url.parse().unwrap()));
        }
    }

    #[test]
    fn profile_b_tracks_login_window_state_without_querying_webview_runtime() {
        let root = tempfile::tempdir().unwrap();
        let profile =
            BrowserProfile::under_root(root.path(), &site("state", "https://same.example/"))
                .unwrap();
        set_login_window_open(&profile.login_label(), false);
        assert!(!login_window_open_for_test(&profile));
        set_login_window_open(&profile.login_label(), true);
        assert!(login_window_open_for_test(&profile));
        set_login_window_open(&profile.login_label(), false);
        assert!(!login_window_open_for_test(&profile));
    }

    fn login_window_open_for_test(profile: &BrowserProfile) -> bool {
        OPEN_LOGIN_WINDOWS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(&profile.login_label())
    }

    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "Requires an interactive Windows desktop, WebView2, and CC_SWITCH_TEST_HOME"]
    fn profile_b_native_webview2_isolation_and_persistence() {
        use futures::FutureExt;
        use std::sync::{mpsc, Arc};
        use std::time::Duration;

        assert!(
            std::env::var("CC_SWITCH_TEST_HOME").is_ok(),
            "Use an isolated test home"
        );
        let (done_tx, done_rx) = mpsc::channel::<Result<u16, String>>();
        let reopen_process = std::env::var_os("CC_SWITCH_PROFILE_FIXTURE_REOPEN").is_some();
        let mut context = tauri::generate_context!();
        context.config_mut().app.windows.clear();
        let app = tauri::Builder::default().any_thread().setup(move |app| {
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let test = std::panic::AssertUnwindSafe(async {
                    let replay_port = std::env::var("CC_SWITCH_PROFILE_FIXTURE_PORT")
                        .ok().map(|port| port.parse::<u16>().unwrap()).unwrap_or(0);
                    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, replay_port)).await.unwrap();
                    let port = listener.local_addr().unwrap().port();
                    let base = format!("http://{}", listener.local_addr().unwrap());
                    let router = axum::Router::new()
                        .route("/login/:account", axum::routing::get(|axum::extract::Path(account): axum::extract::Path<String>| async move {
                            let mut headers = axum::http::HeaderMap::new();
                            headers.append(axum::http::header::SET_COOKIE,
                                format!("session={account}; Path=/api; HttpOnly; Max-Age=3600").parse().unwrap());
                            headers.append(axum::http::header::SET_COOKIE,
                                format!("cf_clearance=clearance-{account}; Path=/; HttpOnly; Max-Age=3600").parse().unwrap());
                            (headers, axum::response::Html("<html><body>Local account fixture</body></html>"))
                        }))
                        .route("/challenge", axum::routing::get(|| async { "local verification fixture" }))
                        .route("/api/checkin", axum::routing::get(|headers: axum::http::HeaderMap| async move {
                            axum::Json(serde_json::json!({"message": headers.get("cookie").and_then(|value| value.to_str().ok()).unwrap_or("")}))
                        }));
                    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
                    let state = crate::store::AppState::new(Arc::new(crate::database::Database::memory().unwrap()));
                    let make_site = |id: &str| {
                        let mut entry = site(id, &format!("{base}/api/checkin"));
                        entry.browser = Some(super::super::CheckinBrowser {
                            login_url: format!("{base}/login/{id}"),
                            challenge_url: format!("{base}/challenge"),
                            cached: None,
                        });
                        super::super::CheckinService::upsert_site(&state, entry).unwrap()
                    };
                    let a = make_site("account-a");
                    let b = make_site("account-b");
                    if std::env::var_os("CC_SWITCH_PROFILE_FIXTURE_REOPEN").is_some() {
                        for entry in [&a, &b] {
                            let session = read_session(&handle, entry).await.unwrap();
                            assert_eq!(session.account_cookie, format!("session={}", entry.id));
                            assert_eq!(session.clearance_cookie, Some(format!("cf_clearance=clearance-{}", entry.id)));
                            close_login_window(&handle, entry);
                        }
                        eprintln!("[native-profile] fresh-process persistence passed for A and B");
                        server.abort();
                        return port;
                    }
                    for entry in [&a, &b] {
                        eprintln!("[native-profile] open login: {}", entry.id);
                        open_login(&handle, entry).await.unwrap();
                        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
                        loop {
                            let session = read_session(&handle, entry).await.unwrap();
                            if session.account_cookie == format!("session={}", entry.id) { break; }
                            assert!(tokio::time::Instant::now() < deadline, "login cookie timeout for {}", entry.id);
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                    }
                    let profile_a = BrowserProfile::for_site(&a).unwrap();
                    let window_a = handle.get_webview_window(&profile_a.login_label()).unwrap();
                    eprintln!("[native-profile] create A popup");
                    // A popup must share A's context, never the main window or B's context.
                    window_a.eval("window.open('/challenge', '_blank')").unwrap();
                    let popup_prefix = format!("{}-popup-", window_a.label());
                    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
                    let popup = loop {
                        if let Some((_, window)) = handle.webview_windows().into_iter().find(|(label, _)| label.starts_with(&popup_prefix)) {
                            break window;
                        }
                        assert!(tokio::time::Instant::now() < deadline, "popup window timeout");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    };
                    let popup_session = split_session(&popup.cookies_for_url(parse_browser_url(&a.request.url).unwrap()).unwrap(), true);
                    assert_eq!(popup_session.account_cookie, "session=account-a");
                    eprintln!("[native-profile] popup isolation passed");

                    for entry in [&a, &b] {
                        eprintln!("[native-profile] close login: {}", entry.id);
                        let label = BrowserProfile::for_site(entry).unwrap().login_label();
                        let window = handle.get_webview_window(&label).unwrap();
                        window.close().unwrap();
                        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
                        while login_window_open(&handle, &BrowserProfile::for_site(entry).unwrap()).unwrap() {
                            assert!(tokio::time::Instant::now() < deadline, "login close timeout");
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                        assert!(handle.get_webview_window(&label).is_some(), "close preserves session window");
                        let session = read_session(&handle, entry).await.unwrap();
                        assert_eq!(session.account_cookie, format!("session={}", entry.id));
                        eprintln!("[native-profile] acquire clearance: {}", entry.id);
                        let outcome = super::super::browser::acquire_clearance(&handle, entry, false).await.unwrap();
                        assert_eq!(outcome.cookie, format!("cf_clearance=clearance-{}", entry.id));
                        let result = super::super::runner::run_browser_checkin(entry,
                            Some(super::super::runner::Clearance { cookie: &outcome.cookie, user_agent: &outcome.user_agent }),
                            &session.account_cookie).await;
                        assert_eq!(result.status, super::super::CheckinStatus::Success, "{result:?}");
                        assert!(result.message.contains(&format!("session={}", entry.id)));
                        eprintln!("[native-profile] HTTP checkin passed: {}", entry.id);
                    }
                    // Re-create A's root WebView while another WebView still owns the same
                    // context. Destroying the final WebView intentionally tears Chromium's
                    // environment down; immediately reopening that user-data folder can race
                    // its asynchronous disk shutdown. The application never does that for an
                    // unchanged binding: its hidden session root lives until origin removal or
                    // process exit. The fresh child process below verifies on-disk persistence.
                    eprintln!("[native-profile] recreate A session window");
                    let keeper_label = format!("{}-recreate-keeper", profile_a.window_prefix);
                    let keeper = create_window(
                        &handle,
                        &profile_a,
                        &keeper_label,
                        url::Url::parse("about:blank").unwrap(),
                        false,
                        None,
                    ).unwrap();
                    window_a.destroy().unwrap();
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    assert_eq!(read_session(&handle, &a).await.unwrap().account_cookie, "session=account-a");
                    assert_eq!(read_session(&handle, &b).await.unwrap().account_cookie, "session=account-b");
                    keeper.destroy().unwrap();
                    eprintln!("[native-profile] persistence passed; close fixture windows");
                    close_login_window(&handle, &a);
                    close_login_window(&handle, &b);
                    server.abort();
                    port
                }).catch_unwind();
                let result = tokio::time::timeout(Duration::from_secs(75), test).await;
                let result = match result {
                    Ok(Ok(port)) => Ok(port),
                    Ok(Err(_)) => Err("native profile fixture panicked".to_string()),
                    Err(_) => Err("native profile fixture timed out".to_string()),
                };
                eprintln!("[native-profile] exit fixture app, success={}", result.is_ok());
                if reopen_process {
                    std::process::exit(if result.is_ok() { 0 } else { 1 });
                }
                done_tx.send(result).unwrap();
                handle.exit(0);
            });
            Ok(())
        }).build(context).unwrap();
        assert_eq!(app.run_return(|_, _| {}), 0);
        let port = done_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap_or_else(|error| panic!("native profile isolation failed: {error}"));
        if std::env::var_os("CC_SWITCH_PROFILE_FIXTURE_REOPEN").is_none() {
            std::thread::sleep(Duration::from_secs(1));
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "services::checkin::profile::tests::profile_b_native_webview2_isolation_and_persistence",
                    "--exact", "--ignored", "--test-threads=1", "--nocapture",
                ])
                .env("CC_SWITCH_PROFILE_FIXTURE_REOPEN", "1")
                .env("CC_SWITCH_PROFILE_FIXTURE_PORT", port.to_string())
                .status()
                .unwrap();
            assert!(
                status.success(),
                "fresh-process profile persistence failed: {status}"
            );
        }
    }
}
