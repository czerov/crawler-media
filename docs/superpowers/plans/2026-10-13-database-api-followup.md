# 数据库与接口优化收敛 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
> 工具／技能以实际环境为准；旧技能名不存在时采用等价公开 seam TDD＋独立复核，不创建 GitHub issue。

**Goal:** 修复已确认的清理控制／报告／配置缺陷，补齐接口离锁与大历史读路径，完成尚未完成的性能与发布验收。

**Architecture:** 保留四 Store 文件＋独立 Queue/cache，先在所属 crate 修单库原子性与明确生命周期，再由 API 编排 owned blocking 工作。限额同时区分返回行数、删除行数、扫描工作量和真实终止保证；不新增平行维护调度器，不借本计划默认引入连接池／持久 root_id／缓存自动删除。

**Tech Stack:** Rust edition 2024、Axum 0.8、Tokio、parking_lot、rusqlite 0.37 bundled SQLite；React/TypeScript、pnpm。

**Spec:** [总复核 F01–F12](<../../audits/2026-10-13-database-api-review.md>)＋[DB8 合同与偏差](<2026-10-12-db8-maintenance-retention.md>)。代码基线 `723394e`。

> **状态更新（2026-10-15）：** T1–T9 已有实现提交（`6f34c7a` T1／`9c4a1f1` T2／`d996788` T3／`af4dbbc` T4／`57a4837` T5／`f51b3f6` T6／`59b9c98` T7／`ce40b56` T8／`7aa1c7e` T9）。其中 T4／T5／T8 只改代码，未新增计划要求的 RED 目标；T9 的 release 矩阵、gallery ≥30% 目标、DB8 并发矩阵、VidHub／Infuse 设备证据与 `docs/benchmarks/2026-10-13-optimization-acceptance.md` 仍缺。**T10 未实施。** 下方复选框与“本次未实施”表述保留为当时计划原文，不等于当前状态；未完成项以 [总复核](<../../audits/2026-10-13-database-api-review.md>) §5 与本文 T10 为准。

## Global Constraints

- 用 `CONTEXT.md` 词汇；domain 无 IO，不产生 store/jobs→api 反向依赖。
- 测试先在公开 seam RED→GREEN；同时记录编译红、行为红和既有表征，不能补写未运行的 RED。
- 每个触及 crate 跑 `"$HOME/.cargo/bin/cargo" test -p <crate>`；最终稳定工作树跑一次 workspace，立即记录真实 exit code。
- `.rs`≤800 行，生产／测试函数≤120 行；已超文件被触及时先按责任拆分，不能用调大阈值规避。
- 不访问 live DB、不启动真实网络／Chromium／ffmpeg；人工性能／设备验收单独标注。
- Store/Queue 不嵌套；Registry→Store 锁序保留；只读快照不取代删除／Transfer／heartbeat 最终安全校验。
- 静态图仍 public、先授权再 304；Jellyfin 流／字幕保持协议鉴权和即时撤销。
- 不以 Tokio timeout 丢弃 JoinHandle 代替停止；permit 持有到真实工作结束。
- 永不自动按 expires_at 删除 stale-if-error cache；不默认 VACUUM/TRUNCATE，不擅自 DROP 索引或加跨 kind 唯一 alias。
- 日志不含凭证、完整 payload、播放明细；返回 lower bound 必须显式标截断／不完整。

## Review Focus

1. 管理员在第一批已提交后关闭清理：下一批不能开始；已提交不恢复（T1）。
2. 下一次已经排队时把策略改为更安全值，同时 claim：不能运行混合／陈旧未运行策略；running 保留快照（T2）。
3. 大量保护行、没有过期日志或只剩保护行：有限返回不等于有限扫描，必须按真实预算停止并报告不完整（T3/T4）。
4. legacy 文件升级期间 freeze／DB 拒绝／晚到渲染：当前已引用 bytes 不变，失败只删未引用新版本（T7）。
5. 前端编辑策略后旧 preview 晚返回，或点击已有 queued 的“立即执行”：不得展示旧策略计数，不得误报已立即执行（T5）。

---

## 1. DAG 与顺序

```text
T1 关闭边界 ─→ T2 原子配置/queued 快照 ─→ T3 报告口径 ─→ T4 真正有界执行
T2,T3 ─→ T5 UI/报告/手动触发/tick
T1–T5 完整控制/UI闭环 ─┐
T6 Jobs top-1 读 ─────┤
T7 legacy CAS ────┼→ T9 release/运行时/协议验收
T8 Playback/Artwork owned离锁 ─┘
T10 配置与历史逐ID对账（独立计划，不阻塞确定缺陷先修）
```

T1–T5 共享 Queue／maintenance/HTTP 代码，串行写。T6–T8 可并行研究，不同文件再并行实现。每项独立提交并审阅。F06 的重型维护发布决策是 T4 的前置门，不因“已有披露”当用户批准。

## 2. 文件与接口责任

| 单元 | 主要文件 | 责任 |
|---|---|---|
| 原子维护配置／queued 刷新 | [jobs store](<../../../crates/jobs/src/store.rs>)、[Queue](<../../../crates/jobs/src/queue.rs>)、[HTTP](<../../../crates/api/src/http/job_maintenance.rs>) | 单事务 CAS 全配置＋未运行子任务，返回实际状态 |
| 批边界控制／报告 | [runner](<../../../crates/api/src/maintenance/runner.rs>)、[report](<../../../crates/api/src/maintenance/report.rs>)、[retention](<../../../crates/jobs/src/retention.rs>) | def enabled、固定 cutoff、明确计数／预算 |
| SQL 工作预算／执行器 | Jobs retention、[Playback retention](<../../../crates/store/src/playback/retention.rs>)、[database](<../../../crates/api/src/maintenance/database.rs>) | 扫描／时间预算、恢复游标、重型终止决策 |
| UI 消费 | [抽屉](<../../../web/components/maintenance-job-settings.tsx>)、[任务页](<../../../web/components/scheduled-jobs-view.tsx>)、[jobs API](<../../../web/lib/api/jobs.ts>) | preview 绑定 draft、报告可见、执行结果真实 |
| hot read / offlock | [Jobs read](<../../../crates/jobs/src/read.rs>)、[PlaybackRead](<../../../crates/api/src/playback_read.rs>)、[history](<../../../crates/api/src/http/playback_logs.rs>)、[stats](<../../../crates/api/src/http/playback_stats.rs>)、[artwork](<../../../crates/api/src/http/library_artwork.rs>) | 有界选 latest、请求快照、锁外 IO |
| 封面 | [cover service](<../../../crates/api/src/library_cover_service.rs>) | legacy 与正常路径同一不可变发布保证 |

### T1 · P1 · 禁用在批次／库边界生效（F01）

**Files:** Modify runner、jobs查询seam、[job_loop](<../../../crates/api/src/job_loop.rs>)、ApiState/main的生命周期接线；Test Create `crates/api/tests/maintenance_control.rs`（避免向接近800的maintenance_http追加）。

**Interfaces:** 复用 `Queue::def(id)`检查enabled；内部 `MaintenanceStop::{Active,Disabled,Cancelled,ShuttingDown,ReadFailed}`，读失败不继续删；独立无def Job仍按attempt/status控制。

新增 `JobLoopControl`（Arc<AtomicBool>停止意图，`request_stop(&self)`/`is_stopping(&self)->bool`）与 `JobLoopHandle::shutdown(self) -> impl Future<Output=Result<(),String>>`。loop持有control并追踪每个spawn_blocking JoinHandle；ApiState的maintenance边界读取同一control。main停止HTTP准入后request_stop，停止新tick/claim，等待真实blocking workers退出才释放连接；不能只abort外层async任务。其他长期普通Job不能被强杀，本阶段如实报告等待drain，不伪造全进程硬截止。

- [ ] 写 `disable_after_first_committed_batch_stops_the_next_batch`：gate 在第一批 500 已提交后，HTTP disable，再释放；deleted=500、第二批未删，note=disabled；日志不泄露数据。
- [ ] 写enabled不变继续、取消attempt停、独立无def Job、读状态失败保守停的表征。
- [ ] 写 `shutdown_stops_claims_and_drains_real_blocking_work`：gate阻塞第一批→request_stop→无新claim／第二批→释放→shutdown await结束且worker/permit已释放；不能以JoinHandle drop当结束。fake数据库执行器阻塞时shutdown必须等其真退出；不可中断VACUUM场景由T4合同决定（未批准前不宣称有限drain）。
- [ ] Run `cargo test -p api --test maintenance_control`，确认实际行为红。
- [ ] 在每批／每库开始前读取 enabled＋attempt，不打断当前 SQLite 事务；返回有区分的报告。
- [ ] Run `cargo test -p jobs; cargo test -p api`，green 后仅提交任务文件。

### T2 · P1/P2 · 全维护配置 CAS＋刷新 queued 快照（F02/F04）

**Files:** Modify jobs store/queue/types、job_maintenance、[旧schedule handler](<../../../crates/api/src/http/jobs.rs#L454-L515>)、jobs schedule相关语义、web任务页/jobs API；Create `crates/jobs/tests/maintenance_config_atomic.rs`、`crates/api/tests/maintenance_config_atomic.rs`；维护DTO与合同随任务更新。

**Interfaces:** `MaintenanceConfigSnapshot { payload: String, schedule: Option<Schedule> }`；`Queue::update_maintenance_config_cas(id: JobDefId, expected: &MaintenanceConfigSnapshot, desired: &MaintenanceConfigSnapshot) -> Result<MaintenanceConfigUpdate, JobError>`；`MaintenanceConfigUpdate::{Updated(MaintenanceConfigSnapshot), Missing, Conflict}`。新 opaque config token 必须编码当前 payload＋schedule（可用版本化 JSON），不是 schema version；兼容旧 expected_payload 时不得假装其能保护 schedule，先升级客户端 token 后移除旧写契约。

- [ ] 写旧策略已 queued→保存 365 天或 dry_run=true→claim：运行使用新策略；running 仍保持旧快照；已提交批不重写。
- [ ] 写 schedule trigger 拒绝→payload/schedule/queued 全部不变；两个仅改 schedule 的同旧 token 保存，第二个 409。
- [ ] gate强制A/B交错、config/claim竞争、**旧schedule端点与新config端点交错**：完整A或B，不混合；省略interval保留并返回实际周期。
- [ ] 维护周期唯一写入口选定为原子maintenance-config：前端维护卡片的周期保存也走该端点＋完整token。旧PUT `/jobs/{id}/schedule` 对维护def返回明确409 `job.use_maintenance_config`、不写／不取消／不触发全局ensure_scheduled；非维护契约原样。维护配置省略interval=保留，显式null=400（永久保留用禁用，不允许隐藏维护def）。验证旧接口409与新client迁移，不留无token维护writer。
- [ ] Run 两个新 target 验证 RED。
- [ ] 在一个 Queue 事务完成 compare/update/queued refresh；周期调整同时明确 queued run_after 的重新排期规则（非手动 future child 按新周期从保存时间计算，run_after=0 的显式手动任务保留立即执行）。保持 running 行不变。
- [ ] Run jobs/api 全量＋web typecheck；修改 token 客户端并补兼容测试，green 后提交。

### T3 · P2 · 计数／截断合同（F03）

**Files:** Modify retention、report、runner、preview、web jobs types；Create `crates/jobs/tests/retention_reporting.rs`、`crates/api/tests/maintenance_reporting.rs`。

**Interfaces:** 批结果增加 `protected_truncated: bool`；preview 的 `truncated = candidates_truncated || protected_truncated`，独立字段说明被截断的是哪类；运行报告的保护计数只取一份本轮诊断、不逐批累加，显式 `protected_is_lower_bound`。`scanned` 改清晰命名或明确其是匹配窗口行数、不是 SQLite 实际扫描量。

- [ ] 写 600 protected/0 deletable→protected lower bound 且 truncated=true；600＋1200 deletable 多批不再报1503，diag capped501＋lower_bound=true；≤500保护时精确。
- [ ] 写 dry_run 一批不重复累加，仍报告更多候选；UI 显示下界而非完整窗口。
- [ ] Run 报告两个 target 验证 RED。
- [ ] 分离候选／保护 probe 与截断信息，不破坏 latest/tie/NULL/queued/running 保护，更新合同。
- [ ] Run jobs/store/api 全量＋web test/typecheck 后提交。

### T4 · P2 · 真正的扫描预算、预览隔离与 VACUUM 门（F05/F06）

**Files:** Modify jobs retention、store playback retention、api maintenance database/runner、HTTP preview、Cargo features（若 progress hook 需要）；Create `crates/api/tests/maintenance_budget.rs` 与对应 jobs/store 查询测试。

**Interfaces:** `RetentionWorkBudget { max_examined: usize, deadline: std::time::Instant }`、可恢复 cursor 使用 `(finished_at,id)`／`(ended_at,id)` 业务稳定键（不将 VACUUM 可改变的 rowid 持久化为游标）。返回 matched counts＋`incomplete`＋`resume_after`；旧接口可 wrapper，但默认固定安全预算不得开放任意用户扩大。

- [ ] **先批准 F06 发布方案**：本计划推荐保持自动 in-process VACUUM 默认关闭，硬上限无法证明前把重型执行置于明确离线操作；若用户选择 best-effort 必须在 UI/report 明写 deadline_exceeded 并保留同实例互斥，不谎报 bounded。隔离子进程是另一个需恢复验证的方案，不能本任务擅自采用。
- [ ] 写 all-protected、all-independent、无 expired、结束时间与开始时间错位各 200001 行 fixture：中断可观测、结果下界／cursor 明确，普通请求在 gate 下推进；扫描预算不是 `LIMIT` 断言。
- [ ] Run `maintenance_budget` 观察 RED；bundled SQLite 用 VM/progress seam 测停止，计时阈值只做人类报告。
- [ ] preview 进入现有 BlockingBudget，工作持有 permit 至 SQL 真正结束；明确 try-lock/admission 和 timeout，不把等待 SQLite／mutex 时间藏掉。
- [ ] Jobs 用稳定扫描游标越过保护行，后续一轮重查保证不漏；Playback `(ended_at,id)` 候选索引先记录 EQP/写成本，批准后迁移；有界返回不是全量计数。
- [ ] busy/locked 是可恢复 skip；IO/corrupt/full 为 fail；冷却 read/parse 错误保守 skip VACUUM＋tracing，不回落无冷却。
- [ ] Run jobs/store/api 全量；人工 release scan/锁等待数据写 benchmarks 后提交。

### T5 · P2/P3 · 维护 UI／报告／立即执行／tick（F12）

**Files:** Modify web 抽屉/任务页/jobs API、api http/jobs、job_loop；Create `web/tests/maintenance-settings.test.ts`、`crates/api/tests/maintenance_actions.rs`，必要时抽出可独立 node-test 的纯状态函数。

**Interfaces:** preview 结果带请求 policy signature/generation；仅与当前 draft 完全相同的响应可接受。manual run 响应区分 `created | expedited | already_running`，保留旧 queued 布尔兼容字段但不误导。报告 fallback 查询最近有 progress 的终态（有界 top-1），不读全部历史。

- [ ] 写 draft 改动清旧预览、旧请求晚到丢弃、失败不展示旧“完整”计数；报告展示 lower_bound／skip_reason／deadline_exceeded（依 T3/T4）。
- [ ] 写 future queued→立即执行 run_after=0、不新增第二 live；running 不新增；disabled409；DevHttp tick 不取消／提前后台维护 child，非维护任务旧响应保持。
- [ ] Run node/API target 看到 RED。
- [ ] 区分执行来源而不是通过 Queue 锁100ms采样推断来源；开发 tick 过滤 claim 集合，不以取消数据达到“跳过”。
- [ ] Run api 全量、pnpm web test/typecheck/build，green 后提交。

### T6 · P2 · Jobs bounded latest-child 读取（F07）

**Files:** Modify jobs read、api jobs 调用；Create `crates/jobs/tests/latest_child_scale.rs`；扩展 ignored query_plans。

**Interfaces:** 保留 `Queue::children_for_defs(&[JobDefId]) -> Result<Vec<JobChildren>, JobError>`，实现只映射每 def 最多一 live＋一 last，SQL 使用实测 indexed correlated top-1 或等价查询，不用读全部后丢弃。

- [ ] 写单 def20万终态＋多def／tie／queued-running 表征；旧无关坏 payload 行不得破坏最新 child 响应。
- [ ] 在 bundled SQLite EQP/release 对照：旧批读、定向LIMIT1、候选新SQL，记录解码数／锁占用／总体而非只 SQL 次数。
- [ ] Run target RED→最小实现→jobs/api 全绿；不放松真实最新行损坏的错误，提交。

### T7 · P2 · legacy 封面不可变 CAS（F08）

**Files:** Modify cover service；Create `crates/api/tests/cover_legacy_publication.rs`，复用现有 fake renderer。

**Interfaces:** 所有生成路径复用现有 `publish`/`compare_set_library_cover`；legacy 文件只读，不新增第二 CAS。

- [ ] legacy eligible＋gated render：手动替换／none／freeze／trigger拒绝各场景，旧引用与旧 bytes 不变；新未引用文件清除；成功只改 DB pointer。
- [ ] Run target 看到旧分支行为红。
- [ ] 把 legacy marker／fanart 比对、读盘也放 blocking，生成不可变新文件后 publication gate；不清用户任意自定义路径。
- [ ] Run store/api 全量＋既有 cover tests，green 后提交。孤儿旧版本 GC 另在 T10 设计。

### T8 · P2 · Playback／Artwork owned 离锁与输入错误闭环（F09/F10/F11）

**Files:** Modify playback_read、history/stats、Jellyfin items、library_artwork；Create `crates/api/tests/playback_offlock.rs`、`history_errors.rs`、`artwork_candidates_runtime.rs`。按独立 endpoint 可拆小提交，不同时改共享快照结构。

**Interfaces:** `PlaybackRead` 只含 owned DB facts；`media_target` 不做 IO。poster existence 用现有 BrowseFileRead seam 或显式 owned `ArtworkFacts` 在 blocking 阶段补全；visibility 一次 ownership＋Media batch＋角色，不建立 Media→单一Library。

- [ ] slow poster/NFO/catalog gate 下 Store try_lock、current-thread 无关 HTTP 可推进；多库同Media、nested hidden、ambiguous/frozen 权限表征不变。
- [ ] SQL注入故障 history 返回标准5xx；正常空可见集200；before i128极值／days overflow非法400、不回绕；明确保留兼容范围而非随意 clamp。
- [ ] Run 新 target RED。
- [ ] API 入口完整 owned工作进入预算；history/stats 查询与 DB snapshot 仍在所属连接串行，不携带 guard 到锁外；渲染不调用 poster_path/stat。
- [ ] Run store/api/media-server 全量，green 后提交；不把聚合13条SQL错误优化成一条但改变时区／hidden语义。

### T9 · 验收门 · release／真实 loop／协议证据

**Files:** Modify [endpoint_perf](<../../../crates/api/tests/endpoint_perf.rs>)、[maintenance_acceptance](<../../../crates/api/tests/maintenance_acceptance.rs>)；Create `docs/benchmarks/2026-10-13-optimization-acceptance.md`（实际运行时填，不提前写结果）。

**Interfaces:** fixture movie/tv/mixed 对称命中 NFO/图片、多个Library；history200001＋；操作完整 response 而非只有字节大小。已有 StateRunner 直调不叫真实 background loop 验收。

- [ ] 真实 `spawn_job_loop_with` 临时库：首次调度／90天默认警告、enabled=false播放0删、改策略queued刷新、disable边界、shutdown/drain、busy／恢复；gate控制、无真实外网。
- [ ] release fixed seed≥5轮：100/500/2000/10000 Media＋200001历史；相同DTO before/after，记录median/p95、response bytes、CPU/RSS、锁等待、maintenance时长、DB/WAL大小。
- [ ] gallery 2000行目标 **延迟下降≥30%**（不是speedup1.3×）；未达到继续profile或明确批准改目标，不能标完成。
- [ ] DB8开启／关闭并发读矩阵；没有真实停止证明不能填deadline通过。
- [ ] VidHub/Infuse版本＋脱敏method/path/response与Range206/416/断开取消记录；没设备写Not run，不猜测全协议缺口。
- [ ] 最终 workspace一次稳定树＋web test/typecheck/build，记录真实退出码；重跑绿不能抹掉首次失败原因；独立整支review后提交验收记录。

### T10 · 独立后续，不自动大改

| 项 | 当前状态 | 可执行退出条件 |
|---|---|---|
| env_override UI／空 DATA | 后端pin有，UI未消费；空路径回落是既有行为 | 单独配置合同：UI只读提示与后端409一致；空DATA显式选择拒绝或兼容提示，startup/restart/_FILE隔离进程测试 |
| DB11 typed settings／overlay | 未做 | 逐域 key/类型/默认/version/损坏策略，同一事实来源，兼容内置profile，不自动增加热加载 |
| R5外部同路径替换 | 已知设计边界 | per-item generation／来源身份方案＋原路径替换／publication gate，明确可防与不可防范围 |
| 封面孤儿版本GC | 未做 | 只删内部版本目录内、确认无当前引用且无inflight保留的文件，竞态／重启测试；不删用户任意路径 |
| DB12冗余索引 | 未批准DROP | 每条真实查询EQP、体积、读写成本，再单独迁移 |
| connection pool/root_id/持久read model | 设计门 | T9证明瓶颈仍在，再ADR跨文件一致性／backfill／失效／恢复 |
| 删除离锁／heartbeat strict | 安全门 | 列出全部file/ledger发布者、per-item排他／generation、same-path重用和失败重试证据，不能照只读快照机械改 |
| 默认分页／operation handle | 产品门 | 所有FE/CLI traversal消费者清单＋版本兼容合同，不静默截断省略limit |
| 全Jellyfin／streambuffer | 证据门 | 真实客户端调用；可控吞吐/Range/CPU/RSS/取消测试，≥10%真实收益后另批 |
| 09-29/10-05/10-10历史审计 | **未逐ID对账** | 每ID=已修(代码+测试)、仍坏(公开复现)、设计变化、证据不足四类；不能全关／全开 |
| adaptive/rename/removal/probe独立功能 | 已有实现，经验验收未全确认 | 对各自计划任务／取消恢复／整季STRM/真UI/设备验收补闭环；不重执行旧 unchecked 实现步骤 |
| 文件／日志维护 | 既有债务 | cli820触及时拆；domain软限按责任review；损坏payload日志重复与冷却错误保守性单独修，不当核心缺陷替代品 |

## 3. 本轮文档交付与执行交接

- [x] 比较固定基线与当前实现，独立数据库/API/全部docs审阅，当前结果写入总复核。
- [x] 将已修 R1–R4、AuthUser／Debug／proxy 错误旧待办撤下当前清单，保留提交映射与误报纠正规则。
- [x] 把 DB8 从“全部完成”改为“主功能存在／控制与验收缺陷未闭环”，区分目标合同与当前偏差。
- [ ] T1–T9 生产修复／验收：**本次未实施**。
- [ ] T10 独立设计与旧审计逐ID核验：**本次未实施**。

建议先审阅 T1/T2/T4 的控制与发布决策，再另行授权执行；不把“更新计划”当自动批准改数据库结构或清理 live 数据。
