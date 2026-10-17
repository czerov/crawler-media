//! SCOPE-6-01..04 的回归：库/folder/SeriesId 作用域与读取失败。

use super::*;

#[tokio::test]
async fn library_scoped_series_id_lists_episodes_without_include_types() {
    let fixture = tv_fixture(1, 2);
    // 第二个库：同名结构，用来验证库归属仍然生效。
    let other_root = fixture.dir.path().join("tv-other");
    std::fs::create_dir_all(&other_root).unwrap();
    let (other_library, other_show, other_episode) = {
        let store = fixture.state.store();
        let store = store.lock();
        let library = store
            .create_library(
                MediaKind::Tv,
                "Other shows",
                &[other_root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let media_id = new_media(&store, "Other show");
        let episode = insert_episode(&store, &other_root, media_id, 1, 1);
        (library.id, media_id, episode)
    };
    let app = router(fixture.state.clone());
    let show = media_id_for_title(&fixture.state, "Show000");

    let by_series = get_json(&app, &format!("/Items?SeriesId={show}")).await;
    assert_eq!(by_series["TotalRecordCount"], 2, "{by_series}");
    let scoped = get_json(
        &app,
        &format!(
            "/Items?ParentId={}&SeriesId={show}&Recursive=true",
            fixture.library_id
        ),
    )
    .await;
    assert_eq!(
        scoped["TotalRecordCount"], by_series["TotalRecordCount"],
        "库作用域 + SeriesId 必须与单独 SeriesId 等价：{scoped}"
    );
    assert_eq!(items(&scoped).len(), 2);
    assert!(items(&scoped).iter().all(|item| item["Type"] == "Episode"));

    // 库归属：另一个库的剧不得从本库作用域里泄露。
    let foreign = get_json(
        &app,
        &format!(
            "/Items?ParentId={}&SeriesId={}&Recursive=true",
            fixture.library_id,
            other_show.to_string().replace('-', "")
        ),
    )
    .await;
    assert_eq!(foreign["TotalRecordCount"], 0, "{foreign}");
    let own_scope = get_json(
        &app,
        &format!(
            "/Items?ParentId={}&SeriesId={}&Recursive=true",
            other_library,
            other_show.to_string().replace('-', "")
        ),
    )
    .await;
    assert_eq!(items(&own_scope).len(), 1);
    assert_eq!(
        items(&own_scope)[0]["Id"],
        other_episode.to_string().replace('-', "")
    );
}

/// SCOPE-6-02：`Series,Episode` + folder 作用域时，代表行在别的根下的剧也必须保留
/// （folder 归属先于折叠确定）。
#[tokio::test]
async fn folder_series_and_episode_types_keep_the_series_of_that_folder() {
    let dir = tempfile::tempdir().unwrap();
    let files = Arc::new(CountingFiles::default());
    let state = new_state(dir.path(), files);
    let root_a = dir.path().join("tv-a");
    let root_b = dir.path().join("tv-b");
    std::fs::create_dir_all(&root_a).unwrap();
    std::fs::create_dir_all(&root_b).unwrap();
    let (library_id, episode_a, episode_b) = {
        let store = state.store();
        let store = store.lock();
        let library = store
            .create_library(
                MediaKind::Tv,
                "Split shows",
                &[root_a.to_str().unwrap(), root_b.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let media_id = new_media(&store, "Split show");
        let episode_a = insert_episode(&store, &root_a, media_id, 1, 1);
        let episode_b = insert_episode(&store, &root_b, media_id, 1, 2);
        (library.id, episode_a, episode_b)
    };
    let app = router(state);
    let listing = get_json(
        &app,
        &format!(
            "/Items?ParentId={}&Recursive=true&IncludeItemTypes=Series,Episode",
            folder_id(&root_b)
        ),
    )
    .await;
    assert_eq!(listing["TotalRecordCount"], 2, "{listing}");
    assert_eq!(items(&listing).len(), 2);
    assert_eq!(
        items(&listing)
            .iter()
            .filter(|item| item["Type"] == "Series")
            .count(),
        1,
        "folder 里有集的剧必须保留：{listing}"
    );
    let episodes: Vec<&Value> = items(&listing)
        .iter()
        .filter(|item| item["Type"] == "Episode")
        .collect();
    assert_eq!(episodes.len(), 1);
    assert_eq!(episodes[0]["Id"], episode_b.to_string().replace('-', ""));

    let listing_a = get_json(
        &app,
        &format!(
            "/Items?ParentId={}&Recursive=true&IncludeItemTypes=Series,Episode",
            folder_id(&root_a)
        ),
    )
    .await;
    assert_eq!(listing_a["TotalRecordCount"], 2, "{listing_a}");
    let episodes_a: Vec<&Value> = items(&listing_a)
        .iter()
        .filter(|item| item["Type"] == "Episode")
        .collect();
    assert_eq!(episodes_a[0]["Id"], episode_a.to_string().replace('-', ""));

    // 同一份候选上的缺省（无 IncludeItemTypes）列表仍然只有剧条目。
    let series_only = get_json(
        &app,
        &format!("/Items?ParentId={}&Recursive=true", library_id),
    )
    .await;
    assert_eq!(series_only["TotalRecordCount"], 1);
    assert_eq!(items(&series_only)[0]["Type"], "Series");
}

/// SCOPE-6-03：媒体交集下推到 SQL——整库分集列表只做一次候选遍历，剧作用域只读
/// 这部剧的行（不为每个 series 重读整库）。
#[tokio::test]
async fn series_scoped_selection_reads_only_that_series_candidates() {
    let fixture = tv_fixture(100, 10);
    let app = router(fixture.state.clone());
    let listing = get_json(
        &app,
        &format!(
            "/Items?ParentId={}&Recursive=true&IncludeItemTypes=Episode&Limit=1&SortBy=SortName",
            fixture.library_id
        ),
    )
    .await;
    assert_eq!(listing["TotalRecordCount"], 1000, "整库分集数");
    assert_eq!(items(&listing).len(), 1);
    assert_eq!(
        fixture.files.subtitles(),
        1,
        "一次候选遍历 + 只 enrich 本页：{}",
        fixture.files.subtitles()
    );
    assert!(
        fixture.files.metadata() <= 2,
        "整库候选不得逐个 stat，实际 {}",
        fixture.files.metadata()
    );

    let show = media_id_for_title(&fixture.state, "Show000");
    fixture.files.reset();
    fixture.state.store().lock().reset_root_ledger_scans();
    let scoped = get_json(
        &app,
        &format!(
            "/Items?ParentId={}&SeriesId={show}&Recursive=true&Limit=1",
            fixture.library_id
        ),
    )
    .await;
    let scans = fixture.state.store().lock().root_ledger_scans();
    assert_eq!(scoped["TotalRecordCount"], 10, "只列这部剧的分集：{scoped}");
    assert_eq!(scans, 0, "剧作用域不得扫无 WHERE 的根账本，实际 {scans}");
    assert_eq!(fixture.files.subtitles(), 1);
}

/// SCOPE-6-04：读取失败必须变成错误或完整的一页，绝不静默缩成一页空结果。
#[tokio::test]
async fn read_failure_never_becomes_a_silently_empty_page() {
    let fixture = tv_fixture(1, 2);
    let app = router(fixture.state.clone());
    let db = fixture.dir.path().join("data/library.db");
    fixture.files.fail_store_on_first_io(Box::new(move || {
        let conn = rusqlite::Connection::open(&db).expect("open library db");
        conn.execute_batch("DROP TABLE ledger").expect("drop ledger");
    }));
    let (status, listing) = get(
        &app,
        &format!(
            "/Items?ParentId={}&Recursive=true&IncludeItemTypes=Episode&Limit=1&SortBy=SortName",
            fixture.library_id
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{listing}");
    assert_eq!(
        listing["TotalRecordCount"], 2,
        "候选读先于文件 IO：请求进行中的存储故障不得吞掉这一页：{listing}"
    );
    assert_eq!(items(&listing).len(), 1);

    let heavy_fixture = tv_fixture(1, 2);
    let heavy_app = router(heavy_fixture.state.clone());
    let failing = {
        let store = heavy_fixture.state.store();
        let store = store.lock();
        store.list_ledger().unwrap().into_iter().next().expect("row").id
    };
    let conn = rusqlite::Connection::open(
        heavy_fixture.dir.path().join("data/library.db"),
    )
    .unwrap();
    conn.execute(
        "INSERT INTO file_meta (ledger_id, audio_json, subtitle_json, chapters_json)
         VALUES (?1, '[]', '[]', x'0102')",
        [failing.to_string()],
    )
    .unwrap();
    drop(conn);
    let (broken_status, broken) = get(
        &heavy_app,
        &format!(
            "/Items?ParentId={}&Recursive=true&IncludeItemTypes=Episode&Limit=2",
            heavy_fixture.library_id
        ),
    )
    .await;
    assert_eq!(
        broken_status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "重元数据读取失败必须报错：{broken}"
    );
}

/// 既有组合等价性：genre/studio 过滤在选页之前完成，StartIndex 与 ties 稳定。

#[tokio::test]
async fn jellyfin_selection_records_candidate_and_heavy_counts() {
    let fixture = tv_fixture(100, 10);
    let candidates = fixture.state.jellyfin_candidates();
    let heavy = fixture.state.jellyfin_heavy_items();
    let app = router(fixture.state.clone());
    let listing = get_json(
        &app,
        &format!(
            "/Items?ParentId={}&Recursive=true&IncludeItemTypes=Episode&Limit=1&SortBy=SortName",
            fixture.library_id
        ),
    )
    .await;
    assert_eq!(listing["TotalRecordCount"], 1000);
    assert_eq!(
        candidates.load(Ordering::SeqCst),
        1000,
        "整库分集列表：一遍候选遍历（100 部剧 × 10 集）"
    );
    assert_eq!(heavy.load(Ordering::SeqCst), 1, "只有本页条目被 enrich");

    let show = media_id_for_title(&fixture.state, "Show000");
    let before = candidates.load(Ordering::SeqCst);
    fixture.state.store().lock().reset_root_ledger_scans();
    let scoped = get_json(
        &app,
        &format!(
            "/Items?ParentId={}&SeriesId={show}&Recursive=true&Limit=1",
            fixture.library_id
        ),
    )
    .await;
    let scans = fixture.state.store().lock().root_ledger_scans();
    assert_eq!(scoped["TotalRecordCount"], 10);
    assert_eq!(scans, 0, "cross-thread series selection must not root-scan ledger: {scans}");
    assert_eq!(candidates.load(Ordering::SeqCst) - before, 10);
    assert_eq!(heavy.load(Ordering::SeqCst), 2, "第二次请求仍然只 enrich 本页");
}

/// A3-06-01：播放单元解码失败发生在台账读完之后，已读行仍要计入候选。
#[tokio::test]
async fn failed_playback_unit_read_still_counts_loaded_ledger_rows() {
    let fixture = tv_fixture(1, 2);
    let show = media_id_for_title(&fixture.state, "Show000");
    let user_id = {
        let store = fixture.state.store();
        let store = store.lock();
        store
            .list_users()
            .unwrap()
            .into_iter()
            .find(|user| store.user_role(user.id).unwrap() == "admin")
            .expect("admin")
            .id
    };
    let stored_media = uuid::Uuid::parse_str(&show).unwrap().to_string();
    let conn = rusqlite::Connection::open(fixture.dir.path().join("data/subscribe.db")).unwrap();
    conn.execute(
        "INSERT INTO playback_units
            (user_id, media_id, season, episode, position_ms, played, favorite, updated_at)
         VALUES (?1, ?2, 1, 1, 'bad', 0, 0, 1)",
        rusqlite::params![user_id.to_string(), stored_media],
    )
    .unwrap();
    drop(conn);

    let candidates = fixture.state.jellyfin_candidates();
    let app = router(fixture.state.clone());
    let (status, body) = get(&app, &format!("/Items?ParentId={show}&Limit=1")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(
        candidates.load(Ordering::SeqCst),
        2,
        "两行台账已读完，播放单元失败不得把候选计成 0"
    );
}

/// A3-06-02：不可见剧的分类读取仍计入，且同一媒体不得再读一遍只报一次。
#[tokio::test]
async fn invisible_series_classification_keeps_its_ledger_read() {
    let fixture = tv_fixture(1, 2);
    let hidden_root = fixture.dir.path().join("tv-hidden");
    std::fs::create_dir_all(&hidden_root).unwrap();
    let show = {
        let store = fixture.state.store();
        let store = store.lock();
        store
            .create_library(
                domain::MediaKind::Tv,
                "Hidden",
                &[hidden_root.to_str().unwrap()],
                "everyone",
                false,
                &[],
            )
            .unwrap();
        let media_id = new_media(&store, "HiddenShow");
        let show_dir = hidden_root.join("HiddenShow");
        std::fs::create_dir_all(&show_dir).unwrap();
        for episode in 1..=2 {
            insert_episode(&store, &show_dir, media_id, 1, episode);
        }
        media_id.to_string().replace('-', "")
    };

    let candidates = fixture.state.jellyfin_candidates();
    fixture.state.store().lock().reset_media_ledger_reads();
    let app = router(fixture.state.clone());
    let (status, body) = get(
        &app,
        &format!("/Items?ParentId={show}&SeriesId={show}&Limit=1"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["TotalRecordCount"], 0, "admin_visible=false 的剧不得列出：{body}");
    let reads = fixture.state.store().lock().media_ledger_reads();
    assert_eq!(reads, 1, "分类已经读过这部剧，显式目标不得再读一遍，实际 {reads}");
    assert_eq!(
        candidates.load(Ordering::SeqCst),
        2,
        "分类读了 2 行并复用，不得丢掉这次读取：实际 {}",
        candidates.load(Ordering::SeqCst)
    );
}

