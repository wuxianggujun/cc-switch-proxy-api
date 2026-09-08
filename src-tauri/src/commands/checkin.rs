//! 公益站签到命令：参数接收与应用服务转发。

use crate::services::checkin::{
    executor, CheckinConfig, CheckinResult, CheckinService, CheckinSite,
};
use crate::store::AppState;
use tauri::{AppHandle, State};

#[tauri::command]
pub fn get_checkin_config(state: State<'_, AppState>) -> Result<CheckinConfig, String> {
    CheckinService::load(state.inner()).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn upsert_checkin_site(
    state: State<'_, AppState>,
    site: CheckinSite,
) -> Result<CheckinSite, String> {
    CheckinService::upsert_site(state.inner(), site).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn delete_checkin_site(state: State<'_, AppState>, id: String) -> Result<bool, String> {
    CheckinService::delete_site(state.inner(), &id).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn set_checkin_schedule(
    state: State<'_, AppState>,
    schedule_enabled: bool,
    schedule_hour: u8,
) -> Result<CheckinConfig, String> {
    CheckinService::set_schedule(state.inner(), schedule_enabled, schedule_hour)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn run_checkin_site(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<CheckinResult, String> {
    executor::run_site(&app, state.inner(), &id).await
}

#[tauri::command]
pub async fn run_all_checkin_sites(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<(String, CheckinResult)>, String> {
    executor::run_all(&app, state.inner()).await
}

#[tauri::command]
pub async fn refresh_checkin_clearance(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    executor::refresh_clearance(&app, state.inner(), &id).await
}
