use super::*;

#[tokio::test]
async fn selected_library_bytes_require_auth_and_visibility() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let hidden_root = tmp.path().join("hidden");
    std::fs::create_dir_all(&hidden_root).unwrap();
    let hidden_file = hidden_root.join("secret.mkv");
    std::fs::write(&hidden_file, b"secret media").unwrap();
    std::fs::write(hidden_root.join("poster.jpg"), b"secret poster").unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    store
        .create_library(
            MediaKind::Movie,
            "仅管理员可见",
            &[hidden_root.to_str().unwrap()],
            "selected",
            true,
            &[],
        )
        .unwrap();
    let hidden_media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Hidden Movie".into(),
        year: None,
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&hidden_media).unwrap();
    let hidden_row = LedgerRow {
        id: LedgerId::new(),
        media_id: hidden_media.id,
        path: hidden_file.display().to_string(),
        season: None,
        episode: None,
        resolution: Some("1080p".into()),
        codec: None,
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: None,
    };
    store.insert_ledger(&hidden_row).unwrap();
    drop(store);
    let created = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/users")
                .header("authorization", "Bearer admin-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "login": "member", "password": "member-pass" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let login = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/Users/AuthenticateByName")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "Username": "member", "Pw": "member-pass" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let token = json(login).await["AccessToken"]
        .as_str()
        .unwrap()
        .to_string();
    let member_id = Store::open(tmp.path().join("data"))
        .unwrap()
        .user_by_token(&token)
        .unwrap()
        .unwrap()
        .id;
    let hidden_id = hidden_row.id.to_string().replace('-', "");
    let hidden_media_id = hidden_media.id.to_string().replace('-', "");
    let items = super::playback::items_for(&app, &token).await;
    assert!(
        items["Items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["Id"] != hidden_id)
    );
    let anonymous = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/Videos/{hidden_id}/stream"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
    let member_stream = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/Videos/{hidden_id}/stream"))
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(member_stream.status(), StatusCode::NOT_FOUND);
    let member_poster = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/Items/{hidden_id}/Images/Primary"))
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(member_poster.status(), StatusCode::NOT_FOUND);
    let anonymous_poster = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/Items/{hidden_id}/Images/Primary"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(anonymous_poster.status(), StatusCode::OK);
    assert_eq!(
        &to_bytes(anonymous_poster.into_body(), usize::MAX)
            .await
            .unwrap()[..],
        b"secret poster"
    );
    let hidden_mark = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/Users/{member_id}/FavoriteItems/{hidden_media_id}"
                ))
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(hidden_mark.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        Store::open(tmp.path().join("data"))
            .unwrap()
            .playback_marks(member_id, hidden_media.id)
            .unwrap(),
        None
    );
    let stream = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/Videos/{hidden_id}/stream?api_key=admin-token"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stream.status(), StatusCode::OK);
    assert_eq!(
        &to_bytes(stream.into_body(), usize::MAX).await.unwrap()[..],
        b"secret media"
    );
    let poster = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!(
                    "/Items/{hidden_id}/Images/Primary?api_key=admin-token"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(poster.status(), StatusCode::OK);
    assert_eq!(
        &to_bytes(poster.into_body(), usize::MAX).await.unwrap()[..],
        b"secret poster"
    );
}
#[tokio::test]
async fn items_series_counts_only_episodes_from_visible_libraries() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let visible_root = tmp.path().join("visible-tv");
    let hidden_root = tmp.path().join("hidden-tv");
    std::fs::create_dir_all(&visible_root).unwrap();
    std::fs::create_dir_all(&hidden_root).unwrap();
    store
        .create_library(
            MediaKind::Tv,
            "Visible shows",
            &[visible_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    store
        .create_library(
            MediaKind::Tv,
            "Restricted shows",
            &[hidden_root.to_str().unwrap()],
            "selected",
            true,
            &[],
        )
        .unwrap();
    let show = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "Shared series".into(),
        year: None,
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&show).unwrap();
    for (root, episode) in [(&visible_root, 1), (&hidden_root, 2)] {
        let path = root.join(format!("episode-{episode}.mkv"));
        std::fs::write(&path, b"episode").unwrap();
        store
            .insert_ledger(&LedgerRow {
                id: LedgerId::new(),
                media_id: show.id,
                path: path.display().to_string(),
                season: Some(1),
                episode: Some(episode),
                resolution: None,
                codec: None,
                hdr: None,
                quality_source: QualitySource::Probe,
                confidence: Confidence::High,
                filter_score: None,
            })
            .unwrap();
    }
    drop(store);

    let created = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/users")
                .header("authorization", "Bearer admin-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "login": "series-member", "password": "member-pass" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let login = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/Users/AuthenticateByName")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "Username": "series-member", "Pw": "member-pass" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let token = json(login).await["AccessToken"].as_str().unwrap().to_string();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let member = store.user_by_token(&token).unwrap().unwrap();
    store
        .update_library(
            &store
                .list_libraries()
                .unwrap()
                .into_iter()
                .find(|library| library.name == "Restricted shows")
                .unwrap()
                .id,
            None,
            None,
            Some(("selected", true, &[])),
        )
        .unwrap();
    drop(store);
    let items = app
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
    assert_eq!(items.status(), StatusCode::OK);
    let items = json(items).await;
    let series = items["Items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["Name"] == "Shared series")
        .unwrap();
    assert_eq!(series["RecursiveItemCount"], 1);
}


#[test]
fn debug_store_open_creates_subscribes() {
    let tmp = tempfile::tempdir().unwrap();
    let store = api::Store::open(tmp.path().join("data")).unwrap();
    let rows = store.list_all_subscribes().unwrap();
    assert_eq!(rows.len(), 0);
}
