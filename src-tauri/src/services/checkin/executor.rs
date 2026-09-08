//! Shared application service for manual and scheduled check-ins.

use super::browser::{acquire_clearance, ClearanceOutcome};
use super::runner::{run_checkin_with, Clearance};
use super::scheduler::{schedule_is_due, succeeded_on_date};
use super::{
    CheckinAuthKind, CheckinClearance, CheckinResult, CheckinService, CheckinSite, CheckinStatus,
};
use crate::store::AppState;
use chrono::{DateTime, Local};
use tauri::{AppHandle, Emitter};

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
                execute_and_record(app, state, &site).await?;
            }
        }
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
    let site = load_site(state, id)?;
    if site.auth_kind != CheckinAuthKind::Browser {
        return Err("该站点未使用浏览器验证方式".into());
    }
    let outcome = acquire_clearance(app, site.resolve_challenge_url()).await?;
    persist_clearance(state, id, &outcome)?;
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

fn persist_clearance(state: &AppState, id: &str, outcome: &ClearanceOutcome) -> Result<(), String> {
    CheckinService::record_clearance(
        state,
        id,
        CheckinClearance {
            cookie: outcome.cookie.clone(),
            user_agent: outcome.user_agent.clone(),
            acquired_at: chrono::Utc::now().timestamp(),
        },
    )
    .map_err(|error| error.to_string())
}

fn blocked(message: String) -> CheckinResult {
    CheckinResult {
        status: CheckinStatus::Blocked,
        at: chrono::Utc::now().timestamp(),
        http_status: None,
        message,
    }
}

async fn acquire_and_store(
    app: &AppHandle,
    state: &AppState,
    site: &CheckinSite,
) -> Result<CheckinClearance, String> {
    let outcome = acquire_clearance(app, site.resolve_challenge_url()).await?;
    log::info!(
        "[Checkin] 站点验证完成（用户交互: {}）",
        outcome.needed_interaction
    );
    if let Err(error) = persist_clearance(state, &site.id, &outcome) {
        log::warn!("[Checkin] 保存验证凭证失败: {error}");
    }
    Ok(CheckinClearance {
        cookie: outcome.cookie,
        user_agent: outcome.user_agent,
        acquired_at: chrono::Utc::now().timestamp(),
    })
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
    let clearance = match site
        .fresh_clearance(chrono::Utc::now().timestamp())
        .cloned()
    {
        Some(cached) => cached,
        None => match acquire_and_store(app, state, site).await {
            Ok(clearance) => clearance,
            Err(error) => return blocked(error),
        },
    };
    let result = run_checkin_with(
        site,
        Some(Clearance {
            cookie: &clearance.cookie,
            user_agent: &clearance.user_agent,
        }),
    )
    .await;
    if result.status != CheckinStatus::Blocked {
        return result;
    }
    if let Err(error) = CheckinService::clear_clearance(state, &site.id) {
        log::warn!("[Checkin] 清除过期凭证失败: {error}");
    }
    match acquire_and_store(app, state, site).await {
        Ok(clearance) => {
            run_checkin_with(
                site,
                Some(Clearance {
                    cookie: &clearance.cookie,
                    user_agent: &clearance.user_agent,
                }),
            )
            .await
        }
        Err(error) => blocked(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Database;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

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
