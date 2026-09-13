//! API 网关 DAO
//!
//! 接入点与密钥的增删改查 + 选线查询。
//!
//! `api_keys.last_used_at` 持久化 LRU 顺序；请求选线与预占在连接锁内完成，
//! 避免秒级时间戳并列、在途请求尚未记账时重复选择首 key。预览不修改顺序。

use rusqlite::{named_params, params, Connection, OptionalExtension, Transaction};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

use super::api_gateway_types::{
    last4, ApiEndpointModelFetchConfig, ApiEndpointRecord, ApiKeyRecord, CreatedApiEndpoint,
    HardState, KeyMutationOutcome, NewApiEndpoint, NewApiEndpointWithKey, NewApiKey,
    NewApiKeyValue, RouteCandidate, UpdateApiEndpointInput, UpdatedApiEndpoint, UpstreamType,
};
use crate::database::{lock_conn, Database};
use crate::error::AppError;

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// A persisted millisecond ordering marker, strictly increasing even when several
/// requests arrive within the same clock tick or the system clock moves back.
fn next_route_timestamp(conn: &Connection) -> Result<i64, AppError> {
    let previous: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(last_used_at), 0) FROM api_keys",
            [],
            |row| row.get(0),
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    Ok(now.max(previous.saturating_add(1)))
}

pub(crate) fn validate_endpoint_url(raw: &str) -> Result<(), AppError> {
    let url =
        url::Url::parse(raw).map_err(|_| AppError::Config("接入点地址不是有效的 URL".into()))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(AppError::Config(
            "接入点必须使用无凭据、查询参数和片段的 HTTP(S) 基础地址".into(),
        ));
    }
    Ok(())
}

/// 生成 ID。时间戳纳秒 + 进程内计数器取 sha256 前缀，避免同秒碰撞。
fn new_id(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);

    let mut hasher = Sha256::new();
    hasher.update(nanos.to_string().as_bytes());
    hasher.update(b"|");
    hasher.update(seq.to_string().as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    format!("{prefix}_{hex}")
}

fn row_to_endpoint(row: &rusqlite::Row<'_>) -> rusqlite::Result<ApiEndpointRecord> {
    let upstream_raw: String = row.get("upstream_type")?;
    let models_raw: String = row.get("models")?;
    let enabled_int: i64 = row.get("enabled")?;

    Ok(ApiEndpointRecord {
        id: row.get("id")?,
        name: row.get("name")?,
        // 表有 CHECK 约束，解析失败只可能是人工改库，回落 openai 而非 panic
        upstream_type: UpstreamType::parse(&upstream_raw).unwrap_or(UpstreamType::Openai),
        base_url: row.get("base_url")?,
        models: serde_json::from_str(&models_raw).unwrap_or_default(),
        priority: row.get("priority")?,
        enabled: enabled_int != 0,
        sort_index: row.get("sort_index")?,
        notes: row.get("notes")?,
        created_at: row.get("created_at")?,
        key_count: row.get("key_count").unwrap_or(0),
        enabled_key_count: row.get("enabled_key_count").unwrap_or(0),
    })
}

fn row_to_key(row: &rusqlite::Row<'_>) -> rusqlite::Result<ApiKeyRecord> {
    let enabled_int: i64 = row.get("enabled")?;
    let hard_raw: Option<String> = row.get("hard_state")?;

    Ok(ApiKeyRecord {
        id: row.get("id")?,
        endpoint_id: row.get("endpoint_id")?,
        key_last4: row.get("key_last4")?,
        name: row.get("name")?,
        internal_priority: row.get("internal_priority")?,
        enabled: enabled_int != 0,
        last_used_at: row.get("last_used_at")?,
        cooldown_until: row.get("cooldown_until")?,
        cooldown_reason: row.get("cooldown_reason")?,
        hard_state: hard_raw.as_deref().and_then(HardState::parse),
        request_count: row.get("request_count")?,
        success_count: row.get("success_count")?,
        error_count: row.get("error_count")?,
        total_tokens: row.get("total_tokens")?,
        total_cost_usd: row.get("total_cost_usd")?,
        last_error_at: row.get("last_error_at")?,
        last_error_message: row.get("last_error_message")?,
        created_at: row.get("created_at")?,
    })
}

fn normalize_endpoint_url(raw: &str) -> Result<String, AppError> {
    let value = raw.trim();
    validate_endpoint_url(value)?;
    Ok(value.trim_end_matches('/').to_string())
}

fn validate_endpoint_input(input: &NewApiEndpoint) -> Result<(String, String, String), AppError> {
    let name = input.name.trim();
    if name.is_empty() {
        return Err(AppError::Config("接入点名称不能为空".into()));
    }
    let base_url = normalize_endpoint_url(&input.base_url)?;
    let models = serde_json::to_string(&input.models)
        .map_err(|e| AppError::Config(format!("模型列表序列化失败: {e}")))?;
    Ok((name.to_string(), base_url, models))
}

fn validate_key_value(input: &NewApiKeyValue) -> Result<&str, AppError> {
    let api_key = input.api_key.trim();
    if api_key.is_empty() {
        return Err(AppError::Config("密钥不能为空".into()));
    }
    Ok(api_key)
}

fn insert_endpoint(
    tx: &Transaction<'_>,
    input: &NewApiEndpoint,
    enabled: bool,
) -> Result<String, AppError> {
    let (name, base_url, models) = validate_endpoint_input(input)?;
    let id = new_id("ep");
    let next_sort: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(sort_index), -1) + 1 FROM api_endpoints",
            [],
            |row| row.get(0),
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
    tx.execute(
        "INSERT INTO api_endpoints
         (id, name, upstream_type, base_url, models, priority, enabled, sort_index, notes, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            id,
            name,
            input.upstream_type.as_str(),
            base_url,
            models,
            input.priority,
            i64::from(enabled),
            next_sort,
            input.notes.as_deref().map(str::trim).filter(|s| !s.is_empty()),
            now_secs(),
        ],
    )
    .map_err(|e| AppError::Database(e.to_string()))?;
    Ok(id)
}

fn insert_key(
    tx: &Transaction<'_>,
    endpoint_id: &str,
    input: &NewApiKeyValue,
) -> Result<String, AppError> {
    let api_key = validate_key_value(input)?;
    let id = new_id("key");
    tx.execute(
        "INSERT INTO api_keys
         (id, endpoint_id, api_key, key_last4, name, internal_priority, enabled, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7)",
        params![
            id,
            endpoint_id,
            api_key,
            last4(api_key),
            input
                .name
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty()),
            input.internal_priority,
            now_secs(),
        ],
    )
    .map_err(|e| AppError::Database(e.to_string()))?;
    Ok(id)
}

/// 无启用密钥时关闭接入点，并只在状态确实从启用变为禁用时返回 true。
fn disable_endpoint_without_enabled_key(
    tx: &Transaction<'_>,
    endpoint_id: &str,
) -> Result<bool, AppError> {
    let affected = tx
        .execute(
            "UPDATE api_endpoints
             SET enabled = 0
             WHERE id = ?1 AND enabled = 1
               AND NOT EXISTS (
                   SELECT 1 FROM api_keys
                   WHERE endpoint_id = ?1 AND enabled = 1
               )",
            [endpoint_id],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
    Ok(affected != 0)
}

impl Database {
    /// 列出接入点（按展示序），带密钥计数。
    pub fn list_api_endpoints(&self) -> Result<Vec<ApiEndpointRecord>, AppError> {
        let conn = lock_conn!(self.conn);

        let mut stmt = conn
            .prepare(
                "SELECT e.*,
                        (SELECT COUNT(*) FROM api_keys k WHERE k.endpoint_id = e.id) AS key_count,
                        (SELECT COUNT(*) FROM api_keys k
                         WHERE k.endpoint_id = e.id AND k.enabled = 1) AS enabled_key_count
                 FROM api_endpoints e
                 ORDER BY COALESCE(e.sort_index, 999999), e.created_at ASC",
            )
            .map_err(|e| AppError::Database(e.to_string()))?;

        let items = stmt
            .query_map([], row_to_endpoint)
            .map_err(|e| AppError::Database(e.to_string()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AppError::Database(e.to_string()))?;

        Ok(items)
    }

    /// 兼容旧 Rust 调用：只创建禁用的接入点，避免无密钥半成品参与路由。
    pub fn create_api_endpoint(&self, input: &NewApiEndpoint) -> Result<String, AppError> {
        validate_endpoint_input(input)?;
        let mut conn = lock_conn!(self.conn);
        let tx = conn
            .transaction()
            .map_err(|e| AppError::Database(e.to_string()))?;
        let id = insert_endpoint(&tx, input, false)?;
        tx.commit().map_err(|e| AppError::Database(e.to_string()))?;
        Ok(id)
    }

    /// 同一事务创建启用接入点与首把启用密钥。
    pub fn create_api_endpoint_with_key(
        &self,
        input: &NewApiEndpointWithKey,
    ) -> Result<CreatedApiEndpoint, AppError> {
        validate_endpoint_input(&input.endpoint)?;
        validate_key_value(&input.first_key)?;
        let mut conn = lock_conn!(self.conn);
        let tx = conn
            .transaction()
            .map_err(|e| AppError::Database(e.to_string()))?;
        let endpoint_id = insert_endpoint(&tx, &input.endpoint, true)?;
        let key_id = insert_key(&tx, &endpoint_id, &input.first_key)?;
        tx.commit().map_err(|e| AppError::Database(e.to_string()))?;
        Ok(CreatedApiEndpoint {
            endpoint_id,
            key_id,
        })
    }

    /// 原子更新接入点；目标改变时强制换绑并删除旧密钥。
    pub fn update_api_endpoint(
        &self,
        input: &UpdateApiEndpointInput,
    ) -> Result<UpdatedApiEndpoint, AppError> {
        let endpoint = NewApiEndpoint {
            name: input.name.clone(),
            upstream_type: input.upstream_type,
            base_url: input.base_url.clone(),
            models: input.models.clone(),
            priority: input.priority,
            notes: input.notes.clone(),
        };
        let (name, base_url, models) = validate_endpoint_input(&endpoint)?;
        if let Some(new_key) = &input.new_key {
            validate_key_value(new_key)?;
        }

        let mut conn = lock_conn!(self.conn);
        let tx = conn
            .transaction()
            .map_err(|e| AppError::Database(e.to_string()))?;
        let old: Option<(String, String)> = tx
            .query_row(
                "SELECT base_url, upstream_type FROM api_endpoints WHERE id = ?1",
                [&input.endpoint_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|e| AppError::Database(e.to_string()))?;
        let (old_base_url, old_upstream_type) =
            old.ok_or_else(|| AppError::Config(format!("接入点不存在: {}", input.endpoint_id)))?;
        let rebound = old_base_url != base_url || old_upstream_type != input.upstream_type.as_str();
        if rebound && input.new_key.is_none() {
            return Err(AppError::Config(
                "更换上游地址或协议时必须提供新密钥".into(),
            ));
        }

        tx.execute(
            "UPDATE api_endpoints
             SET name = ?1, upstream_type = ?2, base_url = ?3, models = ?4,
                 priority = ?5, notes = ?6
             WHERE id = ?7",
            params![
                name,
                input.upstream_type.as_str(),
                base_url,
                models,
                input.priority,
                input
                    .notes
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty()),
                input.endpoint_id,
            ],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;

        let key_id = if let Some(new_key) = &input.new_key {
            // 先插入，确保外键和凭据校验失败时旧绑定仍完整；目标改变后再删旧 key。
            let key_id = insert_key(&tx, &input.endpoint_id, new_key)?;
            if rebound {
                tx.execute(
                    "DELETE FROM api_keys WHERE endpoint_id = ?1 AND id <> ?2",
                    params![input.endpoint_id, key_id],
                )
                .map_err(|e| AppError::Database(e.to_string()))?;
            }
            Some(key_id)
        } else {
            None
        };

        tx.commit().map_err(|e| AppError::Database(e.to_string()))?;
        Ok(UpdatedApiEndpoint {
            endpoint_id: input.endpoint_id.clone(),
            key_id,
            rebound,
        })
    }

    /// 删除接入点。其下密钥由外键 ON DELETE CASCADE 一并清除。
    pub fn delete_api_endpoint(&self, endpoint_id: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute("DELETE FROM api_endpoints WHERE id = ?1", [endpoint_id])
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    /// 按给定顺序重写展示序。
    pub fn reorder_api_endpoints(&self, endpoint_ids: &[String]) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| AppError::Database(e.to_string()))?;

        for (index, id) in endpoint_ids.iter().enumerate() {
            tx.execute(
                "UPDATE api_endpoints SET sort_index = ?1 WHERE id = ?2",
                params![index as i64, id],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        }

        tx.commit().map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn set_api_endpoint_enabled(
        &self,
        endpoint_id: &str,
        enabled: bool,
    ) -> Result<(), AppError> {
        let mut conn = lock_conn!(self.conn);
        let tx = conn
            .transaction()
            .map_err(|e| AppError::Database(e.to_string()))?;
        let state: Option<(i64, bool)> = tx
            .query_row(
                "SELECT enabled,
                        EXISTS(SELECT 1 FROM api_keys k
                               WHERE k.endpoint_id = e.id AND k.enabled = 1)
                 FROM api_endpoints e WHERE e.id = ?1",
                [endpoint_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|e| AppError::Database(e.to_string()))?;
        let (_, has_enabled_key) =
            state.ok_or_else(|| AppError::Config(format!("接入点不存在: {endpoint_id}")))?;
        if enabled && !has_enabled_key {
            return Err(AppError::Config(
                "接入点至少需要一把已启用的密钥才能启用".into(),
            ));
        }
        tx.execute(
            "UPDATE api_endpoints SET enabled = ?1 WHERE id = ?2",
            params![i64::from(enabled), endpoint_id],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        tx.commit().map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    // ── 密钥 ──────────────────────────────────────────────

    pub fn list_api_keys(&self, endpoint_id: &str) -> Result<Vec<ApiKeyRecord>, AppError> {
        let conn = lock_conn!(self.conn);

        let mut stmt = conn
            .prepare(
                "SELECT * FROM api_keys WHERE endpoint_id = ?1
                 ORDER BY internal_priority ASC, created_at ASC",
            )
            .map_err(|e| AppError::Database(e.to_string()))?;

        let items = stmt
            .query_map([endpoint_id], row_to_key)
            .map_err(|e| AppError::Database(e.to_string()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AppError::Database(e.to_string()))?;

        // 兼容旧库中曾保存的短密钥展示值：绝不能把完整短密钥序列化给前端。
        let items = items
            .into_iter()
            .map(|mut item| {
                if item.key_last4.chars().count() < 4 {
                    item.key_last4 = "*".repeat(item.key_last4.chars().count());
                }
                item
            })
            .collect();

        Ok(items)
    }

    pub fn create_api_key(&self, input: &NewApiKey) -> Result<String, AppError> {
        let value = NewApiKeyValue {
            api_key: input.api_key.clone(),
            name: input.name.clone(),
            internal_priority: input.internal_priority,
        };
        validate_key_value(&value)?;

        let mut conn = lock_conn!(self.conn);
        let tx = conn
            .transaction()
            .map_err(|e| AppError::Database(e.to_string()))?;
        let endpoint_exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM api_endpoints WHERE id = ?1)",
                [&input.endpoint_id],
                |row| row.get(0),
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        if !endpoint_exists {
            return Err(AppError::Config(format!(
                "接入点不存在: {}",
                input.endpoint_id
            )));
        }
        let id = insert_key(&tx, &input.endpoint_id, &value)?;
        tx.commit().map_err(|e| AppError::Database(e.to_string()))?;
        Ok(id)
    }

    pub fn delete_api_key(&self, key_id: &str) -> Result<KeyMutationOutcome, AppError> {
        let mut conn = lock_conn!(self.conn);
        let tx = conn
            .transaction()
            .map_err(|e| AppError::Database(e.to_string()))?;
        let endpoint_id: Option<String> = tx
            .query_row(
                "SELECT endpoint_id FROM api_keys WHERE id = ?1",
                [key_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| AppError::Database(e.to_string()))?;
        let endpoint_id =
            endpoint_id.ok_or_else(|| AppError::Config(format!("密钥不存在: {key_id}")))?;
        tx.execute("DELETE FROM api_keys WHERE id = ?1", [key_id])
            .map_err(|e| AppError::Database(e.to_string()))?;
        let endpoint_auto_disabled = disable_endpoint_without_enabled_key(&tx, &endpoint_id)?;
        tx.commit().map_err(|e| AppError::Database(e.to_string()))?;
        Ok(KeyMutationOutcome {
            endpoint_id,
            endpoint_auto_disabled,
        })
    }

    pub fn set_api_key_enabled(
        &self,
        key_id: &str,
        enabled: bool,
    ) -> Result<KeyMutationOutcome, AppError> {
        let mut conn = lock_conn!(self.conn);
        let tx = conn
            .transaction()
            .map_err(|e| AppError::Database(e.to_string()))?;
        let endpoint_id: Option<String> = tx
            .query_row(
                "SELECT endpoint_id FROM api_keys WHERE id = ?1",
                [key_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| AppError::Database(e.to_string()))?;
        let endpoint_id =
            endpoint_id.ok_or_else(|| AppError::Config(format!("密钥不存在: {key_id}")))?;
        tx.execute(
            "UPDATE api_keys SET enabled = ?1 WHERE id = ?2",
            params![i64::from(enabled), key_id],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        let endpoint_auto_disabled = if enabled {
            false
        } else {
            disable_endpoint_without_enabled_key(&tx, &endpoint_id)?
        };
        tx.commit().map_err(|e| AppError::Database(e.to_string()))?;
        Ok(KeyMutationOutcome {
            endpoint_id,
            endpoint_auto_disabled,
        })
    }

    /// 获取已保存接入点的网络请求快照。数据库锁在返回前释放。
    pub fn get_api_endpoint_model_fetch_config(
        &self,
        endpoint_id: &str,
    ) -> Result<ApiEndpointModelFetchConfig, AppError> {
        let conn = lock_conn!(self.conn);
        let config: Option<(String, String, String)> = conn
            .query_row(
                "SELECT e.base_url, e.upstream_type, k.api_key
                 FROM api_endpoints e
                 JOIN api_keys k ON k.endpoint_id = e.id
                 WHERE e.id = ?1 AND k.enabled = 1
                 ORDER BY k.internal_priority ASC,
                          k.last_used_at IS NOT NULL ASC,
                          k.last_used_at ASC,
                          k.created_at ASC,
                          k.id ASC
                 LIMIT 1",
                [endpoint_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|e| AppError::Database(e.to_string()))?;
        let (base_url, upstream_raw, api_key) = config.ok_or_else(|| {
            AppError::Config("接入点不存在或没有已启用的密钥，无法获取模型".into())
        })?;
        let upstream_type = UpstreamType::parse(&upstream_raw)
            .ok_or_else(|| AppError::Config("接入点上游类型无效".into()))?;
        Ok(ApiEndpointModelFetchConfig {
            base_url,
            api_key,
            upstream_type,
        })
    }

    /// 清除冷却与硬状态，让密钥立刻重新参与选线。
    pub fn clear_api_key_penalty(&self, key_id: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "UPDATE api_keys
             SET cooldown_until = NULL, cooldown_reason = NULL, hard_state = NULL
             WHERE id = ?1",
            [key_id],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    // ── 选线 ──────────────────────────────────────────────

    /// 铺平候选序列。跨优先级一次性排好，调用方线性推进即可实现降级。
    ///
    /// 排序键的含义：
    /// 1. 同族优先 —— 跨族候选只追加在同族之后
    /// 2. `e.priority` —— 层级，越小越优先
    /// 3. `k.internal_priority` —— 层内序
    /// 4. `last_used_at IS NOT NULL ASC` —— 从未用过的排最前
    /// 5. `last_used_at ASC` —— 其余按最久未用（LRU 轮询）
    ///
    /// 序列是 per-request 构建的，游标只活在这一次请求内：降级纯临时、绝不粘滞，
    /// 高优先级恢复后下一个请求自动回到它。
    pub fn select_route_candidates(
        &self,
        upstream: UpstreamType,
        model: Option<&str>,
    ) -> Result<Vec<RouteCandidate>, AppError> {
        let conn = lock_conn!(self.conn);
        // 预览按单一上游类型展开，不跨协议族。
        query_route_candidates(&conn, &[upstream], model, false)
    }

    /// None: no enabled gateway is configured, so legacy routing is allowed.
    /// Some(empty): configured gateway has no eligible key/model; fail closed.
    /// Selection and reservation share the connection lock, before any network I/O.
    ///
    /// `upstreams` 是入口的自己协议族；`cross_family` 允许并入其它协议族里
    /// 显式白名单命中本次 model 的接入点。
    pub fn reserve_route_candidates(
        &self,
        upstreams: &[UpstreamType],
        model: Option<&str>,
        cross_family: bool,
    ) -> Result<Option<Vec<RouteCandidate>>, AppError> {
        let conn = lock_conn!(self.conn);
        let upstream_json =
            serde_json::to_string(upstreams).map_err(|e| AppError::Config(e.to_string()))?;
        // 同族只看"有没有启用的接入点"（与改造前一致，与 model 无关）：
        // 同族配了但 model 不匹配属于"配了却无线路"，必须 fail closed，不能回落老账号。
        // 跨族则必须白名单命中才算已配置，否则「只配了 Claude 接入点」会把所有入口
        // 都判成已配置，令既有用户在其它入口上失去老 provider 链的回落。
        let configured: bool = conn
            .query_row(
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM api_endpoints e WHERE e.enabled = 1
                     AND EXISTS(SELECT 1 FROM api_keys configured_key
                                WHERE configured_key.endpoint_id = e.id
                                  AND configured_key.enabled = 1)
                     AND (({own})
                          OR (:cross_family = 1 AND {compatible} AND NOT ({own}) AND :model IS NOT NULL
                              AND json_array_length(e.models) > 0 AND {hit})))",
                    own = OWN_FAMILY_SQL,
                    hit = MODEL_HIT_SQL,
                    compatible = CROSS_FAMILY_SQL,
                ),
                named_params! {
                    ":family": &upstream_json,
                    ":model": model,
                    ":cross_family": i64::from(cross_family),
                },
                |row| row.get(0),
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        if !configured {
            return Ok(None);
        }
        let candidates = query_route_candidates(&conn, upstreams, model, cross_family)?;
        if let Some(first) = candidates.first() {
            let timestamp = next_route_timestamp(&conn)?;
            conn.execute(
                "UPDATE api_keys SET last_used_at = ?1 WHERE id = ?2",
                params![timestamp, first.key_id],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        }
        Ok(Some(candidates))
    }

    /// 记账成功。写 last_used_at 同时完成轮询指针前移。
    pub fn record_api_key_success(
        &self,
        key_id: &str,
        tokens: i64,
        cost_usd: f64,
    ) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        let timestamp = next_route_timestamp(&conn)?;
        conn.execute(
            "UPDATE api_keys
             SET last_used_at = ?1,
                 request_count = request_count + 1,
                 success_count = success_count + 1,
                 total_tokens = total_tokens + ?2,
                 total_cost_usd = total_cost_usd + ?3
             WHERE id = ?4",
            params![timestamp, tokens, cost_usd, key_id],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    /// 记账失败。`cooldown_secs` 与 `hard_state` 互斥：前者到期自动恢复，
    /// 后者需人工或额度重置信号才清除。
    pub fn record_api_key_failure(
        &self,
        key_id: &str,
        message: &str,
        cooldown_secs: Option<i64>,
        hard_state: Option<HardState>,
    ) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        let now = now_secs();
        let timestamp = next_route_timestamp(&conn)?;

        conn.execute(
            "UPDATE api_keys
             SET last_used_at = ?1,
                 request_count = request_count + 1,
                 error_count = error_count + 1,
                 last_error_at = ?7,
                 last_error_message = ?2,
                 cooldown_until = CASE WHEN hard_state IS NOT NULL OR ?5 IS NOT NULL THEN NULL
                    WHEN ?3 IS NULL THEN cooldown_until ELSE MAX(COALESCE(cooldown_until, 0), ?3) END,
                 cooldown_reason = COALESCE(?4, cooldown_reason),
                 hard_state = COALESCE(hard_state, ?5)
             WHERE id = ?6",
            params![
                timestamp,
                message,
                cooldown_secs.map(|secs| now + secs),
                cooldown_secs.map(|_| message),
                hard_state.map(|s| s.as_str()),
                key_id,
                now,
            ],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }
}

/// 接入点属于入口的自己协议族。
const OWN_FAMILY_SQL: &str = "e.upstream_type IN (SELECT value FROM json_each(:family))";
// Gemini's URI model and native wire format have no Codex/Chat bridge.
const CROSS_FAMILY_SQL: &str = "e.upstream_type IN ('claude', 'openai', 'codex', 'deepseek')";

/// 模型命中接入点白名单。
///
/// 等值分支保证含 `[` / `?` / `*` 的真实模型 ID 能按字面命中；其余条目保留
/// SQLite GLOB 语义，以兼容用户显式配置的模型族模式（例如 `claude-*`）。
const MODEL_HIT_SQL: &str = "EXISTS (SELECT 1 FROM json_each(e.models)
     WHERE value = :model OR :model GLOB value)";

/// 接入点是否可服务本次请求。
///
/// 同族：与改造前完全一致——`models` 留空即不限模型。
/// 跨族：必须显式白名单命中。`models` 留空 = 只服务自己协议族，
/// 这样现有配置（清一色留空）的路由结果零变化。
fn endpoint_scope_sql() -> String {
    format!(
        "(({own} AND (:model IS NULL OR json_array_length(e.models) = 0 OR {hit}))
          OR (:cross_family = 1 AND {compatible} AND NOT ({own}) AND :model IS NOT NULL
              AND json_array_length(e.models) > 0 AND {hit}))",
        own = OWN_FAMILY_SQL,
        hit = MODEL_HIT_SQL,
        compatible = CROSS_FAMILY_SQL,
    )
}

/// `upstreams` 是入口的**自己协议族**；`cross_family` 决定是否允许并入其它协议族的
/// 显式白名单接入点。
fn query_route_candidates(
    conn: &Connection,
    upstreams: &[UpstreamType],
    model: Option<&str>,
    cross_family: bool,
) -> Result<Vec<RouteCandidate>, AppError> {
    let upstream_json =
        serde_json::to_string(upstreams).map_err(|e| AppError::Config(e.to_string()))?;
    // 同族优先于 e.priority：跨族候选只追加在同族之后，绝不插队。
    // 否则一个 priority=1 的跨族接入点会盖过 priority=100 的同族接入点，
    // 改变既有用户的路由结果。
    let sql = format!(
        "SELECT k.id AS key_id, k.api_key, k.key_last4, k.internal_priority, k.last_used_at,
                e.id AS endpoint_id, e.name AS endpoint_name, e.base_url, e.priority, e.upstream_type
         FROM api_keys k JOIN api_endpoints e ON k.endpoint_id = e.id
         WHERE e.enabled = 1 AND k.enabled = 1 AND k.hard_state IS NULL
           AND (k.cooldown_until IS NULL OR k.cooldown_until <= :now)
           AND {scope}
         ORDER BY ({own}) DESC, e.priority ASC, k.internal_priority ASC,
                  k.last_used_at IS NOT NULL ASC, k.last_used_at ASC, k.id ASC",
        scope = endpoint_scope_sql(),
        own = OWN_FAMILY_SQL,
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| AppError::Database(e.to_string()))?;
    let rows = stmt
        .query_map(
            named_params! {
                ":now": now_secs(),
                ":family": upstream_json,
                ":model": model,
                ":cross_family": i64::from(cross_family),
            },
            |row| {
                let raw: String = row.get("upstream_type")?;
                Ok(RouteCandidate {
                    key_id: row.get("key_id")?,
                    endpoint_id: row.get("endpoint_id")?,
                    endpoint_name: row.get("endpoint_name")?,
                    base_url: row.get("base_url")?,
                    upstream_type: UpstreamType::parse(&raw)
                        .ok_or(rusqlite::Error::InvalidQuery)?,
                    key_last4: row.get("key_last4")?,
                    priority: row.get("priority")?,
                    internal_priority: row.get("internal_priority")?,
                    last_used_at: row.get("last_used_at")?,
                    api_key: row.get("api_key")?,
                })
            },
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| AppError::Database(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::dao::api_gateway_types::HardState;

    fn endpoint(db: &Database, name: &str, upstream: UpstreamType, priority: i64) -> String {
        db.create_api_endpoint(&NewApiEndpoint {
            name: name.to_string(),
            upstream_type: upstream,
            base_url: format!("https://{name}.example.com"),
            models: vec![],
            priority,
            notes: None,
        })
        .unwrap()
    }

    fn endpoint_with_models(
        db: &Database,
        name: &str,
        upstream: UpstreamType,
        priority: i64,
        models: &[&str],
    ) -> String {
        db.create_api_endpoint(&NewApiEndpoint {
            name: name.to_string(),
            upstream_type: upstream,
            base_url: format!("https://{name}.example.com"),
            models: models.iter().map(|m| m.to_string()).collect(),
            priority,
            notes: None,
        })
        .unwrap()
    }

    /// 直接驱动选线 SQL，绕开 reserve 的预占副作用（预占会改 last_used_at，
    /// 影响后续断言的顺序）。
    fn routes(
        db: &Database,
        own_family: &[UpstreamType],
        model: Option<&str>,
        cross_family: bool,
    ) -> Vec<RouteCandidate> {
        let conn = db.conn.lock().unwrap();
        query_route_candidates(&conn, own_family, model, cross_family).unwrap()
    }

    fn route_keys(
        db: &Database,
        own_family: &[UpstreamType],
        model: Option<&str>,
        cross_family: bool,
    ) -> Vec<String> {
        routes(db, own_family, model, cross_family)
            .into_iter()
            .map(|c| c.key_id)
            .collect()
    }

    fn key(db: &Database, endpoint_id: &str, secret: &str, internal_priority: i64) -> String {
        let id = db
            .create_api_key(&NewApiKey {
                endpoint_id: endpoint_id.to_string(),
                api_key: secret.to_string(),
                name: None,
                internal_priority,
            })
            .unwrap();
        db.set_api_endpoint_enabled(endpoint_id, true).unwrap();
        id
    }

    fn endpoint_input(name: &str, upstream_type: UpstreamType, base_url: &str) -> NewApiEndpoint {
        NewApiEndpoint {
            name: name.to_string(),
            upstream_type,
            base_url: base_url.to_string(),
            models: vec![],
            priority: 100,
            notes: None,
        }
    }

    fn key_value(secret: &str) -> NewApiKeyValue {
        NewApiKeyValue {
            api_key: secret.to_string(),
            name: None,
            internal_priority: 50,
        }
    }

    /// 直接改列，模拟时间状态，避免测试依赖真实时间流逝。
    /// 返回 Result 是因为 `lock_conn!` 内部用了 `?`。
    fn patch_key_column(db: &Database, sql: &str, key_id: &str) -> Result<(), AppError> {
        let conn = lock_conn!(db.conn);
        conn.execute(sql, params![key_id])
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    fn set_last_used(db: &Database, key_id: &str, at: i64) {
        patch_key_column(
            db,
            &format!("UPDATE api_keys SET last_used_at = {at} WHERE id = ?1"),
            key_id,
        )
        .unwrap();
    }

    #[test]
    fn endpoint_and_key_lifecycle_preserves_enabled_key_invariant() {
        let db = Database::memory().unwrap();
        let created = db
            .create_api_endpoint_with_key(&NewApiEndpointWithKey {
                endpoint: endpoint_input(
                    "atomic",
                    UpstreamType::Claude,
                    "https://atomic.example.com",
                ),
                first_key: key_value("sk-first"),
            })
            .unwrap();
        let listed = db.list_api_endpoints().unwrap();
        assert_eq!(listed[0].id, created.endpoint_id);
        assert!(listed[0].enabled);
        assert_eq!(listed[0].key_count, 1);
        assert_eq!(listed[0].enabled_key_count, 1);

        let disabled = db.set_api_key_enabled(&created.key_id, false).unwrap();
        assert!(disabled.endpoint_auto_disabled);
        assert!(!db.list_api_endpoints().unwrap()[0].enabled);
        assert!(
            !db.set_api_key_enabled(&created.key_id, true)
                .unwrap()
                .endpoint_auto_disabled
        );
        assert!(!db.list_api_endpoints().unwrap()[0].enabled);
        db.set_api_endpoint_enabled(&created.endpoint_id, true)
            .unwrap();
        assert!(
            db.delete_api_key(&created.key_id)
                .unwrap()
                .endpoint_auto_disabled
        );
        assert!(!db.list_api_endpoints().unwrap()[0].enabled);
    }

    #[test]
    fn endpoint_rebind_requires_new_key_and_replaces_old_keys_atomically() {
        let db = Database::memory().unwrap();
        let created = db
            .create_api_endpoint_with_key(&NewApiEndpointWithKey {
                endpoint: endpoint_input(
                    "before",
                    UpstreamType::Claude,
                    "https://before.example.com",
                ),
                first_key: key_value("sk-old"),
            })
            .unwrap();
        key(&db, &created.endpoint_id, "sk-old-backup", 80);

        let missing_key = UpdateApiEndpointInput {
            endpoint_id: created.endpoint_id.clone(),
            name: "after".to_string(),
            upstream_type: UpstreamType::Openai,
            base_url: "https://after.example.com".to_string(),
            models: vec!["new-model".to_string()],
            priority: 20,
            notes: None,
            new_key: None,
        };
        assert!(db.update_api_endpoint(&missing_key).is_err());
        let unchanged = db.list_api_endpoints().unwrap();
        assert_eq!(unchanged[0].name, "before");
        assert_eq!(unchanged[0].key_count, 2);

        let updated = db
            .update_api_endpoint(&UpdateApiEndpointInput {
                new_key: Some(key_value("sk-new")),
                ..missing_key
            })
            .unwrap();
        assert!(updated.rebound);
        let keys = db.list_api_keys(&created.endpoint_id).unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].id, updated.key_id.unwrap());
        assert_eq!(keys[0].key_last4, "-new");
        let endpoint = &db.list_api_endpoints().unwrap()[0];
        assert_eq!(endpoint.name, "after");
        assert_eq!(endpoint.upstream_type, UpstreamType::Openai);
    }

    #[test]
    fn readiness_fast_requests_rotate_without_timestamp_ties() {
        let db = Database::memory().unwrap();
        let ep = endpoint(&db, "rotate", UpstreamType::Claude, 100);
        key(&db, &ep, "sk-first", 50);
        key(&db, &ep, "sk-second", 50);
        let mut previous = None;
        for _ in 0..12 {
            let selected = db
                .select_route_candidates(UpstreamType::Claude, None)
                .unwrap();
            let current = selected[0].key_id.clone();
            assert_ne!(previous.as_ref(), Some(&current));
            db.record_api_key_success(&current, 0, 0.0).unwrap();
            previous = Some(current);
        }
    }

    #[test]
    fn reservation_rotates_before_requests_finish_but_preview_is_read_only() {
        let db = Database::memory().unwrap();
        let ep = endpoint(&db, "reserve", UpstreamType::Claude, 100);
        key(&db, &ep, "sk-a", 50);
        key(&db, &ep, "sk-b", 50);
        let preview = db
            .select_route_candidates(UpstreamType::Claude, None)
            .unwrap();
        let again = db
            .select_route_candidates(UpstreamType::Claude, None)
            .unwrap();
        assert_eq!(preview[0].key_id, again[0].key_id);
        let first = db
            .reserve_route_candidates(&[UpstreamType::Claude], Some("test"), false)
            .unwrap()
            .unwrap();
        let second = db
            .reserve_route_candidates(&[UpstreamType::Claude], Some("test"), false)
            .unwrap()
            .unwrap();
        assert_ne!(first[0].key_id, second[0].key_id);
        assert!(db
            .list_api_keys(&ep)
            .unwrap()
            .iter()
            .all(|key| key.request_count == 0));
    }

    #[test]
    fn late_success_does_not_undo_a_concurrent_cooldown() {
        let db = Database::memory().unwrap();
        let ep = endpoint(&db, "cooldown", UpstreamType::Claude, 100);
        let id = key(&db, &ep, "sk-a", 50);
        db.record_api_key_failure(&id, "rate limit", Some(60), None)
            .unwrap();
        db.record_api_key_success(&id, 0, 0.0).unwrap();
        assert!(db
            .select_route_candidates(UpstreamType::Claude, None)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn gateway_reservation_distinguishes_unconfigured_from_unavailable() {
        let db = Database::memory().unwrap();
        assert!(db
            .reserve_route_candidates(&[UpstreamType::Claude], None, false)
            .unwrap()
            .is_none());
        let ep = endpoint(&db, "empty", UpstreamType::Claude, 100);
        assert!(db
            .reserve_route_candidates(&[UpstreamType::Claude], None, false)
            .unwrap()
            .is_none());
        assert!(db.set_api_endpoint_enabled(&ep, true).is_err());
        db.set_api_endpoint_enabled(&ep, false).unwrap();
        assert!(db
            .reserve_route_candidates(&[UpstreamType::Claude], None, false)
            .unwrap()
            .is_none());
    }

    #[test]
    fn readiness_transient_failure_cannot_clear_hard_penalty() {
        let db = Database::memory().unwrap();
        let ep = endpoint(&db, "penalty", UpstreamType::Claude, 100);
        let id = key(&db, &ep, "sk-test", 50);
        db.record_api_key_failure(&id, "invalid", None, Some(HardState::AuthInvalid))
            .unwrap();
        db.record_api_key_failure(&id, "late timeout", Some(60), None)
            .unwrap();
        assert_eq!(
            db.list_api_keys(&ep).unwrap()[0].hard_state,
            Some(HardState::AuthInvalid)
        );
    }

    #[test]
    fn tier_priority_outranks_lru() {
        let db = Database::memory().unwrap();
        let low = endpoint(&db, "low", UpstreamType::Claude, 10);
        let high = endpoint(&db, "high", UpstreamType::Claude, 200);

        // 让高优先级那条"刚用过"、低优先级那条"很久没用"。
        // 若 LRU 盖过层级，顺序就会反过来——这正是要防的。
        let low_key = key(&db, &low, "sk-low", 50);
        let high_key = key(&db, &high, "sk-high", 50);
        set_last_used(&db, &low_key, 1000);
        set_last_used(&db, &high_key, 1);

        let candidates = db
            .select_route_candidates(UpstreamType::Claude, None)
            .unwrap();
        let order: Vec<&str> = candidates.iter().map(|c| c.key_id.as_str()).collect();
        assert_eq!(order, vec![low_key.as_str(), high_key.as_str()]);
    }

    #[test]
    fn never_used_key_goes_first_within_tier() {
        let db = Database::memory().unwrap();
        let ep = endpoint(&db, "ep", UpstreamType::Claude, 100);
        let used = key(&db, &ep, "sk-used", 50);
        let fresh = key(&db, &ep, "sk-fresh", 50);
        set_last_used(&db, &used, 500);
        // fresh 保持 last_used_at = NULL

        let candidates = db
            .select_route_candidates(UpstreamType::Claude, None)
            .unwrap();
        assert_eq!(candidates[0].key_id, fresh);
        assert_eq!(candidates[1].key_id, used);
    }

    #[test]
    fn same_tier_rotates_least_recently_used() {
        let db = Database::memory().unwrap();
        let ep = endpoint(&db, "ep", UpstreamType::Claude, 100);
        let a = key(&db, &ep, "sk-a", 50);
        let b = key(&db, &ep, "sk-b", 50);
        let c = key(&db, &ep, "sk-c", 50);
        set_last_used(&db, &a, 300);
        set_last_used(&db, &b, 100);
        set_last_used(&db, &c, 200);

        let order: Vec<String> = db
            .select_route_candidates(UpstreamType::Claude, None)
            .unwrap()
            .into_iter()
            .map(|c| c.key_id)
            .collect();
        assert_eq!(order, vec![b.clone(), c.clone(), a.clone()]);

        // 记账成功后 b 变成最近使用，下一轮应轮到 c
        db.record_api_key_success(&b, 100, 0.01).unwrap();
        let next = db
            .select_route_candidates(UpstreamType::Claude, None)
            .unwrap();
        assert_eq!(next[0].key_id, c);
        assert_eq!(next.last().unwrap().key_id, b);
    }

    #[test]
    fn internal_priority_splits_within_endpoint() {
        let db = Database::memory().unwrap();
        let ep = endpoint(&db, "ep", UpstreamType::Claude, 100);
        let backup = key(&db, &ep, "sk-backup", 90);
        let primary = key(&db, &ep, "sk-primary", 10);
        // backup 从未使用，primary 用过——层内优先级仍应压过 LRU
        set_last_used(&db, &primary, 999);

        let candidates = db
            .select_route_candidates(UpstreamType::Claude, None)
            .unwrap();
        assert_eq!(candidates[0].key_id, primary);
        assert_eq!(candidates[1].key_id, backup);
    }

    #[test]
    fn cooldown_and_hard_state_leave_candidate_set() {
        let db = Database::memory().unwrap();
        let ep = endpoint(&db, "ep", UpstreamType::Claude, 100);
        let healthy = key(&db, &ep, "sk-ok", 50);
        let cooling = key(&db, &ep, "sk-cool", 50);
        let exhausted = key(&db, &ep, "sk-dead", 50);

        db.record_api_key_failure(&cooling, "rate limited", Some(600), None)
            .unwrap();
        db.record_api_key_failure(&exhausted, "quota", None, Some(HardState::QuotaExhausted))
            .unwrap();

        let ids: Vec<String> = db
            .select_route_candidates(UpstreamType::Claude, None)
            .unwrap()
            .into_iter()
            .map(|c| c.key_id)
            .collect();
        assert_eq!(ids, vec![healthy.clone()]);

        // 冷却到期后自动回归，无需人工干预
        set_expired_cooldown(&db, &cooling);
        let after: Vec<String> = db
            .select_route_candidates(UpstreamType::Claude, None)
            .unwrap()
            .into_iter()
            .map(|c| c.key_id)
            .collect();
        assert!(after.contains(&cooling));
        // 硬状态不会自动恢复
        assert!(!after.contains(&exhausted));

        // 人工清除后硬状态项回归
        db.clear_api_key_penalty(&exhausted).unwrap();
        let cleared: Vec<String> = db
            .select_route_candidates(UpstreamType::Claude, None)
            .unwrap()
            .into_iter()
            .map(|c| c.key_id)
            .collect();
        assert!(cleared.contains(&exhausted));
    }

    fn set_expired_cooldown(db: &Database, key_id: &str) {
        patch_key_column(
            db,
            "UPDATE api_keys SET cooldown_until = 1 WHERE id = ?1",
            key_id,
        )
        .unwrap();
    }

    #[test]
    fn disabled_endpoint_or_key_is_excluded() {
        let db = Database::memory().unwrap();
        let live = endpoint(&db, "live", UpstreamType::Claude, 100);
        let dark = endpoint(&db, "dark", UpstreamType::Claude, 50);
        let live_key = key(&db, &live, "sk-live", 50);
        let dark_key = key(&db, &dark, "sk-dark", 50);
        let off_key = key(&db, &live, "sk-off", 50);

        db.set_api_endpoint_enabled(&dark, false).unwrap();
        db.set_api_key_enabled(&off_key, false).unwrap();

        let ids: Vec<String> = db
            .select_route_candidates(UpstreamType::Claude, None)
            .unwrap()
            .into_iter()
            .map(|c| c.key_id)
            .collect();
        assert_eq!(ids, vec![live_key]);
        assert!(!ids.contains(&dark_key));
    }

    #[test]
    fn upstream_and_model_scope_the_candidates() {
        let db = Database::memory().unwrap();
        let claude = endpoint(&db, "claude", UpstreamType::Claude, 100);
        let openai = endpoint(&db, "openai", UpstreamType::Openai, 100);
        let claude_key = key(&db, &claude, "sk-c", 50);
        key(&db, &openai, "sk-o", 50);

        // 上游类型隔离
        let ids: Vec<String> = db
            .select_route_candidates(UpstreamType::Claude, None)
            .unwrap()
            .into_iter()
            .map(|c| c.key_id)
            .collect();
        assert_eq!(ids, vec![claude_key.clone()]);

        // 限定模型的接入点只在 model 命中时参与
        let scoped = db
            .create_api_endpoint(&NewApiEndpoint {
                name: "scoped".to_string(),
                upstream_type: UpstreamType::Claude,
                base_url: "https://scoped.example.com".to_string(),
                models: vec!["claude-sonnet-5".to_string()],
                priority: 1,
                notes: None,
            })
            .unwrap();
        let scoped_key = key(&db, &scoped, "sk-s", 50);

        let hit: Vec<String> = db
            .select_route_candidates(UpstreamType::Claude, Some("claude-sonnet-5"))
            .unwrap()
            .into_iter()
            .map(|c| c.key_id)
            .collect();
        // priority=1 让它排在不限模型的那条前面
        assert_eq!(hit.first(), Some(&scoped_key));

        let miss: Vec<String> = db
            .select_route_candidates(UpstreamType::Claude, Some("gpt-5"))
            .unwrap()
            .into_iter()
            .map(|c| c.key_id)
            .collect();
        assert!(!miss.contains(&scoped_key));
        // 不限模型的那条仍然可选
        assert!(miss.contains(&claude_key));
    }

    #[test]
    fn deleting_endpoint_cascades_to_keys() {
        let db = Database::memory().unwrap();
        let ep = endpoint(&db, "ep", UpstreamType::Claude, 100);
        key(&db, &ep, "sk-1", 50);
        key(&db, &ep, "sk-2", 50);
        assert_eq!(db.list_api_keys(&ep).unwrap().len(), 2);

        db.delete_api_endpoint(&ep).unwrap();
        assert!(db
            .select_route_candidates(UpstreamType::Claude, None)
            .unwrap()
            .is_empty());
        assert!(db.list_api_keys(&ep).unwrap().is_empty());
    }

    #[test]
    fn short_plaintext_key_is_fully_redacted_in_key_records() {
        let db = Database::memory().unwrap();
        let ep = endpoint(&db, "ep", UpstreamType::Claude, 100);
        let id = key(&db, &ep, "abc", 50);

        let records = db.list_api_keys(&ep).unwrap();
        assert_eq!(records[0].key_last4, "***");

        // 历史版本可能已经把短 key 全量写入 key_last4；读取边界仍须兜底。
        {
            let conn = db.conn.lock().unwrap();
            conn.execute("UPDATE api_keys SET key_last4 = 'abc' WHERE id = ?1", [&id])
                .unwrap();
        }
        assert_eq!(db.list_api_keys(&ep).unwrap()[0].key_last4, "***");
    }

    #[test]
    fn plaintext_key_never_reaches_key_records() {
        let db = Database::memory().unwrap();
        let ep = endpoint(&db, "ep", UpstreamType::Claude, 100);
        key(&db, &ep, "sk-supersecret-tail", 50);

        let records = db.list_api_keys(&ep).unwrap();
        assert_eq!(records[0].key_last4, "tail");

        // 选线候选内部才带明文，供转发使用
        let candidates = db
            .select_route_candidates(UpstreamType::Claude, None)
            .unwrap();
        assert_eq!(candidates[0].api_key, "sk-supersecret-tail");
        // 但序列化时明文被跳过
        let json = serde_json::to_string(&candidates[0]).unwrap();
        assert!(!json.contains("supersecret"));
        assert!(json.contains("tail"));
    }

    // ── 跨协议族选线 ──────────────────────────────────────

    /// `models` 留空 = 只服务自己协议族。这是兼容边界：现有配置清一色留空，
    /// 若留空也跨族参与，所有既有用户的路由结果都会变。
    #[test]
    fn unrestricted_cross_family_endpoint_never_joins_another_entry() {
        let db = Database::memory().unwrap();
        let openai = endpoint(&db, "openai", UpstreamType::Openai, 100);
        let cross = key(&db, &openai, "sk-o", 50);

        for model in [None, Some("gpt-6"), Some("claude-sonnet-4")] {
            assert!(
                route_keys(&db, &[UpstreamType::Claude], model, true).is_empty(),
                "models 留空的 openai 接入点不该进入 Claude 入口候选 (model={model:?})"
            );
        }
        // 自己入口仍然可用
        assert_eq!(
            route_keys(&db, &[UpstreamType::Openai], Some("gpt-6"), true),
            vec![cross]
        );
    }

    #[test]
    fn explicit_whitelist_lets_cross_family_endpoint_serve_the_entry() {
        let db = Database::memory().unwrap();
        let openai = endpoint_with_models(
            &db,
            "openai",
            UpstreamType::Openai,
            100,
            &["claude-sonnet-4"],
        );
        let cross = key(&db, &openai, "sk-o", 50);

        let hit = routes(&db, &[UpstreamType::Claude], Some("claude-sonnet-4"), true);
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].key_id, cross);
        // 上游类型如实带出，转发链据此选协议转换器
        assert_eq!(hit[0].upstream_type, UpstreamType::Openai);

        // 白名单外的 model 不命中
        assert!(route_keys(&db, &[UpstreamType::Claude], Some("gpt-6"), true).is_empty());
        // cross_family 关闭时也不命中
        assert!(
            route_keys(&db, &[UpstreamType::Claude], Some("claude-sonnet-4"), false).is_empty()
        );
    }

    /// 同族必须优先于 e.priority，否则一个 priority=1 的跨族接入点会插到
    /// priority=100 的同族接入点前面，改变既有用户的路由结果。
    #[test]
    fn own_family_outranks_cross_family_priority() {
        let db = Database::memory().unwrap();
        let own = endpoint(&db, "own", UpstreamType::Claude, 100);
        let own_key = key(&db, &own, "sk-own", 50);
        let cross_ep =
            endpoint_with_models(&db, "cross", UpstreamType::Openai, 1, &["claude-sonnet-4"]);
        let cross_key = key(&db, &cross_ep, "sk-cross", 50);
        // 再给跨族一个"从未使用"的优势，同族那条已经用过
        set_last_used(&db, &own_key, 9999);

        assert_eq!(
            route_keys(&db, &[UpstreamType::Claude], Some("claude-sonnet-4"), true),
            vec![own_key, cross_key]
        );
    }

    #[test]
    fn glob_patterns_match_model_families() {
        let db = Database::memory().unwrap();
        let cross = endpoint_with_models(&db, "cross", UpstreamType::Openai, 100, &["claude-*"]);
        let cross_key = key(&db, &cross, "sk-cross", 50);

        assert_eq!(
            route_keys(&db, &[UpstreamType::Claude], Some("claude-sonnet-4"), true),
            vec![cross_key.clone()]
        );
        assert!(route_keys(&db, &[UpstreamType::Claude], Some("gpt-6"), true).is_empty());

        let own = endpoint_with_models(&db, "own", UpstreamType::Claude, 100, &["claude-3.*"]);
        let own_key = key(&db, &own, "sk-own", 50);
        assert_eq!(
            route_keys(&db, &[UpstreamType::Claude], Some("claude-3.7"), true),
            vec![own_key, cross_key]
        );
    }

    /// SQLite GLOB 把 `[` / `?` / `*` 当元字符，所以等值分支必须保留，
    /// 否则含元字符的模型名字面量会漏配。
    #[test]
    fn literal_model_names_with_glob_metacharacters_still_match() {
        let db = Database::memory().unwrap();
        let own = endpoint_with_models(
            &db,
            "own",
            UpstreamType::Claude,
            100,
            &["gpt-4[preview]", "o1?mini", "star*model"],
        );
        let own_key = key(&db, &own, "sk-own", 50);
        let cross =
            endpoint_with_models(&db, "cross", UpstreamType::Openai, 100, &["gpt-4[preview]"]);
        let cross_key = key(&db, &cross, "sk-cross", 50);

        for model in ["gpt-4[preview]", "o1?mini", "star*model"] {
            assert_eq!(
                route_keys(&db, &[UpstreamType::Claude], Some(model), true)
                    .first()
                    .map(String::as_str),
                Some(own_key.as_str()),
                "等值匹配应命中 {model}"
            );
        }
        assert_eq!(
            route_keys(&db, &[UpstreamType::Claude], Some("gpt-4[preview]"), true),
            vec![own_key.clone(), cross_key]
        );
        assert_eq!(
            route_keys(&db, &[UpstreamType::Claude], Some("o1xmini"), true),
            vec![own_key]
        );
        assert!(route_keys(&db, &[UpstreamType::Claude], Some("gemini-2"), true).is_empty());
    }

    /// catalog/metadata 请求没有 model 可判定，跨族不能凭空并入。
    #[test]
    fn catalog_requests_without_model_exclude_cross_family() {
        let db = Database::memory().unwrap();
        let own = endpoint(&db, "own", UpstreamType::Claude, 100);
        let own_key = key(&db, &own, "sk-own", 50);
        let cross = endpoint_with_models(&db, "cross", UpstreamType::Openai, 1, &["claude-*"]);
        key(&db, &cross, "sk-cross", 50);

        assert_eq!(
            route_keys(&db, &[UpstreamType::Claude], None, true),
            vec![own_key]
        );
    }

    /// 三态语义是安全边界：None 才允许回落老 provider 链，Some(empty) 必须报错。
    #[test]
    fn cross_family_reservation_keeps_the_three_state_contract() {
        let db = Database::memory().unwrap();
        let entry = &[UpstreamType::Claude];

        // 什么都没配 → None，允许回落
        assert!(db
            .reserve_route_candidates(entry, Some("claude-sonnet-4"), true)
            .unwrap()
            .is_none());

        // 只配了跨族且白名单命中 → 已配置，且必须真的选出线路，
        // 否则「只配 openai 接入点、请求从 claude 入口进来」会被误判成未配置。
        let cross = endpoint_with_models(&db, "cross", UpstreamType::Openai, 100, &["claude-*"]);
        let cross_key = key(&db, &cross, "sk-cross", 50);
        let reserved = db
            .reserve_route_candidates(entry, Some("claude-sonnet-4"), true)
            .unwrap()
            .expect("跨族白名单命中应视为已配置网关");
        assert_eq!(
            reserved.iter().map(|c| &c.key_id).collect::<Vec<_>>(),
            vec![&cross_key]
        );

        // 跨族白名单未命中、也没有同族接入点 → None，回落老链（与改造前一致）
        assert!(db
            .reserve_route_candidates(entry, Some("gpt-6"), true)
            .unwrap()
            .is_none());

        // 同族配了接入点但 key 全被判罚 → Some(empty)，fail closed 不回落
        let own = endpoint(&db, "own", UpstreamType::Claude, 100);
        let own_key = key(&db, &own, "sk-own", 50);
        db.record_api_key_failure(&own_key, "quota", None, Some(HardState::QuotaExhausted))
            .unwrap();
        db.record_api_key_failure(&cross_key, "quota", None, Some(HardState::QuotaExhausted))
            .unwrap();
        let exhausted = db
            .reserve_route_candidates(entry, Some("claude-sonnet-4"), true)
            .unwrap()
            .expect("配了接入点就必须 fail closed，不能回落老账号");
        assert!(exhausted.is_empty());

        // 同族接入点存在、model 不匹配任何白名单 → 仍是"已配置但无线路"
        let scoped = endpoint_with_models(&db, "scoped", UpstreamType::Claude, 100, &["claude-*"]);
        key(&db, &scoped, "sk-scoped", 50);
        let mismatch = db
            .reserve_route_candidates(entry, Some("gpt-6"), true)
            .unwrap()
            .expect("同族已配置时 model 不匹配必须 fail closed");
        assert!(mismatch.is_empty());
    }
}
