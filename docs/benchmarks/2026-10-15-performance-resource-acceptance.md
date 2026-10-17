# Performance and Resource Remediation Acceptance (2026-10-15)

## Scope and evidence boundary

This is the X1 acceptance record for the working tree inspected on 2026-10-17. No commit SHA was observed for this work, so every matrix commit value is **Not recorded (uncommitted)**. The integration tests use temp-backed stores and fakes; they do not establish production, device, slow-NAS, or ffmpeg behavior. Acceptance tests are characterization, not a claim that the named remediation is green.

## Acceptance matrix

Statuses are restricted to `open / partial / fixed-tested / design-gated / not-reproduced`.

| ID | Status | Commit | Public regression / observed evidence |
|---|---|---|---|
| DB-01 | partial | Not recorded (uncommitted) | Prior synthetic SQLite VM evidence only (audit §3); this run did not remeasure bundled SQLite. |
| DB-02 | partial | Not recorded (uncommitted) | Prior synthetic VM evidence only (audit §3); no new run. |
| DB-03 | fixed-tested | 8d899f4 | `reconciliation_budget` (5): blocking admission Busy->503 / Timeout->504 and missing/inaccessible/non-directory roots mark the result incomplete. |
| DB-04 | fixed-tested | 2cfe87d, 4fe9807 | `projection_dependency_invalidation` (6), `browse_change_recovery`, `browse_change_journal_bound`: durable journal, sentinel replay/pruning, root+owner invalidation, deleted-library eviction. |
| DB-05 | fixed-tested | 0f6bf32 | `data_retention` (9) + `data_retention_control` (7): opt-in, default disabled/dry-run, classified pruning with persisted cursor and reference protection; production enablement still needs explicit approval. |
| DB-06 | partial | Not recorded (uncommitted) | Prior synthetic VM counts only (audit §3); no release remeasurement. |
| DB-07 | design-gated | 0f6bf32 | `docs/benchmarks/2026-10-15-default-library-candidates.md` + `browse_reads` characterization/benchmark; derived index not implemented pending approval. |
| API-01 | fixed-tested | d8bb63a | `jellyfin_paged_metadata` (24) + `next_up_paging` (4) + `jellyfin_read_scale` (3): page selection before heavy metadata (`limit=1` over 200x10 builds 1 heavy snapshot), batched next-up, indexed expansion. |
| API-02 | fixed-tested | 8d899f4 | `reconciliation_budget` + `management` (545): filesystem work outside store locks, bounded reconciliation, documented status mapping. |
| API-03 | partial | 0b05de2, 0f6bf32 | `library_statistics_scale` (10 passed, 2 ignored): one aggregation per request, facts-derived sizes, stale semantics, roster coherence; A5-02 (journal freshness for same-count path changes) and the A5-03 barrier fixture remain open. |
| API-04 | fixed-tested | 63857d8, 4e21459 | `browse_page_reads` (12) + `browse_page_work_bounds` (9): page-scoped facts, head revalidation under the selection lock, restart hydration via dependency columns, real decode counters. |
| API-05 | fixed-tested | 8d899f4 | `up_next_local_first` (4): cached TV titles served locally, movies/videos never request seasons, bounded enrichment cooldown. |
| MEM-01 | partial | Not recorded (uncommitted) | Prior fake-fetcher concurrency evidence in audit §5; not rerun here. |
| MEM-02 | open | Not recorded (uncommitted) | No new capacity/eviction measurement. |
| MEM-03 | open | Not recorded (uncommitted) | R3 is in progress; worker-budget assertion is ignored pending public drain/recovery seam. |
| MEM-04 | open | Not recorded (uncommitted) | R4 not started; queue-capacity/drain assertion is ignored pending public seam. |
| MEM-05 | partial | Not recorded (uncommitted) | Existing web signal regression passed in `pnpm -C web test`; typecheck also passed. |

## Commands and results

| Exact command | Exit | Result |
|---|---:|---|
| `CARGO_TARGET_DIR=/tmp/crawler-media-transfer-flake-target cargo test -p api --test remediation_acceptance` | 124 | Initial run hung because the RAII gate release guard was dropped only after joining its waiter. |
| `CARGO_TARGET_DIR=/tmp/crawler-media-transfer-flake-target cargo test -p api --test remediation_acceptance -- --nocapture` | 0 | 3 passed, 1 ignored (R3/R4 pending); 0.43s test result. |
| `CARGO_TARGET_DIR=/tmp/crawler-media-transfer-flake-target cargo test -p api --release --test remediation_scale -- --ignored --nocapture` | 101, then 0 | First revision had a compile error after measuring loop edit; corrected release run passed 1 test. |
| `pnpm -C web test` | 0 | 791 passed, 0 failed. |
| `pnpm -C web typecheck` | 0 | `tsc --noEmit` completed successfully. |
| `pnpm -C web build` | Not run | Not executed. |

## Scale samples

Release-mode, local deterministic CPU checksum loop; three sizes, five rounds each. Median/p95: size 100: 0.000166/0.000208 ms; size 2,000: 0.001250/0.001584 ms; size 10,000: 0.006250/0.035375 ms. These are not media/database/API scale evidence and do not imply a remediation target passed. Unavailable plan metrics remain `None`/Not run: response bytes, VM steps, decoded rows, peak RSS, store lock wait/hold, threads, children, DB and WAL bytes.

## Not run

- **Device gate:** real NAS and ffmpeg, VidHub and Infuse (including Range 206/416, disconnect, and image presentation); no device environment or authorization was used.
- Release-scale bundled-DB/API matrix, 50k ledger, 100k pending recovery, cold/hot/NFO comparison, RSS/heap and lock timing.
- Slow NAS fault injection, real ffmpeg workloads, production database/traffic measurements, full workspace tests.
- Frontend build.
- Full recovery/crash/restart/drain assertions for R3/R4; public lifecycle and queue-stat seams are pending.

The three requested composite tests are characterized through currently available fake/temp public seams. They do not claim the missing route, device, task-drain, or scale behaviors passed.
