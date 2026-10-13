# Store 边界与硬性代码规模收口 Implementation Plan

> **历史状态（2026-10-13 整理）：** Store compose 边界与模块拆分历史整改；原迁移/测试步骤不代表当前文件布局或全仓规模已合规。
>
> 本文保留原设计、发现/任务 ID 与当时证据；日期本身不表示过时。旧执行/提交/清理命令仅作历史记录，不应直接重跑；复选框不是当前实现清单。当前依据：[数据库/API review](<../../audits/2026-10-13-database-api-review.md>) 与 [剩余计划](<2026-10-13-database-api-followup.md>)，未覆盖的旧 ID 仍须单独对账。

> **规模规则更新：** 文中的 60 行硬限/整改目标是历史要求；当前 [AGENTS.md](<../../../AGENTS.md>) 为生产函数软限 60、测试软限 80、两者硬限 120。历史测试/行数记录不改写，规模合规不等于功能安全。

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修复已合并的 Store 抽离计划偏离，并把本轮触及而仍违反仓库硬限的 Rust 源文件/测试逻辑拆到限制以内，不改变 HTTP 与数据库行为。

**Architecture:** Store 只拥有 SQLite 行/KV 读写，刮削配置组合属于 API 业务层；用 `ScrapeStoreExt` 扩展 trait 保持应用调用接口。随后按测试 use-case 将长文件拆入兄弟模块，源码/测试调用点和对外 API 保持不变。两个改变各自独立提交、独立跑 crate 测试。

**Tech Stack:** Rust 2024、rusqlite、Axum、tower oneshot；`cargo test -p store` 和 `cargo test -p api`。不动 Jellyfin、文件持久化布局或 `/api/v1` 路由。

## Global Constraints

- 仓库 `AGENTS.md`：`.rs` 文件硬限 **800 行**，函数/方法硬限 **60 行**，测试逻辑不豁免；已触及过限文件需拆分或在对应问题登记明确后续。
- 领域词按 `CONTEXT.md`。`domain` 纯类型；`store` 不依赖 `api`；不要把 HTTP DTO 放入 `store`。
- 不改变 `api::Store`、`api::store::*` 的导出、`Store::get_setting` / `put_setting` 的数据库行为。
- 测试先行、每 Task 测绿再 commit。本计划不改数据 schema、不引入新数据库迁移。

## File map

| File | Action |
|---|---|
| `crates/store/src/scrape_config.rs`, `scrape.rs`, `lib.rs` | 移除 compose 类型/方法（KV 本身继续由 `open.rs` 提供），保留 ABI 所需 re-export 迁移策略 |
| `crates/api/src/scrape_config.rs`, `scrape_store.rs`, `lib.rs` | 恢复配置类型/compose；本地 `ScrapeStoreExt` 实现，公开 trait 给调用方 |
| `crates/api/src/{worker.rs,claim.rs,watch_intake.rs,watch_scrape.rs,scrape_metadata.rs,poster_fetch.rs,directory.rs}` | 导入 `ScrapeStoreExt` |
| `crates/api/src/http/{directory,library_chapters,library_scan,library_organize,routing_preview,scrape_settings}.rs`, `crates/api/src/{probe_manager/{markers,probe}.rs,worker/finish_search.rs,management/subscribes/run.rs}` | 同上；编译器验证遗漏调用点 |
| `crates/api/src/http/downloaders/{tasks,instances}.rs` | 按 list_tasks/submit 用例继续拆，handler 均 ≤60 行 |
| `crates/api/tests/management/{catalog,library,authz,cli,subscription_depth,subscribes}.rs` + `main.rs` | 按相关测试簇迁到新 `.rs` 测试模块，所有模块 ≤800、测试函数 ≤60 |
| `docs/superpowers/plans/2026-09-27-extract-store-crate.md` | 记录实际偏离及本轮修正，不将错误的旧步骤冒充执行事实 |

---

### Task 1: 恢复 scrape compose 在 api 的领域边界

**Files:** 上述 `scrape_config`、`scrape_store`、`store` 模块和引用 `get_scrape_config`、`save_scrape_config`、`naming_pattern`、`set_legacy_naming` 的 API 文件。**Interfaces:** `ScrapeStoreExt` 为 `Store` 实现原四个方法；所有调用方需 `use crate::scrape_store::ScrapeStoreExt;`。`crates/store` 只提供 `get_setting`/`put_setting` 原 API。

- [ ] **Step 1 — 基线测试：** `cargo test -p api --test management scrape_settings directory`、`cargo test -p store` 均 PASS；新增 `crates/api/tests/management/scrape_settings.rs` 断言空配置、保存/重新打开后的命名组合结果仍一致（现有测试可直接加强）；编译期约束验证 `crates/store/src/` 无 `ScrapeConfig` / `compose_config` 引用。
- [ ] **Step 2 — 实施：** 把 `crates/store/src/scrape_config.rs` 完整内容移回 `crates/api/src/scrape_config.rs`，其中 `KEY = crate::settings_keys::METADATA_SCRAPE`；将 `crates/store/src/scrape.rs` 四方法移到新 `crates/api/src/scrape_store.rs`，定义 `pub(crate) trait ScrapeStoreExt` 及 `impl ScrapeStoreExt for crate::Store`，方法签名与返回值保持原样。`crate::Store` 已来自外部 crate，**不可写 `impl Store` 固有方法**。`store/lib.rs` 删除 `mod scrape; pub mod scrape_config;`。
- [ ] **Step 3 — 导入：** 每个使用上述方法的 `api` 模块增加 `use crate::scrape_store::ScrapeStoreExt;`。如某调用方是集成测试中直接调用 `api::Store::naming_pattern`，从 `api::ScrapeStoreExt` 公开导入 trait；检查 `api::scrape_config` 旧可见性不下降。`cargo check -p api` 确认没有 E0599/E0116。
- [ ] **Step 4 — 验证：** `cargo test -p store && cargo test -p api --test management scrape_settings directory && cargo test -p api --test jellyfin`；确认 `crates/store/src/` 不再有对 `api` / HTTP / scrape compose 的依赖。
- [ ] **Step 5 — Commit：** `git add crates/store/src crates/api/src crates/api/tests/management/scrape_settings.rs docs/superpowers/plans/2026-09-27-extract-store-crate.md && git commit -m "refactor(store): keep scrape composition in api"`。

### Task 2: 拆 downloader handler 函数硬限，不改路由

**Files:** Modify `crates/api/src/http/downloaders/tasks.rs`, `instances.rs`，Create `crates/api/src/http/downloaders/{task_listing,submission}.rs`，Modify `crates/api/src/http/downloaders/mod.rs`; Test `crates/api/tests/management/{downloaders,delivery}.rs`。

**Interfaces:** `mod.rs` 继续提供 `downloaders::{list_tasks,submit,create_downloader,patch_downloader,delete_downloader,get_downloader,get_limits,list_downloaders,pause_task,put_target_pref,remove_task,replace_task,resume_task,set_limits,target_prefs,verify_downloader}`；GET/POST JSON 与 pending 清理一致。

- [ ] **Step 1 — 基线：** 在 `downloaders.rs` 集成测试中确认「客户端不可达时 pending 不删」「可达且超过宽限期才删」「submit 失败不记 pending」「带订阅 submit 成功记 grab」；`cargo test -p api --test management downloaders delivery`。
- [ ] **Step 2 — 提取 list：** 把 `list_tasks` 移到 `task_listing.rs`，从现有函数分出 `prune_orphan_pending`、`load_task_snapshots`、`prune_absent_pending`、`map_task_rows`。每函数 ≤60；快照读取失败仍不能触发 pending 删除。`tasks.rs` 的删除/暂停/换源 handler 保持不变，`mod.rs` 再导出新 `list_tasks`。
- [ ] **Step 3 — 提取 submit：** 把 `submit` 移到 `submission.rs`，拆出 `resolve_subscribe_and_check_owner`、`validate_candidate`、`choose_downloader_and_save_path`、`record_submitted_pending`，每函数 ≤60；不改变订阅身份校验、save_path remap、投递顺序。`mod.rs` 再导出新 `submit`。
- [ ] **Step 4 — 检查其它变更函数：** `instances.rs::patch_downloader` 现约 61 行，抽 `apply_downloader_patch`；命令 `awk '/^pub\(crate\) async fn patch_downloader/,/^}$/' crates/api/src/http/downloaders/instances.rs | wc -l` 应 ≤60。其它所有刚移动或新增函数按同法检查。
- [ ] **Step 5 — 验证与提交：** `cargo test -p api --test management downloaders delivery && cargo test -p api && wc -l crates/api/src/http/downloaders/*.rs`，每文件 ≤800；`git add crates/api/src/http/downloaders && git commit -m "refactor(api): split downloader use cases under function limits"`。

### Task 3: 分割已触及的超限测试文件

**Files:** `crates/api/tests/management/{catalog,library,authz,cli,subscription_depth,subscribes}.rs`, `crates/api/tests/management/main.rs`; create sibling test modules named `catalog_wall.rs`, `library_configuration.rs`, `authz_sessions.rs`, `cli_downloaders.rs`, `subscription_depth_activity.rs`, `subscription_depth_cleanup.rs`, `subscription_policy.rs`.

**Interfaces:** 保持测试用 `super::common::*` fixture；如旧模块私有 fixture 被新模块用到，只在 `#[cfg(test)]` 模块内标 `pub(super)`；不新增生产 API。

- [ ] **Step 1 — 测试目录：** 在 `main.rs` 增 `mod catalog_wall; mod library_configuration; mod authz_sessions; mod cli_downloaders; mod subscription_depth_activity; mod subscription_depth_cleanup; mod subscription_policy;`；把各原文件内相关完整 `#[tokio::test]` 和其专属 fixture **一同**移进同名新文件。基线：`cargo test -p api --test management -- --list` 的测试总数与拆前一致，原测试名逐项保留。
- [ ] **Step 2 — 函数长度：** 新增测试 `library.rs` 原 789–888 的整段测试、`subscribes.rs` 原 387–516 的整段测试分别抽 `setup_missing_rows` / `assert_missing_rows` 和 `create_library_for_subscription` / `assert_subscription_library_patch`，创建 fixture 与业务断言在函数之间按用途划分。要求每个测试及辅助函数 ≤60 行、每文件 ≤800 行；不删断言来达标。
- [ ] **Step 3 — 绿灯：** `cargo test -p api --test management` 全量 PASS；`wc -l` 确认原文件及七个新模块均 ≤800。再跑 `cargo test -p api && cargo test --workspace`。
- [ ] **Step 4 — Commit：** `git add crates/api/tests/management && git commit -m "test(api): split oversized management suites by use case"`。

**独立验收与顺序：** Task 1（架构边界）→ Task 2（handler 规模）→ Task 3（测试逻辑）；每项单独 review、提交。完成后 `git status --short` 为空。Task 3 的测试拆分不改变任何业务路由；若一个测试函数难以拆到 60 行，先保持全部业务断言，再拆 fixture/断言帮助方法，不以压缩代码到同一行规避硬限。
