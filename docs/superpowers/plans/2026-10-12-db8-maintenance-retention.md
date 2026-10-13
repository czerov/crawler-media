# DB8 数据库维护与历史保留 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
> 执行时按当前会话实际可用的技能与工具选择等价执行方式，不因上述历史技能名缺失而绕过 review。

**Goal:** 复用现有 Jobs 模块，为数据库维护、终止态任务历史清理和播放历史清理提供独立任务、默认开关、默认频率、用户配置与安全验收。

**Architecture:** 调度和开关继续保存在 `job_defs`，保留参数采用带版本的任务 payload，不另建调度器或第二份 settings 配置。清理由所属 crate 的公开接口完成，API 层只做配置验证、任务编排和报告；数据库维护使用受控的独立连接，不在全局 Store/Queue 锁内执行 VACUUM。

**Tech Stack:** Rust edition 2024、Axum、Tokio、rusqlite bundled SQLite、现有 jobs/Store、React/TypeScript。

**Spec:** 本文第 1–7 节是设计规格；第 8 节是实施计划。需求依据为本次 DB8 沟通及 [复核报告 DB8](../../audits/2026-10-12-api-remediation-review-and-backlog.md)、[ADR-0008](../../adr/0008-sqlite-system-of-record.md)。

**状态：待 review，仅文档，不代表已实现。** 默认值是基于本次沟通整理的建议，涉及真实删除与 VACUUM 的配置须在实施前确认。

## Global Constraints

- 复用现有 Jobs 模块；每类维护任务独立开关和频率，不新增平行定时器。
- `.rs` 文件硬限 800 行；生产函数、测试函数硬限 120 行。
- 测试不访问真实 TMDB/Site/Downloader/Chromium/ffmpeg，只用临时 SQLite 与假 IO。
- 每个修改的 crate 先运行 `cargo test -p <crate>`，稳定工作树上最后运行一次 `cargo test --workspace`。
- 不修改 live 数据，不用文件复制替代 SQLite 在线备份一致性协议。
- `domain` 无 IO；不引入 `store → api` 或 `jobs → api` 反向依赖。
- 不在 HTTP 请求内执行删除批次、ANALYZE 或 VACUUM。
- Registry → Store 是已有 Library 工作锁序；Store/Queue 不嵌套持锁。
- 日志不包含 token、密码、完整任务 payload、缓存响应体或用户播放明细。

## Review Focus

1. 用户关闭、调频率或改保留期后重启，配置不得被 seed 默认值覆盖（Task 1/4/5）。
2. 清理与任务完成/重试同时发生，不得删活跃任务或改变 last-child 结果（Task 2）。
3. 播放历史删除不得同时清掉继续观看进度、收藏、活跃会话（Task 3）。
4. 扫描在预检后才开始、数据库正在写入或磁盘不足，不得把“刚才空闲”当成安全保证（Task 4）。
5. 管理员误填 0、负数、巨大数值、未知版本或非管理员调用，不得扩大删除范围（Task 1/5）。

---

## 1. 本次沟通确定的方向

用户希望：维护能力作为现有“定时任务模块里的任务”，有默认开启/关闭、默认执行频率，用户能自行开关和调整。

首期收敛为三类：

1. 数据库维护：常规轻量优化；重型空间回收单独显式开启。
2. 终止态 Jobs 历史清理。
3. `playback_logs` 播放历史清理。

**不纳入首期自动清理：** fanart/catalog 缓存、字幕缓存、probe/fingerprint 数据、playback metrics、封面版本文件 GC。

### 1.1 需要更正前文的技术判断

- **没有显式 ANALYZE，不等于索引不会被使用。** SQLite 无统计信息也能选择索引；是否收益应以实际 EQP/耗时验证，不能承诺维护后所有接口变快。
- **DELETE 不是“白删”。** SQLite 可复用空闲页；即使文件不缩小，删除仍减少逻辑行数、查询和索引维护负担。VACUUM 主要为向文件系统归还空间，不是每次删除后的必需步骤。
- **VACUUM 不保证按主键物理重排。** 它重建数据库、压缩空闲空间；无显式 INTEGER PRIMARY KEY 的表还需注意 rowid 变化。本项目业务不能依赖隐式 rowid。
- **fanart 的缓存 key 哈希不代表图片 URL 必然过期。** 源站 URL 生命周期不能由缓存 key 推断。
- **catalog cache 不可简单认定只与 Media 数量成正比。** key 含请求路径/query，也包含搜索类请求；且跨库没有完整的 Media→cache 外键。不能直接承诺“无 Media 引用即安全删除”。
- **probe/fingerprint 不能统称 30 天后可删的缓存。** 其中可能包含恢复状态、重用样本和已验证识别依据，需要另行逐表设计。
- **过期缓存仍是 stale-if-error 兜底。** 首期不按 expires_at 或年龄删除它们。

## 2. 当前代码事实与可复用能力

基线：`4670239`（实施时重新核对 HEAD，不回退现有变更）。

| 能力 | 当前位置 | 结论 |
|---|---|---|
| JobKind、Interval 调度 | `crates/jobs/src/kinds.rs` | 目前仅 Interval，不宣称已有 cron/固定凌晨时间 |
| 幂等默认任务注册 | `crates/api/src/jobs_api.rs`、`crates/jobs/src/store.rs::ensure_def` | 以 kind + concurrency_key 查已有 def，存在则原样返回 |
| 开关、调频率、手动执行 | `crates/api/src/http/jobs.rs` | 可复用，但 disabled 的手动执行当前可能返回 queued:true 却未入队，须修正 |
| 系统任务页面 | `web/components/scheduled-jobs-view.tsx` | 已有开关/频率/执行；只显示 schedule 非空任务 |
| 任务执行离 Queue 锁 | `crates/api/src/job_loop.rs` | 正式后台循环已先 claim、锁外 run、再持短锁落结果 |
| 公共 Queue::tick | `crates/jobs/src/queue.rs` | runner 在 &Queue 调用内执行；维护任务需拒绝或改造该入口，不能递归拿同一个 Queue 锁 |
| 执行分派 | `crates/api/src/worker.rs::StateRunner` | 新任务需要新增匹配分支 |
| 播放历史 | `crates/store/src/playback/sessions.rs` | close_session 写入 playback_logs；不是收藏/进度表 |
| last-child | `crates/jobs/src/schedule.rs::get_last_child` | terminal 中按 finished_at DESC、run_after DESC 取首行 |
| 数据库路径 | `crates/store/src/open.rs` | app 可由 open_at 自定义，不能一律假定 data_dir/app.db |

历史观察：曾见 Jobs 7094 行，其中 7087 行终止态；fanart 缓存 1 行。**这些是当时样本，不是当前实时数量，也不能直接外推年度增长或性能瓶颈。**

## 3. 任务与默认策略

### 3.1 默认任务表（建议配置）

| JobKind / 显示名 | 默认开关 | 默认频率 | payload 默认参数 |
|---|---|---|---|
| `database_maintenance` / 数据库维护 | 开 | 21600 秒（6 小时） | `vacuum_enabled=false`，其余见 3.2 |
| `jobs_retention` / 任务历史清理 | 开 | 86400 秒（1 天） | `retention_days=90`、`dry_run=false` |
| `playback_history_retention` / 播放历史清理 | **关** | 86400 秒（1 天） | `retention_days=90`、`dry_run=false` |

关闭任务仍保留非空 schedule，保证现有页面可见；默认开关与频率只在首次注册时使用。用户调整后重启保持原值。

**首次安装/升级提醒：** 当前 ensure_scheduled 会立即补首个执行实例，并非等满一个周期才首次执行。默认开启的 Jobs 清理必须在发布说明中明确“首次后台执行可能清理超过 90 天的历史”。若 review 不接受，改为默认关闭，不暗中改变全局调度器语义。

### 3.2 数据库维护：轻量与重型分开

- 每 6 小时的默认任务执行受控 `PRAGMA optimize`；不是每 6 小时无条件全库 ANALYZE + VACUUM。
- 使用 bundled SQLite 的真实版本测试 optimize 行为；初始化/模式变更后的统计维护与周期性维护分开。没有数据变化、没有统计动作时允许成功 no-op。
- 显式全量 ANALYZE 不在默认周期路径运行；仅用于维护诊断/后续实测证明有必要的表。
- `vacuum_enabled` 默认 **false**。这是对前文“默认每六小时 VACUUM”建议的安全修正。
- 用户主动启用后，VACUUM 每库至少间隔 604800 秒（7 天），且 `freelist_count * page_size >= 64 MiB`、空闲页比例 `>=20%` 才候选。阈值用整数运算避免溢出/浮点误差。
- 先观测再执行，记录是否命中阈值；小库通常跳过，不能据此宣称未实现。
- 默认不执行 `wal_checkpoint(TRUNCATE)`；不删除/搬移数据库、WAL 或 SHM 文件。

### 3.3 终止态 Jobs 删除规则

- 仅 `succeeded / failed / cancelled` 且 `finished_at < cutoff` 可进入候选；`finished_at IS NULL` 保留。
- queued/running 永不进入候选；正在重试的 job 是 queued，必须保留。
- `cutoff = 固定的本次 now - retention_days * 86400`，运算检查溢出；等于 cutoff 的行保留。
- 每个 def 保留 get_last_child 会读到的最新 terminal。若 finished_at 与 run_after 完全并列，**全部保留并列组**，不改变原先未规定的 tie 选择。
- def 已删除但 def_id 非空的历史仍按 def_id 分组保护最新 terminal；def_id=NULL 的独立历史按年龄删除，不把全部 NULL 当同一个 def。
- 不删除 job_defs，不删除用于恢复/重试的活跃行。新增删除接口前核对实际引用；若发现对 terminal JobId 的持久引用，须加保护，不能忽略。
- **不做“每 def 最多 N 条”的第二套规则。** 首期只有 90 天 + last-child，避免“年龄和条数取 AND 还是 OR”歧义。

### 3.4 播放历史删除规则

- 只处理 `subscribe.db.playback_logs`，以 `ended_at < cutoff` 判断；等于 cutoff 保留。
- 不改 playback_units、播放进度、收藏、playback_sessions、device credentials、Media 或 ledger。
- 启用后，历史页、活动视图以及**依赖 playback_logs 的统计窗口**都只能看到保留期内的数据；不是仅隐藏历史列表。
- 首期不创建汇总归档，不保留被删日志对应的全时期统计；UI 必须提示这一点。
- playback_metrics 是另一类 QoE 记录（recorded_at、TTFF/rebuffer 等），**不随本任务删除**，后续独立定策略。

## 4. 用户配置与交互

### 4.1 单一配置来源

- enabled/schedule：继续使用 job_defs 既有字段和 API。
- retention_days、dry_run、VACUUM 参数：存入该维护 def 的**带版本 JSON payload**，不再在 settings 另存一份。
- 维护 payload 使用 `version: 1`，按 JobKind 反序列化成对应 Rust 类型；未知字段、未知版本、损坏 JSON一律拒绝并记录错误，不回退为更激进的删除策略。
- `retention_days` 合法范围 1..=36500；0 不代表永久保留。永久保留通过关闭该任务表达。
- `batch_size=500`、单次 `max_batches=20` 为服务端固定安全预算，不暴露用户任意扩大。每次最多删除 10000 行，剩余下次继续。
- maintenance 的 `interval_secs` 采用 3600..=31536000；拒绝负数、0、非整数、越界。只对维护任务加范围，其他任务不改变契约。
- 系统维护任务仅管理员可配置/执行；沿用现有管理路由，会员请求拒绝。

### 4.2 新增 API（在既有 Jobs 下扩展）

- `PUT /api/v1/jobs/{id}/maintenance-config`：按 def.kind 验证 body，事务更新该 def payload。非维护 def 返回 400；未知 def 404；并发配置更新需要 expected payload/version token，冲突 409，不采用 JSON schema version 充当修改版本。
- `POST /api/v1/jobs/{id}/maintenance-preview`：执行**有预算的只读预览**，只返回候选/保护数量，不返回用户播放明细。不写 def、不创建删除 Job、不改数据库内容。
- 预览结果以 `scanned/candidates/protected/truncated` 描述；预算不足必须 truncated=true，不伪称全量精确总数。
- `GET /jobs` 增加维护配置摘要与最近执行报告；非维护任务 DTO 保持兼容。报告可以复用 job.progress JSON，需验证终态后仍可读。
- 手动执行 disabled 任务：本期选择**返回 409 job.disabled**，不再无入队却返回 queued:true；用户先开启再执行。任务运行中返回“已有运行实例”或既有排队结果，不能误报新增。

### 4.3 前端

复用现有定时任务卡片的开关/周期/执行按钮，新增维护任务的参数抽屉：保留天数、预览、dry_run；数据库维护显示 VACUUM 高级开关与风险说明。

**不能承诺“前端完全不用改”。** 现有页面没有保留天数编辑器，也没有对应的通用 settings 编辑页；新增 JobKind 可显示，但参数编辑和风险提示需要小幅 FE 开发。

## 5. 执行安全、锁与停止

### 5.1 删除任务

- runner 已在 blocking 池中，禁止把同步 SQLite 工作搬回 Tokio worker。
- Jobs 删除每批持 Queue 短锁、单事务选择并删除；批间释放。Playback 删除每批持 Store 短锁、单事务选择并删除；批间释放。任何路径都不同时持两者。
- 每批重查候选与保护规则，不把事务外查询的 id 列表当成永久可删凭据。
- 固定 now/cutoff，避免一轮执行期间窗口漂移；使用 id + timestamp 稳定游标或每批重新从最旧候选开始。
- shutdown/任务取消在批次边界检查；不承诺强杀当前 SQLite 事务。取消后不启动下一批，已提交批次不回滚。
- 中断/失败可重复执行；每批原子性，不要求跨 app/library/subscribe/jobs 的全局事务。

### 5.2 数据库维护连接

- 仅 allowlist：Store 实际 app_path、catalog/library/subscribe 路径、Queue 实际 jobs_path、已存在 fanart_path；路径由内部事实产生，不接受任意用户文件路径。
- 导出 owned 路径后立即释放 Store/Queue 锁；使用独立连接 `READ_WRITE` 且**不带 CREATE**，不能意外创建空数据库。
- 复用 WAL/外键策略但不要为维护重复迁移/seed；设置短 `busy_timeout=250ms`，不沿用前台 5s 等待造成维护积压。
- 缺失可选 fanart.db 记 skipped，不创建；无法打开真实库是错误，不静默忽略。
- Library 工作正在运行时跳过重型维护。检查只是减少冲突，**不是与新扫描互斥的证明**；最终仍需 SQLite busy 处理和有界执行。
- 单实例维护 mutex + 全部维护 def 共用 concurrency_key `maintenance:database`，防止维护任务互相重叠；不能据此宣称拦住了所有普通写入。
- VACUUM 开始前检查可用磁盘 >= 2 倍数据库主体大小 + 64MiB（保守准入，不保证执行中空间永不变化）；不足则 skipped 并告警。
- 使用 SQLite progress handler/interrupt 支持有界执行，必须验证当前 rusqlite feature 支持；每库上限 30s。不能把 Tokio timeout 丢弃 JoinHandle 当作已停止 SQLite。
- busy/locked 为 skipped 下轮再试；IO/corrupt/full 为 failed，保留错误上下文。VACUUM 成功后记录成功时间；跳过/失败不能推进该库的 7 天冷却时间。
- 不承诺独立连接让前台零延迟：它只避免占用 Rust 全局 mutex，SQLite 层锁与 IO 竞争仍存在。上线先观察 busy 和 HTTP 延迟。

## 6. 报告、观测与回滚

每次 Job 报告至少包含：kind、policy_version、固定 cutoff、dry_run、scanned/deleted/protected、batches、budget_exhausted、elapsed_ms；维护另含每库 outcome、free_pages、page_size、VACUUM 前后主体大小、skip_reason。

- 执行开始/结束 info；跳过/预算耗尽 warn 或结构化 info；意外错误 error。
- SSE watcher 当前使用 `count_finished_since(0)`；**清理会使计数下降**。必须验证下降也触发刷新，客户端不得将此 count 当单调递增 sequence。
- 删除前展示预览与备份提示；启用清理后不可撤销，只能从备份恢复。关闭任务只阻止后续批次/运行，不恢复历史。
- DB 配置开关可回滚；首期不增加表字段，维护报告/冷却元数据可放版本化 def payload 之外的执行状态，避免执行时覆盖用户配置。建议 `job.progress` 保存单次报告、每库成功 VACUUM 时间使用专用 Store setting key `maintenance.vacuum_last_success.{database}`（内部状态，不作策略副本）。
- 不从 live 文件大小、行数快照推算容量承诺。SQLite 在线库观测必须用只读连接/在线 backup API，不能直接 cp 主库后忽略 WAL。

## 7. Jellyfin 参考与适用边界

本次沟通参考的是 Jellyfin master 的这些源码，**不是锁定发行版行为**；实施前记录确切 commit/tag，避免主分支变化。

- [CleanActivityLogTask](https://github.com/jellyfin/jellyfin/blob/master/Emby.Server.Implementations/ScheduledTasks/Tasks/CleanActivityLogTask.cs)：读取 ActivityLogRetentionDays，任务默认触发器为空。
- [ServerConfiguration](https://github.com/jellyfin/jellyfin/blob/master/MediaBrowser.Model/Configuration/ServerConfiguration.cs)：本次查阅 ActivityLogRetentionDays 默认 30 天。
- [OptimizeDatabaseTask](https://github.com/jellyfin/jellyfin/blob/master/Emby.Server.Implementations/ScheduledTasks/Tasks/OptimizeDatabaseTask.cs)：6 小时触发，扫描中跳过。
- [DeleteCacheFileTask](https://github.com/jellyfin/jellyfin/blob/master/Emby.Server.Implementations/ScheduledTasks/Tasks/DeleteCacheFileTask.cs)：缓存目录和临时目录的文件年龄清理，**不等价于本项目 catalog_cache 表**。
- [CleanupUserDataTask](https://github.com/jellyfin/jellyfin/blob/master/Emby.Server.Implementations/ScheduledTasks/Tasks/CleanupUserDataTask.cs)：清 detached UserData，不等价于删除所有用户进度。

借鉴“独立维护任务 + 用户可配 + 扫描避让”，不照搬所有年龄常量、默认触发器或 VACUUM 频率。**本项目没有已证明安全的缓存孤儿映射，本期不实现缓存孤儿删除。**

---

## 8. 分步实施计划

### Task 1：维护任务定义与版本化策略（基础合同）

**Files:** Create `crates/api/src/maintenance/{mod.rs,policy.rs}`；Modify `crates/api/src/lib.rs`、`crates/jobs/src/kinds.rs`、`crates/api/src/jobs_api.rs`；Test `crates/api/tests/maintenance_config.rs`。

**Interfaces:**
- `MaintenancePolicy::parse(kind: jobs::JobKind, json: &str) -> Result<MaintenancePolicy, String>`。
- 三个类型：`DatabaseMaintenancePolicy`、`JobsRetentionPolicy`、`PlaybackHistoryRetentionPolicy`，均 version=1；字段与范围见第 3/4 节。
- `seed_maintenance_defs(queue: &jobs::Queue) -> Result<(), jobs::JobError>`；每类 def 固定 kind + 共用维护 concurrency_key；默认 schedule 均非空。

- [ ] 写行为测试：首次 seed 恰好出现三类 def、默认值符合 3.1；修改 enabled/schedule/payload 后重开 Queue 并重复 seed，值不被覆盖。
- [ ] 写策略解析测试：0/负数/溢出 retention、未知 version/字段、坏 JSON 均拒绝；合法边界 1 与 36500 通过。
- [ ] Run `"$HOME/.cargo/bin/cargo" test -p api --test maintenance_config`，确认行为红或新接口缺失的编译红，区别记录。
- [ ] 实现类型与 seed，保持默认注册不发起真实清理；以公开 Queue 读取断言，而不是 SQL 文本。
- [ ] Run `cargo test -p jobs`、`cargo test -p api --test maintenance_config`（实际使用 `$HOME/.cargo/bin/cargo`），全绿后提交本任务确切文件。

### Task 2：终止态 Jobs 分批保留接口

**Files:** Create `crates/jobs/src/retention.rs`、`crates/jobs/tests/retention.rs`；Modify `crates/jobs/src/{lib.rs,queue.rs}`。

**Interfaces:** `Queue::prune_terminal_jobs(&self, cutoff: i64, batch_size: usize, dry_run: bool) -> Result<RetentionBatch, JobError>`；`RetentionBatch { scanned: usize, candidates: usize, deleted: usize, protected: usize, has_more: bool }`，由 jobs crate 导出。

- [ ] 写公开 Queue 测试：超龄 terminal 删除；cutoff 边界、NULL finished_at、queued/running、正在重试保留；每 def 最新和完全并列 latest 组保留；NULL def_id 独立历史可删。
- [ ] 写“清理后 get_last_child/children_for_defs 结果不变”和失败注入事务回滚测试；至少两批数据证明 batch_size 上限有效。
- [ ] Run `cargo test -p jobs --test retention` 验证红。
- [ ] 单事务内确定候选与保护集合并删除；dry_run 零写入。不得改动调度/follow-up 语义。
- [ ] Run `cargo test -p jobs`，全绿提交。

### Task 3：播放历史分批保留接口

**Files:** Create `crates/store/src/playback/retention.rs`、`crates/store/tests/playback_retention.rs`；Modify `crates/store/src/playback.rs`（模块注册，实施前核对实际模块根）。

**Interfaces:** `Store::prune_playback_logs(&self, cutoff: i64, batch_size: usize, dry_run: bool) -> Result<PlaybackRetentionBatch, StoreError>`；返回字段与 Task 2 同义但类型由 store 自己定义，不依赖 jobs。

- [ ] 用公开 upsert_session/close_session 造历史，断言 ended_at 严格小于 cutoff 才删；重复执行幂等。
- [ ] 保存进度/收藏/活跃会话/metrics，清理后通过公开查询逐项确认不变；历史查询/统计只剩保留窗口数据。
- [ ] dry_run、批次上限与触发器失败回滚测试先红；Run `cargo test -p store --test playback_retention`。
- [ ] 实现仅 playback_logs 的事务批次，不级联业务状态；大历史 fixture 用 test-only 单事务 SQL 加速 setup，断言仍走公开 seam。
- [ ] Run `cargo test -p store`，全绿提交。

### Task 4：数据库维护执行器与 Jobs runner 接入

**Files:** Create `crates/api/src/maintenance/{database.rs,runner.rs,report.rs}`、`crates/api/tests/maintenance_runtime.rs`；Modify `crates/api/src/worker.rs`、`crates/store/src/open.rs`（仅路径快照接口）、`crates/jobs/src/queue.rs`（真实路径接口）、必要的 rusqlite feature 配置。

**Interfaces:**
- `DatabaseTarget { name: DatabaseName, path: PathBuf }`；固定 DatabaseName allowlist。
- `DatabaseExecutor::execute(&self, target: &DatabaseTarget, policy: &DatabaseMaintenancePolicy, context: &MaintenanceContext) -> Result<DatabaseReport, String>`；生产实现独立 SQLite 连接，测试实现可阻塞/报 busy/磁盘不足。
- `maintenance::run(state: &ApiState, job: &jobs::Job) -> Result<(), String>`；读 job payload 快照，批间检查取消/停止，不重新读可变策略改变本轮 cutoff。

- [ ] 写假执行器测试：执行期间 Store/Queue try_lock 成功；扫描活跃跳过 VACUUM；预检后扫描开始/SQLite busy 时有界跳过；磁盘不足、缺失可选库、open_at 自定义 app 路径均符合第 5 节。
- [ ] 临时真实 DB 测试：optimize 不改变业务数据；VACUUM 默认不执行；显式开启后仅满足阈值/冷却才执行；SQLite 中断后无后台遗留操作。
- [ ] 删除 runner 测试：单次最多 20 批、取消后不启动下一批、失败报告与预算耗尽正确；不嵌套 Store/Queue 锁。
- [ ] 测 `/jobs/tick` 与 Queue::tick 调用路径：不得在持 Queue 锁时运行 maintenance；选择拒绝该 HTTP 开发入口执行维护并返回明确错误，正式 job_loop 正常执行。
- [ ] Run `cargo test -p api --test maintenance_runtime` 验证红，再接入 worker 和执行器。
- [ ] Run 所有触及 crate 测试，确认无真实网络/ffmpeg 后提交。

### Task 5：维护配置 API、预览和定时任务页面

**Files:** Create `crates/api/src/http/job_maintenance.rs`、`crates/api/tests/maintenance_http.rs`、`web/components/maintenance-job-settings.tsx`；Modify `crates/api/src/http/{mod.rs,jobs.rs}`、`crates/jobs/src/{queue.rs,store.rs}`（payload CAS 更新接口）、`web/lib/api/jobs.ts`、`web/components/scheduled-jobs-view.tsx`、`docs/api-contracts/self.md`。

**Interfaces:** Task 1 的 typed policy；Task 4 的报告。新增 HTTP 合同见 4.2；payload CAS 比较保存前的原值/专用修改 token，不用 version=1 做并发 CAS。

- [ ] HTTP 红测：管理员合法保存/预览成功、会员拒绝；无关 kind/未知 def/坏配置分别报错；CAS 冲突 409；preview 不写 DB，truncated 不伪报精确总量。
- [ ] HTTP 红测：disabled 手动 run 返回 409；任务运行中修改配置只影响下次执行；调频率和重启不复位；关闭不启动后续批次。
- [ ] 测 watcher：terminal 计数因为清理下降时仍发更新，客户端不会等待“计数必须增长”。
- [ ] 实现 API 与页面参数编辑/风险提示，保留现有非维护任务行为；播放历史删除提示必须涵盖统计影响。
- [ ] Run `cargo test -p api`；FE 执行仓库现有 typecheck/build 命令（实施前读取 package.json 确认，不能凭空指定脚本）。全绿提交。

### Task 6：整体验收与发布开关

**Files:** Test `crates/api/tests/maintenance_acceptance.rs`；Update 本文验收记录及 `docs/api-contracts/self.md`。

- [ ] 以临时数据库运行真实 job_loop：三任务默认值正确；Jobs 清理删除超龄但保 last-child，Playback 关闭时零删除，开启后只影响日志与对应统计。
- [ ] 验证首次执行、用户改动重启保留、取消/预算恢复、数据库 busy/磁盘不足/停止行为。
- [ ] 量化启用前后 HTTP median/p95、Queue/Store 锁等待、维护耗时、主体/WAL 大小；不设“必然变快/必然缩小”验收口号。
- [ ] 对大历史补 200001 行本地 fixture（不触碰 live），记录批次边界与实际执行成本；性能测试默认 ignored，不把大型 fixture 塞进常规 CI。
- [ ] 执行 touched-crate 测试后，在无并发编辑/cargo clean 的稳定工作树运行一次 `"$HOME/.cargo/bin/cargo" test --workspace`，立即捕获 cargo 真实 exit code。
- [ ] 核对文件/函数硬限、git diff --check；仅提交本任务文件，不夹带未跟踪审计文档或用户其它计划。

## 9. 依赖与交付边界

顺序：Task 1 → Task 2/3（可并行，避免同一模块根并发写）→ Task 4 → Task 5 → Task 6。

首期不要求 DB11 全量 typed settings 注册表；本方案只给维护 payload 做局部 typed/版本验证。未来若迁移配置，只能保留一个事实来源，不在 settings 和 job_defs 各存一份策略。

**文档 review 需要确认的两点：**
1. 接受“数据库维护默认开，但 VACUUM 子功能默认关；满足阈值后且用户开启才重建”的安全修正。
2. 接受“Jobs 超过 90 天清理默认开；播放历史超过 90 天清理默认关”，以及用户改动保留、首次调度可能立即运行的语义。

确认前不实施，不自动删除任何数据。

---

## 10. 验收记录（2026-10-13 实施完成）

实现提交（分支 main，起点 `4670239`）：

| 提交 | 内容 |
|---|---|
| `c03f39c` | 版本化维护策略 + 三类默认 def |
| `cbc98bb` | `Store::prune_playback_logs` 分批保留 |
| `7ce1c60` | `Queue::prune_terminal_jobs` 分批保留 |
| `dd5e009` | 数据库维护执行器 + runner 接线 + seed 接线 |
| `48d6efa` | 维护配置 CAS / 只读预览 / 策略 DTO / 定时任务页参数抽屉 |
| `b06eb9a` | 端到端验收测试 |

**最终门**：`cargo test --workspace` → **exit 0，226 个套件、1663 个测试通过、0 失败**（在无并发编辑的稳定工作树上运行）。
`pnpm -C web typecheck` → exit 0。硬限：除既有 `crates/api/tests/management/cli.rs`（820 行）外无超标文件。

**逐任务证据**：

| 任务 | 测试命令 | 结果 |
|---|---|---|
| Task 1 | `cargo test -p api --test maintenance_config` | 7 passed（先红：编译红，新接口缺失） |
| Task 2 | `cargo test -p jobs` | 45 passed（先红：编译红 + stub 后 7 failed 行为红） |
| Task 3 | `cargo test -p store` | 151 passed（先红：编译红） |
| Task 4 | `cargo test -p api` | 997 passed（先红：编译红） |
| Task 5 | `cargo test -p api` + `pnpm -C web typecheck` | 1010 passed + typecheck exit 0（先红：**行为红**，新端点 404 空体） |
| Task 6 | `cargo test -p api --test maintenance_acceptance` | 4 passed + 1 ignored |

**大历史批次边界实测**（`--ignored` 手动跑，200_001 行超龄历史）：

```
run#1: batches=20 deleted=10000 budget_exhausted=true elapsed_ms=45
run#2: deleted=10000            ← 下一次运行继续推进，不是 while has_more 死循环
```

**实施中偏离计划并已记录的决定**：

1. Task 1 的 `seed_maintenance_defs` 不在启动时接线（worker 三个维护分支临时返回 Err），接线放到 Task 4——避免中间提交后应用按默认频率调度永远失败的任务。
2. `/jobs/tick` 用 **200 + `skipped_maintenance`** 而非计划里的 409：409 会打断 17 个既有 management 测试（它们都用该入口跑非维护任务）。开发入口不为维护 def 排期并显式报告；正式 `job_loop` 正常执行。
3. **VACUUM 的页拷贝阶段实测不可被 `sqlite3_interrupt` 中断**（rusqlite 0.37 + bundled SQLite；`sqlite3_backup` 不检查 interrupt 标志，语句之间的 interrupt 会被丢弃）。看门狗按规格实现，但上限到期若落在拷贝阶段停不下来：该库如实记 `Vacuumed` 且 `elapsed_ms` 超限，**不谎报 Failed**。未擅自加"最大库体积"准入上限（属产品决策，列为剩余风险）。
4. `DatabaseExecutor::execute(..., vacuum_enabled: bool, ...)` 用逐字接口；另加 `SqliteLimits`/`ConfiguredSqliteExecutor` 测试 seam，否则 64MiB/20% 阈值在小库上无法验证真实 VACUUM/磁盘不足/中断路径。
5. 新增 `libc` 依赖（std 无跨平台可用空间 API；已在依赖树中）；非 unix 平台取不到可用空间时保守跳过 VACUUM。
6. 维护连接不重复设 `journal_mode=WAL`（持久属性已在文件头，避免为维护引入额外写锁），只设 `busy_timeout=250ms` + `foreign_keys=ON`。
7. `expected_payload` 必填（缺省 400 `job.missing_token`），CAS 按 JSON 结构比较；`interval_secs` 与 payload 不在同一事务（payload CAS 先提交，schedule 后写）。
8. 新增 `Store::preview_playback_logs` 只读预览：播放行在私有 subscribe 连接里，api crate 无法直接计数。
9. 改了 3 个不在原交付物清单的文件：`job_loop.rs`、`http/jobs.rs`（tick 行为必需）、`tests/management/jobs.rs`（取"最后一个 enabled def"，因新增默认关闭的维护 def 排在最后；断言未减弱）。

**仍未做/剩余风险**（详见实施者报告与第 6 节）：

- **未做完整的启用前后 HTTP median/p95 对比矩阵**——本轮只记录了大历史批次边界的绝对耗时（45ms / 10000 行）。原计划的"量化启用前后 HTTP median/p95、锁等待、主体/WAL 大小"未执行。
- VACUUM 拷贝阶段 30s 上限只是尽力而为，且未加库体积准入上限。
- `has_active_work()`、Queue 锁探测、磁盘准入都是**快照式"降低冲突"**，不是与 Library 扫描互斥的证明。
- 进程内维护 mutex 不覆盖多进程；取消只在批次/库边界检查，且当前生产没有取消运行中维护任务的写入者。
- 前端 `last_report` 已交付并测到 DTO 层，但抽屉尚未渲染成历史列表。
