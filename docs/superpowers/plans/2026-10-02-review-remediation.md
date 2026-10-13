# Review Remediation Implementation Plan

> **历史状态（2026-10-13 整理）：** 二次 review 的历史整改清单状态 UNRECONCILED；Task 1–7 须逐项核对，后续 API review 不构成文件安全、CDP 与部署全部关闭的证据。
>
> 本文保留原设计、发现/任务 ID 与当时证据；日期本身不表示过时。旧执行/提交/清理命令仅作历史记录，不应直接重跑；复选框不是当前实现清单。当前依据：[数据库/API review](<../../audits/2026-10-13-database-api-review.md>) 与 [剩余计划](<2026-10-13-database-api-followup.md>)，未覆盖的旧 ID 仍须单独对账。

> **规模规则更新：** 文中的 60 行硬限/整改目标是历史要求；当前 [AGENTS.md](<../../../AGENTS.md>) 为生产函数软限 60、测试软限 80、两者硬限 120。历史测试/行数记录不改写，规模合规不等于功能安全。

> **For agentic workers:** Execute task-by-task using `executing-plans`. Steps use checkbox syntax; independently scoped subagents may implement tasks with non-overlapping file ownership.

**Goal:** Resolve every finding in the second review at baseline a96f598 without compromising authentication, persisted configuration, or media files.

**Architecture:** Keep deletion bound to persisted Torrent identities and Downloader routes. Scope source-path deduplication to the destination Library. Maintain authenticated Playback file/unit identity and a persistent CDP connection; preserve public artwork routes.

**Tech Stack:** Rust 2024, Axum, rusqlite, tungstenite, React/TypeScript, Docker Compose.

## Global Constraints

- `.rs` files at most 800 lines; new/changed functions at most 60 lines, including test logic.
- No production unwrap/expect on IO or parse paths. Structured logs for failures and lifecycle events.
- No live third-party services or Chromium in tests; use temporary SQLite, injected Downloader and local CDP fixture.
- Static artwork stays public; Jellyfin streaming stays authenticated.
- Do not push or alter production media/configuration during verification.
- GitHub issue lookup failed with a connection reset; preserve local task/verification records and do not invent ticket status or close remote issues.

---

### Task 1: Exact Subscribe cleanup and destination-scoped deduplication
**Files:** Modify `crates/api/src/http/subscriptions/deletion.rs`, `crates/api/src/delivery.rs`; add focused tests under `crates/api/tests/management/`; update their module registry. Store pending APIs already expose active/imported state; add small scoped APIs only if required.
**Interfaces:** Preserve `TorrentRemovalTarget { torrent, downloader_id }` and `RoutedDownloader` interface.
- [ ] Add failing HTTP tests for imported pending and unrelated same-Media tasks, and missing pending (must never trigger name scan). Assert unrelated files/tasks survive.
- [ ] Remove `matching_downloads` fallback and include both persisted active/imported identities. Shared persisted identity must not delete a different Subscribe's still-referenced task.
- [ ] Add two-Library/same-source Transfer regression. Filter only source paths already imported to target Library; retain same-Library idempotence.
- [ ] Run `cargo test -p api --test management`, `cargo test -p store` if Store changes.

### Task 2: Site credential boundaries
**Files:** Modify `crates/api/src/http/sites.rs`; focused tests under `crates/api/tests/management/`; frontend Site adapter/editor only if response contract needs adapting.
**Interfaces:** Existing list/detail routes remain authenticated member-readable, with role-dependent URL fields.
- [ ] Add member/admin GET tests using RSS query secrets and proxy userinfo. Assert member responses do not contain secrets and admin editing preserves original URLs.
- [ ] Never return secret-bearing URL strings to members; permit administrator access needed by edit UI. Ensure absent/null credential fields preserve stored URLs on partial updates.
- [ ] Run scoped API tests and frontend tests/typecheck if touched.

### Task 3: Playback resume and exact unit identity
**Files:** Modify/split `crates/api/src/http/playback.rs`, create focused sibling helper modules; add HTTP tests under `crates/api/tests/management/`.
**Interfaces:** Preserve sessions/decide/progress response envelopes. Explicit zero starts at zero; unspecified start resumes saved position unless played.
- [ ] Test saved-position resume, explicit zero, completed replay, missing season/episode and file-id/unit mismatch.
- [ ] Choose exact requested unit without fallback; infer TV unit from selected file when omitted and reject inconsistent coordinates.
- [ ] Use saved watch position for absent start_ms and validate malformed/negative start inputs.
- [ ] Run `cargo test -p api --test management`.

### Task 4: CDP persistent authenticated render
**Files:** Modify `crates/indexer/src/cdp_page.rs`; add local HTTP/WebSocket fixture tests in `crates/indexer/tests/`.
**Interfaces:** Preserve `PageSession: Send + Sync` and `open_cdp_session`.
- [ ] Add failing local fixture asserting Cookie setup and navigation use the same WebSocket session.
- [ ] Store connection behind mutex, enable Network, preserve response/event correlation and bounded IO deadlines; clean up target and log failures.
- [ ] Wait for document load/JS readiness with bounded deadline instead of unconditional 500ms success; surface protocol/evaluation/navigation failures.
- [ ] Run `cargo test -p indexer` without Chromium.

### Task 5: Compose deployment validation
**Files:** Modify `docker-compose.yml`; use existing CI configuration or add deployment validation test/script if appropriate.
- [ ] Confirm `docker compose -f docker-compose.yml config --quiet` fails on baseline.
- [ ] Delete duplicate root volumes and misplaced service configuration; keep one named volume declaration.
- [ ] Run Compose config validation and `bash -n start-test.sh`; add repeatable regression gate.

### Task 6: Oversized Rust modules
**Files:** Split `crates/api/src/media_server_provider.rs`, `crates/api/tests/management/webapi.rs`, `crates/api/tests/jellyfin.rs`, `crates/subscribe/tests/run.rs` into responsibility-specific sibling modules preserving test discovery.
- [ ] Read complete modules and identify independent impl/test boundaries.
- [ ] Move code without behavior changes; register modules and preserve visibility.
- [ ] Use `wc -l` to verify all tracked/new Rust files <=800 lines; inspect new/changed functions <=60 lines.
- [ ] Run `cargo test -p api`, `cargo test -p subscribe`.

### Task 7: Independent review and final verification
- [ ] Review deletion safety, role boundaries, Playback identity, CDP timeout behavior and module moves independently.
- [ ] Run `cargo test --workspace`, `pnpm --prefix web test`, `pnpm --prefix web typecheck`, Compose config validation, startup syntax and scoped formatting checks.
- [ ] Inspect failures rather than suppress them; report ignored tests and tooling/environment limitations.
- [ ] Record exact commands/results and changed behavior. Commit only verified changes; no push without user request.

## Verification examples

```rust
assert_eq!(session["data"]["start_ms"], 600_000);
assert_eq!(missing_episode_status, axum::http::StatusCode::NOT_FOUND);
assert!(unrelated_download_path.exists());
assert!(!member_response.to_string().contains("rss-passkey-secret"));
```

```sh
cargo test --workspace
pnpm --prefix web test
pnpm --prefix web typecheck
docker compose -f docker-compose.yml config --quiet
bash -n start-test.sh
```

## Coverage self-review

Tasks 1–6 cover all eight reported categories; Task 7 provides independent review and full verification. No remote issue status is fabricated. Runtime regressions are checked through observable public seams, not source-text assertions.
