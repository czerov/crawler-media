# 修复删集误删兄弟台账并重新探测

> **历史状态（2026-10-13 整理）：** 已有实现的历史修复：90fa56a/7cf5505 保护现存兄弟台账并只为新增行排队；复选框/Expected 保留为原记录，不是本次测试结果。原 workspace 免跑说法不是当前 AGENTS 豁免，本次未补跑也不追认当时已跑。
>
> 本文保留原设计、发现/任务 ID 与当时证据；日期本身不表示过时。旧执行/提交/清理命令仅作历史记录，不应直接重跑；复选框不是当前实现清单。当前依据：[数据库/API review](<../../audits/2026-10-13-database-api-review.md>) 与 [剩余计划](<2026-10-13-database-api-followup.md>)，未覆盖的旧 ID 仍须单独对账。

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 删除一集时只移除这一集的台账和派生缓存；同季仍在磁盘上的剧集保持原 `ledger_id`、流信息和声纹，不被重新 ffprobe 或重新采集声纹。

**Architecture:** 删除事件的前缀匹配现在会把「事件路径下的每一行」都删掉，不看文件是否还在。删 `E08.strm` 后，notify 还会报 `Season 1` 和剧目录被修改；这些目录仍存在，于是前缀匹配把整季台账删掉。`delete_ledger_path` 连带删除 `file_meta` 和 `fingerprint_cache`。随后目录事件触发 `scan_library_subdir`，`record_paths` 把现存文件当成新行插入，`enqueue_probes_for_paths` 再把目录里所有文件送去探测。修两处：级联删除只删除磁盘上已经不存在的路径；目录扫描只为本次新插入的台账行排队。已有的版本化探测缓存不改。TMDB `catalog_cache` 与删剧无关，不碰。

**Tech Stack:** Rust、Tokio、`store` 的 ledger / file_meta / fingerprint_cache。

## Global Constraints

- 不删除、不迁移 `catalog_cache`。TMDB、豆瓣、Bangumi 响应缓存只按自己的 TTL 过期。
- 不改 `enqueue` 已有的 `reuse_media_info_cache = true` 和 `reuse_fingerprint_cache = true`。版本匹配时 `probe_tracks_and_store` 已经短路，本计划不重写这层。
- 目录从回收站还原后仍要重新扫描入账。现有 `crates/api/tests/management/strm_directory_deletion.rs` 必须继续通过。
- strm 内容变化仍走 `invalidate_strm_caches`；不要用「扫描时比较 mtime」再做一套失效。
- 生产函数软上限 60 行、硬上限 120 行；`.rs` 文件硬上限 800 行。
- 先写会失败的测试，再改实现。提交只包含对应任务的代码，不建空提交。

---

### Task 1: 级联删除只删除已经不存在的台账路径

**Problem:** `crates/api/src/fs_watcher.rs` 的 `handle_fs_events` 在 `!path.exists()` 时收集 `r.path == path_str || Path::new(&r.path).starts_with(&path)` 的行，然后对每一行调用 `delete_ledger_path`。单集删除只应命中那一行；父目录事件却会命中目录下所有仍存在的 strm。`StrmGraceTracker` 只在没有命中行时才 `mark_deleted`，所以父目录事件还会绕过 45 秒宽限期，立刻删台账。

**Files:**
- Modify: `crates/api/src/fs_watcher.rs`（删除分支，约 340–388 行）
- Modify: `crates/api/tests/management/strm_directory_deletion.rs`（补单集场景；整目录删除和还原场景保持）

**Interfaces:**
- Consumes: `handle_fs_events(state, tracker, events)`
- Produces: 事件路径不存在时，只删除「路径本身等于事件路径，或位于该路径之下且该台账文件也不存在」的行。存在的兄弟文件不删、不改 `ledger_id`。整目录已经不存在时，其下所有台账行仍删除。

- [x] **Step 1: 写失败测试**

在 `strm_directory_deletion.rs` 增加电视剧场景：

1. 媒体库根下建立 `喜剧之王 (2026)/Season 1/`，写入 `S01E01.strm` 和 `S01E02.strm`，各插入一行 ledger，记下两个 `ledger_id`。
2. 只删除 E02 文件。对同一批事件调用 `handle_fs_events`：E02 文件路径、`Season 1` 目录、剧目录。后两个路径在磁盘上仍存在，用真实的 `path.exists()==true` 走现有「目录仍在」分支；E02 走删除分支。
3. 断言 E02 台账消失，E01 的 `ledger_id` 与删除前相同。
4. 再覆盖整目录删除：删掉剧目录后只发剧目录事件，两行台账都消失。现有电影整目录删除和还原测试不改断言。

不要断言函数内部的 `library_scan_targets`。它是局部变量，测不到；行为断言是 `ledger_id`。

- [x] **Step 2: 跑测试确认失败**

Run: `cargo test -p api --test management strm_directory_deletion -- --test-threads=1`

Expected: 单集场景 FAIL。父目录事件把 E01 也删了，或 E01 被重新插入成另一个 `ledger_id`。

- [x] **Step 3: 改删除条件**

在删除分支收集命中行之后、调用 `delete_ledger_path` 之前过滤：

```rust
fn ledger_path_is_gone(event_path: &Path, ledger_path: &str) -> bool {
    let row_path = Path::new(ledger_path);
    let under_event = ledger_path == event_path.display().to_string()
        || row_path.starts_with(event_path);
    under_event && !row_path.exists()
}
```

只对 `ledger_path_is_gone` 为真的行删除。若过滤后为空，且事件路径是 strm，仍走 `tracker.mark_deleted`。不要在删除分支把父目录塞进 `library_scan_targets`；目录还原继续由「路径存在且是目录」分支触发扫描。

- [x] **Step 4: 跑测试确认通过**

Run: `cargo test -p api --test management strm_directory_deletion -- --test-threads=1`

Expected: PASS，包含原整目录删除、还原，以及新的单集删除。

- [x] **Step 5: 提交**

```bash
git add crates/api/src/fs_watcher.rs crates/api/tests/management/strm_directory_deletion.rs
git commit -m "fix(api): cascade-delete only ledger rows whose files are gone"
```

---

### Task 2: 目录扫描只为新插入的台账行排队

**Problem:** `scan_library_subdir`、全库扫描和 `watch_scrape` 都把 `scan_watch` 返回的全部现存文件交给 `enqueue_probes_for_paths`。`record_paths` 对已有路径直接跳过，但探测入队拿不到这个信息。缓存缺失的旧行仍会由 `enqueue_probe` 补探；缓存完整的旧行会被 `meta_cached && !force_fingerprint` 跳过。真正要停掉的是「父目录一抖，整季重新入队」。

**Files:**
- Modify: `crates/api/src/watch_ledger.rs`（`record_paths`、`record_video_paths`）
- Modify: `crates/api/src/http/library_scan.rs`（`scan_library_subdir`、`scan_library_sync` 的探测入队）
- Modify: `crates/api/src/watch_scrape.rs`（探测入队）
- Test: `crates/api/tests/library_scan_incremental.rs`（新建）

**Interfaces:**
- Consumes: `record_paths(store, files) -> Result<(), String>`
- Produces: `record_paths(store, files) -> Result<Vec<PathBuf>, String>`，只返回本次新插入 ledger 的路径。`record_video_paths` 同样改成返回新路径。调用方只把返回值交给 `enqueue_probes_for_paths`。已有路径不入队，即使它还没有 file_meta。

- [x] **Step 1: 写失败测试**

`library_scan_incremental.rs`：

1. 建一个 realtime 电视库，剧目录里放两集 strm，并先插入对应 ledger。
2. 给第一集写入与当前 `source_version` 匹配的 `file_meta`；第二集不写。
3. 调 `scan_library_subdir`。
4. 断言两行 `ledger_id` 都不变，`probe` 队列里没有这两集。第二集缓存仍缺，也不由这次扫描补排。
5. 再放入第三集 strm，不预插 ledger。再次扫描后，只有第三集入账并进入探测队列。

探测队列用 `state.probe` 已有的查询；不要用固定睡眠猜异步结果。`scan_library_subdir` 本身是同步的。

- [x] **Step 2: 跑测试确认失败**

Run: `cargo test -p api --test library_scan_incremental -- --test-threads=1`

Expected: FAIL。当前扫描会把两集都交给 `enqueue_probes_for_paths`，第一集至少被评估入队，第二集会真正入队。

- [x] **Step 3: 改返回值并收紧调用点**

`record_paths` 在已有路径的 `continue` 分支不收集；只在 `insert_ledger` 成功后 push 路径。`record_video_paths` 同样处理。

`library_scan.rs` 的三处入队和 `watch_scrape.rs` 的一处入队改为使用返回的新路径。不要在扫描里重算 mtime 或 `source_version`。strm URL 变化仍由 `fs_watcher` 的 `on_created_or_modified` 调用 `invalidate_strm_caches`。

- [x] **Step 4: 跑测试确认通过**

Run: `cargo test -p api --test library_scan_incremental --test management -- --test-threads=1`

Expected: PASS。

- [x] **Step 5: 提交**

```bash
git add crates/api/src/watch_ledger.rs crates/api/src/http/library_scan.rs crates/api/src/watch_scrape.rs crates/api/tests/library_scan_incremental.rs
git commit -m "fix(api): probe only ledger rows inserted by a library scan"
```

---

### Task 3: 回归

**Files:**
- Test only.

- [x] **Step 1: 跑受影响测试**

Run: `cargo test -p api --test management --test library_scan_incremental -- --test-threads=1`

Expected: PASS。

- [x] **Step 2: 跑 api crate**

Run: `cargo test -p api`

Expected: PASS。原计划当时写了“不跑 workspace”，但这不是当前 [AGENTS.md](<../../../AGENTS.md>) 的有效豁免：Rust 改动需逐 crate 测试并最终运行 `cargo test --workspace`。此处不追认历史 workspace 已运行；本次仅修正文档，未补跑测试。

- [x] **Step 3: 不额外提交**

测试已绿就停。不创建 `--allow-empty` 提交。
