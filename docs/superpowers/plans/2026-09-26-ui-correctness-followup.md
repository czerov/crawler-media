# UI / API 正确性收口 Implementation Plan

> **历史状态（2026-10-13 整理）：** UI/API 契约历史整改；legacy、筛选发现、订阅选库等范围以原 Task 为准，未逐项重核完成状态。
>
> 本文保留原设计、发现/任务 ID 与当时证据；日期本身不表示过时。旧执行/提交/清理命令仅作历史记录，不应直接重跑；复选框不是当前实现清单。当前依据：[数据库/API review](<../../audits/2026-10-13-database-api-review.md>) 与 [剩余计划](<2026-10-13-database-api-followup.md>)，未覆盖的旧 ID 仍须单独对账。

> **规模规则更新：** 文中的 60 行硬限/整改目标是历史要求；当前 [AGENTS.md](<../../../AGENTS.md>) 为生产函数软限 60、测试软限 80、两者硬限 120。历史测试/行数记录不改写，规模合规不等于功能安全。

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修掉 review + 浏览器实点里确认的数据丢失、发现筛选 502、订阅换库空操作和网络设置死链；顺手收掉 Task 7 残留兼容层与 compact 合集封面。不拆 legacy 根路由，不碰 Jellyfin。

**Architecture:** 公开缝仍是 `/api/v1` handler 与 `web/lib/api`。每一刀先写失败测试（Rust oneshot 或 `web/test` 纯函数），再改最小实现。fanout 把「不支持筛选」从致命错误里摘出去；缺失记录按 `media_item_id` 收窄；`PATCH /subscriptions/{id}` 真正写入 `library_id`。

**Tech Stack:** Rust axum `/api/v1`、`FanoutCatalog`、rusqlite `Store`、React SPA `web/lib/api` + 成员/库/发现/订阅页。

## Global Constraints

- 领域词：`Subscribe`、`Filter`、`Library`、`Torrent`、`Wash-cut`（代码 `wash_cut`）。不要发明 `SearchResult` / `MediaMeta`。
- Jellyfin / Emby 路由、compact id、`/Videos/*/stream` **一律不动**。
- Task 8 legacy 根路由（`/subscribes` `/filters` `/search`）**本计划不拆**。
- 不新增 `POST /users/{id}/reset-password`、不恢复 `/tracking-state` 专用端点。
- IO / parse 路径禁止 `unwrap` / `expect`（测试除外）。
- 静态图片路由保持 public（`AGENTS.md`）。
- 前端相对路径走 `VITE_API_BASE_URL` 默认 `/api/v1`。
- 一次只做一个 Task，测绿再 commit。不要 GitHub issue。

## Out of scope this plan

- 拆 `management::router` 的 legacy merge（Task 8 冻结）。
- 用户密码哈希、`Store` 整包搬迁、playback 抽回 `playback` crate。
- 库管理页状态列恒空闲（`GET /libraries` 本来就不下发扫描进度，不是这轮回归）。
- `users.role` 非法值静默变 Member、密码生成器去重（非用户可见故障）。
- 给 `GET /discover/{kind}/filtered` 加 `providers[]` 数组（现契约只有 `items`；本计划用 200 空列表 vs 真 502 区分）。

## File map

| File | Responsibility |
|---|---|
| `crates/api/src/catalog.rs` | `filtered_*` 默认 `Err("该数据源不支持筛选发现")`（已有，保持） |
| `crates/api/src/catalog_fanout.rs` | `merge`：跳过 unsupported，空结果不再被 Bangumi/AniList 带成 502 |
| `crates/api/src/http/discover.rs` | 注释对齐；filtered 仍 502 真上游失败 |
| `crates/api/tests/management/catalog.rs` | 现有 EmptyCatalog 502 测保留或按 Task 1 调整；加 fanout 空结果 200 |
| `crates/api/src/store/library.rs` | `delete_missing_rows_for_media` |
| `crates/api/src/http/library.rs` | `DELETE /libraries/{id}/missing-rows?media_item_id=` |
| `web/lib/api/libraries.ts` | `clearMissingLibraryRecords` 带上 `media_item_id` |
| `web/components/discover-view.tsx` | 「前往网络设置」改到 `/settings/metadata#proxy` |
| `web/components/metadata-settings-section.tsx` | 代理区块 `id="proxy"` |
| `crates/api/src/http/subscriptions/types.rs` | `PatchSubscriptionInput.library_id: Option<Option<String>>` |
| `crates/api/src/http/subscriptions/patch_fields.rs` | 写入 / 清空 `Subscribe.library_id`，校验库存在且 kind 匹配 |
| `web/lib/api/subscriptions.ts` | `Subscription.library_id: string \| null` |
| `web/components/library-view.tsx` | `effectiveLibraryId` 尊重 `sub.library_id` |
| `crates/api/src/cli/{actions,catalog,filters,lists}.rs` | 去掉 v1 信封之外的旧字段回退 |
| `crates/api/src/store/collections.rs` | `collection_cover` 改 `artwork_url` 或删除死方法 |
| `web/lib/library-routing-warnings.ts` | 规则值集合相交即提示，不只全等 JSON |

---

## Wave 1 — 用户可见故障（先做）

### Task 1: fanout 筛选空结果不得 502

**Files:**
- Modify: `crates/api/src/catalog_fanout.rs`（`merge`）
- Modify: `crates/api/src/http/discover.rs:518-520` 注释（「other sources return empty」已过时）
- Test: `crates/api/tests/management/catalog.rs`（现有 `discover_filtered_reports_unsupported_source`）+ 同文件新增 fanout 测

**Interfaces:**
- Consumes: `Catalog::filtered_movie/tv` 默认 `Err("该数据源不支持筛选发现")`；`FanoutCatalog` 会把 TMDB/Douban/Bangumi/AniList 全 merge
- Produces: `merge` 跳过「不支持」类错误；只有**支持该能力的源**全部失败（且 hits 为空）才 `Err`

实测回归：`GET /api/v1/discover/movie/filtered?year=1800` 在生产 fanout 上返回 502 `该数据源不支持筛选发现`，因为 TMDB `Ok([])` + Bangumi/AniList `Err(不支持)`，`merge` 在 `hits.is_empty()` 时把 last_err 上抛。

规则锁定：

```text
对每个源的 Result:
  Ok(rows)           → 并入 hits
  Err(msg) 且 msg 含 "不支持" → 忽略（该源无此能力）
  Err(其它)          → 记下 last_real_err

返回:
  hits 非空          → Ok(hits)          // 即使有真实错误
  hits 空 + 有真实错误 → Err(真实错误)    // 真上游挂了
  hits 空 + 仅不支持  → Ok([])           // 没有能筛的源，或能筛的源就是没片
```

不要改 Bangumi/AniList 去实现假筛选。不要改 Jellyfin。

- [ ] **Step 1: 写失败测试（fanout：支持源空列表 + 不支持源 Err → 200 空 items）**

在 `crates/api/tests/management/catalog.rs` 追加（可复用同文件 `FakeCatalog` / `hit`；再做一个只返回 `Err("该数据源不支持筛选发现")` 的源）：

```rust
struct UnsupportedCatalog;

impl Catalog for UnsupportedCatalog {
    fn source_name(&self) -> &'static str { "bangumi" }
    fn filtered_movie(&self, _query: &str) -> Result<Vec<CatalogHit>, String> {
        Err("该数据源不支持筛选发现".into())
    }
    fn filtered_tv(&self, _query: &str) -> Result<Vec<CatalogHit>, String> {
        Err("该数据源不支持筛选发现".into())
    }
    fn details(&self, _kind: MediaKind, _id: &str) -> Result<Option<Media>, String> {
        Ok(None)
    }
}

struct EmptyTmdb;

impl Catalog for EmptyTmdb {
    fn source_name(&self) -> &'static str { "tmdb" }
    fn filtered_movie(&self, _query: &str) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn filtered_tv(&self, _query: &str) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn details(&self, _kind: MediaKind, _id: &str) -> Result<Option<Media>, String> {
        Ok(None)
    }
}

#[tokio::test]
async fn discover_filtered_empty_tmdb_does_not_fail_because_bangumi_unsupported() {
    let tmp = tempfile::tempdir().unwrap();
    let fanout = api::catalog::FanoutCatalog::new(vec![
        Arc::new(EmptyTmdb),
        Arc::new(UnsupportedCatalog),
    ]);
    let app = router(
        state(
            tmp.path(),
            Arc::new(Fixtures { requests: Mutex::new(Vec::new()), bodies: HashMap::new() }),
            Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
        )
        .with_catalog(fanout),
    );
    let response = app
        .oneshot(request(
            "GET",
            "/api/v1/discover/movie/filtered?year=1800",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "empty TMDB must not become 502");
    let body = json_body(response).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["data"]["items"].as_array().unwrap().len(), 0);
}
```

`state(...).with_catalog` 的签名跟 `webapi.rs:447-469` 的 `fanout_app` 对齐；若 `state` 没有 `with_catalog`，抄那个 helper 到本文件私有 `fn fanout_filtered_app`。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p api --test management discover_filtered_empty_tmdb_does_not_fail_because_bangumi_unsupported -- --nocapture`

Expected: FAIL，status 502，message 含「不支持」。

- [ ] **Step 3: 改 `merge`**

`crates/api/src/catalog_fanout.rs` 现有：

```rust
fn merge(
    results: impl Iterator<Item = Result<Vec<CatalogHit>, String>>,
) -> Result<Vec<CatalogHit>, String> {
    let mut hits = Vec::new();
    let mut last_err = None;
    for result in results {
        match result {
            Ok(mut rows) => hits.append(&mut rows),
            Err(err) => last_err = Some(err),
        }
    }
    if hits.is_empty() {
        if let Some(err) = last_err {
            return Err(err);
        }
    }
    Ok(hits)
}
```

改为：

```rust
fn is_unsupported(err: &str) -> bool {
    err.contains("不支持")
}

fn merge(
    results: impl Iterator<Item = Result<Vec<CatalogHit>, String>>,
) -> Result<Vec<CatalogHit>, String> {
    let mut hits = Vec::new();
    let mut last_real_err = None;
    for result in results {
        match result {
            Ok(mut rows) => hits.append(&mut rows),
            Err(err) if is_unsupported(&err) => {}
            Err(err) => last_real_err = Some(err),
        }
    }
    if hits.is_empty() {
        if let Some(err) = last_real_err {
            return Err(err);
        }
    }
    Ok(hits)
}
```

现有 `discover_filtered_reports_unsupported_source`（默认测试 `state()`、没有 FakeCatalog）：改完后会变成 **200 + items=[]**（只剩 unsupported）。**更新该测试**：

```rust
assert_eq!(response.status(), StatusCode::OK);
assert_eq!(body["ok"], true);
assert_eq!(body["data"]["items"].as_array().unwrap().len(), 0);
```

另加一条：支持源返回真实 `Err("tmdb timeout")` 且无 hits → 仍 502。用一个 `FailingTmdb`：`filtered_movie` 返回 `Err("tmdb timeout")`，fanout 只含它（或它 + UnsupportedCatalog）。

更新 `discover.rs` filtered handler 顶部注释为：不支持的源被 fanout 忽略；空列表是 200；只有支持源的上游失败才 502。

- [ ] **Step 4: 跑测试**

```
cargo test -p api --test management catalog
cargo test -p api --test management webapi
```

Expected: PASS。`year=1800` 这类空墙不再 502。

- [ ] **Step 5: Commit**

```bash
git add crates/api/src/catalog_fanout.rs crates/api/src/http/discover.rs crates/api/tests/management/catalog.rs
git commit -m "$(cat <<'EOF'
fix(catalog): ignore unsupported sources in filtered fanout

Empty TMDB walls no longer 502 just because Bangumi/AniList
cannot do filtered discover.
EOF
)"
```

---

### Task 2: 单条「清理记录」不得删整库缺失行

**Files:**
- Modify: `crates/api/src/store/library.rs`（`delete_missing_rows` 旁）
- Modify: `crates/api/src/http/library.rs`（`delete_missing_rows` handler）
- Modify: `web/lib/api/libraries.ts`（`clearMissingLibraryRecords` / `deleteMissingRows`）
- Test: `crates/api/tests/management/` 新测或挂到现有 library 测试

**Interfaces:**
- Consumes: 现有 `DELETE /api/v1/libraries/{id}/missing-rows`（整库）；ledger 行有 `media_id`
- Produces:
  - 无 query：行为不变，删该库根下全部 `missing_at IS NOT NULL` 行（「全部清理」）
  - `?media_item_id={uuid}`：只删该 `MediaId` 且落在本库根内的缺失行

前端 [`library-detail-view.tsx:2154`](<../../../web/components/library-detail-view.tsx>) 单条按钮文案是「清理「{title}」的 N 条缺失记录」，却调用 `clearMissingLibraryRecords(libraryId, item.media_item_id)`；helper 忽略第二参数。本 Task 让第二参数生效，不改文案。

- [ ] **Step 1: Store + HTTP 失败测试**

在 `crates/api/tests/management/` 里跟现有 library 测试同一文件（或 `library.rs`）追加：

```rust
#[tokio::test]
async fn delete_missing_rows_with_media_item_id_only_clears_that_item() {
    // 1. 建一个 tv 库，插入两部剧的 ledger 行，SQL 把两行 missing_at 都打上
    // 2. DELETE /api/v1/libraries/{id}/missing-rows?media_item_id={a}
    // 3. GET /api/v1/libraries/{id}/missing
    //    A 消失，B 仍在
}
```

夹具优先复用现有 `seed` / `insert_ledger`；没有就用 `Connection::open(data/app.db)` 直接 `UPDATE ledger SET missing_at = 1`。不要碰磁盘文件。

再加一条：不带 query 的 DELETE 仍删该库全部缺失（保持「全部清理」）。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p api --test management delete_missing_rows_with_media_item_id -- --nocapture`

Expected: FAIL（query 被忽略，两行都没了，或 query 未解析）。

- [ ] **Step 3: 最小实现**

`store/library.rs`：

```rust
pub fn delete_missing_rows_for_media(
    &self,
    roots: &[std::path::PathBuf],
    media_id: domain::MediaId,
) -> Result<usize, StoreError> {
    let mut stmt = self.library.prepare(
        "SELECT path FROM ledger WHERE media_id = ?1 AND missing_at IS NOT NULL",
    )?;
    let paths = stmt.query_map(params![media_id.to_string()], |row| row.get::<_, String>(0))?;
    let mut deleted = 0;
    for path in paths {
        let path = path?;
        let in_root = roots.iter().any(|root| std::path::Path::new(&path).starts_with(root));
        if in_root {
            deleted += self
                .library
                .execute("DELETE FROM ledger WHERE path = ?1", params![path])?;
        }
    }
    Ok(deleted)
}
```

handler（`http/library.rs`）增加 query：

```rust
#[derive(Deserialize)]
pub(crate) struct DeleteMissingQuery {
    media_item_id: Option<String>,
}

pub(crate) async fn delete_missing_rows(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Query(query): Query<DeleteMissingQuery>,
) -> Response {
    // get library as today
    let deleted = match query.media_item_id.as_deref().filter(|s| !s.is_empty()) {
        Some(raw) => {
            let media_id = match domain::MediaId::from_str(raw) {
                Ok(id) => id,
                Err(_) => return err(StatusCode::BAD_REQUEST, "library.invalid", "media_item_id 无效"),
            };
            store.delete_missing_rows_for_media(&library.root_paths, media_id)
        }
        None => store.delete_missing_rows(&library.root_paths),
    };
    // map Result → 500 / ok({deleted}) 同现在
}
```

`web/lib/api/libraries.ts`：

```ts
export async function deleteMissingRows(
  libraryId: string,
  mediaItemId?: string,
): Promise<{ deleted: number }> {
  const qs = mediaItemId
    ? `?media_item_id=${encodeURIComponent(mediaItemId)}`
    : "";
  const data = await unwrap(
    request<ApiEnvelope<Record<string, unknown>>>(
      `/libraries/${libraryId}/missing-rows${qs}`,
      { method: "DELETE" },
    ),
  );
  return { deleted: num(data?.deleted) };
}

export async function clearMissingLibraryRecords(
  libraryId: string,
  mediaItemId?: string,
): Promise<{ cleared: number }> {
  const result = await deleteMissingRows(libraryId, mediaItemId);
  return { cleared: result.deleted };
}
```

`library-detail-view.tsx:2029` 全部清理继续不传第二参；`:2154` 单条继续传 `item.media_item_id`（现在会生效）。

成员 `authz.rs` 已覆盖不带 query 的 DELETE 403，不必改。

- [ ] **Step 4: 跑测试**

```
cargo test -p api --test management delete_missing_rows
cargo test -p api --test management authz
cd web && pnpm exec tsc --noEmit
```

Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add crates/api/src/store/library.rs crates/api/src/http/library.rs crates/api/tests web/lib/api/libraries.ts
git commit -m "$(cat <<'EOF'
fix(library): scope missing-row delete to one media item

Per-item 「清理记录」 no longer wipes the whole library.
EOF
)"
```

---

### Task 3: 「前往网络设置」不再 404

**Files:**
- Modify: `web/components/discover-view.tsx:553`
- Modify: `web/components/metadata-settings-section.tsx`（代理 `<section>` 加 `id="proxy"`）
- 不要改路由表、不要复活 `/settings/network` 分区、不要接已删的 appearance/network API

**Interfaces:**
- Consumes: 发现页 `DiscoverError` 在 `UPSTREAM_UNREACHABLE` 时渲染「前往网络设置」
- Produces: 该按钮进入 `/settings/metadata#proxy`（代理表单真实所在）

`settingsSections` 没有 `network`。代理 UI 在元数据源页。hash 滚动：给代理 section 加 `id="proxy"`；浏览器默认会跳锚点。若该页是 SPA 内切换，再在 `metadata-settings-section.tsx` 的 `useEffect` 里 `document.getElementById("proxy")?.scrollIntoView()`，仅当 `window.location.hash === "#proxy"`。

- [ ] **Step 1: 改链接**

```tsx
<Link
  to={"/settings/metadata#proxy"}
  className="btn-accent flex h-9 items-center rounded-full px-5 text-ui font-semibold"
>
  前往网络设置
</Link>
```

- [ ] **Step 2: 锚点**

代理那块 `<section>`：

```tsx
<section id="proxy">
  <h3 className="group-label mb-2.5 px-1">网络代理 (HTTP / SOCKS5)</h3>
  ...
</section>
```

`web/app/(app)/settings/[section]/page.tsx` 已把未知 section 渲染 `<NotFound />`，不要加 `network` 分区。

- [ ] **Step 3: 类型检查**

Run: `cd web && pnpm exec tsc --noEmit`

Expected: 0 errors。

- [ ] **Step 4: Commit**

```bash
git add web/components/discover-view.tsx web/components/metadata-settings-section.tsx
git commit -m "$(cat <<'EOF'
fix(web): send discover network errors to metadata proxy settings
EOF
)"
```

---

## Wave 2 — 订阅归属库真正打通

### Task 4: PATCH 接受 `library_id`，前端按它路由

**Files:**
- Modify: `crates/api/src/http/subscriptions/types.rs`（`PatchSubscriptionInput`）
- Modify: `crates/api/src/http/subscriptions/patch_fields.rs`（`apply_patch_fields`）
- Modify: `web/lib/api/subscriptions.ts`（`Subscription.library_id`、`updateSubscription` 类型）
- Modify: `web/components/library-view.tsx:94-100`（`effectiveLibraryId`）
- Modify: `web/components/subscribe-dialog.tsx` / `subscription-adjust-dialog.tsx`（去掉 `as unknown as number`）
- Test: `crates/api/tests/management/subscribes.rs`（或新建 `subscription_library.rs` 并在 `tests/management/main.rs` `mod`）

**Interfaces:**
- Consumes: 创建订阅已解析 `body.library_id` → `Option<LibraryId>`（`http/subscriptions.rs:300-317`）；JSON 列表已输出 `"library_id"`（`views.rs:123`）
- Produces: `PATCH /api/v1/subscriptions/{id}`：
  - 省略字段 → 不变
  - `null` 或 `""` → 清空，回落类型默认库
  - UUID 字符串 → 校验库存在、`kind` 与 `media.kind` 一致，否则 400 `subscription.invalid`

serde：`Option<Option<String>>` 能区分省略 / JSON null。

前端 `Subscription` 补 `library_id: string | null`。`updateSubscription` 的 `library_id` 从 `number | null` 改为 `string | null`（库 id 已是 UUID）。

`effectiveLibraryId`：

```ts
export function effectiveLibraryId(
  sub: Subscription,
  libraries: MediaLibrary[],
): string | null {
  if (sub.library_id) {
    const hit = libraries.find((l) => l.id === sub.library_id);
    if (hit) return hit.id;
  }
  return libraries.find((l) => l.kind === sub.media.kind && l.is_default)?.id ?? null;
}
```

- [ ] **Step 1: 失败测试**

```rust
#[tokio::test]
async fn patch_subscription_library_id_round_trips() {
    // 建两个 tv 库 A/B，创建订阅（可省略 library_id）
    // PATCH { "library_id": B.id } → GET data.library_id == B.id
    // PATCH { "library_id": null } → GET data.library_id == null
}

#[tokio::test]
async fn patch_subscription_rejects_library_of_wrong_kind() {
    // tv 订阅 PATCH 一个 movie 库 → 400
}
```

创建库/订阅的 oneshot 抄 `subscribes.rs` 现有 helper。

- [ ] **Step 2: 跑红**

Run: `cargo test -p api --test management patch_subscription_library_id -- --nocapture`

Expected: FAIL（PATCH 后 `library_id` 仍是创建时的值 / null）。

- [ ] **Step 3: 实现**

`types.rs`：

```rust
#[derive(Deserialize)]
pub(crate) struct PatchSubscriptionInput {
    // ...existing fields...
    /// None = 省略（不变）；Some(None) = JSON null（清空）；Some(Some(id)) = 指定库
    #[serde(default)]
    pub(super) library_id: Option<Option<String>>,
}
```

`patch_fields.rs` 在 `apply_patch_fields` 里 `update_downloader` 之后调用：

```rust
fn update_library_id(
    store: &Store,
    subscribe: &mut Subscribe,
    library_id: Option<Option<String>>,
) -> Result<(), Response> {
    let Some(maybe) = library_id else { return Ok(()); };
    let Some(raw) = maybe.as_deref().filter(|s| !s.is_empty()) else {
        subscribe.library_id = None;
        return Ok(());
    };
    let id = domain::LibraryId::from_str(raw).map_err(|_| {
        err(StatusCode::BAD_REQUEST, "subscription.invalid", "library_id 无效")
    })?;
    let Some(library) = store.get_library(raw).ok().flatten() else {
        return Err(err(StatusCode::BAD_REQUEST, "subscription.invalid", "媒体库不存在"));
    };
    if library.kind != /* media kind string */ {
        // 需要 media.kind：要么 apply_patch_fields 多传 &Media，要么在此 store.get_media(subscribe.media_id)
    }
    subscribe.library_id = Some(id);
    Ok(())
}
```

`library.kind` 与 `Media.kind.as_str()` 比较。movie/tv/video 不一致 → 400「订阅类型与目标库不一致」。

前端类型与 `effectiveLibraryId` 按上面改。`subscribe-dialog.tsx` / `adjust-dialog.tsx` 里 `library_id: libraryId as unknown as number` 改为直接传 `string | null`（`libraryId` 已是 UUID 字符串）。

- [ ] **Step 4: 跑测试**

```
cargo test -p api --test management patch_subscription_library_id
cargo test -p api --test management subscribes
cd web && pnpm exec tsc --noEmit
```

Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add crates/api/src/http/subscriptions crates/api/tests web/lib/api/subscriptions.ts web/components/library-view.tsx web/components/subscribe-dialog.tsx web/components/subscription-adjust-dialog.tsx
git commit -m "$(cat <<'EOF'
fix(subscribe): honor library_id on PATCH and in the library page
EOF
)"
```

---

## Wave 3 — 计划残留与小正确性

### Task 5: CLI 去掉 v1 之外的兼容回退

**Files:**
- Modify: `crates/api/src/cli/actions.rs:123-140`（`items` / `sites`，删 `torrents` / `failures`）
- Modify: `crates/api/src/cli/catalog.rs:12-15`（只读 `titles`）
- Modify: `crates/api/src/cli/filters.rs:167-170`（只读 `default_rule_set_id`）
- Modify: `crates/api/src/cli/lists.rs:149-160`（downloads 只读 `items`；title 只读 `name`）
- Test: `crates/api/tests/management/cli.rs`

**Interfaces:**
- Consumes: v1 成功体已由 `unwrap_data` 剥开 `data`
  - search torrents: `{ items, sites }`（`http/search.rs:170-175`）
  - search titles: `{ titles }`（`http/search.rs:604-609`）
  - rule-sets default: `{ default_rule_set_id }`（`http/rule_sets.rs:277`）
  - downloaders/tasks: `{ items, sources }`
- Produces: CLI 不再读 `torrents` / `failures` / `default_filter_id` / 根数组

`lists.rs:34-35` 的 `row["media"]["title"] or row["title"]` 若 v1 订阅列表确有 `media.title`，删 `row["title"]` 回退；先打开一条 CLI 测试夹具确认。不要改 CLI 路径前缀（Task 7 已迁完）。

- [ ] **Step 1: 改 CLI 读取，跑 `cargo test -p api --test management cli`**
- [ ] **Step 2: 红了就改测试断言去对齐 v1 字段，不要把兼容层加回**
- [ ] **Step 3: Commit**

```bash
git commit -m "$(cat <<'EOF'
refactor(cli): drop legacy envelope field fallbacks
EOF
)"
```

---

### Task 6: 死代码 `collection_cover` 不再写 compact UUID

**Files:**
- Modify or Delete: `crates/api/src/store/collections.rs:164-182`

**Interfaces:**
- Consumes: HTTP 合集封面已走 `http/collections.rs:102` 的 `artwork_url`
- Produces: 仓库里不再有 `replace('-', "")` 的自有海报写出（Jellyfin 的 compact 除外）

`collection_cover` 无其它调用方。优先**删除整个方法**。若测试或后续代码仍引用，改为：

```rust
crate::http::library::artwork_url("posters", row.id)
```

不要改 `media_server_provider.rs`。

- [ ] **Step 1: `rg collection_cover` 确认只有这一处定义**
- [ ] **Step 2: 删除方法**
- [ ] **Step 3:** `cargo test -p api --lib`
- [ ] **Step 4: Commit**

```bash
git commit -m "$(cat <<'EOF'
chore(api): drop unused compact collection_cover helper
EOF
)"
```

---

### Task 7: 收藏范围重叠不只全等 JSON

**Files:**
- Modify: `web/lib/library-routing-warnings.ts`
- Test: `web/test/library-routing-warnings.test.mjs`（新建；与 `library-manage.test.mjs` 一样用 `node --test` 跑纯模块）

**Interfaces:**
- Consumes: `MediaLibrary.match_rules?: MatchRule[]`，`MatchRule = { field, op: "any_of", values }`
- Produces: 同 `kind` 的两库，若存在同一 `field` 且 `values` 集合相交 → 一条中文警告。无规则的库不提示（与现在一致）。

```ts
function valuesOverlap(a: MatchRule[], b: MatchRule[]): boolean {
  for (const ra of a) {
    for (const rb of b) {
      if (ra.field !== rb.field) continue;
      const set = new Set(ra.values.map(String));
      if (rb.values.some((v) => set.has(String(v)))) return true;
    }
  }
  return false;
}
```

文案保持：「「A」与「B」的收藏范围相同，自动入库可能落到任一库。」相交但不全等时改成：「「A」与「B」的收藏范围有重叠，自动入库可能落到任一库。」全等继续用「相同」。

- [ ] **Step 1: 失败测试**

```js
test("相交但不全等的 match_rules 也要警告", () => {
  const warnings = routingOverlapWarnings([
    lib({ name: "华语", kind: "movie", match_rules: [{ field: "origin_countries", op: "any_of", values: ["CN", "TW"] }] }),
    lib({ name: "大陆", kind: "movie", match_rules: [{ field: "origin_countries", op: "any_of", values: ["CN"] }] }),
  ]);
  assert.equal(warnings.length, 1);
});
```

`lib` helper 只需要 `name/kind/match_rules`。

- [ ] **Step 2:** `cd web && node --test test/library-routing-warnings.test.mjs` → FAIL
- [ ] **Step 3: 实现相交判断**
- [ ] **Step 4: 测绿 + `pnpm exec tsc --noEmit`**
- [ ] **Step 5: Commit**

```bash
git commit -m "$(cat <<'EOF'
fix(web): warn on overlapping library match_rules, not only identical JSON
EOF
)"
```

---

## 明确不做

1. Task 8 拆 legacy HTTP。
2. Jellyfin compact id / 播放路由。
3. `POST /users/{id}/reset-password`。
4. 库管理状态列接扫描进度（需要列表 API 先下发作业，另开计划）。
5. 给 filtered discover 加 `providers[]`（现前端不读）。
6. 改 `top_rated_*` / `trending_*` 的空 default 为 Err（它们已经 `Ok([])`，墙用 `row()` 藏空分区，不是 502 源）。

---

## 执行顺序（必须）

```
Task 1 → 2 → 3 → 4 → 5 → 6 → 7
```

1 和 2 是数据/可用性 P0，必须先做。3 是一行死链。4 依赖 Task 11 已发出的 `library_id` JSON。5–7 不阻塞主路径，但要在本计划内做完，避免又留半截。

每 Task：`cargo test -p api --test management <filter>` 和/或 `cd web && pnpm exec tsc --noEmit`；全部结束后 `cargo test --workspace` 一次 + `cd web && pnpm test`。

## Spec coverage

| 审计项 | Task |
|---|---|
| filtered discover 空墙 502 | 1 |
| 单条清理缺失误删全库 | 2 |
| `/settings/network` 404 | 3 |
| PATCH / 前端忽略 `library_id` | 4 |
| CLI 兼容层（原 Task 7 残留） | 5 |
| `collection_cover` compact UUID | 6 |
| 重叠规则只比 JSON 全等 | 7 |
| Task 8 legacy router | 明确不做 |
| Jellyfin | 明确不做 |

## Placeholder scan

无 TBD。Task 1 的 `with_catalog` 若测试 `state()` 没有该方法，在该 Task 内抄 `webapi.rs` 的 `fanout_app`，不另开任务。
