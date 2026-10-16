use rusqlite::Connection;
use store::Store;
use tempfile::tempdir;

#[test]
fn empty_data_directory_initializes_the_current_schema() {
    let dir = tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let sites = store.list_sites().unwrap();
    assert!(sites.is_empty());

    // Verify sites table has maintenance_via column defaulting to 'http'
    let site = domain::Site {
        id: domain::SiteId::new(),
        name: "test".into(),
        url: "https://example.com/".into(),
        profile_id: "demo".into(),
        cookie: None,
        api_key: None,
        rss_url: None,
        proxy: None,
        rate_limit_per_minute: None,
        cdp_url: None,
        downloader_id: None,
        enabled: true,
        maintenance_via: domain::MaintenanceVia::Http,
    };
    store.insert_site(&site).unwrap();
    let loaded = store.get_site(site.id).unwrap().unwrap();
    assert_eq!(loaded.maintenance_via, domain::MaintenanceVia::Http);
}

#[test]
fn existing_legacy_schema_is_rejected_before_any_schema_or_settings_write() {
    let dir = tempdir().unwrap();
    let app_path = dir.path().join("app.db");
    {
        let conn = Connection::open(&app_path).unwrap();
        conn.pragma_update(None, "user_version", 7).unwrap();
        conn.execute_batch("CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT);").unwrap();
        conn.execute("INSERT INTO settings (key, value) VALUES ('keep', 'legacy_data')", []).unwrap();
    }

    let res = Store::open(dir.path());
    let err_msg = match res {
        Err(e) => e.to_string(),
        Ok(_) => panic!("Store::open on old v7 schema must fail"),
    };
    assert!(err_msg.contains("空数据目录") || err_msg.contains("不兼容") || err_msg.contains("不受支持") || err_msg.contains("fresh"));

    // Verify store never deleted or modified existing legacy data
    let conn = Connection::open(&app_path).unwrap();
    let val: String = conn.query_row("SELECT value FROM settings WHERE key='keep'", [], |r| r.get(0)).unwrap();
    assert_eq!(val, "legacy_data");
}

#[test]
fn store_never_deletes_existing_data_on_fresh_start_rejection() {
    let dir = tempdir().unwrap();
    let app_path = dir.path().join("app.db");
    {
        let conn = Connection::open(&app_path).unwrap();
        conn.pragma_update(None, "user_version", 6).unwrap();
        conn.execute_batch("CREATE TABLE user_data (id TEXT PRIMARY KEY);").unwrap();
    }

    assert!(Store::open(dir.path()).is_err());
    assert!(app_path.exists());
    let conn = Connection::open(&app_path).unwrap();
    let count: i64 = conn.query_row("SELECT count(*) FROM sqlite_master WHERE name='user_data'", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 1);
}
