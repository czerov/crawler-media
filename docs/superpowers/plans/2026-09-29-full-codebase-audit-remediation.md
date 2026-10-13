# 全代码库审计修复 Implementation Plan

> **历史状态（2026-10-13 整理）：** 广域审计整改状态 UNRECONCILED：D/A/W/L/P/U/C 与 T01–T27 需要逐 ID 核对实现和验收。后续数据库/API review 没有覆盖或关闭全部旧发现。
>
> 本文保留原设计、发现/任务 ID 与当时证据；日期本身不表示过时。旧执行/提交/清理命令仅作历史记录，不应直接重跑；复选框不是当前实现清单。当前依据：[数据库/API review](<../../audits/2026-10-13-database-api-review.md>) 与 [剩余计划](<2026-10-13-database-api-followup.md>)，未覆盖的旧 ID 仍须单独对账。

> **规模规则更新：** 文中的 60 行硬限/整改目标是历史要求；当前 [AGENTS.md](<../../../AGENTS.md>) 为生产函数软限 60、测试软限 80、两者硬限 120。历史测试/行数记录不改写，规模合规不等于功能安全。

> **For agentic workers:** 使用当前可用的 `executing-plans` 技能按任务执行。步骤用复选框追踪；只完成一个独立任务的红测、实现、验证和提交后，才进入下一个任务。可在同一任务内并行研究与审查，不并行修改共享文件。

**Goal:** 修复全项目审计中可触发的数据破坏、权限、路由与业务错误，补齐页面/API 契约和启动诊断，保证未实现能力不再伪装可用。

**Architecture:** 分成四个独立验收的子项目：数据与权限、文件与下载业务链、页面/API、配置与部署。统一复用 Library 唯一归属和已投递 Downloader 路由，在破坏性操作前完成校验；所有外部 IO 使用真实 deadline。采用 fake HTTP/CDP、临时 SQLite/文件和真实 React 行为测试验证，不以源码正则或空回执作为验收。

**Tech Stack:** Rust 2024 workspace、Axum、Tokio、rusqlite 0.37、ureq 3、tungstenite、React 19、React Router 7、TypeScript、Vite 7、Node test；新增前端行为测试采用 Vitest 3、jsdom 26、Testing Library React 16。

## Global Constraints

- 依据：[全代码库审计报告](<../../audits/2026-09-29-full-codebase-audit.md>)；源码基线 `db73bf03e8eb454d24522b35ac8601bba4b127f3`，执行前检查当前 HEAD 和工作树漂移。
- 本计划阶段不修改源码；执行期间不自动停止用户服务、不修改 `data/live`、不使用用户真实凭据或媒体作为测试数据。
- 遵守 [AGENTS.md](<../../../AGENTS.md>) 和 [CONTEXT.md](<../../../CONTEXT.md>)；若使用仓库 issue 流程，一个 issue 完成、提交、推送和关闭后再进入下一个。没有网络权限时如实报告推送/关闭未完成，不伪造结果。
- `.rs` 文件硬限 800 行，函数/方法硬限 60 行；fixture 字节豁免，测试逻辑不豁免。触及超限函数时按职责拆分，禁止压缩排版假合规。
- 静态 artwork 保持公共路由；视频播放维持 Jellyfin 身份与可见性契约。
- 先行为红测后实现；每个触及的 crate 单独执行 `cargo test -p <crate>`，每任务末执行一次 `cargo test --workspace`。命令失败必须调查，不能继续声称通过。
- SQLite 多文件不能假装拥有一个跨文件 SQL 事务；文件系统与数据库之间采用校验、持久化/补偿和明确失败状态，不宣称天然原子。
- 清理不扩大目标范围：`All`、空集合和非法输入是不同语义；离线、歧义和数据库错误不得触发删除。
- 不记录 cookie、token、密码、签名下载 URL 查询串；网络诊断仅显示已脱敏地址、阶段、状态码、退出码、耗时。
- 正常支持中文/英文名称变体，同时拒绝同语言续作和冲突季集；不得为一个正例重新放开任意子串/词集合子集。
- 不将默认配置的初始密码视为现有用户当前密码，不自动重设账号密码。
- 外部 Browser 通过注入的 PageSession 接缝测试，不在 cargo tests 拉起真实 Chromium。
- 每个任务独立提交；本计划给出的 test 名称是要新增的测试，用已有同文件 fixture 构造输入，不需要跨任务隐式测试 helper。

---

## 1. 范围、产品决策与阶段顺序

### 纳入本轮

审计编号 D01–D03、A01、W01–W12、L01–L05、P01、U01–U06、C01–C07 全部映射到下文任务。

### 对未实现能力的明确处理

1. **Browser rendering：** 恢复外部 CDP attach 的真实生产实现；自动下载/启动 managed Chromium 不在此次缺陷修复内。仅打开 Browser 开关但没有可用 CDP 地址时，明确返回 unsupported/configuration 错误，UI 不宣称 managed 功能可用。
2. **片源标注：** 本轮移除用户可点击入口及假回执。真实批量片源标注涉及 ledger/NFO/probe 的新功能，单独产品任务；没有真实端点前不得显示“没有未知片源文件”。
3. **Downloader 队列/备用速度：** 本轮保证所选目标的基础限速正确。尚无 trait/adapter 支持的扩展字段在弹层隐藏并声明不支持，不能提交后返回假生效。
4. **Top250 旧书签：** 现有后端只支持豆瓣 top-rated。旧 high-score 映射到 top-rated；旧 top250 显示“该旧榜单已调整”并提供 top-rated 链接，不将不同榜单静默称作 Top250。
5. **转码/trickplay/字体、旧 trash/duplicate、Site protection/pause、trailers：** 做能力/可达性清单；未实现的入口移除或显式 disabled。恢复完整新功能不偷偷混入 bug 修复。仍真实可达的错契约需要独立任务补齐，不能保留 Promise.resolve 假成功。

### 执行顺序和依赖

```text
Phase A 数据权限：T01 → T02 → T03 → T04
Phase B 文件业务：T05 → T06 → T07 → T08 → T09 → T10 → T11 → T12 → T13 → T14 → T15
Phase C 页面契约：T16 → T17 → T18 → T19 → T20 → T21 → T22 → T23
Phase D 启动部署：T24 → T25 → T26
最终验收：T27
```

依赖说明：T03 依赖 T02 的默认库不变量；T07/T08 使用 T06 的文件发布失败契约；T20 使用 T19 的分页 DTO；T17–T23 使用 T16 的真实组件测试环境；T25/T26 可以在 Phase D 中独立执行，但本计划默认串行，避免提交混杂。

## 2. 文件职责与拆分边界

| 设计单元 | 路径 | 职责 |
|---|---|---|
| Playback 清理范围 | 新建 `crates/api/src/http/playback_history_scope.rs` | 解析/校验 All、Item、Library 清理范围，不执行删除 |
| Library 默认不变量 | 新建 `crates/store/src/library_defaults.rs` | app.db 内事务维护每 kind 默认库，启动修复旧状态 |
| Library 唯一归属 | 现有 `crates/store/src/libraries.rs` | 唯一路径归属；所有删除/organize 复用，不再用裸 starts_with |
| 文件发布 | 新建 `crates/library/src/file_transfer.rs` | 临时文件、发布、Move 源保留、清理/回滚；lib.rs 仅 re-export |
| 文件身份与槽位决策 | 新建 `crates/subscribe/src/file_identity.rs`、`slot_replacement.rs` | 文件级身份、安全回退、每槽位质量批准 |
| Downloader 删除路由 | 新建 `crates/api/src/http/subscriptions/torrent_cleanup.rs` | 携带原投递 DownloaderId，不重新按当前默认猜目标 |
| 有限网络 IO | 新建 `crates/downloader/src/http.rs`、`crates/indexer/src/cdp_transport.rs` | 超时 Agent 和 CDP socket deadline；不引入 api 反向依赖 |
| Browser CDP 页面 | 新建 `crates/indexer/src/cdp_page.rs` | PageSession 生产实现、goto/cookie/content、资源释放 |
| 发现 DTO/分页 | 新建 `crates/api/src/http/discover_items.rs`、`discover_filtered.rs` | 条目字段与筛选分页；避免继续扩大 discover.rs |
| 限速目标 | 新建 `crates/api/src/http/downloaders/limits.rs` | 指定 Downloader 的有限读写，能力声明 |
| 前端行为测试 | 新建 `web/vitest.config.ts`、`web/behavior/setup.ts`、各 `*.spec.tsx` | alias/jsdom/fake fetch/observer/路由；与 Node 现有测试隔离 |
| 本地服务探测 | 新建 `scripts/test-env/http-probe.sh` | 无全局代理副作用、deadline、结果诊断 |
| 启动回归 | 新建 `scripts/test-env/tests/startup_test.py` | 临时目录和假服务测试，不启动真实用户服务 |

新文件名称可在执行时因现有同名职责模块调整，但必须先记录变更理由；不要为每个函数另建文件，也不要无关重构整个项目。

---

# Phase A：数据库、破坏性范围与权限

## T01：显式 Playback 历史清理范围（D01）

**Files:** Modify [playback_logs.rs](<../../../crates/api/src/http/playback_logs.rs>)、[Store playback](<../../../crates/store/src/playback.rs>)；Create `crates/api/src/http/playback_history_scope.rs`、`crates/api/tests/management/playback_history_scope.rs`；将新测试模块注册到 management 测试入口。

**Interfaces:** Store 现有 `clear_units/delete_logs/delete_metrics(user_id, media_ids, since)` 保持签名兼容；`None` 仅供已校验的 All，`Some(&[])` 必须返回删除0。HTTP parser 返回 `Result<HistoryScope, Response>`，其中 `HistoryScope::{All, Media(Vec<MediaId>)}`，非法 scope/id 不变成 All。

- [ ] 写 `empty_library_history_clear_preserves_other_library`、`invalid_scope_or_id_rejected_without_writes`、`all_clear_stays_user_scoped`。种入两个 User、两 Library 的 units/logs/metrics，空库清理后逐表验证保留。

```text
DELETE scope=library + empty_library -> 200, deleted_states=0, deleted_metrics=0
DELETE scope=item + bad UUID -> 400
DELETE scope=library + missing id -> 400
DELETE scope=library + unknown id -> 404
DELETE scope=unknown -> 400
DELETE scope=all -> 仅删除当前User的状态
```

- [ ] 跑 `cargo test -p api --test management playback_history_scope`，确认旧代码真实误删或接受非法输入。
- [ ] Store 在 `media_ids == Some(empty)` 提前返回0；HTTP 按 Library 唯一归属解析媒体集，数据库错误返回500，不用 `unwrap_or_default` 扩大范围；三类删除错误不报假成功。

```rust
if matches!(media_ids, Some(ids) if ids.is_empty()) {
    return Ok(0);
}
```

- [ ] 增加非空范围/时间过滤测试；涉及三个表要么单个 subscribe.db 事务完成，要么明确部分失败响应，优先抽 Store 一次事务删除。
- [ ] 执行 crate 单测和 workspace门禁；提交 `fix(playback): preserve explicit history cleanup scope`。

## T02：默认 Library 不变量与旧状态修复（D02）

**Files:** Modify [libraries.rs](<../../../crates/store/src/libraries.rs>)、[schema.rs](<../../../crates/store/src/schema.rs>)、[lib.rs](<../../../crates/store/src/lib.rs>)；Create `crates/store/src/library_defaults.rs`、`crates/store/tests/library_defaults.rs`。

**Interfaces:** 启动迁移和 CRUD 共用 `ensure_library_defaults(conn: &rusqlite::Connection) -> Result<(), StoreError>`。每个有实体的 kind 恰好一个默认；稳定候选顺序 `(sort_order,id)`。创建/删除/设默认使用同一 app.db 事务。

- [ ] 写红测：建立第二 movie 库，删除空的原默认，drop/reopen Store成功；首次 Video 库 create/reopen成功；fixture注入已有无默认和多默认，reopen后唯一默认；重复reopen不改稳定结果。

```text
movie库原default删除后 -> extra.is_default=true
首次video创建 -> is_default=true
第二video创建 -> 原default不变
损坏状态修复 -> 按sort_order,id选择，其他is_default=false
```

- [ ] 跑 `cargo test -p store --test library_defaults`。
- [ ] 启动 seed 不再只检测“有任意库”；补默认缺失与多默认修复，之后可增加条件唯一索引。先修数据后建索引，避免存量升级失败。无新增全局 Video 默认库的强制种入，只有出现该kind时维护。
- [ ] 删除默认和选替代在同事务；根目录替换失败不能残留半个新 Library。
- [ ] crate/workspace门禁；提交 `fix(store): maintain default library invariants across CRUD and reopen`。

## T03：删除有文件 Library 的归属保护（D03）

**Files:** Modify [libraries.rs](<../../../crates/store/src/libraries.rs>)、[library_config.rs](<../../../crates/api/src/http/library_config.rs>)、[library.rs](<../../../crates/api/src/http/library.rs>)；Test [library_path_ownership.rs](<../../../crates/api/tests/library_path_ownership.rs>)。

**Interfaces:** 复用 `Store::library_for_path`；`delete_library` 遇到仍归属该库的 ledger 返回 `StoreError::Protected`，HTTP 409，保留库、roots、文件和权限。本轮不提供隐式迁移/级联文件删除。

- [ ] 红测：everyone默认库+selected受限库+文件；删除受限库返回409，无权Member detail/stream仍拒绝；空受限库可以删；默认库删除测试使用空库避免与T02冲突。

```text
DELETE restricted library with ledger -> 409
before/after root ids、ledger ids、文件字节完全不变
member不能因失败删除获得新可见条目
```

- [ ] 执行 `cargo test -p api --test library_path_ownership`。
- [ ] 在任何删 roots前查询权威 ledger归属；不只判断路径前缀。无归属路径不能自动作为everyone可见的依据；保留迁移兼容逻辑需限制到已知旧数据，不能让删除触发fallback权限扩大。
- [ ] 核验播放、封面可选身份和列表一致，静态图片公共规则不变；crate/workspace门禁。
- [ ] 提交 `fix(library): block deletion that abandons owned ledger rows`。

## T04：全局 Browser 管理授权（A01）

**Files:** Modify [HTTP router](<../../../crates/api/src/http/mod.rs>)、[settings.rs](<../../../crates/api/src/http/settings.rs>)；Test [browser_settings.rs](<../../../crates/api/tests/management/browser_settings.rs>)。

**Interfaces:** GET保留必要安全状态（不输出敏感URL凭据）；PUT `/settings/browser`、POST `/settings/browser/sync-cdp` 必须经过 `require_admin`。

- [ ] 红测：匿名401、Member403、Admin正常更新；Member请求前后KV和fake进程/同步记录不变。不用真实Obscura/CDP做测试。

```text
member PUT browser enabled=true ->403, configured enabled仍false
member POST sync-cdp ->403，不执行连接/凭据写入
```

- [ ] 跑 `cargo test -p api --test management browser_settings`。
- [ ] 拆GET与写路由，写路由移admin；handler需要维护单独调用接缝时再加显式身份检查。不要仅前端隐藏按钮。
- [ ] 检查router现有超60函数，按public/member/admin注册职责拆解，不改变其他路由权限。
- [ ] crate/workspace门禁；提交 `fix(authz): restrict browser mutations and cookie sync to admins`。

---

# Phase B：文件、Downloader、Subscribe 与外部 IO

## T05：所有库文件操作使用唯一归属（L02、L04）

**Files:** Modify [library_delete.rs](<../../../crates/api/src/http/library_delete.rs>)、[library_organize.rs](<../../../crates/api/src/http/library_organize.rs>)；Test `crates/api/tests/management/library_operation_scope.rs`。

**Interfaces:** 复用Store最长root归属；organize在批量操作前验证所有源ledger owner==请求库、目标位于本库允许root且不归属其他嵌套库。不提供任意服务器文件移动API。

- [ ] 红测 A root=/media、B root=/media/private，同Media两文件；DELETE A仅删除A，B字节和ledger保留；只有B有该Media则A返回404。
- [ ] 红测 organize from=B或目标库外/另一库/通过symlink逃逸时400/409且整批无操作；合法重命名更新ledger并保留其他库。

```text
所有renames先验证，再执行；第2项非法不能先移动第1项
已存在不同目标文件 ->409，禁止无提示覆盖
```

- [ ] 跑 `cargo test -p api --test management library_operation_scope`。
- [ ] 共享归属判定；目标父目录用canonical可存在祖先+剩余组件构造规范路径，拒绝ParentDir、非预期symlink；源必须是该库ledger中的真实文件。不仅管理员身份通过就允许from/to。
- [ ] crate/workspace门禁；提交 `fix(library): enforce ownership scope for deletion and organize`。

## T06：Move 的失败发布与源保留（L01）

**Files:** Create `crates/library/src/file_transfer.rs`；Modify [lib.rs](<../../../crates/library/src/lib.rs>)；Test [transfer.rs](<../../../crates/library/tests/transfer.rs>)。

**Interfaces:** 保留 `transfer_file(src: &Path, dest: &Path, mode: TransferMode) -> Result<(), LibraryError>`；任何返回Err的预发布失败，源字节存在、原目标字节不变。Move成功后源删除，Copy/Hardlink语义不变。

- [ ] 写红测：Move到已存在目录失败后源完整；通过注入发布失败测试（文件操作trait仅测试可选）验证原目标不被删；跨设备fallback在最终发布失败时源仍在。

```rust
assert!(transfer_file(&src, &existing_directory, TransferMode::Move).is_err());
assert_eq!(std::fs::read(&src).unwrap(), original_bytes);
```

- [ ] 跑 `cargo test -p library --test transfer`。
- [ ] 推荐安全方案：Move staging先复制，完成flush和目标原子发布后才删源，牺牲短暂额外IO换一致性；同设备rename优化只能在具有完整回滚测试后引入。目标现有目录明确错误，不用remove_file(dest)盲删旧版。
- [ ] 对源删除失败明确返回可重试/部分完成状态并记录，不能把已发布文件当未发布导致再次洗版；临时文件清理有RAII/明确错误日志。
- [ ] 覆盖成功三mode、目标已存在、src==dest、并发不同发布；crate/workspace门禁。
- [ ] 提交 `fix(transfer): retain source until destination publish succeeds`。

## T07：文件级 Media 身份与安全回退（W03）

**Files:** Create `crates/subscribe/src/file_identity.rs`；Modify [collect.rs](<../../../crates/subscribe/src/collect.rs>)；Test `crates/subscribe/tests/file_identity.rs`。

**Interfaces:** 文件身份函数输入Subscribe/Media、文件Release、TorrentRelease和候选文件集合，返回 `Result<Option<Release>, SubscribeError>`。与Torrent级匹配不同：允许generic文件名回退，但不覆盖明确冲突。

- [ ] 红测：目标Torrent含Another.Movie、sample/trailer/extras和目标；错误文件不进ledger/NFO，不替换旧版。TV同S01E01但不同核心名拒绝；唯一generic `movie.mkv` 保留现有有效场景。

```text
明确不同年份/核心名/季集 ->拒绝
sample/bonus/trailer目录或文件 ->跳过，记录原因
唯一generic movie.mkv ->可采用已可信Torrent身份
多generic文件且无法证明part语义 ->不猜测，保持pending并报告歧义
```

- [ ] 跑 `cargo test -p subscribe --test file_identity`。
- [ ] 将pick_release与候选video分类抽独立模块；业务读写前完成验证。不要因显示名不同破坏正常双语或CD1/CD2，添加对应正例。
- [ ] fixture验证视频、NFO、图片、字幕不混淆；crate/workspace门禁。
- [ ] 提交 `fix(subscribe): validate completed file identity before transfer`。

## T08：多槽位 Wash-cut 不降级（W04）

**Files:** Create `crates/subscribe/src/slot_replacement.rs`；Modify [choose.rs](<../../../crates/subscribe/src/choose.rs>)、[collect.rs](<../../../crates/subscribe/src/collect.rs>)；Test `crates/subscribe/tests/multi_slot_wash_cut.rs`。

**Interfaces:** chooser可仍用any挑选Torrent供下载；Transfer对具体不可拆文件重新计算每个槽位。`SlotDecision::{Missing, Upgrade, Preserve}`；不可拆文件只要覆盖任一Preserve已有槽位，整文件不替换/删除，记录冲突并允许后续独立E02候选。

- [ ] 红测E01=100/E02缺失，新S01E01-E02=10：E01文件/事实不变，不出现score10覆盖；E01=5/E02=5，新10则两槽位正常升级。

```text
批准下载 !=批准对所有槽位覆盖
可分离单集视频 ->逐文件批准
不可拆跨多个槽位视频 ->所有已有槽位都可Upgrade才允许替换
```

- [ ] 跑 `cargo test -p subscribe --test multi_slot_wash_cut`。
- [ ] previous_paths只收实际批准覆盖槽位；ladder和score走同一判定；keep_old_versions=true仍不允许事实降级。禁止将一文件同路径高版本绕过检查。
- [ ] 验证late pending、普通fill、ladder同分更高品质；crate/workspace门禁。
- [ ] 提交 `fix(wash-cut): prevent multi-episode files from downgrading owned slots`。

## T09：删除 Subscribe 与自动 Transfer 保留投递目标（W01、W02）

**Files:** Create `crates/api/src/http/subscriptions/torrent_cleanup.rs`；Modify [deletion.rs](<../../../crates/api/src/http/subscriptions/deletion.rs>)、[subscriptions.rs](<../../../crates/api/src/http/subscriptions.rs>)、[worker.rs](<../../../crates/api/src/worker.rs>)、[delivery.rs](<../../../crates/api/src/delivery.rs>)；Test [delivery.rs](<../../../crates/api/tests/management/delivery.rs>)。

**Interfaces:** 清理目标保存 `{torrent: Torrent, downloader_id: Option<DownloaderId>}`，不降成Vec<Torrent>；依赖 `client_for_id` 和原pending路由。自动Transfer调用现有 `transfer_plan_for_library` 携带Subscribe.library_id。

- [ ] fake两个Downloader A/B同名任务，只删除pending记录的B，A无调用/文件不变；B离线保留订阅清理可重试状态，不转默认A。默认改变后老pending仍归B。
- [ ] fake两Library，后台和手动Transfer均落所选B；LibraryId不存在/不可用明确错误，不静默落A。

```text
pending.downloader_id为权威投递历史
无pending的fallback仅对明确归属目标执行；无法证明归属就不删除
```

- [ ] 跑 `cargo test -p api --test management delivery`，另覆盖subscription cleanup测试。
- [ ] 删/Transfer路径共享路由；外部IO在blocking worker，不持Store锁连网；失败不先删pending/subscription。
- [ ] crate/workspace门禁；提交 `fix(routing): preserve downloader and library targets across lifecycle`。

## T10：停用 Job 禁止后续自动重试（W05）

**Files:** Modify [lifecycle.rs](<../../../crates/jobs/src/store/lifecycle.rs>)、[store.rs](<../../../crates/jobs/src/store.rs>)、[HTTP jobs](<../../../crates/api/src/http/jobs.rs>)；Test [queue.rs](<../../../crates/jobs/tests/queue.rs>)。

**Interfaces:** def_id=None的一次性任务不受无定义限制；有def_id但disabled时，running自然完成允许，失败转终态Failed，不再queued；claim过滤disabled定义。启用后通过明确manual-run/新schedule重新生成，不复活已取消旧实例。

- [ ] 红测claim→disable→fail：没有retry queued，时间前移仍claim不到；enabled正常失败退避不变；definition-free job照常运行。
- [ ] 跑 `cargo test -p jobs --test queue`。
- [ ] 在事务内核def enabled与failure状态变更；领取加第二层防护，不仅取消当前queued。保留attempt/started_at CAS避免旧执行覆盖。

```text
disabled + running fail -> Failed(error保留), finished_at=now
enabled + under retry budget -> Queued
```

- [ ] API禁用行为fixture测试，无真实Check-in；crate/workspace门禁。
- [ ] 提交 `fix(jobs): stop automatic retries for disabled definitions`。

## T11：字幕按源视频映射和季集/语言匹配（W06）

**Files:** Modify [sidecars.rs](<../../../crates/subscribe/src/sidecars.rs>)、[collect.rs](<../../../crates/subscribe/src/collect.rs>)；Test [subtitles.rs](<../../../crates/subscribe/tests/subtitles.rs>)。

**Interfaces:** 内部 `VideoPlacement { source: PathBuf, destination: PathBuf, season: Option<u32>, episode: Option<u32> }`；字幕匹配先source stem，再唯一season/episode；保持语言/forced后缀。多个候选不默认first。

- [ ] 红测E01/E02对应中英文字幕，重命名后每集字幕字节正确，语言版本共存；歧义字幕跳过并告警，不覆盖第一集。

```text
Show.S01E02.zh.srt -> 目标E02.zh.srt
Show.S01E02.en.srt -> 目标E02.en.srt
未知字幕 + 两视频 ->不选择first
```

- [ ] 跑 `cargo test -p subscribe --test subtitles`。
- [ ] collector记录源到目标的映射，调用字幕处理；唯一视频generic字幕仍可回退。应用T06安全发布。
- [ ] crate/workspace门禁；提交 `fix(subtitles): match sidecars through source video placement`。

## T12：安全任务身份和 qB 临时字节隔离（W07、W08）

**Files:** Modify [identity.rs](<../../../crates/downloader/src/identity.rs>)、[qbit.rs](<../../../crates/downloader/src/qbit.rs>)；Test [identity.rs](<../../../crates/downloader/tests/identity.rs>)、[qbit.rs](<../../../crates/downloader/tests/qbit.rs>)。

**Interfaces:** `torrent_matches_snapshot`公开签名不变。电影名归一化相等为主；双语变体只允许单侧附加不同文字系统的完整别名，拒绝任意额外同语言token。缺核心名且原始名不同不能靠空字符串放行。后续稳定infohash是独立增强，不靠猜hash。

- [ ] 红测拒绝 Matrix/Matrix Reloaded、Alien/Alien Covenant、相同季集不同作品、缺标题解析但不同名；正例中文.The.Matrix双向、大小写标点、冲突年份/大小仍拒绝。
- [ ] 并发fake qB用barrier阻塞上传，两个合法fixture带不同Torrent bytes，断言各请求内容与调用目标一致；不是只mock调用次数。

```text
tmp文件：每次调用独立，或multipart直接拥有bytes
并发上传任何交错 ->A字节不出现于B请求
打开临时文件失败 ->明确Err，不能发送无torrents/urls的form
```

- [ ] 跑 `cargo test -p downloader --test identity` 和 `cargo test -p downloader --test qbit`。
- [ ] 使用生产tempfile依赖或受控create_new+nonce+RAII清理；添加失败日志，签名URL不泄露。抽form构建避免函数超限。
- [ ] crate/workspace门禁；提交 `fix(downloader): preserve strict identity and isolate torrent upload bytes`。

## T13：网络 deadline、Site proxy 与 CDP IO（W09、W11）

**Files:** Create `crates/downloader/src/http.rs`、`crates/indexer/src/cdp_transport.rs`；Modify [qbit.rs](<../../../crates/downloader/src/qbit.rs>)、[cdp.rs](<../../../crates/indexer/src/cdp.rs>)、[plugins.rs](<../../../crates/hooks/src/plugins.rs>)、[check_in.rs](<../../../crates/api/src/check_in.rs>)；Test `crates/downloader/tests/http_deadline.rs`、`crates/indexer/tests/cdp_deadline.rs`、[login_checkin.rs](<../../../crates/hooks/tests/login_checkin.rs>)。

**Interfaces:** `HttpPost::post`新增Site上下文/显式proxy（优先参数 `site: &Site`）；HTTP global timeout默认5s，测试可注入100–300ms；CDP discovery+connect+handshake+read/write共用Instant deadline。socket timeout必须实际设到TcpStream，elapsed while不是deadline。

- [ ] 写fake listener：接受后不回header、header后不回body、WebSocket握手/响应挂起，调用在测试deadline+小容差内结束；close server线程可join，不永久后台挂着。
- [ ] fixture Site.proxy指向本地fake proxy，Check-in成功只经proxy，直连路径未命中；代理认证不输出日志。

```rust
let remaining = deadline.saturating_duration_since(std::time::Instant::now());
stream.set_read_timeout(Some(remaining))?;
stream.set_write_timeout(Some(remaining))?;
```

- [ ] 执行三个涉及crate定向测试，验证旧路径超时；测试有独立watchdog防旧代码永久卡测试进程。
- [ ] qB预取采用有限Agent，body读取受global截止；CDP handshake使用已有限TcpStream构造WebSocket，处理ping/无关事件时刷新remaining而不重置总预算；HTTPS/WSS不支持时明确报错，不能假成功。
- [ ] 更新所有HttpPost fake调用点；crate/workspace门禁。
- [ ] 提交 `fix(io): bound network stages and honor site maintenance proxy`。

## T14：season pack 与 resolution 边界（W10）

**Files:** Modify [boundary.rs](<../../../crates/release/src/boundary.rs>)；Test [parse.rs](<../../../crates/release/tests/parse.rs>)。

**Interfaces:** `release::parse`签名不变；S01后无E的数字，仅在明确完整集号语法和尾边界时解析，不消费分辨率/年份。

- [ ] 红测 `24 S01 480p/720p/1080p/2160p WEB-DL` 全部episode=None；真实 `S01 E02`、`S01EP02`、支持的独立数字范围仍正确。

```rust
let release = release::parse("24 S01 720p WEB-DL");
assert_eq!(release.season, Some(1));
assert_eq!(release.episode, None);
```

- [ ] 跑 `cargo test -p release --test parse`。
- [ ] 数字扫描后验证非p/i/字母后缀和分隔语义，不通过 `ep<1000` 代替语法边界；回归中文范围与全季。
- [ ] release/subscribe/downloader及workspace门禁；提交 `fix(release): exclude resolution tokens from episode parsing`。

## T15：外部 CDP 的生产 PageSession 接入（W12）

**Files:** Create `crates/indexer/src/cdp_page.rs`；Modify [browser.rs](<../../../crates/indexer/src/browser.rs>)、[main.rs](<../../../crates/api/src/main.rs>)、[settings.rs](<../../../crates/api/src/http/settings.rs>)；Test `crates/indexer/tests/cdp_page.rs`。

**Interfaces:** 实现已有 `PageSession` trait，不改变Fetcher协议；构造生产opener利用T13有限transport。独立target/browser context避免站点cookie跨用户/站点混入；`goto/set_cookie_header/content`均有限时。

- [ ] fake CDP协议服务记录并应答：创建隔离target/context、设置指定站点cookie、导航、等待页面加载、取document.outerHTML、关闭target；render=false不触发CDP。
- [ ] 红测生产构造带外部cdp_url不再必然“no opener”；无CDP配置返回明确配置错误；开启managed开关但未实现不能下载空目录后宣称已安装。

```text
Browser生产fetch_html ->真实transport->返回fixture HTML
销毁/错误 ->关闭本次隔离target，不关闭用户现有浏览器/其他target
```

- [ ] 跑 `cargo test -p indexer --test cdp_page`，不启动Chromium。
- [ ] 连接主进程真实opener，读取有效运行配置；managed auto-launch明确unsupported，UI能力字段反映实际支持。不能只给tests注入fake而生产继续None。
- [ ] crate/workspace门禁；提交 `fix(browser): wire bounded external CDP page rendering`。

---

# Phase C：真实页面/API 契约

## T16：引入真实 React 行为测试门禁

**Files:** Modify [package.json](<../../../web/package.json>)、[pnpm-lock.yaml](<../../../web/pnpm-lock.yaml>)；Create `web/vitest.config.ts`、`web/behavior/setup.ts`、`web/behavior/collection-navigation.spec.tsx`。

**Interfaces:** 新命令 `pnpm test:behavior`；现有 `pnpm test`保持Node测试。Vitest include仅`behavior/**/*.spec.{ts,tsx}`，避免Node收集Vitest测试；使用@ alias与automatic JSX。

- [ ] 安装 `vitest@^3.2.0 jsdom@^26.0.0 @testing-library/react@^16.0.0` 为dev dependency，不引入生产依赖。
- [ ] 创建可执行配置和DOM清理，记录fetch mock/observer使用方式。

```ts
// vitest.config.ts
import { defineConfig } from "vitest/config";
import { fileURLToPath } from "node:url";
export default defineConfig({
  resolve: { alias: { "@": fileURLToPath(new URL(".", import.meta.url)) } },
  esbuild: { jsx: "automatic" },
  test: { environment: "jsdom", include: ["behavior/**/*.spec.{ts,tsx}"],
    setupFiles: ["./behavior/setup.ts"] },
});
```

```ts
// setup.ts
import { afterEach } from "vitest";
import { cleanup } from "@testing-library/react";
afterEach(() => cleanup());
```

- [ ] 用真实CollectionGridView+MemoryRouter挂载，mock API数据、PosterCard/scroll restoration外围，不mock组件自身逻辑；验证首屏full请求、pending刷新不发旧页追加、切换key不显示旧列表。先断言新环境能执行真实事件。
- [ ] 执行 `pnpm exec tsc --noEmit && pnpm test && pnpm test:behavior`；补test script `vitest run`。
- [ ] 提交 `test(web): add rendered component and API behavior gates`。

## T17：合集路由和分季播放（U01、P01）

**Files:** Modify [collections-detail-page.tsx](<../../../web/components/collections-detail-page.tsx>)、[playback.rs](<../../../crates/api/src/http/playback.rs>)、[player-page.tsx](<../../../web/components/player/player-page.tsx>)；Create `web/behavior/collections-route.spec.tsx`、`web/behavior/player-episodes.spec.tsx`、`crates/api/tests/management/playback_episode_scope.rs`。

**Interfaces:** 链接沿真实 `/library/:id/item/:mediaItemId`；分集API接受season_number并返回该季；前端也按season+episode双重过滤，不依赖后端单独防护。

- [ ] 红测点击合集卡片匹配真实Route而非404；fallback内部Media UUID不能当TMDB ID，无法定位Library时返回明确不可打开而非假Media详情。
- [ ] HTTP红测S1E1/S2E2请求season1只回E1；season不存在空列表；非法季号400；User不可见行不输出。
- [ ] 组件红测S1E1不生成假S1E2，S2同集号当前标题正确；正常本季E1→E3跳缺集。

```text
请求season_number=1 -> response.season_number=1，全部episodes.season_number=1
前端next/prev ->同季且owned=true
```

- [ ] 定向行为/HTTP测试验红；修改handler排序去重同季集版本、链接与previous/current/next逻辑。
- [ ] Web三门禁+api/workspace门禁；提交 `fix(web): align collection navigation and season-scoped playback`。

## T18：Metadata source ID 不混用（L03）

**Files:** Modify [catalog_fanout.rs](<../../../crates/api/src/catalog_fanout.rs>)、[catalog.rs](<../../../crates/api/src/catalog.rs>)及title_ref调用；Create `crates/api/tests/catalog_identity.rs`。

**Interfaces:** 当前 `Catalog::details(kind, tmdb_id)` 在Fanout语义为TMDB ID，必须只调用TMDB source。其他来源经source(name)与自己的external_id调用；无已确认alias不能fallback相同数字。

- [ ] fake TMDB返回Err/None、Douban同数字返回不同Media；TMDB引用不得拿DoubanMedia；明确douban引用正确；有alias时仅按已存映射选择来源。

```text
Fanout.details(Movie,"123") + tmdb失败 ->错误/None，不调用douban的"123"
source("douban").details(Movie,"456") ->豆瓣456
```

- [ ] 跑 `cargo test -p api --test catalog_identity`。
- [ ] 按调用链梳理poster/metadata等相同语义，避免只修一个函数；不改缓存source隔离。
- [ ] crate/workspace门禁；提交 `fix(catalog): preserve source namespace for external identities`。

## T19：筛选分页端到端（U02）

**Files:** Create `crates/api/src/http/discover_filtered.rs`；Modify [catalog.rs](<../../../crates/api/src/catalog.rs>)、[catalog_fanout.rs](<../../../crates/api/src/catalog_fanout.rs>)、[discover.rs](<../../../crates/api/src/http/discover.rs>)、[discover.ts](<../../../web/lib/api/discover.ts>)；Test [tmdb.rs](<../../../crates/media/tests/tmdb.rs>)、[catalog_wall.rs](<../../../crates/api/tests/management/catalog_wall.rs>)、`web/behavior/filtered-discovery.spec.tsx`。

**Interfaces:** 复用现有 `collection_paged(kind, query, page)`和TMDB discover_*_paged，不重造分页trait；HTTP返回 `{kind,page,total_pages,total_results,has_more,items}`。不支持分页source明确不支持，不伪造一页总数。

- [ ] fake两页上游，断言page参数实际到达出站URL且缓存key区分；HTTP页1 has_more=true/页2false；Web滚动/observer发第二页并追加，不重复第一页。

```text
GET filtered?genres=28&page=2 ->upstream page=2
total_results=41不可被items.length=20替代
非法page ->400；正常大页码按上游空页语义处理
```

- [ ] media/api定向测试验红；前端用fetch mock测真实browse函数，不仅纯缓存helper。
- [ ] 移除void page，URLSearchParams设置页码，DTO映射真实元数据；保持过滤/排序/语言不丢。
- [ ] Web/API/media/workspace门禁；提交 `fix(discover): paginate filtered catalog results end to end`。

## T20：发现原名、genres 和旧书签（U03、U04）

**Files:** Create `crates/api/src/http/discover_items.rs`；Modify [media parse](<../../../crates/media/src/parse.rs>)、source适配器CatalogHit构造、[discover.ts](<../../../web/lib/api/discover.ts>)、两个旧route页面；Create `web/behavior/discovery-item-fields.spec.tsx`。

**Interfaces:** CatalogHit新增默认空`genre_ids: Vec<i64>`，来源不能提供时保留空不是编造；Media已有original_title直接透传。服务端按本地缓存的genres映射展示名，不逐条详情N+1。DTO `original_title: Option<String>, genres: Vec<String>` 前端映射MediaItem。

- [ ] fakeTMDB搜索fixture含original_title、genre_ids；断言parse→HTTP→前端原名和类型未丢；挂载片单类型按钮可过滤、英文原名搜到中文项。
- [ ] high-score书签导航top-rated；top250明确说明旧榜单调整并提供链接，测试无请求movie_top250后404。

```text
title=黑客帝国,original_title=The Matrix,genre_ids=[28]
->MediaItem.originalTitle="The Matrix",genres=["动作"]
```

- [ ] 定向测试验红；补所有CatalogHit构造fixture默认值（编译帮查缺项），不从详情接口每条拉元数据。
- [ ] Web/media/api/workspace门禁；提交 `fix(discover): preserve item metadata and repair legacy collection routes`。

## T21：所选 Downloader 的基础限速与能力（U05）

**Files:** Create `crates/api/src/http/downloaders/limits.rs`；Modify [instances.rs](<../../../crates/api/src/http/downloaders/instances.rs>)、[mod.rs](<../../../crates/api/src/http/mod.rs>)、[downloaders.ts](<../../../web/lib/api/downloaders.ts>)、[downloader-config-section.tsx](<../../../web/components/downloader-config-section.tsx>)；Create `web/behavior/downloader-limits.spec.tsx`。

**Interfaces:** 新GET/PUT `/downloaders/{id}/limits`；旧全局limits保留显式默认兼容。按client_for_id选择目标；响应 `capabilities` 明确字段支持，未知/unsupported请求不能假成功。

- [ ] fake A/B不同limit，打开B弹层展示B、保存只改B；非法ID404不改A。
- [ ] 测试unsupported queue/alt字段不显示可编辑控件，直接非法写字段返回400/422，不默默丢掉。

```text
GET /downloaders/B/limits ->B limits
PUT /downloaders/B/limits {download_limit_bytes:1024,upload_limit_bytes:0} ->A不变
```

- [ ] 定向HTTP和DOM测试验红；基础字段增加单位/负值/非法字符串校验，不将坏值默认为取消限速。
- [ ] 拆超限弹层组件到同职责模块；Web/api/downloader/workspace门禁。
- [ ] 提交 `fix(downloaders): apply limits to the selected instance`。

## T22：下架假片源标注与能力契约清理（U06、遗留支架）

**Files:** Modify [libraries.ts](<../../../web/lib/api/libraries.ts>)、[media-source-annotation-dialog.tsx](<../../../web/components/media-source-annotation-dialog.tsx>)、[subscription-inspector-view.tsx](<../../../web/components/subscription-inspector-view.tsx>)、[upgrade-run-dialog.tsx](<../../../web/components/upgrade-run-dialog.tsx>)；Create `web/behavior/unsupported-capabilities.spec.tsx` 和文档 `docs/api-contracts/capabilities.md`。

**Interfaces:** 没有真实后端能力时无入口或显式disabled解释；不得候选恒[]/写入假零。移除仅为已下架功能保留的导出和调用，避免死代码“恢复入口”后再次假成功。

- [ ] 红测真实inspector/upgrade弹层不展示可点击片源标注或展示“暂不支持”disabled，不显示“没有未知片源文件”。
- [ ] 对trash/duplicate/Site保护/Playback扩展/videos按真实route与当前调用逐项填写能力矩阵：supported、unsupported-no-entry、partial；禁止静态mock假数据冒充API。

```text
unsupported能力 ->不会调用Promise.resolve假写入
若页面入口保留 ->disabled +具体原因，不显示空数据推断结论
```

- [ ] 组件行为测试验红；删除假候选/annotate回执、入口、无用dialog或改明确能力UI；不临时新增无NFO一致性保障的批量写API。
- [ ] Web三门禁；提交 `fix(web): remove unsupported annotation actions and fake receipts`。

## T23：封面输入与 Filter size 校验（L05、审计附加观察）

**Files:** Modify [color.rs](<../../../crates/cover-generator/src/color.rs>)、[library_artwork.rs](<../../../crates/api/src/http/library_artwork.rs>)、[rule_sets.rs](<../../../crates/api/src/http/rule_sets.rs>)；Test [render.rs](<../../../crates/cover-generator/tests/render.rs>)、[filters.rs](<../../../crates/api/tests/management/filters.rs>)。

**Interfaces:** Color::from_hex非ASCII/非hex返回None不panic；HTTP坏颜色返回400。size边界为空或0表示无限，非数字/溢出/反向min>max明确400，不变无界过滤。

- [ ] 写catch_unwind from_hex("0你00")红测，None；有效#RRGGBB不变。API坏颜色返回400，无文件生成副作用。
- [ ] POST size=`abc-def`/`10-2`/溢出数字返回400且不保存；`-100`、`10-`、`0-0`保持明确含义。

```rust
if !hex.is_ascii() || hex.len() != 6 {
    return None;
}
```

- [ ] 定向crate/API测试验红；校验后再字节切片，或者hex decode；parse_opt改Result而非吞parse错误。
- [ ] crate/workspace门禁；提交 `fix(validation): reject invalid colors and size filter bounds`。

---

# Phase D：启动诊断、配置与部署

## T24：启动脚本真实健康、代理保留和可复现诊断（C01–C05）

**Files:** Modify [start-test.sh](<../../../start-test.sh>)；Create `scripts/test-env/http-probe.sh`、`scripts/test-env/tests/startup_test.py`。

**Interfaces:** local_curl只局部用 `curl -q --noproxy '*'`（-q禁用户curlrc默认覆盖），有connect/max-time；不unset全局proxy、不设置NO_PROXY=*。启动以HTTP health合法body为准，TCP只诊断不作成功。wall-clock期限默认60s、环境可设；构建耗时单独计算。

- [ ] 不先重启用户服务。先创建fake后端/脚本dependency注入测试：端口随机、临时DATA_DIR/PID/log目录、fake cargo/backend，所有进程在finally清理。
- [ ] 写以下红测，记录elapsed和具体输出：

```text
broken http_proxy +NO_PROXY原值 ->local health直连，子进程proxy变量仍原值
TCP接收但HTTP500/无body ->不能报告ready
HTTP200健康 +jobs401 ->ready但独立AuthWarning，不能两个分支都ok
慢health ->真实deadline内退出，打印curl exit/status/elapsed
DATA_DIR=/tmp/x ->子进程收到/tmp/x，不含ROOT_DIR前缀
已有健康服务但PID/配置不同 ->不盲目kill或认领；报告现有实例
```

- [ ] 跑 `python3 scripts/test-env/tests/startup_test.py`（新增测试），确认旧脚本行为失败；`bash -n start-test.sh`。
- [ ] 拆 `resolve_data_dir`、`launch_backend`、`wait_backend`、`diagnose_backend`。使用 `SECONDS`或单调Python时钟获取deadline；每次curl最大时间不超过剩余预算。日志输出当前启动标记之后片段；保存最后失败exit/HTTP/body摘要，不再吞stderr。

```bash
local_curl() { command curl -q --noproxy '*' --connect-timeout 1 --max-time 2 "$@"; }
case "$DATA_DIR" in /*) data_path="$DATA_DIR" ;; *) data_path="$ROOT_DIR/$DATA_DIR" ;; esac
```

- [ ] 前端/status/mock/预热均有限deadline；qB离线只能显示离线不影响后端ready。pid进程退出包含exit code，Zombie/进程复用不能仅kill -0认健康。free_port遇未知占用默认报冲突，不任意kill用户进程。
- [ ] 登录提示为“初始密码仅首次初始化/安全迁移适用”，不打印CLI token/自定义密码；环境变更不重置旧密码。Cargo/backend启动的工作目录固定ROOT_DIR，不改变调用终端cwd。
- [ ] fake测试绿后，经用户许可才运行真实 `./start-test.sh start/restart`；保留实际命令耗时和输出，不因为本机通过承诺用户必不超时。
- [ ] 提交 `fix(test-env): diagnose bounded HTTP readiness without altering child proxies`。

## T25：运行镜像功能依赖（C06）

**Files:** Modify [Dockerfile](<../../../Dockerfile>)；Create `scripts/tests/container_tools_test.sh`。

**Interfaces:** runtime提供ffmpeg/ffprobe；若marker依赖特定fingerprint滤镜需实际验证对应能力，不假设只装包就够。无Chromium默认下载。

- [ ] 容器红测检查 `ffprobe -version`、`ffmpeg -version`和必要滤镜；临时生成1秒视频fixture执行probe/截图，输出与API字段匹配。测试不挂data/live/真实Library。

```bash
ffmpeg -hide_banner -loglevel error -f lavfi -i color=size=32x32:rate=1 -t 1 /tmp/a.mp4
ffprobe -v error -show_entries stream=width,height -of json /tmp/a.mp4
```

- [ ] build现有镜像跑测试确认missing binary；Docker不可用记录未执行，不改成静态grep“通过”。
- [ ] runtime apt安装ffmpeg及实际所需依赖，清apt cache；镜像仍只包含runtime工具而非Rust/Node build工具。
- [ ] build+smoke成功；提交 `fix(deploy): ship media probe and extraction tools in runtime image`。

## T26：secret文件透传与配置文档（C07、数据库配置观察）

**Files:** Modify [docker-compose.yml](<../../../docker-compose.yml>)、[config.rs](<../../../crates/api/src/config.rs>)；Create `docs/configuration-precedence.md`、`scripts/tests/compose_secret_test.py`。

**Interfaces:** Compose传递TOKEN/ADMIN_PASSWORD/QB_PASS/TMDB_KEY/TVDB_KEY/METADATA_PROXY_PASS的已支持_FILE环境；secret路径是容器内路径，volume/secrets显式挂载，优先级直接非空env > FILE > DB/默认（仅适用对应字段）。

- [ ] red test `docker compose config --format json` 在dummy FILE变量下确实包含各字段；不输出值到日志。配置测试直接env优先、FILE缺失/空明确错误策略、不同data_dir独立。

```text
QB_PASS_FILE=/run/secrets/qb ->container.environment存在该键
_FILE路径文件必须真实挂载；主机路径不自动当容器路径
ADMIN_PASSWORD为bootstrap-only，不承诺每次重启reset
```

- [ ] 添加缺失env透传和示例挂载。文档按字段列DB hot-reload/env pinned/restart-only，不改变既有密码分离。
- [ ] 核唯一probe外键：先写delete probe job及孤儿fixture测试，再决定启用foreign_keys+迁移清理或保留手动cascade，未证明前不全库盲开FK。记录schema version兼容与备份策略，不把旧主文件版本差异直接认错。
- [ ] Compose测试+config单测+api/workspace门禁；提交 `fix(config): forward supported secret files and document precedence`。

---

## T27：最终集成验收、覆盖核对与发布说明

**Files:** 更新 [审计报告](<../../audits/2026-09-29-full-codebase-audit.md>) 的状态列；Create `docs/audits/2026-09-29-remediation-verification.md`。只记录真实执行证据。

- [ ] 按下表逐项填写测试名称、修复commit、状态（fixed / unsupported-disabled / deferred / not-verified），未完成项不得写全部修复。
- [ ] 最终fresh workspace执行：

```bash
cargo test --workspace
pnpm --dir web exec tsc --noEmit
pnpm --dir web test
pnpm --dir web test:behavior
pnpm --dir web build
bash -n start-test.sh
python3 scripts/test-env/tests/startup_test.py
python3 scripts/tests/compose_secret_test.py
git diff --check
```

- [ ] Docker可用才运行镜像smoke；人工浏览器QA经用户许可在明确测试实例进行，不借“本页面”误操作DSH GUI或用户生产实例。
- [ ] 检查所有本轮触及Rust文件≤800/函数≤60；采用语法边界统计，不能用“下一个fn前全部行”把注释/结构体误计函数。超限触及函数实际拆解。
- [ ] 不启动重复workspace测试后台job；记录job id并收齐真实最终exit，不读取上一轮旧job结果冒充本轮结果。
- [ ] 回归数据/权限：Member矩阵、静态图片公共、Jellyfin视频认证、用户状态隔离、默认库关闭重开、破坏操作失败不丢数据。
- [ ] 文档写真实症状和证据，不再宣称“1秒秒级、绝不会超时、彻底无bug”。说明支持/未实现能力与迁移影响。
- [ ] 提交 `docs: record verified audit remediation and remaining capability gaps`；用户接受后再推送/关闭对应issue。

## 3. 审计编号到任务覆盖矩阵

| 审计编号 | 任务 | 验收核心 |
|---|---|---|
| D01 | T01 | 空/非法范围不全清，User隔离 |
| D02 | T02 | CRUD→重开，唯一default |
| D03 | T03 | 含ledger删除不抛弃权限 |
| A01 | T04 | Member写/同步403无副作用 |
| L02、L04 | T05 | 嵌套归属和organize整批预校验 |
| L01 | T06 | Move失败源/旧目标保留 |
| W03 | T07 | 文件Media身份/样片拒绝 |
| W04 | T08 | 多槽位不降级/不误删 |
| W01、W02 | T09 | Downloader/Library目标一致 |
| W05 | T10 | disabled定义不新增retry |
| W06 | T11 | 多集字幕与语言不覆盖 |
| W07、W08 | T12 | 双语正例、续作反例、并发字节隔离 |
| W09、W11 | T13 | accepted-stalled deadline、Site proxy |
| W10 | T14 | 480p/720p非episode |
| W12 | T15 | 真CDP attach；managed明确unsupported |
| 行为测试缺口 | T16 | rendered DOM和实际API请求 |
| U01、P01 | T17 | 正确Route、season+episode |
| L03 | T18 | source ID namespace |
| U02 | T19 | 两页上游→HTTP→UI |
| U03、U04 | T20 | genres/originalTitle、旧书签 |
| U05 | T21 | 非默认实例限速、不支持字段禁用 |
| U06、遗留支架 | T22 | 无假回执/无欺骗性入口 |
| L05、Filter size观察 | T23 | 非ASCII颜色/坏size400 |
| C01 | T24 | 子进程代理保留、本地探针独立绕过 |
| C02 | T24 | HTTP健康验证、TCP仅诊断、认证失败明确提示 |
| C03 | T24 | 墙钟deadline与所有探测有限时 |
| C04 | T24 | 绝对/相对DATA_DIR正确解析 |
| C05 | T24 | 初始密码提示不承诺现有密码、不泄露凭据 |
| C06 | T25 | 实际镜像probe/ffmpeg可执行 |
| C07、配置/FK观察 | T26 | FILE真实传入、优先级、证明后处理FK |
| 全量与发布 | T27 | 新鲜测试证据和剩余项诚实标注 |

## 4. 执行前需要确认的非阻断选择

本计划默认保守路径：不隐式迁移受限Library、不猜Torrent身份、不自动重置密码、不恢复整套转码/回收站/片源标注新功能。若用户要求“保留并完整实现”这些能力，应拆新的功能计划并补对应数据/协议设计，不能在执行中临时扩大本轮范围。

开始执行建议先完成 Phase A，并在每个任务后给出：改动、commit、实际测试结果、用户可见变化和下一任务。每阶段失败保持该任务未完成；禁止用缩短超时、TCP兜底或静态regex代替真正验收。
