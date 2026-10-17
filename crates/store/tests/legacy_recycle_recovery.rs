use domain::{Media, MediaId, MediaKind};
use serde_json::json;
use store::Store;

fn make_test_media(media_id: MediaId) -> Media {
    Media {
        id: media_id,
        kind: MediaKind::Movie,
        title: "The Matrix".into(),
        year: Some(1999),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: Some("603".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

fn init_legacy_db_table(conn: &rusqlite::Connection) {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS recycle_bin (
            id TEXT PRIMARY KEY,
            original_path TEXT NOT NULL,
            binned_path TEXT NOT NULL,
            ledger_json TEXT NOT NULL,
            binned_at INTEGER NOT NULL
        );",
    )
    .unwrap();
}

#[test]
fn legacy_recycle_restores_file_already_at_original_path_after_interruption() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let original_dir = tmp.path().join("movies/The Matrix (1999)");
    std::fs::create_dir_all(&original_dir).unwrap();
    let original_path = original_dir.join("The Matrix (1999).mkv");

    // 模拟文件已经在 original 路径就位（即上一轮已将文件移回 original，但在写入 ledger 前中断）
    std::fs::write(&original_path, b"matrix-content").unwrap();

    let media_id = MediaId::new();
    let media = make_test_media(media_id);

    let bin_id = "bin-123";
    let binned_path = tmp.path().join("recycle/binned.mkv");
    assert!(!binned_path.exists());

    let ledger_json = json!({
        "id": domain::LedgerId::new().to_string(),
        "media_id": media_id.to_string(),
        "path": original_path.display().to_string(),
        "season": null,
        "episode": null,
        "resolution": "1080p",
        "codec": null,
        "hdr": null,
        "quality_source": "release",
        "confidence": "high"
    })
    .to_string();

    let db_path = data_dir.join("library.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        init_legacy_db_table(&conn);
        conn.execute_batch(
            "ALTER TABLE recycle_bin ADD COLUMN recovered_path TEXT;
             ALTER TABLE recycle_bin ADD COLUMN recovery_stage TEXT;",
        )
        .ok();
        conn.execute(
            "INSERT INTO recycle_bin (id, original_path, binned_path, ledger_json, binned_at, recovered_path, recovery_stage)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                bin_id,
                original_path.display().to_string(),
                binned_path.display().to_string(),
                ledger_json,
                1000i64,
                original_path.display().to_string(),
                "published"
            ],
        )
        .unwrap();
    }

    let store = Store::open(&data_dir).unwrap();
    let _ = store.ensure_media(media).unwrap();

    let ledger = store.list_ledger().unwrap();
    assert_eq!(ledger.len(), 1, "应自动恢复 1 个 ledger 记录");
    assert_eq!(ledger[0].path, original_path.display().to_string());
}

#[test]
fn legacy_recycle_handles_hardlink_interruption_same_dev_ino() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let target_dir = tmp.path().join("movies/The Matrix (1999)");
    std::fs::create_dir_all(&target_dir).unwrap();
    let target_path = target_dir.join("The Matrix (1999).mkv");

    let recycle_dir = tmp.path().join("recycle");
    std::fs::create_dir_all(&recycle_dir).unwrap();
    let binned_path = recycle_dir.join("binned.mkv");

    // 创建 binned 文件并创建硬链接到 target_path（模拟 link 成功后但在 unlink 前崩溃）
    std::fs::write(&binned_path, b"matrix-hardlink-content").unwrap();
    std::fs::hard_link(&binned_path, &target_path).unwrap();

    let media_id = MediaId::new();
    let media = make_test_media(media_id);
    let bin_id = "bin-hardlink-1";

    let ledger_json = json!({
        "id": domain::LedgerId::new().to_string(),
        "media_id": media_id.to_string(),
        "path": target_path.display().to_string(),
        "season": null,
        "episode": null,
        "resolution": "1080p",
        "codec": null,
        "hdr": null,
        "quality_source": "release",
        "confidence": "high"
    })
    .to_string();

    let db_path = data_dir.join("library.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        init_legacy_db_table(&conn);
        conn.execute_batch(
            "ALTER TABLE recycle_bin ADD COLUMN recovered_path TEXT;
             ALTER TABLE recycle_bin ADD COLUMN recovery_stage TEXT;",
        )
        .ok();
        conn.execute(
            "INSERT INTO recycle_bin (id, original_path, binned_path, ledger_json, binned_at, recovered_path, recovery_stage)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                bin_id,
                target_path.display().to_string(),
                binned_path.display().to_string(),
                ledger_json,
                1000i64,
                target_path.display().to_string(),
                "published"
            ],
        )
        .unwrap();
    }

    let store = Store::open(&data_dir).unwrap();
    let _ = store.ensure_media(media).unwrap();

    let ledger = store.list_ledger().unwrap();
    assert_eq!(ledger.len(), 1, "应自动恢复 1 个 ledger 记录");
    assert_eq!(ledger[0].path, target_path.display().to_string());
    // binned_path 应该已被清理
    assert!(!binned_path.exists(), "中断恢复后应清理多余的 source 链接");
    assert!(target_path.exists(), "目标文件应保持存在");
}

#[test]
fn legacy_recycle_recovers_from_interrupted_partial_copy() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let target_dir = tmp.path().join("movies/The Matrix (1999)");
    std::fs::create_dir_all(&target_dir).unwrap();
    let target_path = target_dir.join("The Matrix (1999).mkv");

    let recycle_dir = tmp.path().join("recycle");
    std::fs::create_dir_all(&recycle_dir).unwrap();
    let binned_path = recycle_dir.join("binned.mkv");

    // 完整的源文件
    let full_content = b"complete-file-content-1234567890";
    std::fs::write(&binned_path, full_content).unwrap();

    // 模拟写入临时文件时中断：存在一个 .tmp 文件，内容不完整或处于 copied 阶段之前
    let staging_path = target_dir.join("The Matrix (1999).mkv.recycle-tmp");
    std::fs::write(&staging_path, b"partial").unwrap();

    let media_id = MediaId::new();
    let media = make_test_media(media_id);
    let bin_id = "bin-partial-copy";

    let ledger_json = json!({
        "id": domain::LedgerId::new().to_string(),
        "media_id": media_id.to_string(),
        "path": target_path.display().to_string(),
        "season": null,
        "episode": null,
        "resolution": "1080p",
        "codec": null,
        "hdr": null,
        "quality_source": "release",
        "confidence": "high"
    })
    .to_string();

    let db_path = data_dir.join("library.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        init_legacy_db_table(&conn);
        conn.execute_batch(
            "ALTER TABLE recycle_bin ADD COLUMN recovered_path TEXT;
             ALTER TABLE recycle_bin ADD COLUMN recovery_stage TEXT;",
        )
        .ok();
        conn.execute(
            "INSERT INTO recycle_bin (id, original_path, binned_path, ledger_json, binned_at, recovered_path, recovery_stage)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                bin_id,
                target_path.display().to_string(),
                binned_path.display().to_string(),
                ledger_json,
                1000i64,
                target_path.display().to_string(),
                "planned"
            ],
        )
        .unwrap();
    }

    let store = Store::open(&data_dir).unwrap();
    let _ = store.ensure_media(media).unwrap();

    let ledger = store.list_ledger().unwrap();
    assert_eq!(ledger.len(), 1, "应自动恢复 1 个 ledger 记录");
    assert_eq!(ledger[0].path, target_path.display().to_string());
    assert_eq!(std::fs::read(&target_path).unwrap(), full_content, "目标内容应完整");
    assert!(!staging_path.exists(), "临时文件应已被清理");
    assert!(!binned_path.exists(), "源文件应在完全成功后被清理");
}

#[test]
fn legacy_recycle_preserves_entry_when_target_occupied_by_other_file() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let target_dir = tmp.path().join("movies/The Matrix (1999)");
    std::fs::create_dir_all(&target_dir).unwrap();
    let target_path = target_dir.join("The Matrix (1999).mkv");

    // 目标路径被另外一个不同内容、不同 inode 的文件占据！
    let foreign_content = b"alien-content-do-not-touch";
    std::fs::write(&target_path, foreign_content).unwrap();

    let recycle_dir = tmp.path().join("recycle");
    std::fs::create_dir_all(&recycle_dir).unwrap();
    let binned_path = recycle_dir.join("binned.mkv");
    let original_bin_content = b"original-recycled-content";
    std::fs::write(&binned_path, original_bin_content).unwrap();

    let media_id = MediaId::new();
    let bin_id = "bin-occupied-1";

    let ledger_json = json!({
        "id": domain::LedgerId::new().to_string(),
        "media_id": media_id.to_string(),
        "path": target_path.display().to_string(),
        "season": null,
        "episode": null,
        "resolution": "1080p",
        "codec": null,
        "hdr": null,
        "quality_source": "release",
        "confidence": "high"
    })
    .to_string();

    let db_path = data_dir.join("library.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        init_legacy_db_table(&conn);
        conn.execute_batch(
            "ALTER TABLE recycle_bin ADD COLUMN recovered_path TEXT;
             ALTER TABLE recycle_bin ADD COLUMN recovery_stage TEXT;",
        )
        .ok();
        // 模拟已计划或者恢复阶段中指向 target_path，但 target_path 实际被别人占用了
        conn.execute(
            "INSERT INTO recycle_bin (id, original_path, binned_path, ledger_json, binned_at, recovered_path, recovery_stage)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                bin_id,
                target_path.display().to_string(),
                binned_path.display().to_string(),
                ledger_json,
                1000i64,
                target_path.display().to_string(),
                "published"
            ],
        )
        .unwrap();
    }

    let store = Store::open(&data_dir).unwrap();

    // 目标文件必须不被覆盖！
    assert_eq!(std::fs::read(&target_path).unwrap(), foreign_content);
    // binned 文件不能被删
    assert!(binned_path.exists());

    // 记录必须保留在 recycle_bin 中，不被删除
    let db_conn = rusqlite::Connection::open(&db_path).unwrap();
    let count: i64 = db_conn
        .query_row("SELECT COUNT(*) FROM recycle_bin WHERE id = ?1", rusqlite::params![bin_id], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1, "占用冲突时必须保留 recycle_bin 记录供人工介入");

    // ledger 也不应当错误登记这个非本源的文件
    let ledger = store.list_ledger().unwrap();
    assert!(ledger.is_empty(), "不应将外来占据的文件错误登记进 ledger");
}

#[test]
fn legacy_recycle_propagates_schema_alter_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let db_path = data_dir.join("library.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        // 创建只读视图或触发 ALTER 失败的条件，或者通过锁定使 ALTER 失败
        // 例如创建名为 recycle_bin 的 VIEW，而不是 TABLE！
        // 在 SQLite 中，对 VIEW 执行 ALTER TABLE 会直接报错：cannot alter view
        conn.execute_batch(
            "CREATE VIEW recycle_bin AS SELECT 1 AS id;",
        )
        .unwrap();
    }

    let result = Store::open(&data_dir);
    assert!(result.is_err(), "schema 操作或 ALTER 失败必须向上传播，不能吞掉错误");
}
