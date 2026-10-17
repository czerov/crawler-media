//! DB-05 分类历史保留（默认关闭）。
//!
//! 三个类别各自有一个**稳定 keyset 游标**，单批只检视 `limit` 行：
//!
//! - [`RetentionClass::Qoe`]：`subscribe.db.playback_metrics`，按 `recorded_at`。
//!   不动 `playback_units` / `playback_sessions` / 播放日志。
//! - [`RetentionClass::TransientCatalog`]：`catalog.db.catalog_cache`，只删
//!   URL path 白名单内的 search/discover/trending 临时结果。详情（`/movie/…`、
//!   `/tv/…` 等）与无法分类的 key 一律保护——stale 不等于可删。
//! - [`RetentionClass::ProbeHistory`]：`library.db.probe_jobs` 的终态行。每个
//!   `scope_key` 至少保留最新 3 条（边界时间并列全部保留），被
//!   `fingerprint_samples.captured_job_id` 引用的 job、以及仍有活动 unit 的 job
//!   全部保护。attempts 只在 job 可删时按 `job_id` 同事务级联；samples/models/
//!   model_members 本轮不删。
//!
//! `dry_run` 走同一段选择逻辑但零写入。`examined` 是窗口内检视的行数，
//! `protected` 是其中因保护规则留下的行数；`incomplete` 表示窗口外仍有未检视
//! 的超龄行（不是精确剩余总数），`resume_after` 是下一次继续的游标。
//!
//! 单条 SQL 不可中断，所以「一批 500 行」只约束返回/删除预算，不证明扫描或持锁
//! 时间有硬上界。

use rusqlite::{Connection, params};

use super::{Store, StoreError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetentionClass {
    Qoe,
    TransientCatalog,
    ProbeHistory,
}

impl RetentionClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Qoe => "qoe",
            Self::TransientCatalog => "transient_catalog",
            Self::ProbeHistory => "probe_history",
        }
    }
}

/// 类别内的稳定 keyset 位置。`id` 的含义由类别决定（QoE metric id、catalog
/// rowid、probe job id），调用方只应原样回传，不要解析。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DataRetentionCursor {
    pub at_ms: i64,
    pub id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DataRetentionBatch {
    pub examined: usize,
    pub deleted: usize,
    pub protected: usize,
    /// 窗口外仍有未检视的超龄行；这不是剩余总数的精确计数。
    pub incomplete: bool,
    pub resume_after: Option<DataRetentionCursor>,
}


/// `cutoff_ms` 是 epoch 毫秒。QoE 的 `recorded_at` 以 unix 秒存储，因此比较时
/// 取秒级下界（严格小于 cutoff 所在秒）：边界行保留，绝不因毫秒换算多删。
pub fn prune_data_history(
    store: &Store,
    class: RetentionClass,
    cutoff_ms: i64,
    cursor: Option<&DataRetentionCursor>,
    limit: usize,
    dry_run: bool,
) -> Result<DataRetentionBatch, StoreError> {
    if limit == 0 {
        return Err(StoreError::Invalid(
            "data retention limit must be >= 1".into(),
        ));
    }
    // 服务端固定预算：调用方传更大值也不会扩大单批窗口。
    let limit = limit.min(500);
    match class {
        RetentionClass::Qoe => prune_qoe(store, cutoff_ms, cursor, limit, dry_run),
        RetentionClass::TransientCatalog => {
            prune_transient_catalog(store, cutoff_ms, cursor, limit, dry_run)
        }
        RetentionClass::ProbeHistory => prune_probe_history(store, cutoff_ms, cursor, limit, dry_run),
    }
}

impl Store {
    /// 见模块文档。签名是公开合同的一部分，不要在调用方复制选择/保护规则。
    pub fn prune_data_history(
        &self,
        class: RetentionClass,
        cutoff_ms: i64,
        cursor: Option<&DataRetentionCursor>,
        limit: usize,
        dry_run: bool,
    ) -> Result<DataRetentionBatch, StoreError> {
        prune_data_history(self, class, cutoff_ms, cursor, limit, dry_run)
    }
}

fn limit_plus_one(limit: usize) -> i64 {
    i64::try_from(limit.saturating_add(1)).unwrap_or(i64::MAX)
}

fn chunk_delete(
    conn: &Connection,
    table: &str,
    id_column: &str,
    ids: &[String],
) -> Result<usize, StoreError> {
    let mut deleted = 0;
    for chunk in ids.chunks(400) {
        let marks = vec!["?"; chunk.len()].join(",");
        let sql = format!("DELETE FROM {table} WHERE {id_column} IN ({marks})");
        deleted += conn.execute(&sql, rusqlite::params_from_iter(chunk))?;
    }
    Ok(deleted)
}

fn chunk_delete_rowids(
    conn: &Connection,
    table: &str,
    rowids: &[i64],
) -> Result<usize, StoreError> {
    let mut deleted = 0;
    for chunk in rowids.chunks(400) {
        let marks = vec!["?"; chunk.len()].join(",");
        let sql = format!("DELETE FROM {table} WHERE rowid IN ({marks})");
        deleted += conn.execute(&sql, rusqlite::params_from_iter(chunk))?;
    }
    Ok(deleted)
}

// ---------------------------------------------------------------------------
// QoE
// ---------------------------------------------------------------------------

fn prune_qoe(
    store: &Store,
    cutoff_ms: i64,
    cursor: Option<&DataRetentionCursor>,
    limit: usize,
    dry_run: bool,
) -> Result<DataRetentionBatch, StoreError> {
    // recorded_at 是 unix 秒；cutoff_ms 向下取整到秒，等于边界的行保留。
    let cutoff_secs = cutoff_ms.div_euclid(1000);
    let cursor_secs = cursor.map(|cursor| cursor.at_ms.div_euclid(1000));
    let cursor_id = cursor.map(|cursor| cursor.id.as_str());

    let tx = store.subscribe.unchecked_transaction()?;
    let mut rows = Vec::with_capacity(limit.saturating_add(1));
    {
        let mut stmt = tx.prepare(
            "SELECT id, recorded_at FROM playback_metrics
             WHERE recorded_at < ?1
               AND (?2 IS NULL
                    OR recorded_at > ?3
                    OR (recorded_at = ?3 AND id > ?4))
             ORDER BY recorded_at ASC, id ASC
             LIMIT ?5",
        )?;
        let mapped = stmt.query_map(
            params![
                cutoff_secs,
                cursor_secs,
                cursor_secs.unwrap_or(0),
                cursor_id.unwrap_or(""),
                limit_plus_one(limit)
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )?;
        for row in mapped {
            rows.push(row?);
        }
    }
    let incomplete = rows.len() > limit;
    rows.truncate(limit);
    let examined = rows.len();
    let deleted = if dry_run {
        0
    } else {
        let ids: Vec<String> = rows.iter().map(|(id, _)| id.clone()).collect();
        chunk_delete(&tx, "playback_metrics", "id", &ids)?
    };
    tx.commit()?;

    let resume_after = if incomplete {
        rows.last().map(|(id, recorded_at)| DataRetentionCursor {
            at_ms: recorded_at.saturating_mul(1000),
            id: id.clone(),
        })
    } else {
        None
    };
    Ok(DataRetentionBatch {
        examined,
        deleted,
        protected: 0,
        incomplete,
        resume_after,
    })
}

// ---------------------------------------------------------------------------
// Transient catalog cache
// ---------------------------------------------------------------------------

/// URL path 白名单：只有 search / discover / trending 的临时结果可删。
/// 详情与未知 key 一律保护（stale cache 仍承担 stale-if-error）。
pub(crate) fn is_transient_catalog_key(cache_key: &str) -> bool {
    let path = cache_key.split('?').next().unwrap_or(cache_key);
    let path = path.trim_end_matches('/');
    let segments: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    match segments.as_slice() {
        ["search", ..] | ["discover", ..] | ["trending", ..] => true,
        ["j", "search_subjects", ..] => true,
        _ => false,
    }
}

fn prune_transient_catalog(
    store: &Store,
    cutoff_ms: i64,
    cursor: Option<&DataRetentionCursor>,
    limit: usize,
    dry_run: bool,
) -> Result<DataRetentionBatch, StoreError> {
    let cutoff_secs = cutoff_ms.div_euclid(1000);
    let cursor_secs = cursor.map(|cursor| cursor.at_ms.div_euclid(1000));
    let cursor_rowid = match cursor {
        Some(cursor) => Some(cursor.id.parse::<i64>().map_err(|_| {
            StoreError::Invalid(format!("invalid catalog retention cursor id: {}", cursor.id))
        })?),
        None => None,
    };

    let tx = store.catalog.unchecked_transaction()?;
    let mut rows = Vec::with_capacity(limit.saturating_add(1));
    {
        let mut stmt = tx.prepare(
            "SELECT rowid, cache_key, fetched_at FROM catalog_cache
             WHERE fetched_at < ?1
               AND (?2 IS NULL
                    OR fetched_at > ?3
                    OR (fetched_at = ?3 AND rowid > ?4))
             ORDER BY fetched_at ASC, rowid ASC
             LIMIT ?5",
        )?;
        let mapped = stmt.query_map(
            params![
                cutoff_secs,
                cursor_secs,
                cursor_secs.unwrap_or(0),
                cursor_rowid.unwrap_or(0),
                limit_plus_one(limit)
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )?;
        for row in mapped {
            rows.push(row?);
        }
    }
    let incomplete = rows.len() > limit;
    rows.truncate(limit);
    let examined = rows.len();
    let mut to_delete = Vec::new();
    let mut protected = 0usize;
    for (rowid, cache_key, _) in &rows {
        if is_transient_catalog_key(cache_key) {
            to_delete.push(*rowid);
        } else {
            protected += 1;
        }
    }
    let deleted = if dry_run {
        0
    } else {
        chunk_delete_rowids(&tx, "catalog_cache", &to_delete)?
    };
    tx.commit()?;

    let resume_after = if incomplete {
        rows.last().map(|(rowid, _, fetched_at)| DataRetentionCursor {
            at_ms: fetched_at.saturating_mul(1000),
            id: rowid.to_string(),
        })
    } else {
        None
    };
    Ok(DataRetentionBatch {
        examined,
        deleted,
        protected,
        incomplete,
        resume_after,
    })
}

// ---------------------------------------------------------------------------
// Probe history
// ---------------------------------------------------------------------------

const PROBE_TERMINAL_STATUSES: &str = "('succeeded', 'failed', 'cancelled')";

fn prune_probe_history(
    store: &Store,
    cutoff_ms: i64,
    cursor: Option<&DataRetentionCursor>,
    limit: usize,
    dry_run: bool,
) -> Result<DataRetentionBatch, StoreError> {
    let cursor_at = cursor.map(|cursor| cursor.at_ms);
    let cursor_id = cursor.map(|cursor| cursor.id.as_str());

    let tx = store.library.unchecked_transaction()?;
    let mut rows = Vec::with_capacity(limit.saturating_add(1));
    {
        let sql = format!(
            "SELECT id, scope_key, finished_at_ms FROM probe_jobs
             WHERE status IN {PROBE_TERMINAL_STATUSES}
               AND finished_at_ms IS NOT NULL
               AND finished_at_ms < ?1
               AND (?2 IS NULL
                    OR finished_at_ms > ?3
                    OR (finished_at_ms = ?3 AND id > ?4))
             ORDER BY finished_at_ms ASC, id ASC
             LIMIT ?5"
        );
        let mut stmt = tx.prepare(&sql)?;
        let mapped = stmt.query_map(
            params![
                cutoff_ms,
                cursor_at,
                cursor_at.unwrap_or(0),
                cursor_id.unwrap_or(""),
                limit_plus_one(limit)
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )?;
        for row in mapped {
            rows.push(row?);
        }
    }
    let incomplete = rows.len() > limit;
    rows.truncate(limit);
    let examined = rows.len();

    let mut to_delete = Vec::new();
    let mut protected = 0usize;
    for (id, scope_key, finished_at_ms) in &rows {
        if probe_job_protected(&tx, id, scope_key, *finished_at_ms)? {
            protected += 1;
        } else {
            to_delete.push(id.clone());
        }
    }

    let mut deleted = 0usize;
    if !dry_run {
        for id in &to_delete {
            // attempts 与 job 必须在同一事务里：job 可删才级联 attempts，
            // 不允许出现「attempts 删了、job 还在」或反向的半状态。
            tx.execute(
                "DELETE FROM fingerprint_attempts WHERE job_id = ?1",
                params![id],
            )?;
            tx.execute("DELETE FROM probe_job_units WHERE job_id = ?1", params![id])?;
            deleted += tx.execute("DELETE FROM probe_jobs WHERE id = ?1", params![id])?;
        }
    }
    tx.commit()?;

    let resume_after = if incomplete {
        rows.last()
            .map(|(id, _, finished_at_ms)| DataRetentionCursor {
                at_ms: *finished_at_ms,
                id: id.clone(),
            })
    } else {
        None
    };
    Ok(DataRetentionBatch {
        examined,
        deleted,
        protected,
        incomplete,
        resume_after,
    })
}

/// 一个终态 job 是否必须保留：
/// 1. 被 fingerprint sample 引用（样本仍在用这份工作）；
/// 2. 仍有 queued/running unit（终态 job 但单位没收口，保守保护）；
/// 3. 属于该 scope 最新 3 条终态之一；边界 `finished_at_ms` 并列的全部保护。
fn probe_job_protected(
    tx: &rusqlite::Transaction<'_>,
    job_id: &str,
    scope_key: &str,
    finished_at_ms: i64,
) -> Result<bool, StoreError> {
    let referenced: bool = tx.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM fingerprint_samples WHERE captured_job_id = ?1
         )",
        params![job_id],
        |row| row.get(0),
    )?;
    if referenced {
        return Ok(true);
    }
    let active_unit: bool = tx.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM probe_job_units
             WHERE job_id = ?1 AND status IN ('queued', 'running')
         )",
        params![job_id],
        |row| row.get(0),
    )?;
    if active_unit {
        return Ok(true);
    }
    // 终态 job 仍可能被 probe stage state 的 active_job_id 引用（收尾竞态）；
    // 删除会留下悬空引用，保守保护。
    let referenced_by_stage: bool = tx.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM probe_stage_state WHERE active_job_id = ?1
         )",
        params![job_id],
        |row| row.get(0),
    )?;
    if referenced_by_stage {
        return Ok(true);
    }

    let sql = format!(
        "SELECT MIN(finished_at_ms) FROM (
             SELECT finished_at_ms FROM probe_jobs
             WHERE scope_key = ?1
               AND status IN {PROBE_TERMINAL_STATUSES}
               AND finished_at_ms IS NOT NULL
             ORDER BY finished_at_ms DESC, id DESC
             LIMIT 3
         )"
    );
    let newest_three_floor: Option<i64> =
        tx.query_row(&sql, params![scope_key], |row| row.get(0))?;
    Ok(match newest_three_floor {
        // scope 内终态不足 3 条：当前行必然在保留集合里。
        None => true,
        // `>=` 让与第 3 新时间并列的行全部保留（时间 tie 全部保护）。
        Some(floor) => finished_at_ms >= floor,
    })
}

#[cfg(test)]
mod tests {
    use super::is_transient_catalog_key;

    #[test]
    fn transient_classifier_only_accepts_search_discover_trending() {
        for key in [
            "/search/movie?query=avatar",
            "/search/tv?query=x",
            "/search?query=x&type=movie",
            "/discover/movie?with_genres=28",
            "/trending/movie/day",
            "/j/search_subjects?type=movie",
        ] {
            assert!(is_transient_catalog_key(key), "{key} 应为 transient");
        }
        for key in [
            "/movie/42",
            "/tv/42?language=zh-CN",
            "/movie/42/credits",
            "/person/7",
            "/j/subject/123",
            "search:movie:avatar",
            "movie:42",
        ] {
            assert!(!is_transient_catalog_key(key), "{key} 必须保护");
        }
    }
}

