//! 分页/过滤/展开的等价性与工作量：重元数据只为选中的页构建。

use super::*;

#[tokio::test]
async fn jellyfin_limit_one_builds_one_heavy_snapshot() {
    let fixture = tv_fixture(200, 10);
    let app = router(fixture.state.clone());
    let listing = get_json(
        &app,
        &format!(
            "/Items?ParentId={}&Recursive=true&Limit=1&SortBy=SortName",
            fixture.library_id
        ),
    )
    .await;
    assert_eq!(items(&listing).len(), 1);
    assert_eq!(listing["TotalRecordCount"], 200);
    assert_eq!(items(&listing)[0]["Name"], "Show199", "缺省 SortName 倒序");
    assert_eq!(
        fixture.files.subtitles(),
        1,
        "limit=1 只应构建一条重元数据（2000 条候选全部 enrich 是旧行为）"
    );
    // stat 只覆盖本页条目自己的行（1 部剧 × 10 集 = 10），不是全部 2000 条候选。
    assert!(
        fixture.files.metadata() <= 10,
        "stat 必须限于本页条目自己的行，实际 {}",
        fixture.files.metadata()
    );

    fixture.files.reset();
    let page = get_json(
        &app,
        &format!(
            "/Items?ParentId={}&Recursive=true&Limit=20&SortBy=SortName",
            fixture.library_id
        ),
    )
    .await;
    assert_eq!(items(&page).len(), 20);
    assert_eq!(page["TotalRecordCount"], 200);
    assert_eq!(fixture.files.subtitles(), 20, "重元数据条数 = 本页条数");
}

/// 页外的重事实损坏（非过滤字段）不得影响当前页。
#[tokio::test]
async fn corrupted_heavy_facts_outside_the_page_do_not_fail_the_page() {
    let fixture = tv_fixture(4, 2);
    let app = router(fixture.state.clone());
    let out_of_page = {
        let store = fixture.state.store();
        let store = store.lock();
        let row = store
            .list_ledger()
            .unwrap()
            .into_iter()
            .find(|row| row.path.contains("Show000"))
            .expect("Show000 row");
        store
            .put_file_meta(&row.id.to_string(), &library::Tracks::default())
            .unwrap();
        let conn = rusqlite::Connection::open(fixture.dir.path().join("data/library.db")).unwrap();
        conn.execute(
            "UPDATE file_meta SET chapters_json = '{ not json', audio_json = 'nope' WHERE ledger_id = ?1",
            [row.id.to_string()],
        )
        .unwrap();
        row.id
    };
    assert!(!out_of_page.to_string().is_empty());
    let listing = get_json(
        &app,
        &format!(
            "/Items?ParentId={}&Recursive=true&Limit=1&SortBy=SortName",
            fixture.library_id
        ),
    )
    .await;
    assert_eq!(listing["TotalRecordCount"], 4);
    assert_eq!(items(&listing)[0]["Name"], "Show003");
}

/// SCOPE-6-01：库作用域 + `SeriesId` 就是「列这部剧的分集」，与 `IncludeItemTypes`
/// 无关，并且仍然受库归属约束。

#[tokio::test]
async fn filter_before_paging_keeps_names_order_and_count() {
    let fixture = tv_fixture(6, 2);
    let app = router(fixture.state.clone());
    {
        let store = fixture.state.store();
        let store = store.lock();
        for row in store.list_ledger().unwrap() {
            let media = store.get_media(row.media_id).unwrap().expect("media");
            let genre = if media.title.ends_with('0') || media.title.ends_with('2') {
                "Drama"
            } else {
                "Comedy"
            };
            let dir = Path::new(&row.path).parent().unwrap();
            std::fs::write(
                dir.join("tvshow.nfo"),
                format!("<tvshow><genre>{genre}</genre><studio>Studio X</studio></tvshow>"),
            )
            .unwrap();
        }
    }
    let drama = get_json(
        &app,
        &format!(
            "/Items?ParentId={}&Recursive=true&IncludeItemTypes=Series&Genres=Drama&Limit=1&SortBy=SortName",
            fixture.library_id
        ),
    )
    .await;
    assert_eq!(drama["TotalRecordCount"], 2, "{drama}");
    assert_eq!(items(&drama).len(), 1);

    let studio = get_json(
        &app,
        &format!(
            "/Items?ParentId={}&Recursive=true&IncludeItemTypes=Series&StudioIds=Studio%20X&SortBy=SortName",
            fixture.library_id
        ),
    )
    .await;
    assert_eq!(studio["TotalRecordCount"], 6, "{studio}");

    // StartIndex 越界与 ties：两次请求结果一致（稳定 tie-break）。
    let first = get_json(
        &app,
        &format!(
            "/Items?ParentId={}&Recursive=true&IncludeItemTypes=Series&SortBy=SortName&StartIndex=5",
            fixture.library_id
        ),
    )
    .await;
    let again = get_json(
        &app,
        &format!(
            "/Items?ParentId={}&Recursive=true&IncludeItemTypes=Series&SortBy=SortName&StartIndex=5",
            fixture.library_id
        ),
    )
    .await;
    assert_eq!(first, again, "同一 fixture 的次序必须稳定");
    assert_eq!(items(&first).len(), 1);
    assert_eq!(first["TotalRecordCount"], 6);
}

/// 分集展开 + folder 归属：folder 作用域只列该根下的分集。
#[tokio::test]
async fn episode_expansion_stays_inside_the_folder() {
    let dir = tempfile::tempdir().unwrap();
    let files = Arc::new(CountingFiles::default());
    let state = new_state(dir.path(), files);
    let root_a = dir.path().join("folder-a");
    let root_b = dir.path().join("folder-b");
    std::fs::create_dir_all(&root_a).unwrap();
    std::fs::create_dir_all(&root_b).unwrap();
    let (episode_a, episode_b, show_media) = {
        let store = state.store();
        let store = store.lock();
        store
            .create_library(
                MediaKind::Tv,
                "Folders",
                &[root_a.to_str().unwrap(), root_b.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let media_id = new_media(&store, "Folder show");
        (
            insert_episode(&store, &root_a, media_id, 1, 1),
            insert_episode(&store, &root_b, media_id, 1, 2),
            media_id,
        )
    };
    let app = router(state);
    let listing = get_json(
        &app,
        &format!(
            "/Items?ParentId={}&Recursive=true&IncludeItemTypes=Episode",
            folder_id(&root_b)
        ),
    )
    .await;
    assert_eq!(listing["TotalRecordCount"], 1, "{listing}");
    assert_eq!(items(&listing)[0]["Id"], episode_b.to_string().replace('-', ""));

    let whole = get_json(&app, "/Items?Recursive=true&IncludeItemTypes=Episode").await;
    assert_eq!(whole["TotalRecordCount"], 2);
    assert!(items(&whole).iter().any(|item| item["Id"] == episode_a.to_string().replace('-', "")));

    // 剧 parent（不给 IncludeItemTypes）就是「列这部剧的分集」：一行一条，不折叠成剧。
    let by_show = get_json(
        &app,
        &format!(
            "/Items?ParentId={}&Recursive=true",
            show_media.to_string().replace('-', "")
        ),
    )
    .await;
    assert_eq!(by_show["TotalRecordCount"], 2, "{by_show}");
    assert!(items(&by_show).iter().all(|item| item["Type"] == "Episode"));
}

/// 认不出的 parent 一律空集合（绝不退化成整库），未知 Media 同样如此。
#[tokio::test]
async fn unknown_parent_returns_an_empty_page() {
    let fixture = tv_fixture(2, 1);
    let app = router(fixture.state.clone());
    let unknown = get_json(&app, "/Items?ParentId=does-not-exist&Recursive=true").await;
    assert_eq!(unknown["TotalRecordCount"], 0, "{unknown}");
    let movie_shaped = get_json(&app, &format!("/Items?ParentId={}", MediaId::new())).await;
    assert_eq!(movie_shaped["TotalRecordCount"], 0, "{movie_shaped}");
    let unknown_folder = get_json(
        &app,
        &format!(
            "/Items?ParentId={}",
            folder_id(Path::new("/nonexistent-root"))
        ),
    )
    .await;
    assert_eq!(unknown_folder["TotalRecordCount"], 0, "{unknown_folder}");
}

/// 最新列表：缺省只排不筛（已看完沉底），显式 `IsPlayed` 才严格筛。
#[tokio::test]
async fn latest_items_keeps_played_last_by_default_and_filters_when_asked() {
    let fixture = tv_fixture(3, 1);
    let app = router(fixture.state.clone());
    let user = {
        let store = fixture.state.store();
        let store = store.lock();
        store.admin_user_id().unwrap()
    };
    let first_show = {
        let store = fixture.state.store();
        let store = store.lock();
        let row = store
            .list_ledger()
            .unwrap()
            .into_iter()
            .find(|row| row.path.contains("Show000"))
            .expect("Show000 row");
        store.upsert_unit(user, row.media_id, -1, -1, 0, Some(true), None, None, None, None, false, 10).unwrap();
        row.media_id
    };
    let latest = get_json(&app, "/Users/me/Items/Latest").await;
    let listed = latest.as_array().expect("latest array");
    assert_eq!(listed.len(), 3);
    assert_eq!(
        listed.last().unwrap()["Id"],
        first_show.to_string().replace('-', ""),
        "缺省：已看完沉底"
    );
    let played = get_json(&app, "/Users/me/Items/Latest?IsPlayed=true").await;
    assert_eq!(played.as_array().map(Vec::len), Some(1), "{played}");
}

/// NextUp：同一 Media 跨库的未看分集聚合成一条。

#[tokio::test]
async fn person_filter_selects_before_paging() {
    let fixture = tv_fixture(4, 1);
    let (person_id, matched) = {
        let store = fixture.state.store();
        let store = store.lock();
        let row = store
            .list_ledger()
            .unwrap()
            .into_iter()
            .find(|row| row.path.contains("Show000"))
            .expect("Show000 row");
        let dir = Path::new(&row.path).parent().unwrap();
        std::fs::write(
            dir.join("tvshow.nfo"),
            "<tvshow><actor><name>Alice</name><role>Lead</role></actor></tvshow>",
        )
        .unwrap();
        let key = format!("name:{}:{}", "alice", "Actor");
        (
            uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, key.as_bytes()).to_string(),
            "Show000",
        )
    };
    let app = router(fixture.state.clone());
    // SortName 倒序下 Show000 排在最后：先选页再筛 person 会选中 Show003。
    let listing = get_json(
        &app,
        &format!(
            "/Items?ParentId={}&Recursive=true&IncludeItemTypes=Series&PersonIds={person_id}&Limit=1&SortBy=SortName",
            fixture.library_id
        ),
    )
    .await;
    assert_eq!(listing["TotalRecordCount"], 1, "{listing}");
    assert_eq!(items(&listing)[0]["Name"], matched, "{listing}");
}

/// 同名 ties 必须稳定：分页不得重复或漏条目。
#[tokio::test]
async fn name_ties_keep_a_stable_order() {
    let dir = tempfile::tempdir().unwrap();
    let files = Arc::new(CountingFiles::default());
    let state = new_state(dir.path(), files);
    let root = dir.path().join("ties");
    std::fs::create_dir_all(&root).unwrap();
    let library_id = {
        let store = state.store();
        let store = store.lock();
        let library = store
            .create_library(
                MediaKind::Tv,
                "Ties",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        for index in 0..3 {
            let media_id = new_media(&store, "Same title");
            let show_dir = root.join(format!("same-{index}"));
            std::fs::create_dir_all(&show_dir).unwrap();
            insert_episode(&store, &show_dir, media_id, 1, 1);
        }
        library.id
    };
    let app = router(state);
    let first = get_json(
        &app,
        &format!(
            "/Items?ParentId={library_id}&Recursive=true&IncludeItemTypes=Series&SortBy=SortName&Limit=2"
        ),
    )
    .await;
    let again = get_json(
        &app,
        &format!(
            "/Items?ParentId={library_id}&Recursive=true&IncludeItemTypes=Series&SortBy=SortName&Limit=2"
        ),
    )
    .await;
    assert_eq!(first, again, "同名 ties 必须稳定");
    assert_eq!(first["TotalRecordCount"], 3);
    let rest = get_json(
        &app,
        &format!(
            "/Items?ParentId={library_id}&Recursive=true&IncludeItemTypes=Series&SortBy=SortName&StartIndex=2&Limit=2"
        ),
    )
    .await;
    let mut ids: Vec<String> = items(&first)
        .iter()
        .chain(items(&rest).iter())
        .map(|item| item["Id"].as_str().unwrap().to_string())
        .collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 3, "分页不得重复或漏条目");
}
