# catalog.db：Metadata source 原始响应缓存

[返回数据库总览](README.md)

当前基线：`main@2c3a5c7`。Store；schema 3；允许按缓存策略重抓，但不能据此清空权威 DB。

共 **3** 张代码创建的表；字段、类型、默认值、主键、唯一约束、外键和索引来自当前初始化代码在全新临时目录中执行迁移后的实际 schema。

## 表目录

- [_browse_changes](#_browse_changes) — 5 个字段
- [_read_revisions](#_read_revisions) — 2 个字段
- [catalog_cache](#catalog_cache) — 5 个字段

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

**关联触发器**：`trg_catalog_rev_bump`。触发器负责的 journal/revision 逻辑见总览；不等于声明 FK。

## catalog_cache

Metadata source 原始响应缓存（source+cache_key）；不是规范 Media 的身份表，可过期、可重建。

主要实现：[crates/store/src/catalog.rs](../../crates/store/src/catalog.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `source` | `TEXT` | 1 | 是 | 无 | Metadata source 或标记产生来源；随表上下文。 |
| `cache_key` | `TEXT` | 2 | 是 | 无 | 缓存键；与来源/算法/请求参数共同确定复用身份。 |
| `body` | `TEXT` | — | 是 | 无 | Metadata source 响应体文本（通常 JSON）；不是规范化 Media。 |
| `fetched_at` | `INTEGER` | — | 是 | 无 | Metadata source 响应获取时间，Unix 秒。 |
| `expires_at` | `INTEGER` | — | 否 | 无 | 缓存过期时间，Unix 秒；NULL 代表无显式到期。 |

**键与约束**

- 主键：`(source, cache_key)`。
- 无显式外键声明；带 `_id` 的字段不因此自动获得引用完整性。

**索引**

```sql
CREATE INDEX catalog_cache_fetched_idx
             ON catalog_cache(fetched_at ASC);
```

**关联触发器**：`trg_cat_cache_changes_del`、`trg_cat_cache_changes_ins`、`trg_cat_cache_changes_upd`、`trg_cat_cache_on_del`、`trg_cat_cache_on_ins`、`trg_cat_cache_on_upd`。触发器负责的 journal/revision 逻辑见总览；不等于声明 FK。

**使用注意**：body 是来源响应负载，Media 的规范身份仍在 app.db.media。source+cache_key 是复合主键；cache_key 可含查询参数。catalog.db 与 fanart.db 同名表不是两张必须同步的镜像，清理/过期规则由各客户端和维护策略控制。
