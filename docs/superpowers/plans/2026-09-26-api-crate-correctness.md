# API / crate 正确性与边界收口 Implementation Plan

> **历史状态（2026-10-13 整理）：** API/crate 边界历史整改；正文含部分完成记录，整体仍需按 Task/发现 ID 逐项对账，不能由后续 API review 推定全部关闭。
>
> 本文保留原设计、发现/任务 ID 与当时证据；日期本身不表示过时。旧执行/提交/清理命令仅作历史记录，不应直接重跑；复选框不是当前实现清单。当前依据：[数据库/API review](<../../audits/2026-10-13-database-api-review.md>) 与 [剩余计划](<2026-10-13-database-api-followup.md>)，未覆盖的旧 ID 仍须单独对账。

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (this session) or superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 先修会错身份、会误删 pending、会 panic 的路径；再把 CLI 迁到 `/api/v1` 并冻结 legacy HTTP；最后补齐 `User.role` / `LibraryId` 等 domain 缺口。不按过时契约补假端点。

**Architecture:** 公开缝在 `/api/v1` handler 与 `Store` 映射。每一刀先写失败测试，再改最小实现。词汇用 `CONTEXT.md`（Subscribe / Filter / Library / Torrent）。不引入 GitHub issue。

**Tech Stack:** Rust workspace crates, axum `/api/v1`, rusqlite `Store`, tower oneshot 集成测试，前端 `web/lib/api` + 成员页。

## Global Constraints

- 领域词：`Subscribe`、`Filter`、`Library`、`Torrent`、`Wash-cut`（代码 `wash_cut`）。不要发明 `SearchResult` / `MediaMeta`。
- 其它 crate 不得依赖 `api`。`domain` 只有类型，无 IO。
- 测试：`cargo test -p <crate>` 再 `cargo test --workspace`。无真实网络、无 Chromium、无真 qB。
- IO / parse 路径禁止 `unwrap` / `expect`（测试除外）。
- 静态图片路由保持 public（`AGENTS.md`）。
- 明确不做：AI、插件市场、转码、IM、OTA、`POST /users/{id}/reset-password`、恢复 `/tracking-state`。
- 一次只做一个 Task，测绿再 commit。

## Out of scope this plan

- 把整个 `Store` 搬出 `api` crate（已由 2026-09-27-extract-store-crate 完成）。
- 用户密码哈希（已由 ADR-0010 与 2026-09-27 完成）。
- 拆 `http/downloaders.rs` 等超限文件（已由 2026-09-27 拆为 instances.rs 与 tasks.rs 完成）。
- `release-forecast` 语义重做（改名或接播出日后开）。
- 把 playback 会话从 `api` 抽回 `playback` crate。

## File map

| File | Responsibility |
|---|---|
| `crates/domain/src/lib.rs` | `User.role`、`UserRole`、`LibraryId` |
| `crates/api/src/http/users.rs` | 用户 JSON 只读 `User.role` |
| `crates/api/src/store/users.rs` | 读写 `users.role`；UUID parse 走 `StoreError` |
| `crates/api/src/store/maps.rs` | 行映射失败返回 `rusqlite::Error`，不 panic |
| `crates/api/src/http/downloaders.rs` | pending ↔ 快照：标题匹配，体积只作附加约束 |
| `crates/downloader/src/lib.rs` | 导出 `snapshot_matches`（或等价公开函数） |
| `crates/media/src/client.rs` 及 douban/tvdb/bangumi/anilist | catalog 缓存打开失败上抛，不 `expect` |
| `crates/api/src/cli/*.rs` | 全部走 `/api/v1`，解析 `{ok,data}` |
| `crates/api/src/management/mod.rs` | CLI 迁完后拆掉 legacy router |
| `crates/api/src/http/library*.rs` 等海报 URL | 自有 API 用带连字符 UUID |
| `web/lib/api/members.ts` + `web/components/members-section.tsx` | 重置密码走 `PATCH /users/{id}` |
| `crates/api/tests/management/main.rs` | 挂上未编译的 `user_lifecycle` |
| `docs/api-contracts/self.md` | 只在行为变化时改一句，不恢复路由表 |

---

## Wave 1 — 正确性（先做）

### Task 1: 挂上从未编译的 user_lifecycle 测试

**Files:**
- Modify: `crates/api/tests/management/main.rs`
- Test: `crates/api/tests/management/user_lifecycle.rs`（已存在，未 `mod`）

**Interfaces:**
- Consumes: 现有 `user_lifecycle.rs`
- Produces: `cargo test -p api --test management user_lifecycle` 能跑

- [ ] **Step 1: 在 `main.rs` 增加模块**

在 `crates/api/tests/management/main.rs` 的 `mod user_state_isolation;` 附近加入：

```rust
mod user_lifecycle;
```

- [ ] **Step 2: 跑测试，修编译错误直到绿**

Run: `cargo test -p api --test management user_lifecycle -- --nocapture`

Expected: 能编译。若 `User { ... }` 稍后加 `role` 字段，本文件会在 Task 2 一起改。本 Task 只保证模块被编进测试。

若当前已绿：保持。若红：只修编译（缺 import / 缺字段），不改产品行为。

- [ ] **Step 3: Commit**

```bash
git add crates/api/tests/management/main.rs
git commit -m "$(cat <<'EOF'
test(api): compile user_lifecycle management tests

The file existed but was never declared in the test harness.
EOF
)"
```

---

### Task 2: User.role 单一真相

**Files:**
- Modify: `crates/domain/src/lib.rs`（`User`）
- Modify: `crates/api/src/http/users.rs`（`user_json`）
- Modify: `crates/api/src/store/users.rs`（insert/get/list/save 读写 role）
- Modify: `crates/api/src/management/state.rs`（种子管理员带 role）
- Modify: 所有 `User { ... }` 字面量（`crates/api/src/users.rs`、测试、jellyfin_images）
- Test: `crates/api/tests/management/user_lifecycle.rs` 或新建断言在同文件

**Interfaces:**
- Consumes: `users.role` 列已存在（schema `DEFAULT 'member'`）
- Produces:

```rust
pub enum UserRole { Admin, Member }

pub struct User {
    pub id: UserId,
    pub login: String,
    pub enabled: bool,
    pub role: UserRole,
}

impl UserRole {
    pub fn as_str(self) -> &'static str { /* "admin" | "member" */ }
}
```

`user_json` 使用 `user.role.as_str()`，删除对 `00000000-0000-0000-0000-000000000001` 的硬编码。

- [ ] **Step 1: 写失败测试**

在 `crates/api/tests/management/user_lifecycle.rs` 追加：

```rust
#[tokio::test]
async fn list_users_role_comes_from_db_not_seed_uuid() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/users",
            Some("management-secret"),
            json!({ "login": "second-admin", "password": "pw" }),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let id = json_body(created).await["data"]["id"].as_str().unwrap().to_string();

    let data = tmp.path().join("data");
    Connection::open(data.join("app.db"))
        .unwrap()
        .execute(
            "UPDATE users SET role = 'admin' WHERE id = ?1",
            params![id],
        )
        .unwrap();

    let listed = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/users",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let users = json_body(listed).await["data"].as_array().unwrap().clone();
    let row = users.iter().find(|u| u["id"] == id).expect("created user");
    assert_eq!(row["role"], "admin", "role must follow users.role, not seed UUID");
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p api --test management list_users_role_comes_from_db_not_seed_uuid -- --nocapture`

Expected: FAIL，`role` 仍是 `"member"`。

- [ ] **Step 3: 最小实现**

1. `domain::UserRole` + `User.role`。
2. `Store::insert_user` 写入 `role`；`get_user` / `list_users` / `user_by_token` SELECT `role`。
3. `http/users.rs` `user_json` 用 `user.role`。`create_user` 新成员 `UserRole::Member`。
4. `ApiState::new_arc` 种子用户 `role: UserRole::Admin`（可保留 SQL UPDATE 作双写，但 JSON 不再看 UUID）。
5. 编译器会指出所有 `User {` 缺字段：测试里管理员用 `UserRole::Admin`，成员用 `Member`。

不要在本 Task 做密码哈希。

- [ ] **Step 4: 跑测试**

Run:

```
cargo test -p domain
cargo test -p api --test management user_lifecycle
cargo test -p api --test management authz
```

Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add crates/domain crates/api
git commit -m "$(cat <<'EOF'
fix(users): serve role from the users table

List/create JSON no longer infers admin from the seed UUID.
EOF
)"
```

---

### Task 3: pending 对齐下载器快照：禁止「只比体积」

**Files:**
- Modify: `crates/downloader/src/lib.rs`（公开匹配函数）
- Modify: `crates/api/src/http/downloaders.rs`（`list_tasks` 两处 find）
- Test: `crates/api/tests/management/downloaders.rs`
- Test: `crates/downloader/tests/` 或 `crates/downloader/src/lib.rs` 的 `#[cfg(test)]`

**Interfaces:**
- Consumes: `names_match`、`TaskSnapshot { name, size_bytes, ... }`、`Torrent.title` / `size_bytes`
- Produces:

```rust
/// Title must match. Size, when known and > 0, must also match.
/// Never match on size alone.
pub fn torrent_matches_snapshot(title: &str, size_bytes: Option<u64>, snapshot_name: &str, snapshot_size: u64) -> bool
```

匹配规则：

- `names_match(snapshot_name, title)` 为假 → 不匹配
- `size_bytes` 为 `Some(n)` 且 `n > 0` → 还要求 `snapshot_size == n`
- 否则（未知体积）→ 标题匹配即可

`list_tasks` 里清理 pending 与展示 state 的两处 `.find` 都改用该函数。删除 `|| size_bytes == snapshot.size`。

- [ ] **Step 1: downloader 单元测试（失败）**

在 `crates/downloader/src/lib.rs` 末尾或 `crates/downloader/tests/match.rs`：

```rust
#[test]
fn same_size_different_title_does_not_match() {
    assert!(!torrent_matches_snapshot(
        "Movie.A.2024.1080p",
        Some(1_000_000),
        "Movie.B.2024.1080p",
        1_000_000,
    ));
}

#[test]
fn matching_title_and_size_matches() {
    assert!(torrent_matches_snapshot(
        "Movie.A.2024.1080p",
        Some(1_000_000),
        "Movie.A.2024.1080p",
        1_000_000,
    ));
}

#[test]
fn unknown_size_still_matches_title() {
    assert!(torrent_matches_snapshot(
        "Movie.A.2024.1080p",
        None,
        "Movie.A.2024.1080p",
        1_000_000,
    ));
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p downloader torrent_matches_snapshot -- --nocapture`

Expected: FAIL（函数尚未存在）。

- [ ] **Step 3: 实现函数并接到 list_tasks**

把 `size_matches` 留作内部或删除；对外只暴露 `torrent_matches_snapshot`。

`http/downloaders.rs` 两处改为：

```rust
list.iter().find(|s| {
    downloader::torrent_matches_snapshot(
        &item.torrent.title,
        item.torrent.size_bytes,
        &s.name,
        s.size_bytes,
    )
})
```

- [ ] **Step 4: 管理面回归：同体积不同标题不得清掉 pending**

在 `downloaders.rs` 测试里加（复用 `seed_pending_at`）：两行 pending，标题不同、`size_bytes` 相同；`MemoryDownloader` 快照只有其中一个标题。超过宽限期后 `GET /api/v1/downloaders/tasks`：

- 快照里有的那条可以按现有逻辑（在则保留，不在且过期则清）
- **标题对不上的那条不得因为体积相同被当成「已在下载器」或「已消失的那条」而误删**

最小断言：种子 B（下载器里没有、标题不同、体积相同）在宽限期内仍在任务列表；过期清理时只删真正缺席且标题对得上的行，或两条都因缺席被删——但 **不得把 B 当成 A 的快照命中**。

更简单的可观察行为：构造 MemoryDownloader 快照含 `Movie.B` 体积 1000；pending 是 `Movie.A` 体积 1000、提交已过宽限期。清理后 pending **仍在**（标题不匹配 → 不能当作「下载器里有这个任务」），且因为缺席+过期会被删——等一下，缺席是 `matched_snapshot.is_none()`。

当前 bug：A pending 体积=1000，快照只有 B 体积=1000 → 被当成 matched → **不会清理、还会显示成 B 的进度**。正确：A 与 B 标题不同 → `matched_snapshot` 为空 → 视为 absent。过期则清理 A；未过期则 state=`missing`。

测试：

```rust
#[tokio::test]
async fn list_tasks_does_not_bind_pending_to_same_size_different_title() {
    // pending: Movie.A size 1000, submitted now (within grace)
    // MemoryDownloader snapshot: Movie.B size 1000
    // expect: state == "missing" (not the snapshot's progress), pending kept
}
```

需要 `MemoryDownloader` 能注入 snapshot。若没有，用现有 `task_snapshots` 钩子；没有则在 MemoryDownloader 加测试用 `push_snapshot`。保持测试不碰网络。

- [ ] **Step 5: 跑测试**

```
cargo test -p downloader
cargo test -p api --test management downloaders
```

Expected: PASS。

- [ ] **Step 6: Commit**

```bash
git add crates/downloader crates/api
git commit -m "$(cat <<'EOF'
fix(downloader): match tasks by title, never size alone

Same-size torrents no longer steal each other's pending rows.
EOF
)"
```

---

### Task 4: Store 行映射不再 expect UUID

**Files:**
- Modify: `crates/api/src/store/maps.rs`
- Modify: `crates/api/src/store/users.rs`、`sites.rs`、`downloaders.rs` 里同类 `expect("uuid")`
- Test: `crates/api/tests/persistence.rs` 或 `crates/api/tests/management/` 新测

**Interfaces:**
- Consumes: `StoreError::Uuid`
- Produces: 脏 UUID 行让 `list_*` 返回 `Err`，HTTP 变 500 `store.error`，进程不 abort

 rusqlite 的 `query_map` 闭包返回 `rusqlite::Result`。把 parse 失败映射为：

```rust
id: MediaId::from_str(&raw).map_err(|e| {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(e),
    )
})?
```

抽一个小助手 `fn parse_id<T: FromStr>(raw: String, idx: usize) -> rusqlite::Result<T>` 放在 `maps.rs`，避免复制。

- [ ] **Step 1: 失败测试**

打开临时 Store，插入合法 media，再用 SQL 把 `id` 改成 `'not-a-uuid'`，调用 `list_media()`（或现有 list API）。

```rust
#[test]
fn list_media_returns_error_on_corrupt_uuid_instead_of_panic() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    // insert one media then:
    store.app.execute("UPDATE media SET id = 'nope' LIMIT 1", []).unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| store.list_media()));
    assert!(result.is_ok(), "must not panic");
    assert!(result.unwrap().is_err());
}
```

`Store.app` 若是 `pub(crate)`，测试在 `crates/api` 内可用；否则用 `Connection::open(data/app.db)`。

- [ ] **Step 2: 确认现在 panic**

Run: `cargo test -p api list_media_returns_error_on_corrupt_uuid -- --nocapture`

Expected: FAIL（panic 或 catch_unwind `Err`）。

- [ ] **Step 3: 改 maps / users / downloaders / sites 的 expect**

同一 Task 清掉 `store/` 下非测试的 `expect("uuid"|"kind"|"fetch")`。非法 `kind` 已有 `StoreError::MediaKind`：映射层转 rusqlite 错误即可。

- [ ] **Step 4: 跑测试**

```
cargo test -p api --test persistence
cargo test -p api --lib
```

Expected: PASS，无 panic。

- [ ] **Step 5: Commit**

```bash
git add crates/api/src/store crates/api/tests
git commit -m "$(cat <<'EOF'
fix(store): map corrupt ids to errors instead of panicking
EOF
)"
```

---

### Task 5: Catalog 缓存打开失败上抛

**Files:**
- Modify: `crates/media/src/client.rs` `Tmdb::new_at`
- Modify: `crates/media/src/douban.rs`、`tvdb.rs`、`bangumi.rs`、`anilist.rs` 同样的 `expect("catalog.db")`
- Test: `crates/media/tests/` 或模块内测试：对不可写路径 `new_at` 返回 `Err`

**Interfaces:**
- Consumes: `CatalogCache::open` 已是 `Result`
- Produces: `Tmdb::new_at` → `Result<Self, TmdbError>`（或保持 Self 但 `new` 返回 Result）。调用方 `api` catalog 构造要 `?` / 日志，不能让 HTTP 进程在启动中途 abort——启动失败可以退出，运行中重建不行。

推荐：

```rust
pub fn new_at(http: H, catalog_db: &Path, now: i64) -> Result<Self, TmdbError>
```

`api` 里 `TmdbCatalog::new` 对 Err 打 `error!` 并跳过该源，fanout 仍可用其它源。

- [ ] **Step 1: 失败测试**（media crate，假路径）

```rust
#[test]
fn tmdb_new_at_returns_err_when_cache_cannot_open() {
    let http = /* 现有 fake CatalogGet */;
    let result = Tmdb::new_at(http, Path::new("/dev/full/not-a-dir/catalog.db"), 0);
    assert!(result.is_err());
}
```

路径按平台选一个必失败的（只读目录）。若不好造失败，测签名变化：编译期让旧的 `Tmdb::new_at(...)` 不带 Result 的调用方失败，再改调用点。

- [ ] **Step 2–4:** 改签名、改 api 调用点、`cargo test -p media`、`cargo test -p api --test management catalog`

- [ ] **Step 5: Commit**

```bash
git commit -m "$(cat <<'EOF'
fix(media): surface catalog cache open errors

Opening catalog.db no longer panics via expect.
EOF
)"
```

---

## Wave 2 — HTTP 表面

### Task 6: 前端重置密码走 PATCH，不新增端点

**Files:**
- Modify: `web/lib/api/members.ts`
- Modify: `web/components/members-section.tsx`
- Test: 若有 vitest/前端单测则加；否则手工约定：`resetMemberPassword` 调用 `PATCH /users/{id}` `{ password }` 并返回服务端用户 + 新密码（客户端生成随机密码再 PATCH）。

**Interfaces:**
- Consumes: 已有 `PATCH /api/v1/users/{id}` `{password?}`（改密并撤会话）
- Produces: `resetMemberPassword(id)` 不再 throw

流程：

1. 前端生成一次性密码（足够长的随机串）。
2. `PATCH /users/{id}` body `{ password }`。
3. 成功则把该密码显示一次（现有 `passwordResult` UI）。

不要加 `POST /users/{id}/reset-password`。

- [ ] **Step 1:** 改 `members.ts`，删除 throw 与 TODO。
- [ ] **Step 2:** `members-section.tsx` 保持调用 `resetMemberPassword`。
- [ ] **Step 3:** `pnpm exec tsc --noEmit`（在 `web/`）。
- [ ] **Step 4: Commit**

```bash
git commit -m "$(cat <<'EOF'
fix(web): reset member password via PATCH /users/{id}
EOF
)"
```

---

### Task 7: CLI 迁到 `/api/v1` 并读 `{ok,data}` 信封

**Files:**
- Modify: `crates/api/src/cli/actions.rs`、`lists.rs`、`filters.rs`、`users.rs`、`downloaders.rs`、`catalog.rs`、`mod.rs`
- Test: `crates/api/tests/management/cli.rs`

**Interfaces:**
- Consumes: `/api/v1` 路由与信封
- Produces: CLI 不再打 `/subscribes`、`/filters`、`/search`、`/users` 等根路径

规则：

- 路径一律加 `/api/v1` 前缀（已有的 libraries 保持）。
- 成功体从 `body["id"]` 改为 `body["data"]["id"]`（兼容：若有 `data` 用 `data`，否则旧字段——**不要兼容层**，测试一起改）。
- `POST /users` body 用 `{login, password}`，把 CLI `--token` 当作初始密码（与 legacy 注释一致）。
- `GET /filters` → `GET /api/v1/rule-sets`；`POST /filters` → `POST /api/v1/rule-sets`。
- `GET /search?query=` → `GET /api/v1/search/torrents?keyword=`。
- `POST /search/admit`：若 v1 无此路径，CLI admit 改为调用现有投递 `POST /api/v1/downloaders/submit`，或保留一个 **admin-only** v1 别名。优先：查 `http/mod.rs`；没有就给 CLI 改用 submit，不把 legacy admit 留下。
- `POST /unidentified/claim` → `POST /api/v1/unidentified/{id}/claim`。
- `POST /jobs/tick` → `/api/v1/jobs/tick`。
- catalog：`/api/v1/search/titles`（不要 `/catalog/search`）。

- [ ] **Step 1:** 先改 `cli.rs` 测试里期望的 path/信封，跑红。
- [ ] **Step 2:** 改 CLI 实现。
- [ ] **Step 3:** `cargo test -p api --test management cli`
- [ ] **Step 4: Commit**

```bash
git commit -m "$(cat <<'EOF'
refactor(cli): talk only to /api/v1 envelopes
EOF
)"
```

---

### Task 8: 去掉 legacy HTTP router

**Status:** **已完成 (DONE)**。所有存量管理测试已全部迁移至 `/api/v1`（统一 `{ok, data}` 信封），根路径上的旧管理表面已完全拆除，断言全部返回 404；Jellyfin/Emby 协议路由保持 100% 隔离并通过全套协议测试验证。详细实施记录见 `docs/superpowers/plans/2026-09-26-unify-v1-drop-legacy.md`。

- [x] **Step 1:** 测试：`GET /subscribes` 等旧管理端点全部返回 404（见 `tests/management/legacy_gone.rs`）。
- [x] **Step 2:** 从 `management::router` 彻底去掉 `.merge(protected)`，仅保留 `/api/v1` nest 与 `jellyfin` merge。
- [x] **Step 3:** 全量测试改打 `/api/v1`。
- [x] **Step 4:** `cargo test -p api --test management` 及 Jellyfin/media-server 系列测试全绿。
- [x] **Step 5: Commit**

```bash
git commit -m "$(cat <<'EOF'
refactor(api): drop legacy management HTTP surface

CLI and tests use /api/v1 only. Jellyfin/Emby routes stay.
EOF
)"
```

---

### Task 9: 自有 API 海报 URL 带连字符 UUID

**Files:**
- Modify: `crates/api/src/http/subscriptions/views.rs`、`library.rs`、`library_artwork.rs`、`collections.rs`、`playback.rs`、`playback_views.rs`、`library_chapters.rs`、`library_organize.rs`、`reidentify.rs`
- 抽 `fn artwork_url(kind: &str, ledger_id: LedgerId) -> String`（`/posters/{uuid}` 带连字符）
- 读路径：handler 已 `Uuid::parse_str` 两种都收，**继续收 compact**，只改**写出**的自有 JSON。
- 不要改 `media-server` compact id（Jellyfin 协议）。
- Test: 任一 library items 测试断言 `poster_url` 含 `-`。

- [ ] **Step 1:** 失败测试：`GET /api/v1/libraries/{id}/items` 的 `poster_url` matches UUID 正则带连字符。
- [ ] **Step 2:** 实现 helper，替换自有 API 的 `replace('-', "")`。
- [ ] **Step 3:** `cargo test -p api --test management library`
- [ ] **Step 4: Commit**

```bash
git commit -m "$(cat <<'EOF'
fix(api): emit hyphenated UUIDs on self-hosted artwork URLs
EOF
)"
```

---

### Task 10: appearance / network stub 收口

**Files:**
- Modify: `crates/api/src/http/mod.rs`（删除或标注）
- `web/lib/backdrop.tsx` 已 noop，不要接假 GET。
- 前端 `/settings/appearance` 是页面分区，不是 API。

决策（本计划锁定）：**删除** `GET /api/v1/settings/appearance` 与 `GET /api/v1/settings/network`。前端若未调用则无事；`discover-view` 的 `/settings/network` 是路由不是 API。

- [ ] **Step 1:** grep ` /settings/appearance` 与 `/settings/network` 在 `web/lib/api` 与 `crates/api/tests`。无调用则删路由。
- [ ] **Step 2:** 若测试打这些路径，改为期望 404。
- [ ] **Step 3: Commit**

```bash
git commit -m "$(cat <<'EOF'
chore(api): remove empty appearance and network setting stubs
EOF
)"
```

---

## Wave 3 — 领域类型与进度

### Task 11: `LibraryId` 进入 domain，Subscribe 不再用 String

**Status (this session):** ✅ 已完成。`domain::LibraryId` 新增强类型实体 ID，`Subscribe.library_id` 改为 `Option<LibraryId>`，`maps.rs` 使用 `parse_id` 安全解析，`subscription_json` 正确输出 `library_id`。全 workspace 测试通过。

**Files:**
- Modify: `crates/domain/src/lib.rs`（`entity_id!(LibraryId)`，`Subscribe.library_id: Option<LibraryId>`）
- Modify: `crates/api/src/store/maps.rs`、`subscribes.rs`、`libraries.rs`（Library.id 可暂留 String，但 Subscribe 侧 typed）
- Modify: 所有 `library_id: None` 测试夹具
- Test: 现有 subscribe / library 测试编译即覆盖；加 round-trip：insert subscribe with library_id，get 回来是同一 `LibraryId`

不要在本 Task 把整个 `store::Library` 搬进 domain（字段太多、含 IO 配置）。只把 **id 类型** 收口。

- [ ] **Step 1:** 改 domain，编译失败列表当 checklist。
- [ ] **Step 2:** 修编译。
- [ ] **Step 3:** `cargo test -p domain && cargo test -p subscribe && cargo test -p api --test management subscribes`
- [ ] **Step 4: Commit**

```bash
git commit -m "$(cat <<'EOF'
refactor(domain): type Subscribe.library_id as LibraryId
EOF
)"
```

---

### Task 12: 无上界 TV 窗口的进度不要报 total=0

**Files:**
- Modify: `crates/api/src/http/subscriptions/views.rs` `progress()`
- Test: `crates/api/tests/management/open_coverage.rs` 或 `subscription_depth.rs`

规则（锁定）：

- `episode_to: None` 且 catalog 里没有 `episode_count` → `total = imported.max(1)` 若已有 facts；若完全空则 `total = 1`（与 `Coverage::units()` 单集窗口一致），**禁止 `(0, n)`**。
- 有 catalog `episode_count` 时保持现有「用季集数当 total」。

- [ ] **Step 1:** 失败测试：TV subscribe `episode_to=None`、无 cache、无 facts → `progress.total >= 1`，`missing` 非负。
- [ ] **Step 2:** 改 `progress()` else 分支。
- [ ] **Step 3:** `cargo test -p api --test management open_coverage`
- [ ] **Step 4: Commit**

```bash
git commit -m "$(cat <<'EOF'
fix(subscribe): never report zero total for open TV coverage
EOF
)"
```

---

### Task 13: Catalog trait 空 default 改为明确不支持（仅调用方可见）

**Files:**
- Modify: `crates/api/src/catalog.rs`
- Modify: fanout / discover handler：源返回 `Err("unsupported")` 时记入 `providers[].ok=false`，不要 silently `[]` 当成功。
- Test: `crates/api/tests/management/catalog.rs` —— 只启用 Douban 的 filtered discover 应 `ok: false` 或空 + message，而不是假装成功墙。

范围控制：不要一次改 30 个 default。只改 **discover 实际调用的** `filtered_*` / `top_rated_*` / `trending_*`：未实现的源返回 `Err`。`person_details` 等保持空 Ok，避免人物页变 502。

- [ ] **Step 1:** 选一条前端会打的路径（`GET /api/v1/discover/movie/filtered`）。
- [ ] **Step 2:** 测试：无 TMDB、只有空 default 的源 → 响应带失败信息。
- [ ] **Step 3: Commit**

```bash
git commit -m "$(cat <<'EOF'
fix(catalog): do not report unsupported discover as success
EOF
)"
```

---

## Wave 4 — 明确不做（计划内禁止）

不要在本计划中实现：

1. `POST /users/{id}/reset-password`
2. `/subscriptions/{id}/tracking-state` 或 `/follow-future`
3. 按旧删除清单拆 `/collections` 或 `/health`
4. 用户密码哈希
5. `Store` 整包搬迁
6. `list_ledger()` 按库 SQL 收窄（正确性之后的性能计划）
7. `release-forecast` 接播出日历

---

## 执行顺序（必须）

```
Task 1 → 2 → 3 → 4 → 5 → 6 → 7 → 8 → 9 → 10 → 11 → 12 → 13
```

7 必须在 8 之前。2 会改 `User` 字面量，越早越好。3 不依赖 2，但不要并行改同一测试文件以外的东西——本会话按序做。

每 Task 结束：该 crate `cargo test -p …`；Wave 1 结束再 `cargo test --workspace` 一次。

## Spec coverage

| 审计项 | Task |
|---|---|
| user_json 硬编码 UUID | 2 |
| 体积误匹配 pending | 3 |
| store expect panic | 4 |
| catalog expect panic | 5 |
| reset-password 前端 throw | 6 |
| 双 HTTP 表面 | 7, 8 |
| compact id 泄漏自有 API | 9 |
| appearance/network stub | 10 |
| LibraryId / Subscribe.library_id | 11 |
| progress total=0 | 12 |
| Catalog 静默空 | 13 |
| 过时契约当规范 | 已在会话内改文档；本计划不恢复路由表 |
| Store 上帝 crate | Out of scope |
| 明文密码 | Out of scope |

## Placeholder scan

无 TBD。Task 3 的 MemoryDownloader 快照注入若缺失，在该 Task 内加测试钩子，不另开任务。
