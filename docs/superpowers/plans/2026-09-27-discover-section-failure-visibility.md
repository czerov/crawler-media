# 发现墙分区故障端到端可见 Implementation Plan

> **历史状态（2026-10-13 整理）：** 发现墙分区降级历史方案；其后另有 Hero/重试补强计划，不能将任一方案当成发现墙全部交互已验收的证明。
>
> 本文保留原设计、发现/任务 ID 与当时证据；日期本身不表示过时。旧执行/提交/清理命令仅作历史记录，不应直接重跑；复选框不是当前实现清单。当前依据：[数据库/API review](<../../audits/2026-10-13-database-api-review.md>) 与 [剩余计划](<2026-10-13-database-api-followup.md>)，未覆盖的旧 ID 仍须单独对账。

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 后端单分区失败不拖垮整页，但前端不得将失败当成功空片单缓存或隐藏；全部失败时给出可重试的页面错误。

**Architecture:** 保留现有 `GET /api/v1/discover/{kind}` HTTP 200 + `sections[]` 合同：成功空列表为正常空分区，失败分区为 `{id,title,presentation,items:[],error}`。前端保留 `error`；若本次响应含失败分区则不缓存整页 `pageCache`，失败片单也不得进入 `popularCache` 或 `collectionCache`；展示部分失败可重试区块，全部失败用现有 `DiscoverError`。不改变筛选发现 `/filtered` 的 502 合同，也不为单分区失败让 20 行一起 502。

**Tech Stack:** Axum/serde_json + React SPA/TypeScript + Node `node:test`，无需真实 TMDB/豆瓣网络。

## Global Constraints

- 领域词：Media、Metadata source、Library；自有 REST `/api/v1` 信封不变。
- `row()` 不返回 `null`；失败信息仅用于 UI，不可展示第三方错误中含的 cookie/token/URL 查询凭证：后端记录内部 error，客户端返回固定可读文案与稳定 code（如 `discover.section_failed`）。
- 不缓存失败，不把 `Ok([])` 当成 `Err`；缓存成功分区和仅成功分区组成的页面可保留。重试必须产生新后端请求。
- 前端改动遵守 React/Next 规范，测试必须覆盖 TypeScript 映射、部分失败、全部失败、点击重试后成功。

## File map

| File | Responsibility |
|---|---|
| `crates/api/src/http/discover.rs` | `row()` 记录内部错误；响应安全的 `error:{code,message}`；成功空分区保留空 `items` 且无 `error` |
| `crates/api/tests/management/catalog.rs` | 修改现有 `FailingSectionCatalog` 合同测试，验证不泄露私密错误字符串 |
| `web/lib/api/discover.ts` | `DiscoverEnvelope` / `DiscoveryPageSection` 保留错误状态；失败不进 `popularCache`；成功空行照旧 |
| `web/lib/discovery-section-state.ts` | 不使用 `@/` 别名或浏览器对象的纯函数：分区 error 归一化、全失败判断、提示文案 |
| `web/components/discover-view.tsx` | 错误行、全失败、Hero 失败与重试缓存失效 |
| `web/test/discover-section-failure.test.mjs` | 导入纯函数测试映射与状态；在 UI 层使用这些输出，无 Chromium/真实网络 |

---

### Task 1: 后端区分成功空行与失败行，避免泄露上游详情

**Files:** Modify `crates/api/src/http/discover.rs`, `crates/api/tests/management/catalog.rs`.

**Interfaces:** 失败分区 `error: { code:"discover.section_failed", message:"分区暂时无法加载，请重试" }`；成功空分区 `items:[]`、无 `error`；页面仍 200。

- [ ] **Step 1 — 红测：** 调整 `discover_section_failure_degrades_gracefully_without_null_or_502`：失败分区断言 `error.code == "discover.section_failed"`、`error.message == "分区暂时无法加载，请重试"`、整个响应字符串不含 fixture 的 `"upstream connection timed out"`；成功 popular 有 item 且无 `error`。新增 `Ok(vec![])` 分区断言无 `error`、仍有 id 和 items。避免向已超限的 `catalog.rs` 继续累加测试逻辑：先从该文件提取发现墙相关测试到 `crates/api/tests/management/discover_wall.rs`，在 `main.rs` 声明模块并保持旧 fixture 使用方式（`super::catalog::test_catalog` 等已有可见接口）。
- [ ] **Step 2 — 验红：** `cargo test -p api --test management discover_section_failure_degrades_gracefully`；当前错误是 String 且返回内部详情。
- [ ] **Step 3 — 实现：** `Err(error)` 分支 `tracing::warn!(source,kind,section=id,%error,...)` 不变；JSON 的 `error` 改为 `json!({"code":"discover.section_failed","message":"分区暂时无法加载，请重试"})`，`items:[]` 不变。
- [ ] **Step 4 — 验绿：** `cargo test -p api --test management discover` 与 `cargo test -p api --test jellyfin`。
- [ ] **Step 5 — Commit：** `git add crates/api/src/http/discover.rs crates/api/tests/management/{catalog,discover_wall,main}.rs && git commit -m "fix(discover): expose sanitized per-section failure"`。

### Task 2: 前端保留失败状态但不缓存失败数据

**Files:** Modify `web/lib/api/discover.ts`; Create `web/test/discover-section-failure.test.mjs`.

**Interfaces:** `DiscoveryPageSection.error?: {code:string;message:string}`；`fetchDiscoveryPage` 返回含全部 sections 的 `DiscoveryPageData`。`browseDiscoveryCollection(ref,...,page=1)` 在失败 section 缓存未命中时绝不能返回成功空列表；错误行由 UI 根据 section.error 展示，**不依赖**抛 Promise 的行为。

- [ ] **Step 1 — 红测：** Node 测试以 fixture 模拟 3 sections：`popular` 有一条、`upcoming` 正常空、`trending-day` 为 `items:[] + error`。在纯 `web/lib/discovery-section-state.ts` 中导出 `normalizeDiscoverySectionError(value: unknown): {code:string;message:string}|undefined`（只读 `value.code/message` 两个非空字符串，否则返回 undefined）及 `shouldCacheDiscoverySection(error: {code:string;message:string}|undefined): boolean`。断言故障保留 error 且不缓存，正常空可缓存；模拟下一次响应 `error` 消失时应转为可缓存。`web/lib/api/discover.ts` 真实调用两个纯函数，并在接收响应时清理失败 ref 的旧缓存，不要在 Node 测试直接导入含 `@/` 别名的 `discover.ts`。
- [ ] **Step 2 — 验红：** `cd web && pnpm test` 中新用例因映射未保留 `error` 失败。
- [ ] **Step 3 — 实现：** 为 `DiscoverEnvelope.sections[number]` 加 `error?: {code:string;message:string}`；为 `DiscoveryPageSection` 加 `error?`。从 `web/lib/discovery-section-state.ts` 导入 `normalizeDiscoverySectionError` / `shouldCacheDiscoverySection`；只有无 error 时 `popularCache.set`，失败时 `popularCache.delete(collectionRef)`（不得伪装成旧成功）；保留 title/presentation，正常空可缓存。`discover-view` 的 `pageCache.set` 仅在 `data.sections.every(section => !section.error)` 时写入；收到失败项还需 `pageCache.delete(cacheKey)`，否则下次切换仍读取错误页缓存。
- [ ] **Step 4 — 验绿：** `cd web && pnpm exec tsc --noEmit && pnpm test`。
- [ ] **Step 5 — Commit：** `git add web/lib/api/discover.ts web/test/discover-section-failure.test.mjs && git commit -m "fix(web): preserve discover section failures without caching them"`。

### Task 3: 部分失败显示、全部失败可重试

**Files:** Modify `web/components/discover-view.tsx`; Create `web/lib/discovery-section-state.ts`; Test `web/test/discover-section-failure.test.mjs`.

**Interfaces:** `DiscoveryPageSection.error` 来自 Task 2；页面错误复用现有 `DiscoverError`。`reloadKey` 点击重试之前清 `pageCache.delete(cacheKey)`、该 provider/media 的失败 `popularCache`（Task 2 保证不缓存）、关联 `collectionCache`；正常切换来源缓存不受影响。

- [ ] **Step 1 — 红测：** 在 `web/lib/discovery-section-state.ts`（仅纯状态逻辑）设计并测试 `discoverySectionStatus(sections: ReadonlyArray<{error?:{code:string;message:string}}>): "all-failed"|"partial"|"ok"`：`error` 数等于 section 数且非零 → all-failed；0<失败数<总数 → partial；0 → ok。再导出 `discoverySectionMessage(section: {error?:{code:string;message:string}}): string|null`：失败返回「分区加载失败，重试」，正常空返回 `null`。`web/test/discover-section-failure.test.mjs` 从此文件真实导入函数；UI 在 JSX 里消费这些输出，不得只匹配源码字符串充当行为测试。
- [ ] **Step 2 — 验红：** `cd web && pnpm test` 新用例失败。
- [ ] **Step 3 — 实现：** `loadPage` 对 `error` sections 不调用 `browseDiscoveryCollection`，用 `rowsByRef[ref] = "error"` 加旁路 `section.error` 展示；Hero 失败显示小型警示而非空白；部分失败行展示 title + 错误提示 + 重试按钮。全失败立即 `setError({message:"所有发现分区暂时无法加载，请重试",code:"discover.section_failed"})`。重试 handler 先删除 `pageCache` 当前 key 与 `collectionCache` 当前失败 refs，再 `setReloadKey(k=>k+1)`；效果重新请求。正常 `Ok([])` 不标失败、不触发全失败。
- [ ] **Step 4 — 验绿：** `cd web && pnpm exec tsc --noEmit && pnpm test`; 后端 `cargo test -p api --test management discover`；最后 `cargo test --workspace`。
- [ ] **Step 5 — Commit：** `git add web/components/discover-view.tsx web/test/discover-section-failure.test.mjs && git commit -m "fix(web): display and retry failed discovery sections"`。

**验收边界：** 本任务只影响 `/discover/{kind}` 片单墙，未改变筛选发现或 `/discover/{kind}/collection/{id}` 的分页合同；手动重试不应把正常空结果误判为上游故障。
