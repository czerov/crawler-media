use super::*;

#[tokio::test]
async fn public_system_info_and_ssdp_description_advertise_this_server() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/System/Info/Public")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let info = json(response).await;
    assert!(info["Id"].as_str().is_some_and(|id| !id.is_empty()));
    assert_eq!(info["ProductName"], "crawler-media");
    let emby_info = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/emby/System/Info/Public")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(emby_info.status(), StatusCode::OK);
    assert_eq!(json(emby_info).await["Id"], info["Id"]);
    let descriptor = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/jellyfin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(descriptor.status(), StatusCode::OK);
    let xml = to_bytes(descriptor.into_body(), 4096).await.unwrap();
    assert!(String::from_utf8_lossy(&xml).contains(info["Id"].as_str().unwrap()));
}

#[tokio::test]
async fn views_and_items_list_library_ledger() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, compact) = app(tmp.path());
    let views = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/UserViews")
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(views.status(), StatusCode::OK);
    let items = get(&app, "/Items").await;
    let body = json(items).await;
    assert_eq!(body["Items"][0]["Id"], compact);
    assert_eq!(body["Items"][0]["Name"], "The Matrix");
}

#[tokio::test]
async fn user_views_hide_libraries_excluded_from_home_unless_requested() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let hidden_root = tmp.path().join("hidden");
    std::fs::create_dir_all(&hidden_root).unwrap();
    let hidden = store
        .create_library(
            MediaKind::Movie,
            "Hidden films",
            &[hidden_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    store
        .set_library_switch_settings(&hidden.id, None, None, None, Some(true), None)
        .unwrap();

    let default_views = json(get(&app, "/UserViews").await).await;
    let default_ids: Vec<&str> = default_views["Items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["Id"].as_str())
        .collect();
    assert!(!default_ids.contains(&hidden.id.as_str()));

    let all_views = json(get(&app, "/UserViews?IncludeHidden=true").await).await;
    let all_ids: Vec<&str> = all_views["Items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["Id"].as_str())
        .collect();
    assert!(all_ids.contains(&hidden.id.as_str()));
}

#[tokio::test]
async fn items_sort_by_date_created_descending_and_emit_date_created() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let store = Store::open(tmp.path().join("data")).unwrap();
    let recent_media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Recently Ingested".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&recent_media).unwrap();
    let recent_file = tmp.path().join("recent.mkv");
    std::fs::write(&recent_file, b"new movie").unwrap();
    store
        .insert_ledger(&LedgerRow {
            id: LedgerId::new(),
            media_id: recent_media.id,
            path: recent_file.display().to_string(),
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

    let body = json(
        get(
            &app,
            "/Items?SortBy=DateCreated,SortName&SortOrder=Descending",
        )
        .await,
    )
    .await;
    let items = body["Items"].as_array().unwrap();
    assert_eq!(items[0]["Name"], "Recently Ingested");
    assert!(items.iter().all(|item| item["DateCreated"].is_string()));
    assert!(items[0]["DateCreated"].as_str().unwrap().ends_with('Z'));
    assert!(items[0]["DateCreated"].as_str() > items[1]["DateCreated"].as_str());
}

#[tokio::test]
async fn items_parent_id_limits_results_to_selected_library() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let movie_root = tmp.path().join("movie-view");
    let tv_root = tmp.path().join("tv-view");
    std::fs::create_dir_all(&movie_root).unwrap();
    std::fs::create_dir_all(&tv_root).unwrap();
    let movie = store
        .create_library(
            MediaKind::Movie,
            "Films",
            &[movie_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let tv = store
        .create_library(
            MediaKind::Tv,
            "Shows",
            &[tv_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    seed_library_items(&store, &movie_root, &tv_root);
    assert_movie_parent(&app, &movie.id, &movie_root).await;
    let series = json(get(&app, format!("/Items?ParentId={}", tv.id)).await).await;
    assert_eq!(series["Items"][0]["Type"], "Series");
    let series_only = json(get(&app, "/Items?IncludeItemTypes=Series").await).await;
    assert!(
        series_only["Items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["Type"] == "Series")
    );
    assert_eq!(series_only["TotalRecordCount"], 1);
    let series_id = series["Items"][0]["Id"].as_str().unwrap();
    assert_series_details(&app, series_id).await;
    assert_user_routes(&app, &movie.id).await;
    let show_episodes = assert_season_routes(&app, series_id).await;
    assert_next_up_and_streams(&app, &show_episodes).await;
}

async fn get(app: &axum::Router, uri: impl AsRef<str>) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri.as_ref())
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

fn seed_library_items(store: &Store, movie_root: &PathBuf, tv_root: &PathBuf) {
    for (kind, root, title, season, episode) in [
        (MediaKind::Movie, movie_root, "A Film", None, None),
        (MediaKind::Tv, tv_root, "A Show", Some(1), Some(1)),
    ] {
        let media = Media {
            id: MediaId::new(),
            kind,
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
        let file = root.join(format!("{title}.mkv"));
        std::fs::write(&file, b"video").unwrap();
        store
            .insert_ledger(&LedgerRow {
                id: LedgerId::new(),
                media_id: media.id,
                path: file.display().to_string(),
                season,
                episode,
                resolution: None,
                codec: None,
                hdr: None,
                quality_source: QualitySource::Probe,
                confidence: Confidence::High,
                filter_score: None,
            })
            .unwrap();
        if kind == MediaKind::Tv {
            seed_second_episode(store, tv_root, media.id);
        }
    }
}

fn seed_second_episode(store: &Store, tv_root: &Path, media_id: MediaId) {
    let second_episode = tv_root.join("A Show S02E01.mkv");
    std::fs::write(&second_episode, b"episode two").unwrap();
    store
        .insert_ledger(&LedgerRow {
            id: LedgerId::new(),
            media_id,
            path: second_episode.display().to_string(),
            season: Some(2),
            episode: Some(1),
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Probe,
            confidence: Confidence::High,
            filter_score: None,
        })
        .unwrap();
}

async fn assert_movie_parent(app: &axum::Router, movie_id: &str, movie_root: &Path) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/Items?ParentId={}", movie_id))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    let items = body["Items"].as_array().unwrap();
    assert!(!items.is_empty());
    assert!(items.iter().all(|item| {
        item["Path"]
            .as_str()
            .unwrap()
            .starts_with(movie_root.to_str().unwrap())
    }));
    assert!(items.iter().all(|item| item["Type"] == "Movie"));
}

async fn assert_series_details(app: &axum::Router, series_id: &str) {
    let episodes = json(get(&app, format!("/Items?ParentId={series_id}")).await).await;
    assert_eq!(episodes["Items"][0]["Type"], "Episode");
    assert_eq!(episodes["Items"][0]["SeriesId"], series_id);
    assert_eq!(episodes["Items"][0]["IndexNumber"], 1);
    assert_eq!(episodes["Items"][0]["Name"], "S01E01");
    let series_detail = json(get(&app, format!("/Items/{series_id}")).await).await;
    assert_eq!(series_detail["Type"], "Series");
    let user_id = "00000000-0000-0000-0000-000000000001";
    let user_series_detail = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/Users/{user_id}/Items/{series_id}"))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(user_series_detail.status(), StatusCode::OK);
    let user_series_detail = json(user_series_detail).await;
    assert_eq!(user_series_detail["Type"], "Series");
    assert_eq!(user_series_detail["Id"], series_id);
    let episode_id = episodes["Items"][0]["Id"].as_str().unwrap();
    let episode_detail = json(get(&app, format!("/Items/{episode_id}")).await).await;
    assert_eq!(episode_detail["Type"], "Episode");
}

async fn assert_user_routes(app: &axum::Router, movie_id: &str) {
    // VidHub 常用格式测试：带 /Users/{userId}/ 前缀的 API
    let user_id = "00000000-0000-0000-0000-000000000001";
    let user_views = json(get(&app, format!("/Users/{user_id}/Views")).await).await;
    // 默认库 2 个 + 上面创建的 2 个 = 4 个库
    assert_eq!(user_views["Items"].as_array().unwrap().len(), 4);

    let user_items = json(
        get(
            &app,
            format!("/Users/{user_id}/Items?ParentId={}", movie_id),
        )
        .await,
    )
    .await;
    assert_eq!(user_items["Items"].as_array().unwrap().len(), 1);
    assert_eq!(user_items["Items"][0]["Name"], "A Film");

    let user_profile = json(get(&app, format!("/Users/{user_id}")).await).await;
    assert_eq!(user_profile["Id"], user_id);
    assert_eq!(user_profile["Policy"]["IsAdministrator"], true);
}

async fn assert_season_routes(app: &axum::Router, series_id: &str) -> Value {
    // VidHub 统计与分季端点测试：
    let counts = json(get(&app, "/Items/Counts").await).await;
    // fixture 包含 initial seed 电影 Matrix (1) + 测试自建 A Film (1) = 2
    assert_eq!(counts["MovieCount"], 2);
    assert_eq!(counts["SeriesCount"], 1);
    assert_eq!(counts["EpisodeCount"], 2);

    let seasons = json(get(&app, format!("/Shows/{series_id}/Seasons")).await).await;
    assert_eq!(seasons["Items"].as_array().unwrap().len(), 2);
    assert_eq!(seasons["Items"][0]["IndexNumber"], 1);
    assert_eq!(seasons["Items"][1]["IndexNumber"], 2);

    let show_episodes = json(get(&app, format!("/Shows/{series_id}/Episodes")).await).await;
    assert_eq!(show_episodes["TotalRecordCount"], 2);
    assert_eq!(show_episodes["Items"][0]["IndexNumber"], 1);
    assert_eq!(show_episodes["Items"][1]["IndexNumber"], 1);
    assert_eq!(show_episodes["Items"][0]["ParentIndexNumber"], 1);
    assert_eq!(show_episodes["Items"][1]["ParentIndexNumber"], 2);

    let episode_page = json(
        get(
            &app,
            format!("/Shows/{series_id}/Episodes?StartIndex=1&Limit=1"),
        )
        .await,
    )
    .await;
    assert_eq!(episode_page["TotalRecordCount"], 2);
    assert_eq!(episode_page["Items"].as_array().unwrap().len(), 1);
    assert_eq!(episode_page["Items"][0]["ParentIndexNumber"], 2);

    let emby_detail = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/emby/Items/{series_id}"))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(emby_detail.status(), StatusCode::OK);
    assert_eq!(json(emby_detail).await["Type"], "Series");

    show_episodes
}

async fn assert_next_up_and_streams(app: &axum::Router, show_episodes: &Value) {
    let next_up = json(get(&app, "/Shows/NextUp").await).await;
    let episodes = show_episodes["Items"].as_array().unwrap();
    let first_episode = episodes
        .iter()
        .find(|episode| episode["ParentIndexNumber"] == 1)
        .unwrap();
    let second_episode = episodes
        .iter()
        .find(|episode| episode["ParentIndexNumber"] == 2)
        .unwrap();
    assert_eq!(next_up["Items"].as_array().unwrap().len(), 1);
    assert_eq!(next_up["Items"][0]["Id"], first_episode["Id"]);

    assert_episode_streams(app, first_episode).await;

    let mark_played = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/UserPlayedItems/{}",
                    first_episode["Id"].as_str().unwrap()
                ))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(mark_played.status(), StatusCode::OK);

    let next_up_after_play = json(get(&app, "/Shows/NextUp").await).await;
    assert_eq!(next_up_after_play["Items"].as_array().unwrap().len(), 1);
    assert_eq!(next_up_after_play["Items"][0]["Id"], second_episode["Id"]);
}

async fn assert_episode_streams(app: &axum::Router, first_episode: &Value) {
    let stream = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!(
                    "/Videos/{}/stream",
                    first_episode["Id"].as_str().unwrap()
                ))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stream.status(), StatusCode::OK);
    assert_eq!(stream.headers()["content-type"], "video/x-matroska");

    let ranged_stream = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!(
                    "/Videos/{}/stream",
                    first_episode["Id"].as_str().unwrap()
                ))
                .header("authorization", "Bearer admin-token")
                .header("range", "bytes=0-3")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ranged_stream.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(ranged_stream.headers()["content-type"], "video/x-matroska");
}
