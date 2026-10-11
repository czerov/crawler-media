use std::{collections::HashMap, sync::Arc};

use api::{management::ApiState, router};
use axum::http::StatusCode;
use domain::MediaKind;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::{Fixtures, json_data, request, state, subscribe_payload};

async fn assert_target(app: &axum::Router, id: &str, library_id: Option<&str>, status: &str) {
    for uri in [
        "/api/v1/subscriptions".to_string(),
        format!("/api/v1/subscriptions/{id}"),
    ] {
        let response = app
            .clone()
            .oneshot(request("GET", &uri, Some("management-secret"), Value::Null))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let data = json_data(response).await;
        let sub = if data.is_array() { &data[0] } else { &data };
        assert_eq!(sub["library_id"], json!(library_id));
        assert_eq!(sub["target_library_status"], status);
        if status == "ready" {
            assert!(sub["target_library_error"].is_null());
        } else {
            let error = sub["target_library_error"].as_str().unwrap();
            assert!(error.contains("重新选择媒体库"), "{error}");
            assert!(
                error.contains(if status == "missing" {
                    "不存在"
                } else {
                    "删除"
                }),
                "{error}"
            );
        }
    }
}

async fn create(app: &axum::Router, library: Option<&str>) -> String {
    let mut payload = subscribe_payload("search");
    payload["library_id"] = json!(library);
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            payload,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    json_data(response).await["id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn fixture(root: &std::path::Path) -> ApiState {
    state(
        root,
        Arc::new(Fixtures {
            requests: Mutex::new(vec![]),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
    )
}

#[tokio::test]
async fn target_library_snapshot_preserves_binding_through_freeze_failure_and_deletion() {
    let tmp = tempfile::tempdir().unwrap();
    let state = fixture(tmp.path());
    let app = router(state.clone());
    let store = state.store();
    let library = store
        .lock()
        .create_library(MediaKind::Movie, "Target", &[], "everyone", true, &[])
        .unwrap();
    let id = create(&app, Some(&library.id)).await;
    assert_target(&app, &id, Some(&library.id), "ready").await;
    let removal = store.lock().begin_library_removal(&library.id).unwrap();
    assert_target(&app, &id, Some(&library.id), "deleting").await;
    store
        .lock()
        .set_removal_state(&removal.operation_id, "failed", Some("fixture failure"))
        .unwrap();
    assert_target(&app, &id, Some(&library.id), "deleting").await;
    store
        .lock()
        .set_removal_state(&removal.operation_id, "cleaning", None)
        .unwrap();
    store
        .lock()
        .clean_library_removal(&removal.operation_id)
        .unwrap();
    assert_target(&app, &id, Some(&library.id), "missing").await;
    let sub = store
        .lock()
        .get_subscribe(id.parse().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(sub.library_id.unwrap().to_string(), library.id);
    let replacement = store
        .lock()
        .default_library(MediaKind::Movie)
        .unwrap()
        .unwrap();
    let response = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/subscriptions/{id}"),
            Some("management-secret"),
            json!({ "library_id": replacement.id }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let updated = json_data(response).await;
    assert_eq!(updated["target_library_status"], "ready");
    assert!(updated["target_library_error"].is_null());
    assert_target(&app, &id, Some(&replacement.id), "ready").await;
}

#[tokio::test]
async fn implicit_target_is_ready_without_changing_default_routing() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(fixture(tmp.path()));
    let id = create(&app, None).await;
    assert_target(&app, &id, None, "ready").await;
}
