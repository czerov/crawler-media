# 数据库修复 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
> 使用当前环境可用的等价执行技能；不安装不存在的技能。本文是待 review 计划，全部实现未开始。

**Goal:** 修复两处索引/范围查询、大 Probe 计数增长与跨库投影失效，并为分类历史容量提供默认关闭的维护能力。

**Architecture:** 保留 SQLite 分库与现有 Queue。先完成单库可证明的小改动，再引入可恢复的派生投影依赖，最后实现 opt-in 分类别 retention；默认 Library 补集归属另过设计门，不直接修改权威文件归属。

**Tech Stack:** Rust 2024、rusqlite 0.37 bundled、Tokio、parking_lot、现有 jobs/maintenance。

**Spec:** [修复设计](<../specs/2026-10-15-performance-resource-remediation.md>)；[审查](<../../audits/2026-10-15-database-api-memory-review.md>)；[总顺序](<2026-10-15-performance-resource-remediation.md>)。

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

1. 相近详情ID、语言并列、`%/_`和子路径：结果不混入且tie一致（D1）。
2. 重复finish、取消后晚到、partial与恢复：计数不重复、不复活取消job（D2）。
3. 移动/删ledger、改root、多库同Media和构建中崩溃：old/new owner失效不丢事件（D3）。
4. 未分类cache/离线stale、active/样本引用、关闭running清理：保护数据、下一批停（D4）。
5. 默认库nested/ambiguous/unmatched与旧数据：派生索引不成为删除权威、升级拒绝安全（D5）。

---

## 文件结构与公共接口地图

| 单元 | 文件 | 责任 |
|---|---|---|
| SQL快路径 | [browse_projection](<../../../crates/store/src/browse_projection.rs>)、[scan obligations](<../../../crates/store/src/scan_probe_obligations.rs>)、[schema](<../../../crates/store/src/schema.rs>) | 详情范围与义务排序索引 |
| Probe计数 | [probe_tasks](<../../../crates/store/src/probe_tasks.rs>)；新 `crates/store/src/probe_tasks/counters.rs` | 单次转移delta与启动一次对账 |
| durable失效 | 新 `crates/store/src/browse_changes.rs`、`crates/store/src/schema/browse_changes.rs`；新 `crates/api/src/browse_projection/changes.rs` | 源事件、cursor、per-Library dirty与增量刷新 |
| 历史维护 | 新 `crates/store/src/data_retention.rs`；新 `crates/api/src/maintenance/data_retention.rs` | 分类保护、稳定游标、现有维护合同接线 |
| 默认归属设计 | 新 `docs/adr/0013-browse-ownership-projection.md`（批准后） | 派生归属版本/backfill/恢复合同，不增加第二权威事实 |

新增模块在 [Store lib](<../../../crates/store/src/lib.rs>) / [API lib](<../../../crates/api/src/lib.rs>) 仅按实际跨crate需求re-export。当前library schema=10、app=7、catalog=2、subscribe=5；每个迁移提交从执行时实际支持版本递增，不能多个任务预写相同版本号。

### Task D1：两处SQL工作量有界（DB-01、DB-02）

**Files:** Modify [详情方法](<../../../crates/store/src/browse_projection.rs#L341-L365>)、[schema](<../../../crates/store/src/schema.rs>)；Test Create `crates/store/tests/catalog_prefix_scale.rs`、`crates/store/tests/probe_obligation_scale.rs`。不改义务公开limit合同。

**Interfaces:** 保留 `Store::catalog_details_for_prefixes(&self, source: &str, prefixes: &[String]) -> Result<Vec<CatalogCacheRow>, StoreError>` 和 `Store::scan_probe_obligations(&self, limit: usize) -> Result<Vec<ScanProbeObligation>, StoreError>`。测试fixture用Store公开写入；VM测量仅测试/ignored诊断seam，不暴露生产任意SQL。

- [ ] **Step 1：写失败测试。** `catalog_read_work_is_bounded_by_target_prefixes` / `obligation_first_window_does_not_scan_the_backlog`，同fixture1k/10k/50k，固定目标和100义务：

```rust
assert_eq!(large.details, small.details);
assert!(large.vm_steps <= small.vm_steps * 2);
assert_eq!(large.obligations.len(), 100);
assert_eq!(large.obligations[0].ledger_id, expected_first);
```

补 exact、两语言同fetched_at、42/420、`/images`排除、空前缀、limit0/101夹限；VM增长是性能bug，可断言，不只比较SQL字符串。
- [ ] **Step 2：跑RED。** `cargo test -p store --test catalog_prefix_scale --test probe_obligation_scale -- --nocapture`；当前工作量断言失败，结果语义表征可绿。记录实际红，不以编译缺fixture当性能红。
- [ ] **Step 3：最小实现。** 使用设计§3.1 exact+question-query BINARY区间；新增 `ledger_probe_obligations_created_ledger_idx(created_at_ms,ledger_id)` 幂等迁移。不加全局case_sensitive_like、不改tie、不更新无关版本。升级前后/重复open/较新版本拒绝加入测试。
- [ ] **Step 4：跑GREEN。** 上述target + `cargo test -p store`；bundled VM倍率≤2，完整返回一致。记录索引写成本/DB大小为release测量，不把Python数值硬编码。
- [ ] **Step 5：提交。** 只stage本任务文件；`git commit -m "perf(store): bound catalog and probe obligation reads"`。

### Task D2：Probe单元转移增量计数（DB-06）

**依赖：** 用户未提交Probe生命周期改动负责人已确认基线；本任务不擅自纳入其diff。

**Files:** Modify [probe_tasks](<../../../crates/store/src/probe_tasks.rs#L269-L342>)、[tables](<../../../crates/store/src/schema/tables.rs>)、[schema](<../../../crates/store/src/schema.rs>)；Create counters模块；Test Create `crates/store/tests/probe_counter_transitions.rs`。

**Interfaces:** 现有 `Store::finish_probe_unit_with_status(&self, job_id: &str, ledger_id: &str, status: &str, error_kind: Option<&str>, error: Option<&str>) -> Result<(ProbeJob, bool, bool), StoreError>` 保持。新增 `Store::reconcile_probe_job_counts(&self, job_id: &str) -> Result<ProbeJob, StoreError>`。内部 `ProbeCounts { completed: usize,succeeded: usize,failed: usize,partial: usize }`，新增DB partial列；不要求增加外部DTO字段。

- [ ] **Step 1：写失败测试。** `large_probe_job_finish_work_is_near_linear` 用100和1,000单元整个公开finish循环：

```rust
assert_eq!(job.completed, 1_000);
assert_eq!(job.succeeded + job.failed + partial + cancelled, job.completed);
assert!(vm_for_1000 <= vm_for_100 * 15);
assert!(!duplicate_finish.1);
```

补mixed partial/failed/cancelled、重复finish、job取消后晚到、旧消息、手工损坏计数后公开reconcile一次修正；返回status/all_done与旧合同一致。
- [ ] **Step 2：跑RED。** `cargo test -p store --test probe_counter_transitions -- --nocapture`；当前COUNT整轮增长断言失败。
- [ ] **Step 3：实现事务delta。** old unit只可queued/running转一次；UPDATE成功才维护job counters；job已终态不可复活。cancel批量/启动恢复调用一次reconcile，不每unit全COUNT；同事务回滚错误。按责任拆模块避免文件/函数上限。
- [ ] **Step 4：跑GREEN。** target + `cargo test -p store` + `cargo test -p api`；重跑现有Probe取消/恢复/marker_publication。若旧用户临时测试失败仍如实记录，不能修改为总green。
- [ ] **Step 5：提交。** `git commit -m "perf(store): update probe counters per accepted transition"`，只包含该任务差异。

### Task D3：可恢复依赖失效与增量投影（DB-04）

**Files:** Create地图中的三个changes模块；Modify [schema/read_revisions](<../../../crates/store/src/schema/read_revisions.rs>)、[schema](<../../../crates/store/src/schema.rs>)、[projection storage](<../../../crates/store/src/browse_projection.rs>)、[service](<../../../crates/api/src/browse_projection.rs>)、[builder](<../../../crates/api/src/browse_projection/builder.rs>)；Test Create `crates/store/tests/browse_change_recovery.rs`、`crates/api/tests/projection_dependency_invalidation.rs`。

**Interfaces（新增，签名需保持）：**

```rust
// store，源DB名字固定enum，不接任意文件
pub enum BrowseChangeSource { App, Library, Catalog }
pub struct BrowseChange { pub source: BrowseChangeSource, pub seq: i64,
    pub entity_kind: String, pub entity_key: String,
    pub old_path: Option<String>, pub new_path: Option<String> }
pub struct ProjectionHead { pub generation: i64, pub dirty_token: i64,
    pub scope_signature: String, pub ready: bool, pub refreshed_at_ms: i64 }
// Store方法
pub fn browse_changes(&self, source: BrowseChangeSource, after: i64,
    limit: usize) -> Result<Vec<BrowseChange>, StoreError>;
pub fn apply_browse_changes(&self, source: BrowseChangeSource, through: i64,
    affected_libraries: &[String]) -> Result<(), StoreError>;
pub fn browse_projection_head(&self, library_id: &str)
    -> Result<Option<ProjectionHead>, StoreError>;
```

apply方法在library事务保存dirty+cursor；source事件清理由确认游标驱动。API changes解析事件→受影响库；Store不依赖API/Catalog。A4使用head避免freshness读完整facts。

- [ ] **Step 1：写失败测试。** `unrelated_catalog_search_does_not_rebuild_other_library`、`moving_ledger_dirties_old_and_new_owner`、`crash_before_cursor_commit_replays_changes`、`new_dirty_during_build_is_not_swallowed`：

```rust
assert_eq!(unchanged_head.generation, initial.generation);
assert!(old_owner_head.dirty_token > initial_old.dirty_token);
assert!(new_owner_head.dirty_token > initial_new.dirty_token);
assert_eq!(after_restart.visible_media, expected_media);
```

补catalog详情语言tie、直接SQL写触发、unknown保守失效、root修改、hidden/frozen、多库拷贝、超过512事件、检查外部NFO变更与无变化不发布新代。gate发生在公开FS seam。
- [ ] **Step 2：跑RED。** `cargo test -p store --test browse_change_recovery`；`cargo test -p api --test projection_dependency_invalidation`。先使fixture编译，再确认旧实现无关重建/全写的行为红。
- [ ] **Step 3：实现设计§3.4。** 每源durable trigger journal、library cursors+dirty事务、最多512；先dirty后cursor再源清理。保留旧rev降级保护/首次full rebuild；正常更新只patch变化的file/media，prepare JSON离Store锁；scope+dirty CAS发布。周期60秒观察但不无条件全标脏，保留未知变化fallback。日志区分检查、变化、构建和retry，不能漏catalog client自己的连接写。
- [ ] **Step 4：跑GREEN。** `cargo test -p store`、`cargo test -p media`、`cargo test -p api`；已有browse等价/budget/scope/parity全过。release记录steady DB/WAL写量；从1个库无关写不触发另库full rebuild。
- [ ] **Step 5：提交。** D3可拆“journal恢复”和“消费者patch”两次提交，各有独立公开测试；任务完成只有两个部分都green。提交信息 `perf(projection): invalidate dependent libraries incrementally`。

### Task D4：分类、默认关闭的容量维护（DB-05）

**依赖：** 旧DB8配置CAS/disable批边界实测可用；与R3/R4共享schema/maintenance接口串行合并。

**Files:** Create data_retention模块和 `crates/store/src/schema/data_retention.rs`；Modify [jobs kinds](<../../../crates/jobs/src/kinds.rs>)、[policy](<../../../crates/api/src/maintenance/policy.rs>)、[maintenance registration](<../../../crates/api/src/maintenance/mod.rs>)、[runner](<../../../crates/api/src/maintenance/runner.rs>)、[worker dispatch](<../../../crates/api/src/worker.rs>)、[job maintenance HTTP](<../../../crates/api/src/http/job_maintenance.rs>)、[report](<../../../crates/api/src/maintenance/report.rs>)、[Jobs FE类型](<../../../web/lib/api/jobs.ts>)、[维护UI](<../../../web/components/maintenance-job-settings.tsx>)；Test Create `crates/store/tests/data_retention.rs`、`crates/api/tests/data_retention_control.rs`。

**Interfaces:** `JobKind::DataRetention`序列化`data_retention`、加入is_maintenance。`DataRetentionPolicy { version:u32,dry_run:bool,qoe_days:u32,transient_cache_days:u32,probe_days:u32 }`（设计固定默认90）。`RetentionClass::{Qoe,TransientCatalog,ProbeHistory}`；`DataRetentionCursor { at_ms:i64,id:String }`；`DataRetentionBatch { examined:usize,deleted:usize,protected:usize,incomplete:bool,resume_after:Option<DataRetentionCursor> }`；`Store::prune_data_history(&self,class:RetentionClass,cutoff_ms:i64,cursor:Option<&DataRetentionCursor>,limit:usize,dry_run:bool)->Result<DataRetentionBatch,StoreError>`。

- [ ] **Step 1：写失败测试。** `new_data_retention_is_disabled_and_dry_run`、`data_retention_protects_active_and_referenced_work`、`disable_after_first_batch_stops_second`：

```rust
assert!(!definition.enabled);
assert!(policy.dry_run);
assert_eq!(protected_detail_body, original_detail_body);
assert_eq!(active_job_after, active_job_before);
assert_eq!(real_deleted_after_disable, 500);
```

补QoE只删old、terminal同scope保留3+ties、captured_job引用保护、attempt与job一致事务、unknown/cache detail stale、preview零写、invalid版本/范围拒绝、restart用户配置不覆盖；200k历史有界keyset/单轮20批或2秒，预算不足remaining明确。
- [ ] **Step 2：跑RED。** `cargo test -p store --test data_retention`；`cargo test -p api --test data_retention_control`；首次新接口编译红后必须记录行为红。
- [ ] **Step 3：实现设计§3.7。** 新def默认false/true、86400秒，策略仅payload一份；复用CAS、disabled/cancel/shutdown检查。transient classifier使用URL path白名单，unknown/detail保护；加入QoE时间、probe终态/attempt关联索引。samples/models/member本轮不删。批报告分别examined/deleted/protected/incomplete，不把目标10000当强制一次全删或硬容量证明。
- [ ] **Step 4：跑GREEN。** `cargo test -p jobs`、`cargo test -p store`、`cargo test -p api`；`pnpm -C web test`、`pnpm -C web typecheck`、`pnpm -C web build`。UI显示默认关闭/预览/范围，旧维护类别合同不变。真实生产启用仍需要用户另行确认。
- [ ] **Step 5：提交。** `git commit -m "feat(maintenance): add opt-in classified data retention"`，更新DB8合同的新增类别而不是宣称历史全关闭。

### Task D5：默认Library候选归属设计门（DB-07）

**Files:** Test Extend [browse read tests](<../../../crates/store/tests/browse_reads.rs>)；Create `docs/benchmarks/2026-10-15-default-library-candidates.md`；用户批准方案后Create ADR地图文件。当前不修改root_id数据。

**Interfaces:** 复用 `Store::ledger_candidates_for_library(&LibraryOwnership,&str)->Result<Vec<LedgerRow>,StoreError>`。候选新接口仅设计：`Store::browse_owned_ledger_ids(&self,library_id:&str,ownership_revision:i64)->Result<Option<Vec<domain::LedgerId>>,StoreError>`，None表示索引未就绪，不当空库；A4不得依赖尚未批准接口。

- [ ] **Step 1：写表征/测量测试。** `default_candidate_growth_and_ownership_parity`：unmatched、nested、ambiguous、非规范路径、multi-kind、freeze：

```rust
assert_eq!(derived_candidates_after_owner_filter, current_owned_rows);
assert!(cold_global_build_is_reported);
assert!(stale_index_is_rejected);
```

这一步是设计表征，不冒充已失败的产品回归。
- [ ] **Step 2：跑基准。** `cargo test -p store --test browse_reads -- --nocapture`；ignored release测量1k/10k/50k无关ledger，记录当前扫描增长与实测size/写成本。
- [ ] **Step 3：编写ADR方案供批准。** 派生 `browse_ledger_owners`、ownership revision、分批初建/切代、rename/root变更/crash恢复、安全回退；删除/Transfer仍strictfresh判定。比较不改数据、派生索引、持久root_id事实三方案，推荐派生索引，不自动改变ADR-0008权威边界。
- [ ] **Step 4：review门。** 未获批准时DB-07保持open，只提交表征/设计；批准后把迁移/公开测试/rollback拆成新的实施任务再执行，不以此门假称已修。
- [ ] **Step 5：提交。** `git commit -m "docs(database): define default-library ownership optimization gate"`；报告明确design-only。

## 自审与验收

D1→DB-01/02；D2→DB-06；D3→DB-04；D4→DB-05；D5→DB-07设计门；DB-03在A1。每项对spec有覆盖，跨计划接口无store→api依赖。新migration在单一串行schema写域依次递增；retention预览/保护/关闭测试覆盖Review Focus。全workspace与release结果由总计划X1记录。
