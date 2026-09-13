use crate::{
    database::{RequestTraceConfig, RequestTraceDetail, RequestTraceFilters, RequestTracePage},
    error::AppError,
    store::AppState,
};
use tauri::State;

#[tauri::command]
pub async fn list_request_traces(
    state: State<'_, AppState>,
    filters: RequestTraceFilters,
    page: u32,
    page_size: u32,
) -> Result<RequestTracePage, AppError> {
    let db = state.db.clone();
    tokio::task::spawn_blocking(move || db.list_request_traces(&filters, page, page_size))
        .await
        .map_err(|e| AppError::Message(format!("读取请求日志失败: {e}")))?
}

#[tauri::command]
pub async fn get_request_trace(
    state: State<'_, AppState>,
    request_id: String,
) -> Result<Option<RequestTraceDetail>, AppError> {
    let db = state.db.clone();
    tokio::task::spawn_blocking(move || db.get_request_trace(&request_id))
        .await
        .map_err(|e| AppError::Message(format!("读取请求详情失败: {e}")))?
}

#[tauri::command]
pub async fn get_request_trace_config(
    state: State<'_, AppState>,
) -> Result<RequestTraceConfig, AppError> {
    let db = state.db.clone();
    tokio::task::spawn_blocking(move || db.get_request_trace_config())
        .await
        .map_err(|e| AppError::Message(format!("读取日志配置失败: {e}")))?
}

#[tauri::command]
pub async fn set_request_trace_config(
    state: State<'_, AppState>,
    config: RequestTraceConfig,
) -> Result<(), AppError> {
    let db = state.db.clone();
    tokio::task::spawn_blocking(move || db.set_request_trace_config(&config))
        .await
        .map_err(|e| AppError::Message(format!("保存日志配置失败: {e}")))?
}

#[tauri::command]
pub async fn clear_request_traces(state: State<'_, AppState>) -> Result<usize, AppError> {
    let db = state.db.clone();
    tokio::task::spawn_blocking(move || db.clear_request_traces())
        .await
        .map_err(|e| AppError::Message(format!("清理请求日志失败: {e}")))?
}
