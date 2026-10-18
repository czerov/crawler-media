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
fn v7_database_upgrades_and_existing_sites_stay_http() {
    let dir = tempdir().unwrap();
    // Build a current DB, then strip it back to the v7 shape (no maintenance_via).
    drop(Store::open(dir.path()).unwrap());
    let app_path = dir.path().join("app.db");
    {
        let conn = Connection::open(&app_path).unwrap();
        conn.execute_batch("ALTER TABLE sites DROP COLUMN maintenance_via;").unwrap();
        conn.execute(
            "INSERT INTO sites (id, name, url, profile_id, enabled) VALUES (?1, 'old', 'https://old.example/', 'demo', 1)",
            [domain::SiteId::new().to_string()],
        )
        .unwrap();
        conn.execute("INSERT OR REPLACE INTO settings (key, value) VALUES ('keep', 'legacy_data')", []).unwrap();
        conn.pragma_update(None, "user_version", 7).unwrap();
    }

    let store = Store::open(dir.path()).expect("v7 database must upgrade, not be rejected");
    let sites = store.list_sites().unwrap();
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].maintenance_via, domain::MaintenanceVia::Http);
    assert_eq!(store.get_setting("keep").unwrap().as_deref(), Some("legacy_data"));
    drop(store);
    let conn = Connection::open(&app_path).unwrap();
    let v: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
    assert_eq!(v, 8, "app schema bumped to 8 by the maintenance_via migration");
}
