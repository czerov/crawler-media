//! C3：图片 conditional GET —— 先授权/可见性，再比较 validator。

use std::path::Path;
use std::sync::Arc;

use api::{ApiState, Store, router};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource, UserRole};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use tower::ServiceExt;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline fixture".into()))
    }
}

fn make_state(root: &Path) -> ApiState {
    ApiState::new(
        Store::open(root.join("data")).unwrap(),
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
        root.join("library"),
    )
    .unwrap()
}

fn request(uri: &str, token: Option<&str>, etag: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method("GET").uri(uri);
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    if let Some(etag) = etag {
        builder = builder.header(header::IF_NONE_MATCH, etag);
    }
    builder.body(Body::empty()).unwrap()
}

async fn body_bytes(response: axum::response::Response) -> Vec<u8> {
    to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec()
}

fn seed_member(store: &Store, login: &str) -> String {
    let id = domain::UserId::new();
    store
        .insert_user_with_password(
            &domain::User {
                id,
                login: login.into(),
                enabled: true,
                role: UserRole::Member,
            },
            "member-pass",
        )
        .unwrap();
    let token = format!("{login}-session");
    store.set_user_token(id, &token).unwrap();
    token
}

/// 建立 `selected`（空成员名单）媒体库 + 一条 ledger 行 + 旁边的 poster.jpg。
fn seed_poster(root: &Path, state: &ApiState, poster: &[u8]) -> LedgerId {
    let library_root = root.join("hidden-library");
    std::fs::create_dir_all(&library_root).unwrap();
    let file = library_root.join("Movie.mkv");
    std::fs::write(&file, b"video").unwrap();
    std::fs::write(library_root.join("poster.jpg"), poster).unwrap();

    let store = state.store();
    let guard = store.lock();
    guard
        .create_library(
            MediaKind::Movie,
            "Hidden",
            &[library_root.to_str().unwrap()],
            "selected",
            true,
            &[],
        )
        .unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Hidden Movie".into(),
        year: Some(2024),
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    guard.insert_media(&media).unwrap();
    let ledger_id = LedgerId::new();
    guard
        .insert_ledger(&LedgerRow {
            id: ledger_id,
            media_id: media.id,
            path: file.display().to_string(),
            season: None,
            episode: None,
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Probe,
            confidence: Confidence::High,
            filter_score: None,
        })
        .unwrap();
    ledger_id
}

#[tokio::test]
async fn conditional_get_requires_authorization_before_validator() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    let app = router(state.clone());
    let first_bytes = b"\xff\xd8\xffPOSTER-ONE".to_vec();
    let ledger_id = seed_poster(tmp.path(), &state, &first_bytes);
    let member = {
        let store = state.store();
        seed_member(&store.lock(), "alice")
    };
    let uri = format!("/api/v1/posters/{ledger_id}");

    // 匿名 200 + ETag（public 静态图路由不得要求认证）。
    let first = app
        .clone()
        .oneshot(request(&uri, None, None))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let etag = first
        .headers()
        .get(header::ETAG)
        .expect("ETag required")
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(
        first.headers().get(header::CACHE_CONTROL).unwrap(),
        "private, no-cache"
    );
    assert!(first.headers().get(header::VARY).is_some());
    assert_eq!(body_bytes(first).await, first_bytes);

    // 命中 validator → 304 且空 body。
    let cached = app
        .clone()
        .oneshot(request(&uri, None, Some(&etag)))
        .await
        .unwrap();
    assert_eq!(cached.status(), StatusCode::NOT_MODIFIED);
    assert!(body_bytes(cached).await.is_empty());

    // 弱比较与逗号列表同样命中。
    for candidate in [format!("W/{etag}"), format!("\"other\", {etag}")] {
        let hit = app
            .clone()
            .oneshot(request(&uri, None, Some(&candidate)))
            .await
            .unwrap();
        assert_eq!(hit.status(), StatusCode::NOT_MODIFIED, "{candidate}");
    }

    // 成员看不到该库：即使带正确 ETag 也必须 404，不能 304。
    let hidden = app
        .clone()
        .oneshot(request(&uri, Some(&member), Some(&etag)))
        .await
        .unwrap();
    assert_eq!(
        hidden.status(),
        StatusCode::NOT_FOUND,
        "hidden library must 404 before any validator comparison"
    );

    // 换图后 ETag 变化：旧 validator 不再命中，返回新内容。
    let second_bytes = b"\xff\xd8\xffPOSTER-TWO-LONGER".to_vec();
    std::fs::write(tmp.path().join("hidden-library/poster.jpg"), &second_bytes).unwrap();
    let refreshed = app
        .clone()
        .oneshot(request(&uri, None, Some(&etag)))
        .await
        .unwrap();
    assert_eq!(refreshed.status(), StatusCode::OK);
    let new_etag = refreshed
        .headers()
        .get(header::ETAG)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert_ne!(new_etag, etag, "changed bytes must change the ETag");
    assert_eq!(body_bytes(refreshed).await, second_bytes);
}
