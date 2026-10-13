# Full-project Review Remediation Implementation Plan

> **历史状态（2026-10-13 整理）：** PIPE/ROOT/ACCESS/I 历史整改索引；后续 follow-up 有局部实现记录，但全部 ID 状态仍需逐项核对（UNRECONCILED）。最新数据库/API review 不自动关闭该全项目清单。
>
> 本文保留原设计、发现/任务 ID 与当时证据；日期本身不表示过时。旧执行/提交/清理命令仅作历史记录，不应直接重跑；复选框不是当前实现清单。当前依据：[数据库/API review](<../../audits/2026-10-13-database-api-review.md>) 与 [剩余计划](<2026-10-13-database-api-followup.md>)，未覆盖的旧 ID 仍须单独对账。

> **For agentic workers:** Execute using executing-plans, test-first. Independent slices use ordinary subagents, not Agent Teams. Parent owns integration, review, verification and commits.

**Goal:** Fix the confirmed 2026-10-10 audit defects on the user's current branch without including unrelated changes.

**Architecture:** Library is authoritative for owned facts and quality; replacement persistence precedes deletion; identity/shared facts outlive individual files. Protocol adapters share state rules, critical failures propagate, and configuration reflects usable runtime capabilities.

**Tech Stack:** Rust 2024, SQLite/rusqlite, Axum, fake HTTP/Downloader/CDP traits, React/TypeScript.

## Global Constraints

- Current branch is `main`; user explicitly requested it. Do not switch branches/worktrees.
- Preserve initial user modifications: `crates/api/src/compact_time.rs`, `crates/api/src/http/library_scan.rs`, `crates/api/src/probe_manager/markers.rs`, `crates/api/tests/management/strm_directory_deletion.rs`, `start-test.sh`, plus later concurrent changes. Initial diff is saved outside the repository for comparison.
- No GitHub issue operations. CONTEXT vocabulary is mandatory. Artwork remains public; video streams remain authenticated and User-scoped.
- Tests first at public seams; temporary databases/files and fakes only, no live network/services/Chromium.
- Rust files <=800 lines; production/test functions <=120 lines. Split by responsibility; no blanket formatting of user files or another slice.
- ACCESS-06 keeps Web's current end-playback contract, not device-credential revocation. Explicitly reject unsupported revocation instead of inventing a Web credential system.
- Unknown existing quality cannot authorize destructive replacement. Probe overlays must not erase Release source.
- File deletion does not implicitly authorize automatic redownloading. Retain referenced Media and handle facts according to explicit deletion semantics.
- Structured logs on critical failure/lifecycle paths. Commit only requested changes after crate/workspace/frontend gates.

## Ownership and Interfaces

Parent owns PIPE-01…05, ROOT-01…05, I01: Subscribe/Marker crates, Store subscribe/quality helpers, API Subscribe/Transfer/Watcher/marker resolver/runtime Downloader, targeted Video scan hunks and regression tests.

Playback slice owns ACCESS-01…09: Store Playback modules and `schema.rs`, media-server event/auth adapters, API Playback handlers/providers, Web Playback DTO/heartbeat, regression tests. Coordinate `store/src/lib.rs` re-exports with parent. Schema owner also adds nullable `ledger.release_quality TEXT`, Library schema version 6; parent persists serialized `domain::Release` through `save_subscribe_facts`, restores it and overlays ledger probe dimensions.

Integration slice owns I02/I03/I04: Downloader path map, Media cache/endpoints, Hooks Login/Check-in, relevant API Site/Check-in adapters and tests. Do not modify Indexer Browser/settings or Playback.

Browser slice owns I05/I06: Indexer Browser/routing, API Browser construction/settings, Browser configuration UI/docs/tests. Do not modify runtime Downloader, schema, Playback, or parent files.

Concurrent tests may wait on Cargo's normal locks. Each owner records red/green commands and collects its background jobs; no duplicate running suites.

## Task 1: Safe replacement and durable quality — PIPE-01/02

**Files:** Store Subscribe/new quality helper, schema migration (schema owner), Subscribe chooser, API legacy run/worker Transfer, new `crates/api/tests/review_remediation_pipeline.rs`.

- [ ] Adapt audit cases `source_quality_survives_persistence_for_wash_cut` and `legacy_wash_cut_keeps_old_file_if_replacement_ledger_write_fails`; add reopen and unknown-source coverage.
- [ ] Run `cargo test -p api --test review_remediation_pipeline`; record source-loss and old-file deletion failures before fixes.
- [ ] Persist owned Release quality before facts, restore source while overlaying probe resolution/codec/HDR. Refuse source-based destructive upgrades whose old source is unknown.
- [ ] Set legacy collector `preserve_removed: true`; persist new ledger and facts before old-file/old-ledger cleanup. Critical write failure keeps both old file and record.
- [ ] Run targeted tests green, including both HTTP and worker injected-write failures.

## Task 2: Pending, shared owned facts and pack identity — PIPE-03/04/05

**Files:** Store owned-facts helpers, API legacy/worker search and Transfer context, Subscribe file identity, pipeline and Subscribe public regression tests.

- [ ] Add correct behavior cases `legacy_second_run_does_not_turn_pending_into_owned_fact`, `existing_library_prevents_redundant_download_for_new_subscribe`, `season_pack_accepts_bare_sxxexx_episode_names`, pending cancellation and target-Library isolation.
- [ ] Run targeted API/Subscribe suites red.
- [ ] Keep temporary pending coverage out of persisted owned/completion facts. Reconcile null-path fake-owned records without changing explicit user-deletion policy.
- [ ] Read matching Media/coverage ledger facts from the selected Library before submission, preserving actual quality and ordinary-fill/Wash-cut semantics.
- [ ] Accept reliable pure SxxExx basenames only under confirmed Torrent identity/coverage; keep genuine title/year/coverage conflict rejection.
- [ ] Verify cancelled pending remains eligible, owned content is not resubmitted, and foreign files stay rejected.

## Task 3: File lifecycle and marker trust — ROOT-01…05

**Files:** API Watcher/new cleanup helper, targeted `http/library_scan.rs`, marker resolver, Marker adaptive verifier, new API/Marker regression suites.

- [ ] Adapt audit watcher-reference, multi-version locked-marker, realtime Video, repeated-empty chapter-read and template permutation tests.
- [ ] Run tests red.
- [ ] Preserve Media still referenced by Subscribe; treat failed reference/remaining queries conservatively. Delete file-derived caches, not markers still needed by another version.
- [ ] Route realtime Video through `record_video_paths`, matching manual scan without Movie/TV recognition.
- [ ] Remove unverified neighbor read fallback; preserve own verified-marker chapter reconstruction.
- [ ] Evaluate all valid template outcomes; equally supported incompatible intervals remain unresolved regardless of model order, including full-window conflict.
- [ ] Verify targeted tests and existing STRM/fingerprint behavior; keep user STRM test changes out of commit.

## Task 4: Playback state and critical writes — ACCESS-01/02/03/07/08/09

**Files:** Store Playback, API heartbeat/history/metrics/provider, Web Playback DTO, new `crates/api/tests/review_remediation_playback.rs` and Store tests.

- [ ] Reverse audit defect assertions: favorites survive clear-history; Jellyfin completion persists watched; first canonical write preserves legacy state; write failures return error; UI QoE shape persists; limit=0 does not panic.
- [ ] Run tests red.
- [ ] Reset viewing fields, not favorite/track facts; ensure preserved rows do not become false recent-history entries.
- [ ] Migrate before both Jellyfin canonical write paths, propagate failure, and reuse Web completion thresholds via a small helper.
- [ ] Propagate migration/unit/session persistence failures instead of successful in-memory snapshots; test each critical failure path.
- [ ] Align telemetry identity DTO while retaining best-effort behavior and logging invalid payloads. Validate pagination lower bounds and eliminate input-driven expect.
- [ ] Run targeted and existing Playback tests green.

## Task 5: Stable devices and supported revocation — ACCESS-04/05/06

**Files:** media-server event/auth adapters, Store session/log/schema/device modules, API activity/revoke handler, Playback/device tests.

- [ ] Add standard-Authorization two-device isolation, stopped-device true-ID revocation, legacy log non-revocability and unsupported Web revoke rejection tests.
- [ ] Run tests red.
- [ ] Reuse authenticated DeviceId/metadata for events and preserve stable ID in logs. Mark only real credential-backed identities revocable.
- [ ] Reject unsupported Web credential revocation explicitly; keep ending playback and other valid device tokens working.
- [ ] Verify revoked Jellyfin devices denied, unrelated devices unaffected, and static images still public.

## Task 6: Production integration truthfulness — I01…04

**Files:** Parent's runtime/dynamic Downloader; integration slice's path map, Media cache/endpoints, Hooks and Site/Check-in adapters, tests.

- [ ] Add no/default-incomplete Downloader rejection, path-component boundaries, malformed-success cache recovery, and login-form/unknown-response rejection tests.
- [ ] Run targeted API/Downloader/Media/Hooks suites red.
- [ ] Replace production Memory fallback with explicit configuration failure; retain explicitly injected fake Downloaders.
- [ ] Use component-aware mappings with configured precedence, exact roots and trailing-separator support.
- [ ] Validate endpoint response contracts before cache replacement; preserve valid stale on decode/transport failure and retry malformed fresh entries.
- [ ] Validate known Login/Check-in success or already-completed outcomes; forms/captcha/unknown pages cannot be business success.
- [ ] Run targeted tests green and each touched crate's complete tests.

## Task 7: Browser capabilities and actual routing — I05/06

**Files:** Indexer Browser/routing, API `main.rs` and Browser settings, relevant UI/docs, injected runtime tests.

- [ ] Add injected-session tests for supported rendering, capability validation, Obscura on/off route choice, dynamic save and startup consistency; no real Chromium/download in tests.
- [ ] Run tests red.
- [ ] Inspect existing managed launcher support. Wire verified supported runtime, or honestly advertise external-CDP-only mode and reject unavailable managed capability instead of treating an empty directory as Chromium.
- [ ] Effective Obscura settings must reach actual Fetcher choice on startup and after save; Site CDP and disabled settings have deterministic behavior.
- [ ] Reject incomplete unsupported combinations without misleading usable status; update affected capability docs/UI and run targeted tests green.

## Task 8: Gates, audit closure and current-branch commit

- [ ] Run `cargo test -p <crate>` for every touched crate and collect exact results. Run `cargo test --workspace` after integration and relevant Web typecheck/tests.
- [ ] Check whitespace, scoped formatting, Rust file/function hard limits. No whole-workspace formatting or wholesale staging of user files.
- [ ] Independently review deletion safety, old-data compatibility, all 25 IDs and evidence; address findings test-first.
- [ ] Record fixed/narrowed behavior, tests and capability limitations in a remediation outcome document. Do not describe unsupported managed capability as implemented.
- [ ] Stage only requested edits; shared pre-dirty files use only remediation hunks. Compare remaining diff with baseline and preserve later concurrent edits.
- [ ] Commit current branch only when green, then report commit, user-visible changes, exact verification and limitations.
