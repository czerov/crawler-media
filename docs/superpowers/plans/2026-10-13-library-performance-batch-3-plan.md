# 第三批 有界接口与数据库性能 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
> 环境映射同前两批：使用已安装`executing-plans`或可用subagent工具；不调用不存在技能、不创建Agent Teams。

**Goal:** 让热浏览与小页请求不随无关全实例数据/文件IO线性增长，扫描和历史维护有真实预算、恢复与并发边界。

**Architecture:** 可重建Library浏览generation承接锁外文件事实；当前权限与ledger身份仍以Store权威事实为准。扫描按operation/chunk持久执行，probe消费第二批义务；Jobs原始窗口游标与事务内计数替代全历史重复扫描。

**Tech Stack:** Rust 2024、Axum/Tokio、rusqlite bundled、现有React/TypeScript、离线Release benchmark。

**Spec:** [第三批设计](<2026-10-13-library-performance-batch-3-design.md>)，前置 [第二批计划](<2026-10-13-library-correctness-batch-2-plan.md>) 全绿；总顺序见 [路线图](<2026-10-13-database-api-scan-master-plan.md>)。

## Global Constraints

- 不改变前两批的用户事实保留、strict owner、lease.publish、PlaySessionId、watch累计和probe obligation合同。
- library8→9，app5→6、catalog1→2、subscribe4→5；同一库只在所属任务注册一次本批迁移，新模块可在后任务加入幂等DDL，最终版本和表必须由迁移测试统一验收。
- 热浏览NFO/stat/read_dir为0；不是保证所有复杂筛选O(page)。无limit原合同不无声截断。
- scan chunk≤100、轮visited≤5000或10秒、全局≤2/每库≤1；Jobs window≤500、轮≤20批或2秒。
- HTTP8/Search4/fs2独立预算；WebSocket发送5秒；STRM64KiB；外部文件facts刷新60秒。
- .rs≤800、函数≤120；TDD公共seam、离线fake、Barrier/Notify；每提交对应crate独立测试，整批workspace；当前分支、小提交、排除无关文件。

## Review Focus

- projection冷启动/dirty/旧scope不能泄露新隐藏或已删除Library，stale只用于合法文件facts（T1–T3）。
- gallery剔空必须发生分页前，复杂filter/排序/legacy观看语义不漂移（T3）。
- 多库revision不原子，构建期间root/rebind变化必须拒绝发布旧代，不漏失效源（T1/T2）。
- protected-only窗口不能被deleted=0终止，游标重启不能漏迟到低key历史（T7）。
- 可取消不是OS IO可中断；背压、超时与副作用不确定性必须诚实报告（T5/T6/T9）。

---

## 文件结构与接口顺序

顺序 `T0基线 → T1投影Store → T2投影构建 → T3浏览接入 → T4可见性/锁 → T5扫描operation → T6扫描walker → T7retention → T8SSE → T9并发/边界 → T10验收`。每个编号独立测试与提交；T5提交可运行旧reported walker的有界准入版本，T6再替换遍历实现，不提交不能运行的队列脚手架。

新模块：
- Store `browse_projection/{mod,reads,writes}.rs`、`schema/browse_projection.rs`、`schema/read_revisions.rs`、`scan_operations.rs`、`schema/scan_operations.rs`。
- API `browse_projection/{mod,builder,invalidation}.rs`、`visible_scope.rs`、`scan_operations/{mod,dispatcher,runner}.rs`、HTTP `library_scan/operations.rs`。
- library `watch/walker.rs`；jobs `retention/cursor.rs`、`change_state.rs`、`store/claim.rs`。
- SQL fixture/HTTP fixtures放各crate tests/support，不能把大测试塞回现存超限文件。

## Task 0: 固定基线与语义金样

**Files:** Modify [性能矩阵](<../../../crates/api/tests/endpoint_perf.rs>)；Create `crates/api/tests/browse_semantic_parity.rs`；Create audit输出目录`docs/audits/2026-10-13-batched-performance/`。
**Interfaces:** 沿用现有router/fixture；新增矩阵环境开关`PERF_ENDPOINTS`选择已知endpoint，错误值直接失败；新增libraries/stale/cold目标，但不改原defaults以免历史复现漂移。

- [ ] **Step 1: 写基线语义fixture。** title/year/rating/added_at/size/random/watch-tier、favorite继承、NFO优先、latest多language catalog、gallery空组，分别保存排序后的MediaId与字段，不把随机UUID写死；两个实现对比用同fixture映射编号。
- [ ] **Step 2: 运行基线。** `PERF_SIZES=100,1000,10000 PERF_MODES=movie,tv PERF_SAMPLES=30 cargo test --release -p api --test endpoint_perf -- --ignored --nocapture --test-threads=1`；保存stdout/stderr、OS/CPU/Rust/SQLite版本、commit、冷热策略；失败状态码不得进入“成功基线”。
- [ ] **Step 3: 验证与提交。** `cargo test -p api`；`git commit -m "test(perf): pin endpoint baseline and browse semantics"`。

## Task 1: 可重建浏览generation与revision

**Files:** Create Store browse_projection与schema模块；Modify [schema](<../../../crates/store/src/schema.rs>)、[Store导出](<../../../crates/store/src/lib.rs>)、B1 ledger_cleanup；Test `crates/store/tests/browse_projection.rs`、`crates/store/tests/read_revisions.rs`。

**Interfaces:**
- `ReadRevisions { app: i64, library: i64, catalog: i64, subscribe: i64 }`；`Store::read_revisions(&self) -> Result<ReadRevisions, StoreError>`，只读各库单行。
- `BrowseFileFacts { ledger_id: LedgerId, media_id: MediaId, facts: serde_json::Value }`、`BrowseMediaFacts { media_id: MediaId, title: String, year: Option<u16>, facts: serde_json::Value }`。
- `ProjectionBuild { library_id: String, generation: i64, scope_signature: String, revisions: ReadRevisions, files: Vec<BrowseFileFacts>, media: Vec<BrowseMediaFacts> }`。
- `Store::publish_browse_projection(&self, build: &ProjectionBuild) -> Result<bool, StoreError>`：library.db事务写并校验当前library revision；其他库revision与B1scope由API锁内复核，false不发布。
- `Store::browse_projection(&self, library: &str) -> Result<Option<ProjectionSnapshot>, StoreError>`；Snapshot含generation/signature/facts/revisions/refreshed_at_ms；`Store::invalidate_browse_projection(&self, library: Option<&str>) -> Result<(), StoreError>`。
- `Store::catalog_details_batch(&self, keys: &[(String,String)]) -> Result<Vec<CatalogCacheRow>, StoreError>`，kind/id精确路径边界，分500键读取，fetched_at desc/cache_key稳定并列。

- [ ] **Step 1: 写事务和失效红测试。** publication中途trigger失败旧generation仍可读；更改ledger/library roots/Media/catalog/units分别revision变，projection自身写不变；raw SQL DML也变；清理ledger后对应投影不可返回旧ID；同Movie/TV numeric ID不串catalog。
- [ ] **Step 2: 红验证。** `cargo test -p store --test browse_projection --test read_revisions`。
- [ ] **Step 3: 实现DDL、revision触发器、批量键读取。** library9、app6、catalog2、subscribe5一次注册；触发范围列表写在schema/read_revisions模块注释并测试所有表，不能仅在API手工发invalidations。投影可重建，移除不影响ledger/Playback。
- [ ] **Step 4: 绿验证。** `cargo test -p store`；迁移幂等/高版本拒绝/旧schema facts保留。
- [ ] **Step 5: 提交。** `git commit -m "feat(store): add rebuildable scoped browse generations"`。

## Task 2: 锁外构建、single-flight与文件失效

**Files:** Create API browse_projection模块；Modify [ApiState](<../../../crates/api/src/management/state.rs>)、[API注册](<../../../crates/api/src/lib.rs>)、[Scrape](<../../../crates/api/src/scrape_metadata.rs>)、[poster发布](<../../../crates/api/src/poster_fetch.rs>)、B1watch presence/B2scan dispatcher；Test `crates/api/tests/browse_projection_refresh.rs`。

**Interfaces:**
- `ProjectionService::new() -> Self`，ApiState共享`Arc<ProjectionService>`。
- `async fn ensure(&self, state: ApiState, library_id: String) -> Result<ProjectionSnapshot, ProjectionError>`；enum Busy/Timeout/Cancelled/StaleScope/Failed(String)。
- `fn mark_dirty(&self, library_id: &str)`；`async fn refresh_once(&self, state: ApiState, library_id: String) -> Result<(), ProjectionError>`。
- builder`fn build(state: &ApiState, library_id: &str) -> Result<ProjectionBuild, ProjectionError>`：锁内owned事实、锁外B1scope/NFO/stat、锁内revision+scope复核/lease.publish；文件事实含全部设计字段，缓存版本化JSON有`version:1`并拒未知版本重建。

- [ ] **Step 1: 写红测试。** gated BrowseFileRead卡住期间auth/me可响应且Store可取；20并发同Library只一个build、不同Library不串facts；root/rebind/冻结在构建中改变旧generation不能发布；NFO变化/海报删除/STRM URL更新/clear catalog各自dirty；投影发布本身不自激重建。
- [ ] **Step 2: 红验证。** `cargo test -p api --test browse_projection_refresh`。
- [ ] **Step 3: 实现builder与60秒已访问库刷新。** 对目标Media定向catalog batch；与当前selection metadata相同优先级，复用/抽出纯解析，不复制不同规则。文件发布成功mark_dirty；失败不谎报ready。启动不强制扫描所有磁盘，首次请求可等待30秒，blocked scope不能stale授权。
- [ ] **Step 4: 绿验证。** `cargo test -p store`、`cargo test -p api`；所有第一批发布安全测试保留。
- [ ] **Step 5: 提交。** `git commit -m "feat(browse): build scoped snapshots off lock with invalidation"`。

## Task 3: items/gallery/facets/search接入投影与页面渲染

**Files:** Modify [条目](<../../../crates/api/src/http/library.rs>)、[gallery](<../../../crates/api/src/http/library_gallery.rs>)、[selection snapshot](<../../../crates/api/src/http/library/selection/snapshot.rs>)、[filters](<../../../crates/api/src/http/library/selection/filters.rs>)、[facets](<../../../crates/api/src/http/library/facets.rs>)、[search](<../../../crates/api/src/http/search.rs>)、[Store reads](<../../../crates/store/src/browse_reads.rs>)；Test `crates/api/tests/browse_projection_paging.rs`，T0 parity继续使用。

**Interfaces:**
- Store `playback_units_for_media(&self, user: UserId, ids: &[MediaId]) -> Result<Vec<PlaybackUnitSnapshot>, StoreError>`，分500、空ids空结果。
- API `fn select_projection(snapshot: ProjectionSnapshot, units: &[PlaybackUnitSnapshot], query: &HashMap<String,String>) -> Result<Vec<LibrarySelection>, Response>`；LibrarySelection文件facts来自snapshot，不运行NFO/stat。
- Store `browse_title_page(&self, library_id: &str, offset: usize, limit: usize, desc: bool) -> Result<Vec<BrowseMediaFacts>, StoreError>`；只用于无额外复杂filter/title排序；其他请求保留target紧凑facts筛选，不错误先分页再filter。

- [ ] **Step 1: 写红测试。** 热页limit24在100k无关ledger+100MB无关catalog下BrowseFileRead调用0；整页输出与T0 oracle相同；gallery空组发生页前剔除；同秒/同title ties无重复丢页；unknown query400；无limit仍全返回；用户whole/legacy电影单位和收藏继承不回退。
- [ ] **Step 2: 红验证。** `cargo test -p api --test browse_projection_paging --test browse_semantic_parity`。
- [ ] **Step 3: 实现投影页与纯render。** 不在wall_item_json/gallery_images中补is_file；从snapshot facts构建URL。facets可扫targetMedia但不扫无关实例。可见性按当前scope复核；stale文件facts响应加header，scope/ledger过期不得用旧代。
- [ ] **Step 4: 绿验证。** `cargo test -p store`、`cargo test -p api`；只对文件facts外部修改测试改为显式等待refresh，不删除语义断言。
- [ ] **Step 5: 提交。** `git commit -m "perf(browse): page projected facts without request time file io"`。

## Task 4: Playback可见范围批量化与剩余锁内IO清除

**Files:** Create `crates/api/src/visible_scope.rs`；Modify [history](<../../../crates/api/src/http/playback_logs.rs>)、[stats](<../../../crates/api/src/http/playback_stats.rs>)、[Library assets](<../../../crates/api/src/http/library/assets.rs>)、[Jellyfin provider](<../../../crates/api/src/media_server_provider.rs>)、[item详情](<../../../crates/api/src/http/library.rs>)、[auth](<../../../crates/api/src/http/auth.rs>)、[STRM流](<../../../crates/media-server/src/routes/media/playback.rs>)；Test `crates/api/tests/read_paths_bounded.rs`、`crates/media-server/tests/strm_read_limits.rs`。

**Interfaces:**
- `VisibleScope::load(store: &Store, user: UserId) -> Result<VisibleScope, StoreError>`，owned当前权限/blocked+projection集合；`media_ids(&self) -> &[MediaId]`。
- `Store::library_projected_stats(&self, library_id: &str) -> Result<LibraryStats, StoreError>`；Stats item_count/file_count/total_size_bytes/strm_file_count；读取投影若不可用由service重建。
- `async fn read_strm_bounded(path: PathBuf) -> Result<String, String>`，64KiB decoded，上限+1检测，blocking有界；错误固定对外消息，详细path只日志。provider持有cover_tag缓存按已发布版本，不再每请求read/hash。

- [ ] **Step 1: 写红测试。** history limit30与stats小scope不调用逐rowget_media/重载roots；通过可观察的查询计数/fixture数据隔离与IO gate测试，而非断言SQL字符串。admin scopeall/member隐藏/删除冻结不泄露；重复temp visible集合不会跨User污染。慢cover/NFO/STRM下其他auth/心跳完成；STRM65536字节允许、65537拒绝、非法UTF8拒绝。
- [ ] **Step 2: 红验证。** `cargo test -p api --test read_paths_bounded`、`cargo test -p media-server --test strm_read_limits`。
- [ ] **Step 3: 实现批量scope与offlock作业。** 所有鉴权DB短作业移blocking，不持Store做metadata；封面tag沿版本发布缓存，legacy60秒刷新；library列表stats使用投影。若无投影需await service，不直接全ledger降级。授权检查不得被图片public规则误删。
- [ ] **Step 4: 绿验证。** `cargo test -p store`、`cargo test -p media-server`、`cargo test -p api`；B2播放ID/时长保持。
- [ ] **Step 5: 提交。** `git commit -m "perf(api): batch visibility and remove remaining locked file reads"`。

## Task 5: 持久扫描operation、单飞与前端202合同

**Files:** Create Store scan_operations/schema模块、API scan_operations模块、HTTP library_scan/operations；Modify [scan入口](<../../../crates/api/src/http/library_scan.rs>)、[创建自动扫描](<../../../crates/api/src/http/library_config.rs>)、[watcher](<../../../crates/api/src/fs_watcher.rs>)、[state](<../../../crates/api/src/management/state.rs>)、[路由](<../../../crates/api/src/http/mod.rs>)、[Store schema](<../../../crates/store/src/schema.rs>)、[Web library API](<../../../web/lib/api/libraries.ts>)、[管理页](<../../../web/components/library-manage-view.tsx>)、[详情页](<../../../web/components/library-detail-view.tsx>)；Test `crates/api/tests/scan_operations.rs`、`crates/store/tests/scan_operations.rs`。

**Interfaces:**
- `ScanOperation { id: String, library_id: String, scope_signature: String, status: ScanOperationStatus, generation: i64, report: serde_json::Value }`；enum Queued/Running/Yielded/Completed/Partial/Cancelled/Failed。
- `Store::enqueue_scan(&self, library_id: &str, signature: &str, targets: &[PathBuf], now_ms: i64) -> Result<ScanOperation, StoreError>`；活跃部分unique、重复同库返回已有operation并合并dirty target。
- `Store::scan_operation(&self, id: &str) -> Result<Option<ScanOperation>, StoreError>`、`Store::cancel_scan(&self, id: &str, now_ms: i64) -> Result<bool, StoreError>`。
- API `ScanCoordinator::enqueue(state: &ApiState, library_id: &str, targets: Vec<PathBuf>) -> Result<ScanOperation, String>`，共享dispatcher最多2；本任务可用B2reported walker执行，但不裸spawn绕过coordinator。

- [ ] **Step 1: 写HTTP/Store红测试。** POST202+operation_id；重复20次同库仅一active；两库允许2、第三queued；GET仅所属Library可见/管理员变更；cancel幂等；冻结409；重启running恢复queued但已scope改变取消；Web202不立即显示scan完成。
- [ ] **Step 2: 红验证。** `cargo test -p store --test scan_operations`、`cargo test -p api --test scan_operations`。
- [ ] **Step 3: 实现operation持久化与dispatcher。** 使用library9迁移模块新增表、保存报告/seen，不引入jobs.db重复调度事实；所有HTTP/auto/watch接同协调器，运行拿B1lease，完成后probe由B2义务派发。operation清理不能误删probe/用户事实。
- [ ] **Step 4: 更新Web API/调用方。** 增加typed202返回、operation状态轮询（完成/partial/failed/cancelled停止），页面离开取消轮询不默认cancel服务器扫描；保留错误诊断，不能无限请求。目标为上述libraries API、管理页、详情页，新增 `web/lib/api/scan-operations.test.ts`；禁止只改类型不改调用方。
- [ ] **Step 5: 绿验证。** `cargo test -p store`、`cargo test -p api`、`pnpm -C web typecheck`及scan operation前端单测。
- [ ] **Step 6: 提交。** `git commit -m "feat(scan): persist single flight scan operations"`。

## Task 6: 有界可取消迭代walker与短事务登记

**Files:** Create `crates/library/src/watch/walker.rs`；Modify [watch](<../../../crates/library/src/watch.rs>)、[library exports](<../../../crates/library/src/lib.rs>)、T5 runner、[watch ledger](<../../../crates/api/src/watch_ledger.rs>)、B1ScanScope、B2report；Test `crates/library/tests/scan_walker.rs`、`crates/api/tests/scan_budget_recovery.rs`。

**Interfaces:**
- `ScanWalkBudget { max_entries: usize, max_elapsed: Duration, chunk_files: usize }`默认5000/10s/100。
- `ScanWalker::new(roots: Vec<PathBuf>, kind: MediaKind, control: library::WorkControl) -> Self`；`next_chunk(&mut self, budget: &ScanWalkBudget) -> Result<ScanChunk, LibraryError>`；Chunk含files/visited/issues/yielded/done。
- `Store::record_scan_seen(&self, operation_id: &str, paths: &[PathBuf]) -> Result<(), StoreError>`与当前chunk提交同library事务（ledger/obligation/seen），app身份写仍独立；`scan_seen`去重仅当前operation，配置signature变取消。
- metadata以本scope `HashMap<MediaId, Vec<LedgerRow>>`快照定向处理，不重复rows_in_library。

- [ ] **Step 1: 写红测试。** 6000entry第一轮≤5000；chunk≤100；深层200目录不栈递归；取消在下个entry/chunk停止；权限错误partial；不跟随symlink。kill点在ledger提交/seen前后、probe ACK前后恢复结果相同且无重复完成probe；重启重新枚举时changed/missing文件不被seen永久跳过。
- [ ] **Step 2: 红验证。** `cargo test -p library --test scan_walker`、`cargo test -p api --test scan_budget_recovery`。
- [ ] **Step 3: 实现迭代walker/chunk事务和恢复。** NFO解析复用现有识别，不移除置信度/NFO别名规则；每chunklease.publish重新validate；seen/obligation一致，旧scope不续扫。扫描后projection dirty；不边walk无保护写sidecar。
- [ ] **Step 4: 绿验证。** `cargo test -p library`、`cargo test -p store`、`cargo test -p api`；模拟慢OS IO的drain超时仍保持frozen，不将cancel=true等同线程已结束。
- [ ] **Step 5: 提交。** `git commit -m "perf(scan): bound traversal and checkpoint ledger chunks"`。

## Task 7: Jobs真实检视预算与持久游标

**Files:** Create jobs retention/cursor；Modify [retention](<../../../crates/jobs/src/retention.rs>)、[Queue](<../../../crates/jobs/src/queue.rs>)、[jobs迁移](<../../../crates/jobs/src/store.rs>)、[maintenance runner](<../../../crates/api/src/maintenance/runner.rs>)、[report](<../../../crates/api/src/maintenance/report.rs>)、[preview](<../../../crates/api/src/http/job_maintenance.rs>)、[Web summary](<../../../web/lib/maintenance-preview.ts>)、[Web jobs类型](<../../../web/lib/api/jobs.ts>)；Test `crates/jobs/tests/retention_budget.rs`、`crates/api/tests/maintenance_budget_cursor.rs`。

**Interfaces:**
- `RetentionCursor { cutoff: i64, finished_at: i64, rowid: i64, cycle_started_at: i64 }`；`Queue::prune_terminal_window(&self, def_id: JobDefId, cutoff: i64, limit: usize, dry_run: bool) -> Result<RetentionWindow, JobError>`。
- `RetentionWindow { scanned: usize, candidates: usize, protected: usize, deleted: usize, window_has_more: bool, cycle_complete: bool }`。scanned=原始行数，保护计入；返回旧RetentionBatch wrapper映射需文档声明scanned口径变化。
- runner新增Jobs专用窗口循环，20批/2秒、批间解Queue锁；不把`deleted==0`当结束，报告protected本cycle累计/未完lowerbound。

- [ ] **Step 1: 写红测试。** 500k独立终态单窗口scanned≤500、VMsteps随window而非全history线性；前10000protected后1可删跨轮最终删除；同timestamp/run_after ties全保留；cutoff相等/NULL/queued/running不删；游标事务trigger失败同时回滚删除和推进；重启继续；cycle结束后迟到低key下一轮可删；dryrun零写不改cursor。
- [ ] **Step 2: 写API/前端合同测试。** protected-only批继续到预算；停用/取消批边界生效；preview报告窗口truncated不是更多可删确数；summary明确“已检视保护（下界）”，不谎报全库总数。
- [ ] **Step 3: 红验证。** `cargo test -p jobs --test retention_budget`、`cargo test -p api --test maintenance_budget_cursor`；pnpm单测显示新口径未支持。
- [ ] **Step 4: 实现原始keyset窗口及cursor同事务。** latest检查利用既有last index，若EXISTS仍增长做等价keyset/index改写并以VM/EQP验证，不放宽保护规则。批间释放锁、dryrun不写；保留旧公共方法有界wrapper，不新增两个相互矛盾后台入口。
- [ ] **Step 5: 绿验证。** `cargo test -p jobs`、`cargo test -p api`、`pnpm -C web typecheck`、`pnpm -C web test -- lib/maintenance-preview.test.ts`。
- [ ] **Step 6: 提交。** `git commit -m "perf(jobs): bound retention by visited windows and durable cursors"`。

## Task 8: 共享Jobs SSE常数查询

**Files:** Create `crates/jobs/src/change_state.rs`；Modify [jobs迁移](<../../../crates/jobs/src/store.rs>)、[Queue](<../../../crates/jobs/src/queue.rs>)、[job_updates](<../../../crates/api/src/job_updates.rs>)、[SSE handler](<../../../crates/api/src/http/jobs.rs>)；Test `crates/jobs/tests/change_state.rs`、`crates/api/tests/job_updates_bounded.rs`。

**Interfaces:**
- `JobChangeState { revision: i64, finished_count: i64 }`；`Queue::change_state(&self) -> Result<JobChangeState, JobError>`。
- `JobCount`保留count新增revision；SSE保持旧event/count兼容，revision驱动更新；查询进owned blocking，不在poller直接同步DB。

- [ ] **Step 1: 写红测试。** INSERT/UPDATE finished_at NULL→positive→NULL、DELETE/prune、rawSQL触发count一致；迁移初始一次COUNT与后续O(1)点查；失败保留last-good首次失败Err；多SSE共1poller、最后drop停止；retention减少count也emit。
- [ ] **Step 2: 红验证。** `cargo test -p jobs --test change_state`、`cargo test -p api --test job_updates_bounded`。
- [ ] **Step 3: 实现幂等DDL/触发器及poller点查。** count谓词严格finished_at>0，不能偷换成status终态；revision所有Jobs/defs变更递增，触发器失败随原事务回滚，重启不每次全表回填。
- [ ] **Step 4: 绿验证。** `cargo test -p jobs`、`cargo test -p api`。
- [ ] **Step 5: 提交。** `git commit -m "perf(jobs): publish change revision without history recounts"`。

## Task 9: 并发准入、协议背压与跨库竞争核实

本任务含独立可拒绝切片，**9A/9B/9C分别提交**，不得一个巨大提交。

### 9A 搜索/目录/WebSocket预算
**Files:** Modify [search](<../../../crates/api/src/http/search.rs>)、[fs browse](<../../../crates/api/src/http/fs.rs>)、[state](<../../../crates/api/src/management/state.rs>)、[WebSocket](<../../../crates/media-server/src/routes/websocket.rs>)；Create `crates/api/tests/request_admission.rs`、`crates/media-server/tests/websocket_backpressure.rs`。
**Interfaces:** ApiState `search_work: Arc<BlockingBudget>`默认4、`fs_work`默认2；WS send helper `async fn send_with_deadline(socket: &mut WebSocket, message: Message) -> Result<(), String>`默认5s。搜索12s、fs10s外层只读等待，permit由worker持有到完成。
- [ ] **Step 1: 红测试。** 5搜索/3目录同时阻塞容量拒绝503，timeout后不提前释放permit；SSE断开不继续排新Site；slow WS不读在5s deadline关闭且撤销巡检不永久挂住；单Site失败仍输出其他站结果。
- [ ] **Step 2: 验证红并实现。** `cargo test -p api --test request_admission`、`cargo test -p media-server --test websocket_backpressure`；独立sender/撤销select防send await饿死检查，不记录敏感URL/token。
- [ ] **Step 3: 绿/提交。** `cargo test -p api`、`cargo test -p media-server`；`git commit -m "fix(runtime): bound search directory and websocket work"`。

### 9B 多连接Jobs claim竞争
**Files:** Create `crates/jobs/src/store/claim.rs`；Modify [claim现实现](<../../../crates/jobs/src/store.rs>)；Test `crates/jobs/tests/claim_concurrency.rs`。
**Interfaces:** 现有Queue::claim签名不变；内部next/key_busy/UPDATE在BEGIN IMMEDIATE同事务；CAS按queued状态避免重复claim。
- [ ] **Step 1: 红测试。** 两独立Queue连接Barrier同时领相同concurrency_key不同def，恰1running；不同key各可运行；timeout/recover旧attempt不能finish新attempt。
- [ ] **Step 2: 红→实现。** `cargo test -p jobs --test claim_concurrency`；实现Immediate事务，busy保持JobError，不声称多进程API整体安全。
- [ ] **Step 3: 绿/提交。** `cargo test -p jobs`；`git commit -m "fix(jobs): claim concurrency keys atomically across connections"`。

### 9C Subscribe补偿与协议兼容边界
**Files:** Modify [Subscribe创建](<../../../crates/api/src/subscribe_create.rs>)、[Media Store](<../../../crates/store/src/media.rs>)、[jobs seed](<../../../crates/api/src/jobs_api.rs>)；Create `crates/api/tests/subscribe_compensation_race.rs`、`crates/media-server/tests/range_compatibility.rs`。
**Interfaces:** Store `undo_ensured_media_if_unreferenced(&self, media_id: MediaId, expected: &Media) -> Result<bool, StoreError>`只在当前内容匹配、无Subscribe/ledger引用时删除；别名恢复使用期望值CAS。API `reconcile_subscribe_jobs(state: &ApiState, limit: usize) -> Result<usize,String>`每轮≤100、为缺def现有Subscribe幂等补齐，启动/创建失败调用，不删除其他请求Media。
- [ ] **Step 1: 故障注入测试。** A写Subscribe后Jobs失败暂停，B同Media创建成功，A补偿不能删B身份/别名；crash前缀重启reconcile补def且不重复；无引用旧Media可补偿但用户事实保留规则不破坏。Range表征单range/suffix/open/416、多range416；If-Range当前行为记录，不把未支持变成已支持断言。
- [ ] **Step 2: 验证/实现。** `cargo test -p api --test subscribe_compensation_race`，只有真实失败才升级风险为bug；最小CAS补偿与reconcile，不能写跨库事务保证。
- [ ] **Step 3: 绿/提交。** `cargo test -p store`、`cargo test -p api`、`cargo test -p media-server`；`git commit -m "fix(subscribe): guard compensation and reconcile missing jobs"`。

## Task 10: Release测量、规模验证与总验收

**Files:** Modify [endpoint perf](<../../../crates/api/tests/endpoint_perf.rs>)；Create `crates/api/tests/bounded_work_acceptance.rs`、审计报告`docs/audits/2026-10-13-batched-performance/acceptance.md`；Update [总路线图](<2026-10-13-database-api-scan-master-plan.md>) 真实状态。
**Interfaces:** 无新生产API。消耗T0基线、T1–T9接口。

- [ ] **Step 1: 跑结构验收。** 无关100kledger/100MBcache小页热IO=0；200kledger混合库权限无泄露；500k Jobs窗口visited≤500且latest查询VM增长受控；scan≤2/每库≤1，6000entries yield，崩溃重启一致。使用观测计数/VMsteps及结果，不以机器绝对ms作为普通单测断言。
- [ ] **Step 2: 跑Release矩阵。** 与T0相同命令/机器/seed，30样本；冷/热分开，记录status/bytes/median/p95、内存、最大锁持有和EQP。items/gallery/history中位≥30%改善、jobs不得>20%退化；不达先定位，不删除慢样本，不用不同fixture冒充改进。
- [ ] **Step 3: 全量回归。** `cargo test -p store`、`cargo test -p library`、`cargo test -p jobs`、`cargo test -p media-server`、`cargo test -p api`、`cargo test --workspace`；Web有改运行typecheck与相关测试；diff/尺寸检查。
- [ ] **Step 4: 审计报告。** 逐条标Pass/Fail/Not run：B1数据安全、B2事件/时长/义务、B3结构/性能/NAS/设备。真实NAS、VidHub、Infuse无人实测则Not run，不以offline基准代替。记录schema备份/回退操作。
- [ ] **Step 5: 提交。** `git commit -m "test(perf): accept bounded api database and scan execution"`。仅当全部必要gate通过，路线图写完成；设备外部验收未运行时另列而不隐去。

## 自审

本批区分projection可重建与durable scan运行意图、保留B1/B2权威事实。复杂筛选允许O目标库而非虚称全O(page)。各失效源、scope/revision竞态、gallery剔空、protected-only、跨连接claim、补偿竞争和WS背压都有任务。SQL/schema/body仅写必要决策，不把计划变成完整实现；所有性能数值是未来门槛，尚未测试。
