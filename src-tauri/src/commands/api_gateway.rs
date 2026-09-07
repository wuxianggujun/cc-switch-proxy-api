//! API 网关命令层
//!
//! 暴露接入点（api_endpoints）与密钥（api_keys）的增删改查。
//! 与既有 provider 命令并行存在：provider 改写 CLI 配置文件，这里只服务本地网关选线。

use tauri::State;

use crate::database::{
    ApiEndpointRecord, ApiKeyRecord, NewApiEndpoint, NewApiKey, RouteCandidate, UpstreamType,
};
use crate::error::AppError;
use crate::store::AppState;

#[tauri::command]
pub async fn list_api_endpoints(
    state: State<'_, AppState>,
) -> Result<Vec<ApiEndpointRecord>, String> {
    state.db.list_api_endpoints().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn create_api_endpoint(
    state: State<'_, AppState>,
    endpoint: NewApiEndpoint,
) -> Result<String, String> {
    state.db.create_api_endpoint(&endpoint).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn update_api_endpoint(
    state: State<'_, AppState>,
    endpoint: ApiEndpointRecord,
) -> Result<(), String> {
    state.db.update_api_endpoint(&endpoint).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_api_endpoint(
    state: State<'_, AppState>,
    endpoint_id: String,
) -> Result<(), String> {
    state.db.delete_api_endpoint(&endpoint_id)
        .map_err(|e| e.to_string())
}

/// 拖拽排序：按传入顺序重写 sort_index。仅影响展示序，不影响路由优先级。
#[tauri::command]
pub async fn reorder_api_endpoints(
    state: State<'_, AppState>,
    endpoint_ids: Vec<String>,
) -> Result<(), String> {
    state.db.reorder_api_endpoints(&endpoint_ids)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn set_api_endpoint_enabled(
    state: State<'_, AppState>,
    endpoint_id: String,
    enabled: bool,
) -> Result<(), String> {
    state.db.set_api_endpoint_enabled(&endpoint_id, enabled)
        .map_err(|e| e.to_string())
}

// ── 密钥 ──────────────────────────────────────────────

#[tauri::command]
pub async fn list_api_keys(
    state: State<'_, AppState>,
    endpoint_id: String,
) -> Result<Vec<ApiKeyRecord>, String> {
    state.db.list_api_keys(&endpoint_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn create_api_key(
    state: State<'_, AppState>,
    key: NewApiKey,
) -> Result<String, String> {
    state.db.create_api_key(&key).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_api_key(
    state: State<'_, AppState>,
    key_id: String,
) -> Result<(), String> {
    state.db.delete_api_key(&key_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn set_api_key_enabled(
    state: State<'_, AppState>,
    key_id: String,
    enabled: bool,
) -> Result<(), String> {
    state.db.set_api_key_enabled(&key_id, enabled)
        .map_err(|e| e.to_string())
}

/// 手动清除冷却与硬状态，让密钥立刻重新参与选线。
#[tauri::command]
pub async fn clear_api_key_penalty(
    state: State<'_, AppState>,
    key_id: String,
) -> Result<(), String> {
    state.db.clear_api_key_penalty(&key_id).map_err(|e| e.to_string())
}

/// 预览选线序列：按 upstream_type + model 铺平候选，供 UI 展示当前生效顺序。
#[tauri::command]
pub async fn preview_route_candidates(
    state: State<'_, AppState>,
    upstream_type: String,
    model: Option<String>,
) -> Result<Vec<RouteCandidate>, String> {
    let upstream = UpstreamType::parse(&upstream_type)
        .ok_or_else(|| AppError::Config(format!("未知上游类型: {upstream_type}")).to_string())?;
    state.db.select_route_candidates(upstream, model.as_deref())
        .map_err(|e| e.to_string())
}
