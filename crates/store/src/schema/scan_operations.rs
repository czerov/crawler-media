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
             report_json TEXT NOT NULL DEFAULT '{}'
         );
         CREATE INDEX IF NOT EXISTS scan_operations_lib_status_idx
             ON scan_operations (library_id, status);
         CREATE TABLE IF NOT EXISTS scan_seen (
             operation_id TEXT NOT NULL,
             path TEXT NOT NULL,
             PRIMARY KEY (operation_id, path)
         );",
    )?;
    Ok(())
}
