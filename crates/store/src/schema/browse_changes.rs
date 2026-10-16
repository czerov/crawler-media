use rusqlite::Connection;

use crate::StoreError;

pub fn migrate_browse_changes_app(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS _browse_changes (
            seq INTEGER PRIMARY KEY AUTOINCREMENT,
            entity_kind TEXT NOT NULL,
            entity_key TEXT NOT NULL,
            old_path TEXT,
            new_path TEXT
        );
        CREATE INDEX IF NOT EXISTS _browse_changes_seq_idx ON _browse_changes(seq);
        CREATE TRIGGER IF NOT EXISTS trg_app_media_changes_ins AFTER INSERT ON media
        BEGIN
            INSERT INTO _browse_changes (entity_kind, entity_key, old_path, new_path)
            VALUES ('media', NEW.id, NULL, NULL);
        END;
        CREATE TRIGGER IF NOT EXISTS trg_app_media_changes_upd AFTER UPDATE ON media
        BEGIN
            INSERT INTO _browse_changes (entity_kind, entity_key, old_path, new_path)
            VALUES ('media', NEW.id, NULL, NULL);
        END;
        CREATE TRIGGER IF NOT EXISTS trg_app_media_changes_del AFTER DELETE ON media
        BEGIN
            INSERT INTO _browse_changes (entity_kind, entity_key, old_path, new_path)
            VALUES ('media', OLD.id, NULL, NULL);
        END;
        CREATE TRIGGER IF NOT EXISTS trg_app_libraries_changes_ins AFTER INSERT ON libraries
        BEGIN
            INSERT INTO _browse_changes (entity_kind, entity_key, old_path, new_path)
            VALUES ('library', NEW.id, NULL, NULL);
        END;
        CREATE TRIGGER IF NOT EXISTS trg_app_libraries_changes_upd AFTER UPDATE ON libraries
        BEGIN
            INSERT INTO _browse_changes (entity_kind, entity_key, old_path, new_path)
            VALUES ('library', NEW.id, NULL, NULL);
        END;
        CREATE TRIGGER IF NOT EXISTS trg_app_libraries_changes_del AFTER DELETE ON libraries
        BEGIN
            INSERT INTO _browse_changes (entity_kind, entity_key, old_path, new_path)
            VALUES ('library', OLD.id, NULL, NULL);
        END;
        CREATE TRIGGER IF NOT EXISTS trg_app_roots_changes_ins AFTER INSERT ON library_roots
        BEGIN
            INSERT INTO _browse_changes (entity_kind, entity_key, old_path, new_path)
            VALUES ('library_root', NEW.id, NULL, NEW.path);
        END;
        CREATE TRIGGER IF NOT EXISTS trg_app_roots_changes_upd AFTER UPDATE ON library_roots
        BEGIN
            INSERT INTO _browse_changes (entity_kind, entity_key, old_path, new_path)
            VALUES ('library_root', NEW.id, OLD.path, NEW.path);
        END;
        CREATE TRIGGER IF NOT EXISTS trg_app_roots_changes_del AFTER DELETE ON library_roots
        BEGIN
            INSERT INTO _browse_changes (entity_kind, entity_key, old_path, new_path)
            VALUES ('library_root', OLD.id, OLD.path, NULL);
        END;",
    )?;
    Ok(())
}

pub fn migrate_browse_changes_catalog(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS _browse_changes (
            seq INTEGER PRIMARY KEY AUTOINCREMENT,
            entity_kind TEXT NOT NULL,
            entity_key TEXT NOT NULL,
            old_path TEXT,
            new_path TEXT
        );
        CREATE INDEX IF NOT EXISTS _browse_changes_seq_idx ON _browse_changes(seq);
        CREATE TRIGGER IF NOT EXISTS trg_cat_cache_changes_ins AFTER INSERT ON catalog_cache
        BEGIN
            INSERT INTO _browse_changes (entity_kind, entity_key, old_path, new_path)
            VALUES ('catalog_cache', NEW.source || ':' || NEW.cache_key, NULL, NULL);
        END;
        CREATE TRIGGER IF NOT EXISTS trg_cat_cache_changes_upd AFTER UPDATE ON catalog_cache
        BEGIN
            INSERT INTO _browse_changes (entity_kind, entity_key, old_path, new_path)
            VALUES ('catalog_cache', NEW.source || ':' || NEW.cache_key, NULL, NULL);
        END;
        CREATE TRIGGER IF NOT EXISTS trg_cat_cache_changes_del AFTER DELETE ON catalog_cache
        BEGIN
            INSERT INTO _browse_changes (entity_kind, entity_key, old_path, new_path)
            VALUES ('catalog_cache', OLD.source || ':' || OLD.cache_key, NULL, NULL);
        END;",
    )?;
    Ok(())
}

pub fn migrate_browse_changes_library(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS _browse_changes (
            seq INTEGER PRIMARY KEY AUTOINCREMENT,
            entity_kind TEXT NOT NULL,
            entity_key TEXT NOT NULL,
            old_path TEXT,
            new_path TEXT
        );
        CREATE INDEX IF NOT EXISTS _browse_changes_seq_idx ON _browse_changes(seq);
        CREATE TABLE IF NOT EXISTS browse_change_cursors (
            source TEXT PRIMARY KEY,
            last_seq INTEGER NOT NULL DEFAULT 0
        );
        CREATE TRIGGER IF NOT EXISTS trg_lib_ledger_changes_ins AFTER INSERT ON ledger
        BEGIN
            INSERT INTO _browse_changes (entity_kind, entity_key, old_path, new_path)
            VALUES ('ledger', NEW.id, NULL, NEW.path);
        END;
        CREATE TRIGGER IF NOT EXISTS trg_lib_ledger_changes_upd AFTER UPDATE ON ledger
        BEGIN
            INSERT INTO _browse_changes (entity_kind, entity_key, old_path, new_path)
            VALUES ('ledger', NEW.id, OLD.path, NEW.path);
        END;
        CREATE TRIGGER IF NOT EXISTS trg_lib_ledger_changes_del AFTER DELETE ON ledger
        BEGIN
            INSERT INTO _browse_changes (entity_kind, entity_key, old_path, new_path)
            VALUES ('ledger', OLD.id, OLD.path, NULL);
        END;
        CREATE TRIGGER IF NOT EXISTS trg_lib_file_meta_changes_ins AFTER INSERT ON file_meta
        BEGIN
            INSERT INTO _browse_changes (entity_kind, entity_key, old_path, new_path)
            VALUES ('file_meta', NEW.ledger_id, NULL, NULL);
        END;
        CREATE TRIGGER IF NOT EXISTS trg_lib_file_meta_changes_upd AFTER UPDATE ON file_meta
        BEGIN
            INSERT INTO _browse_changes (entity_kind, entity_key, old_path, new_path)
            VALUES ('file_meta', NEW.ledger_id, NULL, NULL);
        END;
        CREATE TRIGGER IF NOT EXISTS trg_lib_file_meta_changes_del AFTER DELETE ON file_meta
        BEGIN
            INSERT INTO _browse_changes (entity_kind, entity_key, old_path, new_path)
            VALUES ('file_meta', OLD.ledger_id, NULL, NULL);
        END;
        CREATE TRIGGER IF NOT EXISTS trg_lib_markers_changes_ins AFTER INSERT ON media_markers
        BEGIN
            INSERT INTO _browse_changes (entity_kind, entity_key, old_path, new_path)
            VALUES ('media_marker', 'marker:' || NEW.media_id || ':' || NEW.season || ':' || NEW.episode, NULL, NULL);
        END;
        CREATE TRIGGER IF NOT EXISTS trg_lib_markers_changes_upd AFTER UPDATE ON media_markers
        BEGIN
            INSERT INTO _browse_changes (entity_kind, entity_key, old_path, new_path)
            VALUES ('media_marker', 'marker:' || NEW.media_id || ':' || NEW.season || ':' || NEW.episode, NULL, NULL);
        END;
        CREATE TRIGGER IF NOT EXISTS trg_lib_markers_changes_del AFTER DELETE ON media_markers
        BEGIN
            INSERT INTO _browse_changes (entity_kind, entity_key, old_path, new_path)
            VALUES ('media_marker', 'marker:' || OLD.media_id || ':' || OLD.season || ':' || OLD.episode, NULL, NULL);
        END;",
    )?;
    Ok(())
}
