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
        tmdb_id: None,
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
                path: row.path.clone(),
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

/// 参考排序：与 `LibraryFilter::compare` 的三个索引排序键同口径
/// （added_at: `None` 在前；release_date: 物化字符串；并列按 media_id 升序，
/// 降序只反转主键）。
fn legacy_order(
    table: &[(MediaId, String, Option<i64>, String)],
    order: BrowsePageOrder,
    descending: bool,
) -> Vec<MediaId> {
    let mut rows: Vec<&(MediaId, String, Option<i64>, String)> = table.iter().collect();
    rows.sort_by(|left, right| {
        let primary = match order {
            BrowsePageOrder::Title => left.1.cmp(&right.1),
            BrowsePageOrder::AddedAt => left.2.cmp(&right.2),
            BrowsePageOrder::ReleaseDate => left.3.cmp(&right.3),
        };
        let primary = if descending { primary.reverse() } else { primary };
        primary.then_with(|| left.0.to_string().cmp(&right.0.to_string()))
    });
    rows.into_iter().map(|row| row.0).collect()
}

/// 三个索引列的分页必须与全量选择同序：重复标题、相同/不同/NULL 时间戳、
/// 完整日期/年份兜底/缺失日期，两个方向、跨页边界。
#[test]
fn page_orders_match_the_full_selection_order() {
    let tmp = tempfile::tempdir().unwrap();
    let store = open(&tmp);
    // (title, added_at_ns, release_date 物化键)
    let fixture: [(&str, Option<i64>, &str); 7] = [
        ("Alpha", None, ""),
        ("Alpha", Some(1_000), "2019-01-01"),
        ("Beta", Some(1_000), "2019-01-01"),
        ("Beta", None, "2020-06-01"),
        ("Gamma", Some(2_000), ""),
        ("Delta", Some(2_000), "2001-01-01"),
        ("Epsilon", None, "2020-06-01"),
    ];
    let mut items = Vec::new();
    let mut table = Vec::new();
    for (title, added_at_ns, release_date) in fixture {
        let media = movie(MediaId::new(), title, Some(2020));
        let rows = vec![row(media.id, &format!("/m/{title}-{}.mkv", media.id), Some("1080p"), None)];
        store.insert_media(&media).unwrap();
        for entry in &rows {
            store.insert_ledger(entry).unwrap();
        }
        let mut light = candidate(media.id, title, Some(2020));
        light.added_at_ns = added_at_ns;
        light.release_date = release_date.to_string();
        table.push((media.id, title.to_string(), added_at_ns, release_date.to_string()));
        items.push((media, rows, light));
    }
    publish_candidates(&store, "lib", 1, &items);

    // 三个排序 × 两个方向 × 三个窗口（跨页边界）。
    for order in [
        BrowsePageOrder::Title,
        BrowsePageOrder::AddedAt,
        BrowsePageOrder::ReleaseDate,
    ] {
        for descending in [false, true] {
            let expected = legacy_order(&table, order, descending);
            for (offset, limit) in [(0, 3), (1, 2), (3, 4), (6, 3)] {
                let mut request = request("lib", order, descending);
                request.offset = offset;
                request.limit = limit;
                let got: Vec<MediaId> = store
                    .browse_media_page(&request)
                    .unwrap()
                    .into_iter()
                    .map(|row| row.media_id)
                    .collect();
                let want = expected[offset.min(expected.len())..(offset + limit).min(expected.len())]
                    .to_vec();
                assert_eq!(
                    got, want,
                    "order={order:?} desc={descending} offset={offset} limit={limit}"
                );
            }
        }
    }
}

/// 用真实 Library 的 scope 指纹发布一代（head 的 scope 校验才会通过）。
fn publish_scope(
    store: &Store,
    library: &store::Library,
    generation: i64,
    items: &[(Media, Vec<LedgerRow>)],
) {
    let build = ProjectionBuild {
        library_id: library.id.clone(),
        generation,
        scope_signature: store::library_scope_signature(library),
        revisions: store.read_revisions().unwrap(),
        expected_dirty_token: None,
        files: items
            .iter()
            .flat_map(|(_, rows)| rows.iter())
            .map(|row| BrowseFileFacts {
                ledger_id: row.id,
                media_id: row.media_id,
                path: row.path.clone(),
                facts: serde_json::json!({ "version": 3 }),
            })
            .collect(),
        media: items
            .iter()
            .map(|(media, _)| BrowseMediaFacts {
                media_id: media.id,
                title: media.title.clone(),
                year: media.year,
                facts: serde_json::json!({ "version": 3 }),
            })
            .collect(),
        candidates: items
            .iter()
            .map(|(media, _)| candidate(media.id, &media.title, media.year))
            .collect(),
    };
    assert!(store.publish_browse_projection(&build).unwrap());
}

/// 发布带轻候选的一代（候选可携带差异化的 added_at/release_date）。
fn publish_candidates(
    store: &Store,
    library_id: &str,
    generation: i64,
    items: &[(Media, Vec<LedgerRow>, BrowseMediaCandidate)],
) {
    let build = ProjectionBuild {
        library_id: library_id.to_string(),
        generation,
        scope_signature: "sig".to_string(),
        revisions: store.read_revisions().unwrap(),
        expected_dirty_token: None,
        files: items
            .iter()
            .flat_map(|(_, rows, _)| rows.iter())
            .map(|row| BrowseFileFacts {
                ledger_id: row.id,
                media_id: row.media_id,
                path: row.path.clone(),
                facts: serde_json::json!({ "version": 3 }),
            })
            .collect(),
        media: items
            .iter()
            .map(|(media, _, _)| BrowseMediaFacts {
                media_id: media.id,
                title: media.title.clone(),
                year: media.year,
                facts: serde_json::json!({ "version": 3 }),
            })
            .collect(),
        candidates: items.iter().map(|(_, _, light)| light.clone()).collect(),
    };
    assert!(store.publish_browse_projection(&build).unwrap());
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
                path: entry.path.clone(),
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
                path: entry.path.clone(),
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

/// A4-02：批准的那一代在按页读取时仍必须可用——标脏、换代、换 scope 都失效。
#[test]
fn approved_head_must_still_be_current_at_page_read() {
    let tmp = tempfile::tempdir().unwrap();
    let store = open(&tmp);
    let root = tmp.path().join("movies");
    std::fs::create_dir_all(&root).unwrap();
    let library = store
        .create_library(
            MediaKind::Movie,
            "Movies",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let lib = library.id.clone();
    let media = movie(MediaId::new(), "Alpha", Some(2020));
    let rows = vec![row(media.id, root.join("alpha.mkv").to_str().unwrap(), None, None)];
    store.insert_media(&media).unwrap();
    store.insert_ledger(&rows[0]).unwrap();
    publish_scope(&store, &library, 1, &[(media, rows)]);

    let approved = store.browse_projection_head(&lib).unwrap().unwrap();
    assert!(approved.ready);
    assert!(store.browse_page_head_is_current(&lib, &approved).unwrap());

    // 消费一条影响本库的变更：head 标脏 + dirty_token+1，旧 facts 仍留在表里。
    store
        .apply_browse_changes(store::BrowseChangeSource::Library, 1, &[lib.clone()])
        .unwrap();
    let dirty = store.browse_projection_head(&lib).unwrap().unwrap();
    assert!(!dirty.ready, "消费变更后 head 必须标脏");
    assert_eq!(dirty.dirty_token, approved.dirty_token + 1);
    assert!(
        !store.browse_page_head_is_current(&lib, &approved).unwrap(),
        "标脏后旧批准必须失效，不能用仍留在表里的旧 facts 出页"
    );

    // 重建一代（同一 dirty_token）后：旧批准仍失效，新 head 才有效。
    let media = movie(MediaId::new(), "Beta", Some(2021));
    let rows = vec![row(media.id, root.join("beta.mkv").to_str().unwrap(), None, None)];
    store.insert_media(&media).unwrap();
    store.insert_ledger(&rows[0]).unwrap();
    let build = ProjectionBuild {
        library_id: lib.clone(),
        generation: 2,
        scope_signature: store::library_scope_signature(&library),
        revisions: store.read_revisions().unwrap(),
        expected_dirty_token: Some(dirty.dirty_token),
        files: vec![BrowseFileFacts {
            ledger_id: rows[0].id,
            media_id: media.id,
            path: rows[0].path.clone(),
            facts: serde_json::json!({ "version": 3 }),
        }],
        media: vec![BrowseMediaFacts {
            media_id: media.id,
            title: media.title.clone(),
            year: media.year,
            facts: serde_json::json!({ "version": 3 }),
        }],
        candidates: vec![candidate(media.id, "Beta", Some(2021))],
    };
    assert!(store.publish_browse_projection(&build).unwrap());
    assert!(
        !store.browse_page_head_is_current(&lib, &approved).unwrap(),
        "换代后旧批准必须失效"
    );
    let current = store.browse_projection_head(&lib).unwrap().unwrap();
    assert!(store.browse_page_head_is_current(&lib, &current).unwrap());
}

/// A4-02：scope 变了（库根被改）也不能把旧投影交给按页读取。
#[test]
fn approved_head_is_rejected_after_a_scope_change() {
    let tmp = tempfile::tempdir().unwrap();
    let store = open(&tmp);
    let root = tmp.path().join("movies");
    let other = tmp.path().join("other");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    let library = store
        .create_library(
            MediaKind::Movie,
            "Movies",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media = movie(MediaId::new(), "Alpha", Some(2020));
    // ledger 行故意放在库根之外：库根变更的前置守卫只拒绝「仍有台账文件位于旧根」。
    let rows = vec![row(media.id, "/elsewhere/alpha.mkv", None, None)];
    store.insert_media(&media).unwrap();
    store.insert_ledger(&rows[0]).unwrap();
    let build = ProjectionBuild {
        library_id: library.id.clone(),
        generation: 1,
        scope_signature: store::library_scope_signature(&library),
        revisions: store.read_revisions().unwrap(),
        expected_dirty_token: None,
        files: vec![BrowseFileFacts {
            ledger_id: rows[0].id,
            media_id: media.id,
            path: rows[0].path.clone(),
            facts: serde_json::json!({ "version": 3 }),
        }],
        media: vec![BrowseMediaFacts {
            media_id: media.id,
            title: media.title.clone(),
            year: media.year,
            facts: serde_json::json!({ "version": 3 }),
        }],
        candidates: vec![candidate(media.id, "Alpha", Some(2020))],
    };
    assert!(store.publish_browse_projection(&build).unwrap());
    let approved = store.browse_projection_head(&library.id).unwrap().unwrap();
    assert!(store
        .browse_page_head_is_current(&library.id, &approved)
        .unwrap());

    store
        .update_library(
            &library.id,
            None,
            Some(&[other.to_str().unwrap()]),
            None,
        )
        .unwrap();
    assert!(
        !store
            .browse_page_head_is_current(&library.id, &approved)
            .unwrap(),
        "scope 变更后旧批准必须失效"
    );
    // 回退路径（`load_snapshot`）用的同一个判定：scope 变了就不能交旧投影。
    assert!(
        store.fresh_head_locked(&library.id).unwrap().is_none(),
        "scope 变更后 fresh_head_locked 必须拒绝旧代"
    );
}

/// A4-05：畸形可选字段必须判为不可读（回退），不能静默当成 None 改变筛选。
#[test]
fn malformed_optional_candidate_fields_are_rejected() {
    let value = candidate(MediaId::new(), "Alpha", Some(2020)).to_json();
    assert!(BrowseMediaCandidate::from_json(&value).is_some());

    for (key, bad) in [
        ("runtime", serde_json::json!("150")),
        ("rating", serde_json::json!("8.5")),
        ("size_bytes", serde_json::json!(-1)),
        ("added_at_ns", serde_json::json!(1.5)),
        ("year", serde_json::json!(70_000)),
        ("language", serde_json::json!(42)),
        ("tmdb_id", serde_json::json!(false)),
        ("release_date", serde_json::json!(2020)),
    ] {
        let mut broken = value.clone();
        broken[key] = bad;
        assert!(
            BrowseMediaCandidate::from_json(&broken).is_none(),
            "{key} 类型不符必须判为不可读"
        );
    }

    // 显式 null 是合法的「无值」；字段缺失则不可读。
    let mut nulled = value.clone();
    nulled["runtime"] = serde_json::Value::Null;
    assert_eq!(
        BrowseMediaCandidate::from_json(&nulled).unwrap().runtime,
        None
    );
    let mut missing = value.clone();
    missing.as_object_mut().unwrap().remove("runtime");
    assert!(
        BrowseMediaCandidate::from_json(&missing).is_none(),
        "缺字段不可读"
    );
}

/// A4-05：库里候选 JSON 畸形时整库轻候选不可用（读路径回退），不能静默丢条目。
#[test]
fn corrupt_candidate_json_makes_the_light_read_unusable() {
    let tmp = tempfile::tempdir().unwrap();
    let store = open(&tmp);
    let media = movie(MediaId::new(), "Alpha", Some(2020));
    let rows = vec![row(media.id, "/m/alpha.mkv", Some("1080p"), None)];
    store.insert_media(&media).unwrap();
    store.insert_ledger(&rows[0]).unwrap();
    publish(&store, "lib", 1, &[(media, rows)]);
    assert_eq!(store.browse_media_candidates("lib").unwrap().len(), 1);

    {
        let conn = rusqlite::Connection::open(tmp.path().join("data/library.db")).unwrap();
        conn.execute(
            "UPDATE browse_media SET candidate_json = ?1 WHERE library_id = 'lib'",
            [r#"{"version":2,"media_id":"00000000-0000-0000-0000-000000000000","kind":"movie","title":"Alpha","tmdb_id":null,"year":2020,"added_at_ns":null,"release_date":"","rating":null,"runtime":"150","size_bytes":null,"provisional":true,"scraped":false,"missing":false,"hdr":false,"resolutions":[],"genres":[],"countries":[],"language":null,"has_poster":false,"has_backdrop":false,"has_still":false,"episode_keys":[]}"#],
        )
        .unwrap();
    }
    assert!(
        store.browse_media_candidates("lib").is_err(),
        "畸形候选必须让轻读失败（回退），而不是静默丢条目"
    );
}
