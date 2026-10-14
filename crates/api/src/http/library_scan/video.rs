//! Video library scanning and per-root reporting.

use std::path::{Path, PathBuf};
use axum::http::StatusCode;
use axum::response::Response;
use serde_json::json;

use crate::http::err;
use crate::management::ApiState;
use super::report::format_scan_report;
use super::scope::ScanScope;

pub(crate) fn walk_video_files_reported(
    dir: &Path,
    out: &mut Vec<PathBuf>,
    report: &mut library::ScanRootReport,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(err) => {
            report.error_count += 1;
            report.issues.push(library::ScanIssue {
                path: dir.to_path_buf(),
                kind: err.kind(),
            });
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                report.error_count += 1;
                report.issues.push(library::ScanIssue {
                    path: dir.to_path_buf(),
                    kind: err.kind(),
                });
                continue;
            }
        };
        let path = entry.path();
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(err) => {
                report.error_count += 1;
                report.issues.push(library::ScanIssue {
                    path,
                    kind: err.kind(),
                });
                continue;
            }
        };
        if meta.file_type().is_symlink() {
            report.skipped += 1;
            continue;
        }
        if meta.is_dir() {
            walk_video_files_reported(&path, out, report);
        } else if meta.is_file() {
            if library::is_video_file(&path) {
                out.push(path);
            } else {
                report.skipped += 1;
            }
        }
    }
}

/// 扫描一个 Video Library 的库根。
///
/// `scan_scope` 由调用方（runner）捕获并完成「捕获 scope == 持久化 signature」复核后
/// 传入：Video 与 Movie/TV 共用同一条绑定路径，发布前不得绕过（P2-4）。
pub(crate) fn scan_video_library(
    state: &ApiState,
    roots: &[PathBuf],
    lease: &crate::library_lifecycle::LibraryWorkLease,
    scan_scope: &ScanScope,
) -> Result<(library::ScanReport, serde_json::Value), Response> {
    let mut all_files = Vec::new();
    let mut root_reports = Vec::new();

    for root in roots {
        let mut r_files = Vec::new();
        let mut report = library::ScanRootReport {
            root: root.clone(),
            status: library::ScanRootStatus::Completed,
            visited: 0,
            skipped: 0,
            error_count: 0,
            issues: vec![],
        };
        if !root.exists() {
            report.status = library::ScanRootStatus::Unreachable;
            root_reports.push(report);
            continue;
        }
        walk_video_files_reported(root, &mut r_files, &mut report);
        report.visited = r_files.len();
        if report.error_count > 0 && report.status != library::ScanRootStatus::Unreachable {
            report.status = library::ScanRootStatus::Partial;
        }
        root_reports.push(report);
        all_files.extend(r_files);
    }

    let files = match scan_scope.filter_paths(all_files) {
        Ok(f) => f,
        Err(e) => return Err(err(StatusCode::CONFLICT, "library.ownership_ambiguous", &format!("{:?}", e))),
    };

    let inserted = match lease.publish(|| {
        let store = state.store.lock();
        scan_scope.validate(&store).map_err(|e| format!("{:?}", e))?;
        crate::watch_ledger::record_video_paths(&store, files.clone())
    }) {
        Ok(inserted) => inserted,
        Err(error) => {
            if error.contains("Deleting") {
                return Err(err(StatusCode::CONFLICT, "library.deleting", "Library 正在删除"));
            }
            if error.contains("ScopeChanged") {
                return Err(err(StatusCode::CONFLICT, "library.scope_changed", "Library 配置变化"));
            }
            return Err(err(StatusCode::INTERNAL_SERVER_ERROR, "store.error", &error));
        }
    };

    crate::http::library::enqueue_probes_for_paths(state, inserted.clone());
    for file in &files {
        let media = domain::Media {
            id: domain::MediaId::new(),
            kind: domain::MediaKind::Video,
            title: file
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string(),
            year: None,
            original_title: None,
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        // 真实失败由 poster_fetch 内部记录；None 表示没有可写的图。
        let _ = crate::poster_fetch::attach_poster(state, &media, &file);
        let _ = crate::poster_fetch::attach_backdrop(state, &media, &file);
    }

    let total_errors: usize = root_reports.iter().map(|r| r.error_count).sum();
    let scan_report = library::ScanReport {
        roots: root_reports,
        scanned: files.len(),
        inserted: inserted.len(),
        restored: 0,
        probe_pending: inserted.len(),
        error_count: total_errors,
        errors_truncated: false,
    };
    let report_data = format_scan_report(&scan_report);
    Ok((scan_report, json!(report_data)))
}
