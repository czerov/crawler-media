# Store 迁出 api crate Implementation Plan

> **历史状态（2026-10-13 整理）：** Store 抽离已有实现及正文执行记录；后续边界修正单独追踪，旧来源路径与行数是迁移时快照，不是当前布局。
>
> 本文保留原设计、发现/任务 ID 与当时证据；日期本身不表示过时。旧执行/提交/清理命令仅作历史记录，不应直接重跑；复选框不是当前实现清单。当前依据：[数据库/API review](<../../audits/2026-10-13-database-api-review.md>) 与 [剩余计划](<2026-10-13-database-api-followup.md>)，未覆盖的旧 ID 仍须单独对账。

> **规模规则更新：** 文中的 60 行硬限/整改目标是历史要求；当前 [AGENTS.md](<../../../AGENTS.md>) 为生产函数软限 60、测试软限 80、两者硬限 120。历史测试/行数记录不改写，规模合规不等于功能安全。

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (this session) or superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 SQLite 系统记录从 `crates/api/src/store/` 整包迁到独立 crate `crates/store`。`api` 只再导出 `::store::*`，对外路径 `api::Store` / `api::store::*` 不变。本轮不改 `jobs` / `playback` / `library` 去直接依赖 `store`。

**Architecture:** 先切断 `store` 模块对 HTTP / scrape compose / job_loop 的依赖，并收掉 `Store.app` 跨模块直写。然后在 `crates/store` 一次长出完整 `Store`（`api` 原文件不动）。最后 `api` 切到 `pub use ::store::*` 并删旧文件。禁止「搬走 `StoreError`/`maps.rs`、旧 `impl Store` 还在」的半搬——那会让 `api` 中途编不过。

**Tech Stack:** Rust 2024 workspace、rusqlite 0.37 bundled、Argon2id（ADR-0010）、四个 SQLite 文件（ADR-0008）。

## Global Constraints

- 词汇：`Subscribe`、`Filter`、`Library`、`Torrent`、`Wash-cut`（代码 `wash_cut`）。
- `domain` 无 IO。`store` 可依赖 `domain` 和 `library`（章节/音轨类型），**不得**依赖 `api`。其它 crate 仍不得依赖 `api`。
- 密码：ADR-0010（Argon2id + 存量明文登录升级）。站点凭证：ADR-0003 明文。
- 四个文件不变：`app.db` / `catalog.db` / `library.db` / `subscribe.db`。
- 函数硬限 60 行。文件硬限 800：迁过去时 `playback.rs`（现 807）必须拆，或在 Task 5 注明「下一刀再拆」并在 AGENTS.md 的超限名单加上。本计划 **迁时拆 `playback.rs`**。
- `schema.rs`（740）、`library.rs`（794）、`subscribes.rs`（721）本轮原样迁，不拆。
- 一次一个 Task，该 Task 的 crate 测绿再 commit。不要 GitHub issue。
- 本轮成功标准：**`api` 再导出后 `cargo test --workspace` 全绿**。不把 `jobs` crate 改成依赖 `store`。

## Out of scope

- `jobs` / `playback` / `media-server` 改 `store` 依赖（后续计划）。
- 改 schema / 合并四个 db / 密码哈希算法。
- 把 `scrape_config` 的 compose 逻辑整包塞进 `store`（store 只存 JSON 字符串）。
- 给 `Store` 的 `Connection` 字段改 `pub`。

## File map（迁完后）

当前磁盘：`crates/api/src/store/` **19** 个 `.rs`（含 `mod.rs`）+ `crates/api/src/password.rs`。

| 迁入 `crates/store/src/` | 来源 | 备注 |
|---|---|---|
| `lib.rs` | 新 | 模块树 + `Store` + `StoreError` + 时间函数 |
| `password.rs` | `crates/api/src/password.rs` | 随 users 走 |
| `settings_keys.rs` | 从 `api/settings_keys.rs` **复制用到的常量**（`TRANSFER_MODE`、`PLAYBACK_REVOKED_DEVICES`、`METADATA_SCRAPE`） | 不要把整个网络/代理 key 表搬进 store |
| `schema.rs` | `api/store/schema.rs` | 含 `MEDIA_COLS` |
| `open.rs` | `api/store/open.rs` | 含 settings KV 与 search_snapshots |
| `maps.rs` | `api/store/maps.rs` | `pub(super)` 保持 crate 内 |
| `users.rs` | `api/store/users.rs` | unix_now / password 改成本 crate |
| `sites.rs` | `api/store/sites.rs` | |
| `downloaders.rs` | `api/store/downloaders.rs` | |
| `subscribes.rs` | `api/store/subscribes.rs` | unix_now 改成本 crate |
| `media.rs` | `api/store/media.rs` | |
| `libraries.rs` | `api/store/libraries.rs` | |
| `roots.rs` | `api/store/roots.rs` | |
| `ledger.rs` | `api/store/ledger.rs` | |
| `playback.rs` + `playback_devices.rs`（或同类拆分） | `api/store/playback.rs` | 迁时拆到 ≤800 |
| `catalog.rs` | `api/store/catalog.rs` | |
| `scrape.rs` | `api/store/scrape.rs` | 只读写 settings JSON，不 compose |
| `probe_tasks.rs` | `api/store/probe_tasks.rs` | |
| `collections.rs` | `api/store/collections.rs` | 无 HTTP |
| `legacy_recycle.rs` | `api/store/legacy_recycle.rs` | **清单不可漏** |
| `library.rs` | `api/store/library.rs` | 依赖 `library` crate 的 Tracks/ChapterMarker |

`crates/api` 留下：

- `src/store/mod.rs` → `pub use ::store::*;`
- `src/lib.rs` → 仍 `pub mod store;` + `pub use store::Store;`
- `src/password.rs` → `pub use store::password::*;` 或删掉、调用方改 `store::password`
- `src/scrape_config.rs` 留在 `api`（compose 是产品规则，不是 SQL）
- `src/settings_keys.rs` 留在 `api`（HTTP/代理/CDP 键）

## 泄漏点（Task 2 必须先清，否则 `store` 会依赖 `api`）

| 位置 | 现状 | 处理 |
|---|---|---|
| `management/state.rs:84` | `store_guard.app.execute("UPDATE users SET role = 'admin'…")` | 加 `Store::force_admin_role(user_id)`，state 只调方法 |
| `store/users.rs` | `crate::password::*`、`crate::job_loop::unix_now` | password 进 store；`unix_now` 放 `store::unix_now` |
| `store/subscribes.rs:119,288,322` | `crate::job_loop::unix_now` | 同上 |
| `store/library.rs:83,88` | `crate::settings_keys::TRANSFER_MODE` | store 本地常量 |
| `store/playback.rs:558,587` | `crate::settings_keys::PLAYBACK_REVOKED_DEVICES` | store 本地常量 |
| `store/scrape.rs` | `crate::scrape_config::{KEY, ScrapeConfig, compose_config}` | store 只 `get_setting`/`put_setting` JSON；`get_scrape_config`/`naming_pattern`/`set_legacy_naming` **留在 api**（新文件 `api/src/scrape_store.rs` 或 `http` 旁的 thin impl），因为 compose 依赖 `ScrapeConfig` |
| `store/collections.rs:175-177` | `crate::http::library::{preferred_row, poster_path, artwork_url}` | `collection_cover` **无调用点**：Task 2 **删除该方法**。不要把 HTTP URL 生成带进 store |
| `Store.{app,catalog,library,subscribe}` | `pub(crate)` | 留在 store crate 内 `pub(crate)`；**不要 pub**。api 侧禁止再碰 Connection |

`job_loop::unix_now` 现实现：

```rust
pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}
```

抽到 `store` 后，`api/src/job_loop.rs` 改为 `pub use store::unix_now;`（或 `pub fn unix_now() { store::unix_now() }`），避免所有 worker 调用点大改。

---

## Wave 0 — 空 crate，不动 api store

### Task 1: workspace 挂上 `crates/store`

**Files:**
- Create: `crates/store/Cargo.toml`
- Create: `crates/store/src/lib.rs`
- Modify: 根 `Cargo.toml`（`members` + `workspace.dependencies.store`）
- **不要**改 `crates/api/Cargo.toml`、**不要**改 `crates/api/src/store/**`

**Interfaces:**
- Produces: `cargo check -p store` 过；`cargo check -p api` 仍过且无新依赖

`crates/store/Cargo.toml`：

```toml
[package]
name = "store"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
domain.workspace = true
library.workspace = true
rusqlite.workspace = true
uuid.workspace = true
thiserror.workspace = true
serde.workspace = true
serde_json.workspace = true
argon2 = { version = "0.5", features = ["std"] }
password-hash = { version = "0.5", features = ["rand_core"] }
tracing.workspace = true

[dev-dependencies]
tempfile.workspace = true
```

`crates/store/src/lib.rs` 先只放模块注释 + `pub fn unix_now() -> i64 { … }` 的副本也可以；**不要**定义空的 `pub struct Store;` 然后在下一 Task 去「搬走」api 的 `Store`——两份 `Store` 会让人误删 api 那份。

- [x] **Step 1:** 改根 `Cargo.toml` members / workspace.dependencies
- [x] **Step 2:** 写 `crates/store/Cargo.toml` + 最小 `lib.rs`
- [x] **Step 3:** `cargo check -p store && cargo check -p api`
- [x] **Step 4: Commit** `chore(store): add empty store crate to the workspace`

---

## Wave 1 — 先让现 store 不再依赖 HTTP / compose / 裸 SQL

在 **仍住在 `api` 的 store** 上做。每一步 `api` 必须保持绿。这是抽 crate 的前置，不是可选清洁。

### Task 2: 收掉 Connection 直写和 HTTP 封面

**Files:**
- Modify: `crates/api/src/store/users.rs` — 增加：

```rust
pub fn force_admin_role(&self, user_id: UserId) -> Result<(), StoreError> {
    self.app.execute(
        "UPDATE users SET role = 'admin' WHERE id = ?1",
        params![user_id.to_string()],
    )?;
    Ok(())
}
```

- Modify: `crates/api/src/management/state.rs:84-87` 改为 `store_guard.force_admin_role(user_id)?;`（或 `let _ = …` 若要保持忽略错误，改为打 `warn!` 后仍调用方法，不要 `.app`）
- Modify: `crates/api/src/store/collections.rs` — **删除** `collection_cover`（无调用点）
- Test: `cargo test -p api --test management user_lifecycle`
- Test: `cargo test -p api --test jellyfin authenticate_by_name`

- [x] **Step 1:** 写失败测试不是必须（行为不变）。直接改，跑上面两条。
- [x] **Step 2:** grep `store_guard.app` / `store.lock().app` / `crate::http::library` inside `store/` 必须为空。
- [x] **Step 3: Commit** `refactor(api): stop touching Store connections and HTTP from store`

### Task 3: scrape compose 退出 store 模块

**Files:**
- Create: `crates/api/src/scrape_store.rs`（或 `store` 旁的 `impl` 文件，**不要**放进 `store/` 目录）
- Modify: `crates/api/src/lib.rs` — `mod scrape_store;`
- Modify: 把 `Store::get_scrape_config` / `save_scrape_config` / `naming_pattern` / `set_legacy_naming` 从 `store/scrape.rs` 挪到新文件，内部仍 `self.get_setting` / `put_setting`
- Delete: `crates/api/src/store/scrape.rs` 并从 `store/mod.rs` 去掉 `mod scrape;`
- Test: `cargo test -p api --test management scrape_settings directory`

调用方（`http/directory.rs`、`claim.rs`、`management/subscribes/run.rs`）继续 `store.naming_pattern(...)`，因为方法还在 `impl Store` 上，只是源文件不在 `store/` 目录。抽 crate 时这些方法**留在 api**。

- [x] **Step 1:** 挪 impl，删 `store/scrape.rs`
- [x] **Step 2:** `rg "mod scrape" crates/api/src/store` 为空；`rg get_scrape_config crates/api` 仍能编
- [x] **Step 3: Commit** `refactor(api): keep scrape compose out of the store module`

### Task 4: unix_now / settings key / password 准备搬家

**Files:**
- Modify: `crates/api/src/store/users.rs`、`subscribes.rs` — `crate::job_loop::unix_now` 改成调用将要属于 store 的 `unix_now`。过渡：在 `store/mod.rs` 增加 `pub fn unix_now() -> i64 { … }`（与 job_loop 同实现），`job_loop.rs` 改为：

```rust
pub fn unix_now() -> i64 {
    crate::store::unix_now()
}
```

- Modify: `library.rs` / `playback.rs` — `TRANSFER_MODE` / `PLAYBACK_REVOKED_DEVICES` 改为 `super::settings_keys::…` 或文件内 `const`。不要在 store 模块 import `crate::settings_keys`。
- **不要**这步就搬 `password.rs`（下一波整包一起走）。仍可用 `crate::password`。
- Test: `cargo test -p api --test management -- jobs subscribes playback`

- [x] **Step 1–3:** 改调用 + grep `crate::job_loop` / `crate::settings_keys` inside `crates/api/src/store` 为空（password 除外）
- [x] **Step 4: Commit** `refactor(api): isolate store time and settings keys`

结束 Wave 1 时：`crates/api/src/store/**` 只允许依赖 `domain`、`library`、`rusqlite`、`crate::password`、`super::*`。`rg "crate::(http|scrape_config|settings_keys|job_loop)" crates/api/src/store` 必须空。

---

## Wave 2 — 在新 crate 长出完整 Store（api 原文件不动）

### Task 5: 复制（不是搬走）完整 store 到 `crates/store`

**Files:** 按 File map 创建 `crates/store/src/*`。`crates/api/src/store/**` **一字不删**。

**Interfaces:**
- Consumes: Wave 1 解耦后的 api store 源
- Produces: `store::Store::open`、`store::unix_now`、`store::password::{hash_password,verify_password,is_argon2_hash}`

复制时替换：

```text
crate::password::          → crate::password::
crate::job_loop::unix_now  → crate::unix_now   （若 Wave 1 已改成 super::unix_now，复制后改 crate::unix_now）
crate::settings_keys::X    → crate::settings_keys::X（store 自己的小文件）
crate::store::UNIT_WHOLE   → crate::UNIT_WHOLE
super::                    保持
```

`playback.rs`：复制后立刻拆出 `playback_devices.rs`（revoked devices + 足够让主文件 `wc -l` ≤ 800）。测试逻辑跟着走。

`lib.rs` 模块树 + 现有 `pub use` 列表（从 `api/src/store/mod.rs` 抄）：

```rust
pub use catalog::CatalogCacheRow;
pub use downloaders::DownloaderRow;
pub use libraries::Library;
pub use library::{MarkerResultReplacement, StoredMediaMarker};
pub use playback::{PlayLogRow, SessionRow, UNIT_WHOLE, UnitRow, UnitState};
pub use probe_tasks::{ProbeJob, ProbeJobUnit, ProbeJobUnitSpec};
pub use roots::LibraryRoot;
pub use subscribes::{PendingDownload, WantedHistory};
pub mod password;
```

`Store` 字段保持：

```rust
pub struct Store {
    data_dir: PathBuf,
    app_path: PathBuf,
    pub(crate) app: Connection,
    pub(crate) catalog: Connection,
    pub(crate) library: Connection,
    pub(crate) subscribe: Connection,
}
```

测试（新 crate，不删 api 测试）：

```rust
// crates/store/tests/open.rs
#[test]
fn open_creates_four_sqlite_files() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store::Store::open(tmp.path()).unwrap();
    for name in ["app.db", "catalog.db", "library.db", "subscribe.db"] {
        assert!(tmp.path().join(name).is_file(), "{name}");
    }
    let versions = store.schema_versions();
    assert_eq!(versions.iter().find(|(n, _)| *n == "app").unwrap().1, 3);
    assert_eq!(versions.iter().find(|(n, _)| *n == "library").unwrap().1, 4);
}
```

密码测从 `api/src/password.rs` 的 `#[cfg(test)]` 一起拷。

- [x] **Step 1:** 复制文件、改 import、拆 playback
- [x] **Step 2:** `cargo test -p store`
- [x] **Step 3:** `cargo test -p api --test management user_lifecycle` 仍绿（api 还没用新 crate）
- [x] **Step 4: Commit** `feat(store): copy Store implementation into crates/store`

此 Task 允许一次多文件：这是同一类型的机械复制，不是 12 个无关功能。reviewer 对照 `diff --stat` 看的是「api/store 与 store/src 应对齐」。

---

## Wave 3 — 一切换

### Task 6: api 再导出 `::store`，删除旧实现

**Files:**
- Modify: `crates/api/Cargo.toml` — `store.workspace = true`；可去掉 `api` 对 `argon2` / `password-hash` 的直接依赖（改走 store）
- Modify: `crates/api/src/store/mod.rs` 整文件替换为：

```rust
//! 自有 API 看到的 store 面。实现在 `crates/store`。
pub use ::store::*;
```

- Modify: `crates/api/src/lib.rs` — 保留 `pub mod store;` 和 `pub use store::Store;`
- Modify: `crates/api/src/password.rs` 改为：

```rust
pub use store::password::*;
```

或删除 `mod password`，所有 `crate::password` 改 `store::password` / `crate::store::password`。优先 **再导出**，少改调用点。
- Modify: `crates/api/src/job_loop.rs` — `unix_now` 继续转调 `store::unix_now`（Task 4 已做则保持）
- Modify: `crates/api/src/scrape_store.rs` — `impl Store` 仍编，因为 `Store` 现在是 `::store::Store`。确认 `get_setting` 仍 public。
- Delete: `crates/api/src/store/{catalog,collections,downloaders,ledger,legacy_recycle,libraries,library,maps,media,open,playback,probe_tasks,roots,schema,sites,subscribes,users}.rs`（**没有** scrape.rs，Wave 1 已删）
- **不要**删 `crates/api/src/store/mod.rs`

`pub use store::*;` 在 `mod store` 里会指到自己。必须 `::store`。

- [x] **Step 1:** 改 Cargo.toml + mod.rs 再导出
- [x] **Step 2:** `cargo check -p api`。修剩余 `crate::store::schema` 这类 **模块内路径**（应已无：schema 只被 media.rs 用，而 media.rs 已删）
- [x] **Step 3:** 删旧 `.rs`
- [x] **Step 4:**

```
cargo test -p store
cargo test -p api --test management
cargo test -p api --test jellyfin
cargo test -p api --test users
cargo test -p api --test persistence
```

Expected: 全绿。management 条数以当时 `cargo test` 输出为准，不要写死 369。

- [x] **Step 5: Commit** `refactor(api): re-export Store from crates/store`

### Task 7: workspace 门禁 + AGENTS.md

**Files:**
- Modify: `AGENTS.md` Size 超限名单：若 playback 已拆则不必加；注明：

```
Workspace: crates/<bounded-context>. domain is types only — no IO.
store is SQLite (ADR-0008). It may depend on domain and library.
Other crates may depend on domain and store; they must not depend on api.
```

- Modify: `docs/superpowers/plans/2026-09-26-api-crate-correctness.md` Out of scope 里「把整个 Store 搬出 api」改为「已由 2026-09-27-extract-store-crate 完成」
- **不要**改 ADR-0008 的四文件语义。
- Test: `cargo test --workspace`；`cd web && pnpm exec tsc --noEmit && pnpm test`

- [x] **Step 1:** 跑 workspace + web
- [x] **Step 2:** 改 AGENTS.md / 正确性计划一句
- [x] **Step 3: Commit** `docs: record store crate extraction`

---

## 执行顺序（必须）

```
Task 1（空 crate）
 → Task 2（force_admin_role + 删 collection_cover）
 → Task 3（scrape compose 离开 store/）
 → Task 4（unix_now / settings keys）
 → Task 5（复制完整实现到 crates/store）
 → Task 6（api 切 ::store，删旧文件）
 → Task 7（workspace + 文档）
```

Task 1–4 每步 `api` 必须绿。Task 5 只绿 `store`。Task 6 才是切换，之前 `api` 仍用自己的 store 源码。

## Spec coverage

| 要求 | Task |
|---|---|
| 独立 `crates/store` | 1, 5 |
| 不半搬导致 api 编不过 | 5 复制、6 一切换 |
| `pub(crate)` Connection 不泄漏 | 2, 5 字段保持 |
| 无 HTTP 进 store | 2 删 `collection_cover` |
| scrape compose 留 api | 3 |
| password 随 store（ADR-0010） | 5, 6 再导出 |
| `api::Store` 路径不变 | 6 `pub mod store` + `pub use ::store::*` |
| 四 sqlite 文件 | 5 open 测试 |
| 不改 jobs/playback crate 依赖 | 全局 out of scope |
| playback.rs ≤800 | 5 拆 |

## Placeholder scan

无 TBD。`force_admin_role`、`::store`、删除文件列表、schema version 断言（app=3, library=4）均写死。`collection_cover` 处理是删除不是「或上移」。

---

## 事后修正（2026-09-27）

迁出时曾把 `scrape_config` compose 放进 `crates/store`。后续按边界改回：KV 仍由 store `get_setting`/`put_setting` 持久化，compose 在 `crates/api/src/scrape_config.rs` + `ScrapeStoreExt`。`crates/store` 不再导出 `ScrapeConfig`。
