//! 代理池数据访问层
//!
//! 订阅、节点（含健康状态）、节点-订阅关联、粘性租约的持久化。

use crate::error::AppError;
use crate::proxy_pool::types::{
    Lease, NodeHealth, NodeProtocol, ProxyNode, Subscription, SubscriptionSource,
};
use rusqlite::{params, Row};

use super::super::{lock_conn, Database};

/// 从 DB 行还原节点 + 健康状态
fn row_to_node_with_health(row: &Row<'_>) -> rusqlite::Result<(ProxyNode, NodeHealth)> {
    let protocol_raw: String = row.get("protocol")?;
    // 协议列由本模块写入，理论上不会有非法值；兜底成 Http 而不是 panic
    let protocol = NodeProtocol::parse(&protocol_raw).unwrap_or(NodeProtocol::Http);

    let node = ProxyNode {
        hash: row.get("hash")?,
        protocol,
        host: row.get("host")?,
        port: row.get::<_, i64>("port")? as u16,
        username: row.get("username")?,
        password: row.get("password")?,
        tag: row.get("tag")?,
    };
    let health = NodeHealth {
        failure_count: row.get::<_, i64>("failure_count")? as u32,
        circuit_open_since_ms: row.get("circuit_open_since_ms")?,
        egress_ip: row.get("egress_ip")?,
        latency_ewma_ms: row.get("latency_ewma_ms")?,
        last_probe_at_ms: row.get("last_probe_at_ms")?,
    };
    Ok((node, health))
}

fn row_to_subscription(row: &Row<'_>) -> rusqlite::Result<Subscription> {
    let source_raw: String = row.get("source")?;
    let source = match source_raw.as_str() {
        "inline" => SubscriptionSource::Inline,
        _ => SubscriptionSource::Remote,
    };
    Ok(Subscription {
        id: row.get("id")?,
        name: row.get("name")?,
        source,
        url: row.get("url")?,
        content: row.get("content")?,
        enabled: row.get("enabled")?,
        update_interval_secs: row.get::<_, i64>("update_interval_secs")? as u64,
        created_at_ms: row.get("created_at_ms")?,
        updated_at_ms: row.get("updated_at_ms")?,
        node_count: row.get::<_, i64>("node_count")? as u32,
        last_error: row.get("last_error")?,
    })
}

fn source_str(source: SubscriptionSource) -> &'static str {
    match source {
        SubscriptionSource::Remote => "remote",
        SubscriptionSource::Inline => "inline",
    }
}

impl Database {
    // ---- 订阅 ----

    pub fn pp_list_subscriptions(&self) -> Result<Vec<Subscription>, AppError> {
        let conn = lock_conn!(self.conn);
        let mut stmt = conn
            .prepare(
                "SELECT id, name, source, url, content, enabled, update_interval_secs,
                        created_at_ms, updated_at_ms, node_count, last_error
                 FROM proxy_pool_subscriptions
                 ORDER BY created_at_ms ASC",
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([], row_to_subscription)
            .map_err(|e| AppError::Database(e.to_string()))?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| AppError::Database(e.to_string()))?);
        }
        Ok(out)
    }

    pub fn pp_upsert_subscription(&self, sub: &Subscription) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "INSERT INTO proxy_pool_subscriptions (
                id, name, source, url, content, enabled, update_interval_secs,
                created_at_ms, updated_at_ms, node_count, last_error
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                source = excluded.source,
                url = excluded.url,
                content = excluded.content,
                enabled = excluded.enabled,
                update_interval_secs = excluded.update_interval_secs,
                updated_at_ms = excluded.updated_at_ms,
                node_count = excluded.node_count,
                last_error = excluded.last_error",
            params![
                sub.id,
                sub.name,
                source_str(sub.source),
                sub.url,
                sub.content,
                sub.enabled,
                sub.update_interval_secs as i64,
                sub.created_at_ms,
                sub.updated_at_ms,
                sub.node_count as i64,
                sub.last_error,
            ],
        )
        .map_err(|e| AppError::Database(format!("保存订阅失败: {e}")))?;
        Ok(())
    }

    pub fn pp_delete_subscription(&self, id: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "DELETE FROM proxy_pool_subscriptions WHERE id = ?1",
            params![id],
        )
        .map_err(|e| AppError::Database(format!("删除订阅失败: {e}")))?;
        // 关联表有 ON DELETE CASCADE，但 SQLite 需显式开启外键才生效，
        // 这里手动清理以免依赖连接级 pragma。
        conn.execute(
            "DELETE FROM proxy_pool_node_subscriptions WHERE subscription_id = ?1",
            params![id],
        )
        .map_err(|e| AppError::Database(format!("清理订阅关联失败: {e}")))?;
        Ok(())
    }

    // ---- 节点 ----

    /// 载入全部节点及其所属订阅
    pub fn pp_load_nodes(&self) -> Result<Vec<(ProxyNode, NodeHealth, Vec<String>)>, AppError> {
        let conn = lock_conn!(self.conn);

        let mut stmt = conn
            .prepare(
                "SELECT hash, protocol, host, port, username, password, tag,
                        failure_count, circuit_open_since_ms, egress_ip,
                        latency_ewma_ms, last_probe_at_ms
                 FROM proxy_pool_nodes",
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([], row_to_node_with_health)
            .map_err(|e| AppError::Database(e.to_string()))?;

        let mut nodes = Vec::new();
        for row in rows {
            nodes.push(row.map_err(|e| AppError::Database(e.to_string()))?);
        }

        // 一次取全部关联，避免 N+1
        let mut link_stmt = conn
            .prepare("SELECT node_hash, subscription_id FROM proxy_pool_node_subscriptions")
            .map_err(|e| AppError::Database(e.to_string()))?;
        let links = link_stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| AppError::Database(e.to_string()))?;

        let mut by_node: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for link in links {
            let (hash, sub_id) = link.map_err(|e| AppError::Database(e.to_string()))?;
            by_node.entry(hash).or_default().push(sub_id);
        }

        Ok(nodes
            .into_iter()
            .map(|(node, health)| {
                let subs = by_node.get(&node.hash).cloned().unwrap_or_default();
                (node, health, subs)
            })
            .collect())
    }

    /// 用一个订阅的解析结果替换其节点关联。
    ///
    /// 在单事务内完成：写入/更新节点（保留既有健康状态）、重建关联、
    /// 清理不再被任何订阅引用的孤儿节点。
    pub fn pp_sync_subscription_nodes(
        &self,
        subscription_id: &str,
        nodes: &[ProxyNode],
    ) -> Result<(), AppError> {
        let mut conn = lock_conn!(self.conn);
        let tx = conn
            .transaction()
            .map_err(|e| AppError::Database(e.to_string()))?;

        for node in nodes {
            // 已存在的节点只更新可变元数据，健康状态列不动
            tx.execute(
                "INSERT INTO proxy_pool_nodes (
                    hash, protocol, host, port, username, password, tag
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(hash) DO UPDATE SET tag = excluded.tag",
                params![
                    node.hash,
                    node.protocol.as_str(),
                    node.host,
                    node.port as i64,
                    node.username,
                    node.password,
                    node.tag,
                ],
            )
            .map_err(|e| AppError::Database(format!("写入节点失败: {e}")))?;
        }

        tx.execute(
            "DELETE FROM proxy_pool_node_subscriptions WHERE subscription_id = ?1",
            params![subscription_id],
        )
        .map_err(|e| AppError::Database(format!("清理节点关联失败: {e}")))?;

        for node in nodes {
            tx.execute(
                "INSERT OR IGNORE INTO proxy_pool_node_subscriptions (node_hash, subscription_id)
                 VALUES (?1, ?2)",
                params![node.hash, subscription_id],
            )
            .map_err(|e| AppError::Database(format!("写入节点关联失败: {e}")))?;
        }

        // 孤儿节点：不再被任何订阅引用
        tx.execute(
            "DELETE FROM proxy_pool_nodes
             WHERE hash NOT IN (SELECT node_hash FROM proxy_pool_node_subscriptions)",
            [],
        )
        .map_err(|e| AppError::Database(format!("清理孤儿节点失败: {e}")))?;

        // 租约指向的节点若已消失则一并清理
        tx.execute(
            "DELETE FROM proxy_pool_leases
             WHERE node_hash NOT IN (SELECT hash FROM proxy_pool_nodes)",
            [],
        )
        .map_err(|e| AppError::Database(format!("清理失效租约失败: {e}")))?;

        tx.commit()
            .map_err(|e| AppError::Database(format!("提交节点同步失败: {e}")))?;
        Ok(())
    }

    /// 批量回写健康状态（探测循环结束后调用）
    pub fn pp_save_health(&self, updates: &[(String, NodeHealth)]) -> Result<(), AppError> {
        if updates.is_empty() {
            return Ok(());
        }
        let mut conn = lock_conn!(self.conn);
        let tx = conn
            .transaction()
            .map_err(|e| AppError::Database(e.to_string()))?;

        for (hash, health) in updates {
            tx.execute(
                "UPDATE proxy_pool_nodes SET
                    failure_count = ?2,
                    circuit_open_since_ms = ?3,
                    egress_ip = ?4,
                    latency_ewma_ms = ?5,
                    last_probe_at_ms = ?6
                 WHERE hash = ?1",
                params![
                    hash,
                    health.failure_count as i64,
                    health.circuit_open_since_ms,
                    health.egress_ip,
                    health.latency_ewma_ms,
                    health.last_probe_at_ms,
                ],
            )
            .map_err(|e| AppError::Database(format!("回写节点健康状态失败: {e}")))?;
        }

        tx.commit()
            .map_err(|e| AppError::Database(format!("提交健康状态失败: {e}")))?;
        Ok(())
    }

    // ---- 租约 ----

    pub fn pp_load_leases(&self) -> Result<Vec<Lease>, AppError> {
        let conn = lock_conn!(self.conn);
        let mut stmt = conn
            .prepare(
                "SELECT sticky_key, node_hash, egress_ip, created_at_ms, last_used_at_ms
                 FROM proxy_pool_leases",
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok(Lease {
                    sticky_key: row.get("sticky_key")?,
                    node_hash: row.get("node_hash")?,
                    egress_ip: row.get("egress_ip")?,
                    created_at_ms: row.get("created_at_ms")?,
                    last_used_at_ms: row.get("last_used_at_ms")?,
                })
            })
            .map_err(|e| AppError::Database(e.to_string()))?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| AppError::Database(e.to_string()))?);
        }
        Ok(out)
    }

    /// 全量替换租约表。租约数量与业务身份同级（几十到几百），全量重写足够。
    pub fn pp_save_leases(&self, leases: &[Lease]) -> Result<(), AppError> {
        let mut conn = lock_conn!(self.conn);
        let tx = conn
            .transaction()
            .map_err(|e| AppError::Database(e.to_string()))?;

        tx.execute("DELETE FROM proxy_pool_leases", [])
            .map_err(|e| AppError::Database(format!("清理租约失败: {e}")))?;

        for lease in leases {
            tx.execute(
                "INSERT INTO proxy_pool_leases (
                    sticky_key, node_hash, egress_ip, created_at_ms, last_used_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    lease.sticky_key,
                    lease.node_hash,
                    lease.egress_ip,
                    lease.created_at_ms,
                    lease.last_used_at_ms,
                ],
            )
            .map_err(|e| AppError::Database(format!("写入租约失败: {e}")))?;
        }

        tx.commit()
            .map_err(|e| AppError::Database(format!("提交租约失败: {e}")))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy_pool::types::now_ms;

    fn test_db() -> Database {
        Database::memory().expect("in-memory db")
    }

    fn sub(id: &str) -> Subscription {
        Subscription {
            id: id.to_string(),
            name: format!("sub-{id}"),
            source: SubscriptionSource::Remote,
            url: "https://example.com/sub".into(),
            content: String::new(),
            enabled: true,
            update_interval_secs: 3600,
            created_at_ms: now_ms(),
            updated_at_ms: now_ms(),
            node_count: 0,
            last_error: None,
        }
    }

    fn node(port: u16) -> ProxyNode {
        ProxyNode::new(
            NodeProtocol::Socks5,
            "1.2.3.4".into(),
            port,
            Some("u".into()),
            Some("p".into()),
            format!("node-{port}"),
        )
    }

    #[test]
    fn subscription_roundtrips() {
        let db = test_db();
        let s = sub("s1");
        db.pp_upsert_subscription(&s).expect("insert");

        let all = db.pp_list_subscriptions().expect("list");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, "s1");
        assert_eq!(all[0].url, "https://example.com/sub");
        assert_eq!(all[0].update_interval_secs, 3600);
        assert!(all[0].enabled);
    }

    #[test]
    fn subscription_upsert_updates_in_place() {
        let db = test_db();
        let mut s = sub("s1");
        db.pp_upsert_subscription(&s).expect("insert");

        s.name = "renamed".into();
        s.node_count = 42;
        s.last_error = Some("boom".into());
        db.pp_upsert_subscription(&s).expect("update");

        let all = db.pp_list_subscriptions().expect("list");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "renamed");
        assert_eq!(all[0].node_count, 42);
        assert_eq!(all[0].last_error.as_deref(), Some("boom"));
    }

    #[test]
    fn nodes_roundtrip_with_credentials_and_links() {
        let db = test_db();
        db.pp_upsert_subscription(&sub("s1")).expect("sub");
        let nodes = vec![node(1080), node(1081)];
        db.pp_sync_subscription_nodes("s1", &nodes).expect("sync");

        let loaded = db.pp_load_nodes().expect("load");
        assert_eq!(loaded.len(), 2);
        // 凭据必须能还原，否则重启后无法重建代理 URL
        let (n, _, subs) = &loaded[0];
        assert_eq!(n.username.as_deref(), Some("u"));
        assert_eq!(n.password.as_deref(), Some("p"));
        assert_eq!(n.protocol, NodeProtocol::Socks5);
        assert_eq!(subs, &vec!["s1".to_string()]);
    }

    #[test]
    fn health_survives_subscription_resync() {
        let db = test_db();
        db.pp_upsert_subscription(&sub("s1")).expect("sub");
        let n = node(1080);
        db.pp_sync_subscription_nodes("s1", &[n.clone()])
            .expect("sync");

        let health = NodeHealth {
            failure_count: 2,
            circuit_open_since_ms: 0,
            egress_ip: Some("9.9.9.9".into()),
            latency_ewma_ms: Some(123.5),
            last_probe_at_ms: 555,
        };
        db.pp_save_health(&[(n.hash.clone(), health)])
            .expect("save health");

        // 重新同步同一节点（tag 变了）
        let mut renamed = n.clone();
        renamed.tag = "new-tag".into();
        db.pp_sync_subscription_nodes("s1", &[renamed])
            .expect("resync");

        let loaded = db.pp_load_nodes().expect("load");
        assert_eq!(loaded.len(), 1);
        let (loaded_node, loaded_health, _) = &loaded[0];
        assert_eq!(loaded_node.tag, "new-tag");
        // 健康状态没被覆盖
        assert_eq!(loaded_health.egress_ip.as_deref(), Some("9.9.9.9"));
        assert_eq!(loaded_health.latency_ewma_ms, Some(123.5));
        assert_eq!(loaded_health.failure_count, 2);
    }

    #[test]
    fn node_shared_by_two_subscriptions_survives_one_removal() {
        let db = test_db();
        db.pp_upsert_subscription(&sub("s1")).expect("s1");
        db.pp_upsert_subscription(&sub("s2")).expect("s2");
        let n = node(1080);
        db.pp_sync_subscription_nodes("s1", &[n.clone()])
            .expect("sync1");
        db.pp_sync_subscription_nodes("s2", &[n.clone()])
            .expect("sync2");

        let loaded = db.pp_load_nodes().expect("load");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].2.len(), 2);

        // s1 清空后节点仍被 s2 引用
        db.pp_sync_subscription_nodes("s1", &[]).expect("empty s1");
        let loaded = db.pp_load_nodes().expect("load");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].2, vec!["s2".to_string()]);

        // s2 也清空 → 变成孤儿被删
        db.pp_sync_subscription_nodes("s2", &[]).expect("empty s2");
        assert!(db.pp_load_nodes().expect("load").is_empty());
    }

    #[test]
    fn leases_roundtrip_and_full_replace() {
        let db = test_db();
        db.pp_upsert_subscription(&sub("s1")).expect("sub");
        let n = node(1080);
        db.pp_sync_subscription_nodes("s1", &[n.clone()])
            .expect("sync");

        let lease = Lease {
            sticky_key: "provider-a".into(),
            node_hash: n.hash.clone(),
            egress_ip: Some("9.9.9.9".into()),
            created_at_ms: 100,
            last_used_at_ms: 200,
        };
        db.pp_save_leases(&[lease]).expect("save");

        let loaded = db.pp_load_leases().expect("load");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].sticky_key, "provider-a");
        assert_eq!(loaded[0].egress_ip.as_deref(), Some("9.9.9.9"));

        // 全量替换语义：写空数组等于清空
        db.pp_save_leases(&[]).expect("clear");
        assert!(db.pp_load_leases().expect("load").is_empty());
    }

    #[test]
    fn orphan_lease_removed_when_node_disappears() {
        let db = test_db();
        db.pp_upsert_subscription(&sub("s1")).expect("sub");
        let n = node(1080);
        db.pp_sync_subscription_nodes("s1", &[n.clone()])
            .expect("sync");
        db.pp_save_leases(&[Lease {
            sticky_key: "k".into(),
            node_hash: n.hash.clone(),
            egress_ip: None,
            created_at_ms: 1,
            last_used_at_ms: 1,
        }])
        .expect("save lease");

        // 节点消失后租约应被同事务清理
        db.pp_sync_subscription_nodes("s1", &[]).expect("empty");
        assert!(db.pp_load_leases().expect("load").is_empty());
    }

    #[test]
    fn delete_subscription_clears_links() {
        let db = test_db();
        db.pp_upsert_subscription(&sub("s1")).expect("sub");
        let n = node(1080);
        db.pp_sync_subscription_nodes("s1", &[n]).expect("sync");

        db.pp_delete_subscription("s1").expect("delete");
        assert!(db.pp_list_subscriptions().expect("list").is_empty());
        // 关联已清，节点变孤儿（下次 sync 时清理，或由 load 得到空 subs）
        let loaded = db.pp_load_nodes().expect("load");
        assert!(loaded.iter().all(|(_, _, subs)| subs.is_empty()));
    }

    #[test]
    fn save_health_on_empty_slice_is_noop() {
        let db = test_db();
        db.pp_save_health(&[]).expect("noop");
    }
}
