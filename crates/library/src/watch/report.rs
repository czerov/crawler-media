use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanRootStatus {
    Completed,
    Partial,
    Unreachable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanIssue {
    pub path: PathBuf,
    pub kind: std::io::ErrorKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanRootReport {
    pub root: PathBuf,
    pub status: ScanRootStatus,
    pub visited: usize,
    pub skipped: usize,
    pub error_count: usize,
    pub issues: Vec<ScanIssue>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScanReport {
    pub roots: Vec<ScanRootReport>,
    pub scanned: usize,
    pub inserted: usize,
    pub restored: usize,
    pub probe_pending: usize,
    pub error_count: usize,
    pub errors_truncated: bool,
}
