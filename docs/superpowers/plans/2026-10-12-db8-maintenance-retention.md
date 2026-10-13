# DB8 数据库维护与历史保留：现行合同、实现状态与剩余门

**状态：主功能已实现，控制／报告／有界性存在待修项，尚未完整验收。** 代码快照 `723394e`（2026-10-13）。本页取代此前“待 review、仅文档”与“全部完成”并存的状态；不是重跑旧六任务的执行指令。

权威状态：[数据库／接口总复核](<../../audits/2026-10-13-database-api-review.md>)；唯一后续任务：[T1–T9](<2026-10-13-database-api-followup.md>)。数据库职责：[ADR-0008](<../../adr/0008-sqlite-system-of-record.md>)；Job 调度：[ADR-0009](<../../adr/0009-job-queue.md>)；API：[约定](<../../api-contracts/self.md>)。

## 1. 已确定的范围与默认值

复用 Jobs：enabled/schedule 在 job_defs，维护策略只存带版本 payload，不在 settings 再存一份。冷却时间是执行元数据，不是策略副本。

| kind | 首次注册 enabled | interval | 默认 payload |
|---|---|---|---|
| database_maintenance | true | 21600s | `{version:1,vacuum_enabled:false}` |
| jobs_retention | true | 86400s | `{version:1,retention_days:90,dry_run:false}` |
| playback_history_retention | **false** | 86400s | `{version:1,retention_days:90,dry_run:false}` |

已有定义以 kind＋concurrency_key 命中即保留，不因重启覆盖用户配置。关闭仍保留 schedule，现有定时任务页可见。调度只支持 Interval，未实现 cron／固定凌晨时间。

**升级警告：首个默认开启任务可能立即入队，不等满一个周期；Jobs 历史超过90天可能在首次后台执行时清理。** DELETE 不可撤销，恢复依赖一致备份；本轮文档整理没有运行真实清理。

首期不自动删 fanart/catalog/subtitle cache、probe/fingerprint、playback_metrics、封面版本文件。TTL 是 freshness，不等于可删期限；过期 catalog 仍承担 stale-if-error。

## 2. 数据与输入合同

- version=1，unknown version/unknown字段/损坏 JSON 拒绝，不能回落更激进策略。
- retention_days=1..=36500，0 不代表永久保留；永久保留通过禁用任务表达。
- 维护 interval=3600..=31536000 秒；维护配置端点与旧 schedule 端点均检查。非维护任务旧范围不在本页收紧。
- 固定 cutoff=本次 started_at（缺省 now）−retention_days×86400，checked 运算；严格小于，等于边界保留。
- batch_size=500，max_batches=20，实删每轮最多10000行。**这只证明返回／删除行数预算，不证明 SQL 扫描或持锁时间有界。**

### Jobs

仅 succeeded/failed/cancelled 且 finished_at 非NULL、超龄。queued/running（包括重试排队）绝不删；不删 job_defs。

每个非NULL def_id 保留最新 terminal，排序 finished_at DESC/run_after DESC；完全并列整组保留。已删除 def 的历史仍保最后组。def_id=NULL 独立行按龄处理，不把全部NULL分成同一保护组。每批事务内选择与删除，失败整批回滚；跨批不全局回滚。

`d1c8205` 已把可删谓词放入候选窗口，解决“最旧500行全被保护导致永远不推进”。它**没有**解决扫描量和保护报告：protected 独立诊断最多501，preview 未标其截断，runner 跨批重复累加。不得按当前数值声称精确保护总数，见 F03。

### Playback

只删 subscribe.db.playback_logs 的 ended_at<cutoff，不删 playback_units（进度／收藏）、playback_sessions、设备凭据、QoE metrics、Media、ledger。历史页与依赖日志的统计窗口会随删除收缩；未做汇总归档，不保留被删日志的全时期统计。

候选按 started_at/id 排序但 ended_at 过滤，现有索引并不保证无过期数据时短扫描；整改见 F05。

## 3. 维护连接与 VACUUM

[实现](<../../../crates/api/src/maintenance/database.rs>)使用内部 allowlist：真实 Store app_path，data_dir 下 catalog/library/subscribe/jobs.db，已存在 fanart.db。当前 Queue 生产构造固定 jobs.db，所以推导路径正确；尚未提供 Queue 真实路径 accessor，不宣称支持任意可配置 jobs 路径。

独立连接 READ_WRITE、不带 CREATE，不重复迁移／seed／设置 WAL；busy_timeout250ms、请求 FK ON；不开缺失可选 fanart，报告 skipped。不删除/搬移 DB/WAL/SHM，不自动 TRUNCATE checkpoint。

默认仅 `PRAGMA optimize`，允许 no-op，不默认全量 ANALYZE/VACUUM。没有显式 ANALYZE 不等于索引不用；DELETE 后页可重用，文件不缩小不等于删除无效。

VACUUM 需用户显式开＋距每库上次成功≥604800s＋free页≥64MiB＋free占比≥20%＋可用磁盘≥2×主体大小＋64MiB。Library 工作预检活跃时跳过 VACUUM、仍可 optimize；磁盘／忙碌预检是快照，不与新扫描构成互斥。busy/locked skipped，IO/corrupt/full failed。

**期限偏差尚未验收：** 30s interrupt 看门狗只能尽力中断可响应阶段，SQLite backup-copy 及阻塞文件系统不保证响应。执行器真实结束后才 join；拷贝超限可能仍报 Vacuumed＋超限elapsed。900s Job timeout 不是强制停止机制，active attempt 会排除 recovery。不能继续写“每库硬30s已实现”；重型路径的发布合同须按 T4 单独确认。

三个维护 def 当前使用独立 key（maintenance:database/jobs_retention/playback_history），进程内 shared mutex 避免彼此重叠，但冲突会失败重试。旧规格“全部共享 maintenance:database”**未实现**；不未经兼容迁移直接改key，否则幂等seed可能重复注册已有def。

## 4. API/UI：目标合同与当前偏差必须分开

端点均为管理员管理路由：

- PUT `/api/v1/jobs/{id}/maintenance-config`：typed payload，expected_payload CAS（不是schema version）；缺token400，冲突409，非维护400，未知404。
- POST `/api/v1/jobs/{id}/maintenance-preview`：只读，不创建Job／更新配置／返回播放明细。仅一批匹配候选；不是全量精确规模。
- GET `/jobs` 与单定义：maintenance摘要、payload_token、last_report，非维护DTO兼容。
- run disabled409；运行中不误报新实例；现有queued child短路当前未提前run_after，故“立即执行”尚有缺陷。

**已确认偏差：**

| 期望 | 当前实现 | 后续 |
|---|---|---|
| 关闭后不启动下一批／下一库 | 只查running status/attempt，不查def.enabled；仍可继续删 | F01/T1 |
| 更安全配置用于尚未运行的下次任务，running保留快照 | def更新但future queued payload不变；可能仍实删旧策略 | F02/T2 |
| 全配置CAS原子，失败全回滚 | payload事务先提交，schedule后写；仅payload token不覆盖周期 | F04/T2 |
| 省略周期返回实际保留值 | handler响应schedule=null，DB未清空 | F04/T2 |
| 有预算预览标全部截断 | 仅可删has_more决定truncated，保护窗口截断遗漏；SQL工作未有界 | F03/F05 |
| UI显示本策略预览与最近报告 | 编辑不清旧预览／晚响应未绑定；last_report未渲染 | F12/T5 |
| DevHttp tick跳过维护且不动后台任务 | 会取消queued维护子任务、遮盖最近报告；后台可立即补任务 | F12/T5 |

当前界面“关闭只阻止后续批次”“配置只影响下次执行”的文案是**预期合同而非已成立保证**。不得通过把合同改成允许继续删，来假装修复实现；发布前修控制边界。临时规避不要仅凭UI关闭判断工作停止，应确认running Job已实际结束，修改安全策略后核验queued快照。

## 5. 报告与停止

报告存job.progress：kind/version/cutoff/dry_run/scanned/candidates/deleted/protected/batches/budget_exhausted/elapsed_ms；数据库另含每库outcome/free_pages/page_size/size_before/after/skip_reason。

- dry_run不删除，只检视一批；更多可删候选时budget_exhausted=true＋note，不重复检视同一批制造候选总数。
- scanned/candidates当前是匹配窗口计数，不是SQLite实际examined行；protected不可用于完整总量判断（F03）。
- SSE count_finished_since(0)可因清理下降，watcher按变化而非单调增长推送，已有回归。
- 每批各自Queue或Store锁，不同时持两者；每轮runner在blocking工作运行。preview仍直接同步执行SQL，尚不满足同等线程隔离。
- Job状态／attempt改变会在边界停止；**禁用def、shutdown不是当前已接入的停止信号**。取消只在边界检查，已提交批不回滚。
- Queue持锁路径通过100ms try_lock探测拒绝维护，这是启发式并会误判其他线程占锁；需显式执行来源。

## 6. 实施／验收证据

| 提交 | 已交付 |
|---|---|
| c03f39c | typed策略＋默认定义 |
| cbc98bb / 7ce1c60 | Playback／Jobs事务批次 |
| dd5e009 | 执行器＋runner＋启动seed |
| 48d6efa | config/preview/DTO/UI |
| b06eb9a | ApiState＋StateRunner验收（**不是真实background loop**） |
| d1c8205 | 保护前缀推进、dry_run更多信号、旧schedule端点范围＋saturating next_after |

本次review新跑workspace：226套件1666passed/0failed/4ignored；web typecheck exit0。它没有覆盖F01/F02/F03等新发现的时序。

历史大Playback fixture：200001超龄行，run1=20批/10000删除/预算耗尽/45ms（debug本机历史记录），run2再10000。不是维护并发HTTP p95、不是全部保护的Jobs历史、不证明SQL扫描有界、不是实机SLA。

仍未完成：真实job_loop与shutdown/drain端到端、禁用边界／queued策略更新／原子config／报告截断／preview线程预算／legacy影响；DB8开关前后HTTP median/p95、锁等待、DB/WAL大小矩阵；VACUUM停止保证决策。按 [后续计划](<2026-10-13-database-api-followup.md>)执行，不再用旧六任务全部unchecked或“主功能在”代替验收结果。

## 7. 参考与非目标

Jellyfin的独立维护任务只作为设计启发；此前参考master的ActivityLogRetentionDays／OptimizeDatabaseTask没有固定上游commit，不把其默认值当本项目规范或已验证发行版事实。本项目默认值见§1；不需要借外部主分支行为证明本地删除安全。

不自动实现cache orphan GC、所有表retention、连接池／跨文件事务、默认分页迁移、所有时间戳重写、全局唯一alias/rootpath或Jellyfin全协议。必须先有各自迁移／兼容／安全证据。
