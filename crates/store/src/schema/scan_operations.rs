use rusqlite::Connection;

use crate::StoreError;

pub fn migrate_scan_operations(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS scan_operations (
             id TEXT PRIMARY KEY,
             library_id TEXT NOT NULL,
             scope_signature TEXT NOT NULL,
             status TEXT NOT NULL,
             generation INTEGER NOT NULL,
             created_at_ms INTEGER NOT NULL,
             report_json TEXT NOT NULL DEFAULT '{}',
             targets_json TEXT NOT NULL DEFAULT '[]',
             scope_kind TEXT NOT NULL DEFAULT 'full',
             dirty_rev INTEGER NOT NULL DEFAULT 0,
             consumed_rev INTEGER NOT NULL DEFAULT 0
         );
         CREATE INDEX IF NOT EXISTS scan_operations_lib_status_idx
             ON scan_operations (library_id, status);
         CREATE TABLE IF NOT EXISTS scan_seen (
             operation_id TEXT NOT NULL,
             path TEXT NOT NULL,
             PRIMARY KEY (operation_id, path)
         );",
    )?;
    // 老库补列（幂等）：watch 增量扫描目标必须跟着 operation 一起持久化，才能在
    // 单飞合并与崩溃重排之后继续生效。
    super::ensure_column(
        conn,
        "scan_operations",
        "targets_json",
        "TEXT NOT NULL DEFAULT '[]'",
    )?;
    super::ensure_column(
        conn,
        "scan_operations",
        "scope_kind",
        "TEXT NOT NULL DEFAULT 'full'",
    )?;
    super::ensure_column(
        conn,
        "scan_operations",
        "dirty_rev",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    super::ensure_column(
        conn,
        "scan_operations",
        "consumed_rev",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    super::ensure_column(conn, "scan_seen", "mtime_ms", "INTEGER NOT NULL DEFAULT 0")?;
    super::ensure_column(conn, "scan_seen", "file_size", "INTEGER NOT NULL DEFAULT 0")?;
    // P2-3：STRM 只有 URL 文本，size/mtime 无法建立身份；受限内容指纹单独持久化。
    super::ensure_column(conn, "scan_seen", "content_digest", "TEXT")?;
    Ok(())
}
