//! Video 扫描路径的 WorkControl 取消测试，以及「封面候选 ≠ 探测候选」的
//! chunk 级契约（P2-1）。
//!
//! 集成测试无法确定性地「在遍历中途」取消（`test_scope_hook` 只在 crate 内可见，
//! 且 `WorkControl` 由 coordinator 在运行时注册）。这里直接驱动 `ScanContext`，
//! 证明 Video 也走同一个可取消 walker：控制被取消时立刻失败、零登记，并把
//! 409 `library.deleting` 语义交回调用方。

use std::sync::Arc;

use axum::http::StatusCode;
use domain::MediaKind;

use crate::management::ApiState;
use crate::scrape_store::ScrapeStoreExt;

struct NoFetch;

impl indexer::Fetcher for NoFetch {
    fn fetch(&self, _: &indexer::FetchRequest) -> Result<String, indexer::IndexerError> {
        Err(indexer::IndexerError::Fetch("offline".into()))
    }
}

/// 目录身份固定指向一张海报：封面走 catalog 通道，不依赖 ffmpeg。
struct ArtworkCatalog;

impl crate::catalog::Catalog for ArtworkCatalog {
    fn search_movie(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn search_tv(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn details(&self, _: MediaKind, _: &str) -> Result<Option<domain::Media>, String> {
        Ok(None)
    }
    fn poster_url(&self, _: MediaKind, _: &str) -> Result<Option<String>, String> {
        Ok(Some("https://image.tmdb.org/t/p/w342/clip-poster.jpg".into()))
    }
    fn image_candidates(
        &self,
        _: MediaKind,
        _: &str,
    ) -> Result<crate::catalog::ArtworkCandidates, String> {
        Ok(crate::catalog::ArtworkCandidates::default())
    }
}

struct OfflinePosterFetch;

impl crate::PosterFetch for OfflinePosterFetch {
    fn get(&self, _: &str) -> Result<Vec<u8>, String> {
        Ok(b"poster-image-bytes".to_vec())
    }
}

fn video_fixture() -> (tempfile::TempDir, ApiState, String, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::Store::open(dir.path().join("data")).unwrap();
    let state = ApiState::new(
        store,
        "test-token".into(),
        indexer::ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(downloader::MemoryDownloader::new(dir.path().join("stage"))),
        dir.path().join("library"),
    )
    .unwrap()
    .with_catalog(Arc::new(ArtworkCatalog))
    .with_poster_fetch(Arc::new(OfflinePosterFetch));
    let root = dir.path().join("homevideos");
    std::fs::create_dir_all(&root).unwrap();
    let library_id = {
        let store = state.store();
        let guard = store.lock();
        guard
            .create_library(
                MediaKind::Video,
                "Home Videos",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap()
            .id
    };
    (dir, state, library_id, root)
}

/// 取消的 Library work control 必须让 Video chunk 处理在登记之前停下。
#[test]
fn cancelled_video_scan_fails_without_ledgering() {
    let (_dir, state, library_id, root) = video_fixture();
    std::fs::write(root.join("clip.mp4"), b"v").unwrap();

    let lease = state.acquire_library_work(&library_id).unwrap();
    let scan_scope =
        crate::http::library_scan::scope::ScanScope::capture(&state, &library_id).unwrap();
    lease.control().cancel();

    let ctx = super::walk::ScanContext {
        state: &state,
        id: &library_id,
        lease: &lease,
        scan_scope: &scan_scope,
        kind: MediaKind::Video,
        scrape: false,
        op_id: None,
    };
    let failure = ctx
        .walk_and_process(&root, &root)
        .err()
        .expect("取消必须让 Video 遍历失败");
    assert_eq!(
        failure.response.status(),
        StatusCode::CONFLICT,
        "取消沿用 library.deleting 合同"
    );
    assert_eq!(failure.error, "library.deleting");
    assert_eq!(failure.totals.scanned, 0);
    assert_eq!(failure.totals.inserted, 0);
    assert!(
        state.store().lock().list_ledger().unwrap().is_empty(),
        "取消的 Video 扫描不得登记任何文件"
    );
}

fn set_mirror_images(state: &ApiState, enabled: bool) {
    let store = state.store();
    let guard = store.lock();
    let mut setting = guard.get_scrape_config().unwrap().setting;
    setting.mirror_images = Some(enabled);
    guard.save_scrape_config(&setting).unwrap();
}

fn walk_once(state: &ApiState, library_id: &str, root: &std::path::Path) -> super::walk::ChunkTotals {
    let lease = state.acquire_library_work(library_id).unwrap();
    let scan_scope = crate::http::library_scan::scope::ScanScope::capture(state, library_id).unwrap();
    let ctx = super::walk::ScanContext {
        state,
        id: library_id,
        lease: &lease,
        scan_scope: &scan_scope,
        kind: MediaKind::Video,
        scrape: false,
        op_id: None,
    };
    match ctx.walk_and_process(root, root) {
        Ok((_report, totals)) => totals,
        Err(failure) => panic!("Video 遍历必须成功: {}", failure.error),
    }
}

/// P2-1：已入账、本轮未变化的 Video 文件必须**重新尝试封面**，但不得被重复登记。
///
/// 探测候选（新增/恢复 + 变化）与封面候选（本 chunk 处理过的全部文件）是两套集合：
/// 沿用探测候选会让普通重扫永远不再尝试封面（镜像后开启无法回填、失败无法重试）。
#[test]
fn unchanged_video_file_is_artwork_attempted_without_reregistration() {
    let (_dir, state, library_id, root) = video_fixture();
    let clip = root.join("clip.mp4");
    std::fs::write(&clip, b"v").unwrap();

    // 第一轮：镜像关闭 → 只登记，不写封面。
    set_mirror_images(&state, false);
    let first = walk_once(&state, &library_id, &root);
    assert_eq!(first.inserted, 1);
    assert!(!root.join("poster.jpg").exists(), "镜像关闭时不得写封面");

    // 用户随后打开镜像，并让已入账 Media 拥有可解析的目录身份；文件内容不变。
    set_mirror_images(&state, true);
    {
        let store = state.store();
        let guard = store.lock();
        let row = guard
            .ledger_by_path(&clip.display().to_string())
            .unwrap()
            .expect("clip 必须已入账");
        let mut media = guard.get_media(row.media_id).unwrap().expect("media");
        media.tmdb_id = Some("603".into());
        guard.update_media(&media).unwrap();
    }

    // 第二轮：同一个文件、同样的版本事实 → 不再登记，但封面必须重试。
    let second = walk_once(&state, &library_id, &root);
    assert_eq!(second.scanned, 1, "未变化的文件仍在本 chunk 的处理集合里");
    assert_eq!(second.inserted, 0, "未变化的文件不得重复登记");
    assert_eq!(
        std::fs::read(root.join("poster.jpg")).unwrap(),
        b"poster-image-bytes",
        "未变化的 Video 文件必须重新尝试封面"
    );
}

/// P2-2：故障注入测试：断点失败、durable 失败、两者同时失败、断点命中四种组合。
#[test]
fn checkpoint_decision_fault_injection_matrix() {
    let (dir, state, _library_id, root) = video_fixture();
    let file_path = root.join("movie.mp4");
    std::fs::write(&file_path, b"hello-world-content").unwrap();
    let file = library::TransferredFile {
        path: file_path.clone(),
        identified_release: None,
        nfo_identity: None,
    };
    let key = file_path.display().to_string();
    let current_stamp = store::Store::scan_seen_stamp(&file_path);

    let op_id = "test-op-1";

    // 预备数据：在 ledger 中写入已知版本（大小与内容相同）
    let media = domain::Media {
        id: domain::MediaId::new(),
        kind: MediaKind::Movie,
        title: "Test Movie".into(),
        year: Some(2024),
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    {
        let store = state.store();
        let guard = store.lock();
        guard.insert_media(&media).unwrap();
        guard.insert_ledger(&domain::LedgerRow {
            id: domain::LedgerId::new(),
            media_id: media.id,
            path: key.clone(),
            season: None,
            episode: None,
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: domain::QualitySource::Release,
            confidence: domain::Confidence::High,
            filter_score: None,
        }).unwrap();
        guard.record_scan_seen_stamps(op_id, &[(key.clone(), current_stamp.0, current_stamp.1, current_stamp.2.clone())]).unwrap();
    }

    // 组合 1：断点命中（断点查询成功且版本匹配，durable 查询也成功）
    {
        let dec = super::checkpoint_fresh(&state, Some(op_id), vec![file.clone()]);
        assert_eq!(dec.skipped, 1, "断点命中必须跳过");
        assert_eq!(dec.changed.len(), 0, "断点命中无变化文件");
        assert_eq!(dec.fresh.len(), 0, "断点命中跳过后无 fresh 文件");
    }

    let db_path = dir.path().join("data").join("library.db");

    // 组合 2：断点失败（注入 scan_seen 读取触发器失败，但 durable 成功）
    // 断点查询失败但 durable 成功：仍按 durable 版本判断变化；
    // 此时 durable 版本与当前一致，因此不计为 changed，但由于没有断点命中，fresh 中包含该文件。
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "CREATE TRIGGER fail_scan_seen AFTER INSERT ON scan_seen
             BEGIN
                 SELECT RAISE(ABORT, 'injected scan_seen failure');
             END;",
            [],
        ).unwrap();
        // 重命名 scan_seen 表以让 SELECT s.path FROM scan_seen 失败
        conn.execute("ALTER TABLE scan_seen RENAME TO scan_seen_disabled", []).unwrap();
    }
    {
        let dec = super::checkpoint_fresh(&state, Some(op_id), vec![file.clone()]);
        assert_eq!(dec.skipped, 0, "断点查询失败不得跳过文件");
        assert_eq!(dec.fresh.len(), 1, "断点查询失败按未处理继续进入 fresh");
        assert_eq!(dec.changed.len(), 0, "durable 版本匹配，不应判为 changed");

        // 如果文件内容修改，durable 版本检测仍应生效并将文件计入 changed！
        std::fs::write(&file_path, b"changed-new-content").unwrap();
        let changed_file = library::TransferredFile {
            path: file_path.clone(),
            identified_release: None,
            nfo_identity: None,
        };
        let dec_changed = super::checkpoint_fresh(&state, Some(op_id), vec![changed_file]);
        assert_eq!(dec_changed.changed.len(), 1, "断点查询失败但 durable 成功：仍应按 durable 版本检测出变化！");
        assert_eq!(dec_changed.changed[0], file_path);

        // 恢复文件内容
        std::fs::write(&file_path, b"hello-world-content").unwrap();
    }
    // 恢复 scan_seen 表
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute("ALTER TABLE scan_seen_disabled RENAME TO scan_seen", []).unwrap();
    }

    // 组合 3：durable 失败（断点查询成功，但 durable 查询失败）
    // fail-closed: 无论文件是否变化，全部候选列入 changed，不得确认未知版本。
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute("ALTER TABLE ledger RENAME TO ledger_disabled", []).unwrap();
    }
    {
        let dec = super::checkpoint_fresh(&state, Some(op_id), vec![file.clone()]);
        assert_eq!(dec.skipped, 0, "durable 失败不得跳过");
        assert_eq!(dec.fresh.len(), 1);
        assert_eq!(dec.changed.len(), 1, "durable 失败必须 fail-closed 将所有候选列入 changed");
        assert_eq!(dec.changed[0], file_path);
    }
    // 恢复 ledger 表
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute("ALTER TABLE ledger_disabled RENAME TO ledger", []).unwrap();
    }

    // 组合 4：两者同时失败（scan_seen 和 ledger 同时查询失败）
    // 仍必须 fail-closed：全部候选列入 changed！
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute("ALTER TABLE scan_seen RENAME TO scan_seen_disabled", []).unwrap();
        conn.execute("ALTER TABLE ledger RENAME TO ledger_disabled", []).unwrap();
    }
    {
        let dec = super::checkpoint_fresh(&state, Some(op_id), vec![file.clone()]);
        assert_eq!(dec.skipped, 0, "两者均失败不得跳过");
        assert_eq!(dec.fresh.len(), 1);
        assert_eq!(dec.changed.len(), 1, "两者同时失败仍必须保守 fail-closed 列入 changed");
        assert_eq!(dec.changed[0], file_path);
    }
    // 恢复表结构
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute("ALTER TABLE scan_seen_disabled RENAME TO scan_seen", []).unwrap();
        conn.execute("ALTER TABLE ledger_disabled RENAME TO ledger", []).unwrap();
    }
}
