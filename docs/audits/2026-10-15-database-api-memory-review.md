# 数据库、接口性能与内存资源审查（2026-10-15）

## 1. 范围与结论

用户要求三个独立 agent 分别审查 SQLite、HTTP 接口扩展性、内存与资源生命周期；Lead 复核重点调用链并执行隔离实验。审查未改应用源码、未写生产数据库、未创建 GitHub issue。

- 初次审查包含当时未提交的 Probe/marker 修改；文档落盘时 HEAD 为 `2c649ed`（文档清理提交）。**不是纯 HEAD 审计，也不是生产实例测量。**
- 行号为本次阅读位置；源码还有用户未提交修改，尤其 Probe recovery、marker publication 和临时测试。实施前重新读当前文件，不能覆盖或提交用户改动。
- P1：优先处理的稳定性/规模增长风险；P2：明确优化/容量治理项；P3：次要问题或独立设计门。优先级不是生产事故声明。
- 已确认指可达控制流、SQL 或隔离实验成立；没有生产 p95、长期 RSS/堆采样或真实 NAS/设备性能数据。

**结论：** 没有证明需要更换 SQLite/四 Store 生命周期分区。主要问题是重复读取、归属判断、分页时机、全局锁内文件 IO，以及真实后台生命周期与容量。现有优化必须保留，不照旧计划重做已落地功能。

## 2. 发现与修复覆盖

| ID | 优先级 | 发现 | 证据 | 计划任务 |
|---|---|---|---|---|
| DB-01 | P2 | catalog 前缀查询未有效使用完整复合键 | 代码+合成 EQP/VM | D1 |
| DB-02 | P2 | 探测义务排序缺索引，LIMIT 不限制扫描 | 代码+合成 EQP/VM | D1 |
| DB-03 | P2，慢挂载放大 | Subscribe 事实加载持锁做文件 IO/重复严格归属 | 静态调用链 | A1 |
| DB-04 | P2 | 全局 revision 跨库失效、周期全标脏、整代重写 | 静态调用链 | D3 |
| DB-05 | P2，长期 | QoE/catalog/Probe 历史容量治理不足 | 静态容量分析 | D4 |
| DB-06 | P3 | Probe 每单元四 COUNT，整轮平方增长 | 代码+合成 VM | D2 |
| DB-07 | P3，已知限制 | 默认 Library 未归属补集无法 seek | 现行明确披露 | D5（设计门） |
| API-01 | P1 | Jellyfin 多轮逐行读取，NextUp 按 Media 重扫 | 静态调用链 | A2、A3 |
| API-02 | P1，慢 NAS 条件 | 收藏/episode/路径核对持锁做文件 IO | 静态锁作用域 | A1 |
| API-03 | P2 | Library 列表逐库重扫/逐文件统计 | 静态链+FE 消费 | A5 |
| API-04 | P2 | 热投影全目标库装载、排序后分页 | 静态链+测试边界 | A4 |
| API-05 | P2 | up-next 电影误调 TV，串行等非必要外部数据 | 可达错误调用 | A6 |
| MEM-01 | P2 | 海报取消/超时提前归还许可 | 代码+真实服务/fake 实验 | R1 |
| MEM-02 | P2 | 海报负缓存/单飞 Map 缺完整淘汰 | 静态增长/释放路径 | R2 |
| MEM-03 | P2 | 章节抓帧无界派生任务/ffmpeg | 静态扇出/回收 | R3 |
| MEM-04 | P2，负载相关 | Probe 双无界队列/启动全量回填 | 静态容量分析 | R4 |
| MEM-05 | P3 | 复用 AbortSignal 积累监听 | 注册/清理对照 | R5 |

## 3. 数据库

### DB-01 · 返回定向，但 SQLite 工作量不定向

**证据：** [catalog_details_for_prefixes](<../../crates/store/src/browse_projection.rs#L341-L365>)；消费者 [load_catalog_rows](<../../crates/api/src/browse_projection/builder.rs#L130-L149>) 持 Store 锁。

`source=? AND (cache_key=? OR cache_key LIKE ?)` 的默认 LIKE 与 BINARY 主键不匹配，OR 未实现完整键 seek。每批至多 400 前缀，成本可随缓存行数×前缀数增长。不是旧的“全部 body 返回”，而是数据库内部无关工作仍多。

Lead 用相同 DDL、ANALYZE、单来源内存 Python SQLite 独立复验，始终返回 1 行：

| 无关行数 | 当前 VM | exact + `[prefix+'?',prefix+'@')` VM |
|---:|---:|---:|
| 1,000 | 8,056 | 93 |
| 10,000 | 80,056 | 93 |
| 50,000 | 400,054 | 93 |

50k 原计划 SCAN+临时 ORDER；替代为 MULTI-INDEX OR+复合主键范围。agent 独立 fixture 同趋势；少量 VM 差异来自 fixture。**合成结果不等于 bundled SQLite/生产基准或收益承诺。**

修复保留 exact、语言变体、`fetched_at DESC,rowid ASC` tie；`/movie/42/images` 不属于详情。不得全局修改 LIKE 语义。

### DB-02 · 探测义务缺排序索引

**证据：** [查询](<../../crates/store/src/scan_probe_obligations.rs#L63-L83>)、[DDL](<../../crates/store/src/schema/tables.rs#L213-L217>)、[调度锁](<../../crates/api/src/scan_probe_dispatch.rs#L16-L22>)。

每 5 秒 `ORDER BY created_at_ms,ledger_id LIMIT 100`，仅 ledger_id 主键；大量扫描积压下每轮重扫全队列。

| 积压 | 原 VM | 新增 `(created_at_ms,ledger_id)` 索引后 VM |
|---:|---:|---:|
| 1,000 | 7,611 | 709 |
| 10,000 | 70,611 | 709 |
| 50,000 | 350,611 | 709 |

索引 EQP 仍可能写 SCAN USING INDEX；不能凭 SCAN 单词误判，应验证 LIMIT 早停和 VM 增长。

### DB-03 · Subscribe 事实加载持锁访问文件系统

**证据：** [library_subscribe_facts](<../../crates/store/src/subscribe_library.rs#L42-L90>)、[路径解析](<../../crates/store/src/library_paths.rs#L5-L30>)；调用者 [finish_search](<../../crates/api/src/worker/finish_search.rs>)、[Transfer worker](<../../crates/api/src/worker/transfer.rs>)。

逐事实/ledger 重读配置、逐组件 symlink_metadata/canonicalize，另有 is_file。N 个 episode×R 根重复工作，慢挂载阻塞其他 Store 消费者。与 API-02 同根因、不同消费者。

保持 strict 归属，不能换默认 Library 回退；用 [LibraryPathScope](<../../crates/store/src/library_scope.rs#L40-L78>) 离锁解析，在副作用边界重验配置、来源、freeze/删除状态。

### DB-04 · 粗失效与整代写入放大

**证据：** [revision triggers](<../../crates/store/src/schema/read_revisions.rs#L21-L35>)、[公共比较](<../../crates/api/src/browse_projection.rs#L493-L518>)、[60 秒标脏](<../../crates/api/src/browse_projection.rs#L441-L451>)、[整代发布](<../../crates/store/src/browse_projection.rs#L133-L172>)。

无关搜索缓存、其他库的 ledger/file_meta/marker 写会让目标投影 stale；周期全部 visited 标脏。发布全量 DELETE/INSERT，持锁序列化。已有单飞/容量只限制并发，不消除无关重建。

这是保守失效性能债，不报数据一致性漏洞。细化依赖必须处理跨文件无事务、崩溃、未知写路径保守失效，不简单移除 revision 或外部文件刷新。

### DB-05 · TTL/VACUUM 不等于清理存活历史

**证据：** [维护类别](<../../crates/api/src/maintenance/policy.rs#L64-L69>)、[QoE 插入](<../../crates/store/src/playback/logs.rs#L141-L176>)、[fresh/stale](<../../crates/media/src/cache.rs#L142-L162>)、[fingerprint 历史](<../../crates/store/src/schema/fingerprint.rs#L8-L68>)。

jobs/playback_logs 已有 retention，不能报“完全没有清理”。QoE、不同 catalog 查询键、Probe/attempt 历史仍增长。过期仅变 stale，VACUUM 不删除存活行。

按类别制定默认关闭/预览、分批停止策略；保护 active 工作、模型样本引用与详情离线 stale。**不授权自动删除生产数据。**

### DB-06 · Probe 完成计数累计 O(N²)

**证据：** [finish_probe_unit_with_status](<../../crates/store/src/probe_tasks.rs#L269-L342>)。

每单元完成四次 COUNT，已有 job/status 索引但成本仍随匹配数增长。agent 合成末次 VM：N=100/1k/5k 约 825/7,125/35,125；整轮平方增长。通常小于声纹计算成本，P3。

仅在一次有效状态转移后事务增量计数；partial/failed/cancelled、重复回调、旧 retry 和 job 取消保持等价，启动恢复一次对账。

### DB-07 · 默认 Library 补集候选（已知设计限制）

**证据：** [现行披露](<../../crates/store/src/browse_reads.rs#L208-L215>)。

默认库包含没有匹配任何同 kind 根的历史回退集合，NOT 补集不可像非默认库范围 seek。需派生归属索引/规范事实、backfill、配置改变/恢复设计，不是普通补索引。先 D5 设计门，不把未填充 root_id 当权威事实。

## 4. 接口

### API-01 · Jellyfin/NextUp 乘法放大

**证据：** [同步 provider](<../../crates/api/src/media_server_provider.rs#L246-L252>)、[全量收集](<../../crates/api/src/media_server_provider/items.rs#L120-L201>)、[NextUp](<../../crates/media-server/src/routes/shows.rs#L36-L64>)、[分页](<../../crates/media-server/src/routes/catalog.rs#L140-L164>)。

根列表至少三轮 get_media、多轮归属/可见性；归属反复读 Library/roots。指定 parent 的前两轮仍全实例。stat/NFO/图片/字幕发现先于分页。NextUp 无 SeriesId 时每 TV Media 再跑同链，约 O(S×N×L)，limit=1 也不限制前置工作。

文件 IO 已离 Store 锁，但同步 provider 未进入 blocking 预算、仍占 async worker。先批量 owned 快照/有界 blocking，再下推范围、选择、分页和一次 NextUp 聚合；SQL 次数少不等于只读本页。

### API-02 · Store 锁内 NAS IO 扩大故障半径

**证据：** [收藏图集](<../../crates/api/src/http/playback_views.rs#L617-L741>)、[episode handler](<../../crates/api/src/http/playback.rs#L201-L224>)、[探图](<../../crates/api/src/http/playback/episodes.rs#L84-L90>)、[路径核对](<../../crates/api/src/http/library_housekeeping.rs#L25-L79>)。

收藏/episode 探图 guard 未释放。路径核对逐失联文件重复递归根，深度≤5、宽度不限，最坏 O(失联数×目录项)。慢 FS 调用可阻塞认证/heartbeat。DB-03 后台同问题。

核对实际为 GET，handler 注释 POST，执行以 [live router](<../../crates/api/src/http/mod.rs>) 为准。离锁预览不取消后续关联/删除的最终安全校验。

### API-03 · Library 列表逐库重扫与实时 stat

**证据：** [handler](<../../crates/api/src/http/library.rs#L182-L237>)、[rows_in_library](<../../crates/api/src/http/library/assets.rs#L8-L19>)、[统计](<../../crates/api/src/http/library/assets.rs#L89-L130>)。

每可见库重读全 ledger、逐行 Media/配置，再逐文件 stat；文件阶段已离锁，但 DB 仍重复，约 O(V×N×L)。前端有每 30 秒轮询，部分单库读取也消费全列表。

一次快照/batch 按归属聚合，统计复用投影/持久事实；明确统计刷新时间和缺失根检查，不能把旧统计当实时。

### API-04 · REST 热页仍全目标库装载

**证据：** [整库投影](<../../crates/store/src/browse_projection.rs#L225-L265>)、[typed 快照](<../../crates/api/src/http/library/selection/projected.rs#L66-L151>)、[最终切页](<../../crates/api/src/http/library.rs#L262-L283>)。

每页全部 files/media JSON、groups、相关用户 units，过滤排序后分页，约 O(F+M logM+U)、内存 O(F+M+U)。limit 只限响应；省略 limit 仍全返回。

[browse_title_page](<../../crates/store/src/browse_reads.rs#L135-L169>) 仅定义未接入。先常用简单排序 SQL 分页，再高级过滤轻量候选；不得分页后过滤。Gallery 已仅展开本页图片 JSON，不等于上游只读本页。

### API-05 · up-next 电影错调 TV + 串行外部等待

**证据：** [movie/Video plan](<../../crates/api/src/http/playback_views.rs#L83-L97>)、[season 预取](<../../crates/api/src/http/playback_views.rs#L229-L250>)、[TV URL](<../../crates/media/src/client.rs#L340-L350>)。

movie 带 season=0，循环不区分 kind，调用 `/tv/{movie_id}/season/0`；失败不形成成功缓存，轮询可重复。TV 冷/过期同样串行；单 HTTP 有 timeout/stale，但列表无总体等待预算。

本地 NFO/已缓存名称优先，缺名称先返回可播放卡片，后台有界补齐；保持排序、进度、整季成功缓存。

## 5. 内存与资源

### MEM-01 · 海报许可寿命短于真实工作（实测）

**证据：** [get](<../../crates/api/src/media_posters/service.rs#L112-L150>)、[公开路由](<../../crates/api/src/http/media_posters.rs#L10-L19>)。

permit 存在 async get 栈，blocking 闭包未持有。10 秒 timeout/取消归还名额，但 catalog/fetch/写盘继续；多个网络阶段或慢盘可超过外层预算。

Lead 仓库外临时 harness 调真实服务、gated fake ImageFetcher、临时 SQLite/缓存，无真实网络；依次启动并取消 8 个不同 Media：

```text
configured_upstream_limit=4 observed_simultaneous_fetches=8 all_eight_started=true
```

放行后 worker 结束、实验 exit 0。这是限流绕过/滞留任务叠加，不是已证永远不释放的堆泄漏。permit/单飞终态须由 worker 持有至真正结束。

### MEM-02 · 负缓存和 gate Map 无界逻辑持有

**证据：** [negative cache](<../../crates/api/src/media_posters/service.rs#L55-L69>)、[gate/提前返回](<../../crates/api/src/media_posters/service.rs#L91-L155>)。

30 秒 TTL 仅同 ID 再访问才 remove；不同失败 ID 长期保留，公开合法随机 UUID 也能创建 NotFound。gate 正常尾部清理被取消/等待/双检磁盘命中绕过。

需容量、全局过期淘汰、正确共享 gate 生命周期，不能旧 worker 删新 gate。静态图保持 public，不以鉴权替代治理。

### MEM-03 · 章节抓帧无界扇出

**证据：** [触发](<../../crates/api/src/http/library_chapters.rs#L152-L225>)、[marker 消费者](<../../crates/api/src/probe_manager/marker_jobs.rs#L86-L94>)、[进程控制](<../../crates/library/src/work_control.rs>)。

每 ledger 独立派生 blocking/线程、ffmpeg 和 pipe reader；无独立并发/排队预算或同版本单飞。首次整季无图/慢 STRM 时资源随文件数放大。已有图片跳过、取消、kill/wait/join，属于峰值/积压，不报永久句柄泄漏。

### MEM-04 · Probe backlog 全量复制进无界内存队列

**证据：** [双 unbounded](<../../crates/api/src/probe_manager.rs#L60-L124>)、[恢复入队](<../../crates/api/src/probe_manager/recovery.rs#L284-L370>)。

每批派发 100 不能限制双队列/seen 的总积压。声纹一个 worker 且等 metadata idle；持续生产或重启恢复 RSS 随 backlog 增长。有限库停止生产后可能排空，不等于经典泄漏。

数据库保存完整积压、内存只取有限 ID 窗口；满容量不丢持久化任务/不标完成，不在 Store/seen 锁内 await。

### MEM-05 · 外部 AbortSignal 监听保留

**证据：** [HTTP request](<../../web/lib/http.ts#L91-L120>)、[复用 signal 的轮询](<../../web/lib/use-library-removal.ts#L36-L61>)。

匿名 once listener 在请求完成时未移除；signal 未 abort 时 once 不自清，轮询保留已完成 controller，直到取消/GC。具名 listener + finally remove，保持初始取消/进行中取消语义。

## 6. 排除项与已有优化

- Store/Queue/catalog 已有 WAL、5,000ms busy_timeout、foreign_keys，不重复旧配置问题。
- Media 身份索引、jobs claim/latest 索引、keyset retention 已落地。
- REST 投影、batch、单飞、blocking 预算有效；热页零现场 NFO/stat 的现有测试通过。
- Jellyfin 文件 IO 已离 Store 锁；API-01 指 async worker/重复 DB 工作，不误报旧锁问题。
- BlockingBudget/字幕 permit 已持有到真实工作结束，可复用。
- 未证实生产 Arc 循环堆泄漏；单实例进程级保活非泄漏证明。Probe 重复 start/PCM 短时缓冲仅保留验证条件。
- 不迁数据库、不删 Library/Playback 用户事实、不公开 Playback 流、不改默认省略 limit 行为。

## 7. 实际验证与限制

### 7.1 定向回归：36 PASS、1 ignored

真实执行两批：

```bash
cargo test -p api --test browse_projection_hot_path --test browse_projection_budget --test browse_projection_paging --test subscription_latency
cargo test -p jobs --test query_plans
cargo test -p api --test media_server_list_off_lock --test media_posters
cargo test -p media-server --test jellyfin_scope
```

1 ignored 是测量 harness。小 fixture 分页仅证明返回条数；off-lock 测试仅证明不持 Store 锁，均不能外推大库/async 工作量有界。

### 7.2 全仓未通过

`cargo test --workspace` 在 API lib 停止：136 PASS、1 FAIL、exit 101。失败是当时已有未提交 `tmp_repro_cross_library_season_refresh`；[marker None 后 unwrap](<../../crates/api/src/probe_manager/marker_refresh_lifecycle_tests.rs#L754-L767>) panic。单独复跑仍失败：

```bash
cargo test -p api --lib probe_manager::tests::marker_refresh_lifecycle::tmp_repro_cross_library_season_refresh -- --exact --nocapture
```

临时用例直接 finish 两个单元再读 marker；本报告不判定是测试预期还是跨 Library 业务缺陷。未改此测试，未宣称全仓通过。实施前由该改动负责人核对；后续新运行不得擦除本次失败记录。

### 7.3 文档阶段重新验证（与最初审查分开）

本轮保存计划期间，外部并行工作更新了用户的Probe/marker未提交修改，临时跨库用例已变为 `window6_cross_library_season_refresh_publishes_only_job_members`。Lead未修改这些源码，仅重新执行 `cargo test --workspace`：**完整命令完成，exit 0**，API lib为137 PASS，并继续完成其他crate与doc-tests。

这次绿色工作树包含用户尚未提交的修改，不是本次文档提交实现了任何D/A/R任务，也不抹去7.2原失败。本轮17项性能/容量发现不能因功能测试通过直接关闭；执行前仍要固定实际基线。

### 7.4 未运行

生产规模盘点、release 前后接口矩阵、heap/RSS 长稳、慢 NAS 故障注入、真实 ffmpeg 大批量、VidHub/Infuse 设备验证均未做。计划目标是验收门，不是结果。

### 7.5 X1 closure evidence (2026-10-17)

Additive status evidence only; original experiments above are unchanged. No X1 commit SHA was observed (uncommitted working tree). See [acceptance matrix](<../benchmarks/2026-10-15-performance-resource-acceptance.md>) for the complete per-ID matrix.

| ID | Status | Closure evidence |
|---|---|---|
| DB-01 | partial | Prior synthetic VM evidence retained; no new bundled SQLite release measurement. |
| DB-02 | partial | Prior synthetic VM evidence retained; no new release measurement. |
| DB-03 | open | No slow-mount/lock regression exercised. |
| DB-04 | open | No cross-library invalidation/crash regression exercised. |
| DB-05 | open | No production retention run or cleanup authorization. |
| DB-06 | partial | Prior synthetic VM evidence retained; no new release measurement. |
| DB-07 | design-gated | Default-library ownership remains an unapproved design gate. |
| API-01 | partial | Prior scoped regression coverage exists; no new release-scale sample. |
| API-02 | open | Temp fixture auth characterization only; slow-I/O and heartbeat availability not established. |
| API-03 | partial | Existing scale regression not rerun in X1. |
| API-04 | partial | Existing page-work regression not rerun in X1. |
| API-05 | partial | Existing up-next regression not rerun in X1. |
| MEM-01 | partial | Prior fake-fetcher evidence retained; not rerun in X1. |
| MEM-02 | open | No new cache-capacity/eviction evidence. |
| MEM-03 | open | R3 pending; task drain assertion ignored without public lifecycle seam. |
| MEM-04 | open | R4 pending; queue recovery/drain assertion ignored without public seam. |
| MEM-05 | partial | `pnpm -C web test` (791 passed) and `pnpm -C web typecheck` (exit 0) passed. |

## 8. 文档与授权边界

- [修复设计与硬约束](<../superpowers/specs/2026-10-15-performance-resource-remediation.md>)。
- [总执行计划](<../superpowers/plans/2026-10-15-performance-resource-remediation.md>)。
- [数据库计划](<../superpowers/plans/2026-10-15-database-remediation.md>)。
- [接口计划](<../superpowers/plans/2026-10-15-api-performance-remediation.md>)。
- [内存资源计划](<../superpowers/plans/2026-10-15-memory-resource-remediation.md>)。

新计划不自动关闭 [旧收敛计划](<../superpowers/plans/2026-10-13-database-api-followup.md>) 的验收/T10，不重跑旧 unchecked 实现步骤。用户本轮只授权保存文档，**未授权修复执行或启用清理**。
