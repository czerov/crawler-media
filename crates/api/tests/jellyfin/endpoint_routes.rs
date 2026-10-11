use super::*;

#[tokio::test]
async fn registered_jellyfin_endpoint_families_accept_their_documented_methods() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, item_id) = app(tmp.path());
    let user_id = "00000000-0000-0000-0000-000000000001";
    let progress = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/Sessions/Playing/Progress")
                .header("authorization", "Bearer admin-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "ItemId": item_id, "PositionTicks": 10_000_000 }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(progress.status(), StatusCode::NO_CONTENT);
    assert_ping_methods(&app).await;
    assert_user_endpoint_methods(&app, user_id, &item_id).await;
    assert_playback_event_methods(&app, &item_id).await;
}

async fn assert_ping_methods(app: &axum::Router) {
    for method in ["GET", "POST"] {
        for prefix in ["", "/emby"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(format!("{prefix}/System/Ping"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "{method} {prefix}/System/Ping"
            );
        }
    }
}

async fn assert_user_endpoint_methods(app: &axum::Router, user_id: &str, item_id: &str) {
    for (method, uri) in [
        ("GET", "/Users/Me".to_string()),
        ("GET", "/QuickConnect/Enabled".to_string()),
        ("GET", "/DisplayPreferences/test-device".to_string()),
        ("POST", "/DisplayPreferences/test-device".to_string()),
        ("GET", format!("/Users/{user_id}/Items/Counts")),
        ("GET", format!("/Users/{user_id}/Items/Resume")),
        ("GET", format!("/Users/{user_id}/Items/Latest")),
        ("GET", "/Shows/NextUp".to_string()),
        ("GET", "/Genres".to_string()),
        ("GET", "/Studios".to_string()),
        ("GET", "/Search/Hints?SearchTerm=test".to_string()),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(&uri)
                    .header("authorization", "Bearer admin-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "{method} {uri}: {}",
            response.status()
        );
        if uri.ends_with("/Items/Counts") {
            let body = json(response).await;
            assert_eq!(body["MovieCount"], 1);
        } else if uri.ends_with("/Items/Resume") {
            let body = json(response).await;
            assert_eq!(body["Items"][0]["Id"], item_id);
        } else if uri.ends_with("/Items/Latest") {
            let body = json(response).await;
            assert_eq!(body[0]["Id"], item_id);
        } else if uri == "/QuickConnect/Enabled" {
            assert_eq!(json(response).await, false);
        }
    }
}

async fn assert_playback_event_methods(app: &axum::Router, item_id: &str) {
    for path in [
        "/Sessions/Playing",
        "/Sessions/Playing/Progress",
        "/Sessions/Playing/Stopped",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .header("authorization", "Bearer admin-token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({ "ItemId": item_id, "PositionTicks": 10_000_000 }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT, "{path}");
    }
}
