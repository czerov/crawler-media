//! Iterative bounded directory walker for safe cancellable library scanning.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use domain::{Media, MediaKind};

use crate::watch::report::{ScanIssue, ScanRootStatus};
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
    roots: Vec<PathBuf>,
    dir_stack: Vec<PathBuf>,
    current_entries: Option<fs::ReadDir>,
    kind: MediaKind,
    control: crate::WorkControl,
}

impl ScanWalker {
    pub fn new(roots: Vec<PathBuf>, kind: MediaKind, control: crate::WorkControl) -> Self {
        let mut dir_stack = roots.clone();
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
                                if is_video_file(&path) {
                                    let name = path
                                        .file_name()
                                        .and_then(|n| n.to_str())
                                        .unwrap_or_default();
                                    let parsed = release::parse(name);
                                    files.push(TransferredFile {
                                        path,
                                        identified_release: Some(parsed),
                                        nfo_identity: None,
                                    });
                                } else {
                                    skipped += 1;
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
