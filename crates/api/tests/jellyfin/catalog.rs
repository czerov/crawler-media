use super::*;
#[tokio::test]
async fn season_played_mark_updates_episode_rollup_and_next_up() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("tv-season-mark");
    std::fs::create_dir_all(&root).unwrap();
    store.create_library(MediaKind::Tv, "Season mark shows", &[root.to_str().unwrap()], "everyone", true, &[]).unwrap();
    let show = new_media(MediaKind::Tv, "Season mark show");
    store.insert_media(&show).unwrap();
    let first = insert_episode(&store, &root, show.id, 1, 1);
    insert_episode(&store, &root, show.id, 1, 2);
    drop(store);
    let season_id = get_json(&app, &format!("/Shows/{}/Seasons", show.id.to_string().replace('-', ""))).await["Items"][0]["Id"].as_str().unwrap().to_owned();
    let marked = app.clone().oneshot(Request::builder().method("POST").uri(format!("/UserPlayedItems/{season_id}")).header("authorization", "Bearer admin-token").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(marked.status(), StatusCode::OK);
    let episode = get_json(&app, &format!("/Items/{}", first.to_string().replace('-', ""))).await;
    assert_eq!(episode["UserData"]["Played"], true);
    let listing = get_json(&app, "/Items").await;
    let series = listing["Items"].as_array().unwrap().iter().find(|item| item["Name"] == "Season mark show").unwrap();
    assert_eq!(series["UserData"]["Played"], true);
    assert_eq!(series["UserData"]["UnplayedItemCount"], 0);
    assert!(get_json(&app, "/Shows/NextUp").await["Items"].as_array().unwrap().is_empty());
}
#[tokio::test]
async fn library_episode_expansion_respects_nested_and_default_library_ownership() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let parent_root = tmp.path().join("tv");
    let child_root = parent_root.join("child");
    std::fs::create_dir_all(&child_root).unwrap();
    let parent = store.create_library(MediaKind::Tv, "Parent shows", &[parent_root.to_str().unwrap()], "everyone", true, &[]).unwrap();
    let child = store.create_library(MediaKind::Tv, "Child shows", &[child_root.to_str().unwrap()], "everyone", true, &[]).unwrap();
    store.set_default_library(&parent.id).unwrap();
    let show = new_media(MediaKind::Tv, "Nested show");
    store.insert_media(&show).unwrap();
    let parent_episode = insert_episode(&store, &parent_root, show.id, 1, 1);
    let child_episode = insert_episode(&store, &child_root, show.id, 1, 2);
    let outside_root = tmp.path().join("unowned-tv-path");
    std::fs::create_dir_all(&outside_root).unwrap();
    let outside_episode = insert_episode(&store, &outside_root, show.id, 1, 3);
    drop(store);

    let child_listing = get_json(&app, &format!("/Items?ParentId={}&Recursive=true&IncludeItemTypes=Episode", child.id)).await;
    let child_episodes = child_listing["Items"].as_array().unwrap();
    assert_eq!(child_episodes.len(), 1);
    assert_eq!(child_episodes[0]["Id"], child_episode.to_string().replace('-', ""));

    let parent_listing = get_json(&app, &format!("/Items?ParentId={}&Recursive=true&IncludeItemTypes=Episode", parent.id)).await;
    let parent_episodes = parent_listing["Items"].as_array().unwrap();
    let parent_episode_ids = parent_episodes.iter().map(|episode| episode["Id"].as_str().unwrap()).collect::<std::collections::HashSet<_>>();
    assert_eq!(parent_episodes.len(), 2);
    assert!(parent_episode_ids.contains(parent_episode.to_string().replace('-', "").as_str()));
    assert!(parent_episode_ids.contains(outside_episode.to_string().replace('-', "").as_str()));
    assert!(!parent_episode_ids.contains(child_episode.to_string().replace('-', "").as_str()));
}

#[tokio::test]
async fn folder_episode_expansion_stays_within_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root_a = tmp.path().join("tv-a");
    let root_b = tmp.path().join("tv-b");
    std::fs::create_dir_all(&root_a).unwrap();
    std::fs::create_dir_all(&root_b).unwrap();
    let library = store.create_library(
        MediaKind::Tv,
        "Shows",
        &[root_a.to_str().unwrap(), root_b.to_str().unwrap()],
        "everyone",
        true,
        &[],
    ).unwrap();
    store.set_default_library(&library.id).unwrap();
    let show = new_media(MediaKind::Tv, "Split show");
    store.insert_media(&show).unwrap();
    let episode_a = insert_episode(&store, &root_a, show.id, 1, 1);
    let episode_b = insert_episode(&store, &root_b, show.id, 1, 2);
    drop(store);

    let folder_id = |root: &Path| format!(
        "folder_{}",
        uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, root.to_string_lossy().as_bytes()).simple()
    );
    let folder_listing = get_json(
        &app,
        &format!("/Items?ParentId={}&Recursive=true&IncludeItemTypes=Episode", folder_id(&root_a)),
    ).await;
    let folder_episodes = folder_listing["Items"].as_array().unwrap();
    assert_eq!(folder_episodes.len(), 1);
    assert_eq!(folder_episodes[0]["Id"], episode_a.to_string().replace('-', ""));
    let folder_b_listing = get_json(
        &app,
        &format!("/Items?ParentId={}&Recursive=true&IncludeItemTypes=Episode", folder_id(&root_b)),
    ).await;
    assert_eq!(folder_b_listing["Items"].as_array().unwrap().len(), 1);
    assert_eq!(folder_b_listing["Items"][0]["Id"], episode_b.to_string().replace('-', ""));
    assert_eq!(folder_b_listing["TotalRecordCount"], 1);
    assert_eq!(folder_listing["TotalRecordCount"], 1);
    let all_listing = get_json(&app, "/Items?Recursive=true&IncludeItemTypes=Episode").await;
    let all_episodes = all_listing["Items"].as_array().unwrap();
    assert_eq!(all_episodes.len(), 2);

    let series_listing = get_json(
        &app,
        &format!("/Items?ParentId={}&Recursive=true&IncludeItemTypes=Episode", show.id),
    ).await;
    let series_episodes = series_listing["Items"].as_array().unwrap();
    assert_eq!(series_episodes.len(), 2);
    assert!(series_episodes.iter().any(|episode| episode["Id"] == episode_b.to_string().replace('-', "")));
}
#[tokio::test]
async fn folder_series_id_returns_only_series_episodes_in_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root_a = tmp.path().join("tv-series-scope-a");
    let root_b = tmp.path().join("tv-series-scope-b");
    std::fs::create_dir_all(&root_a).unwrap();
    std::fs::create_dir_all(&root_b).unwrap();
    let library = store.create_library(
        MediaKind::Tv,
        "Series scope shows",
        &[root_a.to_str().unwrap(), root_b.to_str().unwrap()],
        "everyone",
        true,
        &[],
    ).unwrap();
    store.set_default_library(&library.id).unwrap();
    let show = new_media(MediaKind::Tv, "Folder constrained show");
    store.insert_media(&show).unwrap();
    let episode_a = insert_episode(&store, &root_a, show.id, 1, 1);
    insert_episode(&store, &root_b, show.id, 1, 2);
    drop(store);

    let folder_id = format!(
        "folder_{}",
        uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, root_a.to_string_lossy().as_bytes()).simple()
    );
    let listing = get_json(
        &app,
        &format!("/Items?ParentId={folder_id}&SeriesId={}&Recursive=true&IncludeItemTypes=Episode", show.id),
    ).await;
    let episodes = listing["Items"].as_array().unwrap();
    assert_eq!(episodes.len(), 1);
    assert_eq!(episodes[0]["Id"], episode_a.to_string().replace('-', ""));
    assert_eq!(listing["TotalRecordCount"], 1);
}

#[tokio::test]
async fn series_and_episode_types_return_both_series_and_episodes() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("tv-series-and-episode");
    std::fs::create_dir_all(&root).unwrap();
    let library = store.create_library(MediaKind::Tv, "Combined types", &[root.to_str().unwrap()], "everyone", true, &[]).unwrap();
    let show = new_media(MediaKind::Tv, "Combined type show");
    store.insert_media(&show).unwrap();
    let episode = insert_episode(&store, &root, show.id, 1, 1);
    drop(store);

    // This fixture has one visible series row and its one episode row.
    let combined = get_json(&app, &format!("/Items?ParentId={}&Recursive=true&IncludeItemTypes=Series,Episode", library.id)).await;
    let combined_items = combined["Items"].as_array().unwrap();
    assert_eq!(combined_items.len(), 2);
    assert_eq!(combined_items.iter().filter(|item| item["Type"] == "Series").count(), 1);
    assert_eq!(combined_items.iter().filter(|item| item["Type"] == "Episode").count(), 1);
    assert!(combined_items.iter().any(|item| item["Id"] == show.id.to_string().replace('-', "")));
    assert!(combined_items.iter().any(|item| item["Id"] == episode.to_string().replace('-', "")));
    assert_eq!(combined["TotalRecordCount"], 2);

    let episodes_only = get_json(&app, "/Items?Recursive=true&IncludeItemTypes=Episode").await;
    assert_eq!(episodes_only["Items"].as_array().unwrap().len(), 1);
    assert_eq!(episodes_only["Items"][0]["Id"], episode.to_string().replace('-', ""));
}


#[tokio::test]
async fn newer_show_played_mark_supersedes_older_unplayed_season_everywhere() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("tv-precedence");
    std::fs::create_dir_all(&root).unwrap();
    store.create_library(MediaKind::Tv, "Precedence", &[root.to_str().unwrap()], "everyone", true, &[]).unwrap();
    let show = new_media(MediaKind::Tv, "Precedence show");
    store.insert_media(&show).unwrap();
    let episode = insert_episode(&store, &root, show.id, 1, 1);
    let user = store.admin_user_id().unwrap();
    store.upsert_unit(user, show.id, 1, -1, 0, Some(false), None, None, None, None, false, 10).unwrap();
    store.upsert_unit(user, show.id, -1, -1, 0, Some(true), None, None, None, None, false, 20).unwrap();
    drop(store);
    assert_eq!(get_json(&app, &format!("/Items/{}", episode.to_string().replace('-', ""))).await["UserData"]["Played"], true);
    assert!(get_json(&app, "/Shows/NextUp").await["Items"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn resume_hides_episode_whose_effective_played_comes_from_a_newer_show_mark() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("tv-resume-effective");
    std::fs::create_dir_all(&root).unwrap();
    store
        .create_library(
            MediaKind::Tv,
            "Resume effective",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let show = new_media(MediaKind::Tv, "Resume effective show");
    store.insert_media(&show).unwrap();
    let episode = insert_episode(&store, &root, show.id, 1, 1);
    let user = store.admin_user_id().unwrap();
    // Raw episode unit is still in progress. A later whole-show mark is the
    // only source of played=true; resume SQL sees only the raw unit.
    store
        .upsert_unit(user, show.id, 1, 1, 7_500, Some(false), None, None, None, None, false, 10)
        .unwrap();
    store
        .upsert_unit(user, show.id, -1, -1, 0, Some(true), None, None, None, None, false, 20)
        .unwrap();
    let raw = store.user_resumable_units(user).unwrap();
    let raw_played = store.unit_state(user, show.id, 1, 1).unwrap().unwrap().played;
    drop(store);
    let episode_id = episode.to_string().replace('-', "");
    let detail = get_json(&app, &format!("/Items/{episode_id}")).await;
    let resumed = get_json(
        &app,
        "/Users/00000000-0000-0000-0000-000000000001/Items/Resume",
    )
    .await;
    let listed = resumed["Items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["Id"] == episode_id);
    assert!(
        raw.iter().any(|row| row.0 == show.id && row.1 == 1 && row.2 == 1),
        "raw resume query must admit the in-progress episode, got {raw:?}"
    );
    assert!(!raw_played, "raw episode unit must stay played=false");
    assert_eq!(detail["UserData"]["Played"], true, "effective played must be true");
    assert!(
        !listed,
        "resume must not return an item whose UserData.Played is true"
    );
}

#[tokio::test]
async fn resume_fills_the_page_after_dropping_effectively_played_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("tv-resume-page");
    std::fs::create_dir_all(&root).unwrap();
    store
        .create_library(MediaKind::Tv, "Resume page", &[root.to_str().unwrap()], "everyone", true, &[])
        .unwrap();
    let user = store.admin_user_id().unwrap();
    let mut expected = Vec::new();
    for index in 0..3 {
        let show = new_media(MediaKind::Tv, &format!("Resume page {index}"));
        store.insert_media(&show).unwrap();
        let episode_number = index + 1;
        let episode = insert_episode(&store, &root, show.id, 1, episode_number);
        let updated_at = 30 - index as i64;
        store
            .upsert_unit(
                user,
                show.id,
                1,
                episode_number as i32,
                7_500,
                Some(false),
                None,
                None,
                None,
                None,
                false,
                updated_at,
            )
            .unwrap();
        if index == 0 {
            store
                .upsert_unit(user, show.id, 1, -1, 0, Some(true), None, None, None, None, false, updated_at + 1)
                .unwrap();
        } else {
            expected.push(episode.to_string().replace('-', ""));
        }
    }
    drop(store);

    let resumed = get_json(
        &app,
        "/Users/00000000-0000-0000-0000-000000000001/Items/Resume?Limit=2",
    )
    .await;
    let ids = resumed["Items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["Id"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(resumed["TotalRecordCount"], 2);
    assert_eq!(ids, expected);
    assert!(resumed["Items"].as_array().unwrap().iter().all(|item| item["UserData"]["Played"] == false));
}
#[tokio::test]
async fn newer_season_unplayed_mark_wins_detail_and_listing() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("tv-mark-precedence");
    std::fs::create_dir_all(&root).unwrap();
    store.create_library(MediaKind::Tv, "Mark precedence", &[root.to_str().unwrap()], "everyone", true, &[]).unwrap();
    let show = new_media(MediaKind::Tv, "Mark precedence show");
    store.insert_media(&show).unwrap();
    let episode = insert_episode(&store, &root, show.id, 1, 1);
    let user = store.admin_user_id().unwrap();
    store.upsert_unit(user, show.id, 1, 1, 0, Some(true), None, None, None, None, false, 10).unwrap();
    store.upsert_unit(user, show.id, 1, -1, 0, Some(false), None, None, None, None, false, 20).unwrap();
    drop(store);

    let episode_id = episode.to_string().replace('-', "");
    assert_eq!(get_json(&app, &format!("/Items/{episode_id}")).await["UserData"]["Played"], false);
    let listing = get_json(&app, "/Items").await;
    let listed_show = listing["Items"].as_array().unwrap().iter().find(|item| item["Name"] == "Mark precedence show").unwrap();
    assert_eq!(listed_show["UserData"]["Played"], false);
    let next_up = get_json(&app, "/Shows/NextUp").await;
    assert!(next_up["Items"].as_array().unwrap().iter().any(|item| item["Id"] == episode_id));

    let store = Store::open(tmp.path().join("data")).unwrap();
    store.upsert_unit(user, show.id, 1, 1, 0, Some(true), None, None, None, None, false, 30).unwrap();
    drop(store);
    assert_eq!(get_json(&app, &format!("/Items/{episode_id}")).await["UserData"]["Played"], true);
}

#[tokio::test]
async fn native_season_favorite_does_not_resurrect_explicitly_unplayed_episode() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("native-favorite");
    std::fs::create_dir_all(&root).unwrap();
    store.create_library(MediaKind::Tv, "Native favorite", &[root.to_str().unwrap()], "everyone", true, &[]).unwrap();
    let show = new_media(MediaKind::Tv, "Native favorite show");
    store.insert_media(&show).unwrap();
    let episode = insert_episode(&store, &root, show.id, 1, 1);
    let user = store.admin_user_id().unwrap();
    store.upsert_unit(user, show.id, 1, -1, 0, Some(true), None, None, None, None, false, 5).unwrap();
    store.upsert_unit(user, show.id, 1, 1, 0, Some(false), None, None, None, None, false, 10).unwrap();
    drop(store);

    let response = app.clone().oneshot(Request::builder()
        .method("POST")
        .uri("/api/v1/playback/marks")
        .header("authorization", "Bearer admin-token")
        .header("content-type", "application/json")
        .body(Body::from(json!({"media_item_id": show.id.to_string(), "season_number": 1, "favorite": true}).to_string()))
        .unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(get_json(&app, &format!("/Items/{}", episode.to_string().replace('-', ""))).await["UserData"]["Played"], false);
}

#[tokio::test]
async fn favorite_only_episode_does_not_survive_history_clear_as_played() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("favorite-only-episode");
    std::fs::create_dir_all(&root).unwrap();
    store.create_library(MediaKind::Tv, "Favorite only", &[root.to_str().unwrap()], "everyone", true, &[]).unwrap();
    let show = new_media(MediaKind::Tv, "Favorite only show");
    store.insert_media(&show).unwrap();
    let episode = insert_episode(&store, &root, show.id, 1, 1);
    let user = store.admin_user_id().unwrap();
    store.upsert_unit(user, show.id, 1, -1, 0, Some(true), None, None, None, None, false, 10).unwrap();
    drop(store);

    let marked = app.clone().oneshot(Request::builder()
        .method("POST")
        .uri(format!("/UserFavoriteItems/{}", episode.to_string().replace('-', "")))
        .header("authorization", "Bearer admin-token")
        .body(Body::empty())
        .unwrap()).await.unwrap();
    assert_eq!(marked.status(), StatusCode::OK);
    let cleared = app.clone().oneshot(Request::builder()
        .method("DELETE")
        .uri("/api/v1/playback/history?scope=all")
        .header("authorization", "Bearer admin-token")
        .body(Body::empty())
        .unwrap()).await.unwrap();
    assert_eq!(cleared.status(), StatusCode::OK);
    assert_eq!(
        get_json(&app, &format!("/Items/{}", episode.to_string().replace('-', ""))).await["UserData"]["Played"],
        false
    );
}

#[tokio::test]
async fn favoriting_episode_does_not_flip_inherited_played_to_unplayed() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("favorite-played-episode");
    std::fs::create_dir_all(&root).unwrap();
    store
        .create_library(MediaKind::Tv, "Favorite played", &[root.to_str().unwrap()], "everyone", true, &[])
        .unwrap();
    let show = new_media(MediaKind::Tv, "Favorite played show");
    store.insert_media(&show).unwrap();
    let episode = insert_episode(&store, &root, show.id, 1, 1);
    let user = store.admin_user_id().unwrap();
    store
        .upsert_unit(user, show.id, 1, -1, 0, Some(true), None, None, None, None, false, 0)
        .unwrap();
    drop(store);

    let episode_id = episode.to_string().replace('-', "");
    let marked = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/UserFavoriteItems/{episode_id}"))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(marked.status(), StatusCode::OK);
    assert_eq!(
        get_json(&app, &format!("/Items/{episode_id}")).await["UserData"]["Played"],
        true,
        "favouriting must not defeat an inherited played mark"
    );
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
    json(response).await
}

fn new_media(kind: MediaKind, title: &str) -> Media {
    Media {
        id: MediaId::new(),
        kind,
        title: title.into(),
        year: None,
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

fn insert_episode(
    store: &Store,
    root: &Path,
    media_id: MediaId,
    season: u32,
    episode: u32,
) -> LedgerId {
    let path = root.join(format!("Show S{season:02}E{episode:02}.mkv"));
    std::fs::write(&path, b"episode").unwrap();
    let row = LedgerRow {
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
    };
    store.insert_ledger(&row).unwrap();
    row.id
}

#[tokio::test]
async fn tv_season_filter_resume_and_show_played_marks_work() {
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
    let show = new_media(MediaKind::Tv, "Show");
    store.insert_media(&show).unwrap();
    let first_id = insert_episode(&store, &root, show.id, 1, 1);
    let second_id = insert_episode(&store, &root, show.id, 1, 2);
    let third_id = insert_episode(&store, &root, show.id, 2, 1);
    let fourth_id = insert_episode(&store, &root, show.id, 2, 2);
    let series_id = show.id.to_string().replace('-', "");

    let seasons = get_json(&app, &format!("/Shows/{series_id}/Seasons")).await;
    let season_one_id = seasons["Items"][0]["Id"].as_str().unwrap();
    let season_two_id = seasons["Items"][1]["Id"].as_str().unwrap();
    let all_episodes = get_json(&app, &format!("/Shows/{series_id}/Episodes")).await;
    assert_eq!(all_episodes["TotalRecordCount"], 4);
    assert_eq!(
        all_episodes["Items"][0]["Id"],
        first_id.to_string().replace('-', "")
    );
    assert_eq!(
        all_episodes["Items"][1]["Id"],
        second_id.to_string().replace('-', "")
    );
    assert_eq!(
        all_episodes["Items"][2]["Id"],
        third_id.to_string().replace('-', "")
    );
    assert_eq!(
        all_episodes["Items"][3]["Id"],
        fourth_id.to_string().replace('-', "")
    );

    let season_episodes = get_json(
        &app,
        &format!("/Shows/{series_id}/Episodes?SeasonId={season_one_id}"),
    )
    .await;
    assert_eq!(season_episodes["TotalRecordCount"], 2);
    assert_eq!(season_episodes["Items"].as_array().unwrap().len(), 2);
    assert_eq!(season_episodes["Items"][0]["IndexNumber"], 1);
    assert_eq!(season_episodes["Items"][1]["IndexNumber"], 2);
    let invalid_season = get_json(
        &app,
        &format!("/Shows/{series_id}/Episodes?SeasonId=another-season"),
    )
    .await;
    assert!(invalid_season["Items"].as_array().unwrap().is_empty());

    let progress = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/Sessions/Playing/Progress")
                .header("authorization", "Bearer admin-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "ItemId": second_id.to_string().replace('-', ""),
                        "PositionTicks": 75_000_000
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(progress.status(), StatusCode::NO_CONTENT);
    let resumed = get_json(
        &app,
        "/Users/00000000-0000-0000-0000-000000000001/Items/Resume",
    )
    .await;
    let resumed_item = resumed["Items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["Id"] == second_id.to_string().replace('-', ""))
        .unwrap();
    assert_eq!(resumed_item["Type"], "Episode");
    assert_eq!(
        resumed_item["UserData"]["PlaybackPositionTicks"],
        75_000_000
    );

    let mark_episode = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/UserPlayedItems/{}",
                    first_id.to_string().replace('-', "")
                ))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(mark_episode.status(), StatusCode::OK);
    let next_up = get_json(&app, "/Shows/NextUp").await;
    assert_eq!(
        next_up["Items"][0]["Id"],
        second_id.to_string().replace('-', "")
    );

    let favorite_show = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/UserFavoriteItems/{series_id}"))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(favorite_show.status(), StatusCode::OK);
    assert_eq!(
        get_json(&app, "/Shows/NextUp").await["Items"][0]["Id"],
        second_id.to_string().replace('-', "")
    );

    let mark_season = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/UserPlayedItems/{season_one_id}"))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(mark_season.status(), StatusCode::OK);
    let next_up_after_season = get_json(&app, "/Shows/NextUp").await;
    assert_eq!(
        next_up_after_season["Items"][0]["Id"],
        third_id.to_string().replace('-', "")
    );
    let season_episode = get_json(&app, &format!("/Items/{}", first_id.to_string().replace('-', ""))).await;
    assert_eq!(season_episode["UserData"]["Played"], true);
    let series_listing = get_json(&app, "/Items").await;
    let listed_series = series_listing["Items"].as_array().unwrap().iter().find(|item| item["Name"] == "Show").unwrap();
    assert_eq!(listed_series["UserData"]["Played"], false);
    assert_eq!(listed_series["RecursiveItemCount"], 4);
    assert_eq!(listed_series["UserData"]["UnplayedItemCount"], 2);
    let direct_series = get_json(&app, &format!("/Items/{series_id}")).await;
    assert_eq!(direct_series["UserData"]["Played"], listed_series["UserData"]["Played"]);
    assert_eq!(direct_series["RecursiveItemCount"], listed_series["RecursiveItemCount"]);
    assert_eq!(direct_series["UserData"]["UnplayedItemCount"], listed_series["UserData"]["UnplayedItemCount"]);
    let resume_after_season = get_json(
        &app,
        "/Users/00000000-0000-0000-0000-000000000001/Items/Resume",
    )
    .await;
    assert!(resume_after_season["Items"].as_array().unwrap().is_empty());

    let unmark_second = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!(
                    "/UserPlayedItems/{}",
                    second_id.to_string().replace('-', "")
                ))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unmark_second.status(), StatusCode::OK);
    assert_eq!(
        get_json(&app, "/Shows/NextUp").await["Items"][0]["Id"],
        second_id.to_string().replace('-', "")
    );
    let direct_unmarked = get_json(&app, &format!("/Items/{}", second_id.to_string().replace('-', ""))).await;
    assert_eq!(direct_unmarked["UserData"]["Played"], false);
    let favorite_season = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/UserFavoriteItems/{season_one_id}"))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(favorite_season.status(), StatusCode::OK);
    let after_favorite_detail = get_json(&app, &format!("/Items/{}", second_id.to_string().replace('-', ""))).await;
    assert_eq!(after_favorite_detail["UserData"]["Played"], false);
    assert_eq!(
        get_json(&app, "/Shows/NextUp").await["Items"][0]["Id"],
        second_id.to_string().replace('-', "")
    );
    let resume_after_favorite = get_json(
        &app,
        "/Users/00000000-0000-0000-0000-000000000001/Items/Resume",
    )
    .await;
    assert!(
        resume_after_favorite["Items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["Id"] == second_id.to_string().replace('-', ""))
    );

    let mark_show = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/UserPlayedItems/{series_id}"))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(mark_show.status(), StatusCode::OK);
    let next_up_after_show = get_json(&app, "/Shows/NextUp").await;
    assert!(next_up_after_show["Items"].as_array().unwrap().is_empty());
    let resume_after_show = get_json(
        &app,
        "/Users/00000000-0000-0000-0000-000000000001/Items/Resume",
    )
    .await;
    assert!(resume_after_show["Items"].as_array().unwrap().is_empty());

    let favorite_unmarked_season = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/UserFavoriteItems/{season_two_id}"))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(favorite_unmarked_season.status(), StatusCode::OK);
    assert!(
        get_json(&app, "/Shows/NextUp").await["Items"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    let favorite_episode = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/UserFavoriteItems/{}",
                    fourth_id.to_string().replace('-', "")
                ))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(favorite_episode.status(), StatusCode::OK);
    assert!(
        get_json(&app, "/Shows/NextUp").await["Items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let favorited_unplayed_unit = Store::open(tmp.path().join("data"))
        .unwrap()
        .unit_state(
            domain::UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap(),
            show.id,
            2,
            2,
        )
        .unwrap()
        .unwrap();
    assert!(!favorited_unplayed_unit.played, "favouriting must not invent an explicit played fact");
    assert!(favorited_unplayed_unit.favorite);

    let unmark_episode = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!(
                    "/UserPlayedItems/{}",
                    third_id.to_string().replace('-', "")
                ))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unmark_episode.status(), StatusCode::OK);
    let next_up_after_unmark = get_json(&app, "/Shows/NextUp").await;
    assert_eq!(
        next_up_after_unmark["Items"][0]["Id"],
        third_id.to_string().replace('-', "")
    );
    let unplayed_override = Store::open(tmp.path().join("data"))
        .unwrap()
        .unit_state(
            domain::UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap(),
            show.id,
            2,
            1,
        )
        .unwrap()
        .unwrap();
    assert!(!unplayed_override.played);
}

#[tokio::test]
async fn movie_marks_by_item_id_use_the_whole_media_unit() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("films");
    std::fs::create_dir_all(&root).unwrap();
    store
        .create_library(
            MediaKind::Movie,
            "Films",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let movie = new_media(MediaKind::Movie, "Film");
    store.insert_media(&movie).unwrap();
    let path = root.join("Film.mkv");
    std::fs::write(&path, b"film").unwrap();
    let row = LedgerRow {
        id: LedgerId::new(),
        media_id: movie.id,
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
    store.insert_ledger(&row).unwrap();
    let item_id = row.id.to_string().replace('-', "");
    for (route, expected) in [
        (format!("/UserPlayedItems/{item_id}"), "Played"),
        (format!("/UserFavoriteItems/{item_id}"), "Favorite"),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(route)
                    .header("authorization", "Bearer admin-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{expected}");
    }
    let state = Store::open(tmp.path().join("data"))
        .unwrap()
        .unit_state(
            domain::UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap(),
            movie.id,
            -1,
            -1,
        )
        .unwrap()
        .unwrap();
    assert!(state.played);
    assert!(state.favorite);
}

#[tokio::test]
async fn resume_lists_only_the_latest_in_progress_episode_per_series() {
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
    let show = new_media(MediaKind::Tv, "Show");
    store.insert_media(&show).unwrap();
    let user_id = domain::UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap();
    for episode in 1..=4 {
        insert_episode(&store, &root, show.id, 1, episode);
        store
            .upsert_unit(
                user_id,
                show.id,
                1,
                episode as i32,
                10_000 * episode as i64,
                Some(false),
                None,
                None,
                None,
                None,
                false,
                episode as i64 + 100,
            )
            .unwrap();
    }

    let resumed = get_json(
        &app,
        "/Users/00000000-0000-0000-0000-000000000001/Items/Resume",
    )
    .await;
    assert_eq!(resumed["TotalRecordCount"], 1);
    assert_eq!(resumed["Items"].as_array().unwrap().len(), 1);
    assert_eq!(resumed["Items"][0]["IndexNumber"], 4);
}


#[tokio::test]
async fn tv_items_include_nfo_artwork_metadata_and_cached_playback_streams() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("tv");
    let show_dir = root.join("Show");
    let season_dir = show_dir.join("Season 1");
    std::fs::create_dir_all(&season_dir).unwrap();
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
    let show = new_media(MediaKind::Tv, "Show");
    store.insert_media(&show).unwrap();
    std::fs::write(
        show_dir.join("tvshow.nfo"),
        "<tvshow><title>Show</title><year>2024</year><plot>Series synopsis</plot><rating>8.7</rating><genre>Drama</genre><actor><name>Series Actor</name><role>Lead</role><tmdbid>101</tmdbid><thumb>/actor.jpg</thumb></actor></tvshow>",
    )
    .unwrap();
    std::fs::write(show_dir.join("poster.jpg"), b"poster").unwrap();
    std::fs::write(show_dir.join("fanart.jpg"), b"fanart").unwrap();
    let episode_path = season_dir.join("Show S01E01.mkv");
    std::fs::write(&episode_path, b"episode").unwrap();
    std::fs::write(
        season_dir.join("Show S01E01.nfo"),
        "<episodedetails><title>Pilot</title><season>1</season><episode>1</episode><plot>Episode synopsis</plot><thumb>https://image.tmdb.org/t/p/w300/still.jpg</thumb></episodedetails>",
    )
    .unwrap();
    let row = LedgerRow {
        id: LedgerId::new(),
        media_id: show.id,
        path: episode_path.display().to_string(),
        season: Some(1),
        episode: Some(1),
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: None,
    };
    store.insert_ledger(&row).unwrap();
    store
        .put_file_meta(
            &row.id.to_string(),
            &library::Tracks {
                video: Some(library::VideoTrack {
                    codec: Some("h264".into()),
                    width: Some(1920),
                    height: Some(1080),
                    frame_rate: Some(24.0),
                    bit_rate: Some(4_000_000),
                    duration_secs: Some(600.0),
                    ..Default::default()
                }),
                audio: vec![library::AudioTrack {
                    codec: Some("aac".into()),
                    channels: Some(2),
                    language: Some("ja".into()),
                    sample_rate: Some("48000".into()),
                    is_default: true,
                    ..Default::default()
                }],
                subtitles: vec![library::SubtitleTrack {
                    codec: Some("subrip".into()),
                    language: Some("zh".into()),
                    is_default: true,
                    forced: false,
                    ..Default::default()
                }],
            },
        )
        .unwrap();
    store
        .put_cached_chapters(
            &row.id.to_string(),
            &[library::ChapterMarker {
                start_ms: 30_000,
                end_ms: 60_000,
                title: Some("Previously cached chapter".into()),
                marker_type: None,
                synthetic: false,
            }],
        )
        .unwrap();
    store
        .put_media_marker(&api::store::StoredMediaMarker {
            media_id: show.id,
            season: 1,
            episode: 1,
            intro_start_ms: Some(111_000),
            intro_end_ms: Some(222_000),
            outro_start_ms: Some(540_000),
            outro_end_ms: Some(600_000),
            source: "test".into(),
            locked: false,
            updated_at: 1,
        })
        .unwrap();

    let series_id = show.id.to_string().replace('-', "");
    let series_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/Items/{series_id}"))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let series = json(series_response).await;
    assert_eq!(series["Overview"], "Series synopsis");
    assert_eq!(series["CommunityRating"], 8.7);
    assert_eq!(series["Genres"][0], "Drama");
    assert_eq!(series["ProductionYear"], 2024);
    assert_eq!(series["Path"], show_dir.display().to_string());
    assert!(series["MediaStreams"].is_null());
    assert_eq!(series["ChildCount"], 1);
    assert!(
        series["ImageTags"]["Primary"]
            .as_str()
            .is_some_and(|tag| tag.starts_with("poster-"))
    );
    assert_eq!(series["BackdropImageTags"][0], "fanart");

    for route in [
        format!("/UserFavoriteItems/{series_id}"),
        format!("/UserPlayedItems/{series_id}"),
    ] {
        let marked = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(route)
                    .header("authorization", "Bearer admin-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(marked.status(), StatusCode::OK);
    }
    let marked_series = get_json(&app, &format!("/Items/{series_id}")).await;
    assert_eq!(marked_series["UserData"]["IsFavorite"], true);
    assert_eq!(marked_series["UserData"]["Played"], true);

    let episode_id = row.id.to_string().replace('-', "");
    let episode_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/Items/{episode_id}"))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let episode = json(episode_response).await;
    assert_eq!(episode["Name"], "Pilot");
    assert_eq!(episode["Overview"], "Episode synopsis");
    assert_eq!(episode["IndexNumber"], 1);
    assert_eq!(episode["ParentId"], series_id);
    assert_eq!(episode["SeriesName"], "Show");
    assert_eq!(episode["People"][0]["Name"], "Series Actor");
    assert_eq!(episode["People"][0]["Role"], "Lead");

    let playback_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/Items/{episode_id}/PlaybackInfo"))
                .header("authorization", "Bearer admin-token")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    let playback = json(playback_response).await;
    assert_eq!(playback["Chapters"].as_array().unwrap().len(), 3);
    assert_eq!(playback["Chapters"][0]["Name"], "Previously cached chapter");
    assert_eq!(playback["Chapters"][1]["MarkerType"], "IntroStart");
    assert_eq!(playback["Chapters"][2]["MarkerType"], "CreditsStart");
    let playback = playback["MediaSources"][0].clone();
    assert_eq!(playback["RunTimeTicks"], 6_000_000_000i64);
    assert_eq!(playback["MediaStreams"][0]["Type"], "Video");
    assert_eq!(playback["MediaStreams"][0]["Codec"], "h264");
    assert_eq!(playback["MediaStreams"][1]["Type"], "Audio");
    assert_eq!(playback["MediaStreams"][1]["Channels"], 2);
    assert_eq!(playback["MediaStreams"][2]["Type"], "Subtitle");

    let backdrop = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/Items/{series_id}/Images/Backdrop/0"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(backdrop.status(), StatusCode::OK);
    assert_eq!(
        &to_bytes(backdrop.into_body(), 32).await.unwrap()[..],
        b"fanart"
    );

    let episode_image = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/Items/{episode_id}/Images/Primary"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(episode_image.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        episode_image.headers()["location"],
        "https://image.tmdb.org/t/p/w300/still.jpg"
    );
}

#[path = "marks.rs"]
mod marks;
