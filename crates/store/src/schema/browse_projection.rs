use rusqlite::Connection;

use crate::StoreError;

pub fn migrate_browse_projection(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS browse_generations(
             library_id TEXT PRIMARY KEY,
             generation INTEGER NOT NULL,
             scope_signature TEXT NOT NULL,
             state TEXT NOT NULL,
             refreshed_at_ms INTEGER NOT NULL,
             app_revision INTEGER NOT NULL,
             library_revision INTEGER NOT NULL,
             catalog_revision INTEGER NOT NULL,
             subscribe_revision INTEGER NOT NULL,
             dirty_token INTEGER NOT NULL DEFAULT 0
         );
         CREATE TABLE IF NOT EXISTS browse_files(
             library_id TEXT NOT NULL,
             ledger_id TEXT NOT NULL,
             media_id TEXT NOT NULL,
             generation INTEGER NOT NULL,
             facts_json TEXT NOT NULL,
             PRIMARY KEY(library_id, ledger_id)
         );
         CREATE INDEX IF NOT EXISTS browse_files_media_idx ON browse_files(library_id, media_id);
         CREATE TABLE IF NOT EXISTS browse_media(
             library_id TEXT NOT NULL,
             media_id TEXT NOT NULL,
             generation INTEGER NOT NULL,
             title TEXT NOT NULL,
             year INTEGER,
             facts_json TEXT NOT NULL,
             PRIMARY KEY(library_id, media_id)
         );
         CREATE INDEX IF NOT EXISTS browse_media_title_idx ON browse_media(library_id, title, media_id);",
    )?;
    Ok(())
}
