# Lock audit — 2026-10-10

> **历史状态（2026-10-13 整理）：** 已修复锁循环和当时失败基线的历史记录；“Remaining contention risks”不是当前仍存在的逐项结论，亦未全部对账。后续 API offlock 改动须依最新 review 单独核验；不把静态检查或旧 green 当成无死锁/真实压力证明。
>
> 原设计/ID/测试记录保留；旧执行、清理和提交命令不是当前执行授权，不应直接重跑。当前依据：[数据库/API review](<../audits/2026-10-13-database-api-review.md>) 与 [剩余计划](<../superpowers/plans/2026-10-13-database-api-followup.md>)；未覆盖的历史发现仍须独立对账。

## Scope and limits

Static inspection of production Mutex/RwLock acquisition sites across workspace crates, with focused call-chain review of nested locks, temporary guards in match/if let, callbacks, synchronous IO, and await boundaries. Tests and generated code were not treated as production findings. This is not a formal proof of freedom from all deadlocks.

## Fixed findings

- `crates/api/src/probe_manager.rs`: cancellation retained Store in an if-let scrutinee while acquiring seen; enqueue holds seen then Store. Snapshot job units before acquiring seen. The simultaneous enqueue/cancellation regression times out before the fix and completes after it.
- `crates/api/src/probe_manager.rs`: is_queued error fallback acquired seen while retaining Store. Evaluate the database result in a separate statement first.
- `crates/api/src/probe_manager/queue.rs`: missing-job/error match arms called finish(), which reacquires Store while the match scrutinee retains it. Release Store before matching. Missing-job regression times out before correction and completes afterwards.
- `crates/api/src/probe_manager/marker_jobs.rs`: list_ledger failure arm called fail_job(), which reacquires the same Store. Separate result acquisition from matching, applying the same proven correction.
- `crates/hooks/src/bus.rs`: emit called arbitrary Hook code while holding the registry mutex; registering or emitting through the same Bus self-deadlocked. Snapshot Arc<Hook> entries before callbacks. Public-seam reentrancy regression times out before correction and completes after it. No existing production reentrant Hook registration was identified.

## Remaining contention risks (not confirmed lock cycles)

- Library delete and raw ledger file delete retain global Store during filesystem metadata/unlink and strict root canonicalization (`crates/api/src/http/library_delete.rs`, `library_admin.rs`). A stalled filesystem can block authentication. Prefer a validated snapshot, physical deletion outside Store on a blocking worker, and short ledger update; preserve ownership and deletion-race safety when implementing.
- File watcher initialization retains Store during STRM reads; missing-path matching performs filesystem existence checks under Store. Grace tracker sweep holds its pending map while checking existence (`crates/api/src/fs_watcher.rs`).
- Transmission identity lookup retains identities during live_hash network RPC (`crates/downloader/src/transmission.rs`). No inverse session/identities path established.
- Image cache publication holds its own mutex across disk reads/sweeps/write/sync/rename (`crates/api/src/http/image_proxy/cache.rs`); upstream fetch occurs outside it. Publication/budget atomicity must be preserved.
- CDP transport intentionally serializes socket commands with bounded readiness waiting (`crates/indexer/src/cdp_page.rs`).
- Obscura child mutex spans kill/wait (`crates/api/src/obscura_manager.rs`).
- MemoryDownloader maps mutex spans fixture file copies (`crates/downloader/src/memory.rs`).
- Subscribe per-id guards serialize download/transfer lifecycle work. Do not remove them without preserving deletion safety.

## Checked patterns without a demonstrated cycle

- Jobs readers nest jobs → Store; inspected writers release Store before acquiring jobs. No reverse acquisition established.
- DynamicDownloader and RoutedDownloader cache checks release their temporary guards before reconnect/reacquire.
- Authentication releases Store before next.run().await.
- Poster service uses a Tokio per-media gate across await intentionally for single-flight, not a synchronous mutex guard.
- Indexer rate-limit mutex is released before sleep/fetch; catalog-cache network fetch occurs outside its connection mutex.
- Probe worker receiver Tokio mutex spans recv().await intentionally to serialize queue consumption; recovered refresh list is released before processing.

## Verification note

New probe concurrency and missing-job regressions, plus Hook reentrancy, were exercised red → green. Full API/workspace runs encounter the existing `newly_added_tv_episode_runs_voiceprint_when_chapter_detection_is_off` failure (expected enqueue 1, actual 0). A detached clean checkout of baseline 8a8ccd4 reproduces the same failure; it was not changed as part of this lock fix. Continuing API integration coverage additionally finds `restoring_one_episode_after_grace_requeues_probe` failing; the same detached baseline also reproduces it.
