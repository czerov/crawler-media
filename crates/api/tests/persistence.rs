use api::Store;
use domain::{
    Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource, Site, SiteId,
};

fn movie(title: &str) -> Media {
    Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: title.to_string(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: Some("603".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

fn site() -> Site {
    Site {
        id: SiteId::new(),
        name: "demo".into(),
        url: "https://pt.example/".into(),
        profile_id: "demo".into(),
        cookie: Some("uid=1; pass=abc".into()),
        api_key: None,
        rss_url: Some("https://pt.example/rss".into()),
        proxy: None,
        rate_limit_per_minute: Some(12),
        cdp_url: None,
        downloader_id: None,
        enabled: true,
        maintenance_via: domain::MaintenanceVia::Http,
    }
}

#[test]
fn sqlite_defaults_under_the_data_directory() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    assert_eq!(store.sqlite_path(), dir.path().join("app.db"));
    for name in ["app.db", "catalog.db", "library.db", "subscribe.db"] {
        assert!(dir.path().join(name).is_file(), "{name}");
    }
}

#[test]
fn sqlite_path_can_be_overridden() {
    let dir = tempfile::tempdir().unwrap();
    let sqlite = dir.path().join("custom.db");
    let store = Store::open_at(dir.path(), &sqlite).unwrap();
    assert_eq!(store.sqlite_path(), sqlite);
    assert!(sqlite.is_file());
}

#[test]
fn media_round_trips_with_nullable_catalog_aliases() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let media = movie("The Matrix");

    store.insert_media(&media).unwrap();
    let got = store.get_media(media.id).unwrap().expect("media row");

    assert_eq!(got, media);
    assert!(got.douban_id.is_none());
    assert!(got.tvdb_id.is_none());
    assert!(got.bangumi_id.is_none());
    assert!(got.anilist_id.is_none());
}

#[test]
fn list_media_returns_error_on_corrupt_uuid_instead_of_panic() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let media = movie("The Matrix");
    store.insert_media(&media).unwrap();
    rusqlite::Connection::open(dir.path().join("app.db"))
        .unwrap()
        .execute("UPDATE media SET id = 'nope'", [])
        .unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        store.get_media_by_tmdb("603")
    }));
    assert!(result.is_ok(), "must not panic");
    assert!(result.unwrap().is_err());
}

#[test]
fn fresh_store_seeds_a_default_filter() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let filters = store.list_filters().unwrap();
    assert!(!filters.is_empty(), "fresh store should seed one Filter");
    let setting = store
        .get_setting("default_filter_id")
        .unwrap()
        .expect("default_filter_id setting");
    assert!(
        filters.iter().any(|f| f.id.to_string() == setting),
        "seeded Filter must be the default"
    );
}

#[test]
fn second_lookup_by_tmdb_id_reuses_the_media_row() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let media = movie("The Matrix");
    store.insert_media(&media).unwrap();
    let again = store.get_media_by_tmdb("603").unwrap().expect("tmdb alias");
    assert_eq!(again.id, media.id);
    assert!(again.douban_id.is_none());
}

#[test]
fn douban_and_tmdb_aliases_share_one_media_id() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let first = store
        .ensure_media(Media {
            id: MediaId::new(),
            kind: MediaKind::Movie,
            title: "The Matrix".into(),
            year: None,
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: Some("603".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let second = store
        .ensure_media(Media {
            id: MediaId::new(),
            kind: MediaKind::Movie,
            title: "黑客帝国".into(),
            year: None,
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: Some("603".into()),
            douban_id: Some("1291843".into()),
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    assert_eq!(first.id, second.id);
    assert_eq!(second.douban_id.as_deref(), Some("1291843"));
    assert_eq!(second.tmdb_id.as_deref(), Some("603"));
}

#[test]
fn site_credentials_round_trip_as_plaintext() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let site = site();

    store.insert_site(&site).unwrap();
    let got = store.get_site(site.id).unwrap().expect("site row");

    assert_eq!(got, site);
    assert_eq!(got.cookie.as_deref(), Some("uid=1; pass=abc"));
    assert!(got.api_key.is_none());
}

#[test]
fn media_round_trips_year_and_original_title() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let mut media = movie("The Matrix");
    media.year = Some(1999);
    media.original_title = Some("The Matrix".into());
    store.insert_media(&media).unwrap();
    let got = store.get_media(media.id).unwrap().unwrap();
    assert_eq!(got.year, Some(1999));
    assert_eq!(got.original_title.as_deref(), Some("The Matrix"));
}

#[test]
fn naming_templates_and_library_roots_have_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    assert!(
        store
            .naming_template(MediaKind::Movie)
            .unwrap()
            .contains("{title}")
    );
    assert!(
        store
            .naming_template(MediaKind::Tv)
            .unwrap()
            .contains("Season")
    );
    assert!(
        store
            .library_root(MediaKind::Movie)
            .unwrap()
            .ends_with("movies")
    );
    assert!(store.library_root(MediaKind::Tv).unwrap().ends_with("tv"));
}

#[test]
fn legacy_libraries_table_gains_visibility_columns() {
    // 模拟 Block 6 之前的旧库：libraries 表没有 access_mode 等列。
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let app = rusqlite::Connection::open(data.join("app.db")).unwrap();
    app.execute_batch(
        "CREATE TABLE libraries (
            id TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            name TEXT NOT NULL,
            is_default INTEGER NOT NULL DEFAULT 0,
            sort_order INTEGER NOT NULL DEFAULT 0
        );
        INSERT INTO libraries (id, kind, name, is_default) VALUES ('lib-1', 'movie', '旧电影库', 1);",
    )
    .unwrap();
    drop(app);

    let store = crate::Store::open(&data).unwrap();
    let libraries = store.list_libraries().unwrap();
    // seed_defaults 会为缺省类型补默认库，旧电影库 + 新剧集库 = 2。
    assert_eq!(libraries.len(), 2, "旧库行保留且默认库补齐");
    let legacy = libraries
        .iter()
        .find(|l| l.name == "旧电影库")
        .expect("旧库行");
    assert_eq!(legacy.access_mode, "everyone", "新列按默认值补齐");
    assert_eq!(legacy.admin_visible, true);
    assert!(legacy.member_ids.is_empty());
}

#[test]
fn legacy_recycle_bin_is_restored_before_its_table_is_removed() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let original = data.join("library/movies/legacy.mkv");
    let binned = data.join("recycle_bin/legacy.mkv");
    std::fs::create_dir_all(binned.parent().unwrap()).unwrap();
    std::fs::write(&binned, b"legacy file").unwrap();
    let ledger_id = LedgerId::new();
    let media_id = MediaId::new();
    let ledger_json = serde_json::json!({
        "id": ledger_id.to_string(), "media_id": media_id.to_string(),
        "path": original.display().to_string(), "season": null, "episode": null,
        "resolution": "1080p", "codec": null, "hdr": null,
        "quality_source": QualitySource::Release.as_str(),
        "confidence": Confidence::High.as_str(), "filter_score": null,
    })
    .to_string();
    let library = rusqlite::Connection::open(data.join("library.db")).unwrap();
    library
        .execute_batch(
            "CREATE TABLE recycle_bin (
            id TEXT PRIMARY KEY, media_id TEXT NOT NULL, original_path TEXT NOT NULL,
            binned_path TEXT NOT NULL, ledger_json TEXT NOT NULL, reason TEXT NOT NULL,
            size_bytes INTEGER NOT NULL, deleted_at INTEGER NOT NULL, expires_at INTEGER NOT NULL
        );",
        )
        .unwrap();
    library.execute(
        "INSERT INTO recycle_bin
         (id, media_id, original_path, binned_path, ledger_json, reason, size_bytes, deleted_at, expires_at)
         VALUES ('legacy-bin', ?1, ?2, ?3, ?4, 'cleanup', 11, 1, 2)",
        rusqlite::params![media_id.to_string(), original.display().to_string(), binned.display().to_string(), ledger_json],
    ).unwrap();
    drop(library);
    let store = Store::open(&data).unwrap();
    assert!(
        original.is_file(),
        "legacy bytes are restored to their original path"
    );
    assert!(!binned.exists());
    assert_eq!(
        store
            .get_ledger(&ledger_id.to_string().replace('-', ""))
            .unwrap()
            .unwrap()
            .path,
        original.display().to_string()
    );
    drop(store);
    let library = rusqlite::Connection::open(data.join("library.db")).unwrap();
    let table_exists: bool = library.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'recycle_bin')",
        [], |row| row.get(0),
    ).unwrap();
    assert!(
        !table_exists,
        "migration only removes the old table after recovery"
    );
}

#[test]
fn legacy_recycle_restore_never_overwrites_an_existing_recovery_file() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let original = data.join("library/movies/legacy.mkv");
    let binned = data.join("recycle_bin/legacy.mkv");
    let bin_id = "legacy-bin";
    let existing_recovery = data.join("library/movies/legacy.recovered-legacy-bin.mkv");
    std::fs::create_dir_all(binned.parent().unwrap()).unwrap();
    std::fs::create_dir_all(original.parent().unwrap()).unwrap();
    std::fs::write(&original, b"existing original").unwrap();
    std::fs::write(&existing_recovery, b"existing recovery").unwrap();
    std::fs::write(&binned, b"legacy file").unwrap();
    let ledger_id = LedgerId::new();
    let media_id = MediaId::new();
    let ledger_json = serde_json::json!({
        "id": ledger_id.to_string(), "media_id": media_id.to_string(),
        "path": original.display().to_string(), "season": null, "episode": null,
        "resolution": "1080p", "codec": null, "hdr": null,
        "quality_source": QualitySource::Release.as_str(),
        "confidence": Confidence::High.as_str(), "filter_score": null,
    })
    .to_string();
    let library = rusqlite::Connection::open(data.join("library.db")).unwrap();
    library
        .execute_batch(
            "CREATE TABLE recycle_bin (
            id TEXT PRIMARY KEY, media_id TEXT NOT NULL, original_path TEXT NOT NULL,
            binned_path TEXT NOT NULL, ledger_json TEXT NOT NULL, reason TEXT NOT NULL,
            size_bytes INTEGER NOT NULL, deleted_at INTEGER NOT NULL, expires_at INTEGER NOT NULL
        );",
        )
        .unwrap();
    library.execute(
        "INSERT INTO recycle_bin
         (id, media_id, original_path, binned_path, ledger_json, reason, size_bytes, deleted_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'cleanup', 11, 1, 2)",
        rusqlite::params![bin_id, media_id.to_string(), original.display().to_string(), binned.display().to_string(), ledger_json],
    ).unwrap();
    drop(library);

    let store = Store::open(&data).unwrap();
    let restored = store
        .get_ledger(&ledger_id.to_string().replace('-', ""))
        .unwrap()
        .unwrap();
    assert_eq!(std::fs::read(&original).unwrap(), b"existing original");
    assert_eq!(
        std::fs::read(&existing_recovery).unwrap(),
        b"existing recovery"
    );
    assert_ne!(restored.path, existing_recovery.display().to_string());
    assert_eq!(std::fs::read(&restored.path).unwrap(), b"legacy file");
}

#[test]
fn ledger_keeps_its_source_path_for_manual_retransfer() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let media = movie("The Matrix");
    store.insert_media(&media).unwrap();
    let source = dir.path().join("downloads/matrix.mkv");
    let destination = dir.path().join("library/matrix.mkv");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
    std::fs::write(&source, b"source").unwrap();
    std::fs::write(&destination, b"destination").unwrap();
    let row = LedgerRow {
        id: LedgerId::new(),
        media_id: media.id,
        path: destination.display().to_string(),
        season: None,
        episode: None,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    };

    store.insert_ledger_with_source(&row, &source).unwrap();

    let source_path = store
        .list_ledger_with_mode()
        .unwrap()
        .into_iter()
        .find(|(saved, _, _)| saved.id == row.id)
        .and_then(|(_, _, source_path)| source_path);
    assert_eq!(source_path.as_deref(), source.to_str());
}

#[test]
fn stale_stream_cache_is_invalidated_for_detailed_probe_fields() {
    let dir = tempfile::tempdir().unwrap();
    let old_video = library::VideoTrack {
        codec: Some("h264".into()),
        width: Some(1920),
        height: Some(1080),
        ..Default::default()
    };
    let legacy_db = rusqlite::Connection::open(dir.path().join("library.db")).unwrap();
    legacy_db
        .execute_batch(
            "CREATE TABLE file_meta (
                ledger_id TEXT PRIMARY KEY,
                audio_json TEXT NOT NULL,
                subtitle_json TEXT NOT NULL,
                video_json TEXT
            )",
        )
        .unwrap();
    legacy_db
        .execute(
            "INSERT INTO file_meta (ledger_id, audio_json, subtitle_json, video_json)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                "legacy-ledger",
                "[]",
                "[]",
                serde_json::to_string(&old_video).unwrap(),
            ],
        )
        .unwrap();
    drop(legacy_db);

    let store = Store::open(dir.path()).unwrap();
    assert_eq!(store.get_file_meta("legacy-ledger").unwrap(), None);

    let refreshed_tracks = library::Tracks {
        video: Some(library::VideoTrack {
            codec: Some("h264".into()),
            width: Some(1920),
            height: Some(1080),
            ..Default::default()
        }),
        ..Default::default()
    };
    store
        .put_file_meta("legacy-ledger", &refreshed_tracks)
        .unwrap();
    assert_eq!(
        store.get_file_meta("legacy-ledger").unwrap(),
        Some(refreshed_tracks)
    );
}
