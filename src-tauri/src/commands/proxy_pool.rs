//! 代理池 Tauri 命令
//!
//! 前端不直连节点凭据 —— ProxyNode 的 password 字段标了 skip_serializing，
//! 节点列表只下发地址与健康状态。

use crate::proxy_pool::service::RefreshOutcome;
use crate::proxy_pool::types::{
    Lease, NodeView, PoolConfig, PoolStats, Subscription, SubscriptionSource,
};
use crate::store::AppState;

#[tauri::command]
pub fn pp_list_subscriptions(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<Subscription>, String> {
    state
        .proxy_pool
        .list_subscriptions()
        .map_err(|e| e.to_string())
}

/// 新增订阅并立即刷新一次
#[tauri::command]
pub async fn pp_add_subscription(
    state: tauri::State<'_, AppState>,
    name: String,
    source: String,
    url: String,
    content: String,
    update_interval_secs: u64,
) -> Result<RefreshOutcome, String> {
    let source = match source.as_str() {
        "inline" => SubscriptionSource::Inline,
        "remote" => SubscriptionSource::Remote,
        other => return Err(format!("未知的订阅来源类型: {other}")),
    };
    state
        .proxy_pool
        .add_subscription(name, source, url, content, update_interval_secs)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn pp_update_subscription(
    state: tauri::State<'_, AppState>,
    subscription: Subscription,
) -> Result<(), String> {
    state
        .proxy_pool
        .update_subscription(subscription)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn pp_delete_subscription(
    state: tauri::State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    state
        .proxy_pool
        .delete_subscription(&id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn pp_refresh_subscription(
    state: tauri::State<'_, AppState>,
    id: String,
) -> Result<RefreshOutcome, String> {
    state
        .proxy_pool
        .refresh_subscription(&id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn pp_list_nodes(state: tauri::State<'_, AppState>) -> Result<Vec<NodeView>, String> {
    Ok(state.proxy_pool.node_views().await)
}

#[tauri::command]
pub async fn pp_get_stats(state: tauri::State<'_, AppState>) -> Result<PoolStats, String> {
    state.proxy_pool.stats().await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn pp_get_config(state: tauri::State<'_, AppState>) -> Result<PoolConfig, String> {
    Ok(state.proxy_pool.get_config().await)
}

#[tauri::command]
pub async fn pp_set_config(
    state: tauri::State<'_, AppState>,
    config: PoolConfig,
) -> Result<(), String> {
    state
        .proxy_pool
        .set_config(config)
        .await
        .map_err(|e| e.to_string())
}

/// 探测节点。node_hashes 为空表示探测全部。
#[tauri::command]
pub async fn pp_probe_nodes(
    state: tauri::State<'_, AppState>,
    node_hashes: Vec<String>,
) -> Result<usize, String> {
    state
        .proxy_pool
        .probe_now(node_hashes)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn pp_reset_circuit(
    state: tauri::State<'_, AppState>,
    node_hash: String,
) -> Result<bool, String> {
    state
        .proxy_pool
        .reset_circuit(&node_hash)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn pp_list_leases(state: tauri::State<'_, AppState>) -> Result<Vec<Lease>, String> {
    Ok(state.proxy_pool.leases().await)
}

#[tauri::command]
pub async fn pp_clear_lease(
    state: tauri::State<'_, AppState>,
    sticky_key: String,
) -> Result<bool, String> {
    state
        .proxy_pool
        .clear_lease(&sticky_key)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn pp_clear_all_leases(state: tauri::State<'_, AppState>) -> Result<usize, String> {
    state
        .proxy_pool
        .clear_all_leases()
        .await
        .map_err(|e| e.to_string())
}
