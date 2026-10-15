# 接口性能修复 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
> 使用实际环境可用的等价执行技能。未实施；不自动关闭旧计划的设备/性能验收。

**Goal:** 把普通读取从共享Store/async worker离锁，把Jellyfin重复扫描变为批量选页，并让REST热页、Library统计和up-next不做非必要全量/网络工作。

**Architecture:** 先建立owned读取与权限快照，保持strict副作用验证。Jellyfin先批量读后轻候选选择/重元数据按页；REST复用投影head/定向facts；外部Metadata降为不阻塞本地卡片的后台补齐。

**Tech Stack:** Rust 2024、Axum 0.8、Tokio、rusqlite 0.37、parking_lot、React/TypeScript现有消费者。

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

1. 同Media多库、hidden/nested/ambiguous/frozen：count/分页/统计不泄露，strict不回退（A1/A3/A4/A5）。
2. 慢NAS或坏目录、持锁竞争：无关认证/heartbeat推进，预览截断明确而非假查全（A1）。
3. 相同标题/时间、过滤跨页、unknown parent、全已看：不漏不重、默认played-last与显式IsPlayed不变（A3/A4）。
4. movie ID与TV ID恰同数值、过期错误缓存：电影不season请求，本地卡片不等外网（A6）。
5. FS期间root变更/同路径替换/用户撤销：旧快照不授权新副作用，当前scope校验仍生效（A1/A2/A3）。

---

## 文件拆分与接口责任

| 单元 | 文件 | 责任 |
|---|---|---|
| Subscribe owned事实 | 新 `crates/store/src/subscribe_library_snapshot.rs`；新 `crates/api/src/subscribe_library_read.rs` | DB快照/strict离锁观察/使用前校验 |
| Playback视图拆分 | [playback_views](<../../../crates/api/src/http/playback_views.rs>) → 新 `http/playback_views/{up_next,favorites,gallery}.rs` | 原742行入口变模块树，不混新逻辑重写 |
| 路径预览 | [housekeeping](<../../../crates/api/src/http/library_housekeeping.rs>)；新 `http/library_housekeeping/reconciliation.rs` | 单次目录索引/预算/明确截断 |
| Jellyfin批读 | 新 `crates/api/src/media_server_provider/read_snapshot.rs`、`selection.rs` | owned DB、轻选择、按页metadata |
| 协议查询类型 | 新 `crates/media-server/src/provider/query.rs` | 不依赖API的typed query/page合同 |
| REST投影页 | 新 `crates/store/src/browse_pages.rs`、`schema/browse_page_facts.rs`；新 `crates/api/src/http/library/selection/page.rs` | head→candidate→page facts，不全读files |
| Library统计 | 新 `crates/api/src/library_statistics.rs` | 一次批量归属聚合/刷新状态 |

新增模块只在所属lib/module按需re-export。修改 [http/library_chapters](<../../../crates/api/src/http/library_chapters.rs>)、[probe_manager](<../../../crates/api/src/probe_manager.rs>) 等共享近800行文件时与资源计划串行，不为本计划增加无关重构。

### Task A1：移除Store临界区FS，路径核对只扫一次（API-02、DB-03）

**Files:** Modify [playback views](<../../../crates/api/src/http/playback_views.rs>)、[episode入口](<../../../crates/api/src/http/playback.rs#L182-L225>)、[episode聚合](<../../../crates/api/src/http/playback/episodes.rs>)、[housekeeping](<../../../crates/api/src/http/library_housekeeping.rs>)、[Subscribe事实](<../../../crates/store/src/subscribe_library.rs>)、[严格scope](<../../../crates/store/src/library_scope.rs>)、[finish_search](<../../../crates/api/src/worker/finish_search.rs>)、[Transfer](<../../../crates/api/src/worker/transfer.rs>)；Create地图前三单元；Test Create `crates/api/tests/view_io_isolation.rs`、`crates/api/tests/reconciliation_budget.rs`、`crates/api/tests/subscribe_fact_io_isolation.rs`。

**Interfaces:** Store新增 `SubscribeLibrarySnapshot { subscribe:domain::Subscribe,kind:domain::MediaKind,ownership:LibraryOwnership,saved:subscribe::SubscribeFacts,rows:Vec<domain::LedgerRow>,qualities:HashMap<domain::LedgerId,domain::Release>,revisions:ReadRevisions }` 与 `Store::subscribe_library_snapshot(&self,subscribe:&domain::Subscribe,kind:domain::MediaKind)->Result<SubscribeLibrarySnapshot,StoreError>`；API `read_subscribe_library_facts(state:&ApiState,subscribe:&domain::Subscribe,kind:domain::MediaKind)->Result<subscribe::SubscribeFacts,String>`（同步，调用者已在blocking worker；HTTP调用需预算）。纯FS内部 `index_basenames(roots:&[PathBuf],control:&library::WorkControl,max_entries:usize)->Result<ReconciliationIndex,String>`；`ReconciliationIndex { candidates:HashMap<OsString,Vec<PathBuf>>,examined:usize,incomplete:bool }`。Store新增 `pub trait LibraryPathResolver { fn resolve_path(&self,path:&std::path::Path)->Result<PathBuf,StoreError>; }` 和 `LibraryPathScope::resolve_with(snapshot:LibraryOwnership,resolver:&dyn LibraryPathResolver)->Result<Self,StoreError>`；原resolve委托真实resolver，非NotFound错误向上传播，不跳过失败根。fake resolver证明更深根EIO时不得归给更宽根；此输入对应Step1的PermissionDenied/坏根测试。

- [ ] **Step 1：写失败测试。** `slow_gallery_and_episodes_do_not_hold_store`、`slow_subscribe_fact_read_keeps_heartbeat_responsive`、`reconciliation_scans_each_directory_once`：gated FileRead/FileScope fake必须真的走生产seam；同runtime current-thread并发请求：

```rust
assert!(state.store().try_lock().is_some());
assert_eq!(auth_response.status(), StatusCode::OK);
assert_eq!(heartbeat_response.status(), StatusCode::OK);
assert_eq!(preview["incomplete"], true); // 100,001项越过100,000预算
```

补missing/PermissionDenied、depth5边界、symlink外逃不作候选、多个同basename、root变化/freeze、同路径新ledger、删除事实仍保留。目录次数是“重复FS请求错误”诊断，不替代候选结果断言。
- [ ] **Step 2：跑RED。** `cargo test -p api --test view_io_isolation --test reconciliation_budget --test subscribe_fact_io_isolation`。gate先放行再断言，失败不留下永阻线程；不要用真实NAS/sleep构造。
- [ ] **Step 3：实现。** 先只拆视图模块保持表征，锁内一次owned DB/units/Release；锁外用BrowseFileRead/LibraryPathScope和http_work（30秒，8容量）观察。Subscribe在add/Transfer副作用边界重读scope/source并CAS，变更拒绝/重试，不写过期事实。路径预览每root一次read_dir索引，最多100,000项、30秒，返回examined/incomplete，不将preview当应用授权。
- [ ] **Step 4：跑GREEN。** 以上targets；`cargo test -p store`、`cargo test -p subscribe`、`cargo test -p api`。full metadata等价/用户权限/旧取消测试一起绿；本次不改删除语义。
- [ ] **Step 5：提交。** 可先“视图拆分+既有表征”，再每个独立消费者提交；最终 `fix(api): keep filesystem observations outside store locks`。每个切片有自己的RED/GREEN，禁夹带用户Probe改动。

### Task A2：Jellyfin共享批量快照与blocking（API-01第一阶段）

**Files:** Create `read_snapshot.rs`；Modify [provider](<../../../crates/api/src/media_server_provider.rs#L223-L252>)、[items](<../../../crates/api/src/media_server_provider/items.rs>)、[item metadata](<../../../crates/api/src/media_server_provider/item_metadata.rs>)、[browse reads](<../../../crates/store/src/browse_reads.rs>)；Test Create `crates/api/tests/jellyfin_read_scale.rs`，Extend [offlock回归](<../../../crates/api/tests/media_server_list_off_lock.rs>)。

**Interfaces:** API内部 `ServerReadSnapshot { ownership:store::LibraryOwnership,user_id:domain::UserId,is_admin:bool,media:HashMap<MediaId,Media>,rows:Vec<LedgerRow>,units:Vec<store::PlaybackUnitSnapshot> }`；`collect_server_read(store:&Store,user_id:UserId,parent_id:Option<&str>)->Result<ServerReadSnapshot,String>`；`build_visible_items(snapshot:ServerReadSnapshot,files:&dyn BrowseFileRead)->Result<Vec<media_server::provider::MediaItemSnapshot>,String>`。marker/chapters/tracks以定向batch附加owned字段，不在render内重新Store查询；所有字段由该任务定义，不将handler query依赖传入Store。

- [ ] **Step 1：写失败测试。** `jellyfin_metadata_gate_does_not_block_async_worker`、`jellyfin_ownership_is_read_once_per_snapshot`、`parent_scope_does_not_read_unrelated_media`：

```rust
assert_eq!(json_after, characterized_json_before);
assert!(unrelated_http_completed_before_gate_release);
assert!(db_work_large <= db_work_small * 2); // 固定parent目标，仅无关数据增长
```

覆盖multi-copy/hidden/nested/ambiguous、freeze、根parent无匹配、unknown返回空、movie legacy unit。调用计数仅诊断；工作量用VM/读行，结果用公开协议JSON。
- [ ] **Step 2：跑RED。** `cargo test -p api --test jellyfin_read_scale --test media_server_list_off_lock`。区分已有off-lock绿与新async/规模红。
- [ ] **Step 3：实现。** 一次ownership+角色、按parent先限ledger候选、media_batch/units批读；归属只纯内存比较。DB与FS运行在http_work，公开provider异步只等待owned工作。补server library cover读取也离锁；以现有协议映射错误，不能内部故障返回空列表。
- [ ] **Step 4：跑GREEN。** `cargo test -p store`、`cargo test -p api`、`cargo test -p media-server`；现有jellyfin_scope/default latest/playback marks一致。报告阶段状态为“批读/异步修复，分页元数据仍由A3完成”。
- [ ] **Step 5：提交。** `git commit -m "perf(jellyfin): batch scoped reads outside async workers"`。

### Task A3：Jellyfin先选择后重元数据、NextUp一次聚合（API-01完成）

**依赖：** A2；D3可独立，但共享projection/schema写必须排队。

**Files:** Create API `selection.rs`、media-server `provider/query.rs`；Modify [provider trait](<../../../crates/media-server/src/provider.rs>)、[catalog routes](<../../../crates/media-server/src/routes/catalog.rs>)、[shows](<../../../crates/media-server/src/routes/shows.rs>)、[filters](<../../../crates/media-server/src/routes/item_filters.rs>)、[API provider](<../../../crates/api/src/media_server_provider.rs>)；Test Create `crates/api/tests/jellyfin_paged_metadata.rs`、`crates/media-server/tests/next_up_paging.rs`。

**Interfaces:** 协议层新增 `ServerItemQuery { parent_id:Option<String>,series_id:Option<String>,season_id:Option<String>,person_ids:Option<String>,filters:Option<String>,is_favorite:Option<bool>,is_played:Option<bool>,sort_by:Option<String>,sort_order:Option<String>,include_item_types:Option<String>,recursive:bool,is_folder:Option<bool>,search_term:Option<String>,genres:Option<String>,genre_ids:Option<String>,studio_ids:Option<String>,offset:usize,limit:usize }`；`ServerItemPage { items:Vec<MediaItemSnapshot>,total:usize }`；trait新增 `async fn query_visible_items(&self,user:UserId,query:ServerItemQuery)->Result<ServerItemPage,String>` 与 `async fn next_up_items(&self,user:UserId,series_id:Option<&str>,offset:usize,limit:usize)->Result<ServerItemPage,String>`。mock默认实现可按旧方法等价过滤；把既有纯过滤/排序抽到provider/query模块并由routes复用，避免provider调用routes私有模块或复制两份语义；生产必须override，禁止默认实现承诺有界。existing list保留给未迁移消费者。

- [ ] **Step 1：写失败测试。** `jellyfin_limit_one_builds_one_heavy_snapshot`、`next_up_does_not_rescan_for_each_series`：200 TV/10k ledger、limit1/20，插入页外损坏重facts（非过滤字段）不应导致当前页失败：

```rust
assert_eq!(page.items.len(), 1);
assert_eq!(page.total, expected_visible_total);
assert_eq!(heavy_metadata_items, 1);
assert!(whole_candidate_passes <= 1);
```

补Genre/studio/person过滤后分页、Episode expansion、folder、unknown parent、name/time ties、StartIndex、最新默认played-last/显式IsPlayed、同Media跨库未看聚合。轻筛字段必须完整后选页，不允许选页后enrich再筛person。
- [ ] **Step 2：跑RED。** 两个新target分别 `cargo test -p api --test jellyfin_paged_metadata` 与 `cargo test -p media-server --test next_up_paging`；当前重metadata/NextUp重扫失败。
- [ ] **Step 3：实现typed query。** 路由解析保留旧默认/clamp，构建轻候选（排序/过滤所需NFO公共字段使用投影或缓存轻事实），先完整筛选/排序/count，再只构建本页tracks/chapters/NFO细节。NextUp一次候选units分组、每Media选第一未看，再整体分页；按Media调用list全库的循环删除。复杂NFO filter cold成本可线性，日志显式，不宣称全部SQL seek。
- [ ] **Step 4：跑GREEN。** `cargo test -p media-server`、`cargo test -p api`；对相同fixture所有query组合与旧输出名单/次序/count逐项等价。release记录候选与重facts两种数量，不能只报响应条数。
- [ ] **Step 5：提交。** `git commit -m "perf(jellyfin): select pages before metadata and batch next-up"`。

### Task A4：REST投影head、轻候选和本页facts（API-04）

**依赖：** D3的ProjectionHead；A1模块拆分后串行修改library相关入口。

**Files:** Create地图中REST三个模块；Modify [browse reads](<../../../crates/store/src/browse_reads.rs#L135-L169>)、[projection Store](<../../../crates/store/src/browse_projection.rs>)、[selection](<../../../crates/api/src/http/library/selection/snapshot.rs>)、[items](<../../../crates/api/src/http/library.rs>)、[gallery](<../../../crates/api/src/http/library_gallery.rs>)；Test Create `crates/store/tests/browse_page_reads.rs`、`crates/api/tests/browse_page_work_bounds.rs`。

**Interfaces:** Store新增 `BrowsePageOrder::{Title,AddedAt,ReleaseDate}`、`BrowsePageRequest { library_id:String,offset:usize,limit:usize,order:BrowsePageOrder,descending:bool }`；`Store::browse_media_page(&self,request:&BrowsePageRequest)->Result<Vec<BrowseMediaFacts>,StoreError>`；`Store::browse_file_facts_for_media(&self,library_id:&str,ids:&[MediaId])->Result<Vec<BrowseFileFacts>,StoreError>`。API `load_browse_page(state:&ApiState,library:&store::Library,user:Option<UserId>,query:&HashMap<String,String>)->Result<(Vec<LibrarySelection>,projected::ProjectedFacts),axum::response::Response>`。轻候选包含设计过滤/排序字段，每Media标量/小集合；不含LedgerRow或全files JSON，定义在store新模块并由builder写入。

- [ ] **Step 1：写失败测试。** `hot_page_decodes_only_selected_file_facts`、`advanced_filter_selects_before_paging`：20页大小、2k/10k Media多episode：

```rust
assert_eq!(paged_json, legacy_selection_json);
assert!(decoded_file_facts <= files_owned_by_selected_media);
assert_eq!(hot_file_io, 0);
assert_eq!(omitted_limit_result.len(), full_visible_count);
```

补title/added_at/release_date ties与None（release_date保留完整日期/年份兜底，不新增sort=year）、offset越界/usize极值、limit201拒绝、watch/genre/rating/HDR/missing/size/random等组合、用户unit变更不重建公共投影、损坏选中facts报错/安全fallback、scope变更不返回旧页。
- [ ] **Step 2：跑RED。** `cargo test -p store --test browse_page_reads`；`cargo test -p api --test browse_page_work_bounds`。新解码工作量在当前实现失败，不以现有5条paging绿当证据。
- [ ] **Step 3：实现。** ensure只读D3 head；无watch/filter常用排序用SQL索引+stable media_id tie先选ID，再批读facts/units。高级查询从轻候选完整筛选排序，选页后才读files，残余O(M+U)标日志。gallery选有图候选再分页；items省略limit行为不改。新增索引/列幂等迁移，schema版本与D3/R3/R4串行递增。
- [ ] **Step 4：跑GREEN。** `cargo test -p store`、`cargo test -p api`；browse_projection_hot_path/budget/paging/parity/facets全部过；简单热页固定页数据从50k→100k无关files，decoded rows不增。不要保证OFFSET深翻页恒定。
- [ ] **Step 5：提交。** `git commit -m "perf(api): load library projection facts by selected page"`。

### Task A5：Library统计一次聚合与可见刷新状态（API-03）

**依赖：** A2的批读模式、A4页事实列可复用；可以先只一次聚合独立交付，不等待D5默认归属设计。

**Files:** Create library_statistics模块；Modify [list libraries](<../../../crates/api/src/http/library.rs#L182-L237>)、[assets](<../../../crates/api/src/http/library/assets.rs>)、[前端API](<../../../web/lib/api/libraries.ts>)；Test Create `crates/api/tests/library_statistics_scale.rs`。

**Interfaces:** `LibraryStatistics { item_count:u64,file_count:u64,total_size_bytes:u64,strm_file_count:u64,refreshed_at_ms:i64,stale:bool }`；`async fn read_library_statistics(state:ApiState,user:Option<UserId>)->Result<HashMap<String,LibraryStatistics>,String>`。配置JSON保持，stats新增两个兼容字段。统计按当前Library scope，不能Media去重后泄露其他库拷贝。

- [ ] **Step 1：写失败测试。** `library_list_does_not_repeat_global_reads_per_library`、`hot_statistics_do_not_stat_each_media_file`：1/5/20库、固定每库与无关ledger，两个请求之间无文件变化：

```rust
assert_eq!(all_stats, expected_stats_by_owner);
assert_eq!(second_request_media_stats, 0);
assert!(second.stats.refreshed_at_ms >= first.stats.refreshed_at_ms);
assert_eq!(strm_physical_size, 0);
```

补hidden/frozen、nested/tied根、同Media多库、missing/root_missing、配置变化不复用旧统计、冷统计不能返回假0。clock固定测试刷新状态。
- [ ] **Step 2：跑RED。** `cargo test -p api --test library_statistics_scale`。
- [ ] **Step 3：实现。** 锁内一次ownership/role/ledger/media batch聚合；大小从现有file/projection facts汇总，首次无facts进入有界构建；root_missing单独低频观察不混全库file stat。配置范围改即dirty，返回stale及真实refreshed_at，不让每30秒名册轮询重扫全文件系统。
- [ ] **Step 4：跑GREEN。** `cargo test -p api`；`pnpm -C web typecheck`、`pnpm -C web test`；stats/user权限/旧字段一致。release按V增长记录DB映射/锁时间。
- [ ] **Step 5：提交。** `git commit -m "perf(api): aggregate library statistics from shared facts"`。

### Task A6：up-next本地优先，电影不请求TV（API-05）

**依赖：** A1视图拆分；其他API任务可独立。

**Files:** Modify A1创建的 `crates/api/src/http/playback_views/up_next.rs`、[Catalog trait](<../../../crates/api/src/catalog.rs>)、[Tmdb adapter](<../../../crates/api/src/catalog.rs>)、[fanout](<../../../crates/api/src/catalog_fanout.rs>)、[media cache/client](<../../../crates/media/src/client.rs>)；Test Create `crates/api/tests/up_next_local_first.rs`，补media cache seam测试在所属crate。

**Interfaces:** UpNextPlan增加 `kind:MediaKind`。Catalog新增只读无网络 `fn cached_season_details(&self,tmdb_id:&str,season:u32)->Result<Option<Vec<media::EpisodeMeta>>,String>`，默认Ok(None)，Tmdb/fanout override；media新增 `Tmdb::cached_season_details(&self,id:&str,season:u32)->Result<Option<Vec<EpisodeMeta>>,TmdbError>`（读取fresh或stale，绝不fetch）。后台补齐用search_work=4、按(kind,id,season)单飞，worker真结束才清。

- [ ] **Step 1：写失败测试。** `movie_resume_never_requests_tv_season` 和 `blocked_metadata_does_not_delay_local_up_next`，fake Catalog的season方法遇movie直接Err；TV gate不放行也先返回卡片：

```rust
assert_eq!(response.status(), StatusCode::OK);
assert_eq!(body["data"][0]["media_item_id"], movie_id);
assert!(response_ready_before_catalog_gate);
assert_eq!(season_requests_for_movies, 0);
```

此调用计数被允许：bug就是错误outbound请求。补同数值movie/TV ID、cached/fallback名称、failed stale、重复poll去重、后台满不丢本地卡片、freeze后补齐不写已删除本地文件。
- [ ] **Step 2：跑RED。** `cargo test -p api --test up_next_local_first`；media只读cache公开方法测试先编译再行为验证。
- [ ] **Step 3：实现。** 按kind过滤，只读本地NFO/cache装卡片；无标题保持原文件名/空语义，不等待remote；TV缺失后台有界补齐现有catalog cache，不引入第二cache事实。继续保持last_activity顺序与owned episode选择。
- [ ] **Step 4：跑GREEN。** `cargo test -p media`、`cargo test -p api`，回归cache hit/miss/stale、不触发真实网络；单请求不因Q个网络timeout串行等待。
- [ ] **Step 5：提交。** `git commit -m "fix(playback): serve up-next locally without movie season fetches"`。

## 自审与交接

A1覆盖API-02/DB-03；A2/A3完整覆盖API-01，阶段声明分开；A4覆盖API-04；A5覆盖API-03；A6覆盖API-05。Review Focus五类都进入所属task测试。A3协议typed查询不传API私有类型回media-server，A4消费D3 head，不依赖D5未批准接口。新测试fixture/helper在各新test/support中定义，不以假定已有fixture名字直接编译；上方代码只列关键断言，不复制实现正文。最终规模/设备验收见总计划X1。
