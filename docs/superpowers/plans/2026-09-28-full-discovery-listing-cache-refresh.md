# 完整片单分页与下拉刷新 Implementation Plan

> **历史状态（2026-10-13 整理）：** 片单分页/刷新历史方案；后续 project-scan 计划补充响应竞态与组件验收，不据本文复选框判定当前实现缺失。
>
> 本文保留原设计、发现/任务 ID 与当时证据；日期本身不表示过时。旧执行/提交/清理命令仅作历史记录，不应直接重跑；复选框不是当前实现清单。当前依据：[数据库/API review](<../../audits/2026-10-13-database-api-review.md>) 与 [剩余计划](<2026-10-13-database-api-followup.md>)，未覆盖的旧 ID 仍须单独对账。

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 从发现墙点击「看全部」必须使用真实分页信息并可加载后续页；完整片单下拉刷新必须真正重新请求第一页而不是返回旧快照。

**Architecture:** 墙上预览与完整片单是两种请求语义，不再共用 `popularCache` 的 `hasMore=false` 伪造分页元数据。`browseDiscoveryCollection` 增加显式用途 `mode: "preview" | "full"`（或拆两个清晰命名的 wrapper），完整页第一页始终请求 `/discover/{kind}/collection/{id}?page=1` 获取真实 `has_more/total_pages/total_results`。完整页首次进入可复用自己的 `collectionGridSnapshots`，只有用户下拉刷新才绕过/清理快照；刷新失败保留旧内容并显示错误，不污染其它片单。

**Tech Stack:** React 19、TypeScript、Vite、Node `node:test`、Axum `/api/v1` 后端已支持 `collection_paged`。

## Global Constraints

- 自有 REST `/api/v1` `{ok,data}` 信封与官方 Jellyfin 路由不变。
- `source=tmdb` 全片单翻页的 `hasMore/totalResults` 必须来自后端 `discover_collection`，不能从 20 条预览推断；豆瓣只有允许的完整分区可点「看全部」。
- 普通返回页面仍可复用片单网格快照以保留回退时滚动窗口；手动刷新必须发新请求。成功空列表是正常结果，错误不能被缓存为空成功。
- 浏览器网络/页面缓存测试不请求真实 TMDB；Node 单测可测试无 alias 的纯状态模块；对真实组件行为补可执行渲染/浏览器回归，不得只复制 production 条件表达式到测试。

## File map

| File | Responsibility |
|---|---|
| `web/lib/api/discover.ts` | 区分 `popularCache` 预览与完整分页的取数逻辑；公开可测的请求意图接口 |
| `web/lib/discovery-collection-cache.ts` | 无 alias 的纯函数，描述 preview/full 是否可命中缓存、强制刷新 |
| `web/components/discover-view.tsx` | 墙分区调用 preview 模式，无额外请求 |
| `web/components/collection-grid-view.tsx` | 完整页第一页调用 full 模式；下拉刷新清自己的 snapshot 并绕过 API 缓存 |
| `web/test/discovery-collection-cache.test.mjs` | preview/full 命中规则和刷新失效 |
| `web/test/collection-grid-contracts.test.mjs` | 片单入口与分页/刷新行为合同（需可执行 seam） |

---

### Task 1: 墙预览不能冒充完整榜单第一页

**Files:** Modify `web/lib/api/discover.ts:373-461`, `web/components/discover-view.tsx:164-220`, `web/components/collection-grid-view.tsx:140-156`; Create `web/lib/discovery-collection-cache.ts`, `web/test/discovery-collection-cache.test.mjs`.

**Interfaces:**
- Produces `type DiscoveryCollectionMode = "preview" | "full"`。
- `browseDiscoveryCollection(collectionRef: string, limit: number, init?: RequestInit, page=1, mode: DiscoveryCollectionMode="preview")`：preview page=1 允许 `popularCache`；full 的 page=1 必须请求网络；page>1 原行为不变。
- `shouldUsePreviewCache(mode: DiscoveryCollectionMode, page: number): boolean`：仅 `mode === "preview" && page === 1` 为 true。组件「看全部」必须传 `"full"`，墙内 Hero/横滚维持默认 `"preview"`。

- [ ] **Step 1 — 红测：** 纯函数测试：`shouldUsePreviewCache("preview",1) === true`，`("full",1)===false`，`("preview",2)===false`。在测试 HTTP fake/可注入 request seam 中模拟墙预览 `items=20,hasMore=false`、完整榜单 `page=1,totalPages=4,totalResults=73,hasMore=true`；打开完整页第一页必须实际请求 collection endpoint 并返回真实分页字段，第二页可继续请求且保留前 20 条。测试命中「看全部」入口的真实函数调用，不能只在测试里复制 mode 条件。
- [ ] **Step 2 — 验红：** `cd web && node --test test/discovery-collection-cache.test.mjs`。目前 `browseDiscoveryCollection` 的 page=1 必命中预览缓存，`hasMore=false`。
- [ ] **Step 3 — 实现：** 在 `web/lib/api/discover.ts` 仅当 `shouldUsePreviewCache(mode,page)` 时读取 `popularCache`；在 `collection-grid-view.tsx` 完整页 load 调 `browseDiscoveryCollection(collectionRef, TMDB_PAGE_SIZE, {signal}, 1, "full")`；墙 `discover-view.tsx` 可保持默认 preview。不要更改 `CollectionEnvelope` 字段映射与 full listing 检查 `supportsFullListing`。豆瓣榜单第一页从真实分页返回，`provider !== "tmdb"` 不自动加载下一页仍按已有 UI。
- [ ] **Step 4 — 测绿：** `cd web && pnpm exec tsc --noEmit && pnpm test`。后端回归 `cargo test -p api --test management discover`。
- [ ] **Step 5 — Commit：** `git add web/lib/api/discover.ts web/lib/discovery-collection-cache.ts web/components/{discover-view,collection-grid-view}.tsx web/test/discovery-collection-cache.test.mjs web/test/collection-grid-contracts.test.mjs && git commit -m "fix(web): fetch real pagination for full discovery collections"`

### Task 2: 下拉刷新必须绕过自己的历史快照

**Files:** Modify `web/components/collection-grid-view.tsx:24-163`, `web/lib/discovery-collection-cache.ts`, `web/test/discovery-collection-cache.test.mjs`, `web/test/collection-grid-contracts.test.mjs`.

**Interfaces:**
- 新增 `deleteCollectionGridSnapshot(collectionRef: string): void`，仅删除此片单；若 `createSessionSnapshots` API 无 `.delete()`，扩展 `web/lib/session-snapshot.ts` 新增该方法（保持 get/set/clear 兼容）。
- `onPullTouchEnd` 当达到阈值时：先删除当前片单 snapshot，再 `setReloadKey`；effect 中 `reloadKey>0` 跳过快照分支且用 `mode="full"` 发第一页请求。初次进入 `reloadKey=0` 时仍可命中快照并还原列表/页码。刷新旧页时先不 `setItems(null)`，成功后替换 `items,title,nextPage,totalResults,hasMore`，失败保留旧列表并显示错误。

- [ ] **Step 1 — 红测：** 先以快照 `{items:[old],nextPage:3,hasMore:true}` 渲染完整片单：返回页面不重拉，维持旧窗口；模拟触发下拉阈值 `pullHint>=32`：snapshot 删除、第一页 full 网络请求发生一次，成功后 `items:[new],nextPage:2,hasMore:true`；模拟网络失败则旧列表还在且出现错误提示。测另一个 `collectionRef` 快照没被删。
- [ ] **Step 2 — 验红：** `cd web && node --test test/collection-grid-contracts.test.mjs`。当前 effect 的 `cached` 分支直接返回、不发请求。
- [ ] **Step 3 — 实现：** 先查看 `createSessionSnapshots` 的 `.delete` 语义；将快照恢复判断改成 `if (reloadKey === 0 && cached)`；手动刷新时保留旧 `items`，但在请求成功后覆盖，避免 `useLayoutEffect` 同步把旧快照再写回；可用 `refreshInFlightRef` 在刷新期间跳过 `rememberCollectionGridSnapshot`，成功后再写新快照。`AbortController` 取消时不可把错误提示当网络故障。
- [ ] **Step 4 — 测绿：** `cd web && pnpm exec tsc --noEmit && pnpm test`，至少手动验证「看全部」73 项可翻页、下拉刷新后首屏数据变化且失败时旧数据保留。
- [ ] **Step 5 — Commit：** `git add web/components/collection-grid-view.tsx web/lib/session-snapshot.ts web/lib/discovery-collection-cache.ts web/test/{discovery-collection-cache,collection-grid-contracts}.test.mjs && git commit -m "fix(web): bypass collection snapshots on explicit refresh"`

**整体验收：** `cargo test -p api --test management discover && cd web && pnpm exec tsc --noEmit && pnpm test`；不改 `/api/v1` 形状，浏览器手测 SPA 进入和硬刷新都应看到相同 `totalResults/hasMore`。
