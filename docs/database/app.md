# app.db：实例配置与身份

[返回数据库总览](README.md)

当前基线：`main@2c3a5c7`。Store；schema 7；不可当缓存清空。

共 **18** 张代码创建的表；字段、类型、默认值、主键、唯一约束、外键和索引来自当前初始化代码在全新临时目录中执行迁移后的实际 schema。

## 表目录

- [_browse_changes](#_browse_changes) — 5 个字段
- [_read_revisions](#_read_revisions) — 2 个字段
- [collection_items](#collection_items) — 4 个字段
- [collections](#collections) — 5 个字段
- [downloaders](#downloaders) — 10 个字段
- [filters](#filters) — 4 个字段
- [libraries](#libraries) — 19 个字段
- [library_removal_roots](#library_removal_roots) — 2 个字段
- [library_removals](#library_removals) — 8 个字段
- [library_roots](#library_roots) — 6 个字段
- [media](#media) — 11 个字段
- [naming_templates](#naming_templates) — 2 个字段
- [playback_device_credentials](#playback_device_credentials) — 3 个字段
- [search_snapshots](#search_snapshots) — 5 个字段
- [settings](#settings) — 2 个字段
- [sites](#sites) — 12 个字段
- [user_tokens](#user_tokens) — 3 个字段
- [users](#users) — 5 个字段

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

**关联触发器**：`trg_app_rev_bump`。触发器负责的 journal/revision 逻辑见总览；不等于声明 FK。

## collection_items

集合内的 Media item 引用及其排序。

主要实现：[crates/store/src/collections.rs](../../crates/store/src/collections.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `collection_id` | `TEXT` | 1 | 是 | 无 | 逻辑引用 app.db.collections.id。 |
| `media_item_id` | `TEXT` | 2 | 是 | 无 | 集合保存的条目 ID 文本；由 API/协议 item ID 层解释，无声明 FK，不能不经解析就等同 media.id。 |
| `sort_order` | `INTEGER` | — | 是 | `0` | 该集合内的显式条目顺序。 |
| `added_at` | `TEXT` | — | 是 | 无 | 加入集合时间，RFC3339 UTC 文本。 |

**键与约束**

- 主键：`(collection_id, media_item_id)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

无额外命名索引；主键/UNIQUE 的 SQLite 自动索引不重复列出。

**使用注意**：media_item_id 没有声明 FK。当前业务读取其 ID 列表再由条目解析层处理；做设计关联前先确认协议 ID 是 Media 级还是 ledger 级。

## collections

User 创建的集合定义；不是 Library 定义，也不是 TMDB 的 collection 缓存。

主要实现：[crates/store/src/collections.rs](../../crates/store/src/collections.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `TEXT` | 1 | 否 | 无 | 该表记录主键；UUID 文本为主，具体类型见字段类型。 |
| `user_id` | `TEXT` | — | 是 | 无 | User ID；逻辑引用 app.db.users.id。 |
| `name` | `TEXT` | — | 是 | 无 | 显示名称或内部对象名；依表上下文。 |
| `sort_order` | `INTEGER` | — | 是 | `0` | 显式显示顺序数值。 |
| `created_at` | `TEXT` | — | 是 | 无 | 创建时间，RFC3339 UTC 文本。 |

**键与约束**

- 主键：`(id)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

无额外命名索引；主键/UNIQUE 的 SQLite 自动索引不重复列出。

## downloaders

Downloader 连接配置、启用开关、默认选择与下载路径映射。

主要实现：[crates/store/src/downloaders.rs](../../crates/store/src/downloaders.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `TEXT` | 1 | 否 | 无 | 该表记录主键；UUID 文本为主，具体类型见字段类型。 |
| `name` | `TEXT` | — | 是 | 无 | 显示名称或内部对象名；依表上下文。 |
| `kind` | `TEXT` | — | 是 | 无 | Downloader 实现类别（qBittorrent/Transmission 的序列化标签）。 |
| `url` | `TEXT` | — | 是 | 无 | Site/Downloader 端点 URL，含义由表决定。 |
| `username` | `TEXT` | — | 否 | 无 | Downloader 认证用户名。 |
| `password` | `TEXT` | — | 否 | 无 | 连接外部 Downloader 的密码配置；敏感资料，不是 User 密码散列。 |
| `category` | `TEXT` | — | 否 | 无 | 传给 Downloader 的分类标签。 |
| `is_default` | `INTEGER` | — | 是 | `0` | 是否为默认配置；库/根/Downloader 的约束由其写入者维护。 |
| `path_maps` | `TEXT` | — | 否 | 无 | Downloader 路径映射配置文本（序列化结构），用于外部路径到本机路径转换。 |
| `enabled` | `INTEGER` | — | 是 | `1` | 是否启用；SQLite 0/1。 |

**键与约束**

- 主键：`(id)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

无额外命名索引；主键/UNIQUE 的 SQLite 自动索引不重复列出。

## filters

命名 Filter：Torrent 拒绝/评分规则，以及是否保留旧版本。

主要实现：[crates/store/src/sites.rs](../../crates/store/src/sites.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `TEXT` | 1 | 否 | 无 | 该表记录主键；UUID 文本为主，具体类型见字段类型。 |
| `name` | `TEXT` | — | 是 | 无 | 显示名称或内部对象名；依表上下文。 |
| `atoms_json` | `TEXT` | — | 是 | 无 | Filter 原子条件集合的 JSON 表示。 |
| `keep_old_versions` | `INTEGER` | — | 是 | `0` | Wash-cut/Filter 是否保留旧文件版本。 |

**键与约束**

- 主键：`(id)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

无额外命名索引；主键/UNIQUE 的 SQLite 自动索引不重复列出。

## libraries

Library 定义：类型、显示顺序、访问策略、根相关功能开关与封面设置；具体根路径分存在 library_roots。

主要实现：[crates/store/src/libraries.rs](../../crates/store/src/libraries.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `TEXT` | 1 | 否 | 无 | 该表记录主键；UUID 文本为主，具体类型见字段类型。 |
| `kind` | `TEXT` | — | 是 | 无 | Library 接受的 Media 类型（movie/tv）；路径归属按同 kind 判定。 |
| `name` | `TEXT` | — | 是 | 无 | 显示名称或内部对象名；依表上下文。 |
| `is_default` | `INTEGER` | — | 是 | `0` | 是否为默认配置；库/根/Downloader 的约束由其写入者维护。 |
| `sort_order` | `INTEGER` | — | 是 | `0` | 显式显示顺序数值。 |
| `access_mode` | `TEXT` | — | 是 | `'everyone'` | 访问模式（everyone/selected）；与成员名单、管理员可见性共同决定权限。 |
| `admin_visible` | `INTEGER` | — | 是 | `1` | 管理员浏览是否可见；0/1，不等同于 Library 是否接受后台工作。 |
| `member_ids_json` | `TEXT` | — | 是 | `'[]'` | selected 访问模式允许的 User ID JSON 数组；不是规范化关系表。 |
| `detect_intros` | `INTEGER` | — | 是 | `0` | 是否检测 intro；库配置开关，实际 DEFAULT/NULL 规则见本行约束和配置读取代码。 |
| `enable_fingerprint` | `INTEGER` | — | 是 | `0` | 是否启用指纹分析；实际 DEFAULT/NULL 规则见本行约束。 |
| `cover_path` | `TEXT` | — | 否 | 无 | 显式封面路径/禁用标记；NULL 代表由业务规则选择。 |
| `match_rules_json` | `TEXT` | — | 是 | `'[]'` | Library 路由/匹配规则的 JSON 列表。 |
| `default_filter_id` | `TEXT` | — | 否 | 无 | Library 的默认 Filter ID；逻辑引用 app.db.filters.id。 |
| `realtime_watch` | `INTEGER` | — | 是 | `1` | Library 是否启用实时文件监听。 |
| `generate_thumbnails` | `INTEGER` | — | 是 | `1` | Library 是否生成缩略图。 |
| `extract_chapter_images` | `INTEGER` | — | 是 | `1` | 是否提取章节图片。 |
| `exclude_from_home` | `INTEGER` | — | 是 | `0` | 是否从首页 Library 展示中排除。 |
| `auto_series_collections` | `INTEGER` | — | 是 | `1` | 是否启用该 Library 的自动 TV Media 集合。 |
| `cover_revision` | `INTEGER` | — | 是 | `0` | Library 封面变更修订；用于阻止旧封面结果覆盖新配置。 |

**键与约束**

- 主键：`(id)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

无额外命名索引；主键/UNIQUE 的 SQLite 自动索引不重复列出。

**关联触发器**：`trg_app_libraries_changes_del`、`trg_app_libraries_changes_ins`、`trg_app_libraries_changes_upd`、`trg_app_libraries_on_del`、`trg_app_libraries_on_ins`、`trg_app_libraries_on_upd`。触发器负责的 journal/revision 逻辑见总览；不等于声明 FK。

**使用注意**：根路径不存为本表 JSON，而由 library_roots.library_id 连接。member_ids_json 是 JSON 成员数组，不存在独立 library_members 表。一个 Library 可有多个根；同一路径跨库重叠需运行时消歧，DDL 没有为根路径增加全局唯一限制。

## library_removal_roots

删除边界所需的根路径快照，避免每次边界检查反序列化整个删除快照。

主要实现：[crates/store/src/library_deletion.rs](../../crates/store/src/library_deletion.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `operation_id` | `TEXT` | 1 | 是 | 无 | 逻辑引用同库 library_removals.operation_id；没有声明外键。 |
| `path` | `TEXT` | 2 | 是 | 无 | 文件或根的路径；不能仅凭字符串前缀代替组件级归属检查。 |

**键与约束**

- 主键：`(operation_id, path)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

无额外命名索引；主键/UNIQUE 的 SQLite 自动索引不重复列出。

## library_removals

Library 删除操作的持久状态与快照，用于冻结、排空、删除和异常恢复；不是回收站文件表。

主要实现：[crates/store/src/library_deletion.rs](../../crates/store/src/library_deletion.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `operation_id` | `TEXT` | 1 | 否 | 无 | 扫描/删除操作 ID，具体参照同组父表。 |
| `library_id` | `TEXT` | — | 是 | 无 | 逻辑引用 app.db.libraries.id；跨 DB 引用没有 SQLite FK。 |
| `state` | `TEXT` | — | 是 | 无 | 业务状态文本；按表的状态机解释。 |
| `snapshot_json` | `TEXT` | — | 是 | 无 | 删除期间保存 rows[{ledger_id,path,media_id,season,episode}]；根另在 library_removal_roots；成功后快照可裁为 {"rows":[]}。 |
| `removed_ledger_count` | `INTEGER` | — | 是 | `0` | Library 删除操作已清理台账行数量。 |
| `error` | `TEXT` | — | 否 | 无 | 可选错误说明；不是错误状态的唯一判据。 |
| `created_at_ms` | `INTEGER` | — | 是 | 无 | 创建时间，Unix 毫秒。 |
| `updated_at_ms` | `INTEGER` | — | 是 | 无 | 业务状态最后更新时间，Unix 毫秒。 |

**键与约束**

- 主键：`(operation_id)`。
- UNIQUE：`(library_id)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

无额外命名索引；主键/UNIQUE 的 SQLite 自动索引不重复列出。

## library_roots

Library 的一个根路径配置；当前 Library 根通过 library_id 归组，历史 kind/is_default 字段仍保留。

主要实现：[crates/store/src/libraries.rs](../../crates/store/src/libraries.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `TEXT` | 1 | 否 | 无 | 该表记录主键；UUID 文本为主，具体类型见字段类型。 |
| `kind` | `TEXT` | — | 是 | 无 | 根配置的历史/兼容类型标签（movie/tv）；当前同时通过 library_id 归到 Library。 |
| `path` | `TEXT` | — | 是 | 无 | 文件或根的路径；不能仅凭字符串前缀代替组件级归属检查。 |
| `is_default` | `INTEGER` | — | 是 | `0` | 历史根配置的默认标记；当前默认 Library 由 libraries.is_default 表达。 |
| `library_id` | `TEXT` | — | 否 | 无 | 逻辑引用 app.db.libraries.id；跨 DB 引用没有 SQLite FK。 |
| `sort_order` | `INTEGER` | — | 是 | `0` | 显式显示顺序数值。 |

**键与约束**

- 主键：`(id)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

无额外命名索引；主键/UNIQUE 的 SQLite 自动索引不重复列出。

**关联触发器**：`trg_app_roots_changes_del`、`trg_app_roots_changes_ins`、`trg_app_roots_changes_upd`。触发器负责的 journal/revision 逻辑见总览；不等于声明 FK。

**使用注意**：DDL 中没有 library_id 的外键声明。旧库兼容字段 kind/is_default 与当前 libraries.kind/is_default 同时存在；新查询应按实际调用者的连接规则解释，不能把根的历史默认标记当成当前默认库。

## media

Media 的规范身份与外部来源别名；一部 TV Media 对应作品级身份，季/集在其他表表示。

主要实现：[crates/store/src/media.rs](../../crates/store/src/media.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `TEXT` | 1 | 否 | 无 | Media 内部 UUID 文本主键；外部 TMDB 等 id 只是别名。 |
| `kind` | `TEXT` | — | 是 | 无 | 类别标签；取值随表（Media kind/Downloader kind/probe Job kind/sidecar kind）解释。 |
| `title` | `TEXT` | — | 是 | 无 | Media 名称或 Torrent 标题，随表解释。 |
| `year` | `INTEGER` | — | 否 | 无 | 年份；可空。 |
| `original_title` | `TEXT` | — | 否 | 无 | Media 原语言标题。 |
| `tmdb_id` | `TEXT` | — | 否 | 无 | TMDB 外部身份（TEXT）；类型不同的来源 id 不应当成全局主键。 |
| `douban_id` | `TEXT` | — | 否 | 无 | Douban 外部身份（TEXT）。 |
| `tvdb_id` | `TEXT` | — | 否 | 无 | TVDB 外部身份（TEXT）。 |
| `bangumi_id` | `TEXT` | — | 否 | 无 | Bangumi 外部身份（TEXT）。 |
| `anilist_id` | `TEXT` | — | 否 | 无 | AniList 外部身份（TEXT），不是内部主键。 |
| `aliases_json` | `TEXT` | — | 是 | `'[]'` | Media 搜索/匹配别名的 JSON 字符串数组。 |

**键与约束**

- 主键：`(id)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

```sql
CREATE INDEX media_anilist_idx ON media(anilist_id);
CREATE INDEX media_bangumi_idx ON media(bangumi_id);
CREATE INDEX media_douban_idx ON media(douban_id);
CREATE INDEX media_title_kind_year_idx ON media(title, kind, year);
CREATE INDEX media_tmdb_idx ON media(tmdb_id);
CREATE INDEX media_tvdb_idx ON media(tvdb_id);
```

**关联触发器**：`trg_app_media_changes_del`、`trg_app_media_changes_ins`、`trg_app_media_changes_upd`、`trg_app_media_on_del`、`trg_app_media_on_ins`、`trg_app_media_on_upd`。触发器负责的 journal/revision 逻辑见总览；不等于声明 FK。

## naming_templates

实例级 Naming template；按 Media kind 保存 Transfer 的相对路径模式。

主要实现：[crates/store/src/library.rs](../../crates/store/src/library.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `kind` | `TEXT` | 1 | 否 | 无 | 模板适用 Media 类型（movie/tv），也是主键。 |
| `pattern` | `TEXT` | — | 是 | 无 | Naming template 的占位符路径模式。 |

**键与约束**

- 主键：`(kind)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

无额外命名索引；主键/UNIQUE 的 SQLite 自动索引不重复列出。

## playback_device_credentials

将 Playback 设备与已签发 token 关联，支持按设备吊销；不是播放会话记录。

主要实现：[crates/store/src/playback_devices.rs](../../crates/store/src/playback_devices.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `user_id` | `TEXT` | 1 | 是 | 无 | User ID；逻辑引用 app.db.users.id。 |
| `device_id` | `TEXT` | 2 | 是 | 无 | 客户端设备身份；与 User 共同限定会话/撤销范围。 |
| `token` | `TEXT` | 3 | 是 | 无 | 认证 token；敏感字段。 |

**键与约束**

- 主键：`(user_id, device_id, token)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

无额外命名索引；主键/UNIQUE 的 SQLite 自动索引不重复列出。

## search_snapshots

保存一次搜索的查询与结果快照，供后续读取/操作，而非外部目录缓存表。

主要实现：[crates/store/src/open.rs](../../crates/store/src/open.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `TEXT` | 1 | 否 | 无 | 该表记录主键；UUID 文本为主，具体类型见字段类型。 |
| `vertical` | `TEXT` | — | 是 | 无 | 搜索快照的业务分类。 |
| `query` | `TEXT` | — | 是 | 无 | 搜索查询文本。 |
| `payload` | `TEXT` | — | 是 | 无 | 序列化业务负载/查询结果；字段名不保证所有表都使用相同 JSON 结构。 |
| `created_at` | `INTEGER` | — | 是 | 无 | 搜索快照创建时间（INTEGER，由调用者传入；常规路径使用 Unix 秒）。 |

**键与约束**

- 主键：`(id)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

无额外命名索引；主键/UNIQUE 的 SQLite 自动索引不重复列出。

## settings

实例级 key/value 设置；各配置模块解释 key；也承载维护游标等命名空间。

主要实现：[crates/store/src/open.rs](../../crates/store/src/open.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `key` | `TEXT` | 1 | 否 | 无 | 配置 key；命名空间由对应模块约定。 |
| `value` | `TEXT` | — | 是 | 无 | 设置值文本；可能为标量、JSON 或凭证，按具体 key 解码。 |

**键与约束**

- 主键：`(key)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

无额外命名索引；主键/UNIQUE 的 SQLite 自动索引不重复列出。

**使用注意**：不是只有 UI 参数；还承载命名空间配置、凭证和维护游标。设计评审如要拆分应先盘点 key 的写入者，不能直接把所有 value 当 JSON 或当可删缓存。

## sites

Site 实例配置、认证资料、Indexer profile 与网络/Downloader 选择。字段名列出但本文不读取任何真实凭证值。

主要实现：[crates/store/src/sites.rs](../../crates/store/src/sites.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `TEXT` | 1 | 否 | 无 | 该表记录主键；UUID 文本为主，具体类型见字段类型。 |
| `name` | `TEXT` | — | 是 | 无 | 显示名称或内部对象名；依表上下文。 |
| `url` | `TEXT` | — | 是 | 无 | Site/Downloader 端点 URL，含义由表决定。 |
| `profile_id` | `TEXT` | — | 是 | 无 | Site 对应 Indexer profile ID。 |
| `cookie` | `TEXT` | — | 否 | 无 | Site cookie；敏感认证资料。 |
| `api_key` | `TEXT` | — | 否 | 无 | Site API 凭证；敏感字段，不应输出到日志或文档样例。 |
| `rss_url` | `TEXT` | — | 否 | 无 | Site RSS 入口；可能携带认证 token，应按敏感数据处理。 |
| `proxy` | `TEXT` | — | 否 | 无 | Site 代理配置；可能含认证信息，不应直接展示。 |
| `rate_limit_per_minute` | `INTEGER` | — | 否 | 无 | Site 每分钟请求限额。 |
| `cdp_url` | `TEXT` | — | 否 | 无 | Site 专属 CDP discovery 端点；NULL 时由更高层配置决定回退。 |
| `downloader_id` | `TEXT` | — | 否 | 无 | 逻辑引用 app.db.downloaders.id；NULL 按调用链采用默认配置。 |
| `enabled` | `INTEGER` | — | 是 | `1` | 是否启用；SQLite 0/1。 |

**键与约束**

- 主键：`(id)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

```sql
CREATE INDEX sites_enabled_idx ON sites(enabled);
```

## user_tokens

登录会话/CLI 凭证的 token→User 映射；登录鉴权按签发时间、启用状态与角色校验。

主要实现：[crates/store/src/users.rs](../../crates/store/src/users.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `token` | `TEXT` | 1 | 否 | 无 | 认证 token；敏感字段。 |
| `user_id` | `TEXT` | — | 是 | 无 | User ID；逻辑引用 app.db.users.id。 |
| `created_at` | `INTEGER` | — | 是 | `0` | token 签发时间，Unix 秒；用于登录会话 TTL。 |

**键与约束**

- 主键：`(token)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

```sql
CREATE INDEX user_tokens_user_idx ON user_tokens(user_id);
```

## users

User 登录身份与启用状态；保存密码散列和角色，不保存观看进度。

主要实现：[crates/store/src/users.rs](../../crates/store/src/users.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `TEXT` | 1 | 否 | 无 | 该表记录主键；UUID 文本为主，具体类型见字段类型。 |
| `login` | `TEXT` | — | 是 | 无 | User 登录名。 |
| `enabled` | `INTEGER` | — | 是 | `1` | 是否启用；SQLite 0/1。 |
| `role` | `TEXT` | — | 是 | `'member'` | User 角色（admin/member）。 |
| `password` | `TEXT` | — | 是 | `''` | User 密码散列（Argon2id；旧凭证迁移/兼容逻辑见 users.rs/password.rs），不是 Downloader 连接密码。 |

**键与约束**

- 主键：`(id)`。
- UNIQUE：`(login)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

无额外命名索引；主键/UNIQUE 的 SQLite 自动索引不重复列出。
