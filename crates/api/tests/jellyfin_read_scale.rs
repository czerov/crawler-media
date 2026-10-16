use std::path::Path;
use std::sync::Arc;

use api::media_server_provider::ApiServerProvider;
use api::{ApiState, Store, router};
use axum::body::{Body, to_bytes};
use axum::http::Request;
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use media_server::provider::MediaServerProvider;
use serde_json::Value;
use tower::ServiceExt;
struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> { Err(IndexerError::Fetch("unused".into())) }
}

fn seed(path: &Path, unrelated: usize) -> (ApiState, domain::UserId, String) {
    let store = Store::open(path.join("data")).unwrap();
    // 目标行放在**非默认**库里：默认库的 `NOT (...)` 回退按语义必须扫全表，
    // 只有非默认库的作用域读才能证明「读不随无关行增长」。
    let target_root = path.join("target-root");
    std::fs::create_dir_all(&target_root).unwrap();
    let library = store
        .create_library(
            MediaKind::Movie,
            "Target",
            &[target_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let parent_root = library.root_paths.first().cloned().unwrap_or_else(|| path.join("media"));
    std::fs::create_dir_all(&parent_root).unwrap();
    let target = parent_root.join("target.mkv");
    std::fs::write(&target, b"target").unwrap();
    let media = Media { id: MediaId::new(), kind: MediaKind::Movie, title: "Target".into(), year: Some(2024), original_title: None, tmdb_id: None, douban_id: None, tvdb_id: None, bangumi_id: None, anilist_id: None };
    store.insert_media(&media).unwrap();
    let row = LedgerRow { id: LedgerId::new(), media_id: media.id, path: target.display().to_string(), season: None, episode: None, resolution: None, codec: None, hdr: None, quality_source: QualitySource::Probe, confidence: Confidence::High, filter_score: None };
    store.insert_ledger(&row).unwrap();
    for index in 0..unrelated {
        let outside = Media { id: MediaId::new(), kind: MediaKind::Movie, title: format!("Outside {index}"), year: None, original_title: None, tmdb_id: None, douban_id: None, tvdb_id: None, bangumi_id: None, anilist_id: None };
        store.insert_media(&outside).unwrap();
        store.insert_ledger(&LedgerRow { id: LedgerId::new(), media_id: outside.id, path: format!("{}/outside/unrelated-{index}.mkv", path.display()), season: None, episode: None, resolution: None, codec: None, hdr: None, quality_source: QualitySource::Probe, confidence: Confidence::High, filter_score: None }).unwrap();
    }
    let state = ApiState::new(store, "management-secret".into(), ProfileSet::load(None).unwrap(), Arc::new(NoFetch), Arc::new(MemoryDownloader::new(path.join("stage"))), path.join("library")).unwrap();
    let user = state.store().lock().user_id_by_token("management-secret").unwrap().unwrap();
    (state, user, library.id)
}

#[tokio::test]
async fn parent_scope_returns_same_protocol_items_with_unrelated_rows_present() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, user_id, parent_id) = seed(tmp.path(), 40);
    let provider = ApiServerProvider::new(state);
    let items = provider.list_visible_items(user_id, Some(&parent_id)).await.unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].media.title, "Target");
}

#[tokio::test]
async fn parent_scope_protocol_json_is_stable_with_unrelated_growth() {
    let small = tempfile::tempdir().unwrap();
    let (small_state, _, small_parent) = seed(small.path(), 0);
    let large = tempfile::tempdir().unwrap();
    let (large_state, _, large_parent) = seed(large.path(), 100);
    let mut small_json = parent_items_json(small_state, &small_parent).await;
    let mut large_json = parent_items_json(large_state, &large_parent).await;
    for response in [&mut small_json, &mut large_json] {
        let item = &mut response["Items"][0];
        item["Id"] = Value::String("target-item".into());
        item["Path"] = Value::String("target-path".into());
        item["ParentId"] = Value::String("target-library".into());
        item["DateCreated"] = Value::Null;
        item["UserData"]["ItemId"] = Value::String("target-item".into());
        item["UserData"]["Key"] = Value::String("target-item".into());
    }
    assert!(small_json["Items"].as_array().is_some_and(|items| items.iter().any(|item| item["Name"] == "Target")));
    assert!(large_json["Items"].as_array().is_some_and(|items| items.iter().any(|item| item["Name"] == "Target")));
    assert_eq!(small_json, large_json);
}

async fn parent_items_json(state: ApiState, parent_id: &str) -> Value {
    let request = Request::builder()
        .method("GET")
        .uri(format!("/Items?ParentId={parent_id}"))
        .header("authorization", "Bearer management-secret")
        .body(Body::empty())
        .unwrap();
    let response = router(state).oneshot(request).await.unwrap();
    assert!(response.status().is_success());
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[test]
fn fixed_parent_sql_candidate_work_ignores_unrelated_rows() {
    let small = tempfile::tempdir().unwrap();
    let (small_state, _, parent_id) = seed(small.path(), 0);
    let small_store = small_state.store();
    let small_guard = small_store.lock();
    let ownership = small_guard.library_ownership().unwrap();
    let parent_id = ownership
        .libraries()
        .iter()
        .find(|library| library.name == "Target")
        .expect("target library")
        .id
        .clone();
    let (small_rows, small_steps) = small_guard.jellyfin_ledger_candidates_with_vm_steps(&ownership, &parent_id).unwrap();
    assert_eq!(small_rows.len(), 1);

    let large = tempfile::tempdir().unwrap();
    let (large_state, _, large_parent) = seed(large.path(), 300);
    let large_store = large_state.store();
    let large_guard = large_store.lock();
    let large_ownership = large_guard.library_ownership().unwrap();
    // 大库必须用**它自己**的库 id：否则 scoped 读会走 "该库不在本 ownership" 的
    // 提前返回，测出 0 步，断言恒真（测不到退化）。
    let large_parent_id = large_ownership
        .libraries()
        .iter()
        .find(|library| library.name == "Target")
        .expect("target library")
        .id
        .clone();
    let (large_rows, large_steps) = large_guard
        .jellyfin_ledger_candidates_with_vm_steps(&large_ownership, &large_parent_id)
        .unwrap();
    assert_eq!(large_rows.len(), 1);
    assert!(small_steps > 0, "fixture read did no DB work: small={small_steps}");
    assert!(
        large_steps <= small_steps * 2,
        "scoped DB work grew with unrelated rows: small={small_steps}, large={large_steps}"
    );
}
