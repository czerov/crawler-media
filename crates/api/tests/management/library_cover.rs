use std::collections::HashMap;
use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::*;

fn app(tmp: &tempfile::TempDir) -> axum::Router {
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    router(state(tmp.path(), fetcher, downloader))
}

async fn create_movie_library_and_assert_empty_cover(
    app: &axum::Router,
    movie_root: &std::path::Path,
) -> (String, std::path::PathBuf) {
    let movie_lib = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/libraries",
                Some("management-secret"),
                json!({
                    "name": "测试电影库",
                    "kind": "movie",
                    "root_paths": [movie_root.display().to_string()],
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let lib_id = movie_lib["data"]["id"].as_str().unwrap().to_string();

    let cover_res = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/v1/libraries/{lib_id}/cover"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(cover_res.status(), StatusCode::OK);
    let default_cover = axum::body::to_bytes(cover_res.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(default_cover.starts_with(b"\xff\xd8\xff"));

    let film_dir = movie_root.join("The Matrix (1999)");
    std::fs::create_dir_all(&film_dir).unwrap();
    std::fs::write(film_dir.join("poster.jpg"), b"fake poster bytes").unwrap();
    std::fs::write(film_dir.join("matrix.mp4"), b"fake video").unwrap();

    scan_and_wait(&app, &lib_id).await;

    let cover_res2 = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/v1/libraries/{lib_id}/cover"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(
        cover_res2.status(),
        StatusCode::OK,
        "单独的纵版海报不自动填充封面，但默认图仍应可用"
    );
    let fallback = axum::body::to_bytes(cover_res2.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(fallback.starts_with(b"\xff\xd8\xff"));
    (lib_id, film_dir)
}

async fn setup_movie_library_with_fanart(
    app: &axum::Router,
    tmp: &tempfile::TempDir,
) -> (String, std::path::PathBuf) {
    let movie_root = tmp.path().join("movies");
    std::fs::create_dir_all(&movie_root).unwrap();

    let (lib_id, film_dir) = create_movie_library_and_assert_empty_cover(app, &movie_root).await;

    let first_poster = image::RgbImage::from_pixel(300, 450, image::Rgb([30, 90, 160]));
    first_poster.save(film_dir.join("poster.jpg")).unwrap();
    let second_dir = movie_root.join("Arrival (2016)");
    std::fs::create_dir_all(&second_dir).unwrap();
    let second_poster = image::RgbImage::from_pixel(300, 450, image::Rgb([160, 90, 30]));
    second_poster.save(second_dir.join("poster.jpg")).unwrap();
    std::fs::write(second_dir.join("Arrival (2016).mkv"), b"video dummy").unwrap();

    // 即使库内存在横版 fanart，也应从多部作品海报生成库封面。
    let fanart = film_dir.join("fanart.jpg");
    std::fs::write(&fanart, b"fake fanart bytes 12345").unwrap();
    let cover_file = tmp
        .path()
        .join("data")
        .join("library")
        .join("covers")
        .join(format!("library-{lib_id}.jpg"));
    std::fs::create_dir_all(cover_file.parent().unwrap()).unwrap();
    std::fs::copy(&fanart, &cover_file).unwrap();
    scan_and_wait(&app, &lib_id).await;

    let cover_res3 = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/v1/libraries/{lib_id}/cover"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(cover_res3.status(), StatusCode::OK);
    assert_eq!(
        cover_res3.headers().get("content-type").unwrap(),
        "image/jpeg"
    );
    assert!(cover_file.is_file(), "自动拷贝生成了 cover.jpg");
    // C2b / F08：legacy 文件内容保持原始 bytes 不被就地覆盖破坏，新生成的合成封面发布为新版本文件！
    let after_scan = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/v1/libraries/{lib_id}/cover"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(after_scan.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(after_scan.into_body(), usize::MAX)
        .await
        .unwrap();
    let generated = image::load_from_memory(&bytes).unwrap();
    assert_eq!((generated.width(), generated.height()), (1920, 1080));
    assert_ne!(bytes.as_ref(), b"fake fanart bytes 12345");
    assert_eq!(
        std::fs::read(&cover_file).unwrap(),
        b"fake fanart bytes 12345",
        "legacy 封面文件必须保持只读"
    );
    (lib_id, cover_file)
}

async fn assert_cover_crud(app: &axum::Router, lib_id: &str, cover_file: &std::path::Path) {
    // 4. 用户主动上传/更换封面 (POST /cover)
    let upload_res = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/libraries/{lib_id}/cover"),
            Some("management-secret"),
            json!({
                "data_url": "data:image/jpeg;base64,aGVsbG8gY3VzdG9tIGNvdmVy"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(upload_res.status(), StatusCode::OK);
    // C2b：手动封面发布为新的不可变版本文件；legacy 固定路径只读保留。
    let after_upload = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/v1/libraries/{lib_id}/cover"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(after_upload.status(), StatusCode::OK);
    let uploaded = axum::body::to_bytes(after_upload.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(uploaded.as_ref(), b"hello custom cover");

    // 5. 用户删除封面 (DELETE /cover)
    let delete_res = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/libraries/{lib_id}/cover"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(delete_res.status(), StatusCode::OK);
    // C2b：legacy 固定路径仅读、不预先 unlink；删除通过 revision CAS 到 none。
    assert!(
        cover_file.is_file(),
        "legacy 固定路径不被 DELETE 预先 unlink"
    );

    let cover_res4 = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/v1/libraries/{lib_id}/cover"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(cover_res4.status(), StatusCode::OK);
    let fallback_after_delete = axum::body::to_bytes(cover_res4.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(fallback_after_delete.starts_with(b"\xff\xd8\xff"));
    assert_ne!(fallback_after_delete.as_ref(), b"hello custom cover");
}

#[tokio::test]
async fn library_cover_auto_fill_only_uses_fanart_and_supports_crud() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let (lib_id, cover_file) = setup_movie_library_with_fanart(&app, &tmp).await;
    assert_cover_crud(&app, &lib_id, &cover_file).await;
}
