//! Iterative bounded directory walker for safe cancellable library scanning.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use domain::{Confidence, MediaKind, Release};

use crate::watch::report::ScanIssue;
use crate::{LibraryError, TransferredFile, is_video_file};

#[derive(Clone, Debug)]
pub struct ScanWalkBudget {
    pub max_entries: usize,
    pub max_elapsed: Duration,
    pub chunk_files: usize,
}

impl Default for ScanWalkBudget {
    fn default() -> Self {
        Self {
            max_entries: 5000,
            max_elapsed: Duration::from_secs(10),
            chunk_files: 100,
        }
    }
}

pub struct ScanChunk {
    pub files: Vec<TransferredFile>,
    pub visited: usize,
    pub skipped: usize,
    pub issues: Vec<ScanIssue>,
    pub yielded: bool,
    pub done: bool,
}

pub struct ScanWalker {
    /// Library 根：NFO/父目录身份解析的边界（见 `owning_root`）。
    roots: Vec<PathBuf>,
    dir_stack: Vec<PathBuf>,
    current_entries: Option<fs::ReadDir>,
    kind: MediaKind,
    control: crate::WorkControl,
}

impl ScanWalker {
    /// 从库根开始遍历（`start == roots`）。
    pub fn new(roots: Vec<PathBuf>, kind: MediaKind, control: crate::WorkControl) -> Self {
        Self::from_targets(roots.clone(), roots, kind, control)
    }

    /// 只遍历 `targets` 子目录，但仍以 `roots` 作为 NFO 身份解析的库根。
    ///
    /// watch 增量扫描只关心变动的条目子目录，但身份解析必须看到整个库根，
    /// 否则电视剧 NFO 的主身份会退化成文件名解析。
    pub fn from_targets(
        roots: Vec<PathBuf>,
        targets: Vec<PathBuf>,
        kind: MediaKind,
        control: crate::WorkControl,
    ) -> Self {
        let mut dir_stack = targets;
        dir_stack.reverse();
        Self {
            roots,
            dir_stack,
            current_entries: None,
            kind,
            control,
        }
    }

    pub fn next_chunk(&mut self, budget: &ScanWalkBudget) -> Result<ScanChunk, LibraryError> {
        let start = Instant::now();
        let mut files = Vec::with_capacity(budget.chunk_files);
        let mut visited = 0;
        let mut skipped = 0;
        let mut issues = Vec::new();

        loop {
            self.control.check()?;

            if self.current_entries.is_none() {
                let Some(next_dir) = self.dir_stack.pop() else {
                    return Ok(ScanChunk {
                        files,
                        visited,
                        skipped,
                        issues,
                        yielded: false,
                        done: true,
                    });
                };
                match fs::read_dir(&next_dir) {
                    Ok(entries) => self.current_entries = Some(entries),
                    Err(e) => {
                        issues.push(ScanIssue {
                            path: next_dir,
                            kind: e.kind(),
                        });
                        continue;
                    }
                }
            }

            let Some(entries) = &mut self.current_entries else {
                continue;
            };

            while let Some(entry_res) = entries.next() {
                self.control.check()?;
                visited += 1;

                let entry = match entry_res {
                    Ok(entry) => Some(entry),
                    Err(e) => {
                        issues.push(ScanIssue {
                            path: PathBuf::new(),
                            kind: e.kind(),
                        });
                        None
                    }
                };

                if let Some(entry) = entry {
                    let path = entry.path();
                    match fs::symlink_metadata(&path) {
                        Ok(meta) => {
                            if meta.file_type().is_symlink() {
                                skipped += 1;
                            } else if meta.is_dir() {
                                self.dir_stack.push(path);
                            } else if meta.is_file() {
                                match file_entry(path, &self.roots, self.kind) {
                                    Some(file) => files.push(file),
                                    None => skipped += 1,
                                }
                            }
                        }
                        Err(e) => {
                            issues.push(ScanIssue {
                                path,
                                kind: e.kind(),
                            });
                        }
                    }
                }

                let reached_chunk = files.len() >= budget.chunk_files;
                let reached_entry_or_time = visited >= budget.max_entries || start.elapsed() >= budget.max_elapsed;
                if reached_chunk || reached_entry_or_time {
                    return Ok(ScanChunk {
                        files,
                        visited,
                        skipped,
                        issues,
                        yielded: true,
                        done: false,
                    });
                }
            }

            // Finished current directory
            self.current_entries = None;
        }
    }
}

/// 一个普通文件 → 入账条目；`None` 表示按本库语义跳过。
///
/// Video（「其他」库）不做文件名识别、也不按解析置信度丢弃：每个视频文件都是
/// 一个独立条目（标题 = 文件名主干，由登记层决定）。Movie/TV 沿用识别 + NFO
/// 身份；置信度低时不入账，不靠猜把 Unidentified 文件塞进 Library。
fn file_entry(path: PathBuf, roots: &[PathBuf], kind: MediaKind) -> Option<TransferredFile> {
    if !is_video_file(&path) {
        return None;
    }
    if kind == MediaKind::Video {
        return Some(TransferredFile {
            path,
            identified_release: None,
            nfo_identity: None,
        });
    }
    let (parsed, nfo_identity) = identify(&path, roots, kind);
    (parsed.confidence != Confidence::Low).then(|| TransferredFile {
        path,
        identified_release: Some(parsed),
        nfo_identity,
    })
}

/// `path` 所属的库根：取最长的前缀匹配，避免嵌套根互相抢占。
fn owning_root(roots: &[PathBuf], path: &Path) -> PathBuf {
    roots
        .iter()
        .filter(|root| path.starts_with(root))
        .max_by_key(|root| root.components().count())
        .cloned()
        .or_else(|| roots.first().cloned())
        .unwrap_or_default()
}

/// 文件名 → Release + 父目录补全 + NFO 身份，与递归 in-place 扫描保持同一语义。
fn identify(
    path: &Path,
    roots: &[PathBuf],
    kind: MediaKind,
) -> (Release, Option<crate::NfoIdentity>) {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let mut parsed = release::parse(name);
    let root = owning_root(roots, path);
    if let Some(parent) = path.parent() {
        super::enrich_from_ancestors(&mut parsed, parent, &root);
    }
    let nfo_identity = super::identify_local(&mut parsed, path, &root, kind);
    (parsed, nfo_identity)
}
