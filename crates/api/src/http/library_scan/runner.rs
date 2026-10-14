//! 可断点续扫的 Library 扫描执行器（B3 §4.1/§4.2）。
//!
//! 走 `library::ScanWalker`（有界、可取消），逐 chunk 处理：过滤归属 → 入账 →
//! 身份解析 → 探测入队 → 记 `scan_seen` 断点。中断/重启后同一 operation 只处理
//! 断点之外的文件，已处理过的不会重复入账或重复探测。

use std::collections::HashMap;
use std::path::PathBuf;

use axum::http::StatusCode;
use axum::response::Response;
use serde_json::{Value, json};

use crate::http::err;
use crate::http::library::{missing_library, rows_in_library};
use crate::management::ApiState;
use crate::scrape_store::ScrapeStoreExt;

use super::report::format_scan_report;
use super::scope::ScanScope;
use super::video::scan_video_library;
use super::{ledger_row_by_path, library_by_id, media_by_id, scrape_library_metadata};

/// 一次扫描里单个根/目标目录的处理计数。
#[derive(Default, Clone, Copy)]
struct ChunkTotals {
    scanned: usize,
    inserted: usize,
}

/// 有界、可取消、带 `scan_seen` 断点的扫描上下文。
struct ScanContext<'a> {
    state: &'a ApiState,
    id: &'a str,
    lease: &'a crate::library_lifecycle::LibraryWorkLease,
    scan_scope: &'a ScanScope,
    kind: domain::MediaKind,
    scrape: bool,
    op_id: Option<&'a str>,
}

impl ScanContext<'_> {
    /// 用 `ScanWalker` 走一个根或目标子目录，逐 chunk 处理并记断点。
    fn walk_and_process(
        &self,
        root: &std::path::Path,
        start: &std::path::Path,
    ) -> Result<(library::ScanRootReport, ChunkTotals), Response> {
        let mut report = library::ScanRootReport {
            root: start.to_path_buf(),
            status: library::ScanRootStatus::Completed,
            visited: 0,
            skipped: 0,
            error_count: 0,
            issues: vec![],
        };
        let mut totals = ChunkTotals::default();
        if !start.exists() {
            report.status = library::ScanRootStatus::Unreachable;
            return Ok((report, totals));
        }
        // roots 传库根（NFO 身份解析边界），targets 传真正要走的目录。
        let mut walker = library::ScanWalker::from_targets(
            vec![root.to_path_buf()],
            vec![start.to_path_buf()],
            self.kind,
            self.lease.control().clone(),
        );
        let budget = library::ScanWalkBudget::default();
        loop {
            let chunk = match walker.next_chunk(&budget) {
                Ok(chunk) => chunk,
                // 取消（Library 删除/冻结）沿用既有语义：409 library.deleting，不是 500。
                Err(library::LibraryError::Cancelled) => {
                    return Err(err(
                        StatusCode::CONFLICT,
                        "library.deleting",
                        "Library 正在删除",
                    ));
                }
                Err(error) => {
                    return Err(err(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "library.scan",
                        &error.to_string(),
                    ));
                }
            };
            report.visited += chunk.visited;
            report.skipped += chunk.skipped;
            report.error_count += chunk.issues.len();
            report.issues.extend(chunk.issues);
            let (fresh, seen_skipped) = checkpoint_fresh(self.state, self.op_id, chunk.files);
            report.skipped += seen_skipped;
            let processed = self.process_chunk(fresh)?;
            totals.scanned += processed.scanned;
            totals.inserted += processed.inserted;
            if chunk.done {
                break;
            }
        }
        if report.error_count > 0 {
            report.status = library::ScanRootStatus::Partial;
        }
        Ok((report, totals))
    }

    /// 处理一个 chunk：过滤归属 → 入账 → 身份解析 → 探测入队 → 记断点。
    ///
    /// 断点写在**处理成功之后**：中途崩溃的文件下一轮会重新处理，不会因为检查点丢账。
    fn process_chunk(&self, files: Vec<library::TransferredFile>) -> Result<ChunkTotals, Response> {
        if files.is_empty() {
            return Ok(ChunkTotals::default());
        }
        let filtered = self
            .scan_scope
            .filter_transferred(files)
            .map_err(presence_error)?;
        let scanned = filtered.len();
        let inserted = self
            .lease
            .publish(|| {
                let store = self.state.store.lock();
                self.scan_scope.validate(&store).map_err(|e| format!("{e:?}"))?;
                crate::watch_ledger::record_paths(&store, filtered.clone())
            })
            .map_err(publish_error)?;
        let scan_media = scan_media_for(self.state, &filtered);
        resolve_scan_metadata(self.state, self.id, self.lease, scan_media, &inserted, self.scrape)?;
        crate::http::library::enqueue_probes_for_paths(self.state, inserted.clone());
        self.record_seen(&filtered);
        Ok(ChunkTotals {
            scanned,
            inserted: inserted.len(),
        })
    }

    /// 把处理完成的路径写进本 operation 的 `scan_seen` 断点。
    fn record_seen(&self, filtered: &[library::TransferredFile]) {
        let Some(op_id) = self.op_id else { return };
        let paths: Vec<PathBuf> = filtered.iter().map(|file| file.path.clone()).collect();
        let store = self.state.store.lock();
        if let Err(error) = store.record_scan_seen(op_id, &paths) {
            tracing::error!(%error, %op_id, "写入扫描断点失败");
        }
    }
}

/// 断点续扫：丢掉本 operation 已经处理过、且仍然有效的文件。
///
/// 「有效」= 台账行仍存在、未标记 missing、文件版本（size/mtime）与记录一致。
/// 恢复的文件与内容变化的文件必须重新处理（P2-3）。
///
/// 读检查点失败时按「未处理」继续——宁可重复一遍，也不能因为检查点读不出来而漏文件。
fn checkpoint_fresh(
    state: &ApiState,
    op_id: Option<&str>,
    files: Vec<library::TransferredFile>,
) -> (Vec<library::TransferredFile>, usize) {
    let Some(op_id) = op_id else {
        return (files, 0);
    };
    let store = state.store.lock();
    let mut fresh = Vec::with_capacity(files.len());
    let mut skipped = 0usize;
    for file in files {
        let path = file.path.display().to_string();
        match store.scan_seen_is_current(op_id, &path) {
            Ok(true) => skipped += 1,
            Ok(false) => fresh.push(file),
            Err(error) => {
                tracing::error!(%error, %op_id, path, "读取扫描断点失败，按未处理继续");
                fresh.push(file);
            }
        }
    }
    (fresh, skipped)
}

/// 把库根与增量目标映射成 (身份解析根, 起始目录) 列表。
///
/// `scope_kind == full`（或 `targets` 为空）= 整库；`targeted` 时只走目标子目录，
/// 库根仍用于 NFO 身份解析。Full 支配 Targeted：整库意图绝不会被目标列表缩小。
fn scan_starts(
    roots: &[PathBuf],
    targets: &[PathBuf],
    full_scope: bool,
) -> Vec<(PathBuf, PathBuf)> {
    if full_scope || targets.is_empty() {
        return roots
            .iter()
            .map(|root| (root.clone(), root.clone()))
            .collect();
    }
    let mut starts = Vec::new();
    for target in targets {
        let owner = roots
            .iter()
            .filter(|root| target.starts_with(root))
            .max_by_key(|root| root.components().count());
        match owner {
            Some(root) => starts.push((root.clone(), target.clone())),
            None => tracing::warn!(target = %target.display(), "增量扫描目标不属于任何库根，已忽略"),
        }
    }
    starts
}

/// chunk 内涉及到的非 Video Media 台账行（身份解析按 Media 去重）。
fn scan_media_for(
    state: &ApiState,
    files: &[library::TransferredFile],
) -> HashMap<domain::MediaId, (domain::Media, domain::LedgerRow)> {
    let store = state.store.lock();
    let mut map = HashMap::new();
    for file in files {
        let Some(row) = ledger_row_by_path(&store, &file.path.display().to_string()) else {
            continue;
        };
        let Some(m) = media_by_id(&store, row.media_id) else {
            continue;
        };
        if m.kind != domain::MediaKind::Video {
            map.entry(m.id).or_insert((m, row));
        }
    }
    map
}

fn presence_error(error: crate::library_presence::PresenceError) -> Response {
    use crate::library_presence::PresenceError;
    match error {
        PresenceError::Deleting => err(StatusCode::CONFLICT, "library.deleting", "Library 正在删除"),
        PresenceError::ScopeChanged => {
            err(StatusCode::CONFLICT, "library.scope_changed", "Library 配置变化")
        }
        PresenceError::Ambiguous => err(
            StatusCode::CONFLICT,
            "library.ownership_ambiguous",
            "媒体库归属歧义",
        ),
        other => err(StatusCode::CONFLICT, "library.scan", &format!("{other:?}")),
    }
}

fn publish_error(error: String) -> Response {
    if error.contains("Deleting") {
        return err(StatusCode::CONFLICT, "library.deleting", "Library 正在删除");
    }
    if error.contains("ScopeChanged") {
        return err(StatusCode::CONFLICT, "library.scope_changed", "Library 配置变化");
    }
    err(StatusCode::INTERNAL_SERVER_ERROR, "store.error", &error)
}

/// 可断点续扫的扫描入口：`scope_kind == targeted` 时只扫 `targets` 子目录，
/// `full` 时忽略 `targets` 走整库；`op_id` 存在时启用 `scan_seen` 断点。
pub(crate) fn scan_library_leased_resumable(
    state: &ApiState,
    id: &str,
    lease: &crate::library_lifecycle::LibraryWorkLease,
    targets: &[PathBuf],
    full_scope: bool,
    op_id: Option<&str>,
) -> Result<(library::ScanReport, Value), Response> {
    let (kind, roots, scrape) = {
        let store = state.store.lock();
        let Some(library) = library_by_id(&store, id) else {
            return Err(missing_library());
        };
        let cfg = store.get_scrape_config().ok();
        let nfo = cfg.as_ref().map(|c| c.effective.mirror_nfo).unwrap_or(true);
        (library.kind, library.root_paths.clone(), nfo)
    };
    if kind == domain::MediaKind::Video {
        if !full_scope && !targets.is_empty() {
            tracing::warn!(library_id = id, "Video Library 暂不支持增量目标扫描，按整库执行");
        }
        return scan_video_library(state, &roots, id, lease);
    }
    let starts = scan_starts(&roots, targets, full_scope);
    if starts.is_empty() {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "library.scan_target",
            "扫描目标不属于该媒体库",
        ));
    }
    let scan_scope = ScanScope::capture(state, id).map_err(presence_error)?;
    let mut any_readable = false;
    let mut root_reports = Vec::new();
    let mut totals = ChunkTotals::default();
    for (root, start) in starts {
        if state.library_files.readable_root(&root).is_ok() {
            any_readable = true;
        }
        let ctx = ScanContext {
            state,
            id,
            lease,
            scan_scope: &scan_scope,
            kind,
            scrape,
            op_id,
        };
        let (report, chunk) = ctx.walk_and_process(&root, &start)?;
        totals.scanned += chunk.scanned;
        totals.inserted += chunk.inserted;
        root_reports.push(report);
    }
    if !any_readable {
        return Err(err(StatusCode::CONFLICT, "library.root_unavailable", "所有库根目录均不可达"));
    }
    {
        let store = state.store.lock();
        if let Some(lib) = library_by_id(&store, id) {
            crate::http::library::clear_library_cover_checked(&store, &lib.id);
            let _ = crate::http::library::library_cover_path(&store, &lib);
        }
    }
    let total_errors: usize = root_reports.iter().map(|r| r.error_count).sum();
    let scan_report = library::ScanReport {
        roots: root_reports,
        scanned: totals.scanned,
        inserted: totals.inserted,
        restored: 0,
        probe_pending: totals.inserted,
        error_count: total_errors,
        errors_truncated: false,
    };
    let report_data = format_scan_report(&scan_report);
    Ok((scan_report, json!(report_data)))
}

fn resolve_scan_metadata(
    state: &ApiState,
    id: &str,
    lease: &crate::library_lifecycle::LibraryWorkLease,
    scan_media: HashMap<domain::MediaId, (domain::Media, domain::LedgerRow)>,
    inserted: &[PathBuf],
    scrape: bool,
) -> Result<(), Response> {
    for (_, (media, row)) in scan_media {
        if lease.control().is_cancelled() {
            return Err(err(
                StatusCode::CONFLICT,
                "library.deleting",
                "Library 正在删除",
            ));
        }
        let path_buf = PathBuf::from(&row.path);
        let path = path_buf.as_path();
        if crate::auto_resolve::has_local_scan_metadata(state, &media, path) {
            continue;
        }
        let resolved = crate::auto_resolve::resolve_scanned_media(state, &media, path)
            .unwrap_or_else(|| media.clone());
        if let Err(error) =
            crate::auto_resolve::rebind_scan_rows(state, media.id, resolved.id, &inserted)
        {
            return Err(err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error,
            ));
        }
        if scrape {
            // 见上：真实失败由 poster_fetch 内部记录，None 不是错误。
            let _ = crate::poster_fetch::attach_poster(state, &resolved, path);
            let _ = crate::poster_fetch::attach_backdrop(state, &resolved, path);
            // Reread after alias convergence; stale rows still contain the provisional ID.
            let (row, rows) = {
                let store = state.store.lock();
                let row = ledger_row_by_path(&store, &row.path).unwrap_or(row);
                let rows = library_by_id(&store, id)
                    .map(|library| rows_in_library(&store, &library))
                    .unwrap_or_default();
                (row, rows)
            };
            scrape_library_metadata(state, &resolved, &row, path, &rows);
        }
    }

    Ok(())
}
