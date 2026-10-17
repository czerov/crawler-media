use rusqlite::{Connection, OptionalExtension, params};

use super::StoreError;

mod chapter_frame_tasks;
mod fingerprint;
mod library_deletion;
mod probe_state;
mod tables;
mod browse_changes;
mod browse_page_facts;
mod browse_projection;
mod data_retention;
mod read_revisions;
mod scan_operations;

/// Schema version constants. Bump when adding new migrations.
/// The version is stored via `PRAGMA user_version` on each database file.
pub const APP_SCHEMA_VERSION: i64 = 8;
pub const CATALOG_SCHEMA_VERSION: i64 = 3;
pub const LIBRARY_SCHEMA_VERSION: i64 = 17;
pub const SUBSCRIBE_SCHEMA_VERSION: i64 = 8;

/// Read the current schema version from a database file.
///
/// 读取失败必须向上报错：版本判定决定是否允许迁移，吞成 0 会把「读不出来」
/// 伪装成「空库」。
pub(crate) fn read_schema_version(conn: &Connection) -> Result<i64, StoreError> {
    Ok(conn.pragma_query_value(None, "user_version", |row| row.get(0))?)
}

/// 降级保护：任一库的 `user_version` 高于本程序支持的版本就拒绝打开。
///
/// 纯只读预检：不写 `user_version`、不建表、不 seed、不改连接模式，
/// 因此命中时不会改动任何数据库文件。
pub(crate) fn ensure_supported_versions(
    databases: &[(&str, &Connection, i64)],
) -> Result<(), StoreError> {
    for (name, conn, supported) in databases {
        let found = read_schema_version(conn)?;
        if found > *supported {
            tracing::error!(
                database = name,
                found,
                supported,
                "数据库 schema 版本高于本程序，拒绝降级打开"
            );
            return Err(StoreError::Invalid(format!(
                "{name} database schema version {found} is newer than supported {supported}"
            )));
        }
    }
    Ok(())
}

/// Set the schema version after successful migration.
fn set_schema_version(conn: &Connection, version: i64) -> Result<(), StoreError> {
    conn.pragma_update(None, "user_version", version)?;
    Ok(())
}

pub fn migrate_app(conn: &Connection) -> Result<(), StoreError> {
    ensure_supported_versions(&[("app", conn, APP_SCHEMA_VERSION)])?;
    let previous = read_schema_version(conn)?;
    conn.execute_batch(tables::APP_TABLES)?;
    conn.execute_batch(library_deletion::TABLES)?;
    // app schema 7：删除边界根目录从 `snapshot_json` 迁到 `library_removal_roots`，
    // 让路径边界检查不再反序列化整份快照。回填幂等（INSERT OR IGNORE），只对旧库执行。
    if previous < APP_SCHEMA_VERSION {
        crate::library_deletion::backfill_library_removal_roots(conn)?;
    }
    // b3192f1 之前删除 Library 不清理 `library_cover_checked.{id}`。当时的修复被绑在
    // `previous < APP_SCHEMA_VERSION` 上却没有升版本，于是已经处于 app schema 7 的库
    // 永远跑不到它。这里改成每次打开都做一次幂等对账：按定义只删「removal 已成功且
    // Library 行已不存在」的键，重复执行安全，对 v7 老库也不是空操作。
    crate::library_deletion::repair_removed_library_settings(conn)?;
    ensure_column(conn, "media", "aliases_json", "TEXT NOT NULL DEFAULT '[]'")?;
    ensure_column(conn, "downloaders", "path_maps", "TEXT")?;
    // app schema 8：Site 维护方式。存量 Site（v7 及更早）一律补成 `http`，
    // 与旧版本只走 HTTP 登录/签到的行为一致；不会自动切到 Browser。
    ensure_column(
        conn,
        "sites",
        "maintenance_via",
        "TEXT NOT NULL DEFAULT 'http' CHECK (maintenance_via IN ('http','browser'))",
    )?;
    // downloaders.enabled：停用的下载器不参与自动投递（手动提交仍可用）。
    ensure_column(conn, "downloaders", "enabled", "INTEGER NOT NULL DEFAULT 1")?;
    ensure_column(conn, "users", "role", "TEXT NOT NULL DEFAULT 'member'")?;
    ensure_column(conn, "users", "password", "TEXT NOT NULL DEFAULT ''")?;
    ensure_column(conn, "users", "enabled", "INTEGER NOT NULL DEFAULT 1")?;
    // 老库回填：把既有的 token 行当作密码（token 即密码的旧模型），
    // 保证升级后 admin 仍能用原来的口令登录（changeme 等）。
    conn.execute(
        "UPDATE users SET password = COALESCE(
             (SELECT token FROM user_tokens WHERE user_id = users.id LIMIT 1), '')
          WHERE password = ''",
        [],
    )?;
    ensure_column(
        conn,
        "filters",
        "keep_old_versions",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    migrate_app_library_columns(conn)?;
    // 会话 token 时效：老库补 created_at 列，并把存量 token 视为刚刚签发，
    // 升级后获得一个完整的 30 天窗口（Session TTL）而不是立即被过期掉。
    ensure_column(
        conn,
        "user_tokens",
        "created_at",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    conn.execute(
        "UPDATE user_tokens SET created_at = strftime('%s','now') WHERE created_at = 0",
        [],
    )?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS collections (
            id TEXT PRIMARY KEY,
            user_id TEXT NOT NULL,
            name TEXT NOT NULL,
            sort_order INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS collection_items (
            collection_id TEXT NOT NULL,
            media_item_id TEXT NOT NULL,
            sort_order INTEGER NOT NULL DEFAULT 0,
            added_at TEXT NOT NULL,
            PRIMARY KEY (collection_id, media_item_id)
        );",
    )?;
    read_revisions::migrate_read_revisions("app", conn)?;
    read_revisions::create_revision_trigger(conn, "media", "app_media")?;
    read_revisions::create_revision_trigger(conn, "libraries", "app_libraries")?;
    browse_changes::migrate_browse_changes_app(conn)?;
    set_schema_version(conn, APP_SCHEMA_VERSION)?;
    Ok(())
}

fn migrate_app_library_columns(conn: &Connection) -> Result<(), StoreError> {
    ensure_column(conn, "library_roots", "library_id", "TEXT")?;
    ensure_column(
        conn,
        "library_roots",
        "sort_order",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(conn, "libraries", "detect_intros", "INTEGER")?;
    ensure_column(conn, "libraries", "enable_fingerprint", "INTEGER")?;
    ensure_column(conn, "libraries", "cover_path", "TEXT")?;
    ensure_column(
        conn,
        "libraries",
        "cover_revision",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        conn,
        "libraries",
        "match_rules_json",
        "TEXT NOT NULL DEFAULT '[]'",
    )?;
    ensure_column(conn, "libraries", "default_filter_id", "TEXT")?;
    ensure_column(
        conn,
        "libraries",
        "realtime_watch",
        "INTEGER NOT NULL DEFAULT 1",
    )?;
    ensure_column(
        conn,
        "libraries",
        "generate_thumbnails",
        "INTEGER NOT NULL DEFAULT 1",
    )?;
    ensure_column(
        conn,
        "libraries",
        "extract_chapter_images",
        "INTEGER NOT NULL DEFAULT 1",
    )?;
    ensure_column(
        conn,
        "libraries",
        "exclude_from_home",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        conn,
        "libraries",
        "auto_series_collections",
        "INTEGER NOT NULL DEFAULT 1",
    )?;
    // libraries 可见性列（老库需逐列补齐，否则 SELECT 报 no such column）。
    ensure_column(
        conn,
        "libraries",
        "access_mode",
        "TEXT NOT NULL DEFAULT 'everyone'",
    )?;
    ensure_column(
        conn,
        "libraries",
        "admin_visible",
        "INTEGER NOT NULL DEFAULT 1",
    )?;
    ensure_column(
        conn,
        "libraries",
        "member_ids_json",
        "TEXT NOT NULL DEFAULT '[]'",
    )?;
    Ok(())
}

fn ensure_column(
    conn: &Connection,
    table: &str,
    column: &str,
    decl: &str,
) -> Result<(), StoreError> {
    if !column_exists(conn, table, column)? {
        conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl};"))?;
    }
    Ok(())
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool, StoreError> {
    Ok(conn
        .prepare(&format!("PRAGMA table_info({table})"))?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .any(|name| name == column))
}

pub fn migrate_catalog(conn: &Connection) -> Result<(), StoreError> {
    ensure_supported_versions(&[("catalog", conn, CATALOG_SCHEMA_VERSION)])?;
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS catalog_cache (
            source TEXT NOT NULL,
            cache_key TEXT NOT NULL,
            body TEXT NOT NULL,
            fetched_at INTEGER NOT NULL,
            expires_at INTEGER,
            PRIMARY KEY (source, cache_key)
        );
        "#,
    )?;
    read_revisions::migrate_read_revisions("catalog", conn)?;
    read_revisions::create_revision_trigger(conn, "catalog_cache", "cat_cache")?;
    browse_changes::migrate_browse_changes_catalog(conn)?;
    // catalog schema 3：transient cache retention 的 fetched_at keyset 索引。
    data_retention::migrate_data_retention_catalog(conn)?;
    set_schema_version(conn, CATALOG_SCHEMA_VERSION)?;
    Ok(())
}

pub fn migrate_library(conn: &Connection) -> Result<(), StoreError> {
    ensure_supported_versions(&[("library", conn, LIBRARY_SCHEMA_VERSION)])?;
    let previous = read_schema_version(conn)?;
    conn.execute_batch(tables::LIBRARY_TABLES)?;
    // ledger.missing_at：文件核验标记（老库补列，幂等）。
    ensure_column(conn, "ledger", "missing_at", "INTEGER")?;
    ensure_column(conn, "ledger", "transfer_mode", "TEXT")?;
    ensure_column(conn, "ledger", "source_path", "TEXT")?;
    ensure_column(conn, "ledger", "transfer_pending", "INTEGER NOT NULL DEFAULT 0")?;
    ensure_column(conn, "ledger", "release_quality", "TEXT")?;
    ensure_column(conn, "file_meta", "chapters_json", "TEXT")?;
    ensure_column(conn, "file_meta", "video_json", "TEXT")?;
    ensure_column(
        conn,
        "file_meta",
        "tracks_version",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(conn, "file_meta", "fingerprint_json", "TEXT")?;
    ensure_column(conn, "file_meta", "outro_fingerprint_json", "TEXT")?;
    ensure_column(conn, "file_meta", "source_version", "TEXT")?;
    ensure_column(conn, "file_meta", "format_duration_ms", "INTEGER")?;
    ensure_column(
        conn,
        "probe_job_units",
        "reuse_fingerprint_cache",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(conn, "probe_jobs", "partial", "INTEGER NOT NULL DEFAULT 0")?;
    fingerprint::migrate_library_fingerprint(conn)?;
    probe_state::migrate_probe_state(conn)?;
    // library schema 8: ledger_probe_obligations 表用于扫描原子登记后的 probe 义务跟踪与恢复
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS ledger_probe_obligations (
             ledger_id TEXT PRIMARY KEY,
             created_at_ms INTEGER NOT NULL,
             reason TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS ledger_probe_obligations_created_ledger_idx
            ON ledger_probe_obligations(created_at_ms, ledger_id);",
    )?;
    scan_operations::migrate_scan_operations(conn)?;
    chapter_frame_tasks::migrate_chapter_frame_tasks(conn)?;
    browse_projection::migrate_browse_projection(conn)?;
    // library schema 15：browse_media 的 SQL 排序键列与索引（A4 浏览页）。
    browse_page_facts::migrate_browse_page_facts(conn)?;
    // library schema 16：probe 终态/scope/attempt 关联索引（DB-05）。
    data_retention::migrate_data_retention_library(conn)?;
    // library schema 17：probe 派发 stage 的有界窗口索引。旧库补列后建索引，
    // 两者都幂等，重启不会重扫全表以外的工作。
    ensure_column(
        conn,
        "probe_job_units",
        "stage",
        "TEXT NOT NULL DEFAULT 'metadata'",
    )?;
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS probe_job_units_dispatch_idx
             ON probe_job_units(stage, status, job_id)
             WHERE status = 'queued';",
    )?;
    if previous < 15 {
        // 旧代的投影没有候选 JSON：标脏让下一次请求重建，而不是永久回退全量读取。
        browse_page_facts::invalidate_legacy_projections(conn)?;
    }
    // library schema 10：一次性清理指向已删除台账行的探测工作项，并把因此
    // 空掉的活动任务收尾，避免旧库残留悬空 ledger 引用。
    if previous < 10 {
        crate::ledger_cleanup::repair_orphan_probe_units(conn)?;
    }
    read_revisions::migrate_read_revisions("library", conn)?;
    read_revisions::create_revision_trigger(conn, "ledger", "lib_ledger")?;
    read_revisions::create_revision_trigger(conn, "file_meta", "lib_file_meta")?;
    read_revisions::create_revision_trigger(conn, "media_markers", "lib_markers")?;
    ensure_column(
        conn,
        "browse_generations",
        "dirty_token",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    browse_changes::migrate_browse_changes_library(conn)?;
    set_schema_version(conn, LIBRARY_SCHEMA_VERSION)?;
    Ok(())
}

pub fn migrate_subscribe(conn: &Connection) -> Result<(), StoreError> {
    ensure_supported_versions(&[("subscribe", conn, SUBSCRIBE_SCHEMA_VERSION)])?;
    conn.execute_batch(tables::SUBSCRIBE_TABLES)?;
    migrate_playback_session_key(conn)?;
    // Old logs have no trustworthy device identity and remain nonrevocable.
    ensure_column(conn, "playback_logs", "device_id", "TEXT")?;
    ensure_column(conn, "playback_sessions", "ledger_id", "TEXT")?;
    ensure_column(
        conn,
        "subscribes",
        "tracking_state",
        "TEXT NOT NULL DEFAULT 'active'",
    )?;
    ensure_column(
        conn,
        "subscribes",
        "follow_future",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        conn,
        "subscribes",
        "search_interval_secs",
        "INTEGER NOT NULL DEFAULT 1800",
    )?;
    ensure_column(
        conn,
        "subscribes",
        "keep_old_versions",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(conn, "subscribes", "created_at", "TEXT NOT NULL DEFAULT ''")?;
    ensure_column(conn, "subscribes", "updated_at", "TEXT NOT NULL DEFAULT ''")?;
    ensure_column(conn, "subscribes", "library_id", "TEXT")?;
    ensure_column(
        conn,
        "pending_downloads",
        "submitted_at",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    // pending_downloads.state：'active'（在途）| 'imported'（已入库，保留为
    // 源链接——库文件缺失时据此重新硬链接）。必须在建表之后补列。
    ensure_column(
        conn,
        "pending_downloads",
        "state",
        "TEXT NOT NULL DEFAULT 'active'",
    )?;
    // Index on (subscribe_id, state) — must come after the state column exists.
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS pending_downloads_sub_state_idx \
         ON pending_downloads(subscribe_id, state);",
    )?;
    // 老库遗留的 playback_progress 表：数据并入 playback_units 后删除。
    migrate_legacy_playback_progress(conn)?;
    // subscribe schema 8：QoE recorded_at keyset 索引（DB-05）。
    data_retention::migrate_data_retention_subscribe(conn)?;
    // subscribe schema 4: playback_sessions 新增代际与毫秒字段，新增 playback_session_ids 表
    ensure_column(conn, "playback_sessions", "play_session_id", "TEXT")?;
    ensure_column(
        conn,
        "playback_sessions",
        "last_report_at_ms",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(conn, "playback_sessions", "last_event_sequence", "INTEGER")?;
    ensure_column(conn, "playback_logs", "admin_ended", "INTEGER NOT NULL DEFAULT 0")?;
    conn.execute_batch(
        "UPDATE playback_sessions SET last_report_at_ms = last_report_at * 1000 WHERE last_report_at_ms = 0;
         CREATE TABLE IF NOT EXISTS playback_session_ids (
             user_id TEXT NOT NULL,
             device_id TEXT NOT NULL,
             play_session_id TEXT NOT NULL,
             item_id TEXT NOT NULL,
             issued_at INTEGER NOT NULL,
             generation INTEGER NOT NULL,
             state TEXT NOT NULL,
             PRIMARY KEY (user_id, device_id, play_session_id)
         );
         CREATE INDEX IF NOT EXISTS playback_session_ids_expiry_idx ON playback_session_ids(issued_at);",
    )?;
    read_revisions::migrate_read_revisions("subscribe", conn)?;
    read_revisions::create_revision_trigger(conn, "playback_units", "sub_units")?;
    // subscribe schema 6: favourite activity has its own clock so a favourite
    // write cannot reorder played marks or invent a viewing timestamp.
    // Databases that already have that clock keep a zero as unknown time.
    // Only a database that predates the column, or is receiving it now, gets
    // the one-time backfill from the playback clock.
    let previous = read_schema_version(conn)?;
    let favorite_at_existed = column_exists(conn, "playback_units", "favorite_at")?;
    ensure_column(
        conn,
        "playback_units",
        "favorite_at",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    if previous < 6 || !favorite_at_existed {
        conn.execute_batch(
            "UPDATE playback_units SET favorite_at = updated_at
             WHERE favorite != 0 AND favorite_at = 0 AND updated_at != 0;",
        )?;
    }
    set_schema_version(conn, SUBSCRIBE_SCHEMA_VERSION)?;
    Ok(())
}

/// Drop the legacy `playback_progress` table after merging its rows into
/// `playback_units`. New installs never create it (it is absent from
/// `migrate_subscribe`'s initial batch), so this only fires on old databases.
fn migrate_legacy_playback_progress(conn: &Connection) -> Result<(), StoreError> {
    let has_table: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'playback_progress')",
        [],
        |row| row.get(0),
    )?;
    if !has_table {
        return Ok(());
    }
    // 旧表 (user_id, media_id) 主键 → 新表 (user_id, media_id, season, episode)；
    // 行键取 (-1, -1)（整部作品），已存在的行不覆盖（新表是权威）。
    conn.execute_batch(
        "INSERT OR IGNORE INTO playback_units (
             user_id, media_id, season, episode, position_ms, played, favorite, updated_at
         )
         SELECT user_id, media_id,
                COALESCE(season, -1), COALESCE(episode, -1),
                position_ms,
                COALESCE(played, 0),
                COALESCE(favorite, 0),
                0
         FROM playback_progress;
         DROP TABLE playback_progress;",
    )?;
    Ok(())
}

fn migrate_playback_session_key(conn: &Connection) -> Result<(), StoreError> {
    let mut primary_key: Vec<(i64, String)> = conn
        .prepare("PRAGMA table_info(playback_sessions)")?
        .query_map([], |row| {
            Ok((row.get::<_, i64>(5)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|(position, _)| *position > 0)
        .collect();
    primary_key.sort_by_key(|(position, _)| *position);
    let columns: Vec<&str> = primary_key.iter().map(|(_, name)| name.as_str()).collect();
    if columns == ["user_id", "device_id"] {
        return Ok(());
    }
    conn.execute_batch(
        r#"
        BEGIN IMMEDIATE;
        DROP TABLE IF EXISTS playback_sessions_new;
        CREATE TABLE playback_sessions_new (
            user_id TEXT NOT NULL,
            device_id TEXT NOT NULL,
            media_id TEXT NOT NULL,
            season INTEGER,
            episode INTEGER,
            client TEXT,
            device_name TEXT,
            client_version TEXT,
            play_method TEXT NOT NULL DEFAULT 'local',
            position_ms INTEGER NOT NULL DEFAULT 0,
            start_position_ms INTEGER NOT NULL DEFAULT 0,
            duration_ms INTEGER,
            paused INTEGER NOT NULL DEFAULT 0,
            watched_ms INTEGER NOT NULL DEFAULT 0,
            rate_bps INTEGER NOT NULL DEFAULT 0,
            bytes_sent INTEGER NOT NULL DEFAULT 0,
            connections INTEGER NOT NULL DEFAULT 0,
            admin_ended INTEGER NOT NULL DEFAULT 0,
            started_at INTEGER NOT NULL,
            last_report_at INTEGER NOT NULL,
            PRIMARY KEY (user_id, device_id)
        );
        INSERT INTO playback_sessions_new (
            user_id, device_id, media_id, season, episode, client, device_name,
            client_version, play_method, position_ms, start_position_ms, duration_ms,
            paused, watched_ms, rate_bps, bytes_sent, connections, admin_ended,
            started_at, last_report_at
        ) SELECT
            user_id, device_id, media_id, season, episode, client, device_name,
            client_version, play_method, position_ms, start_position_ms, duration_ms,
            paused, watched_ms, rate_bps, bytes_sent, connections, admin_ended,
            started_at, last_report_at
        FROM playback_sessions;
        DROP TABLE playback_sessions;
        ALTER TABLE playback_sessions_new RENAME TO playback_sessions;
        COMMIT;
        "#,
    )?;
    Ok(())
}

pub fn seed_defaults(app: &Connection, data_dir: &std::path::Path) -> Result<(), StoreError> {
    let movie_root = data_dir.join("library/movies");
    let tv_root = data_dir.join("library/tv");
    std::fs::create_dir_all(&movie_root)?;
    std::fs::create_dir_all(&tv_root)?;
    app.execute(
        "INSERT OR IGNORE INTO naming_templates (kind, pattern) VALUES (?1, ?2)",
        params![
            "movie",
            "{title} ({year})/{title} ({year}){part} - {resolution}{ext}"
        ],
    )?;
    app.execute(
        "INSERT OR IGNORE INTO naming_templates (kind, pattern) VALUES (?1, ?2)",
        params![
            "tv",
            "{title} ({year})/Season {season}/{title} - {season_episode}{part}{ext}"
        ],
    )?;
    let movie_id = uuid::Uuid::new_v4().to_string();
    let tv_id = uuid::Uuid::new_v4().to_string();
    app.execute(
        "INSERT OR IGNORE INTO library_roots (id, kind, path, is_default)
         SELECT ?1, 'movie', ?2, 1 WHERE NOT EXISTS (SELECT 1 FROM library_roots WHERE kind='movie')",
        params![movie_id, movie_root.display().to_string()],
    )?;
    app.execute(
        "INSERT OR IGNORE INTO library_roots (id, kind, path, is_default)
         SELECT ?1, 'tv', ?2, 1 WHERE NOT EXISTS (SELECT 1 FROM library_roots WHERE kind='tv')",
        params![tv_id, tv_root.display().to_string()],
    )?;
    let filter_id = uuid::Uuid::new_v4().to_string();
    // JSON atom form (see maps::serialize_atoms); the legacy DSL is still read.
    let atoms = r#"[{"priority":100,"rule":"resolution","exclude":false,"value":"2160p"},{"priority":50,"rule":"resolution","exclude":false,"value":"1080p"}]"#;
    app.execute(
        "INSERT INTO filters (id, name, atoms_json)
         SELECT ?1, '默认', ?2 WHERE NOT EXISTS (SELECT 1 FROM filters)",
        params![filter_id, atoms],
    )?;
    app.execute(
        "INSERT OR IGNORE INTO settings (key, value) VALUES ('default_filter_id', ?1)",
        params![filter_id],
    )?;
    seed_libraries(app)?;
    Ok(())
}

/// Backfill the `libraries` entity from `library_roots`: one default library per
/// kind, roots attached in current order (first root = primary). Idempotent —
/// existing libraries are left untouched, orphaned roots (library_id NULL) are
/// re-attached to their kind's default library.
fn seed_libraries(app: &Connection) -> Result<(), StoreError> {
    let kinds: Vec<String> = app
        .prepare("SELECT DISTINCT kind FROM library_roots")?
        .query_map([], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    for kind in kinds {
        let has_library: bool = app
            .prepare("SELECT 1 FROM libraries WHERE kind = ?1 LIMIT 1")?
            .query_row(params![kind], |_| Ok(()))
            .optional()?
            .is_some();
        if !has_library {
            let name = if kind == "tv" {
                "剧集库"
            } else {
                "电影库"
            };
            app.execute(
                "INSERT INTO libraries (id, kind, name, is_default, sort_order)
                 VALUES (?1, ?2, ?3, 1, 0)",
                params![uuid::Uuid::new_v4().to_string(), kind, name],
            )?;
        }
    }
    crate::library_defaults::ensure_library_defaults(app)?;
    Ok(())
}

pub const MEDIA_COLS: &str =
    "id, kind, title, year, original_title, tmdb_id, douban_id, tvdb_id, bangumi_id, anilist_id, aliases_json";
