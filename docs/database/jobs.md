# jobs.db：通用 Job 调度、执行、重试与历史保留

[返回数据库总览](README.md)

当前基线：`main@2c3a5c7`。jobs::Queue；PRAGMA user_version 实测为 0，使用幂等 migrate；0 不表示没有表或未初始化。

共 **4** 张代码创建的表；字段、类型、默认值、主键、唯一约束、外键和索引来自当前初始化代码在全新临时目录中执行迁移后的实际 schema。

## 表目录

- [job_change_state](#job_change_state) — 3 个字段
- [job_defs](#job_defs) — 8 个字段
- [jobs](#jobs) — 13 个字段
- [retention_cursors](#retention_cursors) — 5 个字段

## job_change_state

Job 更新流用的单例修订/终态计数，避免每次订阅更新都全表聚合。

主要实现：[crates/jobs/src/change_state.rs](../../crates/jobs/src/change_state.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `INTEGER` | 1 | 否 | 无 | 单例主键，固定为 1（CHECK id = 1）。 |
| `revision` | `INTEGER` | — | 是 | `0` | 单例单调修订号；不是跨 DB 全局事务序号。 |
| `finished_count` | `INTEGER` | — | 是 | `0` | 满足终态时间判定的 jobs 行数，由 INSERT/UPDATE/DELETE 触发器增减；不是只增不减的累计完成量。 |

**键与约束**

- 主键：`(id)`。
- 表级/列级约束：`CHECK(id = 1)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

无额外命名索引；主键/UNIQUE 的 SQLite 自动索引不重复列出。

**使用注意**：单例 id=1（CHECK）。revision 同时由 jobs 和 job_defs 变化推进；finished_count 随 Job 删除回减，并非历史上总共完成过多少次。

## job_defs

Job 定义：调度周期、启用状态、payload 与并发/超时策略；用户配置不可当缓存丢弃。

主要实现：[crates/jobs/src/store.rs](../../crates/jobs/src/store.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `TEXT` | 1 | 否 | 无 | 该表记录主键；UUID 文本为主，具体类型见字段类型。 |
| `kind` | `TEXT` | — | 是 | 无 | 与 jobs.kind 同一 JobKind 标签体系。 |
| `name` | `TEXT` | — | 是 | 无 | 显示名称或内部对象名；依表上下文。 |
| `enabled` | `INTEGER` | — | 是 | 无 | 是否启用；SQLite 0/1。 |
| `schedule` | `TEXT` | — | 否 | 无 | Job 调度序列化文本；当前 Schedule::Interval 写成 interval:<secs>，NULL 表示无周期。 |
| `payload` | `TEXT` | — | 是 | 无 | 序列化业务负载/查询结果；字段名不保证所有表都使用相同 JSON 结构。 |
| `timeout_secs` | `INTEGER` | — | 否 | 无 | Job 超时秒数。 |
| `concurrency_key` | `TEXT` | — | 否 | 无 | Job 并发互斥键；相同键的运行实例需串行。 |

**键与约束**

- 主键：`(id)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

无额外命名索引；主键/UNIQUE 的 SQLite 自动索引不重复列出。

**关联触发器**：`trg_job_defs_change_del`、`trg_job_defs_change_ins`、`trg_job_defs_change_upd`。触发器负责的 journal/revision 逻辑见总览；不等于声明 FK。

**使用注意**：schedule 当前为 interval:<secs> 或 NULL；payload 是按 JobKind 解码的 JSON 文本，维护策略/启用状态属于用户配置，不是可弃缓存。

## jobs

通用 Job 执行实例与重试状态；由 def_id 指向可选定义，也支持一次性 Job。

主要实现：[crates/jobs/src/store.rs](../../crates/jobs/src/store.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `TEXT` | 1 | 否 | 无 | 该表记录主键；UUID 文本为主，具体类型见字段类型。 |
| `def_id` | `TEXT` | — | 否 | 无 | 可选逻辑引用 jobs.db.job_defs.id；一次性 Job 可以没有定义。 |
| `kind` | `TEXT` | — | 是 | 无 | JobKind 的序列化名称；包括 subscribe_search、subscribe_rss、transfer、scrape、check_in、catalog_refresh、watch_intake 及维护类。 |
| `payload` | `TEXT` | — | 是 | 无 | 序列化业务负载/查询结果；字段名不保证所有表都使用相同 JSON 结构。 |
| `status` | `TEXT` | — | 是 | 无 | 通用 Job 状态：queued/running/succeeded/failed/cancelled。 |
| `attempt` | `INTEGER` | — | 是 | `0` | 已记录的失败尝试次数（含失败/超时恢复路径）；领取和成功完成不递增，不是执行总次数。 |
| `run_after` | `INTEGER` | — | 是 | 无 | Job 最早可运行时刻，Unix 秒。 |
| `started_at` | `INTEGER` | — | 否 | 无 | 通用 Job/Playback 开始时间，Unix 秒。 |
| `finished_at` | `INTEGER` | — | 否 | 无 | 通用 Job 结束时间，Unix 秒。 |
| `error` | `TEXT` | — | 否 | 无 | 可选错误说明；不是错误状态的唯一判据。 |
| `progress` | `TEXT` | — | 否 | 无 | Job 进度/报告的序列化文本。 |
| `concurrency_key` | `TEXT` | — | 否 | 无 | Job 并发互斥键；相同键的运行实例需串行。 |
| `timeout_secs` | `INTEGER` | — | 否 | 无 | Job 超时秒数。 |

**键与约束**

- 主键：`(id)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

```sql
CREATE INDEX jobs_finished_idx ON jobs(finished_at)
            WHERE finished_at IS NOT NULL;
CREATE INDEX jobs_last_def_idx
            ON jobs(def_id, finished_at DESC, run_after DESC)
            WHERE status IN ('succeeded','failed','cancelled');
CREATE INDEX jobs_live_def_idx
            ON jobs(def_id, status, run_after) WHERE status IN ('queued','running');
CREATE INDEX jobs_payload_idx ON jobs(payload);
CREATE UNIQUE INDEX jobs_queued_def
            ON jobs(def_id) WHERE def_id IS NOT NULL AND status = 'queued';
CREATE UNIQUE INDEX jobs_running_def
            ON jobs(def_id) WHERE def_id IS NOT NULL AND status = 'running';
CREATE INDEX jobs_running_key_idx
            ON jobs(concurrency_key) WHERE status = 'running';
CREATE INDEX jobs_running_status_idx
            ON jobs(status) WHERE status = 'running';
CREATE INDEX jobs_status_run_after_idx
            ON jobs(status, run_after) WHERE status = 'queued';
```

**关联触发器**：`trg_jobs_change_del`、`trg_jobs_change_ins`、`trg_jobs_change_upd`。触发器负责的 journal/revision 逻辑见总览；不等于声明 FK。

**使用注意**：queued/running/succeeded/failed/cancelled；def_id 可空。两个部分唯一索引分别约束每定义的 queued 和 running 实例，允许一个 running 加一个 queued；并非 def_id 在全表唯一。concurrency_key 的运行互斥另由领取逻辑检查。

## retention_cursors

按维护 Job 定义保存历史清理的 keyset 断点和周期截点。

主要实现：[crates/jobs/src/retention_cursor.rs](../../crates/jobs/src/retention_cursor.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `def_id` | `TEXT` | 1 | 否 | 无 | 历史保留任务的游标命名空间/定义 ID；通常按维护 def 隔离，不声明 FK。 |
| `cutoff` | `INTEGER` | — | 是 | 无 | 本轮历史清理固定的时间截点；沿用 Job 终态时间的 Unix 秒口径。 |
| `finished_at` | `INTEGER` | — | 是 | 无 | keyset 窗口最后处理的 Job finished_at，Unix 秒；与 last_rowid 一起消歧。 |
| `last_rowid` | `INTEGER` | — | 是 | 无 | 清理断点同时间戳下的 rowid 消歧游标。 |
| `cycle_started_at` | `INTEGER` | — | 是 | 无 | 最近一次非最终窗口保存游标的时间，Unix 秒；当前实现每窗口覆盖，并非稳定的周期起点。 |

**键与约束**

- 主键：`(def_id)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

无额外命名索引；主键/UNIQUE 的 SQLite 自动索引不重复列出。

**使用注意**：这里是 jobs.db 通用历史保留游标；data_retention 的分类清理断点还使用 app.db.settings 命名空间，不能假定所有清理游标都在本表。
