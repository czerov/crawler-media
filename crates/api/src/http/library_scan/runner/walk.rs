//! chunk 遍历与处理：过滤归属 → 入账 → 失效变化事实 → 身份解析 → 探测入队 → 记断点。
//!
//! Video 与 Movie/TV 共用同一条 chunk 路径，只有两处按 `kind` 分叉：登记
//! （[`ScanContext::register_chunk`]）与身份解析（Video 无解析，只补封面）。
//!
//! 锁纪律（B3 §2）：文件版本事实在 Store 锁外采集，锁内只做短 DB 读写与内存判断。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use axum::http::StatusCode;
use axum::response::Response;

use crate::http::err;
use crate::http::library::enqueue_probes_for_paths;
use crate::management::ApiState;

use super::super::video::attach_video_artwork;
use super::checkpoint::{checkpoint_fresh, invalidate_changed_facts, revalidate_observed};
#[cfg(test)]
use super::hooks::test_metadata_hook;
use super::metadata::{resolve_scan_metadata, scan_media_for};
use super::outcome::presence_error;
use crate::http::library_scan::scope::ScanScope;

/// 一次扫描里单个根/目标目录的处理计数。
#[derive(Default, Clone, Copy)]
pub(super) struct ChunkTotals {
    pub(super) scanned: usize,
    pub(super) inserted: usize,
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

/// 一个根/目标目录遍历失败时带回的已提交前缀。
pub(super) struct ChunkFailure {
    pub(super) response: Response,
    pub(super) error: String,
    pub(super) root_report: library::ScanRootReport,
    pub(super) totals: ChunkTotals,
}

/// 有界、可取消、带 `scan_seen` 断点的扫描上下文。
pub(super) struct ScanContext<'a> {
    pub(super) state: &'a ApiState,
    pub(super) id: &'a str,
    pub(super) lease: &'a crate::library_lifecycle::LibraryWorkLease,
    pub(super) scan_scope: &'a ScanScope,
    pub(super) kind: domain::MediaKind,
    pub(super) scrape: bool,
    pub(super) op_id: Option<&'a str>,
}

impl ScanContext<'_> {
    /// 用 `ScanWalker` 走一个根或目标子目录，逐 chunk 处理并记断点。
    pub(super) fn walk_and_process(
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

    /// 处理一个 chunk：过滤归属 → 入账 → 失效变化文件事实 → 身份解析 → 收尾。
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
                self.register_chunk(&store, &filtered)
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
        let (affected, failed_invalidations) = match invalidate_changed_facts(self.state, &changed_in_scope) {
            Ok(res) => res,
            Err(e) => {
                let committed = ChunkTotals {
                    scanned,
                    inserted: inserted.len(),
                };
                return Err(ChunkError {
                    response: err(StatusCode::INTERNAL_SERVER_ERROR, "library.scan_invalidation", &e),
                    error: format!("library.scan_invalidation: {e}"),
                    totals: committed,
                });
            }
        };
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
        if self.kind == domain::MediaKind::Video {
            // Video 没有身份解析/刮削阶段：登记即完成，只补封面。
            //
            // 封面候选与探测候选**不同**（P2-1）：封面要覆盖本 chunk 过滤后真正
            // 处理过的**所有**文件，而不只是新增/变化的那几个。已入账且本轮未变化的
            // 文件不会被 `record_video_paths` 放进 `inserted`，`affected` 也只含当前
            // operation 断点标记为变化的路径；若沿用探测候选，普通重扫就再也不会
            // 尝试封面——镜像图片后开启无法回填、上次截帧失败无法重试、用户删掉的
            // 自动海报无法重生。断点跳过的文件不在 `filtered` 里，不会被重复处理。
            let artwork_paths: Vec<PathBuf> = filtered
                .iter()
                .map(|file| file.path.clone())
                .collect();
            attach_video_artwork(self.state, &artwork_paths, self.lease.control());
        } else {
            let scan_media = scan_media_for(self.state, &filtered);
            resolve_scan_metadata(self.state, self.id, self.lease, scan_media, &inserted, self.scrape)
                .map_err(|response| ChunkError {
                    response,
                    error: "library.scan_metadata".to_string(),
                    totals: committed,
                })?;
        }
        self.finish_chunk(filtered, stamps, &inserted, affected, failed_invalidations);
        Ok(committed)
    }

    /// 收尾一个已处理 chunk：复核版本 → 重挂窗口内变化的处理义务 → 探测入队 → 记断点。
    ///
    /// 版本观察发生在处理之前，文件可能在这段窗口里被改写（TOCTOU）。推进 `scan_seen`
    /// 断点与 durable 源版本之前必须复核：不能证明仍是观察时版本的路径不推进，改为
    /// 失效派生事实并重挂处理义务（P2-3），下一轮重新处理。未变化的路径行为不变。
    fn finish_chunk(
        &self,
        filtered: Vec<library::TransferredFile>,
        stamps: &HashMap<String, (i64, i64, Option<String>)>,
        inserted: &[PathBuf],
        affected: Vec<PathBuf>,
        failed_invalidations: HashSet<String>,
    ) {
        let (stable_files, changed_late) = match self.op_id {
            // 无 operation 归属 = 不写断点，也没有 durable 源版本要推进，无需复核。
            None => (filtered, Vec::new()),
            Some(_) => revalidate_observed(filtered, stamps),
        };
        // 探测候选 = 新增/恢复 + 断点判定变化 + 窗口内变化。
        let mut processed_paths = inserted.to_vec();
        processed_paths.extend(affected);
        processed_paths.extend(changed_late.iter().cloned());
        let mut uncheckpointed = failed_invalidations;
        if !changed_late.is_empty() {
            // 窗口内变化：派生事实可能来自已被替换的内容，必须失效并重挂处理义务。
            match invalidate_changed_facts(self.state, &changed_late) {
                Ok((_, failed)) => uncheckpointed.extend(failed),
                Err(error) => {
                    tracing::error!(
                        %error,
                        "窗口内变化路径的处理义务重挂失败，fail-closed: 这些路径不得推进断点与源版本"
                    );
                    uncheckpointed.extend(changed_late.iter().map(|path| path.display().to_string()));
                }
            }
        }
        enqueue_probes_for_paths(self.state, processed_paths);
        // P2-3: 排除失效/重挂失败与窗口内变化的路径，它们不得推进 durable 源版本与断点。
        let successful_files: Vec<library::TransferredFile> = stable_files
            .into_iter()
            .filter(|file| !uncheckpointed.contains(&file.path.display().to_string()))
            .collect();
        self.record_seen(&successful_files, stamps);
    }

    /// 按库类型登记 chunk：Video 走「每个视频文件一个 Media」，Movie/TV 走 Release
    /// 识别 + NFO 身份。两条路径都只登记，不做识别（识别在 `process_chunk` 后半段）。
    fn register_chunk(
        &self,
        store: &crate::Store,
        filtered: &[library::TransferredFile],
    ) -> Result<Vec<PathBuf>, String> {
        if self.kind == domain::MediaKind::Video {
            crate::watch_ledger::record_video_paths(
                store,
                filtered.iter().map(|file| file.path.clone()),
            )
        } else {
            crate::watch_ledger::record_paths(store, filtered.to_vec())
        }
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

fn publish_error(error: String) -> Response {
    if error.contains("Deleting") {
        return err(StatusCode::CONFLICT, "library.deleting", "Library 正在删除");
    }
    if error.contains("ScopeChanged") {
        return err(StatusCode::CONFLICT, "library.scope_changed", "Library 配置变化");
    }
    err(StatusCode::INTERNAL_SERVER_ERROR, "store.error", &error)
}
