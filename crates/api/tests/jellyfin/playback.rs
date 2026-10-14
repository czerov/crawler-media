use super::*;

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

async fn post_session_event(
    app: &axum::Router,
    path: &str,
    item_id: &str,
    ticks: i64,
) -> StatusCode {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("authorization", "Bearer admin-token")
                .header("content-type", "application/json")
                .header("X-Emby-Device-Id", "infuse-bedroom")
                .header("X-Emby-Device-Name", "Bedroom Apple TV")
                .header("X-Emby-Client", "Infuse")
                .header("X-Emby-Client-Version", "8.1")
                .body(Body::from(
                    json!({ "ItemId": item_id, "PositionTicks": ticks }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    response.status()
}

#[tokio::test]
async fn playback_info_disables_transcoding() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, compact) = app(tmp.path());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/Items/{compact}/PlaybackInfo"))
                .header("authorization", "Bearer admin-token")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = json(response).await;
    assert_eq!(body["MediaSources"][0]["SupportsDirectPlay"], true);
    assert_eq!(body["MediaSources"][0]["SupportsTranscoding"], false);
}

#[tokio::test]
async fn stream_supports_http_range() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, compact) = app(tmp.path());
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/Videos/{compact}/stream"))
                .header("authorization", "Bearer admin-token")
                .header("range", "bytes=0-3")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(&bytes[..], b"0123");
}

#[tokio::test]
async fn playing_progress_is_stored_per_user() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, media_id, compact) = app(tmp.path());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/Sessions/Playing/Progress")
                .header("authorization", "Bearer admin-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "ItemId": compact, "PositionTicks": 50_000_000 }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let store = Store::open(tmp.path().join("data")).unwrap();
    assert_eq!(
        store
            .playback_progress(
                domain::UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap(),
                media_id
            )
            .unwrap(),
        Some(5000)
    );
}

#[tokio::test]
async fn jellyfin_session_events_create_activity_and_a_play_log() {
    let tmp = tempfile::tempdir().unwrap();
    let now = now_secs();
    let clock = Arc::new(FakeClock(std::sync::atomic::AtomicI64::new(now * 1000)));
    let (app, media_id, item_id) = app_with_clock(tmp.path(), clock.clone());

    assert_eq!(
        post_session_event(&app, "/Sessions/Playing", &item_id, 10_000_000).await,
        StatusCode::NO_CONTENT
    );

    clock.0.store((now + 2) * 1000, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        post_session_event(&app, "/Sessions/Playing/Progress", &item_id, 30_000_000).await,
        StatusCode::NO_CONTENT
    );

    let user_id = domain::UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let sessions = store.active_sessions(now, 60).unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].user_id, user_id);
    assert_eq!(sessions[0].media_id, media_id);
    assert_eq!(sessions[0].device_id, "infuse-bedroom");
    assert_eq!(sessions[0].client.as_deref(), Some("Infuse"));
    assert_eq!(sessions[0].device_name.as_deref(), Some("Bedroom Apple TV"));
    assert_eq!(sessions[0].position_ms, 3_000);

    clock.0.store((now + 4) * 1000, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        post_session_event(&app, "/Sessions/Playing/Stopped", &item_id, 50_000_000).await,
        StatusCode::NO_CONTENT
    );
    assert!(store.active_sessions(now + 4, 60).unwrap().is_empty());
    let logs = store
        .list_logs(10, None, None, None, now + 4, Some(user_id))
        .unwrap();
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].media_id, media_id);
    assert_eq!(logs[0].device_name.as_deref(), Some("Bedroom Apple TV"));
    assert_eq!(logs[0].client.as_deref(), Some("Infuse"));
    assert_eq!(logs[0].end_position_ms, 5_000);
    assert_eq!(logs[0].watched_ms, 4_000);
}

#[tokio::test]
async fn progress_persistence_failure_is_returned_to_the_client() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, item_id) = app(tmp.path());
    let db = rusqlite::Connection::open(tmp.path().join("data/subscribe.db")).unwrap();
    db.execute_batch("DROP TABLE playback_units;").unwrap();
    drop(db);

    let response = app
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
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

/// 管理员结束会话后，即使会话随后被清理（sweep 关闭并写日志），
/// 迟到的 Progress 也不得把会话复活；只有客户端显式重新起播才可以。
#[tokio::test]
async fn admin_ended_session_is_not_resurrected_by_a_late_progress_event() {
    let tmp = tempfile::tempdir().unwrap();
    let now = now_secs();
    let clock = Arc::new(FakeClock(std::sync::atomic::AtomicI64::new(now * 1000)));
    let (app, _, item_id) = app_with_clock(tmp.path(), clock.clone());
    let user_id = domain::UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap();

    assert_eq!(
        post_session_event(&app, "/Sessions/Playing", &item_id, 10_000_000).await,
        StatusCode::NO_CONTENT
    );

    // 管理员在 web 管理接口结束该设备会话。
    let ended = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/v1/playback/activity/sessions/infuse-bedroom/end?user_id={user_id}"
                ))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ended.status(), StatusCode::OK);

    // 会话随后被清理（等价于 sweep 关闭并写 admin_ended=1 的播放日志）。
    let store = Store::open(tmp.path().join("data")).unwrap();
    store.close_session(user_id, "infuse-bedroom", now + 1).unwrap();
    assert!(store.active_sessions(now + 1, 60).unwrap().is_empty());

    // 迟到 progress：不得复活会话。
    clock.0.store((now + 2) * 1000, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        post_session_event(&app, "/Sessions/Playing/Progress", &item_id, 30_000_000).await,
        StatusCode::NO_CONTENT
    );
    let store = Store::open(tmp.path().join("data")).unwrap();
    assert!(
        store.active_sessions(now + 2, 60).unwrap().is_empty(),
        "管理员已结束的会话不得被迟到的 progress 复活"
    );
}

pub(super) async fn items_for(app: &axum::Router, token: &str) -> Value {    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/Items")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    json(response).await
}

#[tokio::test]
async fn items_include_playback_ticks_for_authenticated_user_only() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, compact) = app(tmp.path());
    let none = items_for(&app, "admin-token").await;
    assert_eq!(none["Items"][0]["UserData"]["PlaybackPositionTicks"], 0);

    let posted = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/Sessions/Playing/Progress")
                .header("authorization", "Bearer admin-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "ItemId": compact, "PositionTicks": 50_000_000 }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(posted.status(), StatusCode::NO_CONTENT);

    let mine = items_for(&app, "admin-token").await;
    assert_eq!(
        mine["Items"][0]["UserData"]["PlaybackPositionTicks"],
        50_000_000
    );

    // 新模型：/users 的 token 字段是初始密码；other 需登录拿到会话 token。
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/users")
                .header("authorization", "Bearer admin-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "login": "other", "password": "other-token" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let other_login = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/Users/AuthenticateByName")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "Username": "other", "Pw": "other-token" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let other_token = json(other_login).await["AccessToken"]
        .as_str()
        .unwrap()
        .to_string();
    let other = items_for(&app, &other_token).await;
    assert_eq!(other["Items"][0]["UserData"]["PlaybackPositionTicks"], 0);
}

#[tokio::test]
async fn stream_serves_ranges_and_full_file_without_buffering() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, compact) = app(tmp.path());
    // 带 Range → 206 + Content-Range，只回请求区间。
    let ranged = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/Videos/{compact}/stream"))
                .header("authorization", "Bearer admin-token")
                .header("Range", "bytes=2-5")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ranged.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        ranged.headers().get("content-range").unwrap(),
        "bytes 2-5/16"
    );
    assert_eq!(ranged.headers().get("content-length").unwrap(), "4");
    assert_eq!(ranged.headers().get("accept-ranges").unwrap(), "bytes");
    let body = to_bytes(ranged.into_body(), 1024).await.unwrap();
    assert_eq!(&body[..], b"2345");

    // Range: bytes=0-（整片）也走流式，Content-Length = 文件大小。
    let whole = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/Videos/{compact}/stream"))
                .header("authorization", "Bearer admin-token")
                .header("Range", "bytes=0-")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(whole.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(whole.headers().get("content-length").unwrap(), "16");
    let body = to_bytes(whole.into_body(), 1024).await.unwrap();
    assert_eq!(&body[..], b"0123456789abcdef");

    // 无 Range → 200 + 整文件。
    let plain = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/Videos/{compact}/stream"))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(plain.status(), StatusCode::OK);
    assert_eq!(plain.headers().get("content-length").unwrap(), "16");
    let body = to_bytes(plain.into_body(), 1024).await.unwrap();
    assert_eq!(&body[..], b"0123456789abcdef");
}

#[tokio::test]
async fn stream_serves_suffix_range_and_rejects_unsatisfiable_range() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, compact) = app(tmp.path());
    let suffix = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/Videos/{compact}/stream"))
                .header("authorization", "Bearer admin-token")
                .header("Range", "bytes=-4")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(suffix.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(suffix.headers()["content-range"], "bytes 12-15/16");
    assert_eq!(
        &to_bytes(suffix.into_body(), 1024).await.unwrap()[..],
        b"cdef"
    );
    let invalid = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/Videos/{compact}/stream"))
                .header("authorization", "Bearer admin-token")
                .header("Range", "bytes=16-")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(invalid.headers()["content-range"], "bytes */16");
}

#[tokio::test]
async fn strm_playback_exposes_the_direct_source_and_redirects_to_it() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, media_id, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let strm_path = tmp.path().join("remote.strm");
    let direct_url = "https://media.example.test/watch/episode.mp4?ticket=play-token";
    std::fs::write(&strm_path, direct_url).unwrap();
    let row = LedgerRow {
        id: LedgerId::new(),
        media_id,
        path: strm_path.display().to_string(),
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
    let item_id = row.id.to_string().replace('-', "");

    let info = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/Items/{item_id}/PlaybackInfo"))
                .header("authorization", "Bearer admin-token")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = json(info).await;
    assert_eq!(body["MediaSources"][0]["Path"], direct_url);
    assert_eq!(body["MediaSources"][0]["Protocol"], "Http");

    let stream = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/Videos/{item_id}/stream"))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stream.status(), StatusCode::FOUND);
    assert_eq!(stream.headers()["location"], direct_url);
}
