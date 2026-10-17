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

每种 `kind` 可以有多个库（多根、可见性、刮削开关等）。完整字段见 [Store Library](<../../crates/store/src/libraries.rs>) 与 `GET /libraries`。

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

### 维护任务（DB8）

维护任务是 `kind ∈ {database_maintenance, jobs_retention, playback_history_retention, data_retention}` 的普通 JobDef；
调度与开关仍走既有 JobDef 字段，**保留参数只存在该 def 的版本化 payload 里**（不在 settings 另存一份）。

`payload`（`version: 1`；未知版本/未知字段/越界一律 400，绝不回退成更激进的删除策略）：

- `database_maintenance`：`{version: 1, vacuum_enabled: bool}`（默认 `false`）。
- `jobs_retention` / `playback_history_retention`：`{version: 1, retention_days: 1..=36500, dry_run: bool}`。
  `retention_days=0` **不代表永久保留**；永久保留请关闭该任务。`batch_size=500`、`max_batches=20` 是服务端固定安全预算，不暴露给用户。
- `data_retention`（DB-05）：`{version: 1, dry_run: bool, qoe_days: 1..=36500, transient_cache_days: 1..=36500, probe_days: 1..=36500}`。
  首次注册 `enabled=false`、`dry_run=true`、三个窗口默认 90 天；生产启用需要用户显式确认。QoE 只删 `playback_metrics`；
  catalog 只删 URL path 白名单内的 search/discover/trending 临时缓存（详情与未知 key 保护）；Probe 只删终态 job，
  每个 scope 至少保留最新 3 条（时间并列全部保留），被 sample 引用或仍有活动 unit 的 job 保护。samples/models/model_members 本轮不删。

维护 def 在 `GET /jobs` / `GET /jobs/{id}` 上**额外**返回（非维护 def 不返回这些键，既有字段不变）：

- `maintenance`：typed 策略摘要，如 `{kind, version, retention_days, dry_run}`、`{kind, version, vacuum_enabled}` 或 `{kind, version, dry_run, qoe_days, transient_cache_days, probe_days}`；payload 损坏时为 `{kind, invalid: true, error}`。
- `payload_token`：**修改令牌**，等于服务端当前 `payload` 的原始 JSON 字符串。
- `last_report`：最近一次维护运行报告（`job.progress` 的原始 JSON 字符串，可能为 `null`）。

`PUT /api/v1/jobs/{id}/maintenance-config`（**仅管理员**，会员 403）：

- body = `{payload, expected_payload, interval_secs?}`；`payload` 与 `expected_payload` 必填。
- `expected_payload` 是并发令牌：**保存前的原 payload**，不是 payload 内的 schema `version`。与当前值不符 → 409 `job.conflict`；缺失 → 400 `job.missing_token`。
- 未知 def → 404 `job.missing`；def 存在但非维护 kind → 400 `job.not_maintenance`；非法配置 → 400 `job.invalid_config`（且不落库）。
- `interval_secs` 只对维护任务加范围 `3600..=31536000`，越界 → 400 `job.invalid_interval`；其他任务契约不变。
- 返回 `{id, kind, payload, payload_token, schedule?}`。**当前只有 payload 的比较与更新在同一事务**；schedule 在后续独立写入，可能部分保存、并发混合，且省略 interval 时返回 `schedule:null` 不代表实际周期已清空。这是待修 F04，不是保证整个配置原子。
- 目标是完整配置（payload＋schedule）的 CAS，并刷新尚未运行的 queued 子任务；当前旧 payload token 不能检测只改周期的冲突。见 [F02/F04 与后续计划](<../superpowers/plans/2026-10-13-database-api-followup.md>)。

`POST /api/v1/jobs/{id}/maintenance-preview`（**仅管理员**，会员 403）：

- body = `{payload?}`（缺省用该 def 当前已保存的 payload）。
- **只读、匹配返回行数有限额**：不写def、不创建删除Job、不改数据库内容，也不返回播放明细；真正SQL扫描／时间预算仍是待实现目标，不能将行数限额当硬执行上限。
- 返回 `{kind, policy_version, retention_days?, cutoff?, scanned, candidates, protected, truncated, note?}`；`data_retention` 另含 `examined, incomplete, classes[]`（每类 `examined/candidates/protected/incomplete/resume_after`）；
  目标合同中 `truncated=true` 表示固定预算截断了窗口，计数是**下界**而不是精确总量。**当前 Jobs 只按可删候选决定 truncated，保护计数最多501但未标其截断；不能把 `truncated=false` 当全部计数完整**（F03）。数据库维护没有删除候选，计数字段恒为 0。
- `scanned/candidates` 当前是匹配窗口的行数，不是 SQLite 实际扫描量；LIMIT500 不构成时间或持锁预算，preview 同步 SQL 尚未 offload（F05）。

手动执行（`POST /jobs/{id}/run`，仅管理员）：

- `enabled=false` 的 def → 409 `job.disabled`（不再"返回 queued:true 却没入队"）。
- 已有 queued/running 子任务 → 200 `{queued: false, live_status}`，不误报新增实例；否则 200 `{queued: true}`。**当前 future queued 不会被提前**，因而按钮“立即执行”的语义未完整实现（F12）。
- **目标合同**：修改配置用于尚未运行的下次执行，运行中保留快照。**当前偏差**：已 queued 子任务仍带旧策略，改为更安全策略也可能再按旧策略删除一次（F02）；发布前需修复，而不是将偏差当允许的行为。

清理语义提醒：启用播放历史清理后，**历史页与依赖播放历史的统计窗口都只剩保留期内的数据**（不是仅隐藏列表）；删除不可撤销，只能从一致备份恢复。

**目标合同**要求关闭后不启动下一批；**当前禁用只取消 queued，running 清理不检查 def.enabled，仍会继续后续批次**（F01）。不能只凭UI开关认定清理已停止，已删除数据也不会恢复。SSE `/jobs/stream` 的 `count` 因清理会**下降**，客户端不得把它当单调递增序列。

上述已确认偏差与优先级以 [总复核](<../audits/2026-10-13-database-api-review.md>) 为准；DB8 不是“全部完成并验收”。

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
