# 搜索、订阅、转移与维护共用一个 Job 队列

实例能排队、运行、重试或检查的每个工作单元都是 **Job** 行。搜索、**Subscribe** 填充、**Transfer**、**Scrape**、**Check-in**、catalog 刷新，以及以后的 Watch-directory 扫描都进同一张表。不存在带私有内存映射的第二个“scheduler 模块”。

MoviePilot 拆分：APScheduler 进程内任务（`subscribe_search`、`transfer`、`sitedata_refresh`、…）外加独立的 `JobManager` 管理单个媒体的 transfer 任务。重启会丢失运行中的 APScheduler 状态；transfer 任务又是另一个 dict。那是两套 UI 和两套重试故事。我们只有一个 NAS 进程和 SQLite——一个队列就够了。

**Hook** 保持步骤拦截器（ADR-0002）。**Job** 是工作单元；**Hook** 可以在其中的步骤上行使否决。登录/**Check-in** **Plugins** 由该类 **Job** *调用*，它们本身不是队列。

## 位置

Store 四文件之外的队列文件：`{data}/jobs.db`。它不是可随意清空的cache：清空会丢用户修改的def策略／开关／周期、queued/running工作、重试和历史。Media身份留在app，catalog是缓存，ledger在library，Subscribe事实在subscribe（[ADR-0008](<0008-sqlite-system-of-record.md>)）。终态历史可按明确的latest/tie保护规则分批删；不得据“不丢Media身份”推断整个jobs.db可丢。

两张表：

```
job_defs (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,            -- subscribe_search | subscribe_rss | transfer
                                 -- | scrape | check_in | catalog_refresh | watch_intake
  name TEXT NOT NULL,
  enabled INTEGER NOT NULL,
  schedule TEXT,                 -- null = 仅按需；现行仅 interval:<secs>，未实现 cron
  payload TEXT NOT NULL,         -- JSON: subscribe_id, site_id, root_id, …
  timeout_secs INTEGER,
  concurrency_key TEXT           -- 可选互斥，如 site:<id> 或 subscribe:<id>
)

jobs (
  id TEXT PRIMARY KEY,
  def_id TEXT,                   -- null 表示一次性（手动搜索）
  kind TEXT NOT NULL,
  payload TEXT NOT NULL,
  status TEXT NOT NULL,          -- queued | running | succeeded | failed | cancelled
  attempt INTEGER NOT NULL DEFAULT 0,
  run_after INTEGER NOT NULL,    -- unix 秒
  started_at INTEGER,
  finished_at INTEGER,
  error TEXT,
  progress TEXT,                 -- 简短的运维可见行，不是日志文件
  -- 现行分别对 queued 和 running 建部分UNIQUE(def_id)；各状态至多一条，
  -- 不是一个覆盖两状态的共同唯一约束。应用逻辑另控制普通排期的live child。
)
```

WAL。进程内通过Queue mutex串行访问该连接，**不是专用单写线程**。API进程就是worker；正式循环claim时持锁、StateRunner执行离锁、落结果短锁，无Redis或独立worker服务。

## 运行时

每秒一次循环：`SELECT … FROM jobs WHERE status='queued' AND run_after<=now ORDER BY run_after LIMIT N`。用 `UPDATE … SET status='running' WHERE status='queued'` 认领，崩溃不会双跑同一行。进程启动时，超过 `timeout_secs` 的 `running` 变为 `queued` 且 `attempt+1`。`attempt` 达到 5 后置为 `failed` 并停止。

带schedule的def在完成时spawn follow-up（run_after=完成时间＋interval）；缺live child时ensure_scheduled也可立即补首个子任务。这里只是Interval循环，不是已支持cron表达式。UI 列出 `job_defs`（能跑什么）与近期 `jobs`（跑了什么）。手动搜索 POST 一个 `def_id` 为 null 的一次性 `jobs` 行。

并发：每tick最多claim2条，当前tick内逐条执行；后台每秒派发blocking tick且不等待上一tick结束，因此**不等价于全实例最多2个运行任务**。`concurrency_key`阻止同key的running重叠；不同key可跨tick并行，若需全局并发上限须单独设计／验收。

## Kind → 现有模块

| kind | payload | 调用 |
|---|---|---|
| `subscribe_search` | `subscribe_id` | Indexer.search + Filter + subscribe::run |
| `subscribe_rss` | （无 / site 列表） | Indexer.rss，然后对每个匹配 **Subscribe** 跑同一 run |
| `transfer` | `downloader_id` 可选 | 轮询已完成文件、**Naming template**、ledger |
| `scrape` | `ledger_id` 或 path | scrape_beside |
| `check_in` | `site_id` | Check-in **Plugin** |
| `catalog_refresh` | `media_id` | TMDB（等）经 `catalog.db`，upsert **Media** |
| `watch_intake` | `root_id` | 以后 |

`subscribe::run` 今天一次性做 search-admit-add-poll-transfer。在 Job 边界拆分：`subscribe_search` Job **加**到 Downloader 即结束；`transfer` Job 之后看到已完成文件。在搜索 Job 内轮询 qB 正是 live harness 卡在 `ledger=0` 的原因。Downloader 是外部的；完成是更晚的 Job。

手动关键词搜索是没有 **Subscribe** 的 `subscribe_search` 一次性 Job（payload 有 `query` + `filter_id`）。同一 runner，同一 UI 日志行。

## 默认（启用）

- 每个 active **Subscribe** 一个 `subscribe_search`，间隔 30 分钟（search **Fetch mode** 或 both）。
- 实例级 `subscribe_rss`，间隔 10 分钟（rss 或 both）。
- 实例级 `transfer`，间隔 30 秒。
- 每个启用 **Site** 一个 `check_in`，间隔 12 小时，`concurrency_key=site:<id>`。
- 每个被 **Subscribe** 指向的 **Media** 一个 `catalog_refresh`，间隔 24 小时。

创建 **Subscribe** 会插入其 `job_defs` 行。删除它会取消 queued 子行并禁用该 def。

## 维护任务（2026-10-13 状态）

已注册database_maintenance、jobs_retention、playback_history_retention；默认开关／周期／payload、latest/tie保护与剩余控制缺陷见 [DB8合同](<../superpowers/plans/2026-10-12-db8-maintenance-retention.md>)。保留参数只在job_defs.payload，schema version不是配置并发revision。

当前关闭只取消queued，running retention尚未在批边界检查def.enabled；保存新策略未刷新future queued快照。这两条是已确认待修项，不是本ADR许可继续删除的合同。`/jobs/tick`的开发skip会取消queued维护子任务，也不是无副作用的“观察”。详见 [总复核](<../audits/2026-10-13-database-api-review.md>)。

## 不做这些

- APScheduler / cron 守护进程 / 额外进程。
- 每文件 transfer 子任务（MoviePilot 的 `task` vs `job`）。一个 `transfer` Job 处理 Downloader 自上次运行以来完成的任何内容；ledger 行是每文件记录。
- 把 Indexer 或 Downloader *作为* Job kind 内嵌。它们保持模块。Job 只做编排。
- Job kind 的插件市场。只有第一方 kind，与 **Plugins** 相同（ADR-0002）。
