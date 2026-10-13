# Review Follow-up Implementation Plan

> **历史状态（2026-10-13 整理）：** 正文 Execution Notes/Final Gates 记录 Task 1–6 的实现和当时测试；保留尚未勾选的收尾与规模边界，不据此重跑已完成任务或宣称全仓无缺陷。
>
> 本文保留原设计、发现/任务 ID 与当时证据；日期本身不表示过时。旧执行/提交/清理命令仅作历史记录，不应直接重跑；复选框不是当前实现清单。当前依据：[数据库/API review](<../../audits/2026-10-13-database-api-review.md>) 与 [剩余计划](<2026-10-13-database-api-followup.md>)，未覆盖的旧 ID 仍须单独对账。

> **For agentic workers:** Use the available `executing-plans` skill to implement task-by-task. Unavailable superpowers skills must not be invoked. Steps use checkbox syntax.

**Goal:** Close the two validation blockers and six concrete functional findings from the a466da2 → 95498e3 + working-tree review.

**Architecture:** Preserve the existing Downloader RPC and domain DTO contracts; isolate Filter edit serialization as a testable seam. Resolve file ownership fail-closed with canonical existing ancestors and no default Library fallback. Share Job run identity between writes and reads, and scope Media aliases by kind.

**Tech Stack:** Rust/Axum/SQLite; React/TypeScript/Node tests.

## Global Constraints

- User explicitly authorized continuing on main, preserving all 22 existing modified files. No reset, stash, wholesale overwrite, push, issue closure or mixed commit.
- GitHub issue discovery failed with connection reset; work is local, sequential review-finding slices. Record this deviation; do not invent ticket IDs or claim repository issue gates passed.
- No live external services or Chromium in tests; localhost fixtures and tempdirs only.
- Rust file ceiling 800 lines; function ceiling 120. lib.rs module tree/reexports only: extract Filter implementations to sibling modules when touching them.
- Errors must preserve IO context and log structured failures. Static artwork authentication remains unchanged.
- For each Rust slice run its crate tests --locked --offline; final workspace test. Frontend: executed behavior tests and typecheck, not just source pattern checks.
- Previous plan 2026-10-03-page-api-module-remediation.md is context, not authorization to repeat completed T01–T12.

## Task 1: Restore Transmission task snapshots
**Files:** modify crates/downloader/src/transmission.rs; tests crates/downloader/tests/transmission.rs.
**Interfaces:** Downloader::task_snapshots returns Result<Vec<TaskSnapshot>, DownloaderError>; fields conform to crates/downloader/src/lib.rs; RPC uses existing rpc("torrent-get", arguments).
- [ ] Add localhost RPC test returning tagged and foreign tasks; assert tag, u64 size, hash, progress, speeds, stopped/downloading/seeding state. Example assertions: `assert_eq!(rows[0].tag, downloader::TASK_TAG); assert_eq!(rows[1].tag, "");`.
- [ ] Run `cargo test -p downloader --locked --offline --test transmission`; record current compile failure rather than mislabeling it a behavior red.
- [ ] Replace nonexistent torrent_get with existing RPC, validate torrents array, map size via `.as_u64().unwrap_or(0)`, derive tag only from supported ownership labels. Use `TaskSnapshot { tag, size_bytes, .. }`, never fabricated universal ownership.
- [ ] Run full downloader crate; record test output before next slice.

## Task 2: Filter edits preserve actual changes
**Files:** web/components/rule-sets-panel.tsx; web/lib/api/subscriptions.ts; focused new pure helper/test under web/lib and web/test; web/lib/subscription-ui.ts if needed for accurate projection.
**Interfaces:** updateRuleSet(id,name,spec,overrideAtoms?) remains current wrapper; pure edit helper consumes original spec/atoms and edited spec, returns atoms respecting unchanged priority/exclude.
- [ ] Add executed tests for rename preserving original atoms, resolution 1080p→2160p, seeders 1→10, codec edits and opaque atoms preservation. Example: `assert.equal(next.find(a=>a.kind==='resolution').value,'2160p')`.
- [ ] Run focused Node test and typecheck; record missing symbol and no-op failure.
- [ ] Remove component call to private undefined specToAtoms. Compare actual edited fields, not kind lists; preserve unchanged atoms and deliberately replace changed dimensions. Ensure production component/wrapper consumes helper.
- [ ] Run focused tests and frontend typecheck.

## Task 3: Fail-closed path ownership and source reconciliation
**Files:** crates/store/src/libraries.rs (or sibling path module); crates/store/tests focused path ownership test; crates/api/src/http/library_organize.rs; crates/api/src/http/library_housekeeping.rs; new API management regression module and register in main.rs.
**Interfaces:** library_for_path_strict(path,kind) returns Result<Option<Library>,StoreError>, never default fallback.
- [ ] Add tempdir cases: existing inside/outside, nested owners, ambiguous roots, nonexisting descendants, `new/../../outside/file.mkv`, symlink ancestors and canonicalization failures. Assert outside ownership None.
- [ ] Run store focused test to demonstrate path escape red.
- [ ] Canonicalize nearest existing ancestor, resolve remaining components safely (including ParentDir), fail closed on IO errors, compare canonical roots and canonical depth. Validate source ledger before directory creation. Use strict source lookup for reconciliations.
- [ ] Add HTTP organize test asserting outside destination never created, source file and ledger unchanged; reconciliation outside-source rejected. Preserve collision/rollback and log concrete rename error via `if let Err(error) = ...`.
- [ ] Run store and api crate tests.

## Task 4: Exclude-only Filter admission
**Files:** crates/filter/src/lib.rs and new sibling implementation module(s); crates/filter/tests/admit.rs.
**Interfaces:** public admit/admit_scored exports unchanged; only blacklist/metadata atoms should not require a positive admission match.
- [ ] Add HR-only filter test with HR and non-HR candidates: `assert_eq!(outcome.admitted.len(),1); assert!(!outcome.admitted[0].torrent.hr);` and empty/positive-filter regressions.
- [ ] Run focused test red.
- [ ] Give surviving exclude-only filters default positive admission score, preserving existing positive include matching and wash-target non-admission semantics. Move implementation out of lib.rs without changing API.
- [ ] Run filter crate tests.

## Task 5: Job run-specific dismissals
**Files:** web/lib/job-attention.ts; web/components/job-center.tsx; web/components/task-center-view.tsx; web/test focused dismissal tests.
**Interfaces:** markJobDismissed(jobId,finishedAt?) and isDismissed(job) must share identical key; all production writers supply job.last_finished_at.
- [ ] Test production sequence with timestamp: mark current run→dismissed true; next timestamp→false; undismiss→false; no timestamp legacy behavior preserved.
- [ ] Run focused Node test red using current caller-shaped invocation.
- [ ] Update individual/bulk writers to pass finished time; share key construction if useful. Preserve persisted legacy semantics intentionally, not globally hide future runs.
- [ ] Run frontend tests/typecheck.

## Task 6: Kind-scoped canonical Library links
**Files:** crates/api/src/http/media.rs; focused API management test and module registration.
**Interfaces:** get_media_by_alias_kind("tmdb_id",id,Some(media.kind)) resolves canonical Media; detail Library links retain DTO shape.
- [ ] Seed movie and TV with identical TMDB numeric ID and independent ledger rows. GET TV detail must link only TV; reverse case also tested. Use injected Catalog, no TMDB network.
- [ ] Run focused API test red.
- [ ] Replace unscoped lookup with existing kind-scoped API, retaining incoming-id fallback.
- [ ] Run api crate tests.

## Execution Notes (2026-10-05)

- Task 1 completed: compiler red captured; complete downloader crate green (45 passed, 1 ignored). RPC ownership derives from actual labels, not universal our_tag.
- Task 2 implemented: exported serializer, editor initial draft snapshot avoids codec-family/default normalization being mistaken for edits; unchanged same-kind atoms retain original priority/exclude; executable helper regressions cover rename, actual value edits, opaque atoms, codec-family expansion and excluded siblings. Initial typecheck failure was captured in the review; no claim of an isolated pre-fix behavior red for every helper test.
- Task 3 implemented and Store red/green captured. HTTP escape, external source reconciliation and unregistered-source/no-mkdir tests added. Rename logs retain IO errors. This closes deterministic path traversal, not a claim of protection against a privileged external process concurrently changing filesystem symlinks.
- Task 4 completed with HR-only behavioral red/green; admission moved out of lib.rs.
- Task 5 implemented with all individual/bulk production writers supplying finished time; current/next-run and legacy tests added.
- Task 6 implemented with kind-scoped alias and real HTTP regressions in both directions and insertion orders.
- Additional preexisting working-tree compile omissions uncovered sequentially: Codec missing from Store JSON mapping, nonexistent TmdbMetadata name (correct type media::ItemMeta), gallery test nonexistent list_media (use ledger IDs). Corrected with Codec reopen regression; removed irrelevant sleep from title-sort test.
- Independent review found codec-family rename, same-kind sibling metadata, and mkdir-before-ledger gaps; all patched before final gates.
- First full workspace run passed; rerunning after final review fixes. Frontend final gates passed. Rust warnings and historical oversized functions outside these changes remain follow-up, not silently claimed resolved.

## Final Gates
- [x] Run related crate suite and workspace suite: both exit 0; related suite 765 passed/1 ignored, workspace 886 passed/1 ignored. Final added Codec test separately passed (filter now 15 tests); final strict-path HTTP module 3 passed.
- [x] Run frontend typecheck and `node --experimental-strip-types --test web/test/*.test.mjs`: 740 passed, 0 failed; typecheck exit 0.
- [ ] Run git diff --check and review changes against the original dirty baseline. Record test counts, unresolved failures, and any remaining historical function-size violations explicitly.
- [ ] Report completion only for verified targets; leave changes uncommitted and unpushed unless separately authorized under issue workflow.
