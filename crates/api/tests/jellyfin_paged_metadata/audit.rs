//! A3 审计回归：未知 folder + SeriesId、剧 parent 的 SQL 下推、person catalog
//! 失败、favourite 观察一致性、工作量计数、页外重读失败、选页前不得探测。

use std::sync::atomic::Ordering;
use std::sync::Arc;

use axum::http::StatusCode;
use domain::{MediaId, MediaKind};
use media::ItemMeta;

use super::*;

struct SelectiveCatalog {
    fail: String,
    actor: String,
}

impl api::catalog::Catalog for SelectiveCatalog {
    fn search_movie(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> { Ok(Vec::new()) }
    fn search_tv(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> { Ok(Vec::new()) }
    fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> { Ok(Vec::new()) }
    fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> { Ok(Vec::new()) }
    fn details(&self, _: MediaKind, _: &str) -> Result<Option<domain::Media>, String> { Ok(None) }
    fn metadata_with_preferences(
        &self,
        _kind: MediaKind,
        tmdb_id: &str,
        _: &[String],
        _: &[String],
    ) -> Result<Option<ItemMeta>, String> {
        if tmdb_id == self.fail {
            return Err("catalog unavailable".into());
        }
        Ok(Some(ItemMeta {
            cast: vec![media::CastRow {
                name: self.actor.clone(),
                role: Some("Lead".into()),
                person_id: Some(42),
                ..Default::default()
            }],
            ..Default::default()
        }))
    }
}

fn set_tmdb(state: &ApiState, title_contains: &str, tmdb_id: &str) {
    let store = state.store();
    let store = store.lock();
    for row in store.list_ledger().unwrap() {
        let Some(mut media) = store.get_media(row.media_id).unwrap() else {
            continue;
        };
        if media.title.contains(title_contains) {
            media.tmdb_id = Some(tmdb_id.into());
            store.update_media(&media).unwrap();
        }
    }
}

#[tokio::test]
async fn unknown_folder_plus_series_id_returns_no_episodes() {
    let fixture = tv_fixture(1, 2);
    let app = router(fixture.state.clone());
    let show = media_id_for_title(&fixture.state, "Show000");
    let listing = get_json(
        &app,
        &format!(
            "/Items?ParentId={}&SeriesId={show}&Recursive=true",
            folder_id(Path::new("/nonexistent-root"))
        ),
    )
    .await;
    assert_eq!(listing["TotalRecordCount"], 0, "{listing}");
    assert!(items(&listing).is_empty(), "未知 folder 不得带回别的分集：{listing}");
}

#[tokio::test]
async fn series_parent_does_not_scan_the_root_ledger() {
    let fixture = tv_fixture(8, 2);
    let app = router(fixture.state.clone());
    let show = media_id_for_title(&fixture.state, "Show000");
    {
        let conn = rusqlite::Connection::open(fixture.dir.path().join("data/library.db")).unwrap();
        let foreign = {
            let store = fixture.state.store();
            let store = store.lock();
            store
                .list_ledger()
                .unwrap()
                .into_iter()
                .find(|row| row.path.contains("Show001"))
                .expect("unrelated row")
                .id
        };
        conn.execute(
            "UPDATE ledger SET quality_source = 'not-a-source' WHERE id = ?1",
            [foreign.to_string()],
        )
        .unwrap();
    }
    fixture.state.store().lock().reset_root_ledger_scans();
    let listing = get_json(
        &app,
        &format!("/Items?ParentId={show}&Recursive=true&Limit=1&SortBy=SortName"),
    )
    .await;
    let scans = fixture.state.store().lock().root_ledger_scans();
    assert_eq!(listing["TotalRecordCount"], 2, "坏掉的无关行不得让这部剧失败：{listing}");
    assert_eq!(items(&listing).len(), 1);
    assert_eq!(scans, 0, "剧 parent 不得执行无 WHERE 的根账本扫描，实际 {scans} 次");
}

#[tokio::test]
async fn person_filter_fails_when_required_catalog_fails() {
    let fixture = tv_fixture(2, 1);
    set_tmdb(&fixture.state, "Show000", "100");
    set_tmdb(&fixture.state, "Show001", "200");
    let state = fixture
        .state
        .clone()
        .with_catalog(Arc::new(SelectiveCatalog {
            fail: "200".into(),
            actor: "Catalog Alice".into(),
        }));
    let app = router(state);
    let person = uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_URL,
        b"tmdb:42:Actor",
    )
    .to_string();
    let (status, body) = get(
        &app,
        &format!(
            "/Items?ParentId={}&Recursive=true&IncludeItemTypes=Series&PersonIds={person}",
            fixture.library_id
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "必需的 catalog 失败不得变成更小的 200：{body}"
    );
}

#[tokio::test]
async fn favorite_filter_uses_the_same_observation_as_the_response() {
    let fixture = tv_fixture(1, 1);
    let app = router(fixture.state.clone());
    let user = fixture.state.store().lock().admin_user_id().unwrap();
    let media = {
        let store = fixture.state.store();
        let store = store.lock();
        let row = store.list_ledger().unwrap().into_iter().next().expect("row");
        store
            .upsert_unit(user, row.media_id, -1, -1, 0, None, Some(true), None, None, None, false, 10)
            .unwrap();
        row.media_id
    };
    fixture.files.fail_store_on_first_io(Box::new({
        let state = fixture.state.clone();
        move || {
            let store = state.store();
            let store = store.lock();
            store
                .upsert_unit(user, media, -1, -1, 0, None, Some(false), None, None, None, false, 11)
                .unwrap();
        }
    }));
    let listing = get_json(
        &app,
        &format!(
            "/Items?ParentId={}&Recursive=true&IncludeItemTypes=Series&IsFavorite=true&SortBy=DateCreated",
            fixture.library_id
        ),
    )
    .await;
    assert_eq!(listing["TotalRecordCount"], 1, "{listing}");
    assert_eq!(
        items(&listing)[0]["UserData"]["IsFavorite"],
        true,
        "过滤用的 favourite 必须就是响应里的 favourite：{listing}"
    );
}

#[tokio::test]
async fn heavy_counter_records_a_completed_snapshot_before_a_later_failure() {
    let fixture = tv_fixture(2, 1);
    let heavy = fixture.state.jellyfin_heavy_items();
    let app = router(fixture.state.clone());
    let failing = {
        let store = fixture.state.store();
        let store = store.lock();
        store
            .list_ledger()
            .unwrap()
            .into_iter()
            .find(|row| row.path.contains("Show001"))
            .expect("Show001")
            .id
    };
    fixture.files.fail_store_on_first_io(Box::new({
        let db = fixture.dir.path().join("data/library.db");
        move || {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute_batch("DROP TABLE file_meta").unwrap();
        }
    }));
    let (status, _) = get(
        &app,
        &format!(
            "/Items?ParentId={}&Recursive=true&IncludeItemTypes=Series&SortBy=SortName&SortOrder=Ascending&Limit=2",
            fixture.library_id
        ),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        heavy.load(Ordering::SeqCst) >= 1,
        "后一行失败前，已经建完的重元数据必须记上，实际 {}",
        heavy.load(Ordering::SeqCst)
    );
}

#[tokio::test]
async fn person_filter_does_not_assess_probes_for_unselected_rows() {
    let fixture = tv_fixture(4, 3);
    let before = fixture.state.probe.assessments.load(Ordering::SeqCst);
    let app = router(fixture.state.clone());
    {
        let store = fixture.state.store();
        let store = store.lock();
        let row = store
            .list_ledger()
            .unwrap()
            .into_iter()
            .find(|row| row.path.contains("Show000"))
            .expect("Show000");
        let dir = Path::new(&row.path).parent().unwrap();
        std::fs::write(
            dir.join("tvshow.nfo"),
            "<tvshow><actor><name>Alice</name><role>Lead</role></actor></tvshow>",
        )
        .unwrap();
    }
    let person = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, b"name:alice:Actor").to_string();
    let listing = get_json(
        &app,
        &format!(
            "/Items?ParentId={}&Recursive=true&IncludeItemTypes=Series&PersonIds={person}&Limit=1",
            fixture.library_id
        ),
    )
    .await;
    assert_eq!(listing["TotalRecordCount"], 1, "{listing}");
    let assessed = fixture.state.probe.assessments.load(Ordering::SeqCst) - before;
    assert_eq!(
        assessed, 0,
        "person 过滤不得在选页前为未选中的分集做探测评估，实际 {assessed}"
    );
}

#[tokio::test]
async fn off_page_blob_chapters_fail_only_when_that_row_is_selected() {
    let fixture = tv_fixture(4, 1);
    let app = router(fixture.state.clone());
    let failing = {
        let store = fixture.state.store();
        let store = store.lock();
        store
            .list_ledger()
            .unwrap()
            .into_iter()
            .find(|row| row.path.contains("Show000"))
            .expect("Show000")
            .id
    };
    let conn = rusqlite::Connection::open(fixture.dir.path().join("data/library.db")).unwrap();
    conn.execute(
        "INSERT INTO file_meta (ledger_id, audio_json, subtitle_json, chapters_json)
         VALUES (?1, '[]', '[]', x'0102')",
        [failing.to_string()],
    )
    .unwrap();
    drop(conn);
    let other = get_json(
        &app,
        &format!(
            "/Items?ParentId={}&Recursive=true&IncludeItemTypes=Series&SortBy=SortName&Limit=1",
            fixture.library_id
        ),
    )
    .await;
    assert_eq!(other["TotalRecordCount"], 4);
    assert_eq!(items(&other)[0]["Name"], "Show003");
    let (status, body) = get(
        &app,
        &format!(
            "/Items?ParentId={}&Recursive=true&IncludeItemTypes=Series&SortBy=SortName&SortOrder=Ascending&Limit=1",
            fixture.library_id
        ),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "选中损坏行必须失败：{body}");
    let _ = MediaId::new();
}
