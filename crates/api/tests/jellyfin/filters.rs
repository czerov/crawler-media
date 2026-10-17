use super::*;

#[tokio::test]
async fn items_series_and_season_filters_return_episode_children() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("tv");
    std::fs::create_dir_all(&root).unwrap();
    store
        .create_library(
            MediaKind::Tv,
            "Shows",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let show = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "Filter Show".into(),
        year: Some(2024),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&show).unwrap();
    for (season, episode) in [(1, 1), (1, 2), (2, 1)] {
        let path = root.join(format!("S{season:02}E{episode:02}.mkv"));
        std::fs::write(&path, b"episode").unwrap();
        store
            .insert_ledger(&LedgerRow {
                id: LedgerId::new(),
                media_id: show.id,
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

    let series_id = show.id.to_string().replace('-', "");
    let all_episodes = get_json(&app, &format!("/Items?SeriesId={series_id}")).await;
    assert_eq!(all_episodes["TotalRecordCount"], 3);
    assert!(
        all_episodes["Items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["Type"] == "Episode")
    );

    let season_id = format!("{series_id}02");
    let season_episodes = get_json(
        &app,
        &format!("/Items?SeriesId={series_id}&SeasonId={season_id}"),
    )
    .await;
    assert_eq!(season_episodes["TotalRecordCount"], 1);
    assert_eq!(season_episodes["Items"][0]["ParentIndexNumber"], 2);
}

#[tokio::test]
async fn item_mark_filters_follow_favorite_and_played_mutations() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, item_id) = app(tmp.path());

    assert_filter_count(&app, "IsFavorite", 0).await;
    assert_filter_count(&app, "IsPlayed", 0).await;
    assert_filter_count(&app, "IsUnplayed", 1).await;
    assert_query_count(&app, "isFavorite=true", 0).await;
    assert_query_count(&app, "isFavorite=false", 1).await;
    assert_query_count(&app, "isPlayed=true", 0).await;
    assert_query_count(&app, "isPlayed=false", 1).await;

    for mark in ["FavoriteItems", "PlayedItems"] {
        update_item_mark(&app, axum::http::Method::POST, mark, &item_id).await;
    }

    assert_filter_count(&app, "IsFavorite", 1).await;
    assert_filter_count(&app, "IsPlayed", 1).await;
    assert_filter_count(&app, "IsUnplayed", 0).await;
    assert_query_count(&app, "isFavorite=true", 1).await;
    assert_query_count(&app, "isFavorite=false", 0).await;
    assert_query_count(&app, "isPlayed=true", 1).await;
    assert_query_count(&app, "isPlayed=false", 0).await;

    for mark in ["FavoriteItems", "PlayedItems"] {
        update_item_mark(&app, axum::http::Method::DELETE, mark, &item_id).await;
    }

    assert_filter_count(&app, "IsFavorite", 0).await;
    assert_filter_count(&app, "IsPlayed", 0).await;
    assert_filter_count(&app, "IsUnplayed", 1).await;
}

async fn assert_filter_count(app: &axum::Router, filter: &str, expected: usize) {
    assert_query_count(app, &format!("Filters={filter}"), expected).await;
}

async fn assert_query_count(app: &axum::Router, query: &str, expected: usize) {
    let response = get_json(app, &format!("/Users/admin/Items?{query}")).await;
    assert_eq!(response["TotalRecordCount"], expected, "query={query}");
    assert_eq!(
        response["Items"].as_array().unwrap().len(),
        expected,
        "query={query}"
    );
}

async fn update_item_mark(
    app: &axum::Router,
    method: axum::http::Method,
    mark: &str,
    item_id: &str,
) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(format!("/User{mark}/{item_id}"))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{mark}");
}

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
    assert_eq!(response.status(), StatusCode::OK, "{uri}");
    json(response).await
}
