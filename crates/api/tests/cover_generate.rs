use api::router;
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use image::{Rgba, RgbaImage};
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use tower::ServiceExt;

#[path = "management/common.rs"]
mod common;
use common::{Fixtures, json_body, request, state};

fn test_app(tmp: &tempfile::TempDir) -> axum::Router {
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    router(state(tmp.path(), fetcher, downloader))
}

async fn setup_library_with_poster(app: &axum::Router, tmp: &tempfile::TempDir) -> String {
    let movies_dir = tmp.path().join("library").join("movies");
    std::fs::create_dir_all(&movies_dir).unwrap();

    let create_lib_res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/libraries",
            Some("management-secret"),
            json!({
                "name": "华语经典",
                "kind": "movie",
                "root_paths": [movies_dir.display().to_string()]
            }),
        ))
        .await
        .unwrap();

    assert_eq!(create_lib_res.status(), StatusCode::OK);
    let lib_info = json_body(create_lib_res).await;
    let lib_id = lib_info["data"]["id"].as_str().unwrap().to_string();

    let res_empty = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/libraries/{lib_id}/cover/generate"),
            Some("management-secret"),
            json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(res_empty.status(), StatusCode::BAD_REQUEST);

    let movie_item_dir = movies_dir.join("Inception (2010)");
    std::fs::create_dir_all(&movie_item_dir).unwrap();
    let mut fake_poster = image::RgbImage::new(300, 450);
    for pixel in fake_poster.pixels_mut() {
        *pixel = image::Rgb([30, 90, 160]);
    }
    fake_poster.save(movie_item_dir.join("poster.jpg")).unwrap();
    std::fs::write(movie_item_dir.join("Inception (2010).mkv"), b"video dummy").unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(600)).await;

    let scan_res = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/libraries/{lib_id}/scan"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(scan_res.status(), StatusCode::ACCEPTED);
    let scan_body = json_body(scan_res).await;
    let op_id = scan_body["data"]["operation_id"].as_str().unwrap();
    let mut completed = false;
    for _ in 0..100 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let poll_res = app
            .clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/libraries/{lib_id}/scan-operations/{op_id}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap();
        if poll_res.status() == StatusCode::OK {
            let body = json_body(poll_res).await;
            if body["data"]["status"] == "completed" {
                completed = true;
                break;
            }
        }
    }
    assert!(completed, "scan must complete");
    lib_id
}

#[tokio::test]
async fn test_generate_library_cover_endpoint() {
    let tmp = tempfile::tempdir().unwrap();
    let app = test_app(&tmp);
    let lib_id = setup_library_with_poster(&app, &tmp).await;

    // 4. 调用预览模式 (preview_only: true)
    let res_preview = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/libraries/{lib_id}/cover/generate"),
            Some("management-secret"),
            json!({
                "title_zh": "华语经典",
                "title_en": "CHINESE MOVIES",
                "preview_only": true,
                "style": "macaron_card_single"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(res_preview.status(), StatusCode::OK);
    let preview_body = json_body(res_preview).await;
    assert_eq!(preview_body["data"]["preview"], true);
    assert!(
        preview_body["data"]["data_url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/jpeg;base64,")
    );

    // 5. 调用正式生成模式 (preview_only: false)
    let res_apply = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/libraries/{lib_id}/cover/generate"),
            Some("management-secret"),
            json!({
                "preview_only": false,
                "background": {
                    "mode": "solid_color",
                    "hex_color": "#121824"
                }
            }),
        ))
        .await
        .unwrap();
    assert_eq!(res_apply.status(), StatusCode::OK);

    // 6. 验证 GET /libraries/{id}/cover 可以成功获取生成的封面 JPEG
    let res_cover = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/v1/libraries/{lib_id}/cover"),
            None,
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(res_cover.status(), StatusCode::OK);
    assert_eq!(
        res_cover.headers().get("content-type").unwrap(),
        "image/jpeg"
    );
}
