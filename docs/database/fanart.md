# fanart.db：fanart.tv 图片元数据缓存

[返回数据库总览](README.md)

当前基线：`main@2c3a5c7`。media::fanart::FanartClient；按需创建；PRAGMA user_version 实测为 0；仅一个 catalog_cache 表。

共 **1** 张代码创建的表；字段、类型、默认值、主键、唯一约束、外键和索引来自当前初始化代码在全新临时目录中执行迁移后的实际 schema。

## 表目录

- [catalog_cache](#catalog_cache) — 5 个字段

## catalog_cache

fanart.tv 响应缓存；复用 catalog_cache 结构，但由 media crate 单独打开，不带 Store 的 journal/revision 表。

主要实现：[crates/media/src/cache.rs](../../crates/media/src/cache.rs)。

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

无额外命名索引；主键/UNIQUE 的 SQLite 自动索引不重复列出。

**使用注意**：body 是来源响应负载，Media 的规范身份仍在 app.db.media。source+cache_key 是复合主键；cache_key 可含查询参数。catalog.db 与 fanart.db 同名表不是两张必须同步的镜像，清理/过期规则由各客户端和维护策略控制。
