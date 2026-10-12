# 自有 API 约定（v1）

> **本文不是现行完整路由表。** 资源、方法、字段以代码为准：
>
> - 后端路由：`crates/api/src/http/mod.rs`（`/api/v1`）
> - 前端调用：`web/lib/api/*`
> - 领域词汇：`CONTEXT.md`
>
> 本文只锁定信封、认证、核心 DTO 形状，以及「明确不做」的产品边界。
> 阶段进度见 `docs/agents/roadmap.md`，那是历史档案，不是接口规范。

本实例的管理面是**自有 REST**。不做上游 MoviePilot / MovieClaw 的信封、DTO 字段名或数字哈希 ID。Jellyfin/Emby 播放协议（Infuse 直连）是另一套表面，见 ADR-0005，不是「上游兼容层」。

## 通用约定

- 前缀：自有 REST 挂 `/api/v1`。
- 信封：成功 `{"ok": true, "data": ...}`；失败 `{"ok": false, "error": {"code": "...", "message": "..."}}`，HTTP status 4xx/5xx。
- ID：领域实体用 **UUID 字符串**（`MediaId` / `SiteId` / `SubscribeId` / … 的 Display）。Jellyfin 表面会把 UUID 去连字符（compact id），那是播放协议，不要写进自有 DTO。
- 认证：`Authorization: Bearer <token>`。`POST /auth/login` 返回 `{token, user}`。静态图片（海报/背板/剧照/章节图/库封面）走 public 路由，因为 `<img>` 带不了 Bearer（见 `AGENTS.md`）。
- 错误 code：`resource.action`，如 `subscribe.missing`、`site.invalid`。message 中文可读。
- 枚举：`media.kind` = `movie|tv|video`（`video` 是「其他」库，不识别、不刮削）；`subscribe.fetch_mode` = `search|rss|both`。
- 时间：RFC3339 UTC。空值用 `null`。

## 核心形状

下列是稳定的领域投影。列表不穷尽字段；实现多出来的字段以 handler JSON 为准。

### User

`{id, login, role: "admin"|"member", enabled}`

`role` 以 `users.role` 列为准，不是某个固定 UUID。

### Site

`{id, name, url, profile_id, cookie?, api_key?, rss_url?, proxy?, rate_limit_per_minute?, cdp_url?, downloader_id?, enabled, boost?}`

凭证列表/读取时遮盖；刚写入时可回显一次。

### MediaView

`{id, kind, title, year?, original_title?, tmdb_id?, douban_id?, tvdb_id?, bangumi_id?, anilist_id?, poster_url?}`

### Subscription（CONTEXT：**Subscribe**）

`{id, media: MediaView, user_id, coverage, fetch_mode, filter_id, wash_cut, wash_cut_filter_id?, keep_old_versions, full_season_pack, downloader_id?, library_id?, tracking_state: "active"|"paused", follow_future, search_interval_secs, progress: {total, imported, missing, grabbing, downloaded}, created_at, updated_at}`

`coverage` = `{"kind": "movie"}` 或 `{"kind": "tv", "season", "episode_from", "episode_to"?}`。

暂停 / 续订窗口走 `PATCH /subscriptions/{id}`（`tracking_state` / `follow_future`），没有独立的 `/tracking-state`、`/follow-future` 端点。

### RuleSet（CONTEXT：**Filter** 组）

`{id, name, is_default, atoms: [{kind, value?, priority, exclude?}]}`

`kind` 至少包括：`resolution` / `source` / `free` / `hr` / `title_match` / `hdr` / `size` / `min_seeders` / `subtitle_language` / `audio_language` / `site` / `wash_target` / `upgrade_ladder`。以 `domain::AtomRule` 为准。

HTTP 路径是 `/rule-sets`；表名仍是 `filters`。这是有意的 API 别名。

### Downloader

`{id, name, kind: "qbittorrent"|"transmission", url, username?, password?, category?, path_maps: [{from, to}], is_default, status, last_error?}`

### Library

`{id, kind, name, root_paths, is_default, item_count, file_count, total_size_bytes, …}`

每种 `kind` 可以有多个库（多根、可见性、刮削开关等）。完整字段见 `crates/api/src/store/libraries.rs` 与 `GET /libraries`。

### LibraryFacets

`GET /libraries/{id}/facets`（筛选面板的唯一数据源；前端 `web/lib/api/libraries.ts` 的 `LibraryFacets`）：

`{total, genres, countries, decades, watch, ratings, runtimes, languages, resolutions, hdr, stock, years: []}`，每项 `{value, label, count}`。

- **计数**：维度 D 的值 v 的 `count` = 满足「**除 D 以外**全部已选条件」且 D 取 v 的 **distinct Media** 数；`total` = 满足全部条件的 distinct Media 数（不是 ledger 行数）。候选全集来自该库全部可见 Media，含 `count: 0`（前端置灰）。
- **查询键**（与 items 接口共用 `filterQuery`，维内 OR、维间 AND）：`g`(TMDB genre id) `c`(ISO 3166-1 二字码) `d`(`2020s|2010s|2000s|1990s|earlier`) `w`(`unwatched|watching|played|favorite`) `rating_gte`(6|7|8|9) `rt`(`lte60|60to90|90to120|gt120`) `lang` `res` `hdr`(true|false) `stock`(`missing|unscraped`)。
- **`tier`**：`primary` 只填 `genres/countries/decades/watch`，其余空数组；`all` 十维全填；**缺失或空串按 `all`**；其他非空值 → 400 `library.invalid`。
- `years` 是历史遗留的空数组键，保留以免破坏未知消费者。
- 未知年份与 `>=2030` 不归入任何 `decades` 档。

### JobDef / Job

`JobDef` = `{id, kind, name, enabled, schedule?: {interval_secs} | null, payload, concurrency_key?, last_status?, last_finished_at?, next_run_after?}`

`Job` status = `queued|running|succeeded|failed|cancelled`。

## 现行表面（去哪看）

| 面 | 挂载 | 说明 |
|---|---|---|
| 自有 REST | `/api/v1` | 唯一自有业务与管理契约。member / admin 在 `http::router` 分层。 |
| Jellyfin / Emby | `/` 与 `/emby` | 专供 Infuse / VidHub 等播放器直连，严格遵守 Jellyfin 官方开放协议。 |

根路径上的历史遗留管理路径（`/subscribes` `/filters` `/search` 等）已彻底移除并返回 404；自有管理面完全统一在 `/api/v1`。

完整 method/path 以 `crates/api/src/http/mod.rs` 的 `router()` 为准。

前端 `web/lib/api/*` 的路径是相对 `VITE_API_BASE_URL`（默认 `/api/v1`）。改密走 `PATCH /users/{id}` 的 `password`，没有 `POST /users/{id}/reset-password`。

## 明确不做

这些不是「暂时 stub」，是产品边界。不要为它们补端点或前端入口。

- AI：`/agent*` `/llm/*` `/mcp/*` `/skills`、对话 session
- 插件市场 / 浏览器扩展：`/extension/*`（issue #129 挂起）
- 转码 / 字幕生成 / PGS OCR
- IM 通道：微信 / Telegram / Discord / 上游 webhook。通知只走 `GET|PUT /notify/config` → `jianxcao/notify`
- OTA：`/app/update/*`
- Photos 模块

历史上的上游兼容层（`webapi/` 信封、`numeric_id`）已删除，不要加回来。
