//! DB-05 分类历史保留所需索引（幂等迁移）。
//!
//! 这些索引只服务 retention 的稳定 keyset 游标与保护判定，不改变任何既有查询
//! 语义。`CREATE INDEX IF NOT EXISTS` 每次打开都执行，旧库无需回填。

use rusqlite::Connection;

use crate::StoreError;

/// subscribe.db：QoE 按 `recorded_at` 的 keyset。
pub(crate) fn migrate_data_retention_subscribe(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS playback_metrics_recorded_idx
             ON playback_metrics(recorded_at ASC, id ASC);",
    )?;
    Ok(())
}

/// catalog.db：按 `fetched_at` 的 keyset（rowid 是索引的隐式尾列）。
pub(crate) fn migrate_data_retention_catalog(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS catalog_cache_fetched_idx
             ON catalog_cache(fetched_at ASC);",
    )?;
    Ok(())
}

/// library.db：probe 终态窗口、per-scope 最新 3 条判定、sample 引用保护。
pub(crate) fn migrate_data_retention_library(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS probe_jobs_terminal_finished_idx
             ON probe_jobs(finished_at_ms ASC, id ASC)
             WHERE status IN ('succeeded', 'failed', 'cancelled');
         CREATE INDEX IF NOT EXISTS probe_jobs_scope_terminal_finished_idx
             ON probe_jobs(scope_key, finished_at_ms DESC, id DESC)
             WHERE status IN ('succeeded', 'failed', 'cancelled');
         CREATE INDEX IF NOT EXISTS fingerprint_samples_captured_job_idx
             ON fingerprint_samples(captured_job_id);",
    )?;
    Ok(())
}
