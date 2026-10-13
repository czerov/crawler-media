# 发现墙 Hero 失败与重试缓存收口 Implementation Plan

> **历史状态（2026-10-13 整理）：** Hero 失败及重试缓存历史补强；本次未运行真实 UI，不把纯状态测试当成浏览器交互验证。
>
> 本文保留原设计、发现/任务 ID 与当时证据；日期本身不表示过时。旧执行/提交/清理命令仅作历史记录，不应直接重跑；复选框不是当前实现清单。当前依据：[数据库/API review](<../../audits/2026-10-13-database-api-review.md>) 与 [剩余计划](<2026-10-13-database-api-followup.md>)，未覆盖的旧 ID 仍须单独对账。

> **规模规则更新：** 文中的 60 行硬限/整改目标是历史要求；当前 [AGENTS.md](<../../../AGENTS.md>) 为生产函数软限 60、测试软限 80、两者硬限 120。历史测试/行数记录不改写，规模合规不等于功能安全。

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Hero 分区失败不再伪装成空白成功；重试清掉失败片单缓存；后端成功空分区有回归断言；UI 使用已有纯函数文案。

**Architecture:** 纯状态继续放在 `web/lib/discovery-section-state.ts`。`discover-view` 对 `section.error` 的 Hero 与普通行一视同仁：不调用 `browseDiscoveryCollection`。重试删除当前 `pageCache` key、失败 `popularCache` 无法命中（已由 Task 2 保证），并按失败 refs 删除 `collectionCache`。后端 `discover_wall` 增加 `Ok([])` 分区断言。

**Tech Stack:** TypeScript + Node `node:test`、Axum oneshot。无真实 TMDB 网络。

## Global Constraints

- `/api/v1/discover/{kind}` 仍 200 + `sections[]`。失败 `{code:"discover.section_failed", message:"分区暂时无法加载，请重试"}`。成功空行 `items:[]` 且无 `error`。
- 不缓存失败页/失败片单。正常 `Ok([])` 不是失败。
- 前端测试不得直接导入带 `@/` 的 `discover.ts`；只测纯函数，UI 必须调用这些函数。
- 文件 800 / 函数 60 硬限。

## File map

| File | Responsibility |
|---|---|
| `crates/api/tests/management/discover_wall.rs` | 成功空分区无 `error` |
| `web/lib/discovery-section-state.ts` | `heroFailureMessage`、`retryCacheKeys` |
| `web/components/discover-view.tsx` | Hero 失败警示、重试清 `collectionCache`、使用 `discoverySectionMessage` |
| `web/test/discover-section-failure.test.mjs` | 纯函数：Hero 文案、重试要删的 cache key |

---

### Task 1: 后端成功空分区合同

**Files:** Modify `crates/api/src/http/discover.rs` only if needed; Test `crates/api/tests/management/discover_wall.rs`.

**Interfaces:** `FailingSectionCatalog` 已有；新增 `EmptyUpcomingCatalog`：`upcoming_movie() -> Ok(vec![])`，`popular_movie()` 返回一条，其它默认空或 Ok。

- [ ] **Step 1 — 红测：**

```rust
#[tokio::test]
async fn discover_successful_empty_section_has_items_array_without_error() {
    // catalog.upcoming_movie = Ok(vec![])
    // GET /api/v1/discover/movie
    let upcoming = sections.iter().find(|s| s["id"] == "upcoming").unwrap();
    assert_eq!(upcoming["items"].as_array().unwrap().len(), 0);
    assert!(upcoming.get("error").is_none());
    assert!(upcoming.get("id").is_some());
}
```

`upcoming` 在 `movie_sections` 里是 `"即将上映"` 那一行，id 为 `"upcoming"`（见 `crates/api/src/http/discover.rs` `movie_sections`）。

- [ ] **Step 2 — 验红：** `cargo test -p api --test management discover_successful_empty_section`。若现实现已满足则该测试直接绿，仍提交测试。
- [ ] **Step 3 — 实现：** `row()` 的 `Ok(hits)` 分支已不写 `error`。不要给空成功行加 `error`。
- [ ] **Step 4 — 测绿：** `cargo test -p api --test management discover`
- [ ] **Step 5 — Commit：** `git add crates/api/tests/management/discover_wall.rs crates/api/src/http/discover.rs && git commit -m "test(discover): assert successful empty sections have no error"`

### Task 2: Hero 失败与重试缓存纯函数

**Files:** Modify `web/lib/discovery-section-state.ts`; Test `web/test/discover-section-failure.test.mjs`.

**Interfaces:**
- Produces: `export function heroFailureMessage(section: {error?: SectionErrorInfo}): string | null` — 有 error 返回 `discoverySectionMessage(section)`，否则 `null`。
- Produces: `export function retryCollectionCacheKeys(cacheKey: string, sections: ReadonlyArray<{collectionRef: string; previewLimit: number; error?: SectionErrorInfo}>): { pageKey: string; collectionKeys: string[] }`
  - `pageKey === cacheKey`
  - `collectionKeys` = 失败 section 的 `` `${collectionRef}:${previewLimit}` ``，与 `discover-view` 现有 `cacheKeyFor` 一致。

- [ ] **Step 1 — 红测：**

```js
test("Hero 失败文案与普通分区相同，成功空为 null", () => {
  assert.equal(heroFailureMessage({ error: { code: "discover.section_failed", message: "分区暂时无法加载，请重试" } }), "分区暂时无法加载，请重试");
  assert.equal(heroFailureMessage({}), null);
});

test("重试只列出失败分区的 collectionCache key", () => {
  const keys = retryCollectionCacheKeys("movie:tmdb", [
    { collectionRef: "tmdb:movie:featured-weekly", previewLimit: 20, error: { code: "x", message: "y" } },
    { collectionRef: "tmdb:movie:popular", previewLimit: 20 },
  ]);
  assert.equal(keys.pageKey, "movie:tmdb");
  assert.deepEqual(keys.collectionKeys, ["tmdb:movie:featured-weekly:20"]);
});
```

- [ ] **Step 2 — 验红：** `cd web && pnpm test -- test/discover-section-failure.test.mjs`
- [ ] **Step 3 — 实现：** 按上述签名写两个函数；`heroFailureMessage` 直接调用已有 `discoverySectionMessage`。
- [ ] **Step 4 — 测绿：** `cd web && pnpm exec tsc --noEmit && pnpm test -- test/discover-section-failure.test.mjs`
- [ ] **Step 5 — Commit：** `git add web/lib/discovery-section-state.ts web/test/discover-section-failure.test.mjs && git commit -m "fix(web): define hero failure and retry cache keys"`

### Task 3: discover-view 消费纯函数

**Files:** Modify `web/components/discover-view.tsx`.

**Interfaces:** 从 `@/lib/discovery-section-state` 导入 `discoverySectionStatus`, `discoverySectionMessage`, `heroFailureMessage`, `retryCollectionCacheKeys`。

- [ ] **Step 1 — 行为约束（写入组件，用纯函数测试覆盖）：**
  1. `handleRetry`：`const keys = retryCollectionCacheKeys(cacheKey, page?.sections ?? []); pageCache.delete(keys.pageKey); for (const k of keys.collectionKeys) collectionCache.delete(k);` 然后 `setRowsByRef({}); setHero(undefined); setError(null); setReloadKey(k=>k+1)`。
  2. `loadPage` Hero 分支：`if (heroSection.error) { setHero([]); /* 另存 heroError 用 heroFailureMessage(heroSection) 渲染警示 */ return from browse }`。有 `section.error` 时**禁止** `browseDiscoveryCollection`。
  3. 普通行失败文案改用 `{discoverySectionMessage(section)}`，不要内联 `section.error?.message || "..."`。
  4. Hero 失败 UI：在 Hero 槽渲染小型警示，文案 `heroFailureMessage(heroSection)`，带「重试」按钮调用 `handleRetry`。不要只 `setHero([])` 后整块消失。

Hero 警示 JSX：

```tsx
{heroSection?.error && (
  <div className={`rounded-xl border border-white/5 bg-white/[0.02] p-4 ${isNf ? "mx-[4vw]" : "mx-6"}`}>
    <p className="text-body font-semibold text-[var(--text)]">{heroSection.title}</p>
    <p className="mt-0.5 text-caption text-[var(--text-muted)]">{heroFailureMessage(heroSection)}</p>
    <button type="button" onClick={handleRetry} className="btn-glass mt-3 h-8 rounded-full px-3 text-caption font-semibold">重试</button>
  </div>
)}
```

普通失败行把 `{section.error?.message || "分区暂时无法加载，请重试"}` 换成 `{discoverySectionMessage(section)}`。

- [ ] **Step 2 — 类型检查：** `cd web && pnpm exec tsc --noEmit`
- [ ] **Step 3 — 测绿：** `cd web && pnpm test && cargo test -p api --test management discover`
- [ ] **Step 4 — Commit：** `git add web/components/discover-view.tsx && git commit -m "fix(web): show hero failure and clear collection cache on retry"`

**验收：** 失败 Hero 不再走 collection 端点；重试后不会读到旧 `collectionCache` 成功行；`Ok([])` 仍隐藏空行且不进全失败。
