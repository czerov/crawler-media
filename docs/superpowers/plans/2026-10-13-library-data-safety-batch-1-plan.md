# 第一批 Library 数据安全 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> 本环境执行映射：上述 `superpowers:` 名称来自计划模板，不代表当前技能已安装。Native 执行使用目录中的 `executing-plans`；如选择 subagent-driven，使用可用 subagent 工具逐任务实施/独立复核，不擅自创建 Agent Teams，不调用不存在的技能。

**Goal:** 修复文件消失/恢复、缺失索引清理和扫描归属边界，确保不误删 Media、用户 Playback 或其他 Library 的事实。

**Architecture:** watcher 只写持久 missing 状态，管理员清理才删除索引；所有操作使用锁外规范化严格 scope、文件观察与提交身份复核。同库派生清理复用 Immediate 事务，scan/verify/prune 共用 Library 工作租约与发布屏障，不引入跨库事务假象。

**Tech Stack:** Rust 2024、Axum 0.8、Tokio、parking_lot、rusqlite 0.37（bundled SQLite）、现有 tempfile/tower 测试；无新增第三方依赖。

**Spec:** [第一批技术设计](<2026-10-13-library-data-safety-batch-1-design.md>)。执行者必须同时阅读设计与本计划；代码基线 `ba82258`。

## Global Constraints

- 本批无 schema 变更、不增加数据库版本、不迁移 Playback。
- watch、verify、missing-rows 和扫描不会删除 Media、Playback 历史/进度/观看/收藏、collection 关联、Subscribe、pending submission 或 Downloader 配置。
- 核验/清理不删除、移动或重写视频、STRM、字幕、NFO、poster、fanart、章节图等磁盘内容。
- 严格归属采用同 MediaKind 的最长唯一根；无匹配不回退 default，等深歧义拒绝操作；不改变旧 browse 的 fallback。
- 只有明确 `NotFound` 才是文件缺失；其他 IO 错误保持原状态。
- 取消 watcher 自动删除 ledger 的行为，包括 STRM 45 秒到期删除。
- 已有路径恢复保留 LedgerId、MediaId、probe 质量和 filter_score；只清 `missing_at`。
- 同库清理使用一个 library.db Immediate 事务；保留执行历史与共享 Media/episode 事实。
- Registry → Store 是唯一允许的嵌套锁顺序；有副作用作业使用 `run_to_completion`，不包“超时后仍后台删除”的等待超时。
- `.rs` 文件硬上限 800 行；生产/测试函数硬上限 120 行。新模块按职责拆分，不借机全面重构。
- TDD 必须在 Store 公共方法、HTTP 路由或公开 watcher event seam；不启动真实网络、Chromium、ffmpeg，不用 sleep 猜测并发完成。
- 每个提交前运行涉及 crate 的 `cargo test -p <crate>`；本批最后 `cargo test --workspace`；当前分支提交，排除用户无关修改。

## Review Focus

- 最后一个文件消失而无 Subscribe：两名 User 的历史/收藏和 Media 必须保留，重启也不能触发 GC（T4）。
- root 存在不等于 NAS 已挂载：PermissionDenied/根不可读不可变成缺失；空 mountpoint 检测限制必须保留在交付说明（T1/T3/T4）。
- 观察后同路径新建 ledger 或文件恢复：旧观测不能删新 LedgerId；恢复不改探测质量（T3/T5）。
- 外层 watch/Video scan 进入嵌套库，或扫描中冻结/改根：不能为别库/旧 scope 发布 ledger 或 sidecar（T3/T5）。
- SQLite 清理中途失败、probe 正在执行：事务回滚且旧 worker 不复活派生行，不能只断言 deleted 数量（T2/T3/T6）。

---

## 0. 文件结构与依赖

实施顺序：`T1 → T2 → T3 → T4 → T5 → T6`。每个任务独立红绿测试与提交；不得把只有类型脚手架、没有可验证行为的改动单独提交。

| 文件 | 责任 |
|---|---|
| 新建 `crates/store/src/library_scope.rs` | 锁外规范化根/路径，严格匹配结果与 scope 配置比较 |
| 新建 `crates/store/src/ledger_cleanup.rs` | library.db 内统一按 LedgerId 删除文件派生事实 |
| 新建 `crates/store/src/ledger_presence.rs` | 带旧 LedgerId/path/media_id 条件的 missing 写入/清理 |
| 新建 `crates/api/src/library_presence.rs` | 文件观察 trait、生产实现、owned 协调作业与结果类型 |
| 新建 `crates/api/src/http/library_presence.rs` | verify/missing-rows HTTP 解析、状态码映射 |
| 新建 `crates/api/src/fs_watcher/presence.rs` | watcher 事件到 missing/恢复的协调；不自动删除 |
| 新建 `crates/api/src/http/library_scan/scope.rs` | 四类扫描共用 owner 筛选与发布检查 |
| 修改 `crates/store/src/{lib,library_ownership,ledger,library}.rs` | 注册/导出接口、复用删除 helper、移走旧 root-only 操作 |
| 修改 `crates/api/src/{lib,fs_watcher,watch_ledger}.rs` | 注册 presence、移除 watcher GC、扫描恢复清标记 |
| 修改 `crates/api/src/management/state.rs` | 文件观察 seam 注入，生产默认真实实现 |
| 修改 `crates/api/src/http/{mod,library,library_scan}.rs` | handler 路由改到新模块，调用扫描 scope |
| 新建下列测试文件 | 分任务详列；fixtures 放相邻支持模块，单函数不超 120 行 |

当前规模：扫描模块 754 行、watcher 686 行、Library handler 652 行。新增逻辑放上述 sibling modules，避免越过 800 行。计划中的尚未存在文件以代码路径列出，实际创建后补文档链接。

## Task 1: 可验证的严格 Library scope

**Files:**
- Create: `crates/store/src/library_scope.rs`
- Modify: [ownership matcher](<../../../crates/store/src/library_ownership.rs>)、[Store exports](<../../../crates/store/src/lib.rs>)
- Test: `crates/store/tests/library_scope.rs`；保持 [旧 ownership 测试](<../../../crates/store/tests/ownership_snapshot.rs>) 不变

**Interfaces:**
- Consumes: `Store::library_ownership() -> Result<LibraryOwnership, StoreError>`、`LibraryOwnership::libraries()`、现有私有 `library_paths::resolved_path`。
- Produces: `LibraryPathScope::resolve(snapshot: LibraryOwnership) -> Result<LibraryPathScope, StoreError>`，必须在 Store 锁外调用；保留原始配置并规范化全部根。
- Produces: `LibraryPathScope::resolve_path(path: &Path) -> Result<PathBuf, StoreError>`，复用现有 resolver，锁外执行。
- Produces: `LibraryPathScope::owner(&self, resolved_path: &Path, kind: MediaKind) -> StrictOwner<'_>`；`StrictOwner<'a> { Owned(&'a Library), Unmatched, Ambiguous }`，Library 的返回值保留原始 root 配置。
- Produces: `LibraryPathScope::matches(&self, current: &LibraryOwnership) -> bool`，比较所有 Library 的 id/kind/root_paths 及 accepts_work；任一根/blocked 变化失败关闭。
- Produces: `LibraryPathScope::accepts_work(&self, library_id: &str) -> bool`；新增类型均由 Store crate 导出。

- [ ] **Step 1: 写公共 scope 行为测试。** 定义 fixture `fn create(store: &Store, kind: MediaKind, roots: &[&Path]) -> store::Library`，调用现有create_library；测试名及断言：
  ```rust
  #[test]
  fn strict_scope_does_not_fall_back_to_default() {
      let tmp = tempfile::tempdir().unwrap();
      let store = store::Store::open(tmp.path().join("data")).unwrap();
      let outside = tmp.path().join("outside/one.mkv");
      let snapshot = store.library_ownership().unwrap();
      assert!(snapshot.owner(&outside, domain::MediaKind::Movie).is_some());
      let scope = store::LibraryPathScope::resolve(snapshot).unwrap();
      let resolved = store::LibraryPathScope::resolve_path(&outside).unwrap();
      assert!(matches!(scope.owner(&resolved, domain::MediaKind::Movie),
          store::StrictOwner::Unmatched));
  }
  ```
  - `strict_scope_does_not_fall_back_to_default`：库外路径为 Unmatched，旧 owner 仍返回 default。
  - `nested_root_wins_and_sibling_prefix_does_not_match`：内层同 kind 胜出；`/owned-other` 不属于 `/owned`。
  - `equal_depth_is_ambiguous`：两 Library 同 root 返回 Ambiguous，不任取一库。
  - `resolved_scope_preserves_symlink_and_missing_descendant_rules`：配置 root 软链接正确匹配、缺失后代仍可判归属；注入/真实非权限依赖错误必须 Err 而非词法 fallback。
  - `scope_is_stale_after_root_kind_or_freeze_change`：改根、换 blocked 状态后 matches=false；未改变的原快照 matches=true。
  - `different_kind_overlap_keeps_existing_kind_contract`：同路径不同 kind 各自匹配相应 kind。
- [ ] **Step 2: 验证红。** `cargo test -p store --test library_scope`；预期新接口未定义导致编译失败。保存输出，不能把构建环境错误当红。
- [ ] **Step 3: 实现上述精确接口。** 复用 matcher，增加 strict outcome；不修改 `owner_with_default` 行为。根规范化一次、每个目标路径规范化一次；纯匹配不触碰磁盘。
- [ ] **Step 4: 验证绿。** `cargo test -p store`；预期全部通过，特别是旧 default fallback 测试。
- [ ] **Step 5: 提交。** 仅暂存本任务文件：`git commit -m "fix(store): add strict validated library scope"`。

## Task 2: 按 LedgerId 原子清理文件派生事实

**Files:**
- Create: `crates/store/src/ledger_cleanup.rs`
- Modify: [ledger 删除](<../../../crates/store/src/ledger.rs#L106-L121>)、[派生 helper](<../../../crates/store/src/library.rs#L439-L455>)、[模块注册](<../../../crates/store/src/lib.rs>)
- Test: `crates/store/tests/ledger_cleanup.rs`

**Interfaces:**
- Consumes: 现有 `delete_file_meta_for_ledger(tx, ledger_id)`、rusqlite Immediate transaction。
- Produces: `pub(crate) fn delete_ledger_in_tx(tx: &rusqlite::Transaction<'_>, id: domain::LedgerId) -> Result<bool, StoreError>`，只处理该行文件派生表与 scrape_sidecars DB 关联。
- Existing preserved: `Store::delete_ledger_path(&self, path: &str) -> Result<(), StoreError>`、`Store::delete_ledger_for_media(&self, media_id: MediaId) -> Result<usize, StoreError>`；两者复用内部 helper。

- [ ] **Step 1: 写删除合同测试。** fixture 建目标 row 和另一 Library 的同 Media row，填充 file_meta/fingerprint cache/attempts/stage/samples/scrape_sidecars，设置两名 User Playback 与共享 marker。`deleting_ledger_removes_only_file_derived_rows` 断言目标派生和 sidecar DB 关联清空、另库缓存/Media/Playback/marker/磁盘 sidecar bytes 不变；`repeated_delete_is_idempotent` 再删无错误。对没有公共读取接口的派生表可用外部 SQLite 连接断言持久行，不断言 SQL 文本。
- [ ] **Step 2: 写事务故障测试。** `derived_delete_failure_rolls_back_ledger_and_caches`：用 SQLite trigger 在删除后续派生表时 RAISE(ABORT)；调用公共删除后断言 Err、ledger 及先前已处理缓存仍在。`media_delete_uses_same_transaction_cleanup` 验证按 Media 删除也复用同规则。
- [ ] **Step 3: 验证红。** `cargo test -p store --test ledger_cleanup`；当前至少 sidecar 关联清理断言失败，确认是目标行为而非 fixture 错误。
- [ ] **Step 4: 实现 helper 并替换两个公共删除入口内部循环。** 不在 helper 删除 Media/Playback/shared markers/probe 执行历史；既有样本清理继续调用现有关联一致性 helper，不写粗粒度 model DELETE。
- [ ] **Step 5: 验证绿。** `cargo test -p store`；全部通过；检查 rollback 实际恢复所有目标行。
- [ ] **Step 6: 提交。** `git commit -m "fix(store): unify transactional ledger derived cleanup"`。

## Task 3: 安全的 verify 与管理员 missing prune

**Files:**
- Create: `crates/store/src/ledger_presence.rs`、`crates/api/src/library_presence.rs`、`crates/api/src/http/library_presence.rs`
- Modify: [Store 注册](<../../../crates/store/src/lib.rs>)、[旧 root-only Store 方法](<../../../crates/store/src/library.rs#L325-L436>)、[ApiState](<../../../crates/api/src/management/state.rs>)、[API 注册](<../../../crates/api/src/lib.rs>)、[HTTP 路由](<../../../crates/api/src/http/mod.rs>)、[旧 handlers](<../../../crates/api/src/http/library.rs#L584-L652>)
- Test: `crates/store/tests/ledger_presence.rs`、`crates/api/tests/library_presence.rs`、`crates/api/tests/support/library_presence.rs`

**Interfaces:**
- Consumes: T1 scope、T2 `delete_ledger_in_tx`、`ApiState::acquire_library_work`、`LibraryWorkLease::publish`、`ProbeManager::cancel_ledger_work`。
- Produces (Store): `LedgerPresenceObservation { pub row: LedgerRow, pub missing_at: Option<i64>, pub presence: FilePresence }`；`FilePresence { Present, Missing, Unknown }`；`LedgerPresenceMode { Verify, Prune }`；`LedgerPresenceResult { pub missing: usize, pub restored: usize, pub deleted: usize, pub skipped: usize }`，Default。
- Produces (Store): `Store::apply_ledger_presence(&self, observations: &[LedgerPresenceObservation], mode: LedgerPresenceMode, now: i64) -> Result<LedgerPresenceResult, StoreError>`。一个 Immediate 事务；每行核对当前 id/path/media_id/missing_at 与旧观测相同，否则 skipped；Verify Missing 仅写首次时间，Present 只清标记；Prune 仅 Missing 且有旧标记才用 T2 删除，Present 清标记，Unknown 不变。
- Produces (API): `pub trait LibraryFileRead: Send + Sync { fn readable_root(&self, path: &Path) -> std::io::Result<()>; fn presence(&self, path: &Path) -> std::io::Result<bool>; }`，`SystemLibraryFileRead` 默认实现；真实 presence 只把 metadata NotFound 转成 Ok(false)。
- Produces (API): `ApiState::with_library_file_read(self, files: Arc<dyn LibraryFileRead>) -> Self`；state 字段 `library_files: Arc<dyn LibraryFileRead>`。
- Produces (API): `pub(crate) fn verify(state: &ApiState, library_id: &str) -> Result<LedgerPresenceResult, PresenceError>` 和 `pub(crate) fn prune(state: &ApiState, library_id: &str, media_id: Option<MediaId>) -> Result<LedgerPresenceResult, PresenceError>`，同步 owned 作业内部调用。
- Produces: `PresenceError { Missing, Deleting, Ambiguous, RootUnavailable, ScopeChanged, Store(StoreError) }`，固定 HTTP code 按设计第4.2节；具体 IO 上下文日志记录，响应不泄露路径/SQL。

- [ ] **Step 1: 写 Store 身份/事务测试。** `stale_observation_does_not_delete_replacement_ledger`、`restored_file_clears_flag_without_replacing_identity`、`unknown_observation_preserves_prior_state`、`prune_rolls_back_whole_batch_on_derived_error`；分别断言新 LedgerId 保留、原 row 全字段除 missing 外不变、unknown 无写入、批内首行也回滚。
- [ ] **Step 2: 写 HTTP 合同测试及支持 fixture。** `PresenceFixture::new() -> Self` 在 Tokio runtime 外建 ApiState，避免真实 probe；提供 router/state/root/library_id/row、`request(method, path) -> (StatusCode, Value)`、`mark_missing(now)`、`seed_user_facts()`；fake files 用 Mutex 状态与 Barrier 控制，不依赖 chmod 在 root 用户下的效果。测试：
  - `restored_missing_row_is_not_pruned`：verify→恢复→DELETE，deleted=0/restored=1、LedgerId不变。
  - `parent_verify_and_prune_do_not_touch_nested_library`：内层 missing/cache 不变；外层只有自己计数。
  - `root_failure_rejects_whole_request_without_writes`：两 root 一坏，409/root_unavailable，无半数更新。
  - `ambiguous_scope_rejects_without_default_fallback`：409/ownership_ambiguous。
  - `file_permission_error_is_skipped_not_missing`：200/skipped=1，旧标记不变。
  - `prune_preserves_other_library_and_user_facts`：仅删除真实缺失目标 row，别库 cache、两名 User、Media、collection、pending 与 sidecar bytes 不变。
  - `presence_routes_keep_admin_and_envelope_contract`：member 403；无库404；坏UUID400；重复DELETE deleted=0；原 missing/restored/deleted 字段存在。
- [ ] **Step 3: 写提交竞态测试。** fake 在最后一次 presence 前用 Barrier 暂停；分别恢复文件、替换 ledger、修改根、启动 Library removal；预期恢复不删、新 ID 不删、改根409 scope_changed、冻结409 deleting。另 `owned_prune_finishes_after_request_is_dropped` 检查已准入作业有明确完成结果，不遗留半事务。
- [ ] **Step 4: 验证红。** `cargo test -p store --test ledger_presence` 和 `cargo test -p api --test library_presence`；预期缺新接口/当前路径行为失败。
- [ ] **Step 5: 实现 Store 提交接口。** 保留现有 clear_ledger_missing；删除不再使用裸 path DELETE。全仓搜索旧 verify_library_files/delete_missing_rows/delete_missing_rows_for_media 调用，迁移到新协调入口；旧 root-only 方法若无调用即移除，相关 store tests 改用公共 observations 合同，不保留可绕过边界的新生产入口。
- [ ] **Step 6: 实现 API 协调及 handler。** 锁内 snapshot→锁外 scope/root/path观察→probe cancel→提交前再次presence→lease.publish/Store核对当前配置→Store事务。HTTP handler 用 `http_work.run_to_completion`；Busy503，其他错误按合同；从旧 Library handler 移走逻辑并改路由，不改变 admin 层。
- [ ] **Step 7: 验证绿。** `cargo test -p store`、`cargo test -p api`；全通过；并发测试必须由 Barrier/Notify 结束，没有长 sleep。
- [ ] **Step 8: 提交。** `git commit -m "fix(library): revalidate scoped missing rows before pruning"`。

## Task 4: watcher 持久标缺失，不再自动删除身份和历史

**Files:**
- Create: `crates/api/src/fs_watcher/presence.rs`、`crates/api/tests/watch_presence.rs`
- Modify: [watcher](<../../../crates/api/src/fs_watcher.rs>)、[STRM 旧行为测试](<../../../crates/api/tests/management/strm_directory_deletion.rs>)、[watcher unit tests](<../../../crates/api/src/fs_watcher_tests.rs>)

**Interfaces:**
- Consumes: T1/T3 scope、LibraryFileRead、Store apply_ledger_presence Verify。
- Produces: `pub(crate) fn observe_event(state: &ApiState, path: &Path) -> Result<store::LedgerPresenceResult, super::super::library_presence::PresenceError>`，在 presence 子模块中实现；只选择唯一活跃且 realtime_watch=true 的 row。该接口不删除 ledger。
- Preserves public: `handle_fs_events(state, tracker, events)` 和 `StrmGraceTracker` 现有公共方法，避免不必要 API 改名。

- [ ] **Step 1: 写公开事件回归。** `last_local_file_missing_keeps_media_and_all_user_facts`、`last_strm_missing_after_45_seconds_keeps_ledger_and_history`：用 no-Subscribe 最后文件 fixture，t=1000事件、t=1045 sweep、重新打开Store；断言 missing_at=1000、原 LedgerId/MediaId、两名User position/favorite/history、collection 保留。
- [ ] **Step 2: 写恢复与异常回归。** `same_url_restore_clears_missing_and_preserves_probe_cache`、`changed_url_restore_invalidates_only_its_file_cache`、`root_unavailable_event_does_not_mark_descendants_missing`、`directory_event_respects_nested_realtime_owner`、`duplicate_missing_event_keeps_first_timestamp`；使用 T3 fake root/permission 状态。外部 rename 测试只断言旧用户事实保留，不假定新文件名能可靠复用身份。
- [ ] **Step 3: 验证红。** `cargo test -p api --test watch_presence`；当前最后文件清理/STRM45秒断言应失败。
- [ ] **Step 4: 替换事件清理与 sweep。** 缺失事件写Verify observation；恢复事件先清 missing 再执行既有STRM URL变化逻辑；删除 cleanup_deleted_ledger 及调用链中的 Media/Playback/collection/pending/marker GC；取消生产自动到期ledger删除。tracker API保留，只维护URL与内存防抖；同步文件事件观察在生产由 blocking作业执行，不把新IO搬进tokio worker。
- [ ] **Step 5: 更新旧45秒测试的合同，不删覆盖。** 45秒后断言仍用原row、不自动重新probe；管理员 prune 之后重建才创建新LedgerId并排probe。增加最后文件与现有 sibling/locked marker 用例；原 intake调度和同URL缓存语义不回退。
- [ ] **Step 6: 验证绿。** `cargo test -p api`；所有watch和管理测试通过；搜索 watcher 中不得剩下delete_playback_for_media/delete_media/delete_collection_items_for_media/delete_imported_pending_for_media调用。
- [ ] **Step 7: 提交。** `git commit -m "fix(watch): preserve identity and history on missing files"`。

## Task 5: 四类扫描统一归属发布与恢复

**Files:**
- Create: `crates/api/src/http/library_scan/scope.rs`、`crates/api/tests/scan_scope.rs`
- Modify: [scan 主模块](<../../../crates/api/src/http/library_scan.rs>)、[登记入口](<../../../crates/api/src/watch_ledger.rs>)、[watcher tests](<../../../crates/api/src/fs_watcher_tests.rs>)

**Interfaces:**
- Consumes: T1 LibraryPathScope、T3 files/readable_root、既有 LibraryWorkLease。
- Produces: `ScanScope::capture(state: &ApiState, library_id: &str) -> Result<ScanScope, crate::library_presence::PresenceError>`；`ScanScope::filter_paths(&self, paths: Vec<PathBuf>) -> Result<Vec<PathBuf>, crate::library_presence::PresenceError>`；`ScanScope::filter_transferred(&self, files: Vec<library::TransferredFile>) -> Result<Vec<library::TransferredFile>, crate::library_presence::PresenceError>`；均在锁外规范化，只保留请求Library同kind owner，歧义为Err。
- Produces: `ScanScope::validate(&self, store: &crate::Store) -> Result<(), crate::library_presence::PresenceError>`，纯配置/blocked复核，无FS IO；在lease.publish内调用。现有publish闭包的外层错误为String：使用 `lease.publish(|| Ok(typed_validation_and_write()))` 保留内层 `Result<T, PresenceError>`；外层屏障拒绝映射Deleting，内层typed错误原样映射，不通过字符串包含关系决定HTTP状态。
- Existing modified behavior: `watch_ledger::record_paths` 与 `record_video_paths` 对确认存在的旧path只 clear_ledger_missing；返回Vec仍仅为新行。

- [ ] **Step 1: 写全量 HTTP 与增量公开事件测试。** `full_movie_scan_skips_nested_independent_library`、`incremental_movie_scan_skips_nested_independent_library`、`full_video_scan_skips_nested_independent_library`、`incremental_video_scan_skips_nested_independent_library`。外层有自己的文件，内层同kind不启用watch；断言只登记外层，内层没有row/probe/新sidecar。测试通过HTTP scan或handle_fs_events触发，不暴露private scan函数只为测试。
- [ ] **Step 2: 写恢复保真测试。** `rescanning_restored_movie_clears_missing_without_overwriting_probe_quality` 与Video对应测试：row已mark missing，恢复后scan、prune deleted=0；LedgerId/MediaId/filter_score/quality/file_meta不变；旧row不额外probe。STRM同URL恢复也不得重新probe。
- [ ] **Step 3: 写冻结/改根竞态测试。** 用T3 fake readable_root的Barrier在遍历前暂停，冻结内层/外层或改变外层root，再继续；旧scope不得发布目标ledger/sidecar，冻结请求返回409，不把失败伪装扫描成功。可共享同模块fixtures，不依赖自动建库扫描抢时序；fixture在runtime外创建。
- [ ] **Step 4: 验证红。** `cargo test -p api --test scan_scope`；现有incremental/Video越界与恢复missing断言失败。
- [ ] **Step 5: 接入四入口并修复恢复。** capture→walk→scope.filter→lease.publish中的validate/record。scan_media只来自已筛选文件，后续识别/Scrape/probe不能重新带入别库行；配置变化错误映射409 library.scope_changed，冻结409 library.deleting。scan主文件新增行不越800，helper放新scope模块。不要顺手实现扫描预算/outbox/格式扩展。
- [ ] **Step 6: 验证绿。** `cargo test -p api`；尤其旧NFO reimport、probe不重复、symlink与nested浏览测试仍通过。
- [ ] **Step 7: 提交。** `git commit -m "fix(scan): enforce scoped publication and clear restored missing flags"`。

## Task 6: 跨路径验收、probe 防复活与发布说明

**Files:**
- Create: `crates/api/tests/library_safety_acceptance.rs`
- Modify if test proves needed: [probe 发布检查](<../../../crates/api/src/probe_manager/probe.rs>)、[probe worker](<../../../crates/api/src/probe_manager/worker.rs>)，只补“ledger仍存在且身份相同”的发布guard，不改probe调度策略
- Modify: [ADR-0012](<../../adr/0012-library-removal.md>)，追加本批watch/verify/prune合同及真实验收记录
- Update: 本设计与本计划的状态/勾选，只在实际完成后

**Interfaces:**
- Consumes: T1–T5全部公共合同、现有fake probe runner/注入seam。
- Produces: 无新生产API；若旧worker缺guard，补私有 `fn ledger_is_current(store: &crate::Store, row: &domain::LedgerRow) -> Result<bool, store::StoreError>`，所有文件派生写入前锁内核对 id/path/media_id，false不发布。

- [ ] **Step 1: 写端到端验收与probe竞态测试。** `verify_restore_scan_prune_preserves_two_users_and_sidecar_bytes` 覆盖设计A3全路径；`prune_cancels_active_probe_and_late_result_does_not_recreate_cache` 用真实已有probe执行流程注入阻塞runner，取消并清理后释放runner，断言ledger/file_meta/stage/cache不复活、执行历史为终态；不能用直接调用私有guard替代。`frozen_library_rejects_presence_and_scan_publication`覆盖并发屏障。
- [ ] **Step 2: 运行并记录结果。** `cargo test -p api --test library_safety_acceptance`；若旧发布检查已满足则可直接绿，不人为修改让测试变红；若失败，只修缺失identity guard，重跑至绿。
- [ ] **Step 3: 完成发布文档。** ADR追加自动删除取消、管理员显式清理、root健康限制、保留用户事实和共享marker的决定；列测试名/命令，不声称真实NAS或VidHub已测。明确本批不恢复此前已经丢失的数据。
- [ ] **Step 4: 全量验证。** 顺序执行 `cargo test -p store`、`cargo test -p api`、`cargo test --workspace`；有任何失败则调查，不以过滤日志替代退出码。若最终修改涉及其他crate，还必须先单独测试对应crate。
- [ ] **Step 5: 尺寸与diff复核。** `wc -l`检查每个触及的.rs≤800；逐个检查新增/修改函数≤120；`git diff --check`、`git diff --stat`、`git status --short`。不暂存预存未跟踪计划或其他用户改动；未改前端无需运行pnpm冒充相关验证。
- [ ] **Step 6: 提交最终验收与文档。** `git commit -m "test(library): verify data safety across missing and scan lifecycles"`；报告实际测试结果、未测NAS边界、行为兼容性变化。

## 自审结果（编写阶段）

- 设计S1–S8分别由T4/T3/T1+T5/T3+T4/T5/T3+T5/T2/T3+T6覆盖。
- A1–A10都有具体测试归属；已补最后文件、恢复后prune、嵌套归属、错误回滚与旧probe发布。
- 所有后续消费接口在前任务定义；FilePresence/StrictOwner/PresenceError职责分开，不混淆unknown与missing。
- 无数据库迁移、第三方依赖、性能重构或第二批业务混入。
- 复核限制：外部FS最后一次检查后的TOCTOU、空mountpoint识别、无别名rename身份恢复未被夸大为已解决。
- 这是开发计划；上述测试未在编写文档时执行，不是绿色验收报告。
