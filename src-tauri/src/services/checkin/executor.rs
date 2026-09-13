//! Shared application service for manual and scheduled check-ins.

use super::browser::{acquire_clearance, ClearanceOutcome, CLEARANCE_USER_AGENT};
use super::profile::{self, BrowserSession};
use super::runner::{run_browser_checkin, run_checkin_with, Clearance};
use super::scheduler::{schedule_is_due, succeeded_on_date};
use super::{
    CheckinAuthKind, CheckinBrowserSessionStatus, CheckinClearance, CheckinResult, CheckinService,
    CheckinSite, CheckinStatus,
};
use crate::store::AppState;
use chrono::{DateTime, Local};
use tauri::{AppHandle, Emitter};

pub async fn upsert_site(
    app: &AppHandle,
    state: &AppState,
    site: CheckinSite,
) -> Result<CheckinSite, String> {
    let _execution = state
        .checkin_runtime
        .execution
        .try_lock()
        .map_err(|_| "签到任务正在执行，请稍后再修改条目".to_string())?;
    let previous = CheckinService::load(state)
        .map_err(|error| error.to_string())?
        .sites
        .into_iter()
        .find(|previous| previous.id == site.id);
    let saved = CheckinService::upsert_site(state, site).map_err(|error| error.to_string())?;
    if let Some(previous) = previous {
        if previous.auth_kind == CheckinAuthKind::Browser
            && !super::same_browser_binding(&previous, &saved)
        {
            profile::close_login_window(app, &previous);
        }
    }
    notify_updated(Some(app));
    Ok(saved)
}

pub async fn delete_site(app: &AppHandle, state: &AppState, id: &str) -> Result<bool, String> {
    let _execution = state
        .checkin_runtime
        .execution
        .try_lock()
        .map_err(|_| "签到任务正在执行，请稍后再删除条目".to_string())?;
    let site = load_site(state, id)?;
    let deleted = CheckinService::delete_site(state, id).map_err(|error| error.to_string())?;
    if deleted {
        profile::close_login_window(app, &site);
        notify_updated(Some(app));
    }
    Ok(deleted)
}

pub async fn open_login(app: &AppHandle, state: &AppState, id: &str) -> Result<(), String> {
    let _execution = state
        .checkin_runtime
        .execution
        .try_lock()
        .map_err(|_| "签到任务正在执行，请稍后打开登录窗口".to_string())?;
    let site = load_browser_site(state, id)?;
    profile::open_login(app, &site).await
}

pub async fn browser_session_status(
    app: &AppHandle,
    state: &AppState,
    id: &str,
) -> Result<CheckinBrowserSessionStatus, String> {
    let _execution = state
        .checkin_runtime
        .execution
        .try_lock()
        .map_err(|_| "签到任务正在执行，请稍后刷新浏览器状态".to_string())?;
    let site = load_browser_site(state, id)?;
    Ok(profile::read_session(app, &site).await?.status)
}

pub async fn run_site(
    app: &AppHandle,
    state: &AppState,
    id: &str,
) -> Result<CheckinResult, String> {
    let _execution = state
        .checkin_runtime
        .execution
        .try_lock()
        .map_err(|_| "签到任务正在执行，请稍后重试".to_string())?;
    execute_and_record(Some(app), state, &load_site(state, id)?).await
}

pub async fn run_all(
    app: &AppHandle,
    state: &AppState,
) -> Result<Vec<(String, CheckinResult)>, String> {
    let _execution = state
        .checkin_runtime
        .execution
        .try_lock()
        .map_err(|_| "签到任务正在执行，请稍后重试".to_string())?;
    let config = CheckinService::load(state).map_err(|error| error.to_string())?;
    let mut results = Vec::new();
    for site in config.sites.into_iter().filter(|site| site.enabled) {
        let result = execute_and_record(Some(app), state, &site).await?;
        results.push((site.id, result));
    }
    Ok(results)
}

pub(super) async fn run_scheduled(
    app: Option<&AppHandle>,
    state: &AppState,
    now: DateTime<Local>,
) -> Result<bool, String> {
    let Ok(_execution) = state.checkin_runtime.execution.try_lock() else {
        return Ok(false);
    };
    let config = CheckinService::load(state).map_err(|error| error.to_string())?;
    if !schedule_is_due(&config, now) {
        return Ok(false);
    }
    let mut login_deferred = false;
    for scheduled in config.sites.into_iter().filter(|site| site.enabled) {
        // Respect edits/deletions made while an earlier browser challenge was open.
        let current = CheckinService::load(state).map_err(|error| error.to_string())?;
        if !current.schedule_enabled {
            return Ok(false);
        }
        if let Some(site) = current
            .sites
            .into_iter()
            .find(|site| site.id == scheduled.id)
        {
            if site.enabled && !succeeded_on_date(&site, now.date_naive()) {
                if let Some(app) = app.filter(|_| site.auth_kind == CheckinAuthKind::Browser) {
                    let browser_profile = profile::BrowserProfile::for_site(&site)?;
                    if profile::login_window_open(app, &browser_profile)? {
                        login_deferred = true;
                        continue;
                    }
                }
                execute_and_record(app, state, &site).await?;
            }
        }
    }
    // Resume this entry after its login window closes, without repeating successful sites.
    if login_deferred {
        return Ok(false);
    }
    // Mark only completed batches. After a crash, resume unfinished sites while
    // skipping sites already successful on this local date.
    CheckinService::complete_scheduled_run(state, now.format("%Y-%m-%d").to_string())
        .map_err(|error| error.to_string())?;
    notify_updated(app);
    log::info!("[Checkin] 每日签到已执行完成");
    Ok(true)
}

pub async fn refresh_clearance(app: &AppHandle, state: &AppState, id: &str) -> Result<(), String> {
    let _execution = state
        .checkin_runtime
        .execution
        .try_lock()
        .map_err(|_| "签到任务正在执行，请稍后重试".to_string())?;
    let site = load_browser_site(state, id)?;
    let browser_profile = profile::BrowserProfile::for_site(&site)?;
    if profile::login_window_open(app, &browser_profile)? {
        return Err("请先完成该条目的登录并关闭登录窗口，再重新过 Cloudflare 验证".into());
    }
    CheckinService::clear_clearance(state, id).map_err(|error| error.to_string())?;
    let outcome = acquire_clearance(app, &site, true).await?;
    persist_clearance(state, &site, &outcome)?;
    notify_updated(Some(app));
    Ok(())
}

fn load_site(state: &AppState, id: &str) -> Result<CheckinSite, String> {
    CheckinService::load(state)
        .map_err(|error| error.to_string())?
        .sites
        .into_iter()
        .find(|site| site.id == id)
        .ok_or_else(|| format!("站点不存在: {id}"))
}

fn load_browser_site(state: &AppState, id: &str) -> Result<CheckinSite, String> {
    let site = load_site(state, id)?;
    if site.auth_kind != CheckinAuthKind::Browser {
        return Err("该条目未使用浏览器认证方式".into());
    }
    Ok(site)
}

async fn execute_and_record(
    app: Option<&AppHandle>,
    state: &AppState,
    site: &CheckinSite,
) -> Result<CheckinResult, String> {
    let result = execute_site(app, state, site).await;
    CheckinService::record_result(state, &site.id, result.clone())
        .map_err(|error| error.to_string())?;
    notify_updated(app);
    Ok(result)
}

fn notify_updated(app: Option<&AppHandle>) {
    if let Some(app) = app {
        if let Err(error) = app.emit("checkin-updated", ()) {
            log::warn!("[Checkin] 推送签到状态失败: {error}");
        }
    }
}

fn persist_clearance(
    state: &AppState,
    site: &CheckinSite,
    outcome: &ClearanceOutcome,
) -> Result<(), String> {
    let stored = CheckinService::record_clearance_if_current(
        state,
        site,
        CheckinClearance {
            cookie: outcome.cookie.clone(),
            user_agent: outcome.user_agent.clone(),
            acquired_at: chrono::Utc::now().timestamp(),
        },
    )
    .map_err(|error| error.to_string())?;
    if !stored {
        return Err("签到条目已变更，未保存旧站点的验证凭证".into());
    }
    Ok(())
}

fn blocked(message: String) -> CheckinResult {
    CheckinResult {
        status: CheckinStatus::Blocked,
        at: chrono::Utc::now().timestamp(),
        http_status: None,
        message,
        needs_login: false,
    }
}

fn execution_error(message: String) -> CheckinResult {
    CheckinResult {
        status: CheckinStatus::Error,
        at: chrono::Utc::now().timestamp(),
        http_status: None,
        message,
        needs_login: false,
    }
}

fn login_in_progress() -> CheckinResult {
    CheckinResult {
        status: CheckinStatus::Failed,
        at: chrono::Utc::now().timestamp(),
        http_status: None,
        message: "该条目的登录窗口仍在打开，请完成登录并关闭窗口后重试签到".into(),
        needs_login: true,
    }
}

trait BrowserSessions: Sync {
    fn read_session(
        &self,
        site: &CheckinSite,
    ) -> impl std::future::Future<Output = Result<BrowserSession, String>> + Send;
    fn acquire(
        &self,
        site: &CheckinSite,
    ) -> impl std::future::Future<Output = Result<ClearanceOutcome, String>> + Send;
}

struct NativeBrowserSessions<'a>(&'a AppHandle);

impl BrowserSessions for NativeBrowserSessions<'_> {
    async fn read_session(&self, site: &CheckinSite) -> Result<BrowserSession, String> {
        profile::read_session(self.0, site).await
    }

    async fn acquire(&self, site: &CheckinSite) -> Result<ClearanceOutcome, String> {
        acquire_clearance(self.0, site, true).await
    }
}

async fn execute_site(
    app: Option<&AppHandle>,
    state: &AppState,
    site: &CheckinSite,
) -> CheckinResult {
    if site.auth_kind != CheckinAuthKind::Browser {
        return run_checkin_with(site, None).await;
    }
    let Some(app) = app else {
        return blocked("浏览器验证需要桌面应用窗口".into());
    };
    execute_browser_site(&NativeBrowserSessions(app), state, site).await
}

async fn execute_browser_site(
    browser: &impl BrowserSessions,
    state: &AppState,
    site: &CheckinSite,
) -> CheckinResult {
    let session = match browser.read_session(site).await {
        Ok(session) => session,
        Err(error) => return execution_error(error),
    };
    if session.status.login_window_open {
        return login_in_progress();
    }
    let cached = site
        .fresh_clearance(chrono::Utc::now().timestamp())
        .filter(|cached| cached.user_agent == CLEARANCE_USER_AGENT);
    let clearance = session
        .clearance_cookie
        .as_deref()
        .map(|cookie| Clearance {
            cookie,
            user_agent: CLEARANCE_USER_AGENT,
        })
        .or_else(|| {
            cached.map(|cached| Clearance {
                cookie: &cached.cookie,
                user_agent: &cached.user_agent,
            })
        });
    // Ordinary sites do not set cf_clearance. Try the scoped account session first;
    // only a real Blocked response should open a Cloudflare challenge window.
    let result = run_browser_checkin(site, clearance, &session.account_cookie).await;
    if result.status != CheckinStatus::Blocked {
        return result;
    }
    if let Err(error) = CheckinService::clear_clearance(state, &site.id) {
        log::warn!("[Checkin] 清除过期凭证失败: {error}");
    }
    let outcome = match browser.acquire(site).await {
        Ok(outcome) => outcome,
        Err(error) => return blocked(error),
    };
    log::info!(
        "[Checkin] 独立账号验证完成（用户交互: {}）",
        outcome.needed_interaction
    );
    if let Err(error) = persist_clearance(state, site, &outcome) {
        return execution_error(error);
    }
    // The user may have logged in while the verification page was open.
    let session = match browser.read_session(site).await {
        Ok(session) => session,
        Err(error) => return execution_error(error),
    };
    if session.status.login_window_open {
        return login_in_progress();
    }
    run_browser_checkin(
        site,
        Some(Clearance {
            cookie: &outcome.cookie,
            user_agent: &outcome.user_agent,
        }),
        &session.account_cookie,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Database;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    #[derive(Default)]
    struct TestBrowserSessions {
        acquisitions: AtomicUsize,
        reads: std::sync::Mutex<Vec<String>>,
        login_open: bool,
        fail_read: bool,
    }

    impl BrowserSessions for TestBrowserSessions {
        async fn read_session(&self, site: &CheckinSite) -> Result<BrowserSession, String> {
            self.reads.lock().unwrap().push(site.id.clone());
            if self.fail_read {
                return Err("mock cookie store unavailable".into());
            }
            let refreshed = self.acquisitions.load(Ordering::Relaxed) > 0;
            Ok(BrowserSession {
                account_cookie: format!(
                    "session={}{}",
                    site.id,
                    if refreshed { "-refreshed" } else { "" }
                ),
                clearance_cookie: refreshed.then(|| "cf_clearance=fresh".into()),
                status: CheckinBrowserSessionStatus {
                    account_cookie_count: 1,
                    login_window_open: self.login_open,
                },
            })
        }

        async fn acquire(&self, _site: &CheckinSite) -> Result<ClearanceOutcome, String> {
            self.acquisitions.fetch_add(1, Ordering::Relaxed);
            Ok(ClearanceOutcome {
                cookie: "cf_clearance=fresh".into(),
                user_agent: CLEARANCE_USER_AGENT.into(),
                needed_interaction: false,
            })
        }
    }

    fn browser_site(id: &str, url: &str) -> CheckinSite {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": id, "authKind": "browser",
            "request": {"url": url, "method": "GET"}
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn profile_b_executor_uses_each_entry_cookie_without_requiring_cloudflare() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let router = axum::Router::new().route(
            "/",
            axum::routing::get(|headers: axum::http::HeaderMap| async move {
                axum::Json(serde_json::json!({"message": headers["cookie"].to_str().unwrap()}))
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let state = AppState::new(Arc::new(Database::memory().unwrap()));
        let browser = TestBrowserSessions::default();
        for account in ["account-a", "account-b"] {
            let site = browser_site(account, &url);
            let result = execute_browser_site(&browser, &state, &site).await;
            assert_eq!(result.status, CheckinStatus::Success);
            assert_eq!(result.message, format!("session={account}"));
        }
        assert_eq!(browser.acquisitions.load(Ordering::Relaxed), 0);
        assert_eq!(*browser.reads.lock().unwrap(), ["account-a", "account-b"]);
        server.abort();
    }

    #[tokio::test]
    async fn profile_b_executor_rereads_the_same_profile_after_cloudflare() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let router = axum::Router::new().route(
            "/",
            axum::routing::get(|headers: axum::http::HeaderMap| async move {
                let cookies = headers["cookie"].to_str().unwrap();
                if !cookies.contains("cf_clearance=fresh") {
                    (
                        axum::http::StatusCode::FORBIDDEN,
                        "<title>Just a moment</title>".to_string(),
                    )
                } else {
                    (
                        axum::http::StatusCode::OK,
                        serde_json::json!({"message": cookies}).to_string(),
                    )
                }
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let state = AppState::new(Arc::new(Database::memory().unwrap()));
        let site = CheckinService::upsert_site(&state, browser_site("account-b", &url)).unwrap();
        let browser = TestBrowserSessions::default();
        let result = execute_browser_site(&browser, &state, &site).await;
        assert_eq!(result.status, CheckinStatus::Success);
        assert!(result.message.contains("session=account-b-refreshed"));
        assert!(result.message.contains("cf_clearance=fresh"));
        assert_eq!(browser.acquisitions.load(Ordering::Relaxed), 1);
        assert_eq!(*browser.reads.lock().unwrap(), ["account-b", "account-b"]);
        let saved = CheckinService::load(&state).unwrap();
        let cached = saved.sites[0]
            .browser
            .as_ref()
            .unwrap()
            .cached
            .as_ref()
            .unwrap();
        assert_eq!(cached.cookie, "cf_clearance=fresh");
        server.abort();
    }

    #[tokio::test]
    async fn profile_b_session_read_errors_and_open_login_windows_are_not_cf_blocks() {
        let state = AppState::new(Arc::new(Database::memory().unwrap()));
        let site = browser_site("account-b", "http://127.0.0.1:1/");
        let browser = TestBrowserSessions {
            fail_read: true,
            ..Default::default()
        };
        let result = execute_browser_site(&browser, &state, &site).await;
        assert_eq!(result.status, CheckinStatus::Error);
        assert!(!result.needs_login);
        let browser = TestBrowserSessions {
            login_open: true,
            ..Default::default()
        };
        let result = execute_browser_site(&browser, &state, &site).await;
        assert_eq!(result.status, CheckinStatus::Failed);
        assert!(result.needs_login);
        assert_eq!(browser.acquisitions.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn daily_checkin_runs_http_once_and_survives_runtime_restart() {
        let requests = Arc::new(AtomicUsize::new(0));
        let counter = requests.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = axum::Router::new().route(
            "/checkin",
            axum::routing::post(move || {
                counter.fetch_add(1, Ordering::Relaxed);
                async { axum::Json(serde_json::json!({"message":"签到成功"})) }
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let database = Arc::new(Database::memory().unwrap());
        let state = AppState::new(database.clone());
        let site: CheckinSite = serde_json::from_value(serde_json::json!({
            "id":"", "name":"本地测试", "authKind":"header",
            "request":{"url":format!("http://{address}/checkin"), "successContains":"签到成功"}
        }))
        .unwrap();
        CheckinService::upsert_site(&state, site).unwrap();
        CheckinService::set_schedule(&state, true, 0).unwrap();
        let now = Local::now();
        assert!(run_scheduled(None, &state, now).await.unwrap());
        assert!(!run_scheduled(None, &state, now).await.unwrap());
        let restarted = AppState::new(database);
        assert!(!run_scheduled(None, &restarted, now).await.unwrap());
        assert_eq!(requests.load(Ordering::Relaxed), 1);
        assert_eq!(
            CheckinService::load(&state).unwrap().sites[0]
                .last_result
                .as_ref()
                .unwrap()
                .status,
            CheckinStatus::Success
        );
        server.abort();
    }

    #[tokio::test]
    async fn scheduled_run_does_not_overlap_manual_execution() {
        let state = AppState::new(Arc::new(Database::memory().unwrap()));
        let _manual = state.checkin_runtime.execution.lock().await;
        assert!(!run_scheduled(None, &state, Local::now()).await.unwrap());
    }
}
