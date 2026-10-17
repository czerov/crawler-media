# library.db：文件台账、探测/扫描/章节义务与浏览投影

[返回数据库总览](README.md)

当前基线：`main@2c3a5c7`。Store；schema 17；其中 ledger/持久工作不是可弃缓存，browse_* 为可重建投影。

共 **24** 张代码创建的表；字段、类型、默认值、主键、唯一约束、外键和索引来自当前初始化代码在全新临时目录中执行迁移后的实际 schema。

## 表目录

- [_browse_changes](#_browse_changes) — 5 个字段
- [_read_revisions](#_read_revisions) — 2 个字段
- [browse_change_cursors](#browse_change_cursors) — 2 个字段
- [browse_files](#browse_files) — 6 个字段
- [browse_generations](#browse_generations) — 10 个字段
- [browse_media](#browse_media) — 9 个字段
- [chapter_frame_tasks](#chapter_frame_tasks) — 10 个字段
- [file_meta](#file_meta) — 10 个字段
- [fingerprint_attempts](#fingerprint_attempts) — 12 个字段
- [fingerprint_cache](#fingerprint_cache) — 8 个字段
- [fingerprint_model_members](#fingerprint_model_members) — 4 个字段
- [fingerprint_samples](#fingerprint_samples) — 12 个字段
- [fingerprint_season_models](#fingerprint_season_models) — 9 个字段
- [ledger](#ledger) — 20 个字段
- [ledger_probe_obligations](#ledger_probe_obligations) — 3 个字段
- [media_markers](#media_markers) — 10 个字段
- [probe_job_units](#probe_job_units) — 12 个字段
- [probe_jobs](#probe_jobs) — 17 个字段
- [probe_season_comparison_history](#probe_season_comparison_history) — 4 个字段
- [probe_stage_state](#probe_stage_state) — 10 个字段
- [scan_operations](#scan_operations) — 11 个字段
- [scan_seen](#scan_seen) — 5 个字段
- [scrape_sidecars](#scrape_sidecars) — 3 个字段
- [unidentified](#unidentified) — 2 个字段

## _browse_changes

该来源 DB 的持久浏览变更 journal；seq 单调递增，事件带旧/新路径；达到阈值合并为 browse_all，消费者确认后裁剪。

主要实现：[crates/store/src/schema/browse_changes.rs](../../crates/store/src/schema/browse_changes.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `seq` | `INTEGER` | 1 | 否 | 无 | 单调序号；journal 中为 AUTOINCREMENT 事件序号。 |
| `entity_kind` | `TEXT` | — | 是 | 无 | journal 实体类别；例如 media/ledger/library_root/catalog_cache/browse_all。 |
| `entity_key` | `TEXT` | — | 是 | 无 | journal 变更实体键；形式随 entity_kind 变化，browse_all 使用 *。 |
| `old_path` | `TEXT` | — | 否 | 无 | journal 变更前的路径；无路径类实体可为 NULL。 |
| `new_path` | `TEXT` | — | 否 | 无 | journal 变更后的路径；无路径类实体可为 NULL。 |

**键与约束**

- 主键：`(seq)`。
- `seq INTEGER PRIMARY KEY AUTOINCREMENT`，由 SQLite 分配递增序号。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

```sql
CREATE INDEX _browse_changes_coalesced_idx
        ON _browse_changes(entity_kind) WHERE entity_kind = 'browse_all';
CREATE INDEX _browse_changes_seq_idx ON _browse_changes(seq);
```

## _read_revisions

该 DB 的单例读修订计数；由触发器推进，供多来源快照/失效校验。不是跨库原子版本。

主要实现：[crates/store/src/schema/read_revisions.rs](../../crates/store/src/schema/read_revisions.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `INTEGER` | 1 | 否 | 无 | 单例主键，固定为 1（CHECK id = 1）。 |
| `revision` | `INTEGER` | — | 是 | `0` | 单例单调修订号；不是跨 DB 全局事务序号。 |

**键与约束**

- 主键：`(id)`。
- 表级/列级约束：`CHECK(id = 1)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

无额外命名索引；主键/UNIQUE 的 SQLite 自动索引不重复列出。

**关联触发器**：`trg_library_rev_bump`。触发器负责的 journal/revision 逻辑见总览；不等于声明 FK。

## browse_change_cursors

投影消费者分别消费 app/library/catalog 来源 journal 的持久游标。

主要实现：[crates/store/src/browse_changes.rs](../../crates/store/src/browse_changes.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `source` | `TEXT` | 1 | 否 | 无 | journal 来源库标签：app/library/catalog。 |
| `last_seq` | `INTEGER` | — | 是 | `0` | 该来源最后确认消费的 journal seq。 |

**键与约束**

- 主键：`(source)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

无额外命名索引；主键/UNIQUE 的 SQLite 自动索引不重复列出。

## browse_files

浏览投影的文件事实，按 Library/ledger 存储；可重建，不取代权威 ledger。独立 path 列供轻量依赖读取。

主要实现：[crates/store/src/browse_projection.rs](../../crates/store/src/browse_projection.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `library_id` | `TEXT` | 1 | 是 | 无 | 逻辑引用 app.db.libraries.id；跨 DB 引用没有 SQLite FK。 |
| `ledger_id` | `TEXT` | 2 | 是 | 无 | 逻辑引用 library.db.ledger.id，除显式声明处外不受 SQLite FK 强制。 |
| `media_id` | `TEXT` | — | 是 | 无 | 规范 Media ID；逻辑引用 app.db.media.id。 |
| `generation` | `INTEGER` | — | 是 | 无 | 该行所属浏览发布代次，需与本 Library 的 browse_generations 一致。 |
| `facts_json` | `TEXT` | — | 是 | 无 | 版本化浏览事实 JSON；browse_files 保存 FileFacts，browse_media 保存 MediaFacts，当前 facts 版本 3。 |
| `path` | `TEXT` | — | 是 | `''` | 文件或根的路径；不能仅凭字符串前缀代替组件级归属检查。 |

**键与约束**

- 主键：`(library_id, ledger_id)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

```sql
CREATE INDEX browse_files_media_idx ON browse_files(library_id, media_id);
```

**使用注意**：facts_json 当前 FileFacts v3，含 ledger 身份/路径/季集/质量、size/has_stat、created_ns/modified_ns、missing/has_still/sidecars；独立 path 列支持不解析 facts 的依赖读取。此表可重建，不取代 ledger。

## browse_generations

每 Library 浏览投影的 head：发布代次、scope 指纹、ready/dirty 状态、来源修订与 dirty token。

主要实现：[crates/store/src/browse_projection.rs](../../crates/store/src/browse_projection.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `library_id` | `TEXT` | 1 | 否 | 无 | 逻辑引用 app.db.libraries.id；跨 DB 引用没有 SQLite FK。 |
| `generation` | `INTEGER` | — | 是 | 无 | 此 Library 当前浏览投影发布代次；与 browse_media/browse_files.generation 对照。 |
| `scope_signature` | `TEXT` | — | 是 | 无 | 按 Library 类型/根等上下文计算的 scope 指纹；用于判断快照是否仍适用于当前配置。 |
| `state` | `TEXT` | — | 是 | 无 | 浏览发布状态（ready/dirty）；读取还要校验 scope、generation 和 dirty_token。 |
| `refreshed_at_ms` | `INTEGER` | — | 是 | 无 | 浏览投影 head 中的刷新时间，Unix 毫秒；generation=0/dirty 时不应仅凭该字段推断已经成功发布。 |
| `app_revision` | `INTEGER` | — | 是 | 无 | 构建浏览投影时观察到的 app.db 读修订。 |
| `library_revision` | `INTEGER` | — | 是 | 无 | 构建浏览投影时观察到的 library.db 读修订。 |
| `catalog_revision` | `INTEGER` | — | 是 | 无 | 构建浏览投影时观察到的 catalog.db 读修订。 |
| `subscribe_revision` | `INTEGER` | — | 是 | 无 | 构建浏览投影时观察到的 subscribe.db 读修订。 |
| `dirty_token` | `INTEGER` | — | 是 | `0` | 浏览投影失效令牌；构建发布要校验，避免吞掉构建期间的新失效。 |

**键与约束**

- 主键：`(library_id)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

无额外命名索引；主键/UNIQUE 的 SQLite 自动索引不重复列出。

**使用注意**：读取须同时校验 ready 状态、scope_signature、generation、dirty_token 等。generation=0 的 dirty head 可能先于首次发布被创建，不能仅因有一行或有 refreshed_at_ms 就认定已发布。跨库修订列是观察证据，不提供跨文件事务。

## browse_media

浏览投影的 Media 事实与轻候选，包含可索引的排序键；先选择 Media 页再按 ID 取文件 facts。

主要实现：[crates/store/src/browse_pages.rs](../../crates/store/src/browse_pages.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `library_id` | `TEXT` | 1 | 是 | 无 | 逻辑引用 app.db.libraries.id；跨 DB 引用没有 SQLite FK。 |
| `media_id` | `TEXT` | 2 | 是 | 无 | 规范 Media ID；逻辑引用 app.db.media.id。 |
| `generation` | `INTEGER` | — | 是 | 无 | 该行所属浏览发布代次，需与本 Library 的 browse_generations 一致。 |
| `title` | `TEXT` | — | 是 | 无 | 此 Media 的物化名称排序键。 |
| `year` | `INTEGER` | — | 否 | 无 | 年份；可空。 |
| `facts_json` | `TEXT` | — | 是 | 无 | 版本化浏览事实 JSON；browse_files 保存 FileFacts，browse_media 保存 MediaFacts，当前 facts 版本 3。 |
| `added_at_ns` | `INTEGER` | — | 否 | 无 | 物化的 Media 入库排序时间，纳秒；文件创建时间优先、修改时间兜底。 |
| `release_date` | `TEXT` | — | 是 | `''` | 物化上映日期排序键；完整日期优先，年份兜底 YYYY-01-01，再缺为空串。 |
| `candidate_json` | `TEXT` | — | 是 | `''` | 版本化浏览轻候选 JSON（当前版本 2）；用于完整过滤/排序后选页，不含整份文件 facts。 |

**键与约束**

- 主键：`(library_id, media_id)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

```sql
CREATE INDEX browse_media_added_at_idx
             ON browse_media(library_id, added_at_ns, media_id);
CREATE INDEX browse_media_release_date_idx
             ON browse_media(library_id, release_date, media_id);
CREATE INDEX browse_media_title_idx ON browse_media(library_id, title, media_id);
```

**使用注意**：facts_json 当前 MediaFacts v3：media 身份及外部 id、metadata（genres/countries/rating/runtime/language/release_date/scraped）、has_poster/has_backdrop。candidate_json 当前 v2：kind/tmdb_id/title/year/added_at_ns/release_date、rating/runtime/size_bytes、provisional/scraped/missing/hdr、resolutions/genres/countries/language、图片标记和 episode_keys。JSON 不是额外 SQL 列，格式版本独立于 PRAGMA user_version。

## chapter_frame_tasks

持久章节图片提取任务；以 ledger/源版本/目标集合哈希幂等，供有界服务领取和恢复。

主要实现：[crates/store/src/chapter_frame_tasks.rs](../../crates/store/src/chapter_frame_tasks.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `TEXT` | 1 | 否 | 无 | 该表记录主键；UUID 文本为主，具体类型见字段类型。 |
| `ledger_id` | `TEXT` | — | 是 | 无 | 逻辑引用 library.db.ledger.id，除显式声明处外不受 SQLite FK 强制。 |
| `source_version` | `TEXT` | — | 是 | 无 | 文件源版本身份文本；用于拒绝旧结果发布/缓存误复用。 |
| `targets_json` | `TEXT` | — | 是 | 无 | 章节区间数组 [[start_ms,end_ms],...]；worker 在区间中点提取场景帧，不是路径数组。 |
| `targets_hash` | `TEXT` | — | 是 | 无 | 章节图片提取目标集合的哈希，参与幂等键。 |
| `status` | `TEXT` | — | 是 | 无 | 章节图片任务状态：queued/running/completed/failed。 |
| `error` | `TEXT` | — | 否 | 无 | 可选错误说明；不是错误状态的唯一判据。 |
| `created_at_ms` | `INTEGER` | — | 是 | 无 | 创建时间，Unix 毫秒。 |
| `started_at_ms` | `INTEGER` | — | 否 | 无 | probe/章节任务开始时间，Unix 毫秒。 |
| `finished_at_ms` | `INTEGER` | — | 否 | 无 | probe/章节提取尝试的结束时间，Unix 毫秒。 |

**键与约束**

- 主键：`(id)`。
- UNIQUE：`(ledger_id, source_version, targets_hash)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

```sql
CREATE INDEX chapter_frame_tasks_pending_idx
            ON chapter_frame_tasks(status, created_at_ms, id);
```

**使用注意**：targets_json 是序列化二元组数组。队列容量/worker 数不在表里；表负责持久义务，服务实现有界派发。queued/running/completed/failed 是该表状态，与 probe 的 succeeded 不同。ledger_id 在部分工作路径使用去连字符 UUID 文本，关联时须使用同一 ID 编解码约定。

## file_meta

逐 ledger 文件的 probe 结果：视频、音频、字幕、章节及指纹 JSON；带格式/源版本用于复用校验。

主要实现：[crates/store/src/library.rs](../../crates/store/src/library.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `ledger_id` | `TEXT` | 1 | 否 | 无 | 逻辑引用 library.db.ledger.id，除显式声明处外不受 SQLite FK 强制。 |
| `audio_json` | `TEXT` | — | 是 | 无 | 音轨探测结果 JSON。 |
| `subtitle_json` | `TEXT` | — | 是 | 无 | 字幕轨探测结果 JSON。 |
| `tracks_version` | `INTEGER` | — | 是 | `0` | 音视频轨道缓存结构版本。 |
| `source_version` | `TEXT` | — | 否 | 无 | 文件源版本身份文本；用于拒绝旧结果发布/缓存误复用。 |
| `format_duration_ms` | `INTEGER` | — | 否 | 无 | 容器探测得到的时长，毫秒。 |
| `chapters_json` | `TEXT` | — | 否 | 无 | 文件章节标记列表 JSON。 |
| `video_json` | `TEXT` | — | 否 | 无 | 视频轨 probe 事实 JSON；NULL 时可能无可复用的完整探测结果。 |
| `fingerprint_json` | `TEXT` | — | 否 | 无 | 指纹数据 JSON；与采样/算法版本字段配合解释。 |
| `outro_fingerprint_json` | `TEXT` | — | 否 | 无 | 片尾侧指纹数据 JSON。 |

**键与约束**

- 主键：`(ledger_id)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

无额外命名索引；主键/UNIQUE 的 SQLite 自动索引不重复列出。

**关联触发器**：`trg_lib_file_meta_changes_del`、`trg_lib_file_meta_changes_ins`、`trg_lib_file_meta_changes_upd`、`trg_lib_file_meta_on_del`、`trg_lib_file_meta_on_ins`、`trg_lib_file_meta_on_upd`。触发器负责的 journal/revision 逻辑见总览；不等于声明 FK。

## fingerprint_attempts

一次指纹捕获/分析尝试的执行历史、阶段、耗时与错误分类；不同于可复用的样本缓存。

主要实现：[crates/store/src/fingerprint_attempts.rs](../../crates/store/src/fingerprint_attempts.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `attempt_id` | `TEXT` | 1 | 否 | 无 | 指纹尝试主键（TEXT ID）。 |
| `job_id` | `TEXT` | — | 是 | 无 | probe 工作/尝试所属的父 Job ID；此上下文通常引用 library.db.probe_jobs.id，而非 jobs.db.jobs.id。 |
| `ledger_id` | `TEXT` | — | 是 | 无 | 逻辑引用 library.db.ledger.id，除显式声明处外不受 SQLite FK 强制。 |
| `kind` | `TEXT` | — | 是 | 无 | 指纹区段类型：intro/outro。 |
| `window_start_ms` | `INTEGER` | — | 是 | 无 | 指纹采样窗口开始位置，毫秒。 |
| `window_end_ms` | `INTEGER` | — | 是 | 无 | 指纹采样窗口结束位置，毫秒。 |
| `phase` | `TEXT` | — | 是 | 无 | 指纹尝试执行阶段。 |
| `started_at_ms` | `INTEGER` | — | 是 | 无 | probe/章节任务开始时间，Unix 毫秒。 |
| `finished_at_ms` | `INTEGER` | — | 否 | 无 | probe/章节提取尝试的结束时间，Unix 毫秒。 |
| `status` | `TEXT` | — | 是 | 无 | Job/任务/阶段状态文本；按对应执行状态机解释。 |
| `error_kind` | `TEXT` | — | 否 | 无 | 可机器判读的错误分类。 |
| `metrics_json` | `TEXT` | — | 是 | 无 | 捕获/分析诊断指标 JSON。 |

**键与约束**

- 主键：`(attempt_id)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

```sql
CREATE INDEX fingerprint_attempts_job_ledger_idx
            ON fingerprint_attempts(job_id, ledger_id);
```

## fingerprint_cache

逐文件已计算的指纹缓存，按 cache_key/算法版本/时长判断是否可复用。

主要实现：[crates/store/src/fingerprint_cache.rs](../../crates/store/src/fingerprint_cache.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `ledger_id` | `TEXT` | 1 | 否 | 无 | 逻辑引用 library.db.ledger.id，除显式声明处外不受 SQLite FK 强制。 |
| `cache_key` | `TEXT` | — | 是 | 无 | 缓存键；与来源/算法/请求参数共同确定复用身份。 |
| `algorithm_version` | `INTEGER` | — | 是 | 无 | 指纹算法版本；版本不兼容时不可直接复用缓存。 |
| `sample_duration_secs` | `INTEGER` | — | 是 | 无 | 指纹采样目标时长，秒。 |
| `media_duration_ms` | `INTEGER` | — | 否 | 无 | 指纹捕获上下文的媒体时长，毫秒。 |
| `intro_json` | `TEXT` | — | 是 | 无 | intro 指纹/匹配结果 JSON。 |
| `outro_json` | `TEXT` | — | 否 | 无 | outro 指纹/匹配结果 JSON。 |
| `captured_at_ms` | `INTEGER` | — | 是 | 无 | 样本/缓存捕获时间，Unix 毫秒。 |

**键与约束**

- 主键：`(ledger_id)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

无额外命名索引；主键/UNIQUE 的 SQLite 自动索引不重复列出。

## fingerprint_model_members

季模型与样本的多对多成员关系，并保留 ledger/源版本以校验成员身份。

主要实现：[crates/store/src/schema/fingerprint.rs](../../crates/store/src/schema/fingerprint.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `model_id` | `TEXT` | 1 | 是 | 无 | 真实 SQLite FK → fingerprint_season_models.model_id，ON DELETE CASCADE。 |
| `sample_id` | `TEXT` | 2 | 是 | 无 | 指纹样本 ID。 |
| `ledger_id` | `TEXT` | — | 是 | 无 | 逻辑引用 library.db.ledger.id，除显式声明处外不受 SQLite FK 强制。 |
| `source_version` | `TEXT` | — | 是 | 无 | 文件源版本身份文本；用于拒绝旧结果发布/缓存误复用。 |

**键与约束**

- 主键：`(model_id, sample_id)`。
- **声明外键**：`model_id` → `fingerprint_season_models.model_id`；ON DELETE `CASCADE`，ON UPDATE `NO ACTION`。

**索引**

```sql
CREATE INDEX fingerprint_model_members_ledger_idx
            ON fingerprint_model_members(ledger_id);
CREATE INDEX fingerprint_model_members_sample_idx
            ON fingerprint_model_members(sample_id);
```

**使用注意**：只有 model_id 声明了 FK；sample_id、ledger_id、source_version 是应用逻辑引用/校验，不自动由 SQLite 约束。

## fingerprint_samples

可供跨文件/整季比较的指纹样本事实，明确记录源版本、采样窗口和捕获来源 Job。

主要实现：[crates/store/src/fingerprint_samples.rs](../../crates/store/src/fingerprint_samples.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `sample_id` | `TEXT` | 1 | 否 | 无 | 指纹样本 ID。 |
| `ledger_id` | `TEXT` | — | 是 | 无 | 逻辑引用 library.db.ledger.id，除显式声明处外不受 SQLite FK 强制。 |
| `source_version` | `TEXT` | — | 是 | 无 | 文件源版本身份文本；用于拒绝旧结果发布/缓存误复用。 |
| `capture_profile_key` | `TEXT` | — | 是 | 无 | 采样配置键；参与样本复用校验。 |
| `kind` | `TEXT` | — | 是 | 无 | 样本区段类型：intro/outro；属于样本复用键的一部分。 |
| `window_start_ms` | `INTEGER` | — | 是 | 无 | 指纹采样窗口开始位置，毫秒。 |
| `window_end_ms` | `INTEGER` | — | 是 | 无 | 指纹采样窗口结束位置，毫秒。 |
| `pcm_duration_ms` | `INTEGER` | — | 否 | 无 | 实际采集的 PCM 时长，毫秒。 |
| `fingerprint_json` | `TEXT` | — | 是 | 无 | 指纹数据 JSON；与采样/算法版本字段配合解释。 |
| `captured_job_id` | `TEXT` | — | 是 | 无 | 产生样本的 probe Job ID；清理历史时用于引用保护。 |
| `captured_at_ms` | `INTEGER` | — | 是 | 无 | 样本/缓存捕获时间，Unix 毫秒。 |
| `metrics_json` | `TEXT` | — | 是 | 无 | 捕获/分析诊断指标 JSON。 |

**键与约束**

- 主键：`(sample_id)`。
- UNIQUE：`(ledger_id, source_version, capture_profile_key, kind, window_start_ms, window_end_ms)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

```sql
CREATE INDEX fingerprint_samples_captured_job_idx
             ON fingerprint_samples(captured_job_id);
CREATE INDEX fingerprint_samples_lookup_idx
            ON fingerprint_samples(ledger_id, kind, source_version, capture_profile_key);
```

## fingerprint_season_models

按 Media/季/intro-outro 类型生成的指纹模型及其成员/策略版本。

主要实现：[crates/store/src/fingerprint_models.rs](../../crates/store/src/fingerprint_models.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `model_id` | `TEXT` | 1 | 否 | 无 | 季指纹模型 ID。 |
| `media_id` | `TEXT` | — | 是 | 无 | 规范 Media ID；逻辑引用 app.db.media.id。 |
| `season` | `INTEGER` | — | 是 | 无 | 此指纹模型适用的 TV 季号。 |
| `kind` | `TEXT` | — | 是 | 无 | 模型区段类型：intro/outro。 |
| `model_version` | `INTEGER` | — | 是 | 无 | 模型结构/算法版本。 |
| `membership_key` | `TEXT` | — | 是 | 无 | 整季模型所用样本成员集合的身份键。 |
| `policy_key` | `TEXT` | — | 是 | 无 | 指纹模型的策略版本/身份键。 |
| `model_json` | `TEXT` | — | 是 | 无 | 版本化指纹模型 JSON。 |
| `created_at_ms` | `INTEGER` | — | 是 | 无 | 创建时间，Unix 毫秒。 |

**键与约束**

- 主键：`(model_id)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

```sql
CREATE INDEX fingerprint_season_models_lookup_idx
            ON fingerprint_season_models(media_id, season, kind);
```

## ledger

Library 的权威文件台账：一行对应一个实际路径，可有同一 Media 的多集/多版本。Media 身份与 Library 配置不在此 DB。

主要实现：[crates/store/src/ledger.rs](../../crates/store/src/ledger.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `TEXT` | 1 | 否 | 无 | 该表记录主键；UUID 文本为主，具体类型见字段类型。 |
| `media_id` | `TEXT` | — | 是 | 无 | 规范 Media ID；逻辑引用 app.db.media.id。 |
| `root_id` | `TEXT` | — | 否 | 无 | 可空历史预留列；当前未作为已填充的权威归属，不能据此直接可靠 JOIN 到 Library。 |
| `path` | `TEXT` | — | 是 | 无 | 唯一文件路径（通常绝对路径）；当前归属根据该路径和 Media kind 动态解析。 |
| `season` | `INTEGER` | — | 否 | 无 | 季号；观看单元 -1 表示整片/作品范围；ledger NULL 表示非分集文件。 |
| `episode` | `INTEGER` | — | 否 | 无 | 集号；观看单元可用 -1 表示整季/整片，ledger NULL 表示非分集文件。 |
| `resolution` | `TEXT` | — | 否 | 无 | 分辨率标签。 |
| `codec` | `TEXT` | — | 否 | 无 | 视频编码文本，例如 H264/HEVC；与具体写入者的规范化规则一致。 |
| `hdr` | `TEXT` | — | 否 | 无 | HDR 类型/标记文本；NULL 表示未知或无记录。 |
| `quality_source` | `TEXT` | — | 是 | 无 | 台账质量来源（probe/release）。 |
| `confidence` | `TEXT` | — | 是 | 无 | Release/识别质量置信度文本（例如 high/low）。 |
| `filter_score` | `INTEGER` | — | 否 | 无 | 台账文件的 Filter/Release 评分；不是 SQL 统计分数。 |
| `transfer_mode` | `TEXT` | — | 否 | 无 | Transfer 模式（hardlink/copy/move 等）。 |
| `missing_at` | `INTEGER` | — | 否 | 无 | 文件缺失标记时间，Unix 秒；NULL 表示未标记缺失。 |
| `source_path` | `TEXT` | — | 否 | 无 | Transfer 的来源文件路径。 |
| `transfer_pending` | `INTEGER` | — | 是 | `0` | 是否为尚未完成持久化收尾的 Transfer 预留行；不能直接当作可用文件。 |
| `release_quality` | `TEXT` | — | 否 | 无 | StoredQuality 的 JSON：resolution/source/codec/hdr 等 Release 后备质量字段，probe 结果优先。 |
| `source_version_ms` | `INTEGER` | — | 是 | `0` | 台账长期保存的上次处理源修改时间，毫秒。 |
| `source_version_size` | `INTEGER` | — | 是 | `0` | 台账长期保存的上次处理源字节数。 |
| `source_version_digest` | `TEXT` | — | 否 | 无 | 台账长期保存的源内容摘要（主要补充 STRM 内容身份）。 |

**键与约束**

- 主键：`(id)`。
- UNIQUE：`(path)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

```sql
CREATE INDEX ledger_media_id_idx ON ledger(media_id);
CREATE INDEX ledger_path_idx ON ledger(path);
```

**关联触发器**：`trg_lib_ledger_changes_del`、`trg_lib_ledger_changes_ins`、`trg_lib_ledger_changes_upd`、`trg_lib_ledger_on_del`、`trg_lib_ledger_on_ins`、`trg_lib_ledger_on_upd`。触发器负责的 journal/revision 逻辑见总览；不等于声明 FK。

**使用注意**：注意：当前无 `kind` / `library_id` 列；`root_id` 不是已回填的归属事实。Media kind 要从 app.db.media 读取。文件归属用路径、同 kind 根集合与最长唯一匹配规则计算；Transfer 预留/缺失也不能当可播放文件。

## ledger_probe_obligations

scan 将 ledger 写入后必须触发 probe 的持久义务；与台账同库，防止进程崩溃丢掉后续派发。

主要实现：[crates/store/src/scan_probe_obligations.rs](../../crates/store/src/scan_probe_obligations.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `ledger_id` | `TEXT` | 1 | 否 | 无 | 逻辑引用 library.db.ledger.id，除显式声明处外不受 SQLite FK 强制。 |
| `created_at_ms` | `INTEGER` | — | 是 | 无 | 创建时间，Unix 毫秒。 |
| `reason` | `TEXT` | — | 是 | 无 | 持久 probe 义务的产生原因。 |

**键与约束**

- 主键：`(ledger_id)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

```sql
CREATE INDEX ledger_probe_obligations_created_ledger_idx
            ON ledger_probe_obligations(created_at_ms, ledger_id);
```

## media_markers

按 Media/季/集保存 intro/outro 时间段及用户锁定信息；不同文件版本可共享同一标记单元。

主要实现：[crates/store/src/library/markers.rs](../../crates/store/src/library/markers.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `media_id` | `TEXT` | 1 | 是 | 无 | 规范 Media ID；逻辑引用 app.db.media.id。 |
| `season` | `INTEGER` | 2 | 是 | 无 | 季号；观看单元 -1 表示整片/作品范围；ledger NULL 表示非分集文件。 |
| `episode` | `INTEGER` | 3 | 是 | 无 | 集号；观看单元可用 -1 表示整季/整片，ledger NULL 表示非分集文件。 |
| `intro_start_ms` | `INTEGER` | — | 否 | 无 | intro 开始位置，毫秒。 |
| `intro_end_ms` | `INTEGER` | — | 否 | 无 | intro 结束位置，毫秒。 |
| `outro_start_ms` | `INTEGER` | — | 否 | 无 | outro 开始位置，毫秒。 |
| `outro_end_ms` | `INTEGER` | — | 否 | 无 | outro 结束位置，毫秒。 |
| `source` | `TEXT` | — | 是 | 无 | Metadata source 或标记产生来源；随表上下文。 |
| `locked` | `INTEGER` | — | 是 | `0` | 标记是否由用户锁定（禁止自动检测覆盖）。 |
| `updated_at` | `INTEGER` | — | 是 | 无 | 标记更新时间，Unix 秒；intro/outro 的位置字段本身则为毫秒。 |

**键与约束**

- 主键：`(media_id, season, episode)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

无额外命名索引；主键/UNIQUE 的 SQLite 自动索引不重复列出。

**关联触发器**：`trg_lib_markers_changes_del`、`trg_lib_markers_changes_ins`、`trg_lib_markers_changes_upd`、`trg_lib_markers_on_del`、`trg_lib_markers_on_ins`、`trg_lib_markers_on_upd`。触发器负责的 journal/revision 逻辑见总览；不等于声明 FK。

## probe_job_units

一个 probe Job 对一个 ledger 文件的工作单元；保存派发阶段、采样计划和检测结果。

主要实现：[crates/store/src/probe_tasks.rs](../../crates/store/src/probe_tasks.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `job_id` | `TEXT` | 1 | 是 | 无 | 真实 SQLite FK → probe_jobs.id，ON DELETE CASCADE。 |
| `ledger_id` | `TEXT` | 2 | 是 | 无 | 逻辑引用 library.db.ledger.id，除显式声明处外不受 SQLite FK 强制。 |
| `kind` | `TEXT` | — | 是 | 无 | 类别标签；取值随表（Media kind/Downloader kind/probe Job kind/sidecar kind）解释。 |
| `force_fingerprint` | `INTEGER` | — | 是 | `0` | 该 probe 单元是否强制指纹捕获/分析。 |
| `reuse_fingerprint_cache` | `INTEGER` | — | 是 | `0` | 是否允许复用已存在的指纹捕获缓存。 |
| `overwrite_markers` | `INTEGER` | — | 是 | `0` | 是否允许这次工作覆盖现有自动标记；用户锁定仍由业务门控保护。 |
| `status` | `TEXT` | — | 是 | 无 | 单元状态：queued/running/succeeded/failed/partial/cancelled。 |
| `error` | `TEXT` | — | 否 | 无 | 可选错误说明；不是错误状态的唯一判据。 |
| `reuse_media_info_cache` | `INTEGER` | — | 是 | `0` | 是否允许复用文件 MediaInfo/轨道探测缓存。 |
| `stage` | `TEXT` | — | 是 | `'metadata'` | 派发窗口类别：metadata/fingerprint；与 probe_stage_state 的 metadata/intro/outro 事实阶段不同。 |
| `sampling_plan_json` | `TEXT` | — | 否 | 无 | 持久化采样计划 JSON。 |
| `detection_outcome_json` | `TEXT` | — | 否 | 无 | probe 检测结果/诊断的 JSON 表示。 |

**键与约束**

- 主键：`(job_id, ledger_id)`。
- **声明外键**：`job_id` → `probe_jobs.id`；ON DELETE `CASCADE`，ON UPDATE `NO ACTION`。

**索引**

```sql
CREATE UNIQUE INDEX probe_job_units_active_ledger_idx
            ON probe_job_units(ledger_id) WHERE status IN ('queued', 'running');
CREATE INDEX probe_job_units_dispatch_idx
             ON probe_job_units(stage, status, job_id)
             WHERE status = 'queued';
CREATE INDEX probe_job_units_job_status_idx
            ON probe_job_units(job_id, status);
```

**使用注意**：stage 只选择 metadata/fingerprint 派发窗口；probe_stage_state 则记录 metadata/intro/outro 事实与退避。活动 ledger 唯一约束是 WHERE status IN (queued,running) 的部分唯一索引，而非所有历史行唯一。

## probe_jobs

Media/季 probe 的父 Job：作用域、生命周期、结果计数及分析策略。不是 jobs.db 的通用 Job 队列。

主要实现：[crates/store/src/probe_tasks.rs](../../crates/store/src/probe_tasks.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `TEXT` | 1 | 否 | 无 | 该表记录主键；UUID 文本为主，具体类型见字段类型。 |
| `kind` | `TEXT` | — | 是 | 无 | 父 probe 工作类型，例如 media_probe/fingerprint_probe/marker_refresh；不是通用 jobs.db.JobKind。 |
| `media_id` | `TEXT` | — | 是 | 无 | 规范 Media ID；逻辑引用 app.db.media.id。 |
| `season` | `INTEGER` | — | 否 | 无 | probe 父工作的可选季范围；NULL 表示不限定季或非分集 Media。 |
| `scope_key` | `TEXT` | — | 是 | 无 | 活跃父 probe 的作用域去重键；部分唯一索引只覆盖 queued/running。 |
| `status` | `TEXT` | — | 是 | 无 | probe 父工作状态；queued/running 与 succeeded/failed/partial/cancelled 等终态由 probe_tasks.rs 决定。 |
| `total` | `INTEGER` | — | 是 | 无 | probe Job 总单元数。 |
| `completed` | `INTEGER` | — | 是 | `0` | 终态单元数，含成功/失败/部分成功/取消；守恒是 completed=succeeded+failed+partial+取消单元数，父表无 cancelled 计数列。 |
| `succeeded` | `INTEGER` | — | 是 | `0` | probe Job 成功单元计数。 |
| `failed` | `INTEGER` | — | 是 | `0` | probe Job 失败单元计数。 |
| `partial` | `INTEGER` | — | 是 | `0` | probe Job 部分成功单元计数。 |
| `error` | `TEXT` | — | 否 | 无 | 可选错误说明；不是错误状态的唯一判据。 |
| `created_at_ms` | `INTEGER` | — | 是 | 无 | 创建时间，Unix 毫秒。 |
| `started_at_ms` | `INTEGER` | — | 否 | 无 | probe/章节任务开始时间，Unix 毫秒。 |
| `finished_at_ms` | `INTEGER` | — | 否 | 无 | probe/章节提取尝试的结束时间，Unix 毫秒。 |
| `analysis_policy_key` | `TEXT` | — | 否 | 无 | 此次分析所用策略身份/版本键。 |
| `sampling_mode` | `TEXT` | — | 否 | 无 | 此次分析采样模式标签。 |

**键与约束**

- 主键：`(id)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

```sql
CREATE UNIQUE INDEX probe_jobs_active_scope_idx
            ON probe_jobs(scope_key) WHERE status IN ('queued', 'running');
CREATE INDEX probe_jobs_media_created_idx
            ON probe_jobs(media_id, created_at_ms DESC);
CREATE INDEX probe_jobs_scope_terminal_finished_idx
             ON probe_jobs(scope_key, finished_at_ms DESC, id DESC)
             WHERE status IN ('succeeded', 'failed', 'cancelled');
CREATE INDEX probe_jobs_terminal_finished_idx
             ON probe_jobs(finished_at_ms ASC, id ASC)
             WHERE status IN ('succeeded', 'failed', 'cancelled');
```

**使用注意**：父表 completed 含取消单元，只有 succeeded/failed/partial 三个结果计数列，取消数须从单元状态或差额解释。probe_jobs 与 jobs.db.jobs 是两套不同粒度的持久状态，不允许只凭列名 job_id 混 JOIN。

## probe_season_comparison_history

记录某个 Media/季已经发布过的比较摘要，避免相同输入重复整季比较。

主要实现：[crates/store/src/probe_state.rs](../../crates/store/src/probe_state.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `media_id` | `TEXT` | 1 | 是 | 无 | 规范 Media ID；逻辑引用 app.db.media.id。 |
| `season` | `INTEGER` | 2 | 是 | 无 | 整季比较适用的 TV 季号。 |
| `comparison_digest` | `TEXT` | — | 是 | 无 | 整季比较输入摘要；用于避免重复比较。 |
| `published_at_ms` | `INTEGER` | — | 是 | 无 | 整季比较发布时刻，Unix 毫秒。 |

**键与约束**

- 主键：`(media_id, season)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

无额外命名索引；主键/UNIQUE 的 SQLite 自动索引不重复列出。

## probe_stage_state

每 ledger/context/事实阶段的持久状态、失败次数和重试截止时间；派发窗口不能替代此状态机。

主要实现：[crates/store/src/probe_state.rs](../../crates/store/src/probe_state.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `ledger_id` | `TEXT` | 1 | 是 | 无 | 逻辑引用 library.db.ledger.id，除显式声明处外不受 SQLite FK 强制。 |
| `context_key` | `TEXT` | 2 | 是 | 无 | probe 源/策略上下文键；相同 ledger 的不同上下文有独立阶段状态。 |
| `stage` | `TEXT` | 3 | 是 | 无 | 事实阶段：metadata/intro/outro；记录独立成功/失败和退避。 |
| `status` | `TEXT` | — | 是 | 无 | 阶段状态：pending/queued/running/succeeded/failed/not_applicable/cancelled。 |
| `failure_count` | `INTEGER` | — | 是 | `0` | 自初始化或显式 reset 以来累计记录的阶段失败次数；成功不自动清零，用于重试上限/退避。 |
| `next_retry_at_ms` | `INTEGER` | — | 否 | 无 | 该阶段允许自动重试的最早时间，Unix 毫秒。 |
| `error_kind` | `TEXT` | — | 否 | 无 | 可机器判读的错误分类。 |
| `error` | `TEXT` | — | 否 | 无 | 可选错误说明；不是错误状态的唯一判据。 |
| `active_job_id` | `TEXT` | — | 否 | 无 | 当前占用该阶段的 probe Job ID；逻辑引用 library.db.probe_jobs.id。 |
| `updated_at_ms` | `INTEGER` | — | 是 | 无 | 业务状态最后更新时间，Unix 毫秒。 |

**键与约束**

- 主键：`(ledger_id, context_key, stage)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

```sql
CREATE INDEX probe_stage_retry_idx
    ON probe_stage_state (status, next_retry_at_ms)
    WHERE status = 'failed' AND next_retry_at_ms IS NOT NULL;
```

## scan_operations

Library 扫描操作的作用域、代次、dirty/consumed 进度及报告，支撑断点与单飞合并。

主要实现：[crates/store/src/scan_operations.rs](../../crates/store/src/scan_operations.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `TEXT` | 1 | 否 | 无 | 该表记录主键；UUID 文本为主，具体类型见字段类型。 |
| `library_id` | `TEXT` | — | 是 | 无 | 逻辑引用 app.db.libraries.id；跨 DB 引用没有 SQLite FK。 |
| `scope_signature` | `TEXT` | — | 是 | 无 | 按 Library 类型/根等上下文计算的 scope 指纹；用于判断快照是否仍适用于当前配置。 |
| `status` | `TEXT` | — | 是 | 无 | 扫描状态：queued/running/yielded/completed/partial/cancelled/failed。 |
| `generation` | `INTEGER` | — | 是 | 无 | 此 Library 的扫描操作代次，用于区分恢复/重排后的扫描工作。 |
| `created_at_ms` | `INTEGER` | — | 是 | 无 | 创建时间，Unix 毫秒。 |
| `report_json` | `TEXT` | — | 是 | `'{}'` | 扫描报告/计数 JSON。 |
| `targets_json` | `TEXT` | — | 是 | `'[]'` | 增量扫描目标路径列表 JSON；与 scope_kind/dirty_rev/consumed_rev 一起解释。 |
| `scope_kind` | `TEXT` | — | 是 | `'full'` | full/targeted；空 targets_json 不能独自决定扫描意图。 |
| `dirty_rev` | `INTEGER` | — | 是 | `0` | scan operation 收到的最新脏修订，用于合并增量目标。 |
| `consumed_rev` | `INTEGER` | — | 是 | `0` | 该 scan operation 已消费的脏修订。 |

**键与约束**

- 主键：`(id)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

```sql
CREATE INDEX scan_operations_lib_status_idx
             ON scan_operations (library_id, status);
```

## scan_seen

一次扫描操作已经处理的文件版本检查点；不等于跨扫描长期文件身份，后者在 ledger.source_version_*。

主要实现：[crates/store/src/scan_seen.rs](../../crates/store/src/scan_seen.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `operation_id` | `TEXT` | 1 | 是 | 无 | 逻辑引用 library.db.scan_operations.id；无声明外键。 |
| `path` | `TEXT` | 2 | 是 | 无 | 文件或根的路径；不能仅凭字符串前缀代替组件级归属检查。 |
| `mtime_ms` | `INTEGER` | — | 是 | `0` | 扫描检查点的文件修改时间，毫秒。 |
| `file_size` | `INTEGER` | — | 是 | `0` | 扫描检查点的文件字节数。 |
| `content_digest` | `TEXT` | — | 否 | 无 | 受限内容摘要；STRM 的 URL 内容变化不能仅用 size/mtime 判断。 |

**键与约束**

- 主键：`(operation_id, path)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

无额外命名索引；主键/UNIQUE 的 SQLite 自动索引不重复列出。

**使用注意**：这是单个 operation 的检查点，终态可清；跨 operation 的源版本在 ledger.source_version_*。path 必须和 operation_id 一起使用，不能当全局唯一文件身份。

## scrape_sidecars

Scrape 产生的 NFO/图片等 sidecar 路径索引，与 ledger 文件关联。

主要实现：[crates/store/src/library.rs](../../crates/store/src/library.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `ledger_id` | `TEXT` | 1 | 是 | 无 | 逻辑引用 library.db.ledger.id，除显式声明处外不受 SQLite FK 强制。 |
| `kind` | `TEXT` | 2 | 是 | 无 | 类别标签；取值随表（Media kind/Downloader kind/probe Job kind/sidecar kind）解释。 |
| `path` | `TEXT` | — | 是 | 无 | 文件或根的路径；不能仅凭字符串前缀代替组件级归属检查。 |

**键与约束**

- 主键：`(ledger_id, kind)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

无额外命名索引；主键/UNIQUE 的 SQLite 自动索引不重复列出。

## unidentified

未达到识别置信度、待用户处理的路径。

主要实现：[crates/store/src/library.rs](../../crates/store/src/library.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `path` | `TEXT` | 1 | 否 | 无 | 文件或根的路径；不能仅凭字符串前缀代替组件级归属检查。 |
| `confidence` | `TEXT` | — | 是 | 无 | Release/识别质量置信度文本（例如 high/low）。 |

**键与约束**

- 主键：`(path)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

无额外命名索引；主键/UNIQUE 的 SQLite 自动索引不重复列出。
