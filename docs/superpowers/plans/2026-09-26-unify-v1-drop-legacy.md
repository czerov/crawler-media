# 自有 API 统一 `/api/v1`、拆除 legacy 管理面 Implementation Plan

> **历史状态（2026-10-13 整理）：** 自有 v1 与 Jellyfin 分界的历史迁移记录；后续 envelope 计划已记录根管理路由收口，不代表本文每项测试/客户端验收已重新执行。
>
> 本文保留原设计、发现/任务 ID 与当时证据；日期本身不表示过时。旧执行/提交/清理命令仅作历史记录，不应直接重跑；复选框不是当前实现清单。当前依据：[数据库/API review](<../../audits/2026-10-13-database-api-review.md>) 与 [剩余计划](<2026-10-13-database-api-followup.md>)，未覆盖的旧 ID 仍须单独对账。

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 自有 HTTP 只留 `/api/v1`（信封 `{ok,data}`）。根路径不再挂 `/subscribes` `/filters` `/search` 等旧管理面。Jellyfin/Emby 继续挂在 `/` 与 `/emby`，只兼容官方协议，不兼容我们自己的旧管理 API。每迁一批测试就跑绿，最后拆 router 时 Jellyfin 测试必须仍绿。

**Architecture:** 先把测试从 legacy 根路径迁到已有的 `/api/v1` handler（路径 + 信封），再删 `management::router` 里的 `.merge(protected)`。Jellyfin 路由来自 `media_server::routes`，与 legacy 管理面是两套 Router，拆后者不会自动拆前者。不要为旧测试保留第二套 JSON 形状。

**Tech Stack:** axum `Router`、oneshot 集成测试、`{ok, data}` 信封、Jellyfin 协议测试在 `crates/api/tests/jellyfin*.rs`。

## Global Constraints

- 领域词：`Subscribe`、`Filter`、`Library`、`Torrent`、`Wash-cut`（代码 `wash_cut`）。
- 自有 API 只走 `/api/v1`，成功 `{ok:true,data}`，失败 `{ok:false,error:{code,message}}`。
- Jellyfin/Emby：`/Users/AuthenticateByName`、`/Items`、`/Videos/{id}/stream`、compact id、`/emby` 前缀 **只兼容官方协议**。不要为了迁测试去改这些路径或 DTO。
- 不要发明 `POST /users/{id}/reset-password` 或 `/tracking-state` 专用端点。
- 静态图片（`/api/v1/posters/*` 等）保持 public。
- IO/parse 路径禁止 `unwrap`/`expect`（测试除外）。
- 一次一个 Task，测绿再 commit。不要 GitHub issue。
- 前端已经打 `/api/v1`，本计划原则上不改 `web/`（发现 Task 中途有调用根路径再改）。

## Why this plan exists

旧管理面（`GET /subscribes`、`POST /filters`…）不是 Jellyfin。它是我们自己更早的 REST，无信封、路径名词过时（Subscribe 叫 `/subscribes`，Filter 叫 `/filters`）。长期双表面会让测试继续写旧形状，新 bug 修两遍。

Jellyfin 客户端（Infuse / VidHub）打的是 `/Users` `/Items` `/Videos/.../stream`。这些必须留下。拆的是 **crawler-media 自己的旧管理路径**，不是 Jellyfin。

## Out of scope

- 改 Jellyfin DTO / compact id / 播放鉴权（除非现有 `cargo test -p api --test jellyfin` 在本计划中变红，那是回归，必须修）。
- 给 v1 增加「兼容旧字段」的第二套 JSON。
- 拆 `Store`、密码哈希、发现墙 `row()` 的 `Err → Null`（那是另一项：分区降级，不是本计划）。
- 把整个 `management/` 模块改名（`ApiState` 仍住在这里没问题）。

## Path map（legacy → v1）

拆 router 前，测试必须全部改成右列。左列最终应对 **404**（有无 token 都是 404，不是 401 的管理面）。

| Legacy (root) | v1 | 信封 |
|---|---|---|
| `POST/GET /subscribes` | `POST/GET /api/v1/subscriptions` | `{ok,data}`；list 是 array；create 201，`data.id` |
| `POST /subscribes/{id}/run` | `POST /api/v1/subscriptions/{id}/search` | `{ok,data}` |
| `GET/POST /filters` | `GET/POST /api/v1/rule-sets` | `{ok,data}` |
| `PUT /filters/default` | `PUT /api/v1/rule-sets/default` | `{ok,data:{default_rule_set_id}}` |
| `GET /search` | `GET /api/v1/search/torrents?keyword=` | `{ok,data:{items,sites}}` |
| `POST /search/admit` | `POST /api/v1/search/admit` | `{ok,data}` |
| `GET/POST /users` | `GET/POST /api/v1/users` | create body `{login,password}`（不是 `{login,token}`） |
| `GET/POST /sites` 及 `/{id}` login/check-in | `/api/v1/sites` 同后缀 | `{ok,data}` |
| `GET/POST /downloaders`、`PATCH /downloaders/{id}` | `/api/v1/downloaders` | `{ok,data}` |
| `GET /jobs`、`POST /jobs/tick` | `/api/v1/jobs`、`/api/v1/jobs/tick` | `{ok,data}` |
| `GET /ledger` | `/api/v1/ledger` | `{ok,data}` |
| `GET /unidentified`、`POST /unidentified/claim` | `/api/v1/unidentified`、`/api/v1/unidentified/claim` 或 `.../{id}/claim` | `{ok,data}` |
| `GET /directory`、`PUT /directory`、roots | `/api/v1/directory` 同后缀 | `{ok,data}` |
| `GET /catalog/search` | `GET /api/v1/search/titles?keyword=` | `{ok,data:{titles}}` |
| `GET /catalog/discover` | `GET /api/v1/discover/{kind}` | `{ok,data:{sections}}` |
| `GET/DELETE /catalog/cache` | `/api/v1/catalog/cache` | `{ok,data}` |
| `GET /downloads` | `GET /api/v1/downloaders/tasks` | `{ok,data:{items,sources}}` |

**不要改：** `/Users`、`/Items`、`/Videos/{id}/stream`、`/Sessions/...`、`/System/Ping`、`/emby/...`。这些是 Jellyfin。

## Envelope helper（所有迁测试共用）

在 `crates/api/tests/management/common.rs` 增加（Task 1）：

```rust
/// 自有 API 成功体。非 `{ok:true}` 时 panic，避免测试再读旧根字段。
pub(crate) fn json_data(response: axum::response::Response) -> impl std::future::Future<Output = Value> {
    async move {
        let status = response.status();
        let body = json_body(response).await;
        assert_eq!(body["ok"], true, "expected v1 envelope ok=true, status={status} body={body}");
        body.get("data").cloned().unwrap_or(Value::Null)
    }
}
```

迁完的测试：

- 成功：`let data = json_data(response).await;` 然后断言 `data["id"]` / `data.as_array()`，不要 `json_body(...)["title"]`。
- 失败：仍用 `json_body`，断言 `body["ok"]==false` 与 `error.code`。

`create_site` / `create_subscribe` 改打 v1 并返回 `data`（见 Task 1）。`create_api_subscribe` 已经如此，可与 `create_subscribe` 合并。

## File map

| File | Responsibility |
|---|---|
| `crates/api/tests/management/common.rs` | `json_data`；`create_site`/`create_subscribe` 改 v1 |
| `crates/api/tests/management/*.rs` | 按 Task 分批改路径 + 信封 |
| `crates/api/tests/users.rs` | 独立测试 crate：同样迁 v1 |
| `crates/api/tests/management/legacy_run_parity.rs` | 先改 v1 run；拆 router 后断言 `/subscribes/{id}/run` 404 |
| `crates/api/src/management/mod.rs` | 最后去掉 `.merge(protected)` 与仅被 legacy 使用的 route |
| `crates/api/src/management/{search,filters,subscribes,sites}.rs` | 无 v1 调用方则删 route；被 v1 复用的 handler 留下 |
| `crates/api/src/users.rs` | `create_user`/`list_subscribes` 若只服务 legacy，随 router 一起停用 |
| `crates/api/tests/jellyfin*.rs` | **只跑、不改路径**；红了按官方协议修 provider，不改测试去迁就旧管理面 |
| `crates/api/src/http/mod.rs` | 本计划不改路由表（v1 已齐）。若缺映射表里的 v1 路径，在对应 Task 里补，不要复活根路径 |

---

## Wave 0 — 安全网

### Task 1: 信封 helper + 共享 create_* 迁 v1 + 冻结 Jellyfin 测试命令

**Files:**
- Modify: `crates/api/tests/management/common.rs`
- Test: 现有 `create_site` / `create_subscribe` 的调用方会暂时红，本 Task 必须把它们改到能编译并通过（至少 `sites.rs` 里用 `create_site` 的那几条、以及所有 `create_subscribe`）

**Interfaces:**
- Consumes: v1 `POST /api/v1/sites`、`POST /api/v1/subscriptions` 已存在
- Produces: `json_data`；`create_site` 返回 `data` 对象（含 `id`）；`create_subscribe` 返回 `data` 对象

站点 create 的 v1 body 与 `site_payload()` 对齐（已有 name/url/profile_id/cookie）。订阅 create 用现有 `subscribe_payload`；v1 要 `filter_id` 时：先 `POST /api/v1/rule-sets` 拿到 `data.id`，再放进 payload 的 `filter_id`（字符串 UUID）。若 `subscribe_payload` 里的嵌套 `filter: {name,atoms}` 只被 legacy `POST /subscribes` 接受，改为：

```rust
pub(crate) async fn ensure_default_filter(app: &axum::Router) -> String {
    let listed = json_data(
        app.clone()
            .oneshot(request("GET", "/api/v1/rule-sets", Some("management-secret"), Value::Null))
            .await
            .unwrap(),
    )
    .await;
    listed.as_array().unwrap()[0]["id"].as_str().unwrap().to_string()
}
```

种子库已有默认 Filter（见 `cli_filters_lists_seeded_default_on_fresh_store`）。`create_subscribe` 设 `filter_id` 为该 id，删掉 payload 里的 `filter` 对象。

- [x] **Step 1: 加 `json_data`，改 `create_site`/`create_subscribe` 打 v1**

`create_site`：

```rust
let response = app.clone().oneshot(request(
    "POST", "/api/v1/sites", Some("management-secret"), site_payload(),
)).await.unwrap();
assert_eq!(response.status(), StatusCode::CREATED);
json_data(response).await
```

`create_subscribe` 同样改 `/api/v1/subscriptions`，返回 `json_data(...).await`。

- [x] **Step 2: 修所有因返回值从「无信封对象」变成 `data` 而编译失败的调用**

典型：`create_subscribe` 之后 `row["id"]` 仍对；若有测试读 `create_site()["id"]` 而旧响应把 id 放根上，现在也在 `data` 里，返回值已是 data，字段路径不变。

- [x] **Step 3: 跑**

```
cargo test -p api --test management create_site -- --nocapture
cargo test -p api --test management sites
cargo test -p api --test jellyfin
cargo test -p api --test jellyfin_images
cargo test -p api --test jellyfin_episode_artwork
```

Expected: management 里依赖 create_* 的先绿或本 Task 修绿。Jellyfin 三套 **必须已经绿**（基线）。把 Jellyfin 命令写进本计划后续每个 Task 的「跑测试」——红了就是回归。

- [x] **Step 4: Commit**

---

## Wave 1 — 按资源迁测试（先测后拆）

每一 Task：该文件内 **所有** legacy 根路径改完再 commit。不要一个文件留一半 `/subscribes`。

断言迁移口诀：

```rust
// 旧
let body = json_body(response).await;
assert_eq!(body["id"], ...);
assert_eq!(body.as_array().unwrap().len(), 1);

// 新
assert_eq!(response.status(), StatusCode::CREATED); // 或 OK
let data = json_data(response).await;
assert_eq!(data["id"], ...);
assert_eq!(data.as_array().unwrap().len(), 1);
```

列表：v1 `GET /subscriptions` 的 `data` 是 array，元素有 `media.title` 不是根上 `title`。`list_subscribes_includes_media_title_and_tv_coverage` 这类测试要改成 `row["media"]["title"]`。

### Task 2: 订阅测试迁 v1

**Files:**
- Modify: `crates/api/tests/management/subscribes.rs`
- Modify: `crates/api/tests/management/http.rs`（`/subscribes` 那几条）
- Modify: `crates/api/tests/management/open_coverage.rs`
- Modify: `crates/api/tests/management/subscription_pause.rs`
- Modify: `crates/api/tests/management/subscribe_atomicity.rs`
- Modify: `crates/api/tests/management/coverage_validation.rs`（`("/subscribes", body)` 改 v1）
- Modify: `crates/api/tests/management/wash_cut.rs`（创建订阅的路径）
- Modify: `crates/api/tests/management/delivery.rs`（`/subscribes`）
- Modify: `crates/api/tests/management/catalog.rs`（文件里混了 `/subscribes`+`/filters`，本 Task 只改订阅；filters 留 Task 3）
- Modify: `crates/api/tests/management/jobs.rs`、`catalog_refresh.rs`、`transfer_isolation.rs` 里的创建订阅调用
- Modify: `crates/api/tests/users.rs` 的 `/subscribes` 与 `/users`

**Interfaces:**
- Consumes: Task 1 的 `json_data` / `create_subscribe`
- Produces: 这些文件不再出现 `"/subscribes"` 字符串（除注释）

`POST /subscribes/{id}/run`（`legacy_run_parity.rs`）改 `POST /api/v1/subscriptions/{id}/search`。v1 search 返回信封；断言 status 200 + `data` 存在即可，不要假设旧 run 的裸字段。

`users.rs`：`POST /users` `{login, token}` → `POST /api/v1/users` `{login, password}`；列表读 `json_data` 的 array。隔离测试改 `GET /api/v1/subscriptions`。

- [x] **Step 1:** `rg '"/subscribes' crates/api/tests` 当 checklist
- [x] **Step 2:** 改路径 + `json_data` + `media.title`
- [x] **Step 3:**

```
cargo test -p api --test management subscribes
cargo test -p api --test management open_coverage
cargo test -p api --test management subscription_pause
cargo test -p api --test management subscribe_atomicity
cargo test -p api --test management coverage_validation
cargo test -p api --test users
cargo test -p api --test jellyfin
```

Expected: PASS。

- [x] **Step 4: Commit**

```bash
git commit -m "$(cat <<'EOF'
test(api): point subscribe tests at /api/v1 envelopes
EOF
)"
```

### Task 3: Filter / 站点 / 下载器 / 目录测试迁 v1

**Files:**
- Modify: `filters.rs`、`default_filter_required.rs`、`catalog.rs` 剩余 `/filters`
- Modify: `sites.rs`、`check_in.rs`（`POST /sites`）
- Modify: `downloaders.rs`
- Modify: `directory.rs`（若有根路径）
- Test: `cargo test -p api --test management filters sites downloaders check_in directory`

路径：`/filters` → `/api/v1/rule-sets`；默认 `PUT /api/v1/rule-sets/default`；站点 `/api/v1/sites`；下载器 `/api/v1/downloaders`。

旧 `GET /filters` 可能是裸 array；v1 `data` 是 array，元素 id 在 `id`。

- [x] **Step 1–4:** 同 Task 2 模式。Commit：`test(api): point filter/site/downloader tests at /api/v1`

### Task 4: jobs / ledger / unidentified / search / catalog / downloads 迁 v1

**Files:**
- Modify: `jobs.rs`、`job_scope.rs`、`unidentified.rs`、`watch_intake.rs`、`watch_scrape.rs`、`library_delete.rs`、`library_features.rs`（`/ledger`）、`run.rs`、`downloads.rs`、`search_scope.rs`（若有根 `/search`）、`http.rs` 剩余 `/ledger`
- Modify: `authz.rs`：`POST /users` 那条改为断言 **404**（或删掉，因为本 Task 尚未拆 router——**先改成 v1 的 403 对照已存在**；legacy `POST /users` 保留到 Task 6 再改成 404）
- Modify: `catalog.rs` 的 `/catalog/search` 若有

`GET /jobs` → `/api/v1/jobs`（v1 可能是 `{items,...}` 或 array，以 handler 为准，测试跟 `json_data`）。
`GET /ledger` → `/api/v1/ledger`。
`GET /unidentified` → `/api/v1/unidentified`。
`GET /downloads` → `/api/v1/downloaders/tasks`，读 `data.items`。
`GET /search` → `/api/v1/search/torrents?keyword=`。

`authz.rs:189` `GET /ledger` 成员 403：改 `GET /api/v1/ledger` 仍应 403（v1 已是 admin）。不要在本 Task 拆 router。

- [x] **Step 1–4:** Commit：`test(api): point jobs/ledger/search tests at /api/v1`

### Task 5: `rg` 清零 legacy 管理路径（测试侧）

**Files:** 全 `crates/api/tests`

跑：

```bash
rg -n '"/(subscribes|filters|search|users|sites|downloaders|jobs|ledger|unidentified|catalog|downloads|directory)' crates/api/tests --glob '!jellyfin*'
```

允许留下的：

- 注释
- Jellyfin：`/Users` `/Items` `/Videos` `/Sessions` `/System` `/emby`（注意大小写）
- `authz.rs` 里若仍测 `POST /users` 403，本 Task 改成「拆 router 后变 404」的预告注释，或挪到 Task 6

CLI 测试（`cli.rs`）应已打 `/api/v1`；若还有根路径一并改。

- [x] **Step 1:** `rg` 输出必须只剩 Jellyfin 与（可选）「即将 404」的断言草稿
- [x] **Step 2:**

```
cargo test -p api --test management
cargo test -p api --test users
cargo test -p api --test jellyfin
cargo test -p api --test jellyfin_images
cargo test -p api --test jellyfin_episode_artwork
```

Expected: 全绿。此时 **生产 router 仍 merge legacy**，但测试已不依赖它。

- [x] **Step 3: Commit** `test(api): finish migrating management tests off legacy root paths`

---

## Wave 2 — 拆 router + 证明 404 + Jellyfin 回归

### Task 6: 去掉 legacy merge，根上的旧管理路径 404

**Files:**
- Modify: `crates/api/src/management/mod.rs`（删 `legacy_admin`/`protected` 的管理 route 与 `.merge(protected)`）
- Modify: `crates/api/tests/management/authz.rs`（`POST /users` 期望从 403 改为 **404**；`GET /ledger` 已是 v1）
- Create or Modify: `crates/api/tests/management/legacy_gone.rs`（新文件，在 `tests/management/main.rs` `mod legacy_gone;`）
- Modify: `legacy_run_parity.rs`：断言 `POST /subscribes/{id}/run` → 404
- Delete routes only：`management/search.rs` 等若已无引用则删模块；`users.rs` 的 `list_subscribes`/`create_user` 仅被 legacy 用则删函数（v1 有自己的 `http/users.rs`）

**保留：**

```rust
Router::new()
    .nest("/api/v1", crate::http::router(state))
    .merge(jellyfin)
    .layer(crate::ui::cors_layer_from_env())
```

`jellyfin` 仍是 `media_server::routes` + `media_routes`（含 `/emby` nest）。不要动 `media-server` crate 除非测试红。

- [x] **Step 1: 失败测试（在仍 merge 时先提交测试会绿——所以本 Task 先写 404 测试，立刻拆 router）**

`legacy_gone.rs`：

```rust
#[tokio::test]
async fn legacy_management_paths_are_gone() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures { requests: Mutex::new(Vec::new()), bodies: HashMap::new() }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    let token = Some("management-secret");
    for uri in [
        "/subscribes",
        "/filters",
        "/search",
        "/users",
        "/sites",
        "/downloaders",
        "/jobs",
        "/ledger",
        "/unidentified",
        "/catalog/search",
        "/downloads",
        "/directory",
    ] {
        let response = app
            .clone()
            .oneshot(request("GET", uri, token, Value::Null))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "GET {uri}");
    }
}

#[tokio::test]
async fn jellyfin_public_ping_still_works() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures { requests: Mutex::new(Vec::new()), bodies: HashMap::new() }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    let response = app
        .oneshot(request("GET", "/System/Ping", None, Value::Null))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
```

`GET /users` 在 Jellyfin 里 **不是** 列表用户的官方常用入口（官方是 `AuthenticateByName` + `/Users/Me`）。若 `GET /users` 被 Jellyfin 路由接住变成 401/200 而不是 404，**不要为了 404 去拆 Jellyfin**。把该 URI 从「必须 404」列表拿掉，并加注释：大小写 `/Users` 是协议，`/users` 若被 axum 大小写不敏感匹配到 Jellyfin，以协议测试为准。

先确认：axum 默认大小写敏感，`/users` ≠ `/Users`。`GET /users` 应 404，`GET /Users/Me` 走 Jellyfin。把两条都写进测试。

- [x] **Step 2: 拆 `.merge(protected)`**，删只服务 legacy 的 handler
- [x] **Step 3: 修编译**（未使用的 `create_user` in `users.rs` 等）
- [x] **Step 4: 跑**

```
cargo test -p api --test management
cargo test -p api --test users
cargo test -p api --test ui
cargo test -p api --test jellyfin
cargo test -p api --test jellyfin_images
cargo test -p api --test jellyfin_episode_artwork
cargo test --workspace
```

Expected: management 全绿；Jellyfin 全绿。若 Jellyfin 红：**停在本 Task 修** `media_server_provider` / 路由冲突，禁止用「把 legacy 加回去」过测试。

常见冲突：axum 先匹配 `/users` vs `/Users`。保持大小写敏感即可。不要加 fallback 把 `/users` 指到 Jellyfin。

- [x] **Step 5: Commit**

```bash
git commit -m "$(cat <<'EOF'
refactor(api): drop legacy management HTTP surface

Self-hosted API is /api/v1 only. Jellyfin/Emby official routes stay.
EOF
)"
```

### Task 7: Jellyfin 协议冒烟（本计划内的功能检测）

**Files:** 只在 Task 6 的 Jellyfin 测试红了才改 `crates/api/src/media_server_provider*.rs` 或 `crates/media-server/src/routes.rs`

**Interfaces:**
- Consumes: Task 6 后的 Router = v1 nest + jellyfin merge
- Produces: 官方路径行为与拆之前一致

最低断言（已有测试覆盖则不必新写，跑过即可）：

| 协议行为 | 测试位置（现有） |
|---|---|
| `POST /Users/AuthenticateByName` 发 token | `jellyfin.rs` / `protocol.rs` |
| `GET /Items` 列出库台账 | `views_and_items_list_library_ledger` |
| `GET /Videos/{id}/stream` Range | `playback.rs` |
| `/emby` 前缀同一套 | `protocol::emby_prefix_*` |
| compact id（无连字符）解析 | playback / items |
| `GET /System/Ping` 公开 | `protocol::ping_is_public_*` |
| 用户隔离 | `user_isolation.rs` / visibility |

跑：

```
cargo test -p api --test jellyfin
cargo test -p api --test jellyfin_images
cargo test -p api --test jellyfin_episode_artwork
```

若全绿：本 Task 只在计划文档打勾，不必空 commit。若红：最小修复 + commit `fix(jellyfin): keep official protocol after dropping legacy management routes`。

- [x] **Step 1:** 跑上述命令
- [x] **Step 2:** 红则修 provider/路由冲突（禁止恢复 `/subscribes`）
- [x] **Step 3:** 再跑到绿
- [x] **Step 4:** 需要时 commit

---

## Wave 3 — 文档

### Task 8: 文档与计划状态

**Files:**
- Modify: `docs/superpowers/plans/2026-09-26-api-crate-correctness.md` Task 8 状态改为「已由 unify-v1-drop-legacy 完成」
- Modify: `docs/api-contracts/self.md` 若仍暗示根路径管理面，加一句「管理面只有 `/api/v1`；根路径是 Jellyfin」
- Modify: `AGENTS.md` Docs map 已指向 `http/mod.rs`，确认无「/subscribes 仍可用」

不要恢复路由表。

- [x] **Step 1:** 改文档
- [x] **Step 2: Commit** `docs: record legacy management HTTP removal`

---

## 执行顺序（必须）

```
Task 1 → 2 → 3 → 4 → 5 → 6 → 7 → 8
```

5 是拆 router 的门禁：`rg` 测试侧清零之前不要做 6。6 之后立刻 7（Jellyfin）。前端不在主路径上。

每个 Task 结束：该 filter 的 `cargo test -p api --test management <filter>` + **Jellyfin 三套至少在 Task 1 和 Task 6/7 跑全**。Task 6 结束必须 `cargo test --workspace`。

## Jellyfin 安全规则（每个 Task 都适用）

1. 不改 `media-server` 路由表来「帮忙」迁测试。
2. 不把 `/users`（小写）做成 Jellyfin `/Users` 的别名。
3. compact id 与 hyphenated UUID：Jellyfin 继续 compact；自有 API 继续带连字符。
4. 若拆 merge 后某个自有测试误打到 `/Users` 混进 Jellyfin：改测试去打 `/api/v1/users`，不要改协议。

## Spec coverage

| 要求 | Task |
|---|---|
| 测试不再打 legacy 管理路径 | 2–5 |
| `json_data` 信封 | 1 |
| 拆 `.merge(protected)` | 6 |
| `GET /subscribes` 等 404 | 6 |
| Jellyfin 官方路径仍可用 | 1 基线、6–7 回归 |
| 文档 | 8 |
| 发现墙 Err→Null | 不做（另一计划） |
| 双信封兼容层 | 禁止 |

## Placeholder scan

无 TBD。`GET /users` vs `/Users` 的 404 判定写在 Task 6：大小写敏感，小写 404，大写走 Jellyfin。`create_subscribe` 的 `filter` 嵌套对象在 Task 1 改为 `filter_id` + 种子默认规则组。
