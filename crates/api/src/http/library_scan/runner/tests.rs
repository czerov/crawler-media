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

struct CheckpointFaultFixture {
    _dir: tempfile::TempDir,
    state: ApiState,
    db_path: std::path::PathBuf,
    file_path: std::path::PathBuf,
    file: library::TransferredFile,
    op_id: &'static str,
}

fn checkpoint_fault_fixture() -> CheckpointFaultFixture {
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
        guard.record_scan_seen_stamps(op_id, &[(key, current_stamp.0, current_stamp.1, current_stamp.2)]).unwrap();
    }
    let db_path = dir.path().join("data").join("library.db");
    CheckpointFaultFixture {
        _dir: dir,
        state,
        db_path,
        file_path,
        file,
        op_id,
    }
}

fn set_scan_seen_disabled(db_path: &std::path::Path, disabled: bool) {
    let conn = rusqlite::Connection::open(db_path).unwrap();
    if disabled {
        conn.execute("ALTER TABLE scan_seen RENAME TO scan_seen_disabled", []).unwrap();
    } else {
        conn.execute("ALTER TABLE scan_seen_disabled RENAME TO scan_seen", []).unwrap();
    }
}

/// 只让 durable 源版本查询失败：重命名 `ledger.source_version_ms`。
/// `scan_seen_checkpoints` 不读该列，因此断点查询仍然成功——这是真实 SQL 故障，
/// 不需要在生产代码里留测试钩子（也不会有跨测试的全局状态）。
fn set_durable_disabled(db_path: &std::path::Path, disabled: bool) {
    let conn = rusqlite::Connection::open(db_path).unwrap();
    if disabled {
        conn.execute(
            "ALTER TABLE ledger RENAME COLUMN source_version_ms TO source_version_ms_disabled",
            [],
        )
        .unwrap();
    } else {
        conn.execute(
            "ALTER TABLE ledger RENAME COLUMN source_version_ms_disabled TO source_version_ms",
            [],
        )
        .unwrap();
    }
}

/// 组合 1：断点成功且命中 + durable 成功
#[test]
fn checkpoint_decision_both_ok_hits() {
    let fix = checkpoint_fault_fixture();
    let dec = super::checkpoint_fresh(&fix.state, Some(fix.op_id), vec![fix.file]);
    assert!(dec.checkpoints_ok, "断点查询必须成功");
    assert!(dec.durable_ok, "durable 查询必须成功");
    assert_eq!(dec.skipped, 1, "断点命中必须跳过");
    assert_eq!(dec.changed.len(), 0, "断点命中无变化文件");
    assert_eq!(dec.fresh.len(), 0, "断点命中跳过后无 fresh 文件");
}

/// 组合 2：断点失败 + durable 成功（且 durable 检测到变化时必须进入 changed）
#[test]
fn checkpoint_decision_checkpoint_err_durable_ok() {
    let fix = checkpoint_fault_fixture();
    set_scan_seen_disabled(&fix.db_path, true);

    let dec = super::checkpoint_fresh(&fix.state, Some(fix.op_id), vec![fix.file.clone()]);
    assert!(!dec.checkpoints_ok, "断点查询必须失败");
    assert!(dec.durable_ok, "durable 查询必须成功");
    assert_eq!(dec.skipped, 0, "断点查询失败不得跳过文件");
    assert_eq!(dec.fresh.len(), 1, "断点查询失败按未处理继续进入 fresh");
    assert_eq!(dec.changed.len(), 0, "durable 版本匹配，不应判为 changed");

    // 文件内容修改，durable 检测仍应生效并将文件计入 changed
    std::fs::write(&fix.file_path, b"changed-new-content").unwrap();
    let changed_file = library::TransferredFile {
        path: fix.file_path.clone(),
        identified_release: None,
        nfo_identity: None,
    };
    let dec_changed = super::checkpoint_fresh(&fix.state, Some(fix.op_id), vec![changed_file]);
    assert!(!dec_changed.checkpoints_ok);
    assert!(dec_changed.durable_ok);
    assert_eq!(dec_changed.changed.len(), 1, "断点失败但 durable 成功必须检测到变化");
    assert_eq!(dec_changed.changed[0], fix.file_path);

    set_scan_seen_disabled(&fix.db_path, false);
}

/// 组合 3：断点成功且命中 + durable 失败 → 不跳过、全部 fresh、全部 changed
#[test]
fn checkpoint_decision_checkpoint_hit_durable_err() {
    let fix = checkpoint_fault_fixture();
    set_durable_disabled(&fix.db_path, true);

    let dec = super::checkpoint_fresh(&fix.state, Some(fix.op_id), vec![fix.file]);
    set_durable_disabled(&fix.db_path, false);

    assert!(dec.checkpoints_ok, "断点查询必须成功且命中");
    assert!(!dec.durable_ok, "durable 查询必须失败");
    assert_eq!(dec.skipped, 0, "durable 失败时断点命中也不得跳过");
    assert_eq!(dec.fresh.len(), 1, "durable 失败全部进入 fresh");
    assert_eq!(dec.changed.len(), 1, "durable 失败必须 fail-closed 全部列入 changed");
    assert_eq!(dec.changed[0], fix.file_path);
}

/// 组合 4：两者同时失败 → fail-closed: 全部 fresh、全部 changed
#[test]
fn checkpoint_decision_both_err() {
    let fix = checkpoint_fault_fixture();
    set_scan_seen_disabled(&fix.db_path, true);
    set_durable_disabled(&fix.db_path, true);

    let dec = super::checkpoint_fresh(&fix.state, Some(fix.op_id), vec![fix.file]);
    set_durable_disabled(&fix.db_path, false);
    set_scan_seen_disabled(&fix.db_path, false);

    assert!(!dec.checkpoints_ok, "断点查询必须失败");
    assert!(!dec.durable_ok, "durable 查询必须失败");
    assert_eq!(dec.skipped, 0, "两者均失败不得跳过");
    assert_eq!(dec.fresh.len(), 1);
    assert_eq!(dec.changed.len(), 1, "两者同时失败必须 fail-closed 全部列入 changed");
    assert_eq!(dec.changed[0], fix.file_path);
}

/// 一轮带 operation 归属的 Video 扫描：有 `op_id` 才会写 `scan_seen` 断点。
fn walk_with_op(
    state: &ApiState,
    library_id: &str,
    root: &std::path::Path,
    op_id: &str,
) -> (library::ScanRootReport, super::walk::ChunkTotals) {
    let lease = state.acquire_library_work(library_id).unwrap();
    let scan_scope = crate::http::library_scan::scope::ScanScope::capture(state, library_id).unwrap();
    let ctx = super::walk::ScanContext {
        state,
        id: library_id,
        lease: &lease,
        scan_scope: &scan_scope,
        kind: MediaKind::Video,
        scrape: false,
        op_id: Some(op_id),
    };
    ctx.walk_and_process(root, root)
        .unwrap_or_else(|failure| panic!("Video 遍历必须成功: {}", failure.error))
}

fn set_mtime_ms(path: &std::path::Path, mtime_ms: i64) {
    let time = std::time::UNIX_EPOCH + std::time::Duration::from_millis(mtime_ms as u64);
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(time)
        .unwrap();
}

/// 断言窗口内变化的路径没有被确认为「已处理」：无断点、durable 源版本未推进，
/// 且处理义务已重挂（`scan_changed`，而不是登记时的 `scan`）。
fn assert_not_confirmed(state: &ApiState, op_id: &str, path: &std::path::Path) {
    let key = path.display().to_string();
    let store = state.store();
    let guard = store.lock();
    assert!(
        guard
            .scan_seen_checkpoints(op_id, std::slice::from_ref(&key))
            .unwrap()
            .is_empty(),
        "窗口内变化的路径不得写入 scan_seen 断点"
    );
    assert_eq!(
        guard
            .ledger_source_versions(std::slice::from_ref(&key))
            .unwrap()
            .get(&key)
            .cloned(),
        Some((0, 0, None)),
        "窗口内变化的路径不得推进 durable 源版本"
    );
    let row = guard.ledger_by_path(&key).unwrap().expect("路径必须已入账");
    let obligation = guard
        .scan_probe_obligations(100)
        .unwrap()
        .into_iter()
        .find(|obligation| obligation.ledger_id == row.id)
        .expect("窗口内变化的路径必须重挂处理义务");
    assert_eq!(obligation.reason, "scan_changed");
}

/// 同一 chunk 里未变化的兄弟文件必须照旧推进断点：复核只影响窗口内真正变化的路径，
/// 不能让整块每轮重做（未变化 = 下一轮命中 `scan_seen` 断点被跳过）。
#[test]
fn unchanged_sibling_keeps_checkpoint_while_changed_file_is_reprocessed() {
    let (_dir, state, library_id, root) = video_fixture();
    let keep = root.join("keep.mp4");
    let edit = root.join("edit.mp4");
    std::fs::write(&keep, b"stable").unwrap();
    std::fs::write(&edit, b"v1").unwrap();
    let edit_observed = store::Store::scan_seen_stamp(&edit);

    let rewrite_target = edit.clone();
    super::hooks::test_metadata_hook::set(
        "toctou-mixed",
        Box::new(move || {
            std::fs::write(&rewrite_target, b"v2-changed-content").unwrap();
            None
        }),
    );
    let (_report, first) = walk_with_op(&state, &library_id, &root, "toctou-mixed");
    assert_eq!(first.inserted, 2, "两个文件都必须登记");

    // 未变化的兄弟文件：断点与 durable 源版本照旧推进。
    let keep_key = keep.display().to_string();
    let keep_stamp = store::Store::scan_seen_stamp(&keep);
    {
        let store = state.store();
        let guard = store.lock();
        assert!(
            guard
                .scan_seen_checkpoints("toctou-mixed", std::slice::from_ref(&keep_key))
                .unwrap()
                .contains_key(&keep_key),
            "未变化的文件必须照旧写入断点"
        );
        assert_eq!(
            guard
                .ledger_source_versions(std::slice::from_ref(&keep_key))
                .unwrap()
                .get(&keep_key)
                .cloned(),
            Some(keep_stamp),
            "未变化的文件必须照旧推进 durable 源版本"
        );
    }
    assert_not_confirmed(&state, "toctou-mixed", &edit);

    // 窗口内变化的文件回到观察时的版本：第二轮只重做它，未变化的兄弟仍命中断点。
    std::fs::write(&edit, b"v1").unwrap();
    set_mtime_ms(&edit, edit_observed.0);
    let (second_report, second) = walk_with_op(&state, &library_id, &root, "toctou-mixed");
    assert_eq!(second_report.skipped, 1, "未变化的兄弟文件必须命中断点被跳过");
    assert_eq!(second.scanned, 1, "只有窗口内变化的文件需要重做");
}

/// TOCTOU 窗口（普通文件）：文件在版本观察之后、chunk 处理完成之前被改写时，本轮
/// 绝不能把这个已经过期的版本写成断点/durable 源版本——否则文件回到旧版本后，后续
/// 轮次会把它当成「已完成」直接跳过，而派生事实其实来自被替换的内容（P2-3）。
#[test]
fn file_changed_during_processing_is_not_checkpointed_and_is_reprocessed() {
    let (_dir, state, library_id, root) = video_fixture();
    let clip = root.join("clip.mp4");
    std::fs::write(&clip, b"v1").unwrap();
    let observed = store::Store::scan_seen_stamp(&clip);

    // 屏障位于 ledger 已提交、元数据解析之前：改写落在观察与处理完成之间。
    let rewrite_target = clip.clone();
    super::hooks::test_metadata_hook::set(
        "toctou-file",
        Box::new(move || {
            std::fs::write(&rewrite_target, b"v2-changed-content").unwrap();
            None
        }),
    );
    let (_report, first) = walk_with_op(&state, &library_id, &root, "toctou-file");
    assert_eq!(first.inserted, 1, "第一轮必须登记该文件");
    assert_ne!(
        store::Store::scan_seen_stamp(&clip),
        observed,
        "测试前提：处理期间的改写改变了版本事实"
    );
    assert_not_confirmed(&state, "toctou-file", &clip);

    // 文件回到观察时的版本：被错误推进的断点会让第二轮把它当成已完成而跳过。
    std::fs::write(&clip, b"v1").unwrap();
    set_mtime_ms(&clip, observed.0);
    assert_eq!(store::Store::scan_seen_stamp(&clip), observed, "测试前提：版本已复原");

    let (second_report, second) = walk_with_op(&state, &library_id, &root, "toctou-file");
    assert_eq!(second_report.skipped, 0, "窗口内变化的文件不得被当成已完成跳过");
    assert_eq!(second.scanned, 1, "第二轮必须重新处理该文件");
}

/// TOCTOU 窗口（`.strm`）：URL 换成同长度、同 mtime 的另一条时，size/mtime 无法区分，
/// 只有受限内容指纹能证明变化；窗口内变化的 `.strm` 同样不得被确认（P2-3）。
#[test]
fn strm_url_changed_during_processing_is_not_checkpointed_and_is_reprocessed() {
    let (_dir, state, library_id, root) = video_fixture();
    let strm = root.join("clip.strm");
    std::fs::write(&strm, "https://a/video").unwrap();
    let observed = store::Store::scan_seen_stamp(&strm);

    let rewrite_target = strm.clone();
    super::hooks::test_metadata_hook::set(
        "toctou-strm",
        Box::new(move || {
            std::fs::write(&rewrite_target, "https://b/video").unwrap();
            // 同 size 同 mtime：只有内容指纹能证明版本变化。
            set_mtime_ms(&rewrite_target, observed.0);
            None
        }),
    );
    let (_report, first) = walk_with_op(&state, &library_id, &root, "toctou-strm");
    assert_eq!(first.inserted, 1, "第一轮必须登记该 .strm");

    let changed = store::Store::scan_seen_stamp(&strm);
    assert_eq!(
        (changed.0, changed.1),
        (observed.0, observed.1),
        "测试前提：URL 替换保持 size/mtime 不变"
    );
    assert_ne!(changed.2, observed.2, "测试前提：只有内容指纹能证明变化");
    assert_not_confirmed(&state, "toctou-strm", &strm);

    std::fs::write(&strm, "https://a/video").unwrap();
    set_mtime_ms(&strm, observed.0);
    let (second_report, second) = walk_with_op(&state, &library_id, &root, "toctou-strm");
    assert_eq!(second_report.skipped, 0, "窗口内变化的 .strm 不得被当成已完成跳过");
    assert_eq!(second.scanned, 1, "第二轮必须重新处理该 .strm");
}
