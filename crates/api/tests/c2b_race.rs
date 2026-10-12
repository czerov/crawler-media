//! baseline red: 晚到的自动渲染会覆盖更新的手动封面（只用基线公开 API）。
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use api::{ApiState, Store, router};
use axum::body::{Body, to_bytes};
use axum::http::Request;
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use serde_json::{Value, json};
use tower::ServiceExt;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline fixture".into()))
    }
}

fn request(method: &str, uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", "Bearer management-secret")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn seed(root: &Path, state: &ApiState) -> String {
    let library_root = root.join("movies");
    let store = state.store();
    let guard = store.lock();
    let library = guard
        .create_library(
            domain::MediaKind::Movie,
            "Race Library",
            &[library_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    for (index, title) in ["Alpha", "Bravo"].iter().enumerate() {
        let dir = library_root.join(format!("{title} (200{index})"));
        std::fs::create_dir_all(&dir).unwrap();
        let poster = image::RgbImage::from_pixel(300, 450, image::Rgb([30, 90, 160]));
        poster.save(dir.join("poster.jpg")).unwrap();
        let file = dir.join(format!("{title}.mp4"));
        std::fs::write(&file, b"video").unwrap();
        let media = domain::Media {
            id: domain::MediaId::new(),
            kind: domain::MediaKind::Movie,
            title: (*title).into(),
            year: None,
            original_title: None,
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        guard.insert_media(&media).unwrap();
        guard
            .insert_ledger(&domain::LedgerRow {
                id: domain::LedgerId::new(),
                media_id: media.id,
                path: file.display().to_string(),
                season: None,
                episode: None,
                resolution: None,
                codec: None,
                hdr: None,
                quality_source: domain::QualitySource::Probe,
                confidence: domain::Confidence::High,
                filter_score: None,
            })
            .unwrap();
    }
    library.id
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn late_auto_generate_must_not_overwrite_manual_cover() {
    let tmp = tempfile::tempdir().unwrap();
    let state = ApiState::new(
        Store::open(tmp.path().join("data")).unwrap(),
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
        tmp.path().join("library"),
    )
    .unwrap();
    let app = router(state.clone());
    let library_id = seed(tmp.path(), &state);

    // 启动（较慢的）自动生成，不等待。
    let generate_app = app.clone();
    let generate_uri = format!("/api/v1/libraries/{library_id}/cover/generate");
    let generate = tokio::spawn(async move {
        generate_app
            .oneshot(request(
                "POST",
                &generate_uri,
                json!({ "style": "multi_poster_pile" }),
            ))
            .await
            .unwrap()
    });

    // 渲染期间用户手动上传更新的封面。
    tokio::time::sleep(Duration::from_millis(20)).await;
    let upload = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/libraries/{library_id}/cover"),
            json!({ "data_url": "data:image/jpeg;base64,aGVsbG8gbWFudWFsIGNvdmVy" }),
        ))
        .await
        .unwrap();
    if upload.status() != 200 {
        let status = upload.status();
        let body = to_bytes(upload.into_body(), usize::MAX).await.unwrap();
        panic!("upload failed {status}: {}", String::from_utf8_lossy(&body));
    }

    let generated = generate.await.unwrap();
    assert!(
        generated.status() == 200 || generated.status() == 409,
        "auto generate should publish or lose the CAS, got {}",
        generated.status()
    );

    let cover = app
        .oneshot(request(
            "GET",
            &format!("/api/v1/libraries/{library_id}/cover"),
            Value::Null,
        ))
        .await
        .unwrap();
    let bytes = to_bytes(cover.into_body(), usize::MAX).await.unwrap();
    assert_eq!(
        bytes.as_ref(),
        b"hello manual cover",
        "晚到的自动渲染覆盖了更新的手动封面"
    );
}
