use std::path::Path;
use std::sync::Arc;

use api::{ApiState, Store, router};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::{
    Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource, User, UserId,
    UserRole,
};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use serde_json::Value;
use tower::ServiceExt;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("unused".into()))
    }
}

fn app(root: &Path) -> (axum::Router, String) {
    let store = Store::open(root.join("data")).unwrap();
    let media = Media {
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
    };
    store.insert_media(&media).unwrap();
    let library_root = root.join("library");
    std::fs::create_dir_all(&library_root).unwrap();
    store
        .create_library(
            MediaKind::Movie,
            "Movies",
            &[library_root.to_str().unwrap()],
            "selected",
            true,
            &[],
        )
        .unwrap();
    let file = root.join("matrix.mkv");
    std::fs::write(&file, b"0123456789abcdef").unwrap();
    let row = LedgerRow {
        id: LedgerId::new(),
        media_id: media.id,
        path: file.display().to_string(),
        season: None,
        episode: None,
        resolution: Some("2160p".into()),
        codec: Some("hevc".into()),
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: Some(100),
    };
    store.insert_ledger(&row).unwrap();
    let compact = row.id.to_string().replace('-', "");
    let router = router(
        ApiState::new(
            store,
            "admin-token".into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(root.join("stage"))),
            root.join("library"),
        )
        .unwrap(),
    );
    (router, compact)
}

fn empty_library_app(root: &Path) -> axum::Router {
    let store = Store::open(root.join("data")).unwrap();
    let library_root = root.join("empty-library");
    std::fs::create_dir_all(&library_root).unwrap();
    store
        .create_library(
            MediaKind::Movie,
            "Empty Movies",
            &[library_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    router(
        ApiState::new(
            store,
            "admin-token".into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(root.join("stage"))),
            library_root,
        )
        .unwrap(),
    )
}

async fn empty_library_id_and_cover_tag(app: &axum::Router) -> (String, String) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/UserViews")
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    let library = body["Items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["Name"] == "Empty Movies")
        .expect("the test library should be visible");
    (
        library["Id"].as_str().unwrap().to_string(),
        library["ImageTags"]["Primary"]
            .as_str()
            .unwrap()
            .to_string(),
    )
}

async fn bytes(response: axum::response::Response) -> (StatusCode, Vec<u8>) {
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, body.to_vec())
}

#[tokio::test]
async fn primary_image_returns_generated_cover_when_library_has_no_cover() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _compact) = app(tmp.path());
    let views = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/UserViews")
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let views: Value =
        serde_json::from_slice(&to_bytes(views.into_body(), usize::MAX).await.unwrap()).unwrap();
    let library_id = views["Items"][0]["Id"].as_str().unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/Items/{library_id}/Images/Primary"))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, body) = bytes(response).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.starts_with(b"\xff\xd8\xff"), "fallback must be JPEG");
    assert!(
        body.len() > 100,
        "fallback should be a real image, not a signature stub"
    );
}

#[tokio::test]
async fn empty_library_primary_image_uses_default_art_and_changes_cache_tag_with_cover() {
    let tmp = tempfile::tempdir().unwrap();
    let app = empty_library_app(tmp.path());
    let (library_id, initial_tag) = empty_library_id_and_cover_tag(&app).await;

    let image = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/Items/{library_id}/Images/Primary"))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, image_bytes) = bytes(image).await;
    assert_eq!(status, StatusCode::OK);
    assert!(image_bytes.starts_with(b"\xff\xd8\xff"));
    assert!(
        image_bytes.len() > 100,
        "empty library should receive the default artwork"
    );
    assert_eq!(
        image_bytes.as_slice(),
        include_bytes!("../assets/covers/movie.jpg"),
        "VidHub should receive the current illustrated default cover"
    );

    let uploaded = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/libraries/{library_id}/cover"))
                .header("authorization", "Bearer admin-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"data_url":"data:image/jpeg;base64,aGVsbG8="}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(uploaded.status(), StatusCode::OK);

    let (_, updated_tag) = empty_library_id_and_cover_tag(&app).await;
    assert_ne!(
        initial_tag, updated_tag,
        "VidHub must see cover changes through ImageTags"
    );
}

#[tokio::test]
async fn primary_image_returns_poster_jpeg_and_items_tag() {
    let tmp = tempfile::tempdir().unwrap();
    let jpeg = b"\xff\xd8\xff\xdb";
    std::fs::write(tmp.path().join("poster.jpg"), jpeg).unwrap();
    let (app, compact) = app(tmp.path());

    let listed = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/Items")
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body: Value =
        serde_json::from_slice(&to_bytes(listed.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["Items"][0]["ImageTags"]["Primary"], "poster");

    let views = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/UserViews")
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let views_body: Value =
        serde_json::from_slice(&to_bytes(views.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert!(views_body["Items"][0]["ImageTags"]["Primary"]
        .as_str()
        .is_some_and(|tag| tag.starts_with("cover-")));

    let library_id = views_body["Items"][0]["Id"].as_str().unwrap();
    let library_image = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/Items/{library_id}/Images/Primary"))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(library_image.status(), StatusCode::OK);
    let image_body = to_bytes(library_image.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(image_body.starts_with(b"\xff\xd8\xff"));

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/Items/{compact}/Images/Primary"))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, body) = bytes(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, jpeg);
}

#[tokio::test]
async fn static_artwork_is_public_but_respects_authenticated_library_visibility() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let private_root = tmp.path().join("private");
    std::fs::create_dir_all(&private_root).unwrap();
    let private = store
        .create_library(
            MediaKind::Movie,
            "Private",
            &[private_root.to_str().unwrap()],
            "selected",
            true,
            &[],
        )
        .unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Private Movie".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let file = private_root.join("secret.mkv");
    std::fs::write(&file, b"video").unwrap();
    let poster = private_root.join("poster.jpg");
    std::fs::write(&poster, b"private poster").unwrap();
    store
        .set_library_cover_path(&private.id, Some(&poster.display().to_string()))
        .unwrap();
    let row = LedgerRow {
        id: LedgerId::new(),
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
    };
    store.insert_ledger(&row).unwrap();
    let compact = row.id.to_string().replace('-', "");
    let member_id = UserId::new();
    store
        .insert_user(&User {
            id: member_id,
            login: "not-allowed".into(),
            enabled: true,
            role: UserRole::Member,
        })
        .unwrap();
    store.set_user_token(member_id, "member-token").unwrap();
    let router = router(
        ApiState::new(
            store,
            "admin-token".into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
            tmp.path().join("library"),
        )
        .unwrap(),
    );

    // 按照系统规范，静态图片资源（/posters/*, /libraries/{id}/cover 等）
    // 为了支持前端原生 <img> 标签无 Header 加载，属于公开路由（不需要 Authorization 头），
    // 此时匿名请求会放行（返回 200），但仍会结合可选的 user 上下文进行可见性校验。
    let anonymous = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/posters/{compact}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(anonymous.status(), StatusCode::OK);

    let member = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/posters/{compact}"))
                .header("authorization", "Bearer member-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(member.status(), StatusCode::NOT_FOUND);

    let jellyfin_anonymous = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/Items/{compact}/Images/Primary"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(jellyfin_anonymous.status(), StatusCode::OK);
    assert_eq!(
        jellyfin_anonymous.headers()[axum::http::header::CONTENT_TYPE],
        "image/jpeg"
    );
    assert_eq!(
        &to_bytes(jellyfin_anonymous.into_body(), usize::MAX)
            .await
            .unwrap()[..],
        b"private poster"
    );

    let jellyfin_member = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/Items/{compact}/Images/Primary"))
                .header("authorization", "Bearer member-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(jellyfin_member.status(), StatusCode::NOT_FOUND);

    let emby_anonymous = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/emby/Items/{compact}/Images/Primary"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(emby_anonymous.status(), StatusCode::OK);

    let cover = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/libraries/{}/cover", private.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cover.status(), StatusCode::OK);

    let hidden_cover = router
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/libraries/{}/cover", private.id))
                .header("authorization", "Bearer member-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(hidden_cover.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn artwork_content_type_matches_png_and_webp_bytes() {
    for (name, bytes, expected) in [
        ("png", b"\x89PNG\r\n\x1a\ncontent".as_slice(), "image/png"),
        (
            "webp",
            b"RIFF\x08\0\0\0WEBPcontent".as_slice(),
            "image/webp",
        ),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let image_path = tmp.path().join("poster.jpg");
        std::fs::write(&image_path, bytes).unwrap();
        let (app, compact) = app(tmp.path());
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/posters/{compact}"))
                    .header("authorization", "Bearer admin-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{name}");
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .unwrap(),
            expected,
            "{name}"
        );

        let jellyfin = app
            .oneshot(
                Request::builder()
                    .uri(format!("/Items/{compact}/Images/Primary"))
                    .header("authorization", "Bearer admin-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(jellyfin.status(), StatusCode::OK, "Jellyfin {name}");
        assert_eq!(
            jellyfin
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .unwrap(),
            expected,
            "Jellyfin {name}"
        );
    }
}
