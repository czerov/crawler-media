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
        expected_dirty_token: None,
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

/// 旧全量路径的最终行集：`list_ledger` + `owner()` 过滤（投影修复前的语义）。
fn owner_rows_for(
    store: &Store,
    ownership: &store::LibraryOwnership,
    library_id: &str,
) -> Vec<String> {
    let mut rows: Vec<String> = store
        .list_ledger()
        .unwrap()
        .into_iter()
        .filter(|row| {
            ownership
                .owner(std::path::Path::new(&row.path), domain::MediaKind::Movie)
                .is_some_and(|owner| owner.id == library_id)
        })
        .map(|row| row.path)
        .collect();
    rows.sort();
    rows
}

/// 候选读再经内存 `owner()` 过滤后的最终行集（投影构建器实际看到的行）。
fn candidate_rows_for(
    store: &Store,
    ownership: &store::LibraryOwnership,
    library_id: &str,
) -> Vec<String> {
    let mut rows: Vec<String> = store
        .ledger_candidates_for_library(ownership, library_id)
        .unwrap()
        .into_iter()
        .filter(|row| {
            ownership
                .owner(std::path::Path::new(&row.path), domain::MediaKind::Movie)
                .is_some_and(|owner| owner.id == library_id)
        })
        .map(|row| row.path)
        .collect();
    rows.sort();
    rows
}

/// P2-residual：规范/非规范根 × 规范/非规范存库路径交叉组合下，候选读的最终行集
/// 必须等于旧全量路径（`list_ledger` + `owner()`）的结果，目标库与默认库回退都算。
///
/// 旧修法只规范化了**配置根**一侧；存库路径把根本身写成 `/a/./b/x`、`/a//b/x`
/// 时文本前缀永不匹配，行到不了内存里的 `owner()` 过滤，条目静默消失。
#[test]
fn candidate_final_rows_match_owner_for_non_canonical_root_and_path_spellings() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().to_str().unwrap().to_string();
    let leading_double = |path: &str| format!("//{}", path.trim_start_matches('/'));
    let leading_dot = |path: &str| format!("/./{}", path.trim_start_matches('/'));
    let canonical = format!("{base}/movies/Ok.mkv");
    let cases: Vec<(String, String, bool)> = vec![
        (format!("{base}/movies"), canonical.clone(), true),
        (
            format!("{base}/movies"),
            format!("{base}/./movies/Ok.mkv"),
            true,
        ),
        (
            format!("{base}/movies"),
            format!("{base}//movies/Ok.mkv"),
            true,
        ),
        (
            format!("{base}/movies"),
            format!("{base}/./movies//Ok.mkv"),
            true,
        ),
        (format!("{base}/movies"), leading_double(&canonical), true),
        (format!("{base}/movies"), leading_dot(&canonical), true),
        (format!("{base}/./movies"), canonical.clone(), true),
        (
            format!("{base}/./movies"),
            format!("{base}//movies/Ok.mkv"),
            true,
        ),
        (format!("{base}//movies"), canonical.clone(), true),
        (
            format!("{base}//movies"),
            format!("{base}/./movies/Ok.mkv"),
            true,
        ),
        // `owner()` 按组件比较：`..` 仍属于该根。
        (
            format!("{base}/movies"),
            format!("{base}/movies/../elsewhere/Ok.mkv"),
            true,
        ),
        // 相邻前缀与无关根不属于目标库：必须留给默认库回退。
        (
            format!("{base}/movies"),
            format!("{base}/movies2/No.mkv"),
            false,
        ),
        (
            format!("{base}/movies"),
            format!("{base}/other/No.mkv"),
            false,
        ),
    ];

    for (index, (root, path, owned)) in cases.iter().enumerate() {
        let store = Store::open(tmp.path().join(format!("case{index}"))).unwrap();
        let target = store
            .create_library(
                domain::MediaKind::Movie,
                "Target",
                &[root.as_str()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let default = store
            .default_library(domain::MediaKind::Movie)
            .unwrap()
            .expect("seeded default movie library");
        // 每个 case 都放一条规范拼写的行，最终行集比较才有非空基准。
        insert_ledger_row(&store, std::path::Path::new(&canonical));
        insert_ledger_row(&store, std::path::Path::new(path));

        let ownership = store.library_ownership().unwrap();
        let expected = if *owned { &target } else { &default };
        assert_eq!(
            ownership
                .owner(std::path::Path::new(path), domain::MediaKind::Movie)
                .map(|owner| owner.id.as_str()),
            Some(expected.id.as_str()),
            "case {index}: 旧 owner() 口径与用例前提不一致（root={root}, path={path}）"
        );
        for library in [&target, &default] {
            assert_eq!(
                candidate_rows_for(&store, &ownership, &library.id),
                owner_rows_for(&store, &ownership, &library.id),
                "case {index}: 库 {} 的最终行集必须等于 owner() 全量路径（root={root}, path={path}）",
                library.id
            );
        }
    }
}

/// 默认库自身根的非规范拼写：别的同 kind **祖先根**的窄谓词会先命中该行，把它从
/// 默认库的 `NOT (...)` 回退里挡掉，所以默认库的正向谓词同样必须用宽谓词。
#[test]
fn default_library_keeps_non_canonical_rows_blocked_by_an_ancestor_root() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let default = store
        .default_library(domain::MediaKind::Movie)
        .unwrap()
        .expect("seeded default movie library");
    let default_root = default
        .root_paths
        .first()
        .expect("default movie root")
        .display()
        .to_string();
    // 同 kind 祖先根：它的窄谓词（`path >= '<ancestor>/'`）会先命中非规范拼写的行。
    let ancestor = default
        .root_paths
        .first()
        .and_then(|root| root.parent())
        .expect("default root parent")
        .display()
        .to_string();
    store
        .create_library(
            domain::MediaKind::Movie,
            "Ancestor",
            &[ancestor.as_str()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    // 偏差落在**祖先根之内、默认根之外**：行仍以祖先根文本开头（窄谓词先命中），
    // 但不再是默认根文本的规范拼写。
    let leaf = default_root
        .strip_prefix(&format!("{ancestor}/"))
        .expect("default root under ancestor");
    let spelled = format!("{ancestor}/./{leaf}");
    insert_ledger_row(&store, std::path::Path::new(&spelled));

    let ownership = store.library_ownership().unwrap();
    assert_eq!(
        ownership
            .owner(std::path::Path::new(&spelled), domain::MediaKind::Movie)
            .map(|owner| owner.id.as_str()),
        Some(default.id.as_str()),
        "最深根仍是默认库（premise）"
    );
    assert_eq!(
        candidate_rows_for(&store, &ownership, &default.id),
        owner_rows_for(&store, &ownership, &default.id),
        "默认库最终行集必须等于 owner() 全量路径：祖先根的窄谓词不得挡掉非规范拼写的行"
    );
}

/// 根为 `/` 时所有绝对路径都属于该库；空路径不属于任何根，必须仍回退默认库。
///
/// `Path::new("").starts_with(Path::new("/"))` 为 false，所以 `owner()` 把空路径
/// 判给默认库；`/` 前缀的谓词若带上 `path = ''` 相等分支，就会把空路径从默认库的
/// `NOT (...)` 回退里排掉，条目静默消失。
#[test]
fn empty_ledger_path_falls_back_to_default_library_with_slash_root_library() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let slash = store
        .create_library(
            domain::MediaKind::Movie,
            "Root",
            &["/"],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let default = store
        .default_library(domain::MediaKind::Movie)
        .unwrap()
        .expect("seeded default movie library");
    insert_ledger_row(&store, std::path::Path::new(""));

    let ownership = store.library_ownership().unwrap();
    assert_eq!(
        ownership
            .owner(std::path::Path::new(""), domain::MediaKind::Movie)
            .map(|owner| owner.id.as_str()),
        Some(default.id.as_str()),
        "空路径不属于 `/` 根，必须回退到默认库"
    );
    assert!(
        candidate_paths(&store, &default.id)
            .iter()
            .any(String::is_empty),
        "空路径行必须在默认库候选里"
    );
    assert!(
        !candidate_paths(&store, &slash.id)
            .iter()
            .any(String::is_empty),
        "`/` 根不得把空路径算进候选"
    );
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
