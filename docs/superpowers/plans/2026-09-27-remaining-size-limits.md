# 剩余函数与测试文件硬限收口 Implementation Plan

> **历史状态（2026-10-13 整理）：** 历史规模整改清单；不是当前超限清单，本次未重新测量或证明这些 Task 全部关闭。
>
> 本文保留原设计、发现/任务 ID 与当时证据；日期本身不表示过时。旧执行/提交/清理命令仅作历史记录，不应直接重跑；复选框不是当前实现清单。当前依据：[数据库/API review](<../../audits/2026-10-13-database-api-review.md>) 与 [剩余计划](<2026-10-13-database-api-followup.md>)，未覆盖的旧 ID 仍须单独对账。

> **规模规则更新：** 文中的 60 行硬限/整改目标是历史要求；当前 [AGENTS.md](<../../../AGENTS.md>) 为生产函数软限 60、测试软限 80、两者硬限 120。历史测试/行数记录不改写，规模合规不等于功能安全。

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把本轮仍超 `AGENTS.md` 硬限的 handler/测试拆到文件 ≤800 行、函数 ≤60 行，不改 HTTP 与数据库行为。

**Architecture:** 先拆生产 handler（`submit`、`map_task_rows`、`new_arc_with_admin_bootstrap`），再按 use-case 切开 `authz` / `cli` / `subscription_depth` / 超长 library 测试。每拆一块先 `--list` 对齐测试名再移动。

**Tech Stack:** Rust 2024、Axum oneshot、`cargo test -p api --test management -- --list`。

## Global Constraints

- `.rs` 文件硬限 800，函数硬限 60，测试逻辑不豁免。
- 不改 `/api/v1` 路由、Jellyfin 路由、schema。
- `store` 不依赖 `api`。词汇按 `CONTEXT.md`。
- 测试名保持不变，避免 CI 过滤失效。

## File map

| File | Action |
|---|---|
| `crates/api/src/http/downloaders/submission.rs` | `submit` 拆 `build_submit_torrent` |
| `crates/api/src/http/downloaders/task_listing.rs` | `map_task_rows` 拆 `task_row_json` + `subscribe_media_index` |
| `crates/api/src/management/state.rs` | `new_arc_with_admin_bootstrap` 拆 `apply_admin_bootstrap` |
| `crates/api/tests/management/authz.rs` | 会话测试迁 `authz_sessions.rs` |
| `crates/api/tests/management/cli.rs` | 下载器 CLI 迁 `cli_downloaders.rs` |
| `crates/api/tests/management/subscription_depth.rs` | 活动/清理/策略迁三个文件 |
| `crates/api/tests/management/library.rs` | 超长测试抽 helper 或迁 `library_cover.rs` |
| `crates/api/tests/management/subscribes.rs` | `patch_subscription_library_id_round_trips` 抽 assert helper |
| `docs/superpowers/plans/2026-09-27-extract-store-crate.md` | 附录记录 scrape compose 已迁回 api |

---

### Task 1: 生产函数压到 60 行

**Files:** Modify `crates/api/src/http/downloaders/submission.rs`, `task_listing.rs`, `crates/api/src/management/state.rs`.

**Interfaces:** 路由导出不变：`downloaders::{list_tasks,submit,...}`。`apply_admin_bootstrap(store, user_id, action: AdminBootstrapAction)` 供 `new_arc_with_admin_bootstrap` 调用。

- [ ] **Step 1 — 基线：** `cargo test -p api --test management downloaders delivery user_lifecycle`
- [ ] **Step 2 — submit：** 抽出

```rust
fn build_submit_torrent(body: SubmitInput) -> (domain::Torrent, release::Release, Vec<Value>) {
    let torrent = domain::Torrent { /* 现 submit 里的字段赋值 */ ... };
    let parsed = release::parse(&torrent.title);
    let units = match (parsed.season, parsed.episode) { /* 现 units match */ };
    (torrent, parsed, units)
}
```

`submit` 变为：解析 explicit_id → `build_submit_torrent` → `resolve_subscribe_and_check_owner` → `resolve_downloader_and_save_path` → spawn_blocking → `record_submitted_pending`。目标 `submit` ≤60。

- [ ] **Step 3 — map_task_rows：** 抽出

```rust
fn subscribe_media_index(store: &Store, kept: &[(SubscribeId, i32, PendingDownload)]) -> HashMap<String,(String,String)> { ... }

fn task_row_json(
    store: &Store,
    subscribe_id: &SubscribeId,
    score: i32,
    pending: &PendingDownload,
    snapshots: &HashMap<...>,
    reachable: &HashSet<...>,
    media: &(String,String),
) -> Value { /* 现 json! 块 */ }
```

匹配快照用已有 `downloader::torrent_matches_snapshot`。`map_task_rows` 只做循环。每个函数 ≤60。

- [ ] **Step 4 — state：** 把 seed/rotate/keep 写成 `fn apply_admin_bootstrap(store: &Store, user_id: UserId, action: AdminBootstrapAction) -> Result<(), StoreError>`。`new_arc_with_admin_bootstrap` 只做建目录、查用户、调 bootstrap、jobs/probe。函数 ≤60。
- [ ] **Step 5 — 验证提交：**

```bash
python3 - <<'PY'
# 断言 submit/map_task_rows/apply_admin_bootstrap/new_arc_with_admin_bootstrap 均 ≤60
PY
cargo test -p api --test management downloaders delivery
git add crates/api/src/http/downloaders crates/api/src/management/state.rs
git commit -m "refactor(api): keep bootstrap and downloader handlers under 60 lines"
```

### Task 2: 切开 authz / cli / subscription_depth

**Files:** Create `crates/api/tests/management/authz_sessions.rs`, `cli_downloaders.rs`, `subscription_depth_activity.rs`, `subscription_depth_cleanup.rs`, `subscription_policy.rs`; Modify originals + `main.rs`.

**Interfaces:** fixture 需要跨文件时标 `pub(super)`。`cli.rs` 的 `transport` / `RouterTransport` 改为 `pub(super)`。`subscription_depth.rs` 的 `authed_app` / `FakeCatalog` / `nexusphp` 同理。

- [ ] **Step 1 — 清单：** `cargo test -p api --test management -- --list > /tmp/mgmt-before.txt`
- [ ] **Step 2 — 移动（整段 `#[tokio::test]` + 专属 helper）：**
  - `authz_sessions.rs`：`logout_revokes_only_the_presented_session`、`password_change_revokes_existing_sessions`、`expired_session_tokens_are_rejected`（若在 authz.rs）。
  - `cli_downloaders.rs`：所有 `cli_downloaders_*`。
  - `subscription_depth_activity.rs`：`activities_report_search_rounds_and_imports`、`wanted_*`、`today_arrivals_*`。
  - `subscription_depth_cleanup.rs`：`season_cleanup_*`、`delete_subscription_can_cascade_torrent_removal`。
  - `subscription_policy.rs`：`rule_sets_accept_wash_target_atom_and_wash_target_prefers_it`、`patch_keep_old_versions_flips_subscribe`、`upgrade_ladder_replaces_even_when_score_is_lower`、`patch_selected_seasons_adjusts_tv_coverage`。
  - `main.rs` 增加对应 `mod`。
- [ ] **Step 3 — 对齐：** `cargo test -p api --test management -- --list > /tmp/mgmt-after.txt && diff -u /tmp/mgmt-before.txt /tmp/mgmt-after.txt` 应无测试名增减（模块路径前缀可变）。
- [ ] **Step 4 — 行数：** `wc -l crates/api/tests/management/{authz,cli,subscription_depth}*.rs` 每个 ≤800。若某新文件仍 >800，按下一个测试簇再切一刀，不要删断言。
- [ ] **Step 5 — 绿灯提交：** `cargo test -p api --test management && git add crates/api/tests/management && git commit -m "test(api): split remaining oversized management suites"`

### Task 3: library / subscribes 测试函数 ≤60

**Files:** Modify `crates/api/tests/management/library.rs`, `subscribes.rs`; Create `crates/api/tests/management/library_cover.rs` if cover test still >60 after helpers.

**Interfaces:** `setup_scan_library(app, tmp) -> String` 创建双根并写入 Matrix/Dune 文件后 scan，返回 lib id。`assert_scan_titles(app, id, expected: &[&str])`。`assert_subscription_library_patch(app, sub_id, lib2_id, lib_tv_id)` 覆盖 PATCH lib2 / 拒绝 tv / 清空 null。

- [ ] **Step 1 — library：** 把 `libraries_scan_walks_all_roots`、`libraries_delete_extra_but_protect_last_of_kind`、`libraries_set_default_and_reorder`、`libraries_scope_items_to_their_roots`、`library_cover_auto_fill_only_uses_fanart_and_supports_crud`、`libraries_support_default_filter_association_and_subscription_inheritance` 中的创建库/扫库抽成 `async fn create_library(...)` / `async fn scan_ok(...)`。封面 CRUD 若仍 >60，整段迁到 `library_cover.rs` 并 `mod library_cover`。每个测试与 helper ≤60。
- [ ] **Step 2 — subscribes：** `patch_subscription_library_id_round_trips` 调用已有 `create_movie_and_tv_libraries`，再调用新 `assert_subscription_library_patch`。主测试 ≤40。
- [ ] **Step 3 — 绿灯：** `cargo test -p api --test management library subscribes && wc -l crates/api/tests/management/library.rs crates/api/tests/management/subscribes.rs`
- [ ] **Step 4 — Commit：** `git add crates/api/tests/management && git commit -m "test(api): split long library and subscribe tests"`

### Task 4: 记录 store 抽离计划偏离

**Files:** Modify `docs/superpowers/plans/2026-09-27-extract-store-crate.md` 文末附录，不要改已勾选步骤冒充当时执行事实。

- [ ] **Step 1 — 附录：**

```markdown
## 事后修正（2026-09-27）

迁出时曾把 `scrape_config` compose 放进 `crates/store`。后续按边界改回：KV 仍由 store `get_setting`/`put_setting` 持久化，compose 在 `crates/api/src/scrape_config.rs` + `ScrapeStoreExt`。`crates/store` 不再导出 `ScrapeConfig`。
```

- [ ] **Step 2 — Commit：** `git add docs/superpowers/plans/2026-09-27-extract-store-crate.md && git commit -m "docs: record scrape compose remaining in api"`

**独立验收顺序：** Task 1 → Task 2 → Task 3 → Task 4。最后 `cargo test --workspace` 与 `cd web && pnpm exec tsc --noEmit && pnpm test`。`git status --short` 应为空。
