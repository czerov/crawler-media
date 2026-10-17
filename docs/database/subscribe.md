# subscribe.db：Subscribe 与用户 Playback 状态

[返回数据库总览](README.md)

当前基线：`main@2c3a5c7`。Store；schema 8；不可随意清空。文件名不代表仅保存 Subscribe。

共 **10** 张代码创建的表；字段、类型、默认值、主键、唯一约束、外键和索引来自当前初始化代码在全新临时目录中执行迁移后的实际 schema。

## 表目录

- [_read_revisions](#_read_revisions) — 2 个字段
- [pending_downloads](#pending_downloads) — 7 个字段
- [playback_logs](#playback_logs) — 17 个字段
- [playback_metrics](#playback_metrics) — 13 个字段
- [playback_session_ids](#playback_session_ids) — 7 个字段
- [playback_sessions](#playback_sessions) — 24 个字段
- [playback_units](#playback_units) — 13 个字段
- [subscribe_facts](#subscribe_facts) — 5 个字段
- [subscribe_wanted](#subscribe_wanted) — 9 个字段
- [subscribes](#subscribes) — 20 个字段

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

**关联触发器**：`trg_subscribe_rev_bump`。触发器负责的 journal/revision 逻辑见总览；不等于声明 FK。

## pending_downloads

已提交给 Downloader、待后续 Transfer/收集的 Torrent 事实；以 Subscribe/enclosure 去重。

主要实现：[crates/store/src/subscribes.rs](../../crates/store/src/subscribes.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `subscribe_id` | `TEXT` | 1 | 是 | 无 | 逻辑引用 subscribe.db.subscribes.id。 |
| `enclosure` | `TEXT` | 2 | 是 | 无 | Torrent 下载入口，参与 pending_downloads 去重键。 |
| `title` | `TEXT` | — | 是 | 无 | Media 名称或 Torrent 标题，随表解释。 |
| `score` | `INTEGER` | — | 是 | 无 | Subscribe 槽位或 pending Torrent 的评分。 |
| `torrent_json` | `TEXT` | — | 是 | 无 | 提交 Torrent 的序列化事实。 |
| `submitted_at` | `INTEGER` | — | 是 | `0` | 提交给 Downloader 的时间，Unix 秒；旧行 0 代表未记录。 |
| `state` | `TEXT` | — | 是 | `'active'` | active（在途）/imported（已 Transfer，仍保留用于下载列表跟踪）。 |

**键与约束**

- 主键：`(subscribe_id, enclosure)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

```sql
CREATE INDEX pending_downloads_sub_state_idx ON pending_downloads(subscribe_id, state);
```

**使用注意**：torrent_json 还保存 site_id、Torrent 下载入口/标题/大小/做种数、实际 Downloader ID，可带 Release override；state=imported 的保留行不等于仍待 Transfer。

## playback_logs

Playback 会话结束/汇总历史，用于历史列表与统计；区别于当前会话和最近位置。

主要实现：[crates/store/src/playback.rs](../../crates/store/src/playback.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `TEXT` | 1 | 否 | 无 | 该表记录主键；UUID 文本为主，具体类型见字段类型。 |
| `user_id` | `TEXT` | — | 是 | 无 | User ID；逻辑引用 app.db.users.id。 |
| `media_id` | `TEXT` | — | 是 | 无 | 规范 Media ID；逻辑引用 app.db.media.id。 |
| `season` | `INTEGER` | — | 否 | 无 | 季号；观看单元 -1 表示整片/作品范围；ledger NULL 表示非分集文件。 |
| `episode` | `INTEGER` | — | 否 | 无 | 集号；观看单元可用 -1 表示整季/整片，ledger NULL 表示非分集文件。 |
| `client` | `TEXT` | — | 否 | 无 | Playback 客户端名称/类型。 |
| `device_name` | `TEXT` | — | 否 | 无 | 客户端提供的可读设备名称。 |
| `play_method` | `TEXT` | — | 是 | `'local'` | 播放方式标签（例如 direct play 等），按协议映射。 |
| `started_at` | `INTEGER` | — | 是 | 无 | 通用 Job/Playback 开始时间，Unix 秒。 |
| `ended_at` | `INTEGER` | — | 是 | 无 | Playback 历史结束时间，Unix 秒。 |
| `watched_ms` | `INTEGER` | — | 是 | `0` | 实际累计观看时间，毫秒，不等于播放位置。 |
| `start_position_ms` | `INTEGER` | — | 是 | `0` | 一次 Playback 会话开始时的位置，毫秒。 |
| `end_position_ms` | `INTEGER` | — | 是 | `0` | Playback 历史结束时的位置，毫秒。 |
| `duration_ms` | `INTEGER` | — | 否 | 无 | Playback 单元/会话的媒体总时长，毫秒。 |
| `completed` | `INTEGER` | — | 是 | `0` | 这次 Playback 历史是否完成（0/1），不是 probe 父 Job 的 completed 计数。 |
| `admin_ended` | `INTEGER` | — | 是 | `0` | 是否由管理员结束；SQLite 0/1。 |
| `device_id` | `TEXT` | — | 否 | 无 | 客户端设备身份；与 User 共同限定会话/撤销范围。 |

**键与约束**

- 主键：`(id)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

```sql
CREATE INDEX playback_logs_ended_idx ON playback_logs(ended_at ASC, started_at ASC, id ASC);
CREATE INDEX playback_logs_started_idx ON playback_logs(started_at DESC, id DESC);
CREATE INDEX playback_logs_user_started_idx ON playback_logs(user_id, started_at);
```

## playback_metrics

QoE 播放质量遥测样本，例如首帧、卡顿、跳转和丢帧；允许按明确保留策略清理。

主要实现：[crates/store/src/playback/logs.rs](../../crates/store/src/playback/logs.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `TEXT` | 1 | 否 | 无 | 该表记录主键；UUID 文本为主，具体类型见字段类型。 |
| `user_id` | `TEXT` | — | 是 | 无 | User ID；逻辑引用 app.db.users.id。 |
| `media_id` | `TEXT` | — | 是 | 无 | 规范 Media ID；逻辑引用 app.db.media.id。 |
| `tier` | `INTEGER` | — | 是 | `0` | Playback QoE 数值分层（INTEGER）；由播放质量上报协议解释。 |
| `engine` | `TEXT` | — | 否 | 无 | 播放器引擎名称/版本标签。 |
| `ttff_ms` | `INTEGER` | — | 否 | 无 | 首帧时间，毫秒。 |
| `rebuffer_ms` | `INTEGER` | — | 是 | `0` | 累计卡顿毫秒。 |
| `rebuffer_count` | `INTEGER` | — | 是 | `0` | 卡顿次数。 |
| `seek_count` | `INTEGER` | — | 是 | `0` | 跳转次数。 |
| `dropped_frames` | `INTEGER` | — | 否 | 无 | 丢帧计数。 |
| `total_frames` | `INTEGER` | — | 否 | 无 | 已报告总帧数。 |
| `watched_ms` | `INTEGER` | — | 是 | `0` | 实际累计观看时间，毫秒，不等于播放位置。 |
| `recorded_at` | `INTEGER` | — | 是 | 无 | QoE 样本记录时间，Unix 秒。 |

**键与约束**

- 主键：`(id)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

```sql
CREATE INDEX playback_metrics_recorded_idx
             ON playback_metrics(recorded_at ASC, id ASC);
```

## playback_session_ids

协议 play_session_id 的签发/代次/状态登记，用于会话验证、重放和撤销边界。

主要实现：[crates/store/src/playback/session_ids.rs](../../crates/store/src/playback/session_ids.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `user_id` | `TEXT` | 1 | 是 | 无 | User ID；逻辑引用 app.db.users.id。 |
| `device_id` | `TEXT` | 2 | 是 | 无 | 客户端设备身份；与 User 共同限定会话/撤销范围。 |
| `play_session_id` | `TEXT` | 3 | 是 | 无 | 客户端/服务端协商的播放会话标识。 |
| `item_id` | `TEXT` | — | 是 | 无 | 当前实现保存具体 LedgerId 字符串（签发时来自 ledger_id），不是泛指任意 Media ID。 |
| `issued_at` | `INTEGER` | — | 是 | 无 | play_session_id 签发时间，Unix 秒。 |
| `generation` | `INTEGER` | — | 是 | 无 | 此 User/设备的 play_session_id 签发代次，防止旧会话事件覆盖新会话。 |
| `state` | `TEXT` | — | 是 | 无 | 签发会话状态：pending/active/closed。 |

**键与约束**

- 主键：`(user_id, device_id, play_session_id)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

```sql
CREATE INDEX playback_session_ids_expiry_idx ON playback_session_ids(issued_at);
```

## playback_sessions

按 User/设备保存当前 Playback 会话与实时计量状态；会话可指向具体 ledger 文件。

主要实现：[crates/store/src/playback/sessions.rs](../../crates/store/src/playback/sessions.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `user_id` | `TEXT` | 1 | 是 | 无 | User ID；逻辑引用 app.db.users.id。 |
| `device_id` | `TEXT` | 2 | 是 | 无 | 客户端设备身份；与 User 共同限定会话/撤销范围。 |
| `media_id` | `TEXT` | — | 是 | 无 | 规范 Media ID；逻辑引用 app.db.media.id。 |
| `season` | `INTEGER` | — | 否 | 无 | 季号；观看单元 -1 表示整片/作品范围；ledger NULL 表示非分集文件。 |
| `episode` | `INTEGER` | — | 否 | 无 | 集号；观看单元可用 -1 表示整季/整片，ledger NULL 表示非分集文件。 |
| `client` | `TEXT` | — | 否 | 无 | Playback 客户端名称/类型。 |
| `device_name` | `TEXT` | — | 否 | 无 | 客户端提供的可读设备名称。 |
| `client_version` | `TEXT` | — | 否 | 无 | Playback 客户端版本。 |
| `play_method` | `TEXT` | — | 是 | `'local'` | 播放方式标签（例如 direct play 等），按协议映射。 |
| `position_ms` | `INTEGER` | — | 是 | `0` | 最近播放位置，毫秒；Jellyfin 输出时另转 100ns ticks。 |
| `start_position_ms` | `INTEGER` | — | 是 | `0` | 一次 Playback 会话开始时的位置，毫秒。 |
| `duration_ms` | `INTEGER` | — | 否 | 无 | Playback 单元/会话的媒体总时长，毫秒。 |
| `paused` | `INTEGER` | — | 是 | `0` | 当前播放是否暂停。 |
| `watched_ms` | `INTEGER` | — | 是 | `0` | 实际累计观看时间，毫秒，不等于播放位置。 |
| `rate_bps` | `INTEGER` | — | 是 | `0` | 会话速率整数；当前字段名为 rate_bps，具体上报单位由 Playback 接入层约定，不凭列名推断 bit/s 与 byte/s。 |
| `bytes_sent` | `INTEGER` | — | 是 | `0` | 当前会话累计发送字节数。 |
| `connections` | `INTEGER` | — | 是 | `0` | 当前会话的连接计数。 |
| `admin_ended` | `INTEGER` | — | 是 | `0` | 是否由管理员结束；SQLite 0/1。 |
| `started_at` | `INTEGER` | — | 是 | 无 | 通用 Job/Playback 开始时间，Unix 秒。 |
| `last_report_at` | `INTEGER` | — | 是 | 无 | 当前会话最后报告时间，Unix 秒。 |
| `ledger_id` | `TEXT` | — | 否 | 无 | 逻辑引用 library.db.ledger.id，除显式声明处外不受 SQLite FK 强制。 |
| `play_session_id` | `TEXT` | — | 否 | 无 | 客户端/服务端协商的播放会话标识。 |
| `last_report_at_ms` | `INTEGER` | — | 是 | `0` | 当前会话最后报告时间，Unix 毫秒；保留与旧秒字段的兼容读写。 |
| `last_event_sequence` | `INTEGER` | — | 否 | 无 | 最近接受的播放事件序号；用于排除乱序旧事件。 |

**键与约束**

- 主键：`(user_id, device_id)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

```sql
CREATE INDEX playback_sessions_user_idx ON playback_sessions(user_id);
```

## playback_units

按 User/Media/季/集保存当前播放位置、已看、收藏、轨道偏好及两类活动时间；不保存文件字节。

主要实现：[crates/store/src/playback/units.rs](../../crates/store/src/playback/units.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `user_id` | `TEXT` | 1 | 是 | 无 | User ID；逻辑引用 app.db.users.id。 |
| `media_id` | `TEXT` | 2 | 是 | 无 | 规范 Media ID；逻辑引用 app.db.media.id。 |
| `season` | `INTEGER` | 3 | 是 | `-1` | -1 表示整片/作品单元；具体季号配 episode=-1 表示整季；0 可是真实 TV 第 0 季。 |
| `episode` | `INTEGER` | 4 | 是 | `-1` | -1 表示整季或整片；具体集号表示分集；历史 movie (0,0) 兼容迁移不能误删 TV (0,0)。 |
| `position_ms` | `INTEGER` | — | 是 | `0` | 最近播放位置，毫秒；Jellyfin 输出时另转 100ns ticks。 |
| `played` | `INTEGER` | — | 是 | `0` | 该行原始显式 played；协议有效值按适用的集/季/整片单元及时间/特定性规则解析。仅收藏行不应视为独立观看断言。 |
| `favorite` | `INTEGER` | — | 是 | `0` | 是否收藏；SQLite 0/1。 |
| `duration_ms` | `INTEGER` | — | 否 | 无 | Playback 单元/会话的媒体总时长，毫秒。 |
| `audio_track` | `TEXT` | — | 否 | 无 | User 选择的音轨标识；不是音轨二进制内容。 |
| `subtitle_track` | `TEXT` | — | 否 | 无 | User 选择的字幕轨标识。 |
| `play_count` | `INTEGER` | — | 是 | `0` | 该观看单元播放次数。 |
| `updated_at` | `INTEGER` | — | 是 | `0` | 播放活动/显式观看标记顺序时间，Unix 秒；收藏排序使用 favorite_at，不复用此字段。 |
| `favorite_at` | `INTEGER` | — | 是 | `0` | 收藏操作时间，Unix 秒；独立于播放活动 updated_at；0 为未知历史时间。 |

**键与约束**

- 主键：`(user_id, media_id, season, episode)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

```sql
CREATE INDEX playback_units_user_media_idx ON playback_units(user_id, media_id);
```

**关联触发器**：`trg_sub_units_on_del`、`trg_sub_units_on_ins`、`trg_sub_units_on_upd`。触发器负责的 journal/revision 逻辑见总览；不等于声明 FK。

**使用注意**：单元键：整片/作品通常 (-1,-1)，整季 (season,-1)，单集 (season,episode)；真实 TV 第 0 季/集与旧电影 (0,0) 兼容行不能混淆。updated_at 与 favorite_at 都是秒；position_ms/duration_ms 是毫秒。有效 Played 需读集/季/整片适用标记，不能直接把原始 played 当协议最终值。

## subscribe_facts

Subscribe 各季/集覆盖槽已拥有文件的路径和评分事实，用于填空与 Wash-cut。

主要实现：[crates/store/src/subscribes.rs](../../crates/store/src/subscribes.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `subscribe_id` | `TEXT` | 1 | 是 | 无 | 逻辑引用 subscribe.db.subscribes.id。 |
| `season` | `INTEGER` | 2 | 是 | `-1` | Subscribe 覆盖槽的季号；电影无季用 -1。 |
| `episode` | `INTEGER` | 3 | 是 | `-1` | Subscribe 覆盖槽的集号；电影无集用 -1。 |
| `score` | `INTEGER` | — | 是 | 无 | Subscribe 槽位或 pending Torrent 的评分。 |
| `path` | `TEXT` | — | 否 | 无 | 文件或根的路径；不能仅凭字符串前缀代替组件级归属检查。 |

**键与约束**

- 主键：`(subscribe_id, season, episode)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

```sql
CREATE INDEX subscribe_facts_sub_idx ON subscribe_facts(subscribe_id);
```

**使用注意**：电影槽位通常 (-1,-1)；TV 为季/集。path 是文件路径逻辑关联，不是 FK；不要把一个槽位直接 JOIN 同一 Media 的所有文件版本后求和。

## subscribe_wanted

Subscribe 待满足槽位的搜索、抓取、导入与拒绝历史状态。

主要实现：[crates/store/src/subscribe_wanted.rs](../../crates/store/src/subscribe_wanted.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `subscribe_id` | `TEXT` | 1 | 是 | 无 | 逻辑引用 subscribe.db.subscribes.id。 |
| `season` | `INTEGER` | 2 | 是 | `-1` | 搜索/抓取履历对应的季槽；无季用 -1，与 subscribe_facts 同构。 |
| `episode` | `INTEGER` | 3 | 是 | `-1` | 搜索/抓取履历对应的集槽；无集用 -1，与 subscribe_facts 同构。 |
| `search_attempts` | `INTEGER` | — | 是 | `0` | 该 Subscribe 槽位累计搜索次数。 |
| `last_search_at` | `INTEGER` | — | 否 | 无 | Subscribe 槽位最近搜索时间，Unix 秒。 |
| `grabbed_at` | `INTEGER` | — | 否 | 无 | Subscribe 槽位首次抓取时间，Unix 秒。 |
| `imported_at` | `INTEGER` | — | 否 | 无 | Subscribe 槽位首次 Transfer 完成时间，Unix 秒。 |
| `grab_title` | `TEXT` | — | 否 | 无 | 已抓取 Torrent 的显示标题。 |
| `last_reject_reason` | `TEXT` | — | 否 | 无 | 上次候选被拒的说明。 |

**键与约束**

- 主键：`(subscribe_id, season, episode)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

```sql
CREATE INDEX subscribe_wanted_sub_idx ON subscribe_wanted(subscribe_id);
```

**使用注意**：该表主要保存履历；是否已满足/已抓取还要结合 subscribe_facts 与 pending_downloads 推导，不存在独立 status 列。季/集无值用 -1。

## subscribes

User 对一个 Media 的 Subscribe 策略：覆盖范围、搜索方式、Filter/Downloader/Library 及 Wash-cut 设置。

主要实现：[crates/store/src/subscribes.rs](../../crates/store/src/subscribes.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `TEXT` | 1 | 否 | 无 | 该表记录主键；UUID 文本为主，具体类型见字段类型。 |
| `user_id` | `TEXT` | — | 是 | 无 | User ID；逻辑引用 app.db.users.id。 |
| `media_id` | `TEXT` | — | 是 | 无 | 规范 Media ID；逻辑引用 app.db.media.id。 |
| `coverage_kind` | `TEXT` | — | 是 | 无 | Subscribe 覆盖类型（movie/tv）；TV 的 season/episode_from/episode_to 描述范围。 |
| `season` | `INTEGER` | — | 否 | 无 | TV Subscribe 目标季号；电影覆盖为 NULL。 |
| `episode_from` | `INTEGER` | — | 否 | 无 | Subscribe 覆盖起始集号。 |
| `episode_to` | `INTEGER` | — | 否 | 无 | Subscribe 覆盖结束集号；NULL 表示无显式上限/由覆盖策略决定。 |
| `fetch_mode` | `TEXT` | — | 是 | 无 | Subscribe 获取 Torrent 的方式（search/rss/both）。 |
| `filter_id` | `TEXT` | — | 是 | 无 | Subscribe 使用的 Filter ID；逻辑引用 app.db.filters.id。 |
| `wash_cut` | `INTEGER` | — | 是 | `0` | Subscribe 是否启用 Wash-cut。 |
| `wash_cut_filter_id` | `TEXT` | — | 否 | 无 | Wash-cut 使用的独立 Filter ID；逻辑引用 app.db.filters.id。 |
| `full_season_pack` | `INTEGER` | — | 是 | `0` | Subscribe 是否约束整季包。 |
| `downloader_id` | `TEXT` | — | 否 | 无 | 逻辑引用 app.db.downloaders.id；NULL 按调用链采用默认配置。 |
| `tracking_state` | `TEXT` | — | 是 | `'active'` | Subscribe 跟随状态标签，默认 active。 |
| `follow_future` | `INTEGER` | — | 是 | `0` | Subscribe 是否跟随未来尚未明确的季/集范围。 |
| `search_interval_secs` | `INTEGER` | — | 是 | `1800` | Subscribe 搜索间隔，秒。 |
| `keep_old_versions` | `INTEGER` | — | 是 | `0` | Wash-cut/Filter 是否保留旧文件版本。 |
| `created_at` | `TEXT` | — | 是 | `''` | 创建时间，RFC3339 UTC 文本。 |
| `updated_at` | `TEXT` | — | 是 | `''` | Subscribe 配置更新时间，RFC3339 UTC 文本。 |
| `library_id` | `TEXT` | — | 否 | 无 | 逻辑引用 app.db.libraries.id；跨 DB 引用没有 SQLite FK。 |

**键与约束**

- 主键：`(id)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

```sql
CREATE INDEX subscribes_media_idx ON subscribes(media_id);
CREATE INDEX subscribes_user_idx ON subscribes(user_id);
```
