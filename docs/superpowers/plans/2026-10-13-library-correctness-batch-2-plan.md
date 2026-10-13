# 第二批 Playback 与扫描正确性 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
> 当前环境映射同第一批：Native使用`executing-plans`；逐任务subagent模式用可用工具，不调用未安装技能、不创建Agent Teams。

**Goal:** 防止迟到播放事件污染新会话、准确累计观看时间，并让部分扫描失败后的probe义务可恢复。

**Architecture:** Playback事件通过服务端签发的持久会话ID限定代际，两协议共享时间累计策略；扫描登记与probe obligation在library.db原子提交，dispatcher复用现有持久probe去重和恢复。结果报告保留旧字段并显式区分partial。

**Tech Stack:** Rust 2024、Axum 0.8、Tokio、rusqlite bundled、React/TypeScript现有播放调用方。

**Spec:** [第二批技术设计](<2026-10-13-library-correctness-batch-2-design.md>)；前置 [第一批计划](<2026-10-13-library-data-safety-batch-1-plan.md>) 全部验收。

## Global Constraints

- 保持第一批S1–S8；无真实网络/Chromium/ffmpeg测试，不删除用户历史。
- subscribe schema 3→4；library schema 7→8；第一批未迁移，若实施基线版本已有增长则重新分配下一个版本并同步三批总表，不能覆盖别人的迁移。
- PlaySessionId不是认证；保持MediaBrowser/token、User可见性、设备撤销和public静态图片规则。
- 会话ID最大128字节；ID记录30天过期只清closed/pending，每次最多500；probe dispatcher每5秒最多100条。
- watch累计间隔上限30_000ms、seek容差2_000ms、倍率0.25..=4.0，缺省1.0。
- 测试优先公共HTTP/Store seam；无sleep猜竞态；每任务crate测试、最后workspace；.rs≤800行，函数≤120行。
- 当前分支小提交；不暂存预存无关计划；未完成代码不勾选本计划。

## Review Focus

- 同一文件再次播放也存在代际，不能只测A/B不同Media（T2）。
- 无ID客户端、登录token和设备撤销不可因新增ID校验损坏（T2）。
- 重复心跳/墙钟倒退/暂停/倍率/seek不虚增或溢出观看时间（T3）。
- enqueue已成功而ACK没提交的重启不得重复probe（T4）。
- 多root部分权限失败和Video格式差异必须可观察，不用queued=true掩盖（T5）。

---

## 文件结构与依赖

`B1完成 → T1 → T2 → T3 → T4 → T5 → T6`。T4独立消费B1、可在T3后顺序执行；不要同时修改schema常量。

| Create | 职责 |
|---|---|
| `crates/store/src/playback/session_ids.rs` | 签发、激活、关闭ID及绑定查询 |
| `crates/store/src/schema/playback_session_ids.rs` | subscribe4迁移 |
| `crates/api/src/playback_events.rs` | 两协议事件代际判定与共享累计 |
| `crates/store/src/scan_probe_obligations.rs` | ledger+obligation事务及ACK |
| `crates/api/src/scan_probe_dispatch.rs` | 有界派发、启动恢复 |
| `crates/library/src/watch/report.rs` | root/文件错误与结果模型 |
| `crates/api/src/http/library_scan/report.rs` | 对外scan结果映射 |
| 下述独立测试及相邻support模块 | 公共合同与故障注入 |

修改现有模块在各任务列明；过长schema/probe管理文件通过新sibling模块承接，不继续堆实现。

## Task 1: 持久播放会话ID与兼容迁移

**Files:** Create Store session_ids与schema模块；Modify [schema](<../../../crates/store/src/schema.rs>)、[SessionRow](<../../../crates/store/src/playback.rs>)、[session SQL](<../../../crates/store/src/playback/sessions.rs>)、[DDL](<../../../crates/store/src/schema/tables.rs>)、[Store exports](<../../../crates/store/src/lib.rs>)；Test `crates/store/tests/playback_session_ids.rs`。

**Interfaces:**
- Produces SessionRow新增`play_session_id: Option<String>, last_report_at_ms: i64, last_event_sequence: Option<i64>`；所有构造方在本任务同步更新为None/既有秒时间*1000/None，不改变原业务。
- `Store::issue_play_session(&self, user: UserId, device: &str, ledger: LedgerId, now: i64) -> Result<String, StoreError>`。
- `Store::play_session_binding(&self, user: UserId, device: &str, id: &str) -> Result<Option<PlaySessionBinding>, StoreError>`；`PlaySessionBinding { ledger_id: LedgerId, issued_at: i64, generation: i64, state: PlaySessionIdState }`；enum Pending/Active/Closed。
- `Store::activate_play_session(&self, user: UserId, device: &str, id: &str) -> Result<bool, StoreError>`；激活时关闭同设备更低generation的pending及旧active，重复active=true，closed/未知=false；签发在同事务取MAX(generation)+1，必须测试同秒两次签发。
- `Store::close_play_session_id(&self, user: UserId, device: &str, id: &str) -> Result<(), StoreError>`；与close_session在同playback_write事务调用。

- [ ] **Step 1: 写Store合同测试。** `issued_id_is_bound_to_user_device_and_ledger`断言不同三元组不命中；`closed_or_older_pending_cannot_reactivate`测试A签发→B签发/激活→A拒绝；`repeated_activation_is_idempotent`；`old_schema_migrates_without_resetting_user_facts` fixture subscribe3，旧Session位置750000、favorite=true，迁移后不变，毫秒时间回填；`expiry_keeps_active_and_is_bounded_500`填501closed+1active只删500。
- [ ] **Step 2: 红验证。** `cargo test -p store --test playback_session_ids`；预期新接口未定义。
- [ ] **Step 3: 实现迁移与接口。** 按设计DDL；激活用事务核对binding/state；PlaySessionId由UUID v4生成，重复签发不复用。更新所有SessionRow构造、mapper、测试fixture；保留较新schema只读拒绝预检。
- [ ] **Step 4: 绿验证。** `cargo test -p store`、`cargo test -p api`、`cargo test -p media-server`；此次涉及测试构造的crate都须独立通过。
- [ ] **Step 5: 提交。** `git commit -m "feat(playback): persist device scoped play session identities"`。

## Task 2: 两套协议接入代际、legacy兼容和幂等

**Files:** Create `crates/api/src/playback_events.rs`；Modify [provider trait](<../../../crates/media-server/src/provider.rs>)、[Sessions DTO](<../../../crates/media-server/src/routes/sessions.rs>)、[PlaybackInfo](<../../../crates/media-server/src/routes/media/playback.rs>)、[API provider](<../../../crates/api/src/media_server_provider.rs>)、[provider播放](<../../../crates/api/src/media_server_provider/playback.rs>)、[自有heartbeat](<../../../crates/api/src/http/playback/heartbeat.rs>)、[自有session](<../../../crates/api/src/http/playback/session.rs>)、[API模块](<../../../crates/api/src/lib.rs>)；Test `crates/api/tests/playback_event_identity.rs`、`crates/media-server/tests/play_session_protocol.rs`。Web修改 [Playback API](<../../../web/lib/api/playback.ts>) 与 [Video player](<../../../web/components/player/video-player.tsx>) 的start/heartbeat/stop/unload全部路径，纳入同提交，仅改会话ID透传，不改UI；新增 `web/lib/api/playback-session-identity.test.ts`。自有start请求必须携带与progress一致的getPlayerDeviceId，不能仅在progress补device_id。

**Interfaces:**
- Consumes T1绑定/激活接口。
- Produces provider `PlaybackEventIdentity { play_session_id: Option<String>, event_sequence: Option<i64> }`；新增`issue_play_session(user, device, item_id) -> Result<String, String>` async trait方法；`report_playback_event`追加identity参数并同步所有fake providers。
- API共享`fn classify_event(previous: Option<&SessionRow>, binding: Option<&PlaySessionBinding>, identity: &PlaybackEventIdentity, ledger: LedgerId, event: PlaybackEvent) -> EventDecision`；enum Start/Update/Stop/Ignore。判定表完全按设计2.3/2.4，不读网络；所有修改仍在playback_write事务里。
- 自有start响应新增`play_session_id`；progress读取同名字段及`event_sequence`；Jellyfin分别使用PascalCase。闭合ID后保留绑定记录state=closed直到过期。

- [ ] **Step 1: 写HTTP回归。** `late_old_stop_does_not_close_new_session`、`late_old_progress_does_not_rewind_position`、`late_old_playing_cannot_reactivate_closed_id`、`same_ledger_different_generation_is_isolated`、`duplicate_playing_and_stopped_are_idempotent`：断言B Session ID/position/watch/log不变，重复Playing play_count只加1、重复Stopped只1条log。
- [ ] **Step 2: 写兼容与隔离测试。** `legacy_no_id_only_updates_matching_current_target`、`unknown_id_returns_protocol_success_without_writes`、`id_cannot_cross_user_device_or_ledger`、`event_sequence_rejects_old_value_but_allows_real_seek`；ID129字节/sequence=-1返回400，合法旧值被忽略；未知ID不泄露binding。既有MediaBrowser/设备revoked/frozen测试不弱化。
- [ ] **Step 3: 红验证。** `cargo test -p api --test playback_event_identity`、`cargo test -p media-server --test play_session_protocol`；当前DTO/替换会话行为失败。
- [ ] **Step 4: 实现共享判定并接入。** PlaybackInfo签发真实pending ID而非裸UUID；只有Start能关闭旧target；Ignore在写unit/close_session之前返回；target按LedgerId而非仅Media+episode。请求体缺ID按legacy表，不强制所有客户端升级。Web透传签发ID，不自行生成不匹配ID。
- [ ] **Step 5: 绿验证。** `cargo test -p store`、`cargo test -p media-server`、`cargo test -p api`；若修改Web运行`pnpm -C web typecheck`及新增session透传单测。
- [ ] **Step 6: 提交。** `git commit -m "fix(playback): ignore stale events across session generations"`。

## Task 3: 观看墙钟累计策略

**Files:** Modify T2 playback_events、[heartbeat](<../../../crates/api/src/http/playback/heartbeat.rs>)、[Jellyfin provider](<../../../crates/api/src/media_server_provider/playback.rs>)；Create `crates/api/tests/playback_watch_time.rs`；Clock模块放`crates/api/src/playback_events/clock.rs`。

**Interfaces:**
- `pub trait PlaybackClock: Send + Sync { fn now_ms(&self) -> i64; }`；ApiState注入`with_playback_clock(self, Arc<dyn PlaybackClock>) -> Self`（修改 [state](<../../../crates/api/src/management/state.rs>)）。
- `fn watched_increment(previous: &SessionRow, position_ms: i64, paused: bool, stopped: bool, now_ms: i64, rate: f64) -> i64`；纯算法按设计3节；两协议唯一实现，秒字段用于原日志，毫秒字段用于累计。

- [ ] **Step 1: 写精确时钟测试。** fake clock从1000→2000：start0→seek600000，watched=0；2000→3000推进1000，watched=1000；后退delta=0；暂停和暂停后恢复首段delta=0；2倍速1s推进2000只加1000；重复毫秒/倒退时钟加0；长掉线夹30s且大跳判seek；i64极值不panic。Stopped计停止前自然推进，不能因Stopped paused=true丢最后一段。
- [ ] **Step 2: 红验证。** `cargo test -p api --test playback_watch_time`；当前seek累计断言失败。
- [ ] **Step 3: 实现算法并更新两个协议。** finite倍率0.25..=4.0，否则400；saturating算术；从事件验证后更新last_report_at_ms；Ignore不动时间锚点；旧日志不重算。
- [ ] **Step 4: 绿验证。** `cargo test -p api`；更新旧无真实耗时fixture用fake clock，不篡改断言让seek虚增“通过”。
- [ ] **Step 5: 提交。** `git commit -m "fix(playback): bound watched time by elapsed playback time"`。

## Task 4: 扫描probe义务原子持久化与重启派发

**Files:** Create `crates/store/src/scan_probe_obligations.rs`、`crates/api/src/scan_probe_dispatch.rs`；Modify [schema](<../../../crates/store/src/schema.rs>)、[Store注册](<../../../crates/store/src/lib.rs>)、B1 ledger_cleanup、[scan登记](<../../../crates/api/src/watch_ledger.rs>)、[probe recovery](<../../../crates/api/src/probe_manager/recovery.rs>)、[ApiState初始化](<../../../crates/api/src/management/state.rs>)、[API注册](<../../../crates/api/src/lib.rs>)；Test `crates/store/tests/scan_probe_obligations.rs`、`crates/api/tests/scan_probe_recovery.rs`。

**Interfaces:**
- `Store::insert_scanned_ledger(&self, row: &LedgerRow, now_ms: i64) -> Result<(), StoreError>`；同事务ledger+obligation。
- `ScanProbeObligation { ledger_id: LedgerId, created_at_ms: i64, reason: String }`。
- `Store::scan_probe_obligations(&self, limit: usize) -> Result<Vec<ScanProbeObligation>, StoreError>`；limit夹1..100，按created/id稳定序。
- `Store::ack_scan_probe_obligation(&self, ledger_id: LedgerId) -> Result<(), StoreError>`。
- `async fn dispatch_once(state: ApiState) -> Result<usize, String>`：owned blocking作业，ACK前读取持久probe状态；启动/每5秒调用，单dispatcher不重叠；任务生命周期接已有job_loop_control/服务关闭，不用无人跟踪重复tokio循环。

- [ ] **Step 1: 写Store事务红测试。** `insert_and_obligation_are_atomic` trigger拒绝obligation插入后ledger不存在；`ledger_cleanup_removes_obligation_not_user_facts`；旧library7升级后旧row不强制重探，新row有义务；较新schema拒绝打开。
- [ ] **Step 2: 写恢复红测试。** `scan_prefix_failure_is_recovered_after_restart`：前缀有效NFO，后项alias冲突，失败后重启dispatcher前缀获持久probe；`enqueue_then_crash_before_ack_does_not_duplicate`；`completed_probe_is_acked_without_requeue`；`frozen_or_deleted_ledger_never_resurrects`。用可控fake probe，不调用真实ffmpeg。
- [ ] **Step 3: 红验证。** `cargo test -p store --test scan_probe_obligations`、`cargo test -p api --test scan_probe_recovery`。
- [ ] **Step 4: 实现schema8、事务登记和dispatcher。** 新增row写义务；旧row恢复保持B1规则。现有enqueue_probes_for_paths可即时触发dispatch，但ACK语义仅在持久接收后；失败保留义务，所有错误结构化log；同事务cleanup新增表清理。不能通过每次重扫所有row强制probe来补偿。
- [ ] **Step 5: 绿验证。** `cargo test -p store`、`cargo test -p api`；重复普通扫描不重probe，第一批取消/删除防复活仍通过。
- [ ] **Step 6: 提交。** `git commit -m "fix(scan): persist probe obligations with ledger registration"`。

## Task 5: 部分扫描报告与统一格式入口

**Files:** Create library watch/report与API scan/report；Modify [遍历](<../../../crates/library/src/watch.rs>)、[library导出](<../../../crates/library/src/lib.rs>)、[扫描HTTP](<../../../crates/api/src/http/library_scan.rs>)、B1 ScanScope；Create `crates/library/tests/scan_report.rs`、`crates/api/tests/scan_report.rs`。

**Interfaces:**
- library `ScanRootStatus { Completed, Partial, Unreachable }`、`ScanIssue { path: PathBuf, kind: std::io::ErrorKind }`、`ScanRootReport { root: PathBuf, status: ScanRootStatus, visited: usize, skipped: usize, error_count: usize, issues: Vec<ScanIssue> }`。
- `ScanReport { roots: Vec<ScanRootReport>, inserted: usize, restored: usize, probe_pending: usize }`由API report持有；最多20条issues返回，error_count不截断。
- 新增`scan_watch_reported(job: &WatchJob, media: &Media) -> Result<(WatchOutcome, ScanRootReport), LibraryError>`；保留原scan_watch wrapper既有调用合同，API新入口消费reported版本。

- [ ] **Step 1: 写公共扫描报告测试。** 多root一完成一不可达status=partial且旧queued/transferred保留；全部不可达409；单文件PermissionDenied有error_count且非completed；目录entry错误不会被flatten吞；错误21条返回20条且errors_truncated=true。用注入目录读取seam，真实平台不可稳定造权限错误时不依赖chmod。
- [ ] **Step 2: 写格式测试。** 在Video库登记mts/mpg/mpeg/vob/iso和mkv，未知扩展跳过；软链接不跟随；只断言ledger，不声称probe支持每种格式。
- [ ] **Step 3: 红验证。** `cargo test -p library --test scan_report`、`cargo test -p api --test scan_report`。
- [ ] **Step 4: 实现report，移除Video重复白名单。** 统一is_video_file，错误收集与返回按设计；提交前缀错误返回scan_failed及计数，不伪造原子回滚；继续用B1 scope，root health与报告一致。
- [ ] **Step 5: 绿验证。** `cargo test -p library`、`cargo test -p api`；已有Watch intake/Transfer合同不变。
- [ ] **Step 6: 提交。** `git commit -m "fix(scan): report partial traversal and unify video formats"`。

## Task 6: 整批回归和第三批交接

**Files:** Create `crates/api/tests/correctness_batch_acceptance.rs`；Modify [ADR0005](<../../adr/0005-jellyfin-compatible-playback.md>)、[ADR0011](<../../adr/0011-probe-stage-retry.md>)及本设计验收状态。
**Interfaces:** 无新生产接口；交接T1 SessionRow/schema4、T4义务/schema8、T5报告/scan_watch_reported。

- [ ] **Step 1: 写整链路测试。** 第一批missing恢复+扫描prefix失败+重启probe，再Jellyfin登录/两代播放/seek/Stop，断言用户事实保留、probe最终单次、日志watched精确。测试无外网、假时钟、假probe。
- [ ] **Step 2: 验证。** `cargo test -p store`、`cargo test -p library`、`cargo test -p media-server`、`cargo test -p api`、`cargo test --workspace`；Web透传若改则typecheck+对应单测；所有退出码0。
- [ ] **Step 3: 交接。** ADR记录legacy同文件代际限制、毫秒计时口径、schema版本和义务ACK合同；真实设备未测写Not run。第三批不得改回只按Media匹配/按inserted推断probe完成。
- [ ] **Step 4: 提交。** diff/尺寸/无关改动检查后`git commit -m "test: accept playback and scan correctness batch"`。

## 自审

Playback ID生命周期、无ID兼容、时间算法、故障恢复、格式及partial均有任务与可观察断言。新增schema只在T1/T4分别写所属库；B1清理helper在T4明确扩展。第三批只消费这些接口，不另建probe队列或重定义播放事实。本文测试命令是待执行步骤，不是已通过记录。
