//! Local-only diagnostic traces, deliberately separate from token/cost accounting.

use crate::{
    database::{lock_conn, Database},
    error::AppError,
};
use rusqlite::{named_params, params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

const CONFIG_KEY: &str = "request_trace_config";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct RequestTraceConfig {
    pub enabled: bool,
    pub capture_bodies: bool,
    pub max_body_bytes: usize,
    pub retention_days: u32,
    pub max_entries: u32,
    pub max_storage_mb: u32,
}

impl Default for RequestTraceConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            capture_bodies: true,
            max_body_bytes: 1024 * 1024,
            retention_days: 3,
            max_entries: 1000,
            max_storage_mb: 128,
        }
    }
}

impl RequestTraceConfig {
    pub fn validate(&self) -> Result<(), AppError> {
        if !(4096..=4 * 1024 * 1024).contains(&self.max_body_bytes)
            || !(1..=90).contains(&self.retention_days)
            || !(10..=100_000).contains(&self.max_entries)
            || !(32..=2048).contains(&self.max_storage_mb)
        {
            return Err(AppError::InvalidInput(
                "日志配置超出范围：报文 4 KiB–4 MiB、保留 1–90 天、10–100000 条、32–2048 MiB"
                    .into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TraceHeader {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TracePayload {
    pub headers: Vec<TraceHeader>,
    pub body: String,
    /// Bytes actually observed on this hop, before decoding/redaction.
    pub body_bytes: u64,
    pub captured_bytes: usize,
    pub truncated: bool,
    pub redacted: bool,
    /// utf8, base64 (binary/partial compression), or omitted.
    pub body_encoding: String,
    pub capture_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RequestTraceSummary {
    pub request_id: String,
    pub started_at: i64,
    pub method: String,
    pub path: String,
    /// Socket peer, never a caller-controlled X-Forwarded-For value.
    pub client_ip: String,
    pub client_port: Option<u16>,
    pub entry_protocol: String,
    pub model: Option<String>,
    pub status_code: Option<u16>,
    pub state: String,
    pub duration_ms: Option<u64>,
    pub first_byte_ms: Option<u64>,
    pub attempt_count: usize,
    pub provider_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RequestTraceAttempt {
    pub index: usize,
    pub provider_id: String,
    pub provider_name: String,
    pub protocol: String,
    pub method: String,
    pub url: String,
    pub proxy: Option<String>,
    pub model: Option<String>,
    pub started_at: i64,
    pub duration_ms: Option<u64>,
    pub status_code: Option<u16>,
    pub error: Option<String>,
    pub request: TracePayload,
    pub response: Option<TracePayload>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RequestTraceDetail {
    #[serde(flatten)]
    pub summary: RequestTraceSummary,
    pub request: TracePayload,
    pub response: Option<TracePayload>,
    pub attempts: Vec<RequestTraceAttempt>,
    pub error: Option<String>,
    pub body_capture_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct RequestTraceFilters {
    pub query: Option<String>,
    pub client_ip: Option<String>,
    pub entry_protocol: Option<String>,
    pub status_code: Option<u16>,
    pub errors_only: bool,
    pub start_time: Option<i64>,
    pub end_time: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestTracePage {
    pub data: Vec<RequestTraceSummary>,
    pub total: u64,
    pub storage_bytes: u64,
}

impl Database {
    pub(crate) fn create_request_trace_tables(conn: &Connection) -> Result<(), AppError> {
        conn.execute_batch("CREATE TABLE IF NOT EXISTS request_traces (
            request_id TEXT PRIMARY KEY, started_at INTEGER NOT NULL,
            client_ip TEXT NOT NULL, entry_protocol TEXT NOT NULL,
            status_code INTEGER, state TEXT NOT NULL,
            summary_json TEXT NOT NULL, detail_json TEXT NOT NULL,
            stored_bytes INTEGER NOT NULL DEFAULT 0, revision INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_request_traces_time ON request_traces(started_at DESC);
        CREATE INDEX IF NOT EXISTS idx_request_traces_ip ON request_traces(client_ip, started_at DESC);
        CREATE INDEX IF NOT EXISTS idx_request_traces_status ON request_traces(status_code, started_at DESC);")?;
        Ok(())
    }

    pub fn get_request_trace_config(&self) -> Result<RequestTraceConfig, AppError> {
        let Some(value) = self.get_setting(CONFIG_KEY)? else {
            return Ok(RequestTraceConfig::default());
        };
        let config: RequestTraceConfig = serde_json::from_str(&value)
            .map_err(|e| AppError::Config(format!("读取请求日志配置失败: {e}")))?;
        config.validate()?;
        Ok(config)
    }

    pub fn set_request_trace_config(&self, config: &RequestTraceConfig) -> Result<(), AppError> {
        config.validate()?;
        self.set_setting(
            CONFIG_KEY,
            &serde_json::to_string(config).map_err(|e| AppError::Config(e.to_string()))?,
        )?;
        let conn = lock_conn!(self.conn);
        prune(&conn, config)?;
        Ok(())
    }

    pub(crate) fn insert_request_trace(
        &self,
        detail: &RequestTraceDetail,
        config: &RequestTraceConfig,
    ) -> Result<(), AppError> {
        let summary =
            serde_json::to_string(&detail.summary).map_err(|e| AppError::Config(e.to_string()))?;
        let body = serde_json::to_string(detail).map_err(|e| AppError::Config(e.to_string()))?;
        let conn = lock_conn!(self.conn);
        conn.execute("INSERT INTO request_traces
            (request_id, started_at, client_ip, entry_protocol, status_code, state, summary_json, detail_json, stored_bytes)
            VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)", params![detail.summary.request_id,
            detail.summary.started_at, detail.summary.client_ip, detail.summary.entry_protocol,
            detail.summary.status_code, detail.summary.state, summary, body, (summary.len() + body.len()) as i64])?;
        prune(&conn, config)?;
        Ok(())
    }

    pub(crate) fn update_request_trace(
        &self,
        detail: &RequestTraceDetail,
        revision: u64,
        config: &RequestTraceConfig,
    ) -> Result<(), AppError> {
        let summary =
            serde_json::to_string(&detail.summary).map_err(|e| AppError::Config(e.to_string()))?;
        let body = serde_json::to_string(detail).map_err(|e| AppError::Config(e.to_string()))?;
        let conn = lock_conn!(self.conn);
        // UPDATE, never UPSERT: a late stream completion must not resurrect a
        // trace the user cleared. Revisions make asynchronous snapshots ordered.
        conn.execute(
            "UPDATE request_traces SET status_code=?2,state=?3,summary_json=?4,
            detail_json=?5,stored_bytes=?6,revision=?7 WHERE request_id=?1 AND revision<?7",
            params![
                detail.summary.request_id,
                detail.summary.status_code,
                detail.summary.state,
                summary,
                body,
                (summary.len() + body.len()) as i64,
                revision
            ],
        )?;
        prune(&conn, config)?;
        Ok(())
    }

    pub fn list_request_traces(
        &self,
        filters: &RequestTraceFilters,
        page: u32,
        page_size: u32,
    ) -> Result<RequestTracePage, AppError> {
        if page_size == 0 || page_size > 100 || page > 1_000_000 {
            return Err(AppError::InvalidInput("无效的日志分页参数".into()));
        }
        if filters.query.as_ref().is_some_and(|v| v.len() > 2048) {
            return Err(AppError::InvalidInput("日志搜索词过长".into()));
        }
        let query = filters.query.as_ref().filter(|s| !s.is_empty()).map(|s| {
            format!(
                "%{}%",
                s.replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            )
        });
        let predicate = "(:query IS NULL OR detail_json LIKE :query ESCAPE '\\')
            AND (:ip IS NULL OR client_ip=:ip) AND (:protocol IS NULL OR entry_protocol=:protocol)
            AND (:status IS NULL OR status_code=:status)
            AND (:errors=0 OR state IN ('error','cancelled','interrupted') OR status_code>=400)
            AND (:start IS NULL OR started_at>=:start) AND (:end IS NULL OR started_at<=:end)";
        let bindings = named_params! {":query":query, ":ip":filters.client_ip,
        ":protocol":filters.entry_protocol, ":status":filters.status_code,
        ":errors":filters.errors_only, ":start":filters.start_time, ":end":filters.end_time};
        let config = self.get_request_trace_config()?;
        let conn = lock_conn!(self.conn);
        // Opening the page also expires logs after an otherwise idle period.
        prune(&conn, &config)?;
        let total = conn.query_row(
            &format!("SELECT COUNT(*) FROM request_traces WHERE {predicate}"),
            bindings,
            |row| row.get(0),
        )?;
        let mut stmt = conn.prepare(&format!(
            "SELECT summary_json,state FROM request_traces WHERE {predicate}
            ORDER BY started_at DESC,request_id DESC LIMIT :limit OFFSET :offset"
        ))?;
        let rows = stmt.query_map(
            named_params! {":query":query, ":ip":filters.client_ip,
            ":protocol":filters.entry_protocol, ":status":filters.status_code,
            ":errors":filters.errors_only, ":start":filters.start_time, ":end":filters.end_time,
            ":limit":page_size, ":offset":u64::from(page)*u64::from(page_size)},
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?;
        let mut data = Vec::new();
        for row in rows {
            let (json, state) = row?;
            let mut summary: RequestTraceSummary = serde_json::from_str(&json)
                .map_err(|e| AppError::Database(format!("请求日志索引损坏: {e}")))?;
            summary.state = state;
            data.push(summary);
        }
        let storage_bytes = conn.query_row(
            "SELECT COALESCE(SUM(stored_bytes),0) FROM request_traces",
            [],
            |row| row.get(0),
        )?;
        Ok(RequestTracePage {
            data,
            total,
            storage_bytes,
        })
    }

    pub fn get_request_trace(&self, id: &str) -> Result<Option<RequestTraceDetail>, AppError> {
        let conn = lock_conn!(self.conn);
        let row: Option<(String, String)> = conn
            .query_row(
                "SELECT detail_json,state FROM request_traces WHERE request_id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        row.map(|(json, state)| {
            let mut detail: RequestTraceDetail = serde_json::from_str(&json)
                .map_err(|e| AppError::Database(format!("请求日志详情损坏: {e}")))?;
            detail.summary.state = state;
            Ok(detail)
        })
        .transpose()
    }

    pub fn clear_request_traces(&self) -> Result<usize, AppError> {
        let conn = lock_conn!(self.conn);
        let deleted = conn.execute("DELETE FROM request_traces", [])?;
        conn.execute_batch("PRAGMA incremental_vacuum(256);")?;
        Ok(deleted)
    }

    pub(crate) fn recover_request_traces(&self) -> Result<(), AppError> {
        let config = self.get_request_trace_config()?;
        let conn = lock_conn!(self.conn);
        conn.execute(
            "UPDATE request_traces SET state='interrupted' WHERE state='in_progress'",
            [],
        )?;
        prune(&conn, &config)?;
        Ok(())
    }
}

fn prune(conn: &Connection, config: &RequestTraceConfig) -> Result<(), AppError> {
    let cutoff =
        chrono::Utc::now().timestamp_millis() - i64::from(config.retention_days) * 86_400_000;
    conn.execute("DELETE FROM request_traces WHERE started_at<?1", [cutoff])?;
    conn.execute("DELETE FROM request_traces WHERE request_id IN (
        SELECT request_id FROM request_traces ORDER BY started_at DESC,request_id DESC LIMIT -1 OFFSET ?1)", [config.max_entries])?;
    conn.execute("DELETE FROM request_traces WHERE request_id IN (SELECT request_id FROM (
        SELECT request_id,SUM(stored_bytes) OVER (ORDER BY started_at DESC,request_id DESC) AS cumulative_bytes
        FROM request_traces) WHERE cumulative_bytes>?1)", [u64::from(config.max_storage_mb) * 1024 * 1024])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trace(id: &str) -> RequestTraceDetail {
        RequestTraceDetail {
            summary: RequestTraceSummary {
                request_id: id.into(),
                started_at: chrono::Utc::now().timestamp_millis(),
                client_ip: "127.0.0.1".into(),
                entry_protocol: "openai_chat".into(),
                state: "in_progress".into(),
                ..Default::default()
            },
            request: TracePayload {
                body: "中文提示词 100% _literal".into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn traces_persist_filter_page_and_do_not_reappear_after_clear() {
        let db = Database::memory().unwrap();
        let config = RequestTraceConfig::default();
        let mut detail = trace("trace-1");
        db.insert_request_trace(&detail, &config).unwrap();
        detail.summary.state = "error".into();
        detail.summary.status_code = Some(400);
        db.update_request_trace(&detail, 2, &config).unwrap();
        let mut stale = detail.clone();
        stale.summary.state = "in_progress".into();
        db.update_request_trace(&stale, 1, &config).unwrap();
        let filters = RequestTraceFilters {
            query: Some("100% _literal".into()),
            errors_only: true,
            ..Default::default()
        };
        assert_eq!(db.list_request_traces(&filters, 0, 20).unwrap().total, 1);
        assert!(db
            .list_request_traces(&filters, 1, 20)
            .unwrap()
            .data
            .is_empty());
        assert_eq!(
            db.get_request_trace("trace-1")
                .unwrap()
                .unwrap()
                .summary
                .state,
            "error"
        );
        assert_eq!(db.clear_request_traces().unwrap(), 1);
        db.update_request_trace(&detail, 3, &config).unwrap();
        assert!(db.get_request_trace("trace-1").unwrap().is_none());
    }

    #[test]
    fn trace_retention_bounds_entries_and_sync_never_exports_prompts() {
        let db = Database::memory().unwrap();
        let config = RequestTraceConfig {
            max_entries: 10,
            ..Default::default()
        };
        for i in 0..12 {
            db.insert_request_trace(&trace(&format!("trace-{i}")), &config)
                .unwrap();
        }
        assert_eq!(
            db.list_request_traces(&RequestTraceFilters::default(), 0, 20)
                .unwrap()
                .total,
            10
        );
        assert!(!db
            .export_sql_string_for_sync()
            .unwrap()
            .contains("中文提示词"));
        db.recover_request_traces().unwrap();
        assert!(db
            .list_request_traces(&RequestTraceFilters::default(), 0, 20)
            .unwrap()
            .data
            .iter()
            .all(|s| s.state == "interrupted"));
    }
}
