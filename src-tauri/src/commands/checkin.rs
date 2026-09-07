//! 公益站签到命令

use crate::services::checkin::browser::{acquire_clearance, ClearanceOutcome};
use crate::services::checkin::runner::{run_checkin_with, Clearance};
use crate::services::checkin::{
    CheckinAuthKind, CheckinClearance, CheckinConfig, CheckinResult, CheckinService, CheckinSite,
    CheckinStatus,
};
use crate::store::AppState;
use tauri::{AppHandle, State};

#[tauri::command]
pub fn get_checkin_config(state: State<'_, AppState>) -> Result<CheckinConfig, String> {
    CheckinService::load(state.inner()).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn upsert_checkin_site(
    state: State<'_, AppState>,
    site: CheckinSite,
) -> Result<CheckinSite, String> {
    CheckinService::upsert_site(state.inner(), site).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_checkin_site(state: State<'_, AppState>, id: String) -> Result<bool, String> {
    CheckinService::delete_site(state.inner(), &id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn set_checkin_schedule(
    state: State<'_, AppState>,
    schedule_enabled: bool,
    schedule_hour: u8,
) -> Result<CheckinConfig, String> {
    CheckinService::set_schedule(state.inner(), schedule_enabled, schedule_hour)
        .map_err(|e| e.to_string())
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 执行单站签到并写回结果。
#[tauri::command]
pub async fn run_checkin_site(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<CheckinResult, String> {
    let site = load_site(state.inner(), &id)?;
    let result = execute_site(&app, state.inner(), &site).await;
    CheckinService::record_result(state.inner(), &id, result.clone()).map_err(|e| e.to_string())?;
    Ok(result)
}

/// 顺序执行所有启用的站点。
///
/// 故意不并发：17 个站同时打过去像脚本爆破，容易触发风控，
/// 而且逐个执行才能让前端边跑边看到进度。Browser 站点还会开 WebView，
/// 并发开窗口更是不可接受。
#[tauri::command]
pub async fn run_all_checkin_sites(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<(String, CheckinResult)>, String> {
    let sites: Vec<CheckinSite> = {
        let config = CheckinService::load(state.inner()).map_err(|e| e.to_string())?;
        config.sites.into_iter().filter(|s| s.enabled).collect()
    };

    let mut results = Vec::with_capacity(sites.len());
    for site in sites {
        let result = execute_site(&app, state.inner(), &site).await;
        CheckinService::record_result(state.inner(), &site.id, result.clone())
            .map_err(|e| e.to_string())?;
        results.push((site.id, result));
    }

    Ok(results)
}

/// 主动重新过闸并刷新缓存。用户在前端点「重新验证」时调用，
/// 不发签到请求，只更新凭证。
#[tauri::command]
pub async fn refresh_checkin_clearance(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    let site = load_site(state.inner(), &id)?;
    if site.auth_kind != CheckinAuthKind::Browser {
        return Err("该站点未使用浏览器验证方式".to_string());
    }
    let outcome = acquire_clearance(&app, site.resolve_challenge_url()).await?;
    log_clearance(&site, &outcome);
    persist_clearance(state.inner(), &id, &outcome)
}

fn load_site(state: &AppState, id: &str) -> Result<CheckinSite, String> {
    let config = CheckinService::load(state).map_err(|e| e.to_string())?;
    config
        .sites
        .into_iter()
        .find(|s| s.id == id)
        .ok_or_else(|| format!("站点不存在: {id}"))
}

/// 记录过闸是否需要用户介入。非交互式挑战应当全自动，一旦频繁需要交互，
/// 说明站点开了 Turnstile，值得让用户从日志里看出来。
fn log_clearance(site: &CheckinSite, outcome: &ClearanceOutcome) {
    if outcome.needed_interaction {
        log::info!("[Checkin] {} 过闸完成（需用户交互）", site.name);
    } else {
        log::info!("[Checkin] {} 自动过闸完成", site.name);
    }
}

fn persist_clearance(state: &AppState, id: &str, outcome: &ClearanceOutcome) -> Result<(), String> {
    CheckinService::record_clearance(
        state,
        id,
        CheckinClearance {
            cookie: outcome.cookie.clone(),
            user_agent: outcome.user_agent.clone(),
            acquired_at: now(),
        },
    )
    .map_err(|e| e.to_string())
}

/// 执行一个站点：Header/Login 直接发请求；Browser 先备好凭证，
/// 若仍被 CF 拦截则重新过闸并重试一次。
async fn execute_site(app: &AppHandle, state: &AppState, site: &CheckinSite) -> CheckinResult {
    if site.auth_kind != CheckinAuthKind::Browser {
        return run_checkin_with(site, None).await;
    }

    // 有未过期的凭证就先复用，避免每次签到都开窗口。
    let cached = site.fresh_clearance(now()).cloned();

    let clearance = match cached {
        Some(existing) => existing,
        None => match acquire_clearance(app, site.resolve_challenge_url()).await {
            Ok(outcome) => {
                log_clearance(site, &outcome);
                let record = CheckinClearance {
                    cookie: outcome.cookie.clone(),
                    user_agent: outcome.user_agent.clone(),
                    acquired_at: now(),
                };
                if let Err(e) = persist_clearance(state, &site.id, &outcome) {
                    // 凭证没存住不影响本次签到，只是下次要重新过闸。
                    log::warn!("[Checkin] 保存过闸凭证失败: {e}");
                }
                record
            }
            Err(message) => {
                return CheckinResult {
                    status: CheckinStatus::Blocked,
                    at: now(),
                    http_status: None,
                    message,
                };
            }
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

    // 走到这里说明手上的凭证已失效（TTL 内也可能被站点提前作废）。
    // 丢弃后重新过闸重试一次；只重试一次，避免无限循环。
    log::info!("[Checkin] {} 凭证失效，重新过闸后重试", site.name);
    if let Err(e) = CheckinService::clear_clearance(state, &site.id) {
        log::warn!("[Checkin] 清除过期凭证失败: {e}");
    }

    let outcome = match acquire_clearance(app, site.resolve_challenge_url()).await {
        Ok(outcome) => outcome,
        Err(message) => {
            return CheckinResult {
                status: CheckinStatus::Blocked,
                at: now(),
                http_status: None,
                message,
            };
        }
    };
    log_clearance(site, &outcome);
    if let Err(e) = persist_clearance(state, &site.id, &outcome) {
        log::warn!("[Checkin] 保存过闸凭证失败: {e}");
    }

    run_checkin_with(
        site,
        Some(Clearance {
            cookie: &outcome.cookie,
            user_agent: &outcome.user_agent,
        }),
    )
    .await
}
