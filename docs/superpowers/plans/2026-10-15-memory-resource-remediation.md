# 内存与资源修复 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
> 使用本环境可用的等价执行技能。此为待 review 计划，不把条件性容量风险改写为已证永久泄漏。

**Goal:** 让海报许可/单飞随真实工作释放、缓存条目可回收、章节/Probe积压有持久来源和有限内存窗口、前端请求监听及时移除。

**Architecture:** 复用BlockingBudget/LibraryWorkRegistry/持久Probe状态，不另建权威队列。请求timeout只结束等待，真实worker持permit/终态guard；有界派发只缓冲ID，容量满延后而非丢工作。

**Tech Stack:** Rust 2024、Tokio、parking_lot、rusqlite 0.37、受控fake进程、TypeScript/Node（现有最低Node20）。

**Spec:** [修复设计](<../specs/2026-10-15-performance-resource-remediation.md>)；[审查](<../../audits/2026-10-15-database-api-memory-review.md>)；[总依赖](<2026-10-15-performance-resource-remediation.md>)。

## Global Constraints

1. 用 [CONTEXT](<../../../CONTEXT.md>) 词汇；domain 无 IO；store/jobs/media-server 不依赖 api。
2. 保留四 Store 文件及独立 jobs/catalog/fanart 生命周期；不引入跨文件外键或假称跨文件原子事务。
3. 每个改动公开 seam RED→GREEN；每个触及 crate `cargo test -p <crate>`，请求结束稳定树 `cargo test --workspace`；真实失败单独记录，不编造绿色基线。
4. `.rs` 文件≤800 行，生产函数≤120 行，测试函数≤120 行；接近上限的文件先按责任拆分，不调大阈值。
5. 静态图片仍 public；Playback 流/字幕保持 Jellyfin 协议鉴权、即时撤销及 Library 可见性。
6. Store/Queue 不嵌套；Registry→Store 锁序保留；同步 DB/FS/CPU 工作进入有界 blocking；permit 存活到真实 worker 结束，timeout 只结束等待。
7. 所有离锁工作使用 owned 快照；执行副作用前短锁校验来源版本、配置、freeze/删除状态；strict 归属无默认回退。
8. 不访问生产 DB、不启动真实网络/Chromium/ffmpeg 做 cargo 测试；临时库、fake Catalog/ImageFetcher/FileRead/Capture/进程 runner。
9. 不默认删除过期详情 stale cache、不删除 active 工作/模型引用样本/用户观看事实；新增 retention 默认 `enabled=false,dry_run=true`。
10. 不记录 token、完整 URL、payload、观看明细；结构化日志记录容量、耗时、数量和错误类型。
11. SQL 性能测试用 bundled SQLite VM/progress/EQP；时间/RSS只做 release 测量，不以偶发毫秒门禁替代行为与工作量断言。
12. 不创建 GitHub issue；只提交本任务文件，用户未提交内容由其负责人管理。

## Review Focus

1. HTTP取消/10秒超时、worker仍卡住：真实fetch≤4，同key不重复（R1）。
2. TTL到期但key不再访问、双检命中、旧worker与新gate：条目有界、旧guard不误清新代（R2）。
3. 200章/episode批次、慢STRM、freeze/source替换：ffmpeg≤2，任务延后不丢、晚到不写文件（R3）。
4. 恢复100k pending、满队列/worker重启：窗口≤64/16，DB工作不丢且启动幂等（R4）。
5. 复用signal 1k请求、初始已abort/中途abort/抛错：无监听增长，原错误合同不变（R5）。

---

## 文件结构和独立写域

| 单元 | 文件 | 责任 |
|---|---|---|
| 海报worker生命周期 | [service](<../../../crates/api/src/media_posters/service.rs>)；新 `media_posters/inflight.rs` | worker-owned permit/终态与gate身份 |
| 有界负缓存 | 新 `crates/api/src/media_posters/negative_cache.rs` | 注入时钟/TTL/LRU容量，不混磁盘缓存 |
| 章节服务 | 新 `crates/api/src/chapter_frame_service.rs`、`chapter_frame_service/worker.rs` | 2 worker、64 ID窗口、受控进程/lease |
| 章节持久义务 | 新 `crates/store/src/chapter_frame_tasks.rs`、`schema/chapter_frame_tasks.rs` | 唯一请求/source version、恢复与ack |
| Probe派发 | 新 `crates/api/src/probe_manager/dispatch.rs`；新 `crates/store/src/probe_dispatch.rs` | persisted stage→有限ID窗口/恢复，不全装 |
| HTTP signal | 新 `web/lib/request-signal.mjs`、`request-signal.d.mts` | 无浏览器alias依赖的链接/释放，可Node20测试 |

R1→R2共享service串行。R3/R4共享 [ApiState](<../../../crates/api/src/management/state.rs>)、[ProbeManager](<../../../crates/api/src/probe_manager.rs>)、schema写域串行；先与用户现有修改核对。近800行 [chapter handler](<../../../crates/api/src/http/library_chapters.rs>)、ProbeManager先抽责任模块，不机械把新功能追加到文件尾。

### Task R1：海报真实worker持许可与单飞（MEM-01）

**Files:** Modify [service](<../../../crates/api/src/media_posters/service.rs#L76-L164>)；Create inflight模块；Test Create `crates/api/tests/poster_worker_lifetime.rs`，复用公开 `MediaPosterService::with_custom_fetcher` + `MediaPosterSource::get`，不测生产网络。

**Interfaces:** 保留公开get/config seam。新增测试配置builder `pub fn with_wait_deadline(mut self,deadline:Duration)->Self`，默认仍10秒、不接HTTP参数。内部 `PosterInflight { outcome: Mutex<Option<Result<PosterBytes,PosterError>>>, changed: tokio::sync::Notify }`；`PosterWorkerGuard`持 `OwnedSemaphorePermit` 和该inflight；`async fn wait_poster(entry:Arc<PosterInflight>,deadline:Duration)->Result<PosterBytes,PosterError>`。worker正常/Err/panic均发布终态后按Arc身份清理；follower在注册等待前后检查终态避免丢通知。

- [ ] **Step 1：写失败测试。** `cancelling_requests_does_not_release_upstream_slots`、`same_media_after_timeout_joins_running_fetch`。gated ImageFetcher：4不同media工作进入后abort请求，再请求4个；gate未放行时：

```rust
assert_eq!(fetcher.active(), 4);
assert_eq!(fetcher.max_active(), 4);
assert_eq!(same_media_fetches, 1);
assert!(all_worker_resources_released_after_gate);
```

timeout使用可注入等待deadline（生产仍10秒），不靠真实10秒sleep；另覆盖upstream Err/worker panic/磁盘写失败/晚follower终态重放。确保失败测试先放行gate，再assert，避免runtime永不退出。
- [ ] **Step 2：跑RED。** `cargo test -p api --test poster_worker_lifetime -- --nocapture`；当前cancel可观测max4→8，必须行为红，不以只检查Semaphore计数替代真实fetch。
- [ ] **Step 3：实现。** worker acquire_owned后把permit移进blocking闭包或复用BlockingBudget；真实并发4，外等待10秒，取消不撤销entry。网络+cache读写都预算化，不让fast disk spawn无限积压；guard处理终态身份，无guard跨await持Store锁。trace请求超时与worker完结分别记录。
- [ ] **Step 4：跑GREEN。** target + `cargo test -p api`；[现有公开海报回归](<../../../crates/api/tests/media_posters.rs>)照常无Authorization返回图片，8MiB限制/磁盘缓存不变。真实fetch峰值≤4，放行后无permit/worker滞留。
- [ ] **Step 5：提交。** `git commit -m "fix(posters): hold concurrency slots for real worker lifetime"`。

### Task R2：有界TTL负缓存和可清理gate（MEM-02）

**依赖：** R1稳定单飞终态。

**Files:** Create negative_cache模块；Modify [service](<../../../crates/api/src/media_posters/service.rs>)、inflight模块、[cache常量](<../../../crates/api/src/media_posters/cache.rs>)；Test Create `crates/api/tests/poster_cache_capacity.rs`。

**Interfaces:** 内部 `pub trait PosterClock: Send+Sync { fn now(&self)->Duration; }`，SystemPosterClock从服务创建Instant.elapsed；service新增 `pub fn with_negative_cache_clock(mut self,clock:Arc<dyn PosterClock>)->Self`。只读 `PosterResourceCounts { negative_entries:usize,registry_entries:usize,active_workers:usize }` 与 `pub fn resource_counts(&self)->PosterResourceCounts`，用于诊断/容量回归，不返回ID或错误内容。`NegativePosterCache::new(capacity:usize,ttl:Duration,clock:Arc<dyn PosterClock>)->Self`、`get(&mut self,id:MediaId)->Option<PosterError>`、`insert(&mut self,id:MediaId,error:PosterError)`。生产capacity4096/TTL30秒；统计seam只暴露entries/active数，不暴露ID/errors或生产可修改缓存。gate weak registry同4096上限，live满时Busy映射503，不删live强引用；先清dead再准入。

- [ ] **Step 1：写失败测试。** `distinct_negative_keys_are_bounded_and_expire_without_revisit`、`cancelled_waiters_and_disk_double_check_leave_no_dead_gate`：10k不同合法未知UUID/fakeclock推进31秒，再任一访问触发sweep：

```rust
assert!(cache_entries <= 4_096);
assert_eq!(expired_entries_after_sweep, 0);
assert_eq!(dead_gate_entries_after_cleanup, 0);
assert_eq!(active_key_fetches, 1);
```

补4096全live→新key503、同key仍join，旧guard不得remove新代，重复disk命中/取消/错误每条路径；缓存不记录Io/InvalidImage等原本不缓存类，clock回退保护不瞬时全过期。
- [ ] **Step 2：跑RED。** `cargo test -p api --test poster_cache_capacity`，旧实现条目超限/TTL只同key删除失败。
- [ ] **Step 3：实现TTL/LRU。** 每get/insert sweep全过期+容量LRU；数量有限4096，线性sweep的成本有固定上限。weak gates清dead且身份匹配，disk双检早返回RAII归还；不能删除仍有worker/follower的gate从而重新单飞。Busy增加PosterError并在公开handler映射503，静态图片不加鉴权。
- [ ] **Step 4：跑GREEN。** target/R1/现有media_posters；`cargo test -p api`。长稳可补10k distinct heap/RSS，但不以allocator未立即返OS判失败；live allocations/条目数先证明回落。
- [ ] **Step 5：提交。** `git commit -m "fix(posters): bound negative cache and retire inactive gates"`。

### Task R3：章节持久义务、两worker和64窗口（MEM-03）

**依赖：** baseline已协调；与D3/D4迁移和A1模块拆分串行。保留现有WorkControl，不能直接替换成Command.output丢取消回收。

**Files:** Create地图中章节服务/Store任务两个模块；Modify [chapter触发](<../../../crates/api/src/http/library_chapters.rs#L152-L225>)、[marker发布](<../../../crates/api/src/probe_manager/marker_jobs.rs#L86-L94>)、[普通probe消费者](<../../../crates/api/src/probe_manager/probe.rs>)、[ApiState](<../../../crates/api/src/management/state.rs>)、[Store schema](<../../../crates/store/src/schema.rs>)、[API lib](<../../../crates/api/src/lib.rs>)；Test Create `crates/store/tests/chapter_frame_tasks.rs`、`crates/api/tests/chapter_frame_capacity.rs`。

**Interfaces:** `ChapterFrameTask { id:String,ledger_id:LedgerId,source_version:String,targets:Vec<(i64,i64)>,targets_hash:String }`；`Store::enqueue_chapter_frame_task(&self,task:&ChapterFrameTask)->Result<bool,StoreError>`（false表示同version/hash已存在）；`Store::pending_chapter_frame_tasks(&self,limit:usize)->Result<Vec<ChapterFrameTask>,StoreError>`；`Store::finish_chapter_frame_task(&self,id:&str,succeeded:bool,error:Option<&str>)->Result<(),StoreError>`；服务 `async fn dispatch_once(&self)->Result<usize,String>` / `async fn shutdown(&self)->Result<(),String>`。Store任务方法本任务内完成claim queued→running CAS、终态回收/恢复；无runtime只persist，不spawn thread。

- [ ] **Step 1：写失败测试。** `chapter_batch_keeps_process_and_queue_limits`、`chapter_capacity_defers_without_losing_requests`：200文件临时ledger+gated fake runner（不启动真实ffmpeg）：

```rust
assert!(runner.max_active() <= 2);
assert!(service.pending_memory() <= 64);
assert_eq!(durable_pending + running + completed, 200);
assert_eq!(old_version_published_images, 0);
```

补重复version/hash、已存在正确图跳过、source替换/freeze/删除、fake reader取消回收、panic/workererr、queued/running restart、无runtime200义务无新增thread；关闭服务后fakechild/reader/lease归零。结果看文件字节与持久行，不只mock调用数。
- [ ] **Step 2：跑RED。** `cargo test -p store --test chapter_frame_tasks`；`cargo test -p api --test chapter_frame_capacity`；当前扇出峰值在gated seam复现。
- [ ] **Step 3：实现。** durable表UNIQUE(ledger_id,source_version,targets_hash)，queued排序索引；claim/source版本/LibraryWorkRegistry→Store验证。内存64 IDs、2实际worker，permit移真实闭包；full只留DB待派发。完成图片不可变暂存+版本/lease publication gate，失败记录错误/重试，不写已冻结路径；取消kill+wait+reader join；任务结束ack，running crash回queued。
- [ ] **Step 4：跑GREEN。** `cargo test -p store`、`cargo test -p library`、`cargo test -p api`；既有detached章节取消/sidecar保持测试全过，新batch最大进程≤2。真实ffmpeg RSS仅人工release记录Not run/实测，不混cargo。
- [ ] **Step 5：提交。** `git commit -m "fix(chapters): dispatch durable frame work within bounded capacity"`。

### Task R4：Probe持久stage有界ID派发与恢复（MEM-04）

**依赖：** R3 ApiState/近800行文件拆分完成；D2计数合同可独立，但同Probe/schema写域串行。

**Files:** Create dispatch/probe_dispatch模块；Modify [ProbeManager](<../../../crates/api/src/probe_manager.rs>)、[queue](<../../../crates/api/src/probe_manager/queue.rs>)、[worker](<../../../crates/api/src/probe_manager/worker.rs>)、[recovery](<../../../crates/api/src/probe_manager/recovery.rs>)、[scan派发](<../../../crates/api/src/scan_probe_dispatch.rs>)、[probe stage](<../../../crates/store/src/probe_state.rs>)、[probe tasks](<../../../crates/store/src/probe_tasks.rs>)；Test Create `crates/store/tests/probe_dispatch_windows.rs`、`crates/api/tests/probe_backlog_capacity.rs`。

**Interfaces:** `ProbeDispatchStage::{Metadata,Fingerprint}`（派发分类，不替代Store的Metadata/Intro/Outro事实）；`ProbeDispatchId { job_id:String,ledger_id:String,stage:ProbeDispatchStage }`；`ProbeDispatchSnapshot { job:store::ProbeJob,unit:store::ProbeJobUnit,row:LedgerRow,source_version:String }`；Store `pending_probe_dispatch_ids(&self,stage:ProbeDispatchStage,limit:usize)->Result<Vec<ProbeDispatchId>,StoreError>`、`load_probe_dispatch_snapshot(&self,id:&ProbeDispatchId)->Result<Option<ProbeDispatchSnapshot>,StoreError>`，此两个类型定义于store，API消费；stage转换从现有job kind/状态和persisted metadata result派生，不猜内存FingerprintWork。manager `pub async fn shutdown_workers(&self)->Result<(),String>`；try_start_workers沿现签名，AtomicBool保证幂等。

- [ ] **Step 1：写失败测试。** `large_pending_recovery_loads_only_dispatch_windows`、`full_fingerprint_window_does_not_lose_metadata_completion`：100k持久pending，不同ledger，fake capture gate：

```rust
assert!(metadata_pending_ids <= 64);
assert!(fingerprint_pending_ids <= 16);
assert_eq!(all_durable_pending_plus_terminal, original_count);
assert!(same_runtime_second_start_does_not_add_workers);
assert_eq!(resources_after_drain, 0);
```

补queue满scan义务ack可恢复、metadata→fingerprint转换crash、旧job消息不完成新retry、source变化/同路径重用、freeze取消、所有completed但publication待恢复、workererr/DBerr。metadata优先/1声纹策略不改；记录持续metadata导致饥饿，不扩queue伪修。
- [ ] **Step 2：跑RED。** `cargo test -p store --test probe_dispatch_windows`；`cargo test -p api --test probe_backlog_capacity`。使用ID/条目数量的deterministic容量证据，RSS额外测量，不设抖动内存门禁。
- [ ] **Step 3：实现。** bounded channels64/16只携ID；数据库待办定向LIMIT索引，启动recover不逐所有units装LedgerRow/seen；metadata结果先持久、fingerprint派发失败只留下queued并wake，不finish失败/丢工作。active/config仍1..8、fingerprint1；worker取ID后短锁重读/claim/source验证。seen只窗口+active，shutdown停止新派发、排空/取消恢复able工作，join真实workers；无同步guard跨await。backlog数据库增长由D4单独治理。
- [ ] **Step 4：跑GREEN。** `cargo test -p store`、`cargo test -p api`，既有Probe恢复/priority/晚到/原子刷新全过。执行用户未提交测试需负责人基线先处理，不能删测试换green。
- [ ] **Step 5：提交。** `git commit -m "perf(probe): dispatch bounded IDs from durable pending stages"`。

### Task R5：请求完成移除外部abort监听（MEM-05）

**Files:** Create地图signal模块/声明、`web/lib/request-signal.test.mjs`；Modify [HTTP封装](<../../../web/lib/http.ts#L91-L120>)。不改React hook行为，不引入测试框架。

**Interfaces:** `.mjs`导出 `linkRequestSignal(externalSignal,timeoutMs=15000)`；`.d.mts`声明 `RequestSignalLease { signal:AbortSignal; release():void; isLocalTimeout():boolean }` 和 `linkRequestSignal(externalSignal:AbortSignal|null|undefined,timeoutMs?:number):RequestSignalLease`。使用具名onAbort，release幂等；external存在不增加本地timeout；初始aborted直接abort且不保留监听。

- [ ] **Step 1：写失败测试。** Node标准`node:test`/`node:assert`/`getEventListeners`，1k同signal完成的link→release，以及fake fetch resolve/reject/cancel契约：

```js
assert.equal(getEventListeners(external.signal, 'abort').length, 0);
assert.equal(lease.signal.aborted, true); // 已取消或进行中取消
assert.equal(externalLease.isLocalTimeout(), false);
```

另测试local timeout分类、release两次、setup同步throw仍finally释放。先在当前行为提取表征证明泄漏，再实现，不只对新工具自说自话。
- [ ] **Step 2：跑RED。** `node --test web/lib/request-signal.test.mjs`，旧adapter重复链接监听增长。Node20支持纯mjs，不能假定原始TS自动可执行/alias可解析。
- [ ] **Step 3：实现utility接入request。** request用lease.signal，finally release；保留15,000ms无external默认、external主动取消原错误透传、HttpError本地timeout分类、401重定向和headers。为了可测，只抽abort生命周期，别把整个HTTP模块/React架构重写。
- [ ] **Step 4：跑GREEN。** `node --test web/lib/request-signal.test.mjs`；`pnpm -C web test`、`pnpm -C web typecheck`、`pnpm -C web build`。全test脚本若原有TS/运行器问题，记录真实环境并用该mjs target验证，不修改全项目工具链掩盖。
- [ ] **Step 5：提交。** `git commit -m "fix(web): release linked abort listeners after requests"`。

## 自审

R1–R5一一覆盖MEM-01–05。R3/R4持久义务是派生工作状态，不新造Media/Library事实；R4的派发分类与ProbeStage三种事实区分；R2与R1以entry身份交接，取消请求不提前回收工作；R5能在最低Node20验证。所有Review Focus有对应test；无界→bounded不靠丢弃任务达标。最终长稳/线程/子进程/RSS和用户设备证据见总计划X1。
