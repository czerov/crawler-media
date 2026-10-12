# DB5：jobs 热查询的查询计划与索引证据

范围：`crates/jobs` 的四条热查询。目标是在**代表性合成数据**上用**项目实际依赖的
bundled SQLite** 拿到 before/after 的 `EXPLAIN QUERY PLAN` 与同进程实测耗时，
只添加计划与实测都证明必要的索引。

> 结论边界：以下数字只描述本机、本数据集、bundled SQLite 3.50.2 下的观测。
> **20 万行上的倍数不能外推到生产**；生产规模、并发与磁盘状态都不同。
> 本文件不继承旧报告「约 300 倍」的结论（那来自另一个 SQLite 版本的孤立 EQP）。

## 1. 测量条件

| 项 | 值 |
|---|---|
| SQLite | 3.50.2（`SELECT sqlite_version()`，rusqlite `0.37` + `bundled` feature，即生产同一份 amalgamation） |
| 数据集 | 200 000 行 `jobs`：200 running / 1 800 queued / 198 000 终止态（succeeded/failed/cancelled 约 1:1:1） |
| def 分布 | 终止态挂在 500 个 def 上（每个约 396 条）；每条 running/queued 各挂独立 def，另有 1 个 def 同时有 queued + running |
| concurrency_key | running/queued 用 `sub-0..sub-49`；终止态为 NULL |
| 统计信息 | 每个状态转换后执行 `ANALYZE`（生成 `sqlite_stat1`） |
| 计时 | 同一进程、同一数据集、同一条已 prepare 的语句连续执行 **51** 次，记录**中位数**与**最小值**（ms） |
| 机器 | 共享开发机，同时有其它 cargo/测试负载；中位数会被并行负载抬高，**最小值**更接近查询本身开销。两次不同负载下的绝对数字差异明显，故以 EQP 为主证据、耗时只作量级参考 |

复现命令（`crates/jobs/tests/query_plans.rs` 里 `#[ignore]` 的测量 harness）：

```bash
cargo test -p jobs --test query_plans -- --ignored --nocapture            # 代表性分布
JOBS_DENSE_RUNNING=1 cargo test -p jobs --test query_plans -- --ignored --nocapture  # 敏感性检查
```

harness 的流程：`Queue::open` 建库 → 原始连接批量插入 20 万行 → `ANALYZE` →
`DROP INDEX` 掉候选索引 → 采集 **BEFORE** → 创建候选索引 → `ANALYZE` → 采集 **AFTER**。
因此 BEFORE 精确对应改动前的索引集合，AFTER 与 `store::migrate` 交付的状态一致。

## 2. 被测查询（与源码逐字相同）

```sql
-- key_busy（改动前，OR 形状）
SELECT COUNT(*) FROM jobs
 WHERE status = 'running' AND id != ?3 AND (
     (?1 IS NOT NULL AND concurrency_key = ?1) OR
     (?2 IS NOT NULL AND def_id = ?2)
 );

-- key_busy（改动后，两次索引探测）
SELECT EXISTS(SELECT 1 FROM jobs
  WHERE status = 'running' AND id != ?1 AND concurrency_key = ?2);
SELECT EXISTS(SELECT 1 FROM jobs
  WHERE status = 'running' AND id != ?1 AND def_id = ?2);

-- get_live_child
SELECT id, def_id, kind, payload, status, attempt, run_after, started_at, finished_at,
       error, progress, concurrency_key, timeout_secs
  FROM jobs
 WHERE def_id = ?1 AND status IN ('queued','running')
 ORDER BY CASE status WHEN 'queued' THEN 0 ELSE 1 END, run_after DESC LIMIT 1;

-- get_last_child
SELECT id, def_id, kind, payload, status, attempt, run_after, started_at, finished_at,
       error, progress, concurrency_key, timeout_secs
  FROM jobs
 WHERE def_id = ?1 AND status IN ('succeeded','failed','cancelled')
 ORDER BY finished_at DESC, run_after DESC LIMIT 1;

-- list_running
SELECT id, def_id, kind, payload, status, attempt, run_after, started_at, finished_at,
       error, progress, concurrency_key, timeout_secs
  FROM jobs WHERE status = 'running';
```

## 3. 基线索引（改动前 `store::migrate` 已有的）

```sql
CREATE UNIQUE INDEX jobs_running_def ON jobs(def_id)
  WHERE def_id IS NOT NULL AND status = 'running';
CREATE UNIQUE INDEX jobs_queued_def ON jobs(def_id)
  WHERE def_id IS NOT NULL AND status = 'queued';
CREATE INDEX jobs_status_run_after_idx ON jobs(status, run_after) WHERE status = 'queued';
CREATE INDEX jobs_payload_idx ON jobs(payload);
CREATE INDEX jobs_finished_idx ON jobs(finished_at) WHERE finished_at IS NOT NULL;
```

`jobs_running_def` / `jobs_queued_def` 是**部分唯一索引**，而 `status IN ('queued','running')`
不蕴含 `status='queued'`，所以 live/last child 用不上它们；这两个唯一约束**必须保留**
（同一 def 至多一个 queued、一个 running，由 `queued_child_is_unique_per_def` 回归）。

## 4. 新增索引（改动后）

```sql
CREATE INDEX jobs_running_status_idx ON jobs(status) WHERE status = 'running';
CREATE INDEX jobs_running_key_idx ON jobs(concurrency_key) WHERE status = 'running';
CREATE INDEX jobs_live_def_idx ON jobs(def_id, status, run_after)
  WHERE status IN ('queued','running');
CREATE INDEX jobs_last_def_idx ON jobs(def_id, finished_at DESC, run_after DESC)
  WHERE status IN ('succeeded','failed','cancelled');
```

## 5. BEFORE：原始 EQP（基线索引 + ANALYZE）

```text
sqlite version: 3.50.2
seed: 200000 jobs in 7.31s
===== BEFORE (baseline indexes only, ANALYZE) =====
--- key_busy (OR, rejected alternative)
SCAN jobs
--- key_busy split probe: concurrency_key
SCAN CONSTANT ROW
SCALAR SUBQUERY 1
SCAN jobs
--- key_busy split probe: def_id
SCAN CONSTANT ROW
SCALAR SUBQUERY 1
SEARCH jobs USING INDEX jobs_running_def (def_id=?)
--- get_live_child
SCAN jobs
USE TEMP B-TREE FOR ORDER BY
--- get_last_child
SCAN jobs
USE TEMP B-TREE FOR ORDER BY
--- list_running
SCAN jobs
```

## 6. AFTER：原始 EQP（新增索引 + ANALYZE）

```text
===== AFTER (shipped indexes + ANALYZE) =====
--- key_busy (OR, rejected alternative)
SEARCH jobs USING INDEX jobs_running_status_idx (status=?)
--- key_busy split probe: concurrency_key
SCAN CONSTANT ROW
SCALAR SUBQUERY 1
SEARCH jobs USING INDEX jobs_running_key_idx (concurrency_key=?)
--- key_busy split probe: def_id
SCAN CONSTANT ROW
SCALAR SUBQUERY 1
SEARCH jobs USING INDEX jobs_running_def (def_id=?)
--- get_live_child
SEARCH jobs USING INDEX jobs_live_def_idx (def_id=? AND status=?)
USE TEMP B-TREE FOR ORDER BY
--- get_last_child
SEARCH jobs USING INDEX jobs_last_def_idx (def_id=?)
--- list_running
SEARCH jobs USING INDEX jobs_running_status_idx (status=?)
```

`get_live_child` 仍需 `USE TEMP B-TREE FOR ORDER BY`：`CASE status WHEN 'queued' THEN 0 ELSE 1 END`
不是索引可排序的表达式；索引已把候选集缩到一个 def 的少量行，排序代价可忽略。

## 7. 实测耗时（代表性分布：200 running / 1 800 queued）

ms，51 次，同一进程同一数据集（一次负载较重的采样）：

| 查询 | before 中位数 | before 最小值 | after 中位数 | after 最小值 |
|---|---:|---:|---:|---:|
| key_busy（OR，**已否决**）hit | 56.690 | 28.221 | 3.202 | 0.154 |
| key_busy（OR，**已否决**）miss | 49.400 | 30.961 | 0.150 | 0.142 |
| key_busy（拆两次探测，**已采用**）hit | 0.750 | 0.494 | 0.040 | 0.023 |
| key_busy（拆两次探测，**已采用**）miss | 54.098 | 30.941 | 0.088 | 0.023 |
| get_live_child | 45.501 | 24.426 | 0.029 | 0.027 |
| get_last_child | 40.680 | 25.137 | 0.048 | 0.019 |
| list_running | 78.780 | 33.804 | 1.698 | 0.218 |

注：
- `key_busy split hit` 的 before 只有 0.75 ms，是因为 `EXISTS` 在没有索引时**扫到第一个
  匹配行就停**；同一查询在 miss（无匹配）时退化成 54 ms 的全表扫描。两种情形都列出来，
  避免只报好看的一面。
- 机器负载波动明显：同一天更安静时的采样里 before 全表扫描约 17–25 ms、after 约
  0.02–0.2 ms。量级差异稳定，绝对值以 `min` 列为准。

## 8. 敏感性检查：running 集合变大（10 000 running / 200 000 行）

用 `JOBS_DENSE_RUNNING=1` 把 running 提到 10 000 行（约 5%），复现同一流程：

| 查询 | before 中位数 | before 最小值 | after 中位数 | after 最小值 |
|---|---:|---:|---:|---:|
| key_busy（OR，已否决）hit | 39.493 | 25.067 | 19.028 | 11.775 |
| key_busy（OR，已否决）miss | 26.261 | 20.626 | 17.911 | 10.697 |
| key_busy（拆两次探测，已采用）hit | 0.022 | 0.022 | 0.024 | 0.024 |
| key_busy（拆两次探测，已采用）miss | 33.474 | 22.835 | 0.023 | 0.023 |
| get_live_child | 38.507 | 24.777 | 0.029 | 0.027 |
| get_last_child | 36.850 | 20.242 | 0.020 | 0.019 |
| list_running | 51.512 | 34.305 | 22.493 | 15.366 |

读法：
- `list_running` 必须返回 10 000 行本身，耗时不随索引下降到亚毫秒——索引只消除了
  全表扫描，没有消除返回行数（51.5/34.3 → 22.5/15.4 ms）。
- OR 形状在 running 集合变大后**线性退化**（after 中位数 19.0 ms / 最小值 11.8 ms，因为它
  要扫完 `jobs_running_status_idx` 里所有 running 条目再逐行判断 OR）；
- 拆两次探测 + `jobs_running_key_idx` 后保持**平坦**（0.022–0.024 ms），与 running 行数无关。
  这是采用拆分的实测依据；代表性分布（200 running）下两者都够快（0.15 vs 0.024 ms），
  拆分在这里不是必需，但它消除了随 running 集合增长的退化。

## 9. 索引取舍

| 索引 | 计划证据 | 实测收益 | 结论 |
|---|---|---|---|
| `jobs_running_status_idx` | `list_running`: `SCAN jobs` → `SEARCH … (status=?)` | 78.8/33.8 → 1.7/0.22 ms | **保留**（`list_running` 唯一可用索引；`jobs_running_def` 因 `def_id IS NOT NULL` 谓词不可用） |
| `jobs_running_key_idx` | 拆分后 concurrency_key 探测：`SCAN jobs` → `SEARCH … (concurrency_key=?)` | miss 情形 54/30.9 → 0.088/0.023 ms；密集 running 下 22.8 → 0.022 ms | **保留**（拆分探测的唯一索引；没有它拆分在 miss 时仍是全表扫描） |
| `jobs_live_def_idx` | `get_live_child`: `SCAN jobs` + TEMP B-TREE → `SEARCH … (def_id=? AND status=?)` | 45.5/24.4 → 0.029/0.027 ms | **保留** |
| `jobs_last_def_idx` | `get_last_child`: `SCAN jobs` + TEMP B-TREE → `SEARCH … (def_id=?)`（TEMP B-TREE 消失） | 40.7/25.1 → 0.048/0.019 ms | **保留** |
| `jobs_running_def`（既有唯一索引） | 拆分后 def 探测：`SEARCH … (def_id=?)` | — | **保留唯一约束**，同时顺带服务 def 探测 |
| `jobs_queued_def`（既有唯一索引） | `status IN (...)` 不蕴含 `status='queued'`，帮不上 live/last child | — | **保留唯一约束**（正确性要求），不作为查询索引 |

被否决的候选：无。四个候选索引各自都有计划变化与实测收益；没有添加无收益索引。

## 10. `key_busy` 拆分：布尔语义等价性

原式在 `status='running' AND id != except` 的行上求
`(?1 IS NOT NULL AND concurrency_key = ?1) OR (?2 IS NOT NULL AND def_id = ?2)`，取 `COUNT(*) > 0`。

拆成两次 `EXISTS` 后：

| key | def_id | 原式 | 拆分后 |
|---|---|---|---|
| NULL | NULL | 两个析取都为 false → 不忙 | 两个 `if let` 都不进入 → 不忙 |
| Some | NULL | 只看 key | 只探测 key |
| NULL | Some | 只看 def_id | 只探测 def_id |
| Some | Some | key 命中或 def_id 命中 | 先探测 key，命中即返回；否则探测 def_id |

- `?1 IS NULL` / `?2 IS NOT NULL` 的短路语义完全一致：NULL 一侧永不参与匹配。
- `id != except` 在两次探测中都保留。
- `COUNT(*) > 0` 与 `EXISTS` 在布尔意义上等价。
- 唯一的可观测差异是**求值顺序**（拆分后命中 key 就不再探测 def_id），但结果是布尔值，
  顺序不可观测。回归 `key_busy_truth_table_via_claim` 覆盖 key/def 为 NULL、命中、
  不匹配四种组合，并经由公开的 `Queue::claim` 断言（不是断言 SQL 文本）。

## 11. 回归测试

`crates/jobs/tests/query_plans.rs`（默认运行，不含 20 万行测量）：

- `live_child_prefers_queued_over_running`：同 def 下 queued 压过 run_after 更大的 running。
- `live_child_orders_same_status_by_run_after_desc`：临时去掉 running 部分唯一索引，
  验证 `ORDER BY … run_after DESC` 兜底键（生产约束下同一 def 不会有两条同状态 live child）。
- `last_child_orders_by_finished_at_then_run_after`：`finished_at DESC, run_after DESC`。
- `key_busy_truth_table_via_claim`：四种布尔组合经 `Queue::claim` 断言。
- `queued_child_is_unique_per_def`：同一 def 只能有一个 queued / 一个 running。

## 12. 未做 / 不能声称

- 未在生产数据库、生产并发或真实磁盘上测量；本文件全部为合成数据 + 本机 SQLite。
- 未测量写放大：新增 4 个索引会增加 INSERT/UPDATE 的维护成本，本文件只量化了读路径收益。
- 未做跨 SQLite 版本的对比；所有数字都来自本仓库 bundled 的 3.50.2。
- 20 万行下的倍数不外推生产；如需要生产结论，应在 release 构建与生产规模数据上重测。
