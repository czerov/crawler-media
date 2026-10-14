use domain::{MediaId, UserId};
use store::{
    BrowseFileFacts, BrowseMediaFacts, ProjectionBuild, ProjectionSnapshot, ReadRevisions, Store,
};

#[test]
fn read_revisions_increments_on_dml_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();

    let rev_initial = store.read_revisions().unwrap();

    // 1. DML on library.db: insert ledger
    let media_id = MediaId::new();
    let row = domain::LedgerRow {
        id: domain::LedgerId::new(),
        media_id,
        path: "/movies/m1.mkv".into(),
        season: None,
        episode: None,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: domain::QualitySource::Release,
        confidence: domain::Confidence::High,
        filter_score: None,
    };
    store.insert_ledger(&row).unwrap();
    let rev_after_lib = store.read_revisions().unwrap();
    assert!(
        rev_after_lib.library > rev_initial.library,
        "library revision must increment on ledger DML"
    );

    // 2. DML on app.db: insert media
    let media = domain::Media {
        id: media_id,
        kind: domain::MediaKind::Movie,
        title: "Test".into(),
        year: Some(2024),
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let rev_after_app = store.read_revisions().unwrap();
    assert!(
        rev_after_app.app > rev_after_lib.app,
        "app revision must increment on media DML"
    );

    // 3. DML on catalog.db
    store
        .put_catalog_cache("movie", "tmdb:123", "{}", 1000, None)
        .unwrap();
    let rev_after_cat = store.read_revisions().unwrap();
    assert!(
        rev_after_cat.catalog > rev_after_app.catalog,
        "catalog revision must increment on catalog DML"
    );

    // 4. DML on subscribe.db
    let user = UserId::new();
    store
        .set_playback_state(user, media_id, 1000, Some(true), None, None, None)
        .unwrap();
    let rev_after_sub = store.read_revisions().unwrap();
    assert!(
        rev_after_sub.subscribe > rev_after_cat.subscribe,
        "subscribe revision must increment on playback_units DML"
    );
}

#[test]
fn projection_publication_and_retrieval() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();

    let lib_id = "lib_test";
    let media_id = MediaId::new();
    let ledger_id = domain::LedgerId::new();

    let revisions = store.read_revisions().unwrap();

    let build = ProjectionBuild {
        library_id: lib_id.to_string(),
        generation: 1,
        scope_signature: "sig-v1".to_string(),
        revisions,
        files: vec![BrowseFileFacts {
            ledger_id,
            media_id,
            facts: serde_json::json!({ "size": 1024, "version": 1 }),
        }],
        media: vec![BrowseMediaFacts {
            media_id,
            title: "Test Movie".to_string(),
            year: Some(2024),
            facts: serde_json::json!({ "poster": true }),
        }],
    };

    let published = store.publish_browse_projection(&build).unwrap();
    assert!(published, "publication must succeed when library revision matches");

    let snapshot = store
        .browse_projection(lib_id)
        .unwrap()
        .expect("snapshot must exist");
    assert_eq!(snapshot.generation, 1);
    assert_eq!(snapshot.scope_signature, "sig-v1");
    assert_eq!(snapshot.media.len(), 1);
    assert_eq!(snapshot.files.len(), 1);

    // Publishing projection itself must NOT increment revisions!
    let rev_after_pub = store.read_revisions().unwrap();
    assert_eq!(rev_after_pub.library, revisions.library);
}

#[test]
fn catalog_details_batch_respects_kind_id_boundaries() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();

    store
        .put_catalog_cache("movie", "tmdb:123", "{\"id\":123}", 1000, None)
        .unwrap();
    store
        .put_catalog_cache("tv", "tmdb:123", "{\"id\":123,\"kind\":\"tv\"}", 1000, None)
        .unwrap();
    store
        .put_catalog_cache("movie", "tmdb:1234", "{\"id\":1234}", 1000, None)
        .unwrap();

    let batch = store
        .catalog_details_batch(&[("movie".into(), "tmdb:123".into())])
        .unwrap();
    assert_eq!(batch.len(), 1);
    assert_eq!(batch[0].source, "movie");
    assert_eq!(batch[0].cache_key, "tmdb:123");
}

#[test]
fn catalog_details_for_prefixes_matches_kind_id_boundary_and_query_variants() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();

    store
        .put_catalog_cache("tmdb", "/movie/123", "{\"v\":1}", 100, None)
        .unwrap();
    store
        .put_catalog_cache("tmdb", "/movie/123?language=zh-CN", "{\"v\":2}", 200, None)
        .unwrap();
    store
        .put_catalog_cache("tmdb", "/movie/1234", "{\"v\":3}", 300, None)
        .unwrap();
    store
        .put_catalog_cache("tmdb", "/movie/123/credits", "{\"v\":4}", 400, None)
        .unwrap();
    store
        .put_catalog_cache("tmdb", "/tv/123", "{\"v\":5}", 500, None)
        .unwrap();
    store
        .put_catalog_cache("other", "/movie/123", "{\"v\":6}", 600, None)
        .unwrap();

    let rows = store
        .catalog_details_for_prefixes("tmdb", &["/movie/123".to_string()])
        .unwrap();
    let keys: Vec<&str> = rows.iter().map(|row| row.cache_key.as_str()).collect();
    assert_eq!(
        keys,
        vec!["/movie/123?language=zh-CN", "/movie/123"],
        "只匹配该 kind/id 前缀（含 ? 变体），按 fetched_at DESC 稳定排序"
    );
    assert!(rows.iter().all(|row| row.source == "tmdb"));
}

/// P2-NEW-2：`fetched_at` 并列时，全量读取与定向读取必须选中**同一行**。
///
/// 旧定向查询只有 `ORDER BY fetched_at DESC`，索引扫描顺序与全量读取不同，
/// 会为同一个 base 选出不同 language，投影与回退路径的 metadata 随之漂移。
#[test]
fn tied_fetched_at_picks_the_same_row_on_full_and_targeted_reads() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();

    // 同一详情的两个 language，`fetched_at` 相同；插入顺序决定 rowid。
    store
        .put_catalog_cache("tmdb", "/movie/42?language=zh-CN", "{\"v\":\"zh\"}", 100, None)
        .unwrap();
    store
        .put_catalog_cache("tmdb", "/movie/42?language=en-US", "{\"v\":\"en\"}", 100, None)
        .unwrap();

    let first_for_base = |rows: &[store::CatalogCacheRow]| {
        rows.iter()
            .filter(|row| {
                row.source == "tmdb" && row.cache_key.split('?').next() == Some("/movie/42")
            })
            .map(|row| row.cache_key.clone())
            .next()
    };
    let full = store.list_catalog_cache().unwrap();
    let targeted = store
        .catalog_details_for_prefixes("tmdb", &["/movie/42".to_string()])
        .unwrap();
    assert_eq!(
        first_for_base(&full),
        first_for_base(&targeted),
        "fetched_at 并列时全量与定向读取必须选中同一行"
    );
    assert_eq!(
        first_for_base(&full).as_deref(),
        Some("/movie/42?language=zh-CN"),
        "稳定并列键保持「先写入者优先」"
    );
}

fn insert_ledger_row(store: &Store, path: &std::path::Path) {
    store
        .insert_ledger(&domain::LedgerRow {
            id: domain::LedgerId::new(),
            media_id: MediaId::new(),
            path: path.to_string_lossy().to_string(),
            season: None,
            episode: None,
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: domain::QualitySource::Release,
            confidence: domain::Confidence::High,
            filter_score: None,
        })
        .unwrap();
}

/// P3-NEW-5：目标 Library 的候选读不读取无关根的 ledger 行，但仍保留
/// 「未命中任何根 → 该 kind 默认 Library」的旧归属回退。
///
/// 这里只断言**返回行**。默认回退分支的 DB 成本（`SCAN ledger`，VM steps 随
/// 无关台账增长）由 `browse_reads::tests::default_library_candidate_read_is_not_bounded`
/// 用 VM steps 度量并记录为 P3-D 限制。
#[test]
fn ledger_candidates_for_library_skip_unrelated_roots_and_keep_default_fallback() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    // `Store::open` 已为每个 kind 种下默认库；新建的库都是非默认。
    let default_lib = store
        .default_library(domain::MediaKind::Movie)
        .unwrap()
        .expect("seeded default movie library");
    let target_root = tmp.path().join("movies");
    let other_root = tmp.path().join("other");
    std::fs::create_dir_all(&target_root).unwrap();
    std::fs::create_dir_all(&other_root).unwrap();

    let target = store
        .create_library(
            domain::MediaKind::Movie,
            "Target",
            &[target_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let other = store
        .create_library(
            domain::MediaKind::Movie,
            "Other",
            &[other_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();

    for index in 0..3 {
        insert_ledger_row(&store, &target_root.join(format!("m{index}.mkv")));
    }
    for index in 0..200 {
        insert_ledger_row(&store, &other_root.join(format!("o{index}.mkv")));
    }
    // 不属于任何根：旧归属回退到该 kind 的默认库。
    insert_ledger_row(&store, &tmp.path().join("stray").join("x.mkv"));

    let ownership = store.library_ownership().unwrap();
    let candidates = store
        .ledger_candidates_for_library(&ownership, &target.id)
        .unwrap();
    assert_eq!(
        candidates.len(),
        3,
        "无关 Library 的 200 行与默认回退行都不得进入非默认库的候选读"
    );
    assert!(
        candidates
            .iter()
            .all(|row| row.path.starts_with(target_root.to_str().unwrap())),
        "候选读只返回目标根下的行"
    );

    let other_candidates = store
        .ledger_candidates_for_library(&ownership, &other.id)
        .unwrap();
    assert_eq!(other_candidates.len(), 200, "非默认库只读自己根下的行");

    let default_candidates = store
        .ledger_candidates_for_library(&ownership, &default_lib.id)
        .unwrap();
    assert_eq!(
        default_candidates.len(),
        1,
        "未命中任何根的行必须回退到默认库"
    );
    assert!(
        default_candidates
            .iter()
            .any(|row| row.path.ends_with("stray/x.mkv")),
        "默认回退行必须包含在候选里"
    );
}

fn candidate_paths(store: &Store, library_id: &str) -> Vec<String> {
    let ownership = store.library_ownership().unwrap();
    let mut paths: Vec<String> = store
        .ledger_candidates_for_library(&ownership, library_id)
        .unwrap()
        .into_iter()
        .map(|row| row.path)
        .collect();
    paths.sort();
    paths
}

/// P2-B：候选读必须与旧 `owner()` 的组件级 `Path::starts_with` 等价。根写成
/// `/movies/./`、`/movies//`、`/movies/` 时，干净路径（扫描外部工具写入的
/// ledger）仍必须进入候选；相邻前缀 `/movies` vs `/movies2` 不得误配。
#[test]
fn ledger_candidates_match_component_ownership_for_non_canonical_roots() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().to_str().unwrap().to_string();
    struct Case {
        root: String,
        path: String,
        owned_by_target: bool,
    }
    let cases = vec![
        Case {
            root: format!("{base}/movies/./"),
            path: format!("{base}/movies/Visible.mkv"),
            owned_by_target: true,
        },
        Case {
            root: format!("{base}//movies//"),
            path: format!("{base}/movies/Visible.mkv"),
            owned_by_target: true,
        },
        Case {
            root: format!("{base}/movies/"),
            path: format!("{base}/movies/Visible.mkv"),
            owned_by_target: true,
        },
        // 扫描自己写下的路径同样带非规范拼写。
        Case {
            root: format!("{base}/movies/./"),
            path: format!("{base}/movies/./Visible.mkv"),
            owned_by_target: true,
        },
        Case {
            root: format!("{base}//movies//"),
            path: format!("{base}//movies//Visible.mkv"),
            owned_by_target: true,
        },
        // 相邻前缀：`/movies2` 不属于 `/movies`，必须回退到默认库而不是目标库。
        Case {
            root: format!("{base}/movies"),
            path: format!("{base}/movies2/Visible.mkv"),
            owned_by_target: false,
        },
    ];

    for (index, case) in cases.iter().enumerate() {
        let store = Store::open(tmp.path().join(format!("case{index}"))).unwrap();
        let library = store
            .create_library(
                domain::MediaKind::Movie,
                "Target",
                &[case.root.as_str()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        insert_ledger_row(&store, std::path::Path::new(&case.path));

        let ownership = store.library_ownership().unwrap();
        let owner_id = ownership
            .owner(std::path::Path::new(&case.path), domain::MediaKind::Movie)
            .map(|owner| owner.id.clone());
        assert_eq!(
            owner_id.as_deref() == Some(library.id.as_str()),
            case.owned_by_target,
            "case {index}: 旧 owner() 口径与用例前提不一致（root={}, path={}）",
            case.root,
            case.path
        );

        let candidates = candidate_paths(&store, &library.id);
        assert_eq!(
            candidates.contains(&case.path),
            case.owned_by_target,
            "case {index}: 候选读必须与 owner() 等价（root={}, path={}）",
            case.root,
            case.path
        );
    }
}

/// 根为 `/` 时所有绝对路径都属于该库；`/` 前缀不能被文本比较漏掉。
#[test]
fn ledger_candidates_treat_root_slash_as_every_absolute_path() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let library = store
        .create_library(
            domain::MediaKind::Movie,
            "Root",
            &["/"],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let path = format!("{}/Visible.mkv", tmp.path().to_str().unwrap());
    insert_ledger_row(&store, std::path::Path::new(&path));

    let ownership = store.library_ownership().unwrap();
    let owner = ownership
        .owner(std::path::Path::new(&path), domain::MediaKind::Movie)
        .expect("root / owns every absolute path");
    assert_eq!(owner.id, library.id);
    assert!(
        candidate_paths(&store, &library.id).contains(&path),
        "根 `/` 的候选读不得漏掉绝对路径"
    );
}

/// 嵌套根：子库的行必须进入子库候选，父库候选仍可包含（由 owner() 在内存里
/// 判给子库），两者都不得丢失。
#[test]
fn ledger_candidates_keep_nested_root_rows_for_parent_and_child() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let parent_root = tmp.path().join("tv");
    let child_root = parent_root.join("anime");
    std::fs::create_dir_all(&child_root).unwrap();
    let parent = store
        .create_library(
            domain::MediaKind::Tv,
            "Parent",
            &[parent_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let child = store
        .create_library(
            domain::MediaKind::Tv,
            "Child",
            &[child_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let path = child_root.join("Show/Episode.mkv");
    insert_ledger_row(&store, &path);
    let path = path.to_string_lossy().to_string();

    let ownership = store.library_ownership().unwrap();
    let owner = ownership
        .owner(std::path::Path::new(&path), domain::MediaKind::Tv)
        .expect("nested root has an owner");
    assert_eq!(owner.id, child.id, "最深根胜出");

    assert!(
        candidate_paths(&store, &child.id).contains(&path),
        "子库候选必须包含自己的行"
    );
    assert!(
        candidate_paths(&store, &parent.id).contains(&path),
        "父库候选保留子库行，交由 owner() 在内存里过滤"
    );
}
