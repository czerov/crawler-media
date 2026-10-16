//! D3-R2-06：单事务内 journal 产量有界（阈值 + 一条保守脏事件），且小批量写仍逐行记录。

use domain::{Confidence, LedgerId, LedgerRow, MediaId, MediaKind, QualitySource};
use store::{BrowseChangeSource, StoredMediaMarker, Store};

/// 与 `crates/store/src/schema/browse_changes.rs::JOURNAL_EVENT_THRESHOLD` 一致
/// （该模块对 crate 外部不可见，schema 模块是私有的）。
const JOURNAL_EVENT_THRESHOLD: usize = 1024;

/// 与 `schema::browse_changes::COALESCED_EVENT_KIND` 一致。
const COALESCED_KIND: &str = "browse_all";

fn create_test_store() -> (tempfile::TempDir, Store) {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    (tmp, store)
}

fn sample_media() -> domain::Media {
    domain::Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Burst Movie".to_string(),
        year: Some(2025),
        original_title: None,
        tmdb_id: Some("4242".to_string()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

fn sample_ledger_row(id: LedgerId, media_id: MediaId, path: &str) -> LedgerRow {
    LedgerRow {
        id,
        media_id,
        path: path.to_string(),
        season: None,
        episode: None,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    }
}

fn sample_marker(media_id: MediaId, episode: u32) -> StoredMediaMarker {
    StoredMediaMarker {
        media_id,
        season: 1,
        episode,
        intro_start_ms: Some(10_000),
        intro_end_ms: Some(70_000),
        outro_start_ms: None,
        outro_end_ms: None,
        source: "test".to_string(),
        locked: false,
        updated_at: 1_000,
    }
}

fn read_journal(store: &Store, source: BrowseChangeSource) -> Vec<store::BrowseChange> {
    store.browse_changes(source, 0, 1_000_000).unwrap()
}

/// 确认并裁剪整个源 journal，让后续批量从空 journal 开始计量。
fn drain_journal(store: &Store, source: BrowseChangeSource) {
    let batch = read_journal(store, source);
    if let Some(last) = batch.last() {
        store.apply_browse_changes(source, last.seq, &[]).unwrap();
    }
    assert!(
        read_journal(store, source).is_empty(),
        "确认后源 journal 必须被裁剪为空"
    );
}

#[test]
fn single_transaction_marker_refresh_bounds_journal_production() {
    let (_tmp, store) = create_test_store();
    let media = sample_media();
    store.insert_media(&media).unwrap();

    // 基线：一整个 season 的 marker（本身也是一次批量写，随后确认裁剪干净）。
    let baseline: Vec<StoredMediaMarker> = (1..=2_000u32)
        .map(|ep| sample_marker(media.id, ep))
        .collect();
    store
        .replace_marker_results(media.id, 1, &baseline, &[])
        .unwrap();
    drain_journal(&store, BrowseChangeSource::Library);

    // 一次 marker 刷新 = 一个事务里删除并重插 2,000 条剧集 marker（4,000 次行事件）。
    let refreshed: Vec<StoredMediaMarker> = (1..=2_000u32)
        .map(|ep| sample_marker(media.id, ep))
        .collect();
    store
        .replace_marker_results(media.id, 1, &refreshed, &[])
        .unwrap();

    let journal = read_journal(&store, BrowseChangeSource::Library);
    let coalesced: Vec<_> = journal
        .iter()
        .filter(|c| c.entity_kind == COALESCED_KIND)
        .collect();
    assert_eq!(coalesced.len(), 1, "保守脏事件必须恰好出现一次");
    assert_eq!(coalesced[0].entity_key, "*");
    assert_eq!(
        journal.last().unwrap().entity_kind,
        COALESCED_KIND,
        "保守事件是最后一条（未被裁剪的兜底事件）"
    );
    assert!(
        journal.len() <= JOURNAL_EVENT_THRESHOLD + 1,
        "单事务 journal 增长 {} 超过阈值 + 1",
        journal.len()
    );
    assert!(
        journal.len() < 4_000,
        "journal 增长 {} 未低于本事务的 4,000 次行事件",
        journal.len()
    );

    // 效不失：保守事件走真实 apply 路径推进游标并标脏来源库。
    let last_seq = journal.last().unwrap().seq;
    let library_id = "lib-burst".to_string();
    store
        .apply_browse_changes(BrowseChangeSource::Library, last_seq, &[library_id.clone()])
        .unwrap();
    let head = store.browse_projection_head(&library_id).unwrap().unwrap();
    assert_eq!(head.dirty_token, 1);
    assert!(!head.ready);
    assert_eq!(store.browse_change_cursor(BrowseChangeSource::Library).unwrap(), last_seq);
    assert!(read_journal(&store, BrowseChangeSource::Library).is_empty());
}

#[test]
fn small_transactions_keep_per_row_events_without_coalescing() {
    let (_tmp, store) = create_test_store();
    let media = sample_media();
    store.insert_media(&media).unwrap();

    for episode in 1..=3 {
        store
            .insert_ledger(&sample_ledger_row(
                LedgerId::new(),
                media.id,
                &format!("/media/burst/ep{episode}.mkv"),
            ))
            .unwrap();
    }

    let library = read_journal(&store, BrowseChangeSource::Library);
    assert_eq!(library.len(), 3, "小批量写必须逐行记录");
    assert!(library.iter().all(|c| c.entity_kind == "ledger"));
    assert!(
        !library.iter().any(|c| c.entity_kind == COALESCED_KIND),
        "未达阈值不得产生保守脏事件"
    );

    let app = read_journal(&store, BrowseChangeSource::App);
    let media_events: Vec<_> = app
        .iter()
        .filter(|c| c.entity_kind == "media" && c.entity_key == media.id.to_string())
        .collect();
    assert_eq!(media_events.len(), 1, "插入的 media 必须逐行记录一次");
    assert!(!app.iter().any(|c| c.entity_kind == COALESCED_KIND));
}

/// 升级路径：`migrate_browse_changes_*` 每次打开库都执行（不受 `user_version` 门控），
/// 老库里的无上限触发器必须被 `DROP` + `CREATE` 替换，否则有界产量对已存在的库无效。
#[test]
fn reopening_replaces_legacy_unbounded_triggers() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    let media = {
        let store = Store::open(&data_dir).unwrap();
        let media = sample_media();
        store.insert_media(&media).unwrap();
        media
    };

    {
        let conn = rusqlite::Connection::open(data_dir.join("library.db")).unwrap();
        conn.execute_batch(
            "DROP TRIGGER IF EXISTS trg_lib_ledger_changes_ins;
             CREATE TRIGGER trg_lib_ledger_changes_ins AFTER INSERT ON ledger
             BEGIN
                 INSERT INTO _browse_changes (entity_kind, entity_key, old_path, new_path)
                 VALUES ('ledger', NEW.id, NULL, NEW.path);
             END;",
        )
        .unwrap();
    }

    let store = Store::open(&data_dir).unwrap();
    for episode in 1..=1_200 {
        store
            .insert_ledger(&sample_ledger_row(
                LedgerId::new(),
                media.id,
                &format!("/media/legacy/ep{episode}.mkv"),
            ))
            .unwrap();
    }

    let journal = read_journal(&store, BrowseChangeSource::Library);
    assert_eq!(
        journal
            .iter()
            .filter(|c| c.entity_kind == COALESCED_KIND)
            .count(),
        1,
        "重开库后旧触发器必须已被替换"
    );
    assert!(journal.len() <= JOURNAL_EVENT_THRESHOLD + 1);
    assert!(journal.len() < 1_200);
}
