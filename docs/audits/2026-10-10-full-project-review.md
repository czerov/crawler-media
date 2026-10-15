# crawler-media 全项目逻辑与设计 Review

> **历史状态（2026-10-13 整理）：** PIPE/ROOT/ACCESS/I 原发现属于当时基线；整改计划和 follow-up 提供局部实现线索，但全清单仍为 **UNRECONCILED**，须逐 ID 确认关闭/残余风险。下文缺陷复现和当时绿色基线不代表今天仍有同样缺陷或全部已修复。
>
> 原设计/ID/测试记录保留；旧执行、清理和提交命令不是当前执行授权，不应直接重跑。当前依据：[数据库/API review](<2026-10-13-database-api-review.md>) 与 [剩余计划](<../superpowers/plans/2026-10-13-database-api-followup.md>)；未覆盖的历史发现仍须独立对账。

审查日期：2026-10-10（Asia/Shanghai）。审查基线：`277de10270f3b2ce7ccb5f30075073fdbfa614b2`。正文行号基于该基线，后续代码变更可能导致位置偏移。

本轮只审查并输出文档，不修复业务代码。确认 **25 项问题：3 项 P1、21 项 P2、1 项 P3**。其中 24 项通过离线行为复现确认，Obscura 接线问题通过生产调用链确认。P1 表示可能丢失已有文件、破坏持续运行的 Subscribe，或批准破坏性质量降级；P2 表示常规功能错误、用户状态丢失或配置宣称的能力不可用；P3 是低优先级输入边界问题。

建议首先处理 PIPE-02（旧文件提前删除）、PIPE-01（Wash-cut 质量降级）、ROOT-01（Media 身份误删）。其次处理 pending/owned 混淆、生产 Downloader 假成功、设备注销失效及观看状态持久化错误。Browser 的两项问题是配置与运行时能力之间的明确设计缺口，并非纯代码风格意见。

## 1. 发现索引

| 编号 | 等级 | 问题 | 验证 |
|---|---|---|---|
| PIPE-02 | P1 | legacy /run 在 replacement ledger 提交前删除旧文件 | 注入 SQLite 写失败，旧文件消失 |
| PIPE-01 | P1 | 恢复 facts 丢失 source，使 Remux 可被 HDTV 替换 | 持久化前拒绝，恢复后接受 |
| ROOT-01 | P1 | 最后文件删除时误删仍被 Subscribe 引用的 Media | Subscribe 存在，Media 不存在 |
| PIPE-03 | P2 | 第二次 /run 将 pending 写成 owned，提前完成 | ledger=0，却 completed=true |
| PIPE-04 | P2 | 新 Subscribe 忽略共享 Library 已有文件 | 同一 Media 重复投递 Torrent |
| PIPE-05 | P2 | 整季包裸 S01E01 文件被当成外来 Media | 已识别 coverage，仍拒绝文件 |
| ROOT-02 | P2 | 删除一个版本会删除其他版本仍需的手动 marker | 另一版本存在，locked marker 丢失 |
| ROOT-03 | P2 | Video Library 的实时扫描误建 Movie/TV | 新 Video 文件产生 Movie 身份 |
| ROOT-04 | P2 | 第二次章节读取借用未验证的同季片头 | 第一次空，第二次自动产生片头 |
| ROOT-05 | P2 | 多个冲突模板按数组顺序选第一个 | 调换模板顺序改变 Verified 区间 |
| ACCESS-01 | P2 | 清空观看记录同时删除收藏及轨道偏好 | 收藏 total 从 1 变为 0 |
| ACCESS-02 | P2 | Jellyfin 播完不更新已看 | 满时长 Stopped 后仍在 Resume |
| ACCESS-03 | P2 | Jellyfin 规范电影单元遮蔽旧版状态 | 旧收藏/已看/轨道不再被读取 |
| ACCESS-04 | P2 | 播放事件忽略标准 Authorization DeviceId | 两设备覆盖成一条会话 |
| ACCESS-05 | P2 | 离线设备使用伪 id，注销无效 | 注销 200，真实设备取流仍 200 |
| ACCESS-06 | P2 | web 注销只禁止心跳，没有禁止取流 | progress 403，原流 URL 200 |
| ACCESS-07 | P2 | web 进度写失败仍返回已保存快照 | trigger 拒绝写，响应仍 200 |
| ACCESS-08 | P2 | QoE 前后端字段不一致，指标全被丢弃 | 实际前端 payload 200，无数据行 |
| I01 | P2 | 默认 Downloader 配置错误回退 Memory | 未发真实下载却接受成功 |
| I02 | P2 | 路径映射错误匹配兄弟目录 | /downloads-old 被重定根 |
| I03 | P2 | 无效 HTTP 200 污染 Metadata 缓存 | 上游恢复后仍命中新鲜坏缓存 |
| I04 | P2 | 登录/Check-in 把登录表单当成功 | fake 登录页使两个插件都返回 Ok |
| I05 | P2 | managed Browser 开启后仍未接通启动能力 | 无外部 CDP 的 render 立即失败 |
| I06 | P2 | Obscura 配置未接入 Indexer Fetcher | 生产调用链无 with_obscura 调用 |
| ACCESS-09 | P3 | history limit=0 导致 handler panic | JoinHandle 捕获 panic |

## 2. 范围与证据边界

覆盖全部 16 个 Rust workspace crate 及 web 的主要调用链，重点沿用户行为跨 crate 检查；这不是逐行正确性证明。

| 范围 | 审查重点 |
|---|---|
| domain、store | Media 身份、Subscribe/facts/pending、Library ledger、质量恢复、Playback 状态、设备、marker/cache |
| subscribe、filter、release | 准入、覆盖范围、完成判定、Wash-cut 质量、文件身份及整季包 |
| library、jobs、hooks | Transfer 删除顺序、Watch 扫描、Job 生命周期与失败处理、Login/Check-in |
| indexer、downloader | 搜索/RSS/Profile、Browser/CDP、令牌、动态 Downloader 选择、完成文件及路径映射 |
| media、cover-generator | Metadata source 缓存/错误恢复、Catalog/Scrape/NFO 接线、封面生成接线 |
| marker | 自适应采样、模板验证、冲突处理、持久化缓存与章节读取 |
| playback、media-server | 播放进度、Jellyfin 协议、设备身份与撤销、Resume/NextUp、取流凭据 |
| api、web | HTTP/worker 两种执行路径、路由权限、成功响应、设置运行时生效、播放器真实请求形状 |

依据仓库 `AGENTS.md`、`CONTEXT.md` 和相关 ADR 核对行为。只使用 fake Fetcher/HTTP/Downloader、临时 SQLite 和临时小文件；没有访问真实 Site、Metadata source、Downloader、真实媒体，也没有启动 Chromium。第三方实际版本兼容性、浏览器真实播放及生产并发压力未验证，不把这些未证实推测列成发现。

静态图片公开、Site 凭据明文保存、尚未实现的实时转码按当前约定处理，未误报为漏洞。这里只确认设备注销链路的具体失效，没有宣称视频路由普遍缺少认证。

审查开始后的外部 amend 仅加入 `anthropic-messages.js`，业务基线与 `c5a435b` 一致。验证期间另有不属于本 review 的未提交声纹/扫描日志和 STRM 测试变动；本次保留它们，不纳入报告提交。末尾完整回归结果描述验证时的当前工作区。

## 3. Subscribe / Transfer / Wash-cut

### PIPE-01 [P1]：持久化丢失 source 质量，Wash-cut 可把已有 Remux 降级为 HDTV

**定位**：[crates/store/src/subscribes.rs:638](<../../crates/store/src/subscribes.rs#L638>)，尤其第 645 行 `source: None`；[crates/subscribe/src/choose.rs:112](<../../crates/subscribe/src/choose.rs#L112>)。

**触发**：已有电影/TV 文件是 Remux；Wash-cut 的 UpgradeLadder 包含 `source`（或 WashTarget 指定来源）；Transfer 完成并将 facts/ledger 保存后进入下一轮搜索。不存在进程重启的必要条件，每次从 Store 重新加载即可触发。

**因果链**：Transfer 内存 facts 的 quality 保留 Torrent 的 source，但 ledger 没有保存该维度。`load_subscribe_facts` 用 ledger 构造质量且显式令 source=None；choose 优先采用已有 `facts.quality(path)`，因此即使文件名明确包含 Remux，也不会再回退解析文件名。UpgradeLadder 将未知旧 source 作为 0，与 HDTV 等任何已知来源比较都认为是升级；含 source 的 WashTarget 也永远不能判断已有来源已达标。替换被批准后，默认不保留旧版本，会删除更高来源质量的旧文件。

**实证**：临时测试 `source_quality_survives_persistence_for_wash_cut`：同一 owned Remux / 新 HDTV / source ladder，在保存前 choose 返回空；`insert_ledger → save_subscribe_facts → load_subscribe_facts` 后 choose 返回该 HDTV。路径刻意包含 Remux，排除仅因重命名信息缺失造成的问题。

**建议**：将决定 Wash-cut 的 Release 维度作为持久化质量事实保存、恢复；流媒体 probe 的 resolution/codec/HDR 可以覆盖同字段，但不得抹除来源。缺少维度时不能简单把“未知”解释为劣于所有已知值并批准破坏性替换。补保存/重开 Store 后 source ladder、source cutoff 测试。

### PIPE-02 [P1]：仍公开的 legacy /run 在新 ledger 提交前删除旧版本

**定位**：[crates/api/src/management/subscribes/run.rs:276](<../../crates/api/src/management/subscribes/run.rs#L276>)（第 291 行 `preserve_removed: false`），`:381-395`；[crates/subscribe/src/collect.rs:361](<../../crates/subscribe/src/collect.rs#L361>)。

**触发**：`POST /api/v1/subscriptions/{id}/run` 对 Wash-cut 获取已完成的更高分文件，且写入新 ledger 时发生 SQLite/IO 错误。

**因果链**：该路由仍启用（[crates/api/src/http/mod.rs:201](<../../crates/api/src/http/mod.rs#L201>)）。collector 在 `preserve_removed=false` 时立刻 unlink 旧版本；legacy persist 又先删旧 ledger，然后才插入新 ledger。新插入一旦失败，请求返回 500，但旧文件和旧 ledger 已经消失，facts 仍指旧路径，新文件则未入账。相比之下，常规后台 Transfer 在 [crates/api/src/worker/transfer.rs:25](<../../crates/api/src/worker/transfer.rs#L25>) 已遵循“先持久化新记录/facts，再删旧文件”；两条入口的删除安全性不同。

**实证**：临时测试 `legacy_wash_cut_keeps_old_file_if_replacement_ledger_write_fails`：用 MemoryDownloader 提供 replacement，用测试临时 library.db 的 BEFORE INSERT trigger 确定性注入 ledger 写入错误，HTTP 返回 500 后 `old.is_file()` 为 false。只使用 tempfile 和 fake Downloader，未访问真实媒体或数据库。

**建议**：让 /run 复用与后台 Transfer 相同的安全提交流程，collector 只返回待删除路径。新 ledger 与 facts 持久化失败时保留旧版本；后续删除失败要有可重试记录。针对两条 HTTP/worker 入口共同验证失败顺序。

### PIPE-03 [P2]：连续 /run 将在途 pending 永久写成已拥有 facts，并错误宣告完成

**定位**：[crates/api/src/management/subscribes/run.rs:224](<../../crates/api/src/management/subscribes/run.rs#L224>)；[crates/subscribe/src/choose.rs:418](<../../crates/subscribe/src/choose.rs#L418>)。

**触发**：普通非 Wash-cut Subscribe 已提交下载但尚无已完成文件时，连续调用两次 legacy `POST /subscriptions/{id}/run`。

**因果链**：第二次 run 为现有 pending 构造 score=0 / path=None 的占位事实，用于抑制重复提交；但这些临时占位进入 RunOutcome 后又被无条件 `save_subscribe_facts`。`is_complete` 和展示进度只判断 fact 是否存在，因此把“下载中”当成“已拥有”。在途下载随后被取消或 Downloaders 列表清理 pending 时，path=None facts 不会一起清理；普通 chooser 又对任何已有 fact 拒绝新候选，可能永久不再补齐缺失内容。

**实证**：`legacy_second_run_does_not_turn_pending_into_owned_fact` 使用从未映射 completed_files 的 MemoryDownloader。第一次 response：`{completed:false,ledger_rows:0}`；第二次 response：`{completed:true,ledger_rows:0}`。Store ledger 始终为空、pending 仍在，却已持久化 movie fact(path=None)。

**建议**：将“在途覆盖”与“已拥有事实”分开传递，临时 pending 占位不得持久化为 imported facts；完成判定应要求对应真实 Library ledger。增加已有 path=None 数据的修复路径，测试 pending 取消后仍能继续搜索补齐。

### PIPE-04 [P2]：新 Subscribe 不从共享 Library 读取已有事实，重复下载已拥有内容

**定位**：[crates/api/src/subscribe_create.rs:24](<../../crates/api/src/subscribe_create.rs#L24>)；[crates/api/src/worker/finish_search.rs:90](<../../crates/api/src/worker/finish_search.rs#L90>)；[crates/api/src/management/subscribes/run.rs:223](<../../crates/api/src/management/subscribes/run.rs#L223>)。

**触发**：目标 Library 已有对应 Media 的文件（来自扫描、旧 Subscribe 或另一 User）；用户为相同 alias 的 Media 新建普通 Subscribe 并开始搜索。

**因果链**：创建只写新的 Subscribe。搜索/准入仅加载该 Subscribe 自己的 facts，既不检查目标 Library 已有 ledger，也不初始化已有 coverage。新 Subscribe 的事实为空，因此 chooser 把已经拥有的电影/集数当作缺失并向 Downloader 提交。`RoutedDownloader::imported_destinations` 的协调发生在准入/提交之后，且还限于带 source_path 的 ledger，无法保护扫描进来的已有文件。与 CONTEXT 中“Library 是权威拥有事实，Subscribe 读取它”的契约不符。

**实证**：`existing_library_prevents_redundant_download_for_new_subscribe` 用相同 douban alias 创建两个 Subscribe，确认两者指向完全相同内部 MediaId；在默认 Movie Library 写入实际文件和 2160p ledger，再运行第二个 Subscribe，MemoryDownloader.added() 非空。不是仅标题相似导致身份未合并。

**建议**：搜索选择前，从目标 Library 按 Media + coverage 构造/协调已有事实，保留 Library 范围；扫描与另一个 User 的 Transfer 应能被新 Subscribe 复用。普通填充不应重新下载已拥有槽位；Wash-cut 则以已有实际质量作为比较基准。

### PIPE-05 [P2]：整季包裸 S01E01 文件被误认成外来 Media，无法 Transfer

**定位**：[crates/subscribe/src/file_identity.rs:5](<../../crates/subscribe/src/file_identity.rs#L5>)；关联 [crates/release/src/boundary.rs:31](<../../crates/release/src/boundary.rs#L31>) 与 `crates/release/src/lib.rs` 的 title 回退。

**触发**：合法整季多文件 Torrent，如 `Test.Show.S01.1080p`，其内部视频使用常见裸文件名 `S01E01.mkv`、`S01E02.mkv`。

**因果链**：Release 正确识别季/集，但无Media 标题时 title 回退为 `S01E01 mkv`。`is_generic_episode_stem` 只识别数字/简单 E 前缀，无法识别 SxxExx；foreign identity guard 把该 season/episode token 作为 Media 标题比较并拒绝。已确认的季/集 coverage 因而也无法通过。由于 resolve_file_release 返回 None 而非 collection error，Transfer 可以显示成功却无 ledger、pending 持续 active。

**实证**：`season_pack_accepts_bare_sxxexx_episode_names`：parse 返回 `season=Some(1),episode=Some(1),confidence=High,title="S01E01 mkv"`；匹配 S01 范围且 full_season_pack=true 的 `resolve_file_release(...,single_file=false)` 仍返回 None。

**建议**：区分“文件名没有 Media 标题”与“有明确冲突的 Media 标题”，对可可靠识别的纯季/集 basename 继承已经确认的 Torrent Media 身份；继续拒绝真正的标题/年份冲突。增加整季真实文件列表用例，避免只测试 E01 的窄分支。

## 4. 文件生命周期与片头验证

### ROOT-01 [P1] 删除最后一个文件会误删仍被 Subscribe 引用的 Media

**定位**：[crates/api/src/fs_watcher.rs:477](<../../crates/api/src/fs_watcher.rs#L477>)；[crates/store/src/media.rs:214](<../../crates/store/src/media.rs#L214>)。

**触发与影响**：存在 active Subscribe，并且目标 Media 只剩一个 Library 文件。用户从磁盘删除该文件，Watcher 清理 ledger 后发现 remaining=0，直接清除 Media。Subscribe 行仍然存在，但其 media_id 已无对应身份；后续搜索不能加载目标 Media，无法按持续目标补齐。Media 身份的生命周期被错误地绑定到“至少拥有一个文件”。

**证据**：`watcher_must_preserve_media_still_targeted_by_subscribe` 建立临时 Media、文件、ledger 与 active Subscribe，删除文件并发送公开 `handle_fs_events` 事件；查询得到 Subscribe 仍存在，Media=None，正确行为断言失败。

**建议**：删除文件只清理该文件拥有的事实；Media 垃圾回收需检查 Subscribe 等身份引用。跨 SQLite 数据库没有外键保护时，必须由用例服务维护不变量；有持续 Subscribe 的 Media 应保留，随后允许补齐缺失文件。

### ROOT-02 [P2] 删除单个版本时会清除其他版本仍需的 locked marker

**定位**：[crates/api/src/fs_watcher.rs:483](<../../crates/api/src/fs_watcher.rs#L483>)。

**触发与影响**：同一 Media/S01E01 有两个 ledger 版本，并有用户手动锁定的 episode marker。删除其中一个版本，Watcher 在检查剩余版本前无条件调用 delete_media_marker；另一版本仍可播放，但手动片头/片尾事实丢失。已有章节缓存可能暂时遮蔽丢失，缓存失效后会暴露。

**证据**：`deleting_one_version_must_preserve_locked_episode_marker_for_other_version`：第二个版本的 ledger 仍存在，公共 Store 查询该 locked marker 却返回 None。

**建议**：区分文件级探测缓存与 Media/episode 级 marker。只清除被删除文件的派生缓存；marker 清理先检查同槽位其他版本及 locked 的保留政策，不能仅按单个路径的删除事件级联删除共享事实。

### ROOT-03 [P2] Video Library 实时扫描走 Movie/TV 识别路径

**定位**：[crates/api/src/fs_watcher.rs:437](<../../crates/api/src/fs_watcher.rs#L437>)；[crates/api/src/http/library_scan.rs:43](<../../crates/api/src/http/library_scan.rs#L43>)；[crates/api/src/watch_ledger.rs:118](<../../crates/api/src/watch_ledger.rs#L118>)。手动扫描的正确分支在 `library_scan.rs:148`。

**触发与影响**：kind=Video、realtime_watch=true 的 Library 中新建 `Vacation.2026.1080p.mp4`。实时扫描虽读取 Library.kind，实际 InPlace 扫描与 record_paths 不使用它决定身份，只按 Release 是否带 season 创建 Movie 或 TV。用户自己的 Video 会从 Video 视图消失，并进入错误分类/元数据识别流程；同一个 Library 的手动扫描与实时扫描语义不一致。

**证据**：`realtime_video_library_must_use_the_video_ingestion_path` 通过公开文件事件等待扫描结束；产生的 Media.kind=Movie，正确行为 Video 断言失败。

**建议**：手动和实时扫描共享按 Library.kind 分流的服务；Video 用 record_video_paths，不通过 Movie/TV 的名称识别准入。测试分别覆盖创建、重扫与文件变更，使用 fake 探测。

### ROOT-04 [P2] 同一章节请求第二次读取会借用未验证的同季片头

**定位**：[crates/api/src/marker_resolver.rs:65](<../../crates/api/src/marker_resolver.rs#L65>)。

**触发与影响**：E1 已有片头，E2 自身没有 marker/章节且无外部检测客户端。第一次读取 E2 返回空并缓存空列表；第二次读取看到空缓存，会任取同季另一个 episode 的 marker，直接构造 E2 的 IntroStart 章节。没有新证据，也没有对 E2 比对。不同开场、特殊集或不同剪辑会得到错误跳过提示/自动跳过区间。

**证据**：`empty_chapter_read_must_not_gain_an_unverified_neighbor_intro_on_second_read`：E1 区间 10–70 秒；连续读取 E2，第一次 `[]`，第二次产生序幕、10–70 秒片头与正片章节。这里没有声称它会覆盖已有完整 NoMatch 缓存；确认的是空缓存分支的隐式借用。

**建议**：章节读服务只返回本 episode 的持久化/已验证事实。邻集只可作为候选模板，目标 episode 需独立验证；“暂未检测”“明确无匹配”“检测失败”应保持不同状态，重复读不能悄悄提升可信度。

### ROOT-05 [P2] 冲突模板选择依赖遍历顺序

**定位**：[crates/marker/src/adaptive/verify.rs:50](<../../crates/marker/src/adaptive/verify.rs#L50>)。

**触发与影响**：目标窗口同时匹配多个模型，分别支持互不兼容的时间区间，且各模型都达到多 episode 支持阈值。verify_template_window 遇到第一个成功模型立即返回 Verified，只做单模型内的歧义检查，没有比较跨模型冲突。生产允许构建多个模板，证据聚类顺序会影响最终跳过范围。

**证据**：`conflicting_templates_must_not_depend_on_model_order`：A 两参考支持 10–70 秒，B 两参考支持 100–160 秒，score/coverage 都是 1。原顺序 Verified=10–70，倒序 Verified=100–160；要求冲突保持未决的断言失败。

**建议**：汇总所有模型的有效区间后进行一致性/歧义决策；不兼容而同样有力的结果应返回未决状态并扩窗/补证据，不依据数组顺序提交 marker。补充多模型的排列不变性用例。

## 5. 权限、设备与 Playback

### ACCESS-01 [P2] 清空观看记录同时删除收藏

**定位**：[crates/store/src/playback.rs:357](<../../crates/store/src/playback.rs#L357>)；入口 [crates/api/src/http/playback_logs.rs:259](<../../crates/api/src/http/playback_logs.rs#L259>)；界面确认说明 [web/components/settings-view.tsx:332](<../../web/components/settings-view.tsx#L332>)。

**触发**：给一个 Media 收藏（可以从未播放），在设置或首页菜单执行“清空全部观看记录”。

**实际后果**：`clear_units` 直接删除整行 `playback_units`，该行同时持有 `favorite` 与音轨/字幕记忆。除承诺清除的进度/已看/次数外，用户所有受影响收藏一起消失，收藏页 total 从 1 变为 0。按单个 Media/Library 清理也存在同样问题。界面未告诉用户会删收藏。

**修复建议**：观看事实与收藏分别持久化，或清历史时只重置观看字段，保留 favorite 和无关偏好；按时间窗口清理也遵循相同边界。

**验证**：`review_access_tmp_clear_history_erases_favorites`：预置 `favorite=true` → `DELETE /api/v1/playback/history?scope=all` 200 → `GET /api/v1/playback/favorites` 返回 `total=0`。回归应断言收藏仍存在且观看字段确实清零。

### ACCESS-02 [P2] Jellyfin 播放至结尾不更新已看状态

**定位**：[crates/api/src/media_server_provider/playback.rs:212](<../../crates/api/src/media_server_provider/playback.rs#L212>)（`played` 恒为 None）；[crates/store/src/playback.rs:142](<../../crates/store/src/playback.rs#L142>)（只保留旧值）；对比 web 完成判定 [crates/api/src/http/playback/heartbeat.rs:140](<../../crates/api/src/http/playback/heartbeat.rs#L140>)。

**触发**：首次通过 Jellyfin 客户端顺序发送 Playing、Progress、Stopped，最后位置达到已知视频时长。

**实际后果**：即使文件 duration 已知且 position 等于 duration，Playback unit 仍 `played=false`，已播完 Media 继续出现在 Resume，TV 的 NextUp 也不会跨到下一集。日志的 completed 计算并不会回写 unit。

**修复建议**：两套协议复用完成判定与观看状态更新服务；达到阈值时持久化 played，保留显式未看/重看语义。

**验证**：`review_access_tmp_complete_jellyfin_stays_unplayed`：duration=120000 ms，Stopped=120000 ms 后 unit.played=false，`GET /Users/Me/Items/Resume` 的 `TotalRecordCount=1`。回归应断言自动已看并从 Resume 移除。

### ACCESS-03 [P2] Jellyfin 首次写入规范电影单元遮蔽旧版观看状态

**定位**：[crates/api/src/media_server_provider/playback.rs:51](<../../crates/api/src/media_server_provider/playback.rs#L51>)；对比已经实现迁移的 web 写路径 [crates/api/src/http/playback/heartbeat.rs:97](<../../crates/api/src/http/playback/heartbeat.rs#L97>)。

**触发**：升级数据中一部电影仅有旧 `(0,0)` 单元（已看、时长、收藏、轨道记忆和次数）；升级后首次通过 Jellyfin 播放。

**实际后果**：Jellyfin 直接新建 `(-1,-1)` 单元，没有先复制 legacy 单元。新的默认 `played=false`、缺省 duration/轨道记忆覆盖后续规范优先读取结果，web `/resume` 也返回未看/无轨道。旧行尚在数据库，因此这是状态遮蔽，而非旧行物理删除；从用户角度升级后观看状态和播放器偏好失效。

**修复建议**：把兼容迁移放入统一的写服务，Jellyfin 的事件与直接 update_progress 路径也应在首次规范写前调用 `copy_legacy_movie_unit_if_missing`，迁移失败不可写空规范行。

**验证**：`review_access_tmp_jellyfin_shadows_legacy_movie_marks`：旧 `(0,0)` 设置 played/favorite=true、duration=120000、audio=`embedded:1`；Playing 后规范 unit played/favorite=false、duration/audio=None，web `/resume` 返回 played=false。回归应断言既有状态完整保留。

### ACCESS-04 [P2] 标准 Authorization 中的 Jellyfin DeviceId 在播放事件里被忽略

**定位**：[crates/media-server/src/routes.rs:705](<../../crates/media-server/src/routes.rs#L705>)；认证解析则支持该来源：[crates/media-server/src/auth.rs:47](<../../crates/media-server/src/auth.rs#L47>)。

**触发**：同一 User 的两台客户端使用 `Authorization: MediaBrowser Token="...", DeviceId="...", Client="..."`，不额外发送 `X-Emby-Device-Id`。这是认证层已接受的输入形状。

**实际后果**：两个不同 DeviceId 都被播放事件记录为 `jellyfin-client`。会话按 User + DeviceId 存储，因此两台客户端覆盖同一行；一台停止会关闭另一台的活动会话，观看日志/时长错误，按真实 DeviceId 的注销与事件检查也不一致。

**修复建议**：播放事件复用 AuthUser.device_id；Client/DeviceName/Version 也统一解析协议头。没有设备身份时使用不会合并同 User 所有客户端的可追踪回退值。

**验证**：`review_access_tmp_standard_authorization_collides`：DeviceId bedroom 与 living-room 各发送 Playing，两请求 204，最终只有 1 行，id=`jellyfin-client`，bedroom 查不到。回归应得到两条独立会话。

### ACCESS-05 [P2] 已停止设备在列表里变成伪造 id，注销返回成功却无法阻止真实设备

**定位**：[crates/api/src/http/playback_activity.rs:215](<../../crates/api/src/http/playback_activity.rs#L215>)；[crates/store/src/playback.rs:511](<../../crates/store/src/playback.rs#L511>) 的日志不保存真实 device_id。

**触发**：设备 `actual-device`（名称 Bedroom Apple TV）播放并停止；到设备列表选中这台离线设备执行注销。

**实际后果**：真实 id 随会话关闭丢失，列表合成 `jf-Bedroom Apple TV` 并标 `revocable=true`。注销只把合成字符串加入撤销列表，真实 actual-device 继续正常取流/上报。不同设备重名还会错误合并；重新在线的同一设备会出现两个身份。

**修复建议**：持久化真实设备 id（独立设备表或日志 device_id），列表与撤销统一用同一稳定身份。历史无真实 id 的行应明确不可撤销，不能伪造可执行身份。

**验证**：`review_access_tmp_idle_revoke_is_ineffective`：Playing/Stopped → devices 返回 `jf-Bedroom Apple TV` → DELETE 200 → 使用 `X-Emby-Device-Id: actual-device` 访问 stream 仍 200。回归应在注销后取流 403。

### ACCESS-06 [P2] web 设备注销阻止进度，却没有阻止该设备的视频取流

**定位**：[crates/api/src/http/playback.rs:467](<../../crates/api/src/http/playback.rs#L467>)；[crates/media-server/src/routes/media/playback.rs:40](<../../crates/media-server/src/routes/media/playback.rs#L40>)；web 吞掉 heartbeat 403：[web/components/player/video-player.tsx:1475](<../../web/components/player/video-player.tsx#L1475>)。

**触发**：浏览器以 device_id=browser 上报 start，管理员在设备列表注销 browser。浏览器取流 URL 来自 start_session，只带用户 api_key，不带设备身份。

**实际后果**：progress 的 browser 身份被撤销后返回 403；原 `<video>` URL 的服务端身份回退成 token，并没有命中 browser 的撤销记录，仍能打开和续传。播放器把 403 当成普通上报失败吞掉，继续播放。不是假设用户主动绕过；当前 UI 自动生成的 URL 已走错身份。

**修复建议**：开播放时把 stream/subtitle 凭据绑定到设备（服务端可验证的绑定，不能只信任随意可改的 query DeviceId），并让播放器区分撤销403与瞬时网络错误，主动退出。

**验证**：`review_access_tmp_browser_revoke_does_not_revoke_stream`：start 200 → DELETE browser 200 → 同设备 progress403 → 当前 URL 形状 `/Videos/{id}/stream?api_key=...` 仍200。回归同时验证被注销流403和另一正常设备不受影响。

### ACCESS-07 [P2] web 观看进度写失败仍返回成功并伪造已保存快照

**定位**：[crates/api/src/http/playback/heartbeat.rs:164](<../../crates/api/src/http/playback/heartbeat.rs#L164>)。

**触发**：SQLite 播放单元写入失败（磁盘/约束/数据库异常），同时 User、Media、ledger 仍可正常读取。

**实际后果**：upsert_unit 的 Err 被转换成仅在内存构造的 UnitState，再用200返回该 position。会话写入/关闭失败也只打日志。用户看到播放继续、服务端仿佛保存了进度，重新打开却退回旧位置或0；与已经正确返回500的 Jellyfin事件路径不一致。

**修复建议**：写入结果返回 Result；关键 unit/session 操作失败返回明确5xx，成功响应只能来自成功持久化结果。需要原子一致性的 unit/session/log 更新放在事务中。

**验证**：`review_access_tmp_web_writes_fail_but_success`：临时 SQLite trigger 对 playback_units INSERT 执行 RAISE(ABORT)；POST start position30000 返回200/data.position_ms30000；随后公共 store seam 查询 unit=None。回归应断言5xx且不报告已保存快照。

### ACCESS-08 [P2] 前后端质量指标字段不一致，所有正常播放器 QoE 上报被丢弃

**定位**：[web/components/player/video-player.tsx:1533](<../../web/components/player/video-player.tsx#L1533>)，[web/lib/api/playback.ts:1104](<../../web/lib/api/playback.ts#L1104>)；后端 [crates/api/src/http/playback.rs:538](<../../crates/api/src/http/playback.rs#L538>)。

**触发**：正常播放后离开播放器，qoeSnapshot 发出 `{library_file_id, tier, engine, watched_ms, ...}`。

**实际后果**：前端接口根本没有 media_item_id 字段；后端只有 media_item_id 字符串解析成功才 insert_metric，也不由 library_file_id 解析 Media。所有 UI 指标请求都200但没有一行指标，丢失播放质量诊断数据。

**修复建议**：统一 DTO：由 file_id 安全解析 Media，或让前端附上字符串 MediaId；无效必需字段不要静默当成功。至少覆盖一次实际前端payload到存储的集成测试。

**验证**：`review_access_tmp_metrics_frontend_shape_dropped`：按实际前端形状 POST metrics，200；临时数据库 playback_metrics 行数为0。回归应验证插入内容对应正确 User/Media 和数值。

### 较低优先级边界问题

### ACCESS-09 [P3] history limit=0 可使已认证请求触发 handler panic

**定位**：[crates/api/src/http/playback_logs.rs:110](<../../crates/api/src/http/playback_logs.rs#L110>)。

**触发**：当前用户存在至少一条可见播放日志，GET `/api/v1/playback/history?limit=0`。

**实际后果**：limit只做上限200，未做下限；has_more=true 后 truncate(0)，last=None，分页游标 expect panic。请求不能得到正常错误信封；这里没有声称整个进程会退出，实际影响取决于服务端 panic 隔离。

**修复建议**：limit clamp(1,200) 或明确400；消除依赖外部输入不变量的 expect。

**验证**：`review_access_tmp_history_zero_limit_panics` 用 Tokio JoinHandle 捕获 handler panic，精确位置160。回归应断言正常200/400且无 panic。


## 6. 外部集成与 Browser 能力

### I01 [P2] 配置不完整的默认 Downloader 会悄悄接入测试用 MemoryDownloader

**位置**：[crates/api/src/runtime_downloader.rs:100](<../../crates/api/src/runtime_downloader.rs#L100>)；调用链 `dynamic_downloader.rs:65`，`delivery.rs:63-69`。`http/downloaders/instances.rs:138-148` 接受缺失 username/password 的配置。

**触发**：在 UI/API 创建 qBittorrent 并设为默认，未填密码；或对已有默认项清空凭证；然后提交 Torrent。也覆盖未配置默认 Downloader 的正式环境，配置选择会直接返回 Memory。

**实际**：`choose_downloader` 将错误配置转换为 `ChosenDownloader::Memory`，正式启动的 `DynamicDownloader` 随即调用 `MemoryDownloader::add`。该方法返回 `Ok(())`，只记录内存条目，不会向 qBittorrent 添加任务。提交路径会记录已投递 pending，而 `completed_files` 永远无法获得实际文件。用户看到已接受，却没有真正下载。作为非默认项时相同配置会在 `delivery.rs:82` 返回 incomplete，说明默认/非默认表现也不一致。

**证据**：临时 integration test 创建缺密码的默认 qBittorrent 行，调用公开 `DynamicDownloader::add`；期望错误实际成功，报错内容为 `incomplete production config accepted a fake download: endpoint=Some("memory|memory")`。没有连 `127.0.0.1:1`，整个操作由内存实现接受。

**建议**：生产中缺少可用 Downloader、配置不完整、类型无效应返回显式错误；Memory 只允许测试/明确的开发模式注入。新增公共 submit seam 回归：无 Downloader、缺凭证、禁用默认项、未知 kind 都不得返回接受成功或写 pending。

### I02 [P2] 路径映射把前缀相同的兄弟目录当作同一个根

**位置**：[crates/downloader/src/path_map.rs:21](<../../crates/downloader/src/path_map.rs#L21>)。

**触发**：映射 `/downloads -> /host/qb`，Downloader 返回 `/downloads-old/Film.mkv`；或者配置多个有共同字符串前缀的下载根。

**实际**：`str::strip_prefix` 将其映射成 `/host/qb/-old/Film.mkv`。两个 Downloader 的 `completed_files` 都调用此映射，Transfer 因而找不到真实完成文件；如果错误目标恰好存在，则读取了错误文件。反向 `remap_to_downloader` 使用 `Path::strip_prefix`，仅正向漏掉路径组件边界。

**证据**：公共 `apply_maps` integration test 失败，实际 `/host/qb/-old/Film.mkv`，期望保持 `/downloads-old/Film.mkv`。

**建议**：正向也按路径组件剥离根，并统一处理尾斜杠。增加兄弟目录、多重根、精确根、尾斜杠四组行为用例。

### I03 [P2] Metadata source 的一次无效成功响应会污染缓存七天，恢复网络仍不能恢复功能

**位置**：[crates/media/src/client.rs:398](<../../crates/media/src/client.rs#L398>)（`body_with_language` 在解析/校验前 `cache.put`）；同样模式存在 `douban.rs`、`tvdb.rs`、`bangumi.rs`、`anilist.rs` 的 `body`。

**触发**：原有正常 search/details 缓存过期；上游/中间代理暂时返回 HTTP 200 的 HTML 错误页、空体或结构不合法的 JSON；随后上游恢复。

**实际**：失败内容先覆盖可用 stale 并被标为新鲜。解析在上层发生，故返回错误；下次访问在 `client.rs:393-395` 命中新鲜坏缓存，根本不会再请求已恢复的上游。默认 TTL 是七天；过程重启也不能清除此持久化错误。现有 stale fallback 只接住 HTTP transport 错误，接不住解析失败。

**证据**：公开 `Tmdb::search_movie` 测试先使用既有 fixture 成功，推进固定时钟到 TTL+1，fake 返回 `<html>Temporary proxy error</html>`，之后恢复正常 fixture。最终调用仍得到 `Err(Json(Error("expected value", line: 1, column: 1)))`。

**建议**：按每个端点的实际契约验证响应再覆盖旧缓存；解析失败也保留/使用可用 stale；新鲜缓存读出后解析失败应失效并允许重拉。不能只做 JSON 语法验证，否则错误 envelope 仍会污染。

### I04 [P2] HTTP Login/Check-in 把返回登录表单的失败当成成功

**位置**：[crates/hooks/src/plugins.rs:78](<../../crates/hooks/src/plugins.rs#L78>)（Login），`:191-198`（Check-in）；生产 API 使用 [crates/api/src/http/sites.rs:443](<../../crates/api/src/http/sites.rs#L443>)，周期流程使用 `api/src/check_in.rs:36-43`。

**触发**：没有 Cookie 或 Cookie 失效；Site 将 attendance/login 请求重定向到登录表单并返回 200；反爬/验证码页也会发生同类响应。

**实际**：插件只要 `HttpPost` 没有 transport error 就成功，既不验证认证态，也不检查签到成功标志；Login 即使完全没有 Set-Cookie、仍无有效 Cookie 也保存后成功。`site_check_in` 返回 `ok:true`，周期 Job 打成功日志且不进入失败提醒/重试。实际未登录、未签到。

**证据**：fake HttpPost 固定返回 `<html><form action='takelogin.php'><input name='password'/></form></html>`，无任何 Set-Cookie。`LoginPlugin::login` 与 `CheckInPlugin::check_in` 两个公共 seam 都返回 `Ok`；两个拒绝错误响应的回归测试均失败。

**建议**：在每种 Indexer/Site Profile 定义明确的认证/签到成功、已签到与失败响应判定；将页面拿到与业务成功分开。无法验证的响应不能宣称成功，失效 Cookie 应触发更新凭证的可操作错误。真实登录所需凭证或人工 Browser 登录也需明确契约。

### I05 [P2] 打开 managed Browser 开关后，无外部 CDP 的 render 请求仍无条件失败

**位置**：[crates/indexer/src/browser.rs:118](<../../crates/indexer/src/browser.rs#L118>)；配置只建目录 `:39-52`；正式启动接线 [crates/api/src/main.rs:205](<../../crates/api/src/main.rs#L205>)。

**触发**：开启 `CRAWLER_MEDIA_BROWSER_ENABLED`，overlay profile 设置 `render:true`，Site 没有单独指定 `cdp_url`。

**实际**：`enable_in` 只创建 chromium 目录/HEADLESS 文件，生产使用的 `Browser::open` 没有下载或启动受管 Chromium，直接返回 `managed Chromium launch is not wired in tests; enable Browser and provide cdp_url or an opener`。此分支没有 `cfg(test)`，实际生产也一样。`chromium_present` 仅检查刚创建的目录，会将空目录当作已具备 Chromium。与 CONTEXT/ADR-0007 约定的开启后受管下载/运行不一致。

**证据**：在静态确认不会启动进程后，临时测试创建 `BrowserConfig::enable_in(tempdir)`，使用无外网的 data URL 通过公开 `fetch_html` 调用。立即得到上述错误；没有启动任何 Chromium。

**建议**：接通明确的 managed Browser 生命周期并验证可执行文件，或将当前模式显式限制为用户提供 CDP 并在配置时提前拒绝不完整组合。回归使用注入 session/launcher，不能依赖真实 Chromium。

### I06 [P2] Obscura 开关只启动守护进程，配置没有接入 Indexer 的 Browser

**位置**：[crates/api/src/main.rs:205](<../../crates/api/src/main.rs#L205>)；[crates/api/src/http/settings.rs:272](<../../crates/api/src/http/settings.rs#L272>)；[crates/indexer/src/browser.rs:55](<../../crates/indexer/src/browser.rs#L55>)。

**触发**：在站点设置打开 Obscura 集成并保存，服务显示已运行；对需要 JS 渲染的 Site 搜索。

**实际**：UI/API 存储 `obscura.enabled` / `obscura.url` 并启动进程，但创建 Fetcher 时完全不读取两者。`BrowserConfig::with_obscura` 在整个 `crates/` 中只有定义、没有调用。`ApiState.indexer` 已持有启动时创建的 `RoutedFetcher`；保存设置既不更新也不替换它。因而开启该功能前后，真正搜索仍使用原有每 Site CDP 或上面的失败分支；即使重启也一样。UI 承诺 Cloudflare/JS 请求调用此引擎（[web/components/site-config-section.tsx:1494](<../../web/components/site-config-section.tsx#L1494>)）没有实现。

**证据**：穷尽生产标识符引用：OBSCURA 配置仅出现在 settings/main 自启；`with_obscura` 无任何调用。此项是静态接线证明，没有启动/下载安装真实服务。

**建议**：Fetch 模式决策读取有效 Browser 设置，建立可更新的运行时路由，保存配置与重启应一致。测试可注入 opener，断言开/关切换后请求实际传给哪个 CDP，无需真实 Browser。

## 7. 需要调整的设计边界

这些建议来自上面的具体反例，未要求在本轮 review 中扩展实现：

1. **Library 是 owned 的权威，pending 只是进行中。** PIPE-01/03/04 表明质量、完成状态与已拥有槽位分散在 ledger、SubscribeFacts、pending 中，彼此恢复不一致。建立统一的 coverage/quality 读取服务；完成必须有实际 ledger，pending 仅用于抑制在途重复投递。
2. **提交新版本与删除旧版本要由同一个用例控制。** PIPE-02 表明 HTTP 与 worker 分别编排，安全顺序不同。先持久化可用的新版本与事实，再执行有记录、可重试的旧版本清理；文件系统与数据库不是一个事务，需显式补偿状态。
3. **身份、共享 episode 事实与文件派生缓存生命周期不同。** ROOT-01/02 表明删除一个文件被当成删除身份/共享 marker。将删除服务分层；Media 是否可回收按引用判断，marker 是否可清除按其来源、版本及用户锁定语义判断。
4. **Playback 的协议适配应共享状态规则和设备身份。** ACCESS-02/03/04/05/06 表明 web/Jellyfin 各自实现迁移、已看、设备解析与撤销。协议层负责解析，各入口调用同一个状态写服务；设备身份必须贯穿事件、持久化列表与流凭据。
5. **成功响应必须对应真实业务事实。** ACCESS-07/08、I01/04 的 200/Ok 分别掩盖写失败、丢指标、假下载及未登录。接口契约区分 transport 成功、动作接受、持久化成功和业务完成，并为失败提供可操作错误及结构化日志。
6. **配置开关需有对应运行时路由。** I05/06 表明开关/目录/进程状态不能证明 Browser 已可抓取。保存配置应验证能力并更新有效路由，重启与动态切换行为保持一致；测试注入 launcher/opener，避免真实 Chromium。
7. **marker 的候选不能被读路径隐式提升为事实。** ROOT-04/05 表明邻集候选、单模型成功和最终 Verified 尚未共享可信度边界。坚持目标窗口验证，并在提交前处理跨模型冲突；失败/无匹配/未检测分别持久化。

## 8. 验证与复现材料

### 缺陷复现

本轮共执行 25 个离线复现测试：16 个断言正确行为的用例失败，9 个断言现有缺陷的用例通过。这两种结果均是确认缺陷的证据，**不是修复后的通过结果**。I06 另以静态生产调用链确认。

| 复现文件 | crate | 用例结果 | 对应发现 |
|---|---|---|---|
| review_pipeline_tmp.rs | api | 5 failed | PIPE-01…05 |
| review_root_tmp.rs | api | 4 failed | ROOT-01…04 |
| review_root_tmp_models.rs | marker | 1 failed | ROOT-05 |
| review_access_tmp.rs | api | 9 passed（断言缺陷存在） | ACCESS-01…09 |
| review_integrations_tmp_incomplete_downloader.rs | api | 1 failed | I01 |
| review_integrations_tmp_path_map.rs | downloader | 1 failed | I02 |
| review_integrations_tmp_cache.rs | media | 1 failed | I03 |
| review_integrations_tmp_login.rs | hooks | 2 failed | I04 |
| review_integrations_tmp_browser.rs | indexer | 1 failed | I05 |

复现源码曾随报告保存在同目录附件 `2026-10-10-full-project-review-repro/`（保持原 crate/tests 路径），**该附件已于 2026-10-15 从工作树移除**，正文可由 `git show <本轮清理提交>^:docs/audits/2026-10-10-full-project-review-repro/README.md` 一类命令取回。它们不是已修复的正式回归测试，默认 `cargo test` 不会执行 docs 下源码。各发现正文包含独立触发步骤和实际结果，不依赖临时日志才能理解。

原始日志保存在 `/tmp/crawler-media-project-review-pipeline-repro.log`、`/tmp/crawler-media-project-review-root.log`、`/tmp/crawler-media-project-review-models.log`、`/tmp/crawler-media-review-access-test.log`，以及 `/tmp/crawler-media-project-review-integrations-repro/` 下的日志；临时日志可能被系统清理，长期证据以文档和保存的源码为准。

### 现有测试基线

`cargo test --workspace` 通过（exit 0）：1127 passed、0 failed、1 ignored。已移除 review 临时用例后运行；当前工作区包含其它会话的未提交变动。日志：`/tmp/crawler-media-project-review-workspace.log`。

前端 `npm run typecheck` 通过（exit 0）。报告和附件的 `git diff --check` 通过。现有测试通过只能说明已有用例未回归，本报告中的错误分支尚未修复，不能据此认为 25 项问题已消失。

本次没有创建 GitHub issue、没有修改业务源码或现有测试；报告与复现附件单独提交。未做实网或真实播放器验证的限制见第 2 节。
