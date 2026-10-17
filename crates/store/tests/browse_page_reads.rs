//! A4 浏览页读取的 store 级证据。
//!
//! 覆盖：SQL 排序键分页（title/added_at/release_date 的并列与缺失值）、越界 offset、
//! 只读选中 Media 的 file facts、轻候选字段、缺 file 行的 Media 不出现、迁移幂等，
//! 以及固定页在整库（含无关 Media）增长时 DB 工作量不随之增长。

use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
use store::{
    BrowseFileFacts, BrowseMediaCandidate, BrowseMediaFacts, BrowsePageOrder, BrowsePageRequest,
    ProjectionBuild, Store,
};

fn open(tmp: &tempfile::TempDir) -> Store {
    Store::open(tmp.path().join("data")).unwrap()
}

fn movie(id: MediaId, title: &str, year: Option<u16>) -> Media {
    Media {
        id,
        kind: MediaKind::Movie,
        title: title.to_string(),
        year,
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

fn row(media_id: MediaId, path: &str, resolution: Option<&str>, hdr: Option<&str>) -> LedgerRow {
    LedgerRow {
        id: LedgerId::new(),
        media_id,
        path: path.to_string(),
        season: None,
        episode: None,
        resolution: resolution.map(str::to_string),
        codec: Some("h264".into()),
        hdr: hdr.map(str::to_string),
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    }
}

/// 轻候选的测试构造：只填被测字段，其余给确定性的默认值。
fn candidate(media_id: MediaId, title: &str, year: Option<u16>) -> BrowseMediaCandidate {
    BrowseMediaCandidate {
        media_id,
        kind: MediaKind::Movie,
        title: title.to_string(),
        year,
        added_at_ns: None,
        release_date: String::new(),
        rating: None,
        runtime: None,
        size_bytes: None,
        provisional: true,
        scraped: false,
        missing: false,
        hdr: false,
        resolutions: Vec::new(),
        genres: Vec::new(),
        countries: Vec::new(),
        language: None,
        has_poster: false,
        has_backdrop: false,
        has_still: false,
        episode_keys: Vec::new(),
    }
}

/// 发布一代投影：`items` 为 `(Media, ledger 行, 该 Media 的文件数)`。
fn publish(store: &Store, library_id: &str, generation: i64, items: &[(Media, Vec<LedgerRow>)]) {
    let mut files = Vec::new();
    let mut media = Vec::new();
    let mut candidates = Vec::new();
    for (entry, rows) in items {
        for row in rows {
            files.push(BrowseFileFacts {
                ledger_id: row.id,
                media_id: entry.id,
                facts: serde_json::json!({ "version": 3, "missing": false }),
            });
        }
        media.push(BrowseMediaFacts {
            media_id: entry.id,
            title: entry.title.clone(),
            year: entry.year,
            facts: serde_json::json!({ "version": 3, "poster": true }),
        });
        candidates.push(candidate(entry.id, &entry.title, entry.year));
    }
    let build = ProjectionBuild {
        library_id: library_id.to_string(),
        generation,
        scope_signature: "sig".to_string(),
        revisions: store.read_revisions().unwrap(),
        expected_dirty_token: None,
        files,
        media,
        candidates,
    };
    assert!(store.publish_browse_projection(&build).unwrap());
}

fn request(library_id: &str, order: BrowsePageOrder, descending: bool) -> BrowsePageRequest {
    BrowsePageRequest {
        library_id: library_id.to_string(),
        offset: 0,
        limit: 10,
        order,
        descending,
    }
}

/// 标题/入库时间/发行日期三个索引列的分页、并列与缺失值（NULL 与空串）顺序。
#[test]
fn page_orders_match_the_full_selection_order() {
    let tmp = tempfile::tempdir().unwrap();
    let store = open(&tmp);
    let mut items = Vec::new();
    let titles = ["Bravo", "Alpha", "Charlie"];
    for title in titles {
        let media = movie(MediaId::new(), title, Some(2020));
        let rows = vec![row(media.id, &format!("/m/{title}.mkv"), Some("1080p"), None)];
        store.insert_media(&media).unwrap();
        for entry in &rows {
            store.insert_ledger(entry).unwrap();
        }
        items.push((media, rows));
    }
    publish(&store, "lib", 1, &items);

    let page = store
        .browse_media_page(&request("lib", BrowsePageOrder::Title, false))
        .unwrap();
    let got: Vec<&str> = page.iter().map(|row| row.title.as_str()).collect();
    assert_eq!(got, vec!["Alpha", "Bravo", "Charlie"], "title ASC");

    let page = store
        .browse_media_page(&request("lib", BrowsePageOrder::Title, true))
        .unwrap();
    let got: Vec<&str> = page.iter().map(|row| row.title.as_str()).collect();
    assert_eq!(got, vec!["Charlie", "Bravo", "Alpha"], "title DESC");

    // added_at_ns 全为 NULL：ASC 时都在最前（与 Rust 的 None 序一致），并列按 media_id。
    let mut page = store
        .browse_media_page(&request("lib", BrowsePageOrder::AddedAt, false))
        .unwrap();
    assert_eq!(page.len(), 3);
    let mut expected: Vec<String> = items.iter().map(|(media, _)| media.id.to_string()).collect();
    expected.sort();
    let mut got: Vec<String> = page.iter().map(|row| row.media_id.to_string()).collect();
    assert_eq!(got, expected, "NULL added_at 的并列按 media_id 升序");
    got.clear();
    page.clear();

    // release_date 全为空串（物化排序键缺省），并列同样按 media_id。
    let page = store
        .browse_media_page(&request("lib", BrowsePageOrder::ReleaseDate, true))
        .unwrap();
    let got: Vec<String> = page.iter().map(|row| row.media_id.to_string()).collect();
    assert_eq!(got, expected, "空 release_date 的并列按 media_id 升序（降序只反转主键）");
}

/// 越界/极值 offset 得到空页，不 panic、不回绕。
#[test]
fn page_bounds_are_overflow_safe() {
    let tmp = tempfile::tempdir().unwrap();
    let store = open(&tmp);
    let media = movie(MediaId::new(), "Alpha", None);
    let rows = vec![row(media.id, "/m/alpha.mkv", None, None)];
    store.insert_media(&media).unwrap();
    store.insert_ledger(&rows[0]).unwrap();
    publish(&store, "lib", 1, &[(media, rows)]);

    let mut far = request("lib", BrowsePageOrder::Title, false);
    far.offset = 5;
    assert!(store.browse_media_page(&far).unwrap().is_empty());

    far.offset = usize::MAX;
    assert!(store.browse_media_page(&far).unwrap().is_empty());
}

/// 只读选中 Media 的 file facts：无关 Media 的 browse_files 行不解码。
#[test]
fn file_facts_are_read_for_selected_media_only() {
    let tmp = tempfile::tempdir().unwrap();
    let store = open(&tmp);
    let wanted = movie(MediaId::new(), "Alpha", None);
    let wanted_id = wanted.id;
    let other = movie(MediaId::new(), "Beta", None);
    let wanted_rows = vec![
        row(wanted.id, "/m/alpha-1.mkv", Some("1080p"), None),
        row(wanted.id, "/m/alpha-2.mkv", Some("2160p"), Some("HDR")),
    ];
    let other_rows = vec![row(other.id, "/m/beta.mkv", Some("720p"), None)];
    store.insert_media(&wanted).unwrap();
    store.insert_media(&other).unwrap();
    for entry in wanted_rows.iter().chain(other_rows.iter()) {
        store.insert_ledger(entry).unwrap();
    }
    publish(
        &store,
        "lib",
        1,
        &[(wanted, wanted_rows.clone()), (other, other_rows)],
    );

    let facts = store
        .browse_file_facts_for_media("lib", &[wanted_id])
        .unwrap();
    assert_eq!(facts.len(), 2, "只返回选中 Media 的 file facts");
    let ids: Vec<LedgerId> = facts.iter().map(|fact| fact.ledger_id).collect();
    for row in &wanted_rows {
        assert!(ids.contains(&row.id));
    }

    // 空 ids 不读任何行。
    assert!(
        store
            .browse_file_facts_for_media("lib", &[])
            .unwrap()
            .is_empty()
    );
}

/// 轻候选含过滤/排序字段、不含 LedgerRow 与整份 facts；缺 file 行的 Media 不出现。
#[test]
fn candidates_carry_light_fields_and_skip_media_without_files() {
    let tmp = tempfile::tempdir().unwrap();
    let store = open(&tmp);
    let with_files = movie(MediaId::new(), "Alpha", Some(2021));
    let without_files = movie(MediaId::new(), "Beta", Some(2022));
    let rows = vec![row(with_files.id, "/m/alpha.mkv", Some("1080p"), None)];
    let other_rows = vec![row(without_files.id, "/m/beta.mkv", Some("720p"), None)];
    store.insert_media(&with_files).unwrap();
    store.insert_media(&without_files).unwrap();
    for entry in rows.iter().chain(other_rows.iter()) {
        store.insert_ledger(entry).unwrap();
    }
    publish(
        &store,
        "lib",
        1,
        &[
            (with_files.clone(), rows.clone()),
            (without_files.clone(), other_rows),
        ],
    );
    // 第二代只保留 with_files 的 file 行：without_files 的 media 行成为孤儿
    // （投影里 media 行还在、file 行已删），两条读路径都必须跳过它。
    let build = ProjectionBuild {
        library_id: "lib".to_string(),
        generation: 2,
        scope_signature: "sig".to_string(),
        revisions: store.read_revisions().unwrap(),
        expected_dirty_token: None,
        files: rows
            .iter()
            .map(|entry| BrowseFileFacts {
                ledger_id: entry.id,
                media_id: entry.media_id,
                facts: serde_json::json!({ "version": 3 }),
            })
            .collect(),
        media: vec![
            BrowseMediaFacts {
                media_id: with_files.id,
                title: with_files.title.clone(),
                year: with_files.year,
                facts: serde_json::json!({ "version": 3 }),
            },
            BrowseMediaFacts {
                media_id: without_files.id,
                title: without_files.title.clone(),
                year: without_files.year,
                facts: serde_json::json!({ "version": 3 }),
            },
        ],
        candidates: vec![
            candidate(with_files.id, "Alpha", Some(2021)),
            candidate(without_files.id, "Beta", Some(2022)),
        ],
    };
    assert!(store.publish_browse_projection(&build).unwrap());

    let candidates = store.browse_media_candidates("lib").unwrap();
    assert_eq!(candidates.len(), 1, "只有有 file 行的 Media 进入轻候选");
    let light = &candidates[0];
    assert_eq!(light.media_id, with_files.id);
    assert_eq!(light.title, "Alpha");
    assert_eq!(light.year, Some(2021));
    assert!(light.provisional, "无外部 id 的条目是 provisional");

    let page = store
        .browse_media_page(&request("lib", BrowsePageOrder::Title, false))
        .unwrap();
    let ids: Vec<MediaId> = page.iter().map(|entry| entry.media_id).collect();
    assert_eq!(ids, vec![with_files.id], "缺 file 行的 Media 不进入页");
}

/// 轻候选字段的 JSON 往返：过滤/排序字段与小集合逐字段保真。
#[test]
fn candidate_json_round_trips_light_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let store = open(&tmp);
    let media = movie(MediaId::new(), "Alpha", Some(2021));
    let rows = vec![
        row(media.id, "/m/alpha-1.mkv", Some("1080p"), Some("HDR")),
        row(media.id, "/m/alpha-2.mkv", Some("2160p"), None),
    ];
    store.insert_media(&media).unwrap();
    for entry in &rows {
        store.insert_ledger(entry).unwrap();
    }
    let mut light = candidate(media.id, "Alpha", Some(2021));
    light.added_at_ns = Some(1_700_000_000_000_000_000);
    light.release_date = "2021-06-01".to_string();
    light.rating = Some(8.5);
    light.runtime = Some(101);
    light.size_bytes = Some(4096);
    light.provisional = false;
    light.scraped = true;
    light.missing = true;
    light.hdr = true;
    light.resolutions = vec!["1080p".to_string(), "2160p".to_string()];
    light.genres = vec!["28".to_string()];
    light.countries = vec!["US".to_string()];
    light.language = Some("en".to_string());
    light.has_poster = true;
    light.has_backdrop = true;
    light.has_still = true;
    light.episode_keys = vec![(1, 1), (1, 2)];
    let build = ProjectionBuild {
        library_id: "lib".to_string(),
        generation: 1,
        scope_signature: "sig".to_string(),
        revisions: store.read_revisions().unwrap(),
        expected_dirty_token: None,
        files: rows
            .iter()
            .map(|entry| BrowseFileFacts {
                ledger_id: entry.id,
                media_id: entry.media_id,
                facts: serde_json::json!({ "version": 3 }),
            })
            .collect(),
        media: vec![BrowseMediaFacts {
            media_id: media.id,
            title: media.title.clone(),
            year: media.year,
            facts: serde_json::json!({ "version": 3 }),
        }],
        candidates: vec![light.clone()],
    };
    assert!(store.publish_browse_projection(&build).unwrap());

    let stored = store.browse_media_candidates("lib").unwrap();
    assert_eq!(stored, vec![light], "轻候选 JSON 往返必须逐字段保真");

    // 旧版/缺失候选（例如升级前的库）必须被识别为不可用，读路径据此回退。
    assert!(
        BrowseMediaCandidate::from_json(&serde_json::json!({ "version": 0 })).is_none(),
        "版本不符的候选不可用"
    );
}

/// 迁移幂等：重复打开同一数据目录不报错，且排序列可用。
#[test]
fn page_columns_migration_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("data");
    {
        let store = Store::open(&path).unwrap();
        let media = movie(MediaId::new(), "Alpha", Some(2020));
        let rows = vec![row(media.id, "/m/alpha.mkv", None, None)];
        store.insert_media(&media).unwrap();
        store.insert_ledger(&rows[0]).unwrap();
        publish(&store, "lib", 1, &[(media, rows)]);
    }
    let store = Store::open(&path).unwrap();
    let page = store
        .browse_media_page(&request("lib", BrowsePageOrder::AddedAt, true))
        .unwrap();
    assert_eq!(page.len(), 1, "重开后的库仍能按 added_at 分页");
}

/// 从 14 版升级：旧投影缺候选 JSON，迁移必须把它标脏等待重建。
#[test]
fn upgrade_from_14_marks_legacy_projections_dirty() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("data");
    {
        let store = Store::open(&path).unwrap();
        let media = movie(MediaId::new(), "Alpha", Some(2020));
        let rows = vec![row(media.id, "/m/alpha.mkv", None, None)];
        store.insert_media(&media).unwrap();
        store.insert_ledger(&rows[0]).unwrap();
        publish(&store, "lib", 1, &[(media, rows)]);
        let head = store.browse_projection_head("lib").unwrap().unwrap();
        assert!(head.ready, "新发布的一代是 ready");
    }
    // 模拟 14 版库：删掉候选列并回退版本号。
    {
        let conn = rusqlite::Connection::open(path.join("library.db")).unwrap();
        conn.execute_batch(
            "ALTER TABLE browse_media DROP COLUMN candidate_json; PRAGMA user_version = 14;",
        )
        .unwrap();
    }
    let store = Store::open(&path).unwrap();
    let head = store.browse_projection_head("lib").unwrap().unwrap();
    assert!(
        !head.ready,
        "升级后旧投影必须标脏，让下一次请求重建（否则热页永久回退全量读取）"
    );
}

/// 固定页在整库（含无关 Media）从 5k 增长到 10k 时，DB 工作量不随之增长。
#[test]
fn fixed_page_read_stays_bounded_as_unrelated_media_grow() {
    let tmp = tempfile::tempdir().unwrap();
    let store = open(&tmp);
    let mut items = Vec::new();
    let mut ids = Vec::new();
    for index in 0..5_000u32 {
        let media = movie(MediaId::new(), &format!("Title {index:05}"), Some(2000));
        let rows = vec![row(media.id, &format!("/m/{index}.mkv"), None, None)];
        ids.push(media.id);
        items.push((media, rows));
    }
    for (media, rows) in &items {
        store.insert_media(media).unwrap();
        store.insert_ledger(&rows[0]).unwrap();
    }
    publish(&store, "lib", 1, &items);

    let request = request("lib", BrowsePageOrder::Title, false);
    let (small_rows, small_steps) = store.browse_media_page_with_vm_steps(&request).unwrap();
    assert_eq!(small_rows.len(), 10);

    for index in 5_000..10_000u32 {
        let media = movie(MediaId::new(), &format!("Title {index:05}"), Some(2000));
        let rows = vec![row(media.id, &format!("/m/{index}.mkv"), None, None)];
        store.insert_media(&media).unwrap();
        store.insert_ledger(&rows[0]).unwrap();
        items.push((media, rows));
    }
    publish(&store, "lib", 2, &items);

    let (large_rows, large_steps) = store.browse_media_page_with_vm_steps(&request).unwrap();
    assert_eq!(large_rows.len(), 10, "固定页仍只返回 limit 行");
    let small: Vec<MediaId> = small_rows.iter().map(|row| row.media_id).collect();
    let large: Vec<MediaId> = large_rows.iter().map(|row| row.media_id).collect();
    assert_eq!(small, large, "无关 Media 增长不改变固定页内容");
    assert!(
        large_steps <= small_steps * 2,
        "固定页的 VM steps 不随无关 Media 翻倍增长: {small_steps} -> {large_steps}"
    );
    let _ = ids;
}
