# v1 信封断言与 resource.action 错误码 Implementation Plan

> **历史状态（2026-10-13 整理）：** v1 错误信封历史补强；原 Task 及测试结果保留，本次未重新核对每条错误码/公开接缝。
>
> 本文保留原设计、发现/任务 ID 与当时证据；日期本身不表示过时。旧执行/提交/清理命令仅作历史记录，不应直接重跑；复选框不是当前实现清单。当前依据：[数据库/API review](<../../audits/2026-10-13-database-api-review.md>) 与 [剩余计划](<2026-10-13-database-api-followup.md>)，未覆盖的旧 ID 仍须单独对账。

> **规模规则更新：** 文中的 60 行硬限/整改目标是历史要求；当前 [AGENTS.md](<../../../AGENTS.md>) 为生产函数软限 60、测试软限 80、两者硬限 120。历史测试/行数记录不改写，规模合规不等于功能安全。

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (this session) or superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 给仍走 `ApiError` 的 v1 路径锁住 `{ok:false,error:{code,message}}`，把泛化 code 改成与 `http::err` 一致的 `resource.action`，并拆开超限的 `def_json_with_store`。不拆 `/api/v1/subscriptions/{id}/run`。

**Architecture:** 先写失败测试（admit / catalog cache / run 的信封），再给 `ApiError` 加上按资源的构造函数并改调用点，最后把 jobs JSON 的 status 解析抽成命名 helper。测试只打公开 HTTP 缝，不改 Jellyfin 路径。

**Tech Stack:** axum `/api/v1`、`{ok, data}` / `{ok, error:{code,message}}`、tower oneshot 集成测试、`crates/api/tests/management/common.rs` 的 `json_data` / `json_body`。

## Global Constraints

- 领域词：`Subscribe`、`Filter`、`Library`、`Torrent`、`Wash-cut`（代码 `wash_cut`）。
- 自有 API 只走 `/api/v1`。成功 `{ok:true,data}`；失败 `{ok:false,error:{code,message}}`，HTTP 4xx/5xx。
- 错误 code：`resource.action`，如 `subscribe.missing`、`site.invalid`。已有 `http::err` 的 code（`subscription.paused`、`catalog.invalid`、`site.missing`）优先复用，不要发明第二套。
- Jellyfin/Emby（`/Users`、`/Items`、`/Videos/{id}/stream`、`/emby`）不改。
- 不要发明 `POST /users/{id}/reset-password` 或 `/tracking-state`。
- 静态图片保持 public。
- IO/parse 路径禁止 `unwrap`/`expect`（测试除外）。
- 一次一个 Task，测绿再 commit。不要 GitHub issue。
- 发现墙 `row()` Err→Null、密码哈希、Store 外迁、拆 `catalog.rs` / `subscription_depth.rs`：**本计划不做**。

## Why this plan exists

unify-v1 已拆根路径管理面。复查 `144560b...c22a17e` 后路径收口已绿，还剩：

1. admit / catalog cache / run 的失败路径只断言 HTTP status，没有锁住信封。
2. `ApiError` 的 code 是 `resource.missing` / `request.invalid` / `internal.error`，和 `http::err("subscribe.missing")` 不一致。
3. `def_json_with_store` 83 行，超过 `AGENTS.md` 函数硬限 60。

`POST /api/v1/subscriptions/{id}/run` 仍是测试用的同步接缝（`run.rs`、`wash_cut.rs`、`delivery.rs`、`legacy_run_parity.rs`）。产品入口是 `POST /api/v1/subscriptions/{id}/search`（入队）。**本计划保留 `/run`**，只修它的信封和 code。

## Out of scope

- 删除或把 `/run` 改成 `/search` 的别名。
- 把 `crates/api/src/directory.rs` / `catalog.rs` 里未挂路由的旧 handler 整文件删掉（碰到 `ApiError` 调用再改 code；删死代码另开计划）。
- 拆 `tests/management/catalog.rs`（1156 行）和 `subscription_depth.rs`（1857 行）。
- 给所有 `http::err` 调用点重写 code。

## File map

| File | Responsibility |
|---|---|
| `crates/api/tests/management/common.rs` | 可选：`json_error` helper，失败体断言 `ok=false` 并返回 `error` 对象 |
| `crates/api/tests/management/admit.rs` | 成功走 `json_data`；拒绝走信封 + `subscribe.rejected` |
| `crates/api/tests/management/catalog_cache.rs` | 缺 `cache_key` 的 DELETE 断言 `catalog.invalid` |
| `crates/api/tests/management/run.rs` | 全站失败断言 `subscribe.run_failed` 信封；成功继续 `json_data` |
| `crates/api/src/management/error.rs` | `ApiError` 增加带 code 的构造；默认构造映射到 `resource.action` |
| `crates/api/src/admit.rs` | Subscribe/Filter/Torrent 缺失与拒绝用具体 code |
| `crates/api/src/catalog_cache.rs` | 缺 `cache_key` → `catalog.invalid` |
| `crates/api/src/management/subscribes/run.rs` | 缺 Subscribe → `subscribe.missing`；全站失败 → `subscribe.run_failed` |
| `crates/api/src/claim.rs` | Unidentified 缺失 → `unidentified.missing` / `unidentified.invalid` |
| `crates/api/src/catalog.rs` | `require_tv_season` 等仍返回 `ApiError` 的路径改 code |
| `crates/api/src/http/jobs.rs` | 从 `def_json_with_store` 抽出 `job_last_status` / `job_friendly_name` |

---

## Wave 1 — 先锁信封（测试先行）

### Task 1: 失败信封 helper + admit / catalog cache / run 断言

**Files:**
- Modify: `crates/api/tests/management/common.rs`
- Modify: `crates/api/tests/management/admit.rs`
- Modify: `crates/api/tests/management/catalog_cache.rs`
- Modify: `crates/api/tests/management/run.rs`
- Test: `cargo test -p api --test management -- admit catalog_cache legacy_run_reports_failure`

**Interfaces:**
- Consumes: 现有 `json_body` / `json_data`；v1 `POST /api/v1/search/admit`、`DELETE /api/v1/catalog/cache`、`POST /api/v1/subscriptions/{id}/run`
- Produces: `json_error`；admit 成功读 `data.title`；失败读 `error.code`

在 `common.rs` 的 `json_data` 旁增加：

```rust
/// 自有 API 失败体。非 `{ok:false, error:{code,message}}` 时 panic。
pub(crate) async fn json_error(response: axum::response::Response) -> Value {
    let status = response.status();
    let body = json_body(response).await;
    assert_eq!(
        body["ok"], false,
        "expected v1 envelope ok=false, status={status} body={body}"
    );
    let error = body.get("error").cloned().unwrap_or(Value::Null);
    assert!(error.get("code").and_then(Value::as_str).is_some(), "missing error.code: {body}");
    assert!(
        error.get("message").and_then(Value::as_str).is_some(),
        "missing error.message: {body}"
    );
    error
}
```

admit 成功（`admit_chosen_torrent_adds_to_downloader_and_downloads`）在 `assert_eq!(admitted.status(), StatusCode::OK)` 后加：

```rust
let admitted_data = json_data(admitted).await;
assert_eq!(admitted_data["title"], "The.Matrix.1999.2160p.BluRay.x265-GROUP");
```

注意：`json_data` 会消费 response body，所以 **先** `json_data`，再用 downloader 断言；不要对同一个 `admitted` 既 `json_data` 又事后读 status（status 可在 `json_data` 前读）。

`admit_rejects_torrent_that_fails_filter` 在 status 400 后加：

```rust
let error = json_error(rejected).await;
assert_eq!(error["code"], "subscribe.rejected");
```

`catalog_cache_rejects_ambiguous_source_id_deletion` 在 status 400 后加：

```rust
let error = json_error(response).await;
assert_eq!(error["code"], "catalog.invalid");
```

`legacy_run_reports_failure_when_every_site_search_fails` 把只断言 `is_server_error()` 改成：

```rust
assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
let error = json_error(response).await;
assert_eq!(error["code"], "subscribe.run_failed");
assert!(error["message"].as_str().unwrap().contains("failure"));
```

本 Task **先提交会红的测试**。现有 `ApiError` 已有信封结构，所以 `json_error` 的 `ok=false` / `error.code` 存在性现在就会绿；**会红的是具体 code**（现在是 `request.invalid` / `internal.error`，不是 `subscribe.rejected` / `catalog.invalid` / `subscribe.run_failed`）。这是故意的：Task 2 改 code 才绿。

- [x] **Step 1:** 加 `json_error`，改三条测试的信封断言
- [x] **Step 2:** 跑

```
cargo test -p api --test management -- admit_rejects_torrent_that_fails_filter catalog_cache_rejects_ambiguous_source_id_deletion legacy_run_reports_failure_when_every_site_search_fails
```

Expected: FAIL，`error.code` 仍是 `request.invalid` 或 `internal.error`。

- [x] **Step 3:** 不要在本 Task 改生产 code。Commit 测试：

```bash
git commit -m "$(cat <<'EOF'
test(api): assert v1 error envelopes on admit, catalog cache, and subscribe run
EOF
)"
```

---

## Wave 2 — resource.action codes

### Task 2: `ApiError` 按资源构造 + 改 v1 调用点

**Files:**
- Modify: `crates/api/src/management/error.rs`
- Modify: `crates/api/src/admit.rs`
- Modify: `crates/api/src/catalog_cache.rs`
- Modify: `crates/api/src/management/subscribes/run.rs`
- Modify: `crates/api/src/claim.rs`
- Modify: `crates/api/src/catalog.rs`（仅 `require_tv_season` / 仍返回 `ApiError` 的路径）
- Modify: `crates/api/src/directory.rs`（仅仍编译的 `ApiError` 调用；这些 handler 未挂路由，改 code 防止以后误挂）
- Modify: `crates/api/src/management/filters.rs`
- Test: Task 1 的三条 + `cargo test -p api --test management -- admit catalog_cache run claim unidentified`

**Interfaces:**
- Consumes: Task 1 的期望 code
- Produces: `ApiError` 带 `resource.action`；`IntoResponse` 形状不变

把 `error.rs` 的无 code 构造改成带 code 的（保留短名字给 `store` / `internal`）：

```rust
impl ApiError {
    pub(crate) fn with(status: StatusCode, code: &'static str, message: String) -> Self {
        Self { status, code, message }
    }

    pub(crate) fn internal(message: String) -> Self {
        Self::with(StatusCode::INTERNAL_SERVER_ERROR, "internal.error", message)
    }

    pub(crate) fn invalid(code: &'static str, message: String) -> Self {
        Self::with(StatusCode::BAD_REQUEST, code, message)
    }

    pub(crate) fn missing(code: &'static str, message: String) -> Self {
        Self::with(StatusCode::NOT_FOUND, code, message)
    }
}
```

删掉无 code 的 `bad_request(message)` / `not_found(message)`，让编译器列出调用点。不要留这两个函数再内部映射到 `request.invalid`——那会让 Task 1 继续红。

调用点对照（message 可保持英文，本计划不强制中文化）：

| 现场 | code |
|---|---|
| invalid Subscribe id | `subscribe.invalid` |
| Subscribe not found | `subscribe.missing` |
| Media not found | `media.missing` |
| Filter not found | `filter.missing` |
| Torrent not found | `torrent.missing` |
| Torrent rejected by Filter | `subscribe.rejected` |
| delivery/submit 失败 | `subscribe.invalid` |
| cache_key is required | `catalog.invalid` |
| run 全站失败 `ApiError::internal(...)` | `subscribe.run_failed`（新增 `fn run_failed(message: String)` 或 `with(INTERNAL_SERVER_ERROR, "subscribe.run_failed", message)`） |
| title is required / kind / file missing | `unidentified.invalid` |
| Unidentified not found | `unidentified.missing` |
| TV Subscribe requires season | `subscribe.invalid` |
| invalid default Filter id | `filter.invalid` |
| directory 根路径/kind/默认根 | `directory.invalid` / `directory.root_missing` / `directory.protected` |
| catalog search 两边都失败 / invalid kind | `catalog.invalid` |

`From<StoreError>` 保持 `store.error`。`From<SubscribeError>` 保持 `upstream.subscribe_error`（502）。不要把 502 改成 500。

`run.rs` 里全站失败现在走 `ApiError::internal`。改成：

```rust
return Err(ApiError::with(
    StatusCode::INTERNAL_SERVER_ERROR,
    "subscribe.run_failed",
    format!("{} subscription processing failure(s): {}", run_errors.len(), run_errors.join("; ")),
));
```

`catalog.rs` 的 `search_catalog` / `discover_catalog` **未挂在** `http::router` 上。仍要改它们的 `ApiError::bad_request`，否则删掉旧构造后编不过。不要把它们重新挂到根路径。

- [x] **Step 1:** 改 `error.rs`，按编译器清单改调用点
- [x] **Step 2:** 跑

```
cargo test -p api --test management -- admit catalog_cache run unidentified claim
cargo test -p api --test jellyfin
```

Expected: Task 1 的 code 断言 PASS；Jellyfin 仍绿。

- [x] **Step 3: Commit**

```bash
git commit -m "$(cat <<'EOF'
fix(api): use resource.action codes on ApiError
EOF
)"
```

---

## Wave 3 — 超限函数

### Task 3: 拆 `def_json_with_store` 到 60 行以下

**Files:**
- Modify: `crates/api/src/http/jobs.rs`
- Test: `cargo test -p api --test management -- jobs active_only jobs_list`

**Interfaces:**
- Consumes: 现有 `last_status` 语义（Running 或 `run_after == 0` 用 live；否则用 last）
- Produces: 同 JSON 字段；函数 ≤ 60 行

不要改 JSON 形状。抽出两个 private helper 到同一文件：

```rust
fn job_media_title(payload_val: &Value, store: &crate::Store) -> Option<String> { /* 现有 subscribe_id / media_id 查找 */ }

fn job_friendly_name(name: &str, media_title: Option<&str>) -> String { /* search/catalog 前缀 + RSS/Transfer/... match */ }

fn job_last_status<'a>(live: Option<&'a jobs::Job>, last: Option<&'a jobs::Job>) -> Option<&'a str> {
    match (live, last) {
        (Some(l), _) if l.status == jobs::JobStatus::Running || l.run_after == 0 => {
            Some(l.status.as_str())
        }
        (_, Some(r)) => Some(r.status.as_str()),
        (Some(l), None) => Some(l.status.as_str()),
        (None, None) => None,
    }
}
```

`def_json_with_store` 只组 `json!({...})`。`wc -l` 该函数（含签名到闭合 `}`）必须 ≤ 60。

- [x] **Step 1:** 抽 helper，不改测试
- [x] **Step 2:** `wc -l` 确认函数体；跑

```
cargo test -p api --test management -- jobs
```

Expected: PASS，含 `active_only_returns_only_defs_with_live_children` 与 `jobs_list_includes_schedule_and_run_times`。

- [x] **Step 3: Commit**

```bash
git commit -m "$(cat <<'EOF'
refactor(api): split job list JSON helpers under the 60-line limit
EOF
)"
```

---

## 执行顺序（必须）

```
Task 1 → 2 → 3
```

Task 1 的失败测试在 Task 2 之前提交。Task 3 不依赖新 code 字符串，但放最后以免和信封 diff 搅在一起。

Task 2 结束必须：

```
cargo test -p api --test management
cargo test -p api --test jellyfin
cargo test -p api --test jellyfin_images
cargo test -p api --test jellyfin_episode_artwork
```

Task 3 结束必须 `cargo test -p api --test management jobs`；若只碰了 `jobs.rs`，不必 workspace。整个计划结束跑一次 `cargo test --workspace`。

## Jellyfin 安全规则

1. 不改 `media-server` 路由表。
2. 不把 `/users` 做成 `/Users` 别名。
3. 不把 `/run` 挂到根路径。

## Spec coverage

| 复查项 | Task |
|---|---|
| admit 失败/成功锁信封 | 1 |
| catalog cache 缺 key 锁信封 | 1 |
| run 全站失败锁信封 | 1 |
| `resource.action` codes（`subscribe.missing` 等） | 2 |
| `def_json_with_store` ≤ 60 | 3 |
| 保留 `/api/v1/subscriptions/{id}/run` | 全部（不删） |
| 发现墙 Err→Null | 不做 |
| 拆 catalog.rs / subscription_depth.rs | 不做 |

## Placeholder scan

无 TBD。`json_error` 的签名和三条期望 code 写在 Task 1。`ApiError::invalid` / `missing` / `with` 写在 Task 2。jobs helper 名字写在 Task 3。
