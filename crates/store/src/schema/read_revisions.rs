use rusqlite::Connection;

use crate::StoreError;

pub fn migrate_read_revisions(name: &str, conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS _read_revisions (
             id INTEGER PRIMARY KEY CHECK(id = 1),
             revision INTEGER NOT NULL DEFAULT 0
         );
         INSERT OR IGNORE INTO _read_revisions (id, revision) VALUES (1, 0);
         CREATE TRIGGER IF NOT EXISTS trg_{name}_rev_bump
         AFTER INSERT ON _read_revisions
         BEGIN
             SELECT 1;
         END;"
    ))?;
    Ok(())
}

pub fn create_revision_trigger(conn: &Connection, table: &str, name: &str) -> Result<(), StoreError> {
    conn.execute_batch(&format!(
        "CREATE TRIGGER IF NOT EXISTS trg_{name}_on_ins AFTER INSERT ON {table}
         BEGIN
             UPDATE _read_revisions SET revision = revision + 1 WHERE id = 1;
         END;
         CREATE TRIGGER IF NOT EXISTS trg_{name}_on_upd AFTER UPDATE ON {table}
         BEGIN
             UPDATE _read_revisions SET revision = revision + 1 WHERE id = 1;
         END;
         CREATE TRIGGER IF NOT EXISTS trg_{name}_on_del AFTER DELETE ON {table}
         BEGIN
             UPDATE _read_revisions SET revision = revision + 1 WHERE id = 1;
         END;"
    ))?;
    Ok(())
}
