use super::*;

const ADMIN_ID: &str = "00000000-0000-0000-0000-000000000001";

async fn get_json(app: &axum::Router, uri: &str) -> Value {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    json(response).await
}

async fn mark_request(app: &axum::Router, method: axum::http::Method, uri: &str) -> Value {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{uri}");
    json(response).await
}

fn insert_episode(store: &Store, root: &Path, media_id: MediaId, season: u32, episode: u32) {
    let path = root.join(format!("Show S{season:02}E{episode:02}.mkv"));
    std::fs::write(&path, b"episode").unwrap();
    store
        .insert_ledger(&LedgerRow {
            id: LedgerId::new(),
            media_id,
            path: path.display().to_string(),
            season: Some(season),
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

/// 加一部全新的、没看过的电影，返回它的条目 id（`/UserPlayedItems/{id}` 用的是它）。
/// Latest 的用例都靠"库里既有看过的、也有没看过的"来分辨「严格筛」与「只排不筛」。
fn insert_unplayed_movie(store: &Store, root: &Path, title: &str) -> String {
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: title.into(),
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
    let path = root.join(format!("{}.mkv", title.replace(' ', "-").to_lowercase()));
    std::fs::write(&path, b"copy").unwrap();
    let row = LedgerRow {
        id: LedgerId::new(),
        media_id: media.id,
        path: path.display().to_string(),
        season: None,
        episode: None,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: None,
    };
    let compact = row.id.to_string().replace('-', "");
    store.insert_ledger(&row).unwrap();
    compact
}

#[tokio::test]
async fn ping_is_public_on_root_and_emby_prefix_for_both_methods() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());

    for method in [axum::http::Method::GET, axum::http::Method::POST] {
        for path in ["/System/Ping", "/emby/System/Ping"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method.clone())
                        .uri(path)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{method} {path}");
            assert!(to_bytes(response.into_body(), 64).await.unwrap().is_empty());
        }
    }
}

#[tokio::test]
async fn user_me_and_user_prefixed_counts_return_profile_and_catalog_facts() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let tv_root = tmp.path().join("tv");
    std::fs::create_dir_all(&tv_root).unwrap();
    store
        .create_library(
            MediaKind::Tv,
            "Shows",
            &[tv_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let show = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "Show".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&show).unwrap();
    insert_episode(&store, &tv_root, show.id, 1, 1);
    insert_episode(&store, &tv_root, show.id, 2, 1);

    let profile = get_json(&app, "/Users/Me").await;
    assert_eq!(profile["Id"], ADMIN_ID);
    assert_eq!(profile["Name"], "admin");
    assert_eq!(profile["Policy"]["IsAdministrator"], true);

    let counts = get_json(&app, &format!("/Users/{ADMIN_ID}/Items/Counts")).await;
    assert_eq!(counts["MovieCount"], 1);
    assert_eq!(counts["SeriesCount"], 1);
    assert_eq!(counts["EpisodeCount"], 2);
    assert_eq!(counts["ItemCount"], 2);
}

#[tokio::test]
async fn latest_items_returns_date_order_and_honors_start_index_and_limit() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, media_id, seeded_id) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let mut added_ids = Vec::new();
    for name in ["matrix-copy-1.mkv", "matrix-copy-2.mkv"] {
        let path = tmp.path().join(name);
        std::fs::write(&path, b"copy").unwrap();
        let id = LedgerId::new();
        store
            .insert_ledger(&LedgerRow {
                id,
                media_id,
                path: path.display().to_string(),
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
        added_ids.push(id.to_string().replace('-', ""));
    }
    let catalog = get_json(&app, "/Items").await;
    let catalog_ids: Vec<String> = catalog["Items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["Id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(catalog_ids.len(), 3);
    assert!(catalog_ids.contains(&seeded_id));
    for id in added_ids {
        assert!(catalog_ids.contains(&id));
    }

    let latest = get_json(&app, &format!("/Users/{ADMIN_ID}/Items/Latest")).await;
    let expected_latest = catalog_ids;
    let latest_ids: Vec<String> = latest
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["Id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(latest_ids, expected_latest);

    let page = get_json(
        &app,
        &format!("/Users/{ADMIN_ID}/Items/Latest?StartIndex=1&Limit=1"),
    )
    .await;
    assert_eq!(page.as_array().unwrap().len(), 1);
    assert_eq!(page[0]["Id"], latest[1]["Id"]);
}

#[tokio::test]
async fn latest_items_honors_is_played_filter() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, played_item_id) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    insert_unplayed_movie(&store, tmp.path(), "Not Watched");

    mark_played(&app, &played_item_id).await;

    let unplayed_items = get_json(&app, "/Users/Me/Items/Latest?IsPlayed=false").await;
    assert_eq!(unplayed_items.as_array().unwrap().len(), 1);
    assert_eq!(unplayed_items[0]["Name"], "Not Watched");
    assert_eq!(unplayed_items[0]["UserData"]["Played"], false);

    let played_items = get_json(&app, "/Users/Me/Items/Latest?IsPlayed=true").await;
    assert_eq!(played_items.as_array().unwrap().len(), 1);
    assert_eq!(played_items[0]["Id"], played_item_id);
    assert_eq!(played_items[0]["UserData"]["Played"], true);
}

/// 客户端不传 `IsPlayed`（Jellyfin 的 web/客户端就是这么调的）：**只排不筛**——
/// 已看完的还在名单里，只是沉底；刚入库但看完了的那部不会顶在没看过的前面。
#[tokio::test]
async fn latest_items_lists_unwatched_first_when_is_played_is_omitted() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, matrix_id) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    // 新加一部（比 The Matrix 新）并把它标成已看完：纯按入库时间它该排第一
    let alpha_id = insert_unplayed_movie(&store, tmp.path(), "Alpha");
    mark_played(&app, &alpha_id).await;

    let latest = get_json(&app, "/Users/Me/Items/Latest").await;
    let items = latest.as_array().unwrap();
    assert_eq!(items.len(), 2, "缺省只排不筛：已看完的也留着");
    assert_eq!(items[0]["Id"], matrix_id, "没看完的在前");
    assert_eq!(items[0]["UserData"]["Played"], false);
    assert_eq!(items[1]["Id"], alpha_id, "刚入库但已看完的沉底");
    assert_eq!(items[1]["UserData"]["Played"], true);
}

/// 整个库都看过了：缺省照样回全部（都在"已看完"那一档，段内还是入库时间倒序），
/// 而不是像 Jellyfin 那样回空列表——「看完了的库」在客户端首页也得有入口。
#[tokio::test]
async fn latest_items_stays_populated_when_everything_is_played() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, played_item_id) = app(tmp.path());
    mark_played(&app, &played_item_id).await;

    let latest = get_json(&app, "/Users/Me/Items/Latest").await;
    let items = latest.as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["Id"], played_item_id);

    // 显式 IsPlayed=false 仍是严格筛：只排不筛只发生在"客户端没指定"的时候
    let strict = get_json(&app, "/Users/Me/Items/Latest?IsPlayed=false").await;
    assert!(strict.as_array().unwrap().is_empty());
}

async fn mark_played(app: &axum::Router, item_id: &str) {
    let played = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/UserPlayedItems/{item_id}"))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(played.status(), StatusCode::OK);
}

#[tokio::test]
async fn quickconnect_and_display_preferences_match_protocol_responses() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());

    let quickconnect = get_json(&app, "/QuickConnect/Enabled").await;
    assert_eq!(quickconnect, json!(false));

    let preferences = get_json(&app, "/DisplayPreferences/vdh-client").await;
    assert_eq!(preferences["Id"], "vdh-client");
    assert_eq!(preferences["SortBy"], "SortName");
    assert_eq!(preferences["SortOrder"], "Ascending");
    assert_eq!(preferences["PrimaryImageWidth"], 250);
    assert_eq!(preferences["PrimaryImageHeight"], 250);
    assert_eq!(preferences["Client"], "emby");

    let updated = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/DisplayPreferences/vdh-client")
                .header("authorization", "Bearer admin-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "SortBy": "DateCreated", "SortOrder": "Descending" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(updated.status(), StatusCode::NO_CONTENT);
    assert!(to_bytes(updated.into_body(), 64).await.unwrap().is_empty());
}

#[tokio::test]
async fn favorite_and_played_delete_aliases_clear_user_marks() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, media_id, item_id) = app(tmp.path());
    let routes = [
        (
            format!("/Users/{ADMIN_ID}/FavoriteItems/{item_id}"),
            "IsFavorite",
        ),
        (format!("/UserFavoriteItems/{item_id}"), "IsFavorite"),
        (format!("/Users/{ADMIN_ID}/PlayedItems/{item_id}"), "Played"),
        (format!("/UserPlayedItems/{item_id}"), "Played"),
    ];

    for (uri, field) in routes {
        let marked = mark_request(&app, axum::http::Method::POST, &uri).await;
        assert_eq!(marked[field], true, "POST {uri}");
        assert_eq!(marked["Key"], item_id, "POST {uri}");
        assert_eq!(marked["ItemId"], item_id, "POST {uri}");
        assert!(marked["PlaybackPositionTicks"].is_number(), "POST {uri}");
        assert!(marked["PlayCount"].is_number(), "POST {uri}");
        assert_eq!(marked["Played"], field == "Played", "POST {uri}");
        let deleted = mark_request(&app, axum::http::Method::DELETE, &uri).await;
        assert_eq!(deleted[field], false, "DELETE {uri}");
        assert_eq!(deleted["Key"], item_id, "DELETE {uri}");
        assert_eq!(deleted["ItemId"], item_id, "DELETE {uri}");
        assert!(deleted["PlaybackPositionTicks"].is_number(), "DELETE {uri}");
        assert!(deleted["PlayCount"].is_number(), "DELETE {uri}");
        assert_eq!(deleted["Played"], false, "DELETE {uri}");
    }

    let user_id = domain::UserId::from_str(ADMIN_ID).unwrap();
    let state = Store::open(tmp.path().join("data"))
        .unwrap()
        .unit_state(user_id, media_id, -1, -1)
        .unwrap()
        .unwrap();
    assert!(!state.played);
    assert!(!state.favorite);
}

async fn emby_login(app: &axum::Router) -> String {
    let login = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/emby/Users/AuthenticateByName")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "Username": "admin", "Pw": "admin-token" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    let login = json(login).await;
    assert_eq!(login["User"]["Name"], "admin");
    login["AccessToken"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn emby_prefix_serves_public_info_and_authenticated_catalog_routes() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, item_id) = app(tmp.path());

    let public_info = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/emby/System/Info/Public")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(public_info.status(), StatusCode::OK);
    assert_eq!(json(public_info).await["ProductName"], "crawler-media");

    let token = emby_login(&app).await;

    let profile = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/emby/Users/Me")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(profile.status(), StatusCode::OK);
    assert_eq!(json(profile).await["Id"], ADMIN_ID);

    let items = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/emby/Items")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(json(items).await["Items"][0]["Name"], "The Matrix");

    let detail = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/emby/Users/{ADMIN_ID}/Items/{item_id}"))
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(detail.status(), StatusCode::OK);
    assert_eq!(json(detail).await["Name"], "The Matrix");
}

#[tokio::test]
async fn emby_prefix_serves_playback_and_public_image_routes() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, item_id) = app(tmp.path());
    let poster = b"\xff\xd8\xff\xdbemby-poster";
    std::fs::write(tmp.path().join("poster.jpg"), poster).unwrap();
    let token = emby_login(&app).await;

    let playback = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/emby/Items/{item_id}/PlaybackInfo"))
                .header("authorization", format!("Bearer {token}"))
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(playback.status(), StatusCode::OK);
    assert_eq!(
        json(playback).await["MediaSources"][0]["SupportsDirectPlay"],
        true
    );

    let image = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/emby/Items/{item_id}/Images/Primary"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(image.status(), StatusCode::OK);
    assert_eq!(image.headers()["content-type"], "image/jpeg");
    assert_eq!(
        &to_bytes(image.into_body(), 1024).await.unwrap()[..],
        poster
    );
}

#[tokio::test]
async fn emby_prefix_stream_route_serves_requested_byte_range() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, item_id) = app(tmp.path());
    let token = emby_login(&app).await;
    let stream = app
        .oneshot(
            Request::builder()
                .uri(format!("/emby/Videos/{item_id}/stream"))
                .header("authorization", format!("Bearer {token}"))
                .header("range", "bytes=1-3")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stream.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(stream.headers()["content-range"], "bytes 1-3/16");
    assert_eq!(&to_bytes(stream.into_body(), 16).await.unwrap()[..], b"123");
}

#[tokio::test]
async fn apikey_query_auth_accepts_canonical_jellyfin_casing_for_favorites() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, item_id) = app(tmp.path());

    for path in [
        format!("/UserFavoriteItems/{item_id}"),
        format!("/emby/Users/{ADMIN_ID}/FavoriteItems/{item_id}"),
    ] {
        for (method, expected) in [
            (axum::http::Method::POST, true),
            (axum::http::Method::DELETE, false),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method.clone())
                        .uri(format!("{path}?ApiKey=admin-token"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{method} {path}");
            assert_eq!(
                json(response).await["IsFavorite"],
                expected,
                "{method} {path}"
            );
        }
    }
}
