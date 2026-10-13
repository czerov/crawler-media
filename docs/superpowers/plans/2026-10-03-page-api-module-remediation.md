# 前端页面 / API 契约与模块生命周期修改开发计划

> **历史状态（2026-10-13 整理）：** 页面/API/生命周期历史整改；10-05 follow-up 记载 T01–T12 已执行，仍需按原 ID 对照实现/验证，不能推定真实浏览器和外部服务已验收。
>
> 本文保留原设计、发现/任务 ID 与当时证据；日期本身不表示过时。旧执行/提交/清理命令仅作历史记录，不应直接重跑；复选框不是当前实现清单。当前依据：[数据库/API review](<../../audits/2026-10-13-database-api-review.md>) 与 [剩余计划](<2026-10-13-database-api-followup.md>)，未覆盖的旧 ID 仍须单独对账。

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> **当前技能目录说明：**上面的技能名是 writing-plans 模板约定；本会话可用的是 `executing-plans`，没有 `subagent-driven-development`。执行时只能加载实际可用技能，不能调用不存在的技能。可以使用普通 subagent 做独立研究/复核；Agent Teams 仅在用户明确要求时使用。

**Goal:** 修复本次两方向审计发现的页面操作不生效、播放/图廊契约错配，以及 Transfer、搜索提交、字幕恢复的生命周期漏洞；以真实输入、外部行为和持久化回读验收。

**Architecture:** 前端保持 UUID 字符串和明确的 optional/null 语义，API 适配层不丢弃页面可编辑字段、不伪造运行状态。后端使用一致的季集/观看状态 DTO；搜索在最终 admission 临界区重新读取策略，Transfer 与删除共享生命周期互斥，字幕恢复复用已落盘的视频目的地。按业务用例拆分 sibling 模块，避免为修复引入全局大锁或重新实现一套业务规则。

**Tech Stack:** React 19 + TypeScript + Vite + react-router-dom；Rust + Axum + Tokio + parking_lot；SQLite/rusqlite；qBittorrent / Transmission 通过 Downloader trait 和本地夹具测试。

## Global Constraints

- 计划基线：`3e2e0b3c17389f949337ac7d9ee35095f93a8637`；2026-10-03 审计时工作树干净。实施前重新记录 HEAD、差异和测试状态；不能假设行号永久不变。
- 本文是开发计划，不授权立即改源码、提交、推送、创建/关闭远端 issue；执行这些操作须遵守用户授权及仓库流程。
- 一次一个 issue/垂直任务；不同问题不得混成一个修复提交。编号 T01–T12 是计划编号，不是 GitHub issue 编号。
- Rust `.rs` 文件 Soft 200 / Hard 800；production function Soft 60 / Hard 120；test function Soft 80 / Hard 120；`lib.rs` 只放模块树和公开重导出。Soft 是审查提示，不是硬失败。
- `domain` 只有类型、不得 IO；`store` 拥有 SQLite；其他 crate 不得依赖 `api`。依赖和 edition 从 workspace 继承。
- 使用 [CONTEXT.md](<../../../CONTEXT.md>) 的 Media / Subscribe / Torrent / Downloader / Library / Transfer / Release 术语。
- 无生产 IO/解析 `unwrap`、`expect`，除非明确无失败可能且不是 IO 路径；错误返回必须包含操作上下文。
- 关键失败使用结构化 `error!`，降级 `warn!`，业务开始/结束 `info!`；不得记录密码、cookie、完整签名 URL。
- 每项 Rust 修改：`cargo test -p <crate> --locked --offline` 覆盖每个改动 crate，并在该 issue 结束时 `cargo test --workspace --locked --offline`。
- 前端修改：实际执行函数的请求/响应测试 + TypeScript typecheck；源码正则检查不能代替行为测试。
- 测试用 tempdir、注入 trait 和 localhost HTTP 夹具；不得依赖真实 TMDB、Downloader、Chromium或生产数据库。并发测试使用 channel/barrier，不用 sleep 猜时序。
- `/posters/*`、`/fanart/*`、`/stills/*`、`/chapters/*`、Library cover 等只读静态图片保持 public 路由；视频流继续遵守原鉴权规则。
- 保留已修好的 movie `(0,0)` → `(-1,-1)` 规范化、canonical existing row 优先、TV 不迁移等行为，不把这些作为本轮重写目标。
- 前一轮“反向默认路由漏保护”和“absent magnet 仍发删除 RPC”属于独立安全议题；本计划不把它们算作新发现或顺带修复。上线前仍需单独核验/关闭相关阻断。

---

## 1. 范围、证据与验收含义

### 1.1 审计覆盖

盘点 27 个页面入口、20 个前端 API 文件和 16 个 Rust crate；重点链路为 Subscribe、Downloader 配置、Library/Playback、搜索 admission 和 Transfer。不是全功能正确性的证明；没有浏览器端完整交互测试，没有真实外部服务操作。

| 任务 | 问题 | 优先级 | 当前证据 | 期望用户可见变化 |
|---|---|---|---|---|
| T01 | Transfer 与订阅删除竞争 | P1 | 源码时序确认，待可控并发复现 | 删除成功后不再冒出新文件、ledger、孤儿 facts |
| T02 | admission 使用旧策略/旧 pending | P1 | 源码确认，待可控并发复现 | 搜索期间改季/Downloader 后不提交旧策略任务，不重复 add |
| T03 | 创建选库丢失，调整不能清库绑定 | P1/P2 | 创建真实 serializer 已复现；调整链路确认 | 所选 Library 真正保存，默认选项能清绑定 |
| T04 | Filter PATCH HTTP 200 no-op | P2 | HTTP 独立测试复现 | 换 Filter 真正生效，错误 ID 明确拒绝 |
| T05 | 分集接口不按季过滤 | P1 | Playback HTTP 独立测试复现；Library 同类源码确认 | 本季列表/上一集下一集只落到本季有效文件 |
| T06 | 分集观看进度/已看契约缺失 | P2 | 源码链路确认 | 每个 User 的已看/续播状态正确展示 |
| T07 | 图廊收藏 UUID → Number → null | P2 | serializer + HTTP 400 复现 | 图廊/收藏灯箱点心成功保存 UUID 对应状态 |
| T08 | 图廊链接、分页、排序、筛选错配 | P2 | FE/handler 链路确认 | 正确详情链接，分页终止，图廊名单与墙一致 |
| T09 | Downloader 验证失败被吞 | P2 | 执行真实 FE wrapper 已复现 | 失败真实显示，正常配置不等于连接正常 |
| T10 | 队列/备用速度控件不生效 | P2 | FE/handler 链路确认 | 未支持能力不再显示可保存的假控件 |
| T11 | 多文件身份忽略 original_title | P2 | 独立 Rust 函数行为复现 | 合法原文标题季包能 Transfer，外来文件仍拒绝 |
| T12 | 视频成功字幕失败无法恢复 | P2 | 源码路径确认，待完整两轮恢复复现 | 重试补齐字幕，pending 最终正确终结 |

### 1.2 不可混用的成功标准

- HTTP 200 ≠ 修改生效：必须回读 DTO、数据库或 Downloader 实际设置。
- 无崩溃 ≠ 无数据丢失：必须检查文件存在性、ledger/source/facts、pending 状态。
- 只检查单季 ≠ 跨季正确：至少两个季、相同 episode number、缺集组合。
- 源码里包含字段 ≠ serializer 发出字段：必须捕获实际请求 body/query。
- 测试间谍没有调用 ≠ 安全：间谍必须实现被执行的接口，禁止用默认 unsupported error 制造假绿。

## 2. 执行顺序、依赖与文件职责

建议顺序：`T01 → T02 → T03 → T04 → T05 → T06 → T07 → T08 → T09 → T10 → T11 → T12`。

依赖：T06 依赖 T05 的 episode DTO/作用域；T08 依赖 T07 的 UUID 操作边界；T12 依赖 T01 的 Transfer 生命周期临界区，T11 在 T12 前完成以免合法文件先被误拒绝。其他任务可以独立 review，但仓库流程仍一次一个 issue。不要在并行任务中共同写相同大文件。

| 文件职责 | 现有入口 | 计划新增或提取的模块 |
|---|---|---|
| Transfer snapshot/IO/commit 生命周期 | [worker.rs](<../../../crates/api/src/worker.rs>) | `crates/api/src/worker/transfer.rs`，独立于搜索逻辑 |
| 搜索最终 admission | [finish_search.rs](<../../../crates/api/src/worker/finish_search.rs>) | 在现有模块内重新读权威状态，不复制策略判断 |
| Subscribe 表单 payload/dirty 判定 | [subscriptions.ts](<../../../web/lib/api/subscriptions.ts>)、调整 dialog | `web/lib/subscription-form.ts`，纯函数 serializer/dirty；wrapper 继续统一 http |
| 季集读取与观看状态 | [playback.rs](<../../../crates/api/src/http/playback.rs>)、[library.rs](<../../../crates/api/src/http/library.rs>) | `crates/api/src/http/playback/episodes.rs`，共享纯 DTO 构造；权限/Library 过滤由入口负责 |
| 图廊分页与图组 DTO | [library_organize.rs](<../../../crates/api/src/http/library_organize.rs>) | `crates/api/src/http/library_gallery.rs`，复用墙的名单/排序/过滤 |
| 图廊 marks target | 两个 gallery callbacks | `web/lib/gallery-mark-target.ts`，UUID string 不强制转换为 number |
| Downloader verification 显示状态 | [downloaders.ts](<../../../web/lib/api/downloaders.ts>)、配置 section | `web/lib/downloader-verification.ts`，映射本次结果与非 verified 状态 |
| Media / Release 标题身份匹配 | choose / file_identity | `crates/subscribe/src/media_identity.rs`，无 IO 的共享匹配规则 |
| sidecar 恢复 | collect / sidecars / delivery | `crates/subscribe/src/collection_destinations.rs`，typed source→dest 映射，不传松散大参数包 |

测试文件按独立用例新增，不把所有回归塞进已有接近 800 行的文件。新增 management 子模块必须注册到 [管理测试入口](<../../../crates/api/tests/management/main.rs>)；Rust 模块必须显式注册，不得出现“测试写了但没有编译运行”。

## 3. 统一任务门禁

每项任务按以下周期执行，并留下日志：

- [ ] 记录基线，读取本任务 Files、相关现有测试和依赖任务产生的接口。
- [ ] 新增会因原问题失败的行为测试；并发任务先写可控 fixture 和指定时序。
- [ ] 执行指定 focused command，确认失败原因是业务断言，而不是编译错误、缺依赖或默认 unsupported。
- [ ] 做最小实现；不要顺带重构不相关模块或改变权限范围。
- [ ] 重跑 focused test；核验实际 request/response、持久化和文件结果。
- [ ] 跑本任务 crate / 前端门禁，检查 size、结构化日志和 `git diff --check`。
- [ ] 一次新 reviewer 复核 Spec 和 Standards；达到独立验收后在用户授权下创建 scoped commit。提交信息建议见各任务；先 `git diff --name-only`，只 stage 本任务路径。
- [ ] 按仓库 issue 流程记录证据；下一任务不得依赖未完成/未关闭阻断。

前端执行命令（使用项目已有 package manager，不更改 lock）：

```bash
# 使用 pnpm 环境；若 harness 返回 bundled Node/pnpm 路径，用那两个绝对路径运行。
pnpm --dir web typecheck
node --experimental-strip-types --test web/test/subscription-form.test.mjs
# 每项 task 指定对应新增 test 文件；不要依靠 node --test 扫描 node_modules。
node --experimental-strip-types --test web/test/*.test.mjs
```

---

## Task T01：Transfer 与删除共享生命周期临界区

**Files**
- Modify: [worker.rs:376–553](<../../../crates/api/src/worker.rs#L376-L553>)，只保留 orchestration/调用。
- Create: `crates/api/src/worker/transfer.rs`，持有 Subscribe guard、权威 snapshot、Transfer commit。
- Inspect/必要时 Modify: [deletion.rs:248–313](<../../../crates/api/src/http/subscriptions/deletion.rs#L248-L313>)、[state.rs](<../../../crates/api/src/management/state.rs>)，复用已有 deletion reservation。
- Test Create: `crates/api/tests/management/transfer_deletion_race.rs`；注册 management/main。

**Interfaces**
- Consumes: `ApiState::subscribe_guard(SubscribeId)`、`subscribe_is_deleting(SubscribeId)`、Store Subscribe/pending/facts 与 `Downloader::completed_files`。
- Produces: `transfer_one(state: &ApiState, id: SubscribeId) -> Result<(), String>`；最终策略/数据来自 guard 内 Store，不来自早先复制的 Subscribe。

- [ ] **Step 1：建立两种可控时序。** `GatedCompletedFiles` 在拿到源文件后向测试发 `started` 并等待 `resume`。用 TempDir 的视频文件、Memory/假 Downloader 和假 probe；不调用真实网络。

```rust
// fixture 提供 spawned_transfer、started_rx、resume_tx；started 表示已进入 Transfer IO。
started_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
let mut deleting = tokio::spawn(delete_request());
assert!(tokio::time::timeout(std::time::Duration::from_millis(100), &mut deleting).await.is_err());
resume_tx.send(()).unwrap();
assert_eq!(deleting.await.unwrap().status(), axum::http::StatusCode::OK);
assert!(store.get_subscribe(id).unwrap().is_none());
assert!(store.ledger_for_media(media_id).unwrap().is_empty());
assert!(store.load_subscribe_facts(id).unwrap().entries().next().is_none());
// fixture 记录 Transfer 目的地，最终 assert!(!destination.exists())。
```

测试名：`transfer_started_first_is_finished_before_delete_snapshot`、`delete_reserved_first_prevents_transfer_mutation`。第二个时序先建立 deletion reservation，再放行 Transfer，assert 无新文件/ledger/facts。

- [ ] **Step 2：红灯复现门槛。** Run `cargo test -p api --locked --offline --test management transfer_deletion_race -- --nocapture`。如果原代码不复现，检查 job 是否已串行、fixture 是否真的卡在 IO seam；结论不成立则暂停实现并记录更正。
- [ ] **Step 3：实施。** Transfer 取得 per-Subscribe guard 后重新加载 Subscribe、pending、facts、Library routing；已删除/预留删除时跳过。guard 必须覆盖文件变更和 Store commit，不能只锁取 snapshot。锁顺序固定 `Subscribe guard → Store`；Store 锁仅包快 DB 操作，不包下载器网络/probe/copy。HTTP DELETE 等待放在现有 spawn_blocking，不阻塞 async executor。
- [ ] **Step 4：验收。** 上面两种时序绿灯；加 `delete_torrents=false` 保留源文件的案例、不同 Subscribe 并行案例和 Transfer 失败释放 guard 的案例。Run `cargo test -p api --locked --offline` + workspace；禁止只断言 DELETE 的 200。
- [ ] **Step 5：独立复核和 scoped commit。** 建议 `fix: serialize subscribe transfer with deletion`。不改全局 scheduler 锁，也不顺带加跨库物理删除。

## Task T02：最终 admission 内重读策略和 pending

**Files**
- Modify: [worker/finish_search.rs:18–40](<../../../crates/api/src/worker/finish_search.rs#L18-L40>)、[117–152](<../../../crates/api/src/worker/finish_search.rs#L117-L152>)、必要时 [worker.rs:81–106](<../../../crates/api/src/worker.rs#L81-L106>)。
- Inspect: [patch.rs:25–39](<../../../crates/api/src/http/subscriptions/patch.rs#L25-L39>)、[patch_fields.rs](<../../../crates/api/src/http/subscriptions/patch_fields.rs>)、[jobs_api.rs](<../../../crates/api/src/jobs_api.rs>)。
- Test Create: `crates/api/tests/management/search_policy_race.rs`，复用 pause gate fetcher模式。

**Interfaces**
- Consumes: 搜索产生的 `Vec<Torrent>`、search_keywords 和 SubscribeId；旧 Subscribe 仅是抓取快照。
- Produces: guard 内最新 Subscribe / Media / Filter / wash_filter / facts / pending 的 admission context；若目标 Media 已改变，应停止旧候选提交并交给后续搜索。

- [ ] **Step 1：写红灯测试。** 初始 S1，site fixture 阻塞；PATCH `{"selected_seasons":[2]}` 返回 200；放行返回只含 S1E1 的候选。断言不提交 S1 Torrent。另一个 test PATCH Downloader A→B，候选依然合法时只能向 B submit。准备两个 localhost Downloaders 或 route-aware fake。

```rust
// 在收到搜索 started 后，使用公开 PATCH seam：
let body = serde_json::json!({"selected_seasons": [2]});
// PATCH 返回成功后放行 fetcher，最终检查 Downloader.added() 不含 S01E01。
assert!(downloader.added().iter().all(|t| !t.title.contains("S01E01")));
assert!(store.load_pending(subscribe_id).unwrap().is_empty());
```

第三个 test：RSS 与 search 同时完成同一 Subscribe 的候选，第二个等待 guard 后重新读 pending，断言同 enclosure 只有一次 outbound add、一条 pending。

- [ ] **Step 2：确认红灯。** Run `cargo test -p api --locked --offline --test management search_policy_race -- --nocapture`。不要用暂停测试代替活跃状态策略变更测试。
- [ ] **Step 3：实施。** `load_search_context` 和 `remove_pending_candidates` 移到 guard 内；重新读当前 Subscribe 和 Filter，再调用现有 admission。不得保持旧 `filter` 引用或仅重查 tracking_state。网络检索仍在锁外；已有候选按最新 coverage/filter 重新评估。
- [ ] **Step 4：验收。** 三类竞态绿灯，现有 subscription_pause/job_concurrency/delivery 不退化；记录拒绝旧候选/跳过已 pending 的结构化日志。Run api crate + workspace。
- [ ] **Step 5：提交门禁。** 建议 `fix: reload subscribe policy at admission commit seam`；不修改 T04 的 PATCH Filter 契约实现。

## Task T03：Subscribe Library 绑定从表单到回读闭环

**Files**
- Modify: [subscriptions.ts:502–525](<../../../web/lib/api/subscriptions.ts#L502-L525>)、[subscribe-dialog.tsx:235–259](<../../../web/components/subscribe-dialog.tsx#L235-L259>)、[subscription-adjust-dialog.tsx:64–65](<../../../web/components/subscription-adjust-dialog.tsx#L64-L65>)、[135–162](<../../../web/components/subscription-adjust-dialog.tsx#L135-L162>)。
- Create: `web/lib/subscription-form.ts`，导出纯 payload/dirty 函数；`web/test/subscription-form.test.mjs`。
- Test Extend/Create: `crates/api/tests/management/subscription_contracts.rs`；先复用已有直接 HTTP library_id 测试。

**Interfaces**
- `library_id?: string | null`：undefined=未提供/不修改，null=使用默认/清绑定，string=显式 UUID。不要 number cast。
- `createSubscription` 的请求必须包含页面实际选择的 string；调整 dialog 初始值来自 `detail.library_id ?? null`。

- [ ] **Step 1：写实际 serializer 红灯。** 捕获 `request` 参数，或对抽出的生产纯函数执行：

```js
assert.equal(createBody({title_ref: 'tmdb:movie:603', library_id: 'library-B'}).library_id, 'library-B');
assert.deepEqual(libraryPatch('library-B', null), {library_id: null});
assert.deepEqual(libraryPatch('library-B', 'library-B'), {});
assert.deepEqual(libraryPatch(null, 'library-B'), {library_id: 'library-B'});
```

`createBody` 与 `libraryPatch` 由本任务新纯模块导出，API wrapper/dialog 必须实际调用它们；不得只测试未消费的辅助函数。
- [ ] **Step 2：红灯。** Run `node --experimental-strip-types --test web/test/subscription-form.test.mjs`；直接 HTTP 回归确认 B 绑定和 null 清除可用。
- [ ] **Step 3：实施。** 创建 request body保留 library_id；dirty 比较初始绑定与当前值，不用 `libraryId !== null` 判断。保存按钮对未改变值不发 PATCH。普通 member 权限保持原后端校验，不开放管理能力。
- [ ] **Step 4：验收。** 创建选 B→返回/Store library_id=B→Transfer 路由 B；调整 B→default 发 null 并回读 null；B→B 不 dirty；弹窗重新打开显示真实绑定。Run test、typecheck、api 相关 tests。
- [ ] **Step 5：提交门禁。** 建议 `fix: preserve subscribe library binding through UI`。

## Task T04：Filter PATCH 真正修改且拒绝非法引用

**Files**
- Modify: [types.rs:76–102](<../../../crates/api/src/http/subscriptions/types.rs#L76-L102>)、[patch_fields.rs:12–25](<../../../crates/api/src/http/subscriptions/patch_fields.rs#L12-L25>)、[subscriptions.ts:590–612](<../../../web/lib/api/subscriptions.ts#L590-L612>)、[inspector:865–876](<../../../web/components/subscription-inspector-view.tsx#L865-L876>)。
- Test: `crates/api/tests/management/subscription_contracts.rs`、`web/test/subscription-form.test.mjs`。

**Interfaces**
- PATCH 新增 `filter_id: Option<String>`，前端 `rule_set_id` 使用 string；缺省不修改，null/空串拒绝，UUID 必须对应存在的 Filter。
- 使用现有 Subscribe owner/admin Policy 校验；普通 member 不应因新字段获得额外全局管理权限。

- [ ] **Step 1：红灯 HTTP。** 插入 Filter A/B，创建使用 A 的 Subscribe；PATCH B，断言响应和 Store 均 B。非法 UUID/不存在 B 返回 400且原 A不变。

```rust
let patch = serde_json::json!({"filter_id": filter_b.id.to_string()});
// 使用管理 router PATCH 后：
assert_eq!(store.get_subscribe(subscribe_id).unwrap().unwrap().filter_id, filter_b.id);
// 未知字段如 filter_idx 不允许 HTTP200假成功；返回明确错误且不写库。
```

- [ ] **Step 2：Run** `cargo test -p api --locked --offline --test management subscription_contracts -- --nocapture`；FE serializer test确认 filter_id保留 UUID。
- [ ] **Step 3：实施。** 解析/验证 FilterId并更新 Subscribe，使用现有搜索重排/下一轮调度策略；先校验完整 PATCH，再写库。审查兼容字段后为该写 DTO添加 `deny_unknown_fields`，或者在入口显式拒绝未支持 key；不无差别给全部 read DTO加 strict。
- [ ] **Step 4：验收。** UI换 B→回读 B→一次候选匹配确实使用 B；400无部分写入；T02活跃搜索策略测试继续绿。Run FE门禁、api + workspace。
- [ ] **Step 5：建议提交** `fix: apply and validate subscribe filter updates`。

## Task T05：本季分集接口和播放器切集严格一致

**Files**
- Modify/Extract: [playback.rs:195–241](<../../../crates/api/src/http/playback.rs#L195-L241>) → `crates/api/src/http/playback/episodes.rs`；[library.rs:535–652](<../../../crates/api/src/http/library.rs#L535-L652>)。
- Modify: [player-page.tsx:85–134](<../../../web/components/player/player-page.tsx#L85-L134>)、[playback API:878–886](<../../../web/lib/api/playback.ts#L878-L886>)、[Library API:1355–1372](<../../../web/lib/api/libraries.ts#L1355-L1372>)。
- Test Create: `crates/api/tests/management/episode_contracts.rs`、`web/lib/player/episode-navigation.ts`、`web/test/episode-navigation.test.mjs`。

**Interfaces**
- Playback `?season_number=N`；Library `?season=N`，保留现有两种入口参数名，内部解析到 `u32`。显式非法季号返回 400；季0合法。缺省参数维持既有 all-season兼容读，不用于本季播放器。
- 返回每条 row 的 `season_number`；Playback 顶层 `season_number` 为请求季号，不固定 null。相同 `(season,episode)` 多文件聚合 file_ids，而不是重复集卡。

- [ ] **Step 1：红灯 fixture。** 同 Media仅有 S1E1和S2E2，两个 HTTP入口请求S1，结果只能有S1E1；补S1E3，next应为S1E3，不从S2拼出S1E2。

```js
assert.equal(nextOwnedEpisode([
 {season_number: 1, episode_number: 1, owned: true},
 {season_number: 2, episode_number: 2, owned: true},
], 1, 1), null);
```

`nextOwnedEpisode` 为本任务纯函数，player-page真实调用；上一集用同一函数或对称纯函数，不再仅筛episode_number。
- [ ] **Step 2：Run** `cargo test -p api --locked --offline --test management episode_contracts -- --nocapture` + Node导航test，确认原跨季bug红灯。
- [ ] **Step 3：实施。** 权限/Library ownership筛选后按请求季过滤，再以 `(season,episode)` 聚合。TMDB metadata按对应季/集取，不把S1 metadata套到S2；player再做 season defensive filter。
- [ ] **Step 4：验收。** 多季相同E1、缺集、Special S0、多版本、隐藏Library、缺失文件；UI切集只指向可播放本季单元。Run api + workspace + FE门禁。
- [ ] **Step 5：建议提交** `fix: scope episode lists and player navigation by season`。

## Task T06：分集 User 观看状态统一 DTO

**Files**
- Modify: T05提取的episodes模块、[library.rs:634–645](<../../../crates/api/src/http/library.rs#L634-L645>)、[libraries.ts:141–156](<../../../web/lib/api/libraries.ts#L141-L156>)、Library item detail分集消费处。
- Test: `crates/api/tests/management/episode_contracts.rs`、`web/test/episode-navigation.test.mjs`。

**Interfaces**
- 每集输出 `position_ms: i64`、`played: bool`、`progress_percent: Option<i64>`，不得固定 watched=false。若其他客户端仍消费 watched，过渡期可同时输出 `watched=played`，并在契约记录弃用，不突然删除。
- UnitState用已认证User和实际季/集查询；未看/匿名为零/default；不能共享另一个User进度。

- [ ] **Step 1：红灯。** User A S1E1 position=300000 duration=1200000，User B无记录；assert A position=300000、played=false、percent=25，B零/null。完成后played=true、percent=null；未提供duration时percent=null。

```rust
assert_eq!(episode["position_ms"], 300_000);
assert_eq!(episode["played"], false);
assert_eq!(episode["progress_percent"], 25);
```

- [ ] **Step 2：Run** episode_contracts，必须先因固定零/字段缺失失败。
- [ ] **Step 3：实施。** 两个入口共享watch字段构造；用Store批量读取本Media单位，避免每集重复网络或大量独立query。仅未完成有效duration计算percent，避免除零/越界；不改canonical movie行为。
- [ ] **Step 4：验收。** 单季也能展示，重进页面不丢状态，A/B隔离、已完成、无duration和重置状态都正确；FE读取played/percent无需猜watched。Run api + FE门禁 + workspace。
- [ ] **Step 5：建议提交** `fix: expose per-user episode watch state consistently`。

## Task T07：图廊 marks UUID全程string

**Files**
- Modify: [库图廊收藏](<../../../web/components/library-detail-view.tsx#L875-L892>)、[收藏图廊](<../../../web/components/favorites-view.tsx#L485-L500>)、[marks API](<../../../web/lib/api/playback.ts#L922-L962>)。
- Create: `web/lib/gallery-mark-target.ts`、`web/test/gallery-mark-target.test.mjs`。
- Test: episode_contracts或单独 `crates/api/tests/management/gallery_contracts.rs`，marks HTTP正确UUID回读。

**Interfaces**
- `galleryMarkTarget(mediaItemId: string)`返回 `{media_item_id: mediaItemId}`；UUID不得Number/parseInt。当前marks写入口要求as_str；若其他API保留number legacy，只在明确适配边界转换，不影响UUID。

- [ ] **Step 1：红灯。** 捕获两处callback实际请求路径；至少运行生产函数，不只测试JS Number语义。

```js
const id = '11111111-1111-1111-1111-111111111111';
assert.deepEqual(JSON.parse(JSON.stringify(galleryMarkTarget(id))), {media_item_id: id});
```

- [ ] **Step 2：Run** Node test，旧生产代码请求null必须被捕获；后端null案例保持400。
- [ ] **Step 3：实施。** 两处callback移除Number；必要时marks写类型收紧为string并迁移全部真实调用。保留乐观更新+失败回滚+toast。
- [ ] **Step 4：验收。** Library图廊加心、favorites图廊取消心，Store和GET marks一致；失败不会假保存；其他User不可修改对方状态。Run FE tests/typecheck和API marks回归。
- [ ] **Step 5：建议提交** `fix: preserve UUIDs in gallery favorite mutations`。

## Task T08：图廊完整DTO、稳定分页及墙名单一致

**Files**
- Extract/Modify: [library_organize.rs:266–329](<../../../crates/api/src/http/library_organize.rs#L266-L329>) → `crates/api/src/http/library_gallery.rs`，注册HTTP模块和现有route。
- Modify: [libraries.ts:1166–1180](<../../../web/lib/api/libraries.ts#L1166-L1180>)、[video-gallery.tsx:212–219](<../../../web/components/video-gallery.tsx#L212-L219>)、[library-detail loadMore](<../../../web/components/library-detail-view.tsx#L787-L807>)。
- Test: `crates/api/tests/management/gallery_contracts.rs`、`web/lib/gallery-detail-link.ts`、`web/test/gallery-contracts.test.mjs`。

**Interfaces**
- `LibraryGalleryGroup`必须含 `library_id:string`、`media_item_id:string`、`title:string`、`images`及当前User favorite状态。
- image的 `season`、`episode`明确number|null；无单位为null，不得undefined query。
- GET保留数组响应兼容；支持 `limit`、`offset`、`sort`、`order`、现有LibraryFilter各键。分页单位为图组/Media，基于可展示图组的稳定名单，tie-break用MediaId。最后页少于limit，EOF=[]。

- [ ] **Step 1：红灯。** 建3个有artwork Media，`limit=2 offset=0`返回2，offset=2返回1，offset=3为空；两页无重叠、重复请求顺序一致。过滤/排序与墙相同；所有group有library_id。

```js
assert.equal(galleryDetailHref({library_id:'lib-A',media_item_id:'media-A'}, {season:null,episode:null}), '/library/lib-A/item/media-A');
assert.equal(galleryDetailHref({library_id:'lib-A',media_item_id:'media-A'}, {season:2,episode:3}), '/library/lib-A/item/media-A?season=2&episode=3');
```

- [ ] **Step 2：Run** API gallery_contracts + Node gallery-contracts；确认原handler忽略limit导致长度失败，原链接undefined失败。
- [ ] **Step 3：实施。** serializer调用现有filterQuery/order参数；handler复用墙的权限、ownership、过滤、排序，只替换投影为图组；别另写不一致的watch/quality名单。组装图片后稳定分页或使用能保证无空页的可展示Media索引。链接只在season/episode同时非null且为数字时加query。
- [ ] **Step 4：验收。** 大于一页时滚动最终停下；排序/过滤切换重置窗口和offset；Library详情链接正确；favorites跨库仍用每组自己的library_id；隐藏库/无图Media不泄露。Run FE + API + workspace。
- [ ] **Step 5：建议提交** `fix: align library gallery DTO pagination and filters`。

## Task T09：Downloader验证状态不再伪装active

**Files**
- Modify: [downloaders.ts:394–400](<../../../web/lib/api/downloaders.ts#L394-L400>)、[instances.rs:43–55](<../../../crates/api/src/http/downloaders/instances.rs#L43-L55>)、[316–319](<../../../crates/api/src/http/downloaders/instances.rs#L316-L319>)、downloader配置section。
- Create: `web/lib/downloader-verification.ts`、`web/test/downloader-verification.test.mjs`；API `crates/api/tests/management/downloader_verification.rs`。

**Interfaces / 决策**
- 配置存在/enabled ≠ 连接验证成功。当前verify HTTP200业务结果 `{ok:false,error}`可保留，但前端必须消费结果。
- 本切片优先不新增Store schema：列表status使用明确 `pending`/未验证语义，不硬编码active；本次verify返回的成功/失败合并到UI；refresh后不假称持续验证状态。如果要跨重启保存verified_at/error，另开任务并为config revision失效设计迁移，不在本次暗加。

- [ ] **Step 1：红灯mock。** verify返回false后GET返回active，生产wrapper最终必须failed/error，而不是无条件GET覆盖。

```js
assert.deepEqual(verificationState({ok:false,error:'bad credentials'}), {status:'failed',last_error:'bad credentials'});
assert.deepEqual(verificationState({ok:true,error:null}), {status:'active',last_error:null});
```

生产wrapper实际调用该函数；另mock网络reject保持error显示。
- [ ] **Step 2：Run** Node verification test和localhost API wrong credentials fixture。禁止真Downloader地址。
- [ ] **Step 3：实施。** 保留业务failure并展示；page创建/更新后明确执行verify或撤掉“已自动验证”文案；status和enabled分开。修改配置立即失效旧verified状态，不能保留上次active。
- [ ] **Step 4：验收。** wrong credentials/unreachable失败可读，正确地址active，GET未测试配置不伪active；不回显秘密。验证connect移入spawn_blocking，避免同步连接阻塞async worker。
- [ ] **Step 5：建议提交** `fix: surface downloader verification failures faithfully`。

## Task T10：撤掉不支持的队列/备用速度假保存

**Files**
- Modify: [limits wrapper](<../../../web/lib/api/downloaders.ts#L295-L349>)、downloader-config-section队列与alt-speed控件、[SetLimitsInput](<../../../crates/api/src/http/downloaders/instances.rs#L356-L362>)必要时仅收紧契约。
- Test Create: `web/test/downloader-limits-capabilities.test.mjs`；已有 [api-contracts测试](<../../../web/test/api-contracts.test.mjs#L40-L52>)补行为回读而不是只查id。

**Interfaces / 方案选择**
- 本次修复采用明确降级：仅开放已实现的download/upload speed；queue/alt speed标明未支持且不允许编辑/保存，不再把null伪装成实际关闭。
- 全功能qB/Transmission queue/alt-speed适配作为后续独立feature；当前审计没有给出跨客户端可移植语义，不猜字段映射。

- [ ] **Step 1：红灯。** mock后端仅两速度，UI可编辑字段集合只含这两项；serializer没有让用户保存queue字段的入口，null显示未知/未支持而非disabled实际值。

```js
assert.deepEqual(editableLimitFields({speed:true,queue:false,alt_speed:false}), ['download_limit_bytes','upload_limit_bytes']);
```

`editableLimitFields`是本任务纯capability函数，配置section真实使用；速度null=unlimited在现有API边界明确转0。
- [ ] **Step 2：Run** Node capabilities test。原代码允许queue/alt编辑时失败。
- [ ] **Step 3：实施。** API返回/内部capability区分supported和value；不为未支持字段填false。保存后真实回读两速度；不显示“队列已生效”。
- [ ] **Step 4：验收。** 两速度roundtrip、unlimited、错误值返回可读错误；不支持项不能假保存；不改变instance ID选择。Run FE + api limits回归。
- [ ] **Step 5：建议提交** `fix: gate unsupported downloader queue controls`。

## Task T11：准入与多文件身份使用同一Media标题规则

**Files**
- Modify: [file_identity.rs:43–92](<../../../crates/subscribe/src/file_identity.rs#L43-L92>)、[choose.rs:330–354](<../../../crates/subscribe/src/choose.rs#L330-L354>)。
- Create: `crates/subscribe/src/media_identity.rs`，注册lib模块；Test Extend [file_identity.rs tests](<../../../crates/subscribe/tests/file_identity.rs>)、candidate_matching。

**Interfaces**
- `media_title_matches(media: &Media, release_title: &str) -> bool`，内部复用现有normalize/title matcher及 `media.title`、`original_title`。search_keywords fallback仍仅用于admission，不把任意keyword当文件身份别名。
- 年份冲突和外来文件拒绝保持不变；generic stem特殊规则不能扩大到可疑明确标题。

- [ ] **Step 1：红灯。** Media中文title+英文original_title，匹配英文多集/多视频文件应Some；同年另一电影应None；中英文标点、The前缀和年份冲突分别测。

```rust
media.title = "黑客帝国".into();
media.original_title = Some("The Matrix".into());
let r = release::parse("The.Matrix.1999.1080p.mkv");
assert!(subscribe::file_identity::resolve_file_release(&sub, &media,
    std::path::Path::new("The.Matrix.1999.1080p.mkv"), &r, &r, false).is_some());
```

- [ ] **Step 2：Run** `cargo test -p subscribe --locked --offline --test file_identity -- --nocapture`，必须先观察原文标题被拒绝。
- [ ] **Step 3：实施。** 提取共享纯标题matcher，file_identity使用它；不把中文/英文不匹配都当generic、不放宽year。通过真实multi-video collect seam补一个Transfer ledger结果测试。
- [ ] **Step 4：验收。** valid原文季包能产生ledger/facts，foreign同年/错年仍拒绝；单文件/多文件和generic命名旧行为不退化。Run subscribe、若修改api则api、workspace。
- [ ] **Step 5：建议提交** `fix: match original titles consistently during transfer`。

## Task T12：字幕失败后可恢复的收集闭环

**Files**
- Modify: [collect.rs:37–78](<../../../crates/subscribe/src/collect.rs#L37-L78>)、[sidecars.rs:40–53](<../../../crates/subscribe/src/sidecars.rs#L40-L53>)、[83–109](<../../../crates/subscribe/src/sidecars.rs#L83-L109>)、[delivery.rs:235–252](<../../../crates/api/src/delivery.rs#L235-L252>)及T01 transfer commit。
- Create: `crates/subscribe/src/collection_destinations.rs`，保存本轮和既有source→dest typed关联。
- Test Create: `crates/subscribe/tests/sidecar_retry.rs`、`crates/api/tests/management/transfer_retry.rs`，注册management入口。

**Interfaces / 恢复规则**
- typed destination包含source_path和ledger_path；从Store现有ledger source回读，不猜新命名、不复制视频。
- active pending导入是否完成依据实际目标视频和必需sidecar是否处理完，而非“本轮复制了至少一段视频”。已imported pending仍不得自动恢复用户明确删除的Library文件。
- 单个字幕无法匹配视频时必须明确warn/result分类；必需sidecar失败保留active并可重试，不能静默Ok再无限active。

- [ ] **Step 1：红灯两轮fixture。** 第1轮合法视频成功、字幕目标路径用同名directory制造写入失败；保持源/视频ledger可观测。删除阻塞directory后第2轮重试，应补字幕且不复制/重命名视频、pending imported。

```rust
assert_eq!(first_ledger.len(), 1); // 第1轮部分成功不能回滚已成功视频
assert_eq!(store.load_pending(subscribe_id).unwrap().len(), 1);
// 移除fixture明确创建的字幕阻塞directory，确保路径验证后再执行。
assert!(subtitle_destination.is_file()); // 第2轮后
assert_eq!(store.ledger_for_media(media_id).unwrap().len(), 1);
assert!(store.load_pending(subscribe_id).unwrap().is_empty());
assert_eq!(store.load_pending_state(subscribe_id, "imported").unwrap().len(), 1);
```

- [ ] **Step 2：Run** subscribe sidecar_retry + api transfer_retry。若真实实现已能恢复，先核验source filter是否命中，复现不成立就修正结论，不新增恢复机制。
- [ ] **Step 3：实施。** 保留视频source去重，但给subtitle处理补充既有destination映射；collect返回结构明确already-owned视频目的地与失败sidecars。所有sidecar处理成功后允许终结active，不要求本轮new ledger。使用T01 guard和commit保证生命周期一致。
- [ ] **Step 4：验收。** 同视频字幕单次失败→重试、TV多集部分成功、语言后缀、多字幕、无字幕任务、Library用户删除不被自动复活、重复tick幂等；权限/disk错误真实日志。Run subscribe + api + workspace。
- [ ] **Step 5：建议提交** `fix: retry sidecar transfer using persisted video destinations`。

---

## 4. 契约与测试策略收口

### 4.1 以业务调用链为中心，不再用假默认值掩盖断口

- 写DTO包含页面真实可编辑字段，未支持字段拒绝/禁用，禁止200 no-op。
- UUID一律保持string；不能靠 `as unknown as number` 消除类型错误。
- `undefined` / `null` / string含义文档化，并测试创建、修改、清除三个动作。
- 服务端响应不伪造active、played=false、position=0代替真实状态；未知值与未支持能力分开。
- 图廊与墙共用权限、过滤、排序名单；DTO字段放在消费者实际读取的层级。
- 异步业务最后权威写入seam重新确认版本/存在性；不能用一次guard弥补之前读取的过期状态。

### 4.2 行为测试方式

现有15个前端api/search契约测试在审计时全绿，但大部分是readFile+正则，未证明payload和值正确。保留有用的结构测试，新增执行生产函数/捕获request的测试：优先纯函数模块+Node strip-types，不为整个应用新装测试框架。需要跨模块import时用项目已有TypeScript transpile能力和明确mock http；不得复制一份serializer到测试里。

HTTP集成测试通过router.oneshot，无需启动新server。并发测试注入Fetcher/Downloader/probe barrier；测试开始/完成都由channel确认，超时只作为死锁失败门槛。外部请求错误是本轮bug的测试对象，允许断言具体outbound add/delete，但同时核验pending/ledger/file。

## 5. 发布与回滚门禁

- [ ] 每项task有红→绿日志、关联issue、真实回读结果和独立review。
- [ ] 前端behavior suite + typecheck绿；api/subscribe/downloader等实际改动crate独立可测；workspace绿。
- [ ] `git diff --check`绿；每个新/修改Rust函数≤120，文件≤800；按职责拆分而不是机械切行。
- [ ] 未引入静态图片鉴权回归、User进度泄漏、跨Library文件删除、错误Downloader投递。
- [ ] 未使用生产账号、真实下载器和线上文件验证；失败fixture的路径/线程均可追踪。
- [ ] T01/T02所有并发门槛完成；已知删除安全议题由单独任务关闭，不能以本计划其他任务绿灯替代。
- [ ] UI smoke：创建非默认Library订阅、调回default、切Filter、图廊点心/详情/分页、切季下一集、已看进度、Downloader失败验证、unsupported设置禁用。
- [ ] API优先兼容部署：新增filter PATCH / gallery字段 / watch字段向后兼容；前端随后发布。使用独立commit回滚；跨库已有文件不因策略变更自动搬迁。
- [ ] 如某项需要schema迁移，先独立迁移/恢复测试和用户数据备份方案；本文当前方案不要求新增schema，不把迁移藏在普通patch中。

## 6. 自检记录与执行交接

**覆盖：**前端10项审计发现由T03–T10覆盖（Library创建/调整合并、图廊链接/分页合并）；模块4项由T01、T02、T11、T12覆盖。没有将已知清理安全问题或未复现的电影收藏猜测混入新任务。

**依赖：**T05产出季集语义供T06；T07产出UUID marks边界供T08；T01生命周期约束供T12；T11统一合法文件匹配供T12恢复fixture。未定义的新增helper必须在对应任务中实现并被生产调用；测试fixture给出的id/path从fixture输出，不使用不明全局。

**证据：**serializer probes确认Library丢失/verify吞错/UUID null；独立Rust checks确认跨季列表、Filter PATCH no-op、original-title拒绝。Transfer/搜索并发和字幕重试尚未完整运行复现，已在任务Step2设停止门槛。全量测试通过不是安全证明。

**执行选择：**
1. **逐任务子代理辅助**：主代理负责一个task的实现/集成，普通subagent独立复现或两轴review；共享文件先顺序，不创建Agent Teams。
2. **当前会话顺序执行**：加载可用 `executing-plans`，每项task在红灯、绿灯、review三个节点汇报，再进入下一项。

推荐从T01复现门槛开始；如需先交付低风险页面修复，可先做T03/T07，但不改变文件生命周期P1是最终上线阻断的结论。文档交付不等于这些修复已完成。
