//! 可断点续扫的 Library 扫描执行器（B3 §4.1/§4.2）。
//!
//! 走 `library::ScanWalker`（有界、可取消），逐 chunk 处理：过滤归属 → 入账 →
//! 身份解析 → 探测入队 → 记 `scan_seen` 断点。中断/重启后同一 operation 只处理
//! 断点之外的文件，已处理过的不会重复入账或重复探测。
//!
//! 锁纪律（B3 §2）：文件版本事实在 Store 锁外采集，锁内只做短 DB 读写与内存判断。
//!
//! ## Yield 语义（P3-3，明确未交回 coordinator）
//!
//! `ScanWalker::next_chunk` 在 100 文件 / 5000 目录项 / 10 秒任一预算到点时返回
//! `yielded = true`。本执行器把 `yielded` 只当作 chunk 边界：发布并记录断点后继续在
//! 同一个 blocking 任务里遍历，直到整库走完才把 coordinator 槽位交回。
//!
//! 没有实现「每个 yield 交回槽位」是刻意的，不是静默忽略：B3 §4.2 禁止依赖不可靠的
//! `read_dir` 位置跨进程续扫，交回后下一轮只能从 root 重新枚举 + `scan_seen` 去重，
//! 而默认每 100 文件就 yield 会让 N 文件的扫描退化成约 N²/100 次目录枚举。要正确交回，
//! 必须先设计可持久化的目录游标，或把「100 文件 chunk 边界」与「10 秒/5000 项目轮边界」
//! 拆成两个信号——两者都超出本次修复范围。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

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

/// 一个 chunk 处理失败：HTTP 语义 + 错误摘要，根报告由调用方补齐。
///
/// `totals` 是**本 chunk 在失败前已经提交的计数**（P3-3）：ledger 登记先于元数据
/// 解析/重绑，后半段失败时调用方必须把这段已提交前缀算进失败报告。
struct ChunkError {
    response: Response,
    error: String,
    totals: ChunkTotals,
}

/// 扫描执行失败：保留已提交前缀的报告与错误摘要（P3-3）。
pub(crate) struct ScanRunFailure {
    pub response: Response,
    pub report: Value,
    pub error: String,
}

/// 扫描执行错误。
pub(crate) enum ScanRunError {
    /// 持久化 signature 与捕获 scope 不一致（P2-4）：必须取消且零登记。
    ScopeChanged { reason: String },
    Failed(ScanRunFailure),
}

/// 一个根/目标目录遍历失败时带回的已提交前缀。
struct ChunkFailure {
    response: Response,
    error: String,
    root_report: library::ScanRootReport,
    totals: ChunkTotals,
}

/// 断点判定结果（P2-NEW-2 / P2-3）。
struct CheckpointDecision {
    fresh: Vec<library::TransferredFile>,
    skipped: usize,
    /// 命中断点行但版本已变化（或台账失效）的路径：必须失效派生事实并重排探测。
    changed: Vec<PathBuf>,
    /// 锁外采集到的版本事实：`path -> (mtime_ms, file_size, content_digest)`。
    stamps: HashMap<String, (i64, i64, Option<String>)>,
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
        root: &Path,
        start: &Path,
    ) -> Result<(library::ScanRootReport, ChunkTotals), ChunkFailure> {
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
        let mut yields = 0usize;
        loop {
            let chunk = match walker.next_chunk(&budget) {
                Ok(chunk) => chunk,
                Err(error) => return Err(self.walk_failure(report, totals, error)),
            };
            // 预算到点只切分 chunk（发布 + 断点）；槽位仍在整库走完后才交回，
            // 见模块文档「Yield 语义」。
            if chunk.yielded {
                yields += 1;
            }
            report.visited += chunk.visited;
            report.skipped += chunk.skipped;
            report.error_count += chunk.issues.len();
            report.issues.extend(chunk.issues);
            let decision = checkpoint_fresh(self.state, self.op_id, chunk.files);
            report.skipped += decision.skipped;
            let processed =
                match self.process_chunk(decision.fresh, &decision.stamps, &decision.changed) {
                    Ok(processed) => processed,
                    Err(error) => {
                        // 失败 chunk 自己已提交的前缀也要计入，否则「ledger 已写入但
                        // 后续阶段失败」的轮次会少报这部分计数（P3-3）。
                        totals.scanned += error.totals.scanned;
                        totals.inserted += error.totals.inserted;
                        return Err(ChunkFailure {
                            response: error.response,
                            error: error.error,
                            root_report: report,
                            totals,
                        });
                    }
                };
            totals.scanned += processed.scanned;
            totals.inserted += processed.inserted;
            if chunk.done {
                break;
            }
        }
        if yields > 0 {
            tracing::debug!(
                op_id = ?self.op_id,
                root = %start.display(),
                yields,
                "扫描按预算切分了多个 chunk（未交回 coordinator 槽位）"
            );
        }
        if report.error_count > 0 {
            report.status = library::ScanRootStatus::Partial;
        }
        Ok((report, totals))
    }

    /// 遍历失败（取消 / 目录读失败）→ 保留已提交前缀的失败。
    fn walk_failure(
        &self,
        report: library::ScanRootReport,
        totals: ChunkTotals,
        error: library::LibraryError,
    ) -> ChunkFailure {
        match error {
            // 取消（Library 删除/冻结）沿用既有语义：409 library.deleting，不是 500。
            library::LibraryError::Cancelled => ChunkFailure {
                response: err(StatusCode::CONFLICT, "library.deleting", "Library 正在删除"),
                error: "library.deleting".to_string(),
                root_report: report,
                totals,
            },
            other => {
                let summary = other.to_string();
                ChunkFailure {
                    response: err(StatusCode::INTERNAL_SERVER_ERROR, "library.scan", &summary),
                    error: summary,
                    root_report: report,
                    totals,
                }
            }
        }
    }

    /// 处理一个 chunk：过滤归属 → 入账 → 失效变化文件事实 → 身份解析 → 探测入队 → 记断点。
    ///
    /// 断点写在**处理成功之后**：中途崩溃的文件下一轮会重新处理，不会因为检查点丢账。
    fn process_chunk(
        &self,
        files: Vec<library::TransferredFile>,
        stamps: &HashMap<String, (i64, i64, Option<String>)>,
        changed: &[PathBuf],
    ) -> Result<ChunkTotals, ChunkError> {
        if files.is_empty() && changed.is_empty() {
            return Ok(ChunkTotals::default());
        }
        let filtered = self
            .scan_scope
            .filter_transferred(files)
            .map_err(presence_failure)?;
        let scanned = filtered.len();
        let inserted = self
            .lease
            .publish(|| {
                let store = self.state.store.lock();
                self.scan_scope
                    .validate(&store)
                    .map_err(|e| format!("{e:?}"))?;
                crate::watch_ledger::record_paths(&store, filtered.clone())
            })
            .map_err(publish_failure)?;
        // 版本变化的文件：派生探测事实必须失效，并按 durable obligation 合同重排探测。
        // 只处理本 Library 真正拥有的路径（过滤后仍在 `filtered` 里）。
        let changed_in_scope: Vec<PathBuf> = changed
            .iter()
            .filter(|path| {
                let key = path.display().to_string();
                filtered
                    .iter()
                    .any(|file| file.path.display().to_string() == key)
            })
            .cloned()
            .collect();
        let affected = invalidate_changed_facts(self.state, &changed_in_scope);
        let scan_media = scan_media_for(self.state, &filtered);
        // 到这里 ledger 已提交：任何后续失败都必须把这段前缀带回调用方（P3-3）。
        let committed = ChunkTotals {
            scanned,
            inserted: inserted.len(),
        };
        // 测试屏障：注入「ledger 已提交、元数据解析失败」的故障（P3-3）。
        #[cfg(test)]
        if let Some(response) = test_metadata_hook::run(self.op_id.unwrap_or_default()) {
            return Err(ChunkError {
                response,
                error: "library.scan_metadata".to_string(),
                totals: committed,
            });
        }
        resolve_scan_metadata(self.state, self.id, self.lease, scan_media, &inserted, self.scrape)
            .map_err(|response| ChunkError {
                response,
                error: "library.scan_metadata".to_string(),
                totals: committed,
            })?;
        let mut probe_paths = inserted.clone();
        probe_paths.extend(affected);
        crate::http::library::enqueue_probes_for_paths(self.state, probe_paths);
        self.record_seen(&filtered, stamps);
        Ok(committed)
    }

    /// 把处理完成的路径写进本 operation 的 `scan_seen` 断点（复用锁外采集的版本事实）。
    fn record_seen(
        &self,
        filtered: &[library::TransferredFile],
        stamps: &HashMap<String, (i64, i64, Option<String>)>,
    ) {
        let Some(op_id) = self.op_id else { return };
        let entries: Vec<(String, i64, i64, Option<String>)> = filtered
            .iter()
            .filter_map(|file| {
                let key = file.path.display().to_string();
                stamps
                    .get(&key)
                    .map(|(mtime_ms, file_size, digest)| {
                        (key, *mtime_ms, *file_size, digest.clone())
                    })
            })
            .collect();
        if entries.is_empty() {
            return;
        }
        let store = self.state.store.lock();
        if let Err(error) = store.record_scan_seen_stamps(op_id, &entries) {
            tracing::error!(%error, %op_id, "写入扫描断点失败");
        }
    }
}

/// 变化文件：失效派生探测事实，并重挂 durable probe 义务（P2-3）。
///
/// 返回实际完成失效的路径，供调用方立即请求探测（obligation 是崩溃后的兜底）。
fn invalidate_changed_facts(state: &ApiState, changed: &[PathBuf]) -> Vec<PathBuf> {
    if changed.is_empty() {
        return Vec::new();
    }
    let now_ms = crate::job_loop::unix_now() * 1000;
    let store = state.store.lock();
    let mut affected = Vec::new();
    for path in changed {
        let path_str = path.display().to_string();
        let Some(row) = store.ledger_by_path(&path_str).ok().flatten() else {
            continue;
        };
        let ledger_id = row.id.to_string();
        if let Err(error) = store.delete_file_meta_by_ledger_id(&ledger_id) {
            tracing::error!(%error, %ledger_id, %path_str, "变化文件的派生探测事实失效失败");
            continue;
        }
        if let Err(error) = store.arm_scan_probe_obligation(&ledger_id, now_ms, "scan_changed") {
            tracing::error!(%error, %ledger_id, "变化文件重挂探测义务失败");
            continue;
        }
        affected.push(path.clone());
    }
    affected
}

/// 断点续扫：丢掉本 operation 已经处理过、且仍然有效的文件（P2-NEW-2 / P2-3）。
///
/// 版本事实（size/mtime，`.strm` 另有受限内容指纹）在 Store 锁外采集；进锁后只做一次
/// 批量 DB 读与纯内存判定。读检查点失败时按「未处理」继续——宁可重复一遍，也不能漏文件。
fn checkpoint_fresh(
    state: &ApiState,
    op_id: Option<&str>,
    files: Vec<library::TransferredFile>,
) -> CheckpointDecision {
    let Some(op_id) = op_id else {
        // 无 operation 归属 = 无断点，不需要采集版本事实。
        return CheckpointDecision {
            fresh: files,
            skipped: 0,
            changed: Vec::new(),
            stamps: HashMap::new(),
        };
    };
    let mut stamps = HashMap::with_capacity(files.len());
    for file in &files {
        // 测试屏障：位于文件版本观察窗口内（锁外），用来证明慢观察不持 Store 锁。
        #[cfg(test)]
        test_observe_hook::run(op_id);
        stamps.insert(
            file.path.display().to_string(),
            store::Store::scan_seen_stamp(&file.path),
        );
    }
    let paths: Vec<String> = files.iter().map(|f| f.path.display().to_string()).collect();
    let checkpoints = {
        let store = state.store.lock();
        store.scan_seen_checkpoints(op_id, &paths)
    };
    let checkpoints = match checkpoints {
        Ok(checkpoints) => checkpoints,
        Err(error) => {
            tracing::error!(%error, %op_id, "读取扫描断点失败，按未处理继续");
            return CheckpointDecision {
                fresh: files,
                skipped: 0,
                changed: Vec::new(),
                stamps,
            };
        }
    };
    let mut fresh = Vec::with_capacity(files.len());
    let mut changed = Vec::new();
    let mut skipped = 0usize;
    for file in files {
        let key = file.path.display().to_string();
        let current = stamps.get(&key).cloned().unwrap_or((0, 0, None));
        match checkpoints.get(&key) {
            Some((mtime_ms, file_size, digest, true))
                if store::Store::scan_seen_stamp_matches(
                    &(*mtime_ms, *file_size, digest.clone()),
                    &current,
                ) =>
            {
                skipped += 1;
            }
            Some(_) => {
                changed.push(file.path.clone());
                fresh.push(file);
            }
            None => fresh.push(file),
        }
    }
    CheckpointDecision {
        fresh,
        skipped,
        changed,
        stamps,
    }
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

fn presence_failure(error: crate::library_presence::PresenceError) -> ChunkError {
    let summary = format!("{error:?}");
    ChunkError {
        response: presence_error(error),
        error: summary,
        totals: ChunkTotals::default(),
    }
}

fn publish_failure(error: String) -> ChunkError {
    let response = publish_error(error.clone());
    ChunkError {
        response,
        error,
        totals: ChunkTotals::default(),
    }
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

/// 失败但无已提交前缀（入口级错误）。
fn failed_response(response: Response, error: impl Into<String>) -> ScanRunError {
    ScanRunError::Failed(ScanRunFailure {
        response,
        report: json!({}),
        error: error.into(),
    })
}

/// 本轮扫描已提交前缀的累计状态。
#[derive(Default)]
struct ScanRun {
    root_reports: Vec<library::ScanRootReport>,
    totals: ChunkTotals,
    any_readable: bool,
}

impl ScanRun {
    /// 用当前已提交前缀构造报告（失败终态也要保留它，P3-3）。
    fn partial_report(&self) -> library::ScanReport {
        let total_errors: usize = self.root_reports.iter().map(|r| r.error_count).sum();
        library::ScanReport {
            roots: self.root_reports.clone(),
            scanned: self.totals.scanned,
            inserted: self.totals.inserted,
            restored: 0,
            probe_pending: self.totals.inserted,
            error_count: total_errors,
            errors_truncated: false,
        }
    }
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
) -> Result<(library::ScanReport, Value), ScanRunError> {
    let (kind, roots, scrape) = {
        let store = state.store.lock();
        let Some(library) = library_by_id(&store, id) else {
            return Err(failed_response(missing_library(), "library.missing"));
        };
        let cfg = store.get_scrape_config().ok();
        let nfo = cfg.as_ref().map(|c| c.effective.mirror_nfo).unwrap_or(true);
        (library.kind, library.root_paths.clone(), nfo)
    };
    // 测试屏障：在「准入读到的配置」与「捕获 scope」之间执行一次（P2-4 / P3-1）。
    #[cfg(test)]
    test_scope_hook::run(op_id.unwrap_or_default());
    if kind == domain::MediaKind::Video {
        if !full_scope && !targets.is_empty() {
            tracing::warn!(library_id = id, "Video Library 暂不支持增量目标扫描，按整库执行");
        }
        // Video 也必须在发布任何文件之前把捕获 scope 绑定到持久化 signature（P2-4）。
        let scan_scope = capture_and_verify_scope(state, id, op_id)?;
        return scan_video_library(state, &roots, lease, &scan_scope)
            .map_err(|response| failed_response(response, "library.scan_video"));
    }
    let starts = scan_starts(&roots, targets, full_scope);
    if starts.is_empty() {
        return Err(failed_response(
            err(
                StatusCode::BAD_REQUEST,
                "library.scan_target",
                "扫描目标不属于该媒体库",
            ),
            "library.scan_target",
        ));
    }
    let scan_scope = capture_and_verify_scope(state, id, op_id)?;
    let mut run = ScanRun::default();
    for (root, start) in starts {
        if state.library_files.readable_root(&root).is_ok() {
            run.any_readable = true;
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
        match ctx.walk_and_process(&root, &start) {
            Ok((report, chunk)) => {
                run.totals.scanned += chunk.scanned;
                run.totals.inserted += chunk.inserted;
                run.root_reports.push(report);
            }
            Err(failure) => {
                run.totals.scanned += failure.totals.scanned;
                run.totals.inserted += failure.totals.inserted;
                run.root_reports.push(failure.root_report);
                let partial = run.partial_report();
                return Err(ScanRunError::Failed(ScanRunFailure {
                    response: failure.response,
                    report: json!(format_scan_report(&partial)),
                    error: failure.error,
                }));
            }
        }
    }
    if !run.any_readable {
        let partial = run.partial_report();
        return Err(ScanRunError::Failed(ScanRunFailure {
            response: err(
                StatusCode::CONFLICT,
                "library.root_unavailable",
                "所有库根目录均不可达",
            ),
            report: json!(format_scan_report(&partial)),
            error: "library.root_unavailable".to_string(),
        }));
    }
    {
        let store = state.store.lock();
        if let Some(lib) = library_by_id(&store, id) {
            crate::http::library::clear_library_cover_checked(&store, &lib.id);
            let _ = crate::http::library::library_cover_path(&store, &lib);
        }
    }
    let total_errors: usize = run.root_reports.iter().map(|r| r.error_count).sum();
    let scan_report = library::ScanReport {
        roots: run.root_reports,
        scanned: run.totals.scanned,
        inserted: run.totals.inserted,
        restored: 0,
        probe_pending: run.totals.inserted,
        error_count: total_errors,
        errors_truncated: false,
    };
    let report_data = format_scan_report(&scan_report);
    Ok((scan_report, json!(report_data)))
}

/// 捕获 scope 并把它绑定到持久化 signature（P2-4）。
///
/// Movie/TV 与 Video 共用同一条绑定路径：任何发布之前都必须完成复核。
fn capture_and_verify_scope(
    state: &ApiState,
    id: &str,
    op_id: Option<&str>,
) -> Result<ScanScope, ScanRunError> {
    let scan_scope = ScanScope::capture(state, id).map_err(|error| {
        let summary = format!("{error:?}");
        failed_response(presence_error(error), summary)
    })?;
    verify_captured_scope(state, op_id, &scan_scope)?;
    Ok(scan_scope)
}

/// 发布前复核：持久化 signature 必须等于刚捕获的 scope（P2-4）。
///
/// 准入（`scan_admission`）释放 Store 锁之后、`ScanScope::capture` 之前，配置可能被
/// PATCH；此时旧 operation 绝不能拿新 roots 登记文件。
///
/// 读不到 operation 或 operation 已消失时必须**失败关闭**（P2-4）：`ScanScope::validate`
/// 只能证明捕获 scope 与当前配置一致，不能替代「捕获 scope == 持久化 signature」，
/// 放行等于让一个无法对账的 operation 继续登记文件。
fn verify_captured_scope(
    state: &ApiState,
    op_id: Option<&str>,
    scan_scope: &ScanScope,
) -> Result<(), ScanRunError> {
    let Some(op_id) = op_id else { return Ok(()) };
    let persisted = {
        let store = state.store.lock();
        store.scan_operation(op_id)
    };
    match persisted {
        Ok(Some(op)) if op.scope_signature != scan_scope.signature() => {
            let reason = format!(
                "captured scope changed: persisted={} captured={}",
                op.scope_signature,
                scan_scope.signature()
            );
            tracing::warn!(%op_id, %reason, "捕获 scope 与持久化 signature 不一致，取消过期 operation");
            Err(ScanRunError::ScopeChanged { reason })
        }
        Ok(Some(_)) => Ok(()),
        Ok(None) => {
            tracing::error!(%op_id, "扫描 operation 已不存在，拒绝在无法对账的情况下发布");
            Err(failed_response(
                err(
                    StatusCode::CONFLICT,
                    "library.scan_operation_missing",
                    "扫描 operation 不存在",
                ),
                "library.scan_operation_missing",
            ))
        }
        Err(error) => {
            tracing::error!(%error, %op_id, "读取扫描 operation 失败，拒绝在无法对账的情况下发布");
            Err(failed_response(
                err(StatusCode::INTERNAL_SERVER_ERROR, "store.error", &error.to_string()),
                format!("scan operation unreadable: {error}"),
            ))
        }
    }
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

/// 测试专用屏障：在「读取 Library 配置」与「捕获 scope」之间按 Library 执行一次。
///
/// 用来确定性地复现 P2-4 的 TOCTOU（准入后 PATCH roots）与 P3-1 的轮内并入。
#[cfg(test)]
pub(crate) mod test_scope_hook {
    use std::collections::HashMap;
    use std::sync::Mutex;

    type Hook = Box<dyn FnOnce() + Send + 'static>;
    static HOOKS: Mutex<Option<HashMap<String, Hook>>> = Mutex::new(None);

    pub(crate) fn set(library_id: &str, hook: Hook) {
        HOOKS
            .lock()
            .unwrap()
            .get_or_insert_with(HashMap::new)
            .insert(library_id.to_string(), hook);
    }

    pub(crate) fn run(library_id: &str) {
        let hook = HOOKS
            .lock()
            .unwrap()
            .get_or_insert_with(HashMap::new)
            .remove(library_id);
        if let Some(hook) = hook {
            hook();
        }
    }
}

/// 测试专用屏障：在每个 chunk 的文件版本观察之前执行一次。
///
/// 用来证明慢文件观察（模拟慢 NAS stat）期间不持有 Store 锁（P2-NEW-2）。
#[cfg(test)]
pub(crate) mod test_observe_hook {
    use std::collections::HashMap;
    use std::sync::Mutex;

    type Hook = Box<dyn FnOnce() + Send + 'static>;
    static HOOKS: Mutex<Option<HashMap<String, Hook>>> = Mutex::new(None);

    pub(crate) fn set(library_id: &str, hook: Hook) {
        HOOKS
            .lock()
            .unwrap()
            .get_or_insert_with(HashMap::new)
            .insert(library_id.to_string(), hook);
    }

    pub(crate) fn run(library_id: &str) {
        let hook = HOOKS
            .lock()
            .unwrap()
            .get_or_insert_with(HashMap::new)
            .remove(library_id);
        if let Some(hook) = hook {
            hook();
        }
    }
}

/// 测试专用屏障：在 chunk 台账已提交、元数据解析之前执行一次，可注入一个失败响应。
///
/// 用来复现「ledger 已提交但后续阶段失败」的故障，验证失败报告仍带上该 chunk 的
/// 已提交前缀（P3-3 fault injection）。
#[cfg(test)]
pub(crate) mod test_metadata_hook {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use axum::response::Response;

    type Hook = Box<dyn FnOnce() -> Option<Response> + Send + 'static>;
    static HOOKS: Mutex<Option<HashMap<String, Hook>>> = Mutex::new(None);

    pub(crate) fn set(op_id: &str, hook: Hook) {
        HOOKS
            .lock()
            .unwrap()
            .get_or_insert_with(HashMap::new)
            .insert(op_id.to_string(), hook);
    }

    pub(crate) fn run(op_id: &str) -> Option<Response> {
        HOOKS
            .lock()
            .unwrap()
            .get_or_insert_with(HashMap::new)
            .remove(op_id)
            .and_then(|hook| hook())
    }
}
