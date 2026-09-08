//! API 网关 DAO
//!
//! 接入点与密钥的增删改查 + 选线查询。
//!
//! `api_keys.last_used_at` 持久化 LRU 顺序；请求选线与预占在连接锁内完成，
//! 避免秒级时间戳并列、在途请求尚未记账时重复选择首 key。预览不修改顺序。

use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

use super::api_gateway_types::{
    last4, ApiEndpointRecord, ApiKeyRecord, HardState, NewApiEndpoint, NewApiKey, RouteCandidate,
    UpstreamType,
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

fn validate_endpoint_url(raw: &str) -> Result<(), AppError> {
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

impl Database {
    /// 列出接入点（按展示序），带密钥计数。
    pub fn list_api_endpoints(&self) -> Result<Vec<ApiEndpointRecord>, AppError> {
        let conn = lock_conn!(self.conn);

        let mut stmt = conn
            .prepare(
                "SELECT e.*, (SELECT COUNT(*) FROM api_keys k WHERE k.endpoint_id = e.id) AS key_count
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

    /// 新建接入点，追加到展示序末尾。
    pub fn create_api_endpoint(&self, input: &NewApiEndpoint) -> Result<String, AppError> {
        let name = input.name.trim();
        if name.is_empty() {
            return Err(AppError::Config("接入点名称不能为空".into()));
        }
        let base_url = input.base_url.trim();
        if base_url.is_empty() {
            return Err(AppError::Config("接入点地址不能为空".into()));
        }
        validate_endpoint_url(base_url)?;

        let conn = lock_conn!(self.conn);
        let id = new_id("ep");
        let models = serde_json::to_string(&input.models)
            .map_err(|e| AppError::Config(format!("模型列表序列化失败: {e}")))?;

        let next_sort: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(sort_index), -1) + 1 FROM api_endpoints",
                [],
                |row| row.get(0),
            )
            .map_err(|e| AppError::Database(e.to_string()))?;

        conn.execute(
            "INSERT INTO api_endpoints
             (id, name, upstream_type, base_url, models, priority, enabled, sort_index, notes, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?8, ?9)",
            params![
                id,
                name,
                input.upstream_type.as_str(),
                base_url,
                models,
                input.priority,
                next_sort,
                input.notes.as_deref().map(str::trim).filter(|s| !s.is_empty()),
                now_secs(),
            ],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;

        Ok(id)
    }

    /// 更新接入点。sort_index 由 reorder 单独维护，这里不动。
    pub fn update_api_endpoint(&self, record: &ApiEndpointRecord) -> Result<(), AppError> {
        let name = record.name.trim();
        if name.is_empty() {
            return Err(AppError::Config("接入点名称不能为空".into()));
        }
        let base_url = record.base_url.trim();
        if base_url.is_empty() {
            return Err(AppError::Config("接入点地址不能为空".into()));
        }
        validate_endpoint_url(base_url)?;

        let conn = lock_conn!(self.conn);
        let models = serde_json::to_string(&record.models)
            .map_err(|e| AppError::Config(format!("模型列表序列化失败: {e}")))?;

        let affected = conn
            .execute(
                "UPDATE api_endpoints
                 SET name = ?1, upstream_type = ?2, base_url = ?3, models = ?4,
                     priority = ?5, enabled = ?6, notes = ?7
                 WHERE id = ?8",
                params![
                    name,
                    record.upstream_type.as_str(),
                    base_url,
                    models,
                    record.priority,
                    if record.enabled { 1 } else { 0 },
                    record
                        .notes
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty()),
                    record.id,
                ],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;

        if affected == 0 {
            return Err(AppError::Config(format!("接入点不存在: {}", record.id)));
        }
        Ok(())
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
        let conn = lock_conn!(self.conn);
        conn.execute(
            "UPDATE api_endpoints SET enabled = ?1 WHERE id = ?2",
            params![if enabled { 1 } else { 0 }, endpoint_id],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
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

        Ok(items)
    }

    pub fn create_api_key(&self, input: &NewApiKey) -> Result<String, AppError> {
        let api_key = input.api_key.trim();
        if api_key.is_empty() {
            return Err(AppError::Config("密钥不能为空".into()));
        }

        let conn = lock_conn!(self.conn);

        let endpoint_exists: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM api_endpoints WHERE id = ?1",
                [&input.endpoint_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| AppError::Database(e.to_string()))?;
        if endpoint_exists.is_none() {
            return Err(AppError::Config(format!(
                "接入点不存在: {}",
                input.endpoint_id
            )));
        }

        let id = new_id("key");
        conn.execute(
            "INSERT INTO api_keys
             (id, endpoint_id, api_key, key_last4, name, internal_priority, enabled, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7)",
            params![
                id,
                input.endpoint_id,
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

    pub fn delete_api_key(&self, key_id: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute("DELETE FROM api_keys WHERE id = ?1", [key_id])
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn set_api_key_enabled(&self, key_id: &str, enabled: bool) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "UPDATE api_keys SET enabled = ?1 WHERE id = ?2",
            params![if enabled { 1 } else { 0 }, key_id],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
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
    /// 1. `e.priority` —— 层级，越小越优先
    /// 2. `k.internal_priority` —— 层内序
    /// 3. `last_used_at IS NOT NULL ASC` —— 从未用过的排最前
    /// 4. `last_used_at ASC` —— 其余按最久未用（LRU 轮询）
    ///
    /// 序列是 per-request 构建的，游标只活在这一次请求内：降级纯临时、绝不粘滞，
    /// 高优先级恢复后下一个请求自动回到它。
    pub fn select_route_candidates(
        &self,
        upstream: UpstreamType,
        model: Option<&str>,
    ) -> Result<Vec<RouteCandidate>, AppError> {
        let conn = lock_conn!(self.conn);
        query_route_candidates(&conn, &[upstream], model)
    }

    /// None: no enabled gateway is configured, so legacy routing is allowed.
    /// Some(empty): configured gateway has no eligible key/model; fail closed.
    /// Selection and reservation share the connection lock, before any network I/O.
    pub fn reserve_route_candidates(
        &self,
        upstreams: &[UpstreamType],
        model: Option<&str>,
    ) -> Result<Option<Vec<RouteCandidate>>, AppError> {
        let conn = lock_conn!(self.conn);
        let upstream_json =
            serde_json::to_string(upstreams).map_err(|e| AppError::Config(e.to_string()))?;
        let configured: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM api_endpoints WHERE enabled = 1
             AND upstream_type IN (SELECT value FROM json_each(?1)))",
                [upstream_json],
                |row| row.get(0),
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        if !configured {
            return Ok(None);
        }
        let candidates = query_route_candidates(&conn, upstreams, model)?;
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

fn query_route_candidates(
    conn: &Connection,
    upstreams: &[UpstreamType],
    model: Option<&str>,
) -> Result<Vec<RouteCandidate>, AppError> {
    let upstream_json =
        serde_json::to_string(upstreams).map_err(|e| AppError::Config(e.to_string()))?;
    let mut stmt = conn.prepare(
        "SELECT k.id AS key_id, k.api_key, k.key_last4, k.internal_priority, k.last_used_at,
                e.id AS endpoint_id, e.name AS endpoint_name, e.base_url, e.priority, e.upstream_type
         FROM api_keys k JOIN api_endpoints e ON k.endpoint_id = e.id
         WHERE e.enabled = 1 AND k.enabled = 1 AND k.hard_state IS NULL
           AND (k.cooldown_until IS NULL OR k.cooldown_until <= ?1)
           AND e.upstream_type IN (SELECT value FROM json_each(?2))
           AND (?3 IS NULL OR json_array_length(e.models) = 0
                OR EXISTS (SELECT 1 FROM json_each(e.models) WHERE value = ?3))
         ORDER BY e.priority ASC, k.internal_priority ASC,
                  k.last_used_at IS NOT NULL ASC, k.last_used_at ASC, k.id ASC"
    ).map_err(|e| AppError::Database(e.to_string()))?;
    let rows = stmt
        .query_map(params![now_secs(), upstream_json, model], |row| {
            let raw: String = row.get("upstream_type")?;
            Ok(RouteCandidate {
                key_id: row.get("key_id")?,
                endpoint_id: row.get("endpoint_id")?,
                endpoint_name: row.get("endpoint_name")?,
                base_url: row.get("base_url")?,
                upstream_type: UpstreamType::parse(&raw).ok_or(rusqlite::Error::InvalidQuery)?,
                key_last4: row.get("key_last4")?,
                priority: row.get("priority")?,
                internal_priority: row.get("internal_priority")?,
                last_used_at: row.get("last_used_at")?,
                api_key: row.get("api_key")?,
            })
        })
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

    fn key(db: &Database, endpoint_id: &str, secret: &str, internal_priority: i64) -> String {
        db.create_api_key(&NewApiKey {
            endpoint_id: endpoint_id.to_string(),
            api_key: secret.to_string(),
            name: None,
            internal_priority,
        })
        .unwrap()
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
            .reserve_route_candidates(&[UpstreamType::Claude], Some("test"))
            .unwrap()
            .unwrap();
        let second = db
            .reserve_route_candidates(&[UpstreamType::Claude], Some("test"))
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
            .reserve_route_candidates(&[UpstreamType::Claude], None)
            .unwrap()
            .is_none());
        let ep = endpoint(&db, "empty", UpstreamType::Claude, 100);
        assert!(db
            .reserve_route_candidates(&[UpstreamType::Claude], None)
            .unwrap()
            .unwrap()
            .is_empty());
        db.set_api_endpoint_enabled(&ep, false).unwrap();
        assert!(db
            .reserve_route_candidates(&[UpstreamType::Claude], None)
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
}
