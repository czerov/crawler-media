use rusqlite::Connection;

use super::StoreError;

/// Durable chapter frame obligations (MEM-03).
///
/// `UNIQUE(ledger_id, source_version, targets_hash)` is the idempotency key: a
/// repeated marker publication for the same source version and chapter set must
/// not create a second process obligation.
pub(super) fn migrate_chapter_frame_tasks(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS chapter_frame_tasks (
            id TEXT PRIMARY KEY,
            ledger_id TEXT NOT NULL,
            source_version TEXT NOT NULL,
            targets_json TEXT NOT NULL,
            targets_hash TEXT NOT NULL,
            status TEXT NOT NULL,
            error TEXT,
            created_at_ms INTEGER NOT NULL,
            started_at_ms INTEGER,
            finished_at_ms INTEGER,
            UNIQUE (ledger_id, source_version, targets_hash)
        );
        CREATE INDEX IF NOT EXISTS chapter_frame_tasks_pending_idx
            ON chapter_frame_tasks(status, created_at_ms, id);
        "#,
    )?;
    Ok(())
}
