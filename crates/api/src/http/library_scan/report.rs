//! HTTP response serialization for detailed library scan reports.

use std::path::PathBuf;

use library::{ScanIssue, ScanReport, ScanRootReport, ScanRootStatus};
use serde::Serialize;

#[derive(Serialize)]
pub struct ScanRootResponse {
    pub path: String,
    pub status: &'static str,
    pub visited: usize,
    pub errors: usize,
}

#[derive(Serialize)]
pub struct ScanIssueResponse {
    pub path: String,
    pub error: String,
}

#[derive(Serialize)]
pub struct ScanReportResponseData {
    pub queued: bool,
    pub transferred: bool,
    pub status: &'static str,
    pub scanned: usize,
    pub inserted: usize,
    pub restored: usize,
    pub probe_pending: usize,
    pub roots: Vec<ScanRootResponse>,
    pub error_count: usize,
    pub errors_truncated: bool,
    pub issues: Vec<ScanIssueResponse>,
}

pub fn format_scan_report(report: &ScanReport) -> ScanReportResponseData {
    let mut total_errors = 0;
    let mut root_responses = Vec::with_capacity(report.roots.len());
    let mut issues = Vec::new();

    for r in &report.roots {
        total_errors += r.error_count;
        let status_str = match r.status {
            ScanRootStatus::Completed => "completed",
            ScanRootStatus::Partial => "partial",
            ScanRootStatus::Unreachable => "unreachable",
        };
        root_responses.push(ScanRootResponse {
            path: r.root.display().to_string(),
            status: status_str,
            visited: r.visited,
            errors: r.error_count,
        });

        for issue in &r.issues {
            if issues.len() < 20 {
                issues.push(ScanIssueResponse {
                    path: issue.path.display().to_string(),
                    error: format!("{:?}", issue.kind),
                });
            }
        }
    }

    let overall_status = if root_responses.iter().any(|r| r.status == "unreachable" || r.status == "partial") || total_errors > 0 {
        "partial"
    } else {
        "completed"
    };

    ScanReportResponseData {
        queued: true,
        transferred: true,
        status: overall_status,
        scanned: report.scanned,
        inserted: report.inserted,
        restored: report.restored,
        probe_pending: report.probe_pending,
        roots: root_responses,
        error_count: total_errors,
        errors_truncated: total_errors > 20,
        issues,
    }
}
