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
//!
//! ## 模块布局
//!
//! 本文件是模块树 + 入口（[`scan_library_leased_resumable`]）与目标展开；按职责拆出的
//! 兄弟模块：`walk`（chunk 遍历与处理）、`checkpoint`（断点判定/事实失效）、
//! `metadata`（身份解析与刮削）、`scope`（发布前 scope 绑定复核）、`outcome`（终态载体
//! 与失败响应）、`hooks`（测试屏障，仅 `cfg(test)`）。

mod checkpoint;
mod metadata;
mod outcome;
mod scope;
mod walk;

pub(crate) use checkpoint::checkpoint_fresh;

#[cfg(test)]
mod hooks;

#[cfg(test)]
mod tests;

use std::path::PathBuf;

use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::http::err;
use crate::http::library::{clear_library_cover_checked, missing_library};
use crate::management::ApiState;
use crate::scrape_store::ScrapeStoreExt;

use super::library_by_id;
use super::report::format_scan_report;
use outcome::{ScanRun, failed_response};
use scope::capture_and_verify_scope;
use walk::ScanContext;

pub(crate) use outcome::{ScanRunError, ScanRunFailure};
#[cfg(test)]
pub(crate) use hooks::{test_metadata_hook, test_observe_hook, test_scope_hook};

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
    // Video 与 Movie/TV 走同一条有界路径：同样的目标展开、chunk、取消与断点。
    // Video 专属语义只在 chunk 内分叉（登记与封面，见 `video` / `walk`）。
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
        // 封面失效只做锁内短 DB 读写：`clear_library_cover_checked` 清掉「可重试」
        // 标记，让下一次封面读取走 `LibraryCoverService::ensure_cover` 重生成。
        //
        // 这里**不再**调用 `library_cover_path`（P2-2）：它只做 `Path::is_file`，
        // 返回值本来就被丢弃，但在 Store 锁内做文件系统 IO 违反 B3 §1.3（慢挂载
        // 会拖住所有 Store 使用者）。封面路径解析/生成由封面服务在锁外负责。
        let store = state.store.lock();
        if let Some(lib) = library_by_id(&store, id) {
            clear_library_cover_checked(&store, &lib.id);
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
