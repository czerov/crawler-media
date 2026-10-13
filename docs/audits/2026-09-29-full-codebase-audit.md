# 全代码库审计：业务、数据库与配置、页面与接口

> **历史状态（2026-10-13 整理）：** 广域审计状态 **UNRECONCILED**；D/A/W/L/P/U/C 原发现及静态/执行证据保留，尚未逐 ID 重核当前实现、回归证据和残余风险。后续数据库/API review 不是这些发现全部关闭的证明。
>
> 原设计/ID/测试记录保留；旧执行、清理和提交命令不是当前执行授权，不应直接重跑。当前依据：[数据库/API review](<2026-10-13-database-api-review.md>) 与 [剩余计划](<../superpowers/plans/2026-10-13-database-api-followup.md>)；未覆盖的历史发现仍须独立对账。

## 1. 基线与验证边界

- 审计版本：`db73bf03e8eb454d24522b35ac8601bba4b127f3`。
- 覆盖清单：16 个 Rust crate、27 个页面入口、20 个 `web/lib/api` 模块及共享 HTTP 客户端、实际 HTTP 路由、启动脚本与 Docker 配置。
- 本轮只读分析源码和调用链；没有修改业务源码、重启服务、清除数据库、操作真实媒体文件或请求写入接口。本文件是本轮唯一新交付物。
- 不是逐行形式化证明：大型 API/UI 模块按功能入口、调用链和风险路径深入；不能据此宣称剩余代码没有 bug。
- 所有标为“确认”的问题均有可触发代码路径。多数为静态调用链确认，不代表已在运行中的用户环境复现。
- 运行了 `bash -n start-test.sh`：通过。父代理在内存 SQLite 中复核了空范围删除和默认库缺失两项 SQL 后果。
- 子代理以只读 `immutable=1` 检查现有 SQLite 主文件结构；不包含未 checkpoint 的 WAL，不读取密码、token 等凭据值。普通只读打开失败后调查采用 immutable，只用于结构观察。
- 本轮没有重新执行 cargo / Web 全量测试。之前的绿色测试只说明已有场景通过，不能覆盖本报告新增场景。
- 严重度：P1＝数据丢失、权限错误、错误文件操作或核心流程不可用；P2＝功能错误、配置无效、可靠性问题；P3＝低影响输入/提示缺陷。

## 2. 优先级最高：数据库与权限

### D01 · P1 · 清理空 Library 的历史会清掉当前 User 的全部历史

位置：[HTTP 范围计算](<../../crates/api/src/http/playback_logs.rs#L187-L220>)；[Store 删除范围](<../../crates/store/src/playback.rs#L327-L351>)，同类逻辑也用于 logs / metrics。

触发：`DELETE /api/v1/playback/history?scope=library&library_id=<存在但空的Library>`。结果集合为 `Some([])`，Store 用 `filter(!is_empty)` 丢弃媒体条件，执行仅带 user_id 的 DELETE。缺失或非法 item/library 参数也可能变成 None，扩大为全清。

影响：本应清空空库的操作删除当前 User 所有 Library 的进度、日志与指标；不是跨 User 删除。

验证：内存 SQLite 按当前动态 SQL 分支复现，删除了 1 条其他 Library 的记录。需要补真实 HTTP 的 empty/invalid scope 回归测试。

### D02 · P1 · 默认 Library 不变量可被普通 CRUD 打破，导致下次启动失败

位置：[删除 Library](<../../crates/store/src/libraries.rs#L454-L479>)；[启动 seed](<../../crates/store/src/schema.rs#L701-L729>)；[创建 Library](<../../crates/store/src/libraries.rs#L262-L280>)。

触发 A：已有第二个 movie Library，删除原默认 Library。删除只保护最后一个 Library，不提升替代默认。

触发 B：首次创建合法的 Video Library，创建时 is_default=0。

影响：留下“该 kind 有 Library、无 default”的状态；下次 Store 打开时 seed 发现已有库而不补默认，随后 `query_row(is_default=1)` 报缺行，后端启动失败。内存 SQL 验证删除默认后的 lookup=None。

建议：创建、删除、设默认同事务维护每个 kind 的默认不变量；迁移需要修复已有破坏状态，补 CRUD→关闭→重开测试。

### D03 · P1 · 删除受限 Library 后，其保留文件回退归属 everyone 默认 Library

位置：[删除实体和 roots](<../../crates/store/src/libraries.rs#L473-L478>)；[路径无归属回退默认](<../../crates/store/src/libraries.rs#L185-L191>)；[可见性判定](<../../crates/api/src/http/library.rs#L28-L75>)。

触发：管理员删除一个含 ledger 的 selected Library，仍保留同类型 everyone 默认 Library。

影响：ledger/文件没有删除，也没有明确转移权限；路径失去原 roots 后回退默认库，原无权 Member 可能看到原受限条目。PATCH 已有 roots ownership 防护，DELETE 没有等价保护。

### A01 · P1 · 全局 Browser 设置写接口没有管理员授权

位置：[member 路由注册](<../../crates/api/src/http/mod.rs#L293-L300>)；[写全局设置及进程启停](<../../crates/api/src/http/settings.rs#L524-L569>)。

触发：普通 Member 直接调用 PUT `/api/v1/settings/browser` 或 POST `/api/v1/settings/browser/sync-cdp`。

影响：修改实例级 CDP URL、User-Agent、Obscura 开关，并启动/停止全局进程；sync-cdp 可写共享 Site 凭据。前端隐藏设置入口不构成后端授权。现有测试仅覆盖管理员操作，没有 member→403 测试。

## 3. 下载、Subscribe、Transfer 与 Job

### W01 · P1 · 删除 Subscribe 时丢失 Downloader 路由，可能清理错误目标

位置：[丢掉 pending.downloader_id](<../../crates/api/src/http/subscriptions/deletion.rs#L142-L150>)；[始终调用默认 Downloader](<../../crates/api/src/http/subscriptions.rs#L376-L388>)。

触发：Subscribe/Site 指定非默认 Downloader，存在 pending，然后删除并勾选 delete_torrents。

影响：真实目标的任务遗留；默认 Downloader 恰有同身份任务时可能删除其文件。正常 Transfer 的 RoutedDownloader 保留 pending 路由，删除链路应共用这一规则。

### W02 · P1 · 自动 Transfer 忽略 Subscribe 指定 Library

位置：[后台 Transfer](<../../crates/api/src/worker.rs#L428-L430>)；[正确的手动路径](<../../crates/api/src/management/subscribes/run.rs#L109-L110>)。

触发：创建/修改 Subscribe 指向非默认 Library，等待自动 Transfer。

影响：后台使用默认 Library；手动运行却使用所选 Library，目的地分歧。可能将本应受限的文件放进公开默认库。

### W03 · P1 · Torrent 身份验证通过后，文件级 Media 身份没有继续验证

位置：[pick_release](<../../crates/subscribe/src/collect.rs#L233-L249>)；[目标 Media ledger](<../../crates/subscribe/src/collect.rs#L217-L229>)。

触发：正确 Torrent 内含另一个电影、sample.mkv，或者 TV 包里含另一个 Media 的同季集号视频。

影响：movie 视频无条件继承 Torrent Release；TV 主要核覆盖槽位，错误视频可被 Transfer 并标成目标 Media。Wash-cut 下还可能替换正确旧版。视频扩展名白名单不能解决文件身份。

### W04 · P1 · 多集 Wash-cut 用 any 批准，却对所有槽位覆盖和删除

位置：[任一槽位通过即批准](<../../crates/subscribe/src/choose.rs#L97-L128>)；[覆盖全部槽位并删旧路径](<../../crates/subscribe/src/collect.rs#L175-L208>)。

触发：E01 已有 score=100，E02 缺失；低分 score=10 的单文件 `S01E01-E02.mkv` 因 E02 获准。

影响：E01 的事实也被替换为 10，正确高分旧文件可能删除。需逐槽位授权，或对不可拆分多集文件制定不降级策略。

### W05 · P2 · running Job 被关闭后，失败仍重新排队重试

位置：[关闭定义](<../../crates/api/src/http/jobs.rs#L315-L323>)；[fail_claimed](<../../crates/jobs/src/store/lifecycle.rs#L48-L81>)；[领取](<../../crates/jobs/src/store.rs#L158-L168>)。

触发：系统 Transfer / Check-in 正在执行，管理员禁用定义，当前执行随后失败。

影响：失败分支不检查 def.enabled，实例重新 queued，领取也不检查定义；停用后仍最多继续多次重试并产生副作用。允许 running 外部工作自然结束是明确策略，本问题仅是停用后的新增重试。

### W06 · P2 · 多集字幕回退到第一个视频，发生错配和覆盖

位置：[字幕目的地匹配](<../../crates/subscribe/src/sidecars.rs#L35-L63>)。

触发：季包多个视频被 Naming template 重命名，原字幕 stem 与新视频 stem 不相等。

影响：所有未命中字幕都选 video_dests.first；同扩展覆盖，后续集没有对应字幕。应保留 src-video→dest-video 映射并匹配季集/语言。

### W07 · P1 · 电影 token 子集放宽重新引入不同电影误匹配

位置：[core_titles_compatible](<../../crates/downloader/src/identity.rs#L38-L49>)。

触发：`The.Matrix.1080p` 与 `The.Matrix.Reloaded.1080p`，年份未知、大小未知或兼容；电影核心 tokens 为子集，后续 names_match 的重合判定也通过。`Alien` 与 `Alien Covenant` 在年份未知/兼容时同理。

影响：跳过真正添加、收集错误文件、remove 错误任务。Alien/Aliens 测试只验证词形，不验证同语言后缀续作。双语别名放行应限制额外 token 的性质，或者引入稳定 torrent 身份，不能将任意标题子集视为双语别名。

### W08 · P1 · 并行 qB 添加共用一份可变临时 Torrent 文件

位置：[临时文件命名和写入](<../../crates/downloader/src/qbit.rs#L89-L102>)。

触发：同进程并发提交两个下载；临时路径只含 PID 和常量 tag，没有每次请求 nonce。

影响：第二次写入截断/覆盖第一次尚未上传完的文件，导致错误 Torrent 或无效字节上传。Multipart 使用文件读，不保证构造时复制 bytes。Library Transfer 自身临时文件有 nonce，但 qB 这条路径没有。

### W09 · P1/P2 · 网络 deadline 仍有遗漏，无法宣称所有调用已有 5 秒保护

位置：[qB Torrent 预取](<../../crates/downloader/src/qbit.rs#L285-L298>)；[Check-in HTTP](<../../crates/api/src/check_in.rs#L107-L130>)；[CDP 同步](<../../crates/indexer/src/cdp.rs#L36-L74>)。

触发：对端接受连接后不回 headers/body，或者 CDP WebSocket 不发预期消息。

影响：bare ureq get/post 未采用有限超时 Agent；CDP 的 elapsed 轮询不能打断 blocking socket.read。投递、Check-in 或 cookie 同步可无限等待。已设置全局 timeout 的 qB RPC / Transmission RPC 不在本问题内。

### W10 · P2 · S01 后的 720p/480p 被误读为 episode

位置：[边界解析](<../../crates/release/src/boundary.rs#L91-L99>)。

触发：`24 S01 720p WEB-DL`。

影响：数字被读取成 episode=720，没有检查尾随 p，整季 Torrent 不再按 season pack 处理。1080p 当前仅因数值>=1000绕过这一分支，不能证明边界正确。

### W11 · P2 · Site 专属 proxy 没有传给登录和 Check-in HTTP

位置：[Hook 请求接口调用](<../../crates/hooks/src/plugins.rs#L184-L185>)；[生产 HTTP 实现](<../../crates/api/src/check_in.rs#L107-L130>)。

触发：Site 只能通过设置的专属代理访问；搜索能走该代理，维护请求却只传 URL/body/cookie。

影响：登录/Check-in 失败，或使用错误出网路径。接口签名无法承载 Site.proxy。

### W12 · P2 · 生产 Browser rendering 只有注入测试接缝，没有真实 opener

位置：[Browser.open](<../../crates/indexer/src/browser.rs#L76-L119>)；[生产构造](<../../crates/api/src/main.rs#L171-L179>)。

触发：render=true 的 Indexer，开启 Browser 或提供 cdp_url。

影响：生产仅 Browser::new，opener=None；所有 render 分支报错，不会实际启动或 attach。现有 Browser 测试是 with_opener 假实现，不能证明生产渲染可用。

## 4. Library、Metadata、Playback 和封面

### L01 · P1 · Move 最终发布失败时，源文件已消失且没有回滚

位置：[transfer_file](<../../crates/library/src/lib.rs#L64-L95>)。

确定触发：src 是普通文件，dest 已存在且是目录。Move 先 src→temp；temp→dest 失败，remove_file(dest目录)失败，返回 Err。

影响：原 src 已不存在，数据只留在隐藏的 .transfer 文件；调用方不会登记或还原。跨设备分支也在最终发布前删除源。应在最终发布成功后才删除源，或实现可靠回滚。

### L02 · P1 · 删除父 Library 条目会越界删除嵌套 Library 文件

位置：[delete_item 筛选与删除](<../../crates/api/src/http/library_delete.rs#L35-L64>)。

触发：同类型 A root=/media，B root=/media/private；对 A 删除某 Media，该 Media 在 B 也有文件。

影响：使用 starts_with 而非最长 roots 的唯一归属，B 文件也被删除。仅管理员可调用，属于删除作用域错误，不是 Member 提权。

### L03 · P2 · Catalog 详情 fallback 把不同 source 的数字 ID 混用

位置：[Fanout.details](<../../crates/api/src/catalog_fanout.rs#L428-L435>)；[Douban details](<../../crates/api/src/catalog_fanout.rs#L105-L107>)。

触发：TMDB ID 详情失败/未配置且无 stale cache；fallback 将相同数字交给 Douban。

影响：豆瓣将其当 subject id，可能返回完全无关 Media。缓存 SQL 的 source 隔离没有问题；错误是 source 调用时的 ID 语义。

### L04 · P2 · organize 没有验证 from/to 的 Library 归属

位置：[organize](<../../crates/api/src/http/library_organize.rs#L103-L144>)。

触发：向 Library A 的 organize 发送指向 B 文件或库外目的地的 renames。

影响：仅验证 A 存在，就操作任意进程可访问文件。管理员接口也应限制操作作用域并验证 preview/ledger 对应关系。

### L05 · P3 · 六字节非 ASCII 颜色触发 UTF-8 切片 panic

位置：[Color.from_hex](<../../crates/cover-generator/src/color.rs#L20-L27>)。

触发：封面背景 hex_color=`0你00`，长度为 6 bytes，索引2不是 UTF-8 字符边界。

影响：panic 由 spawn_blocking 转为 HTTP 500，而非有效参数错误/颜色 fallback；不等于整个服务必然崩溃。

### P01 · P2 · Playback 分集忽略请求季号，上一/下一集跨季混淆

位置：[episodes handler](<../../crates/api/src/http/playback.rs#L151-L197>)；[前端下一集](<../../web/components/player/player-page.tsx#L107-L127>)。

触发：只有 S1E1 和 S2E2，播放 S1E1。后端丢 season_number 参数返回所有季；前端只比较 episode_number，取 E2 后强行配当前 season=1。

影响：按钮导向不存在的 S1E2；同集号多季时 currentEpisode 也可拿错名称。应按 season+episode 标识，并在后端落实筛选。

## 5. UI 和实际 API 契约

### U01 · P2 · 合集条目跳转使用 /items/，真实路由是 /item/

位置：[卡片 href](<../../web/components/collections-detail-page.tsx#L53-L56>)；[实际路由](<../../web/src/main.tsx#L70-L72>)。

触发：点击带 library_id 的合集条目。

影响：必然落入 404；后端正常返回这些字段，不是仅未使用支架。

### U02 · P2 · 筛选发现墙仅第一 TMDB 页，却报告全部

位置：[前端](<../../web/lib/api/discover.ts#L510-L533>)；[后端](<../../crates/api/src/http/discover.rs#L581-L608>)。

触发：任意匹配超过第一页的 genres/country/year 等筛选。

影响：page 被 void，后端不带页码，前端总数伪造为当前条数且 hasMore=false，无法续载。

### U03 · P2 · 原名与 genres 没有透传，片单类型筛选和原名搜索不可用

位置：[DTO](<../../crates/api/src/http/discover.rs#L482-L499>)；[前端映射](<../../web/lib/api/discover.ts#L359-L364>)。

触发：打开真实完整片单，尝试类型筛选或搜索已加载中文标题的英文原名。

影响：genres固定[]导致类型栏不显示；originalTitle=title导致原名搜索无结果，Hero 信息也缺失。

### U04 · P2 · 旧发现书签重定向到后端不接受的 collection ID

位置：[high-score](<../../web/app/(app)/discover/movie/high-score/page.tsx#L5>)；[允许的ID](<../../crates/api/src/http/discover.rs#L633-L640>)。

触发：访问 high-score/top250 兼容路由。它们导向 movie_high_score/movie_top250，豆瓣只接受 top-rated。

影响：兼容书签必然请求404，未完成兼容迁移。

### U05 · P2 · 编辑非默认 Downloader 限制实际修改默认 Downloader

位置：[API 丢 _id](<../../web/lib/api/downloaders.ts#L313-L340>)；[handler固定默认](<../../crates/api/src/http/downloaders/instances.rs#L351-L377>)。

触发：设置页面选择 Downloader B 的限速弹层，B 不是默认 Downloader。

影响：实际读写 A 默认 Downloader。弹层的备用速度/queue/max_active 等字段也未序列化，保存不生效，应补对应契约或移除不可用字段。

### U06 · P2 / 未实现可见入口 · 片源标注弹层接的是假空 API

位置：[候选恒空/假回执](<../../web/lib/api/libraries.ts#L1637-L1651>)；[弹层调用和拦截](<../../web/components/media-source-annotation-dialog.tsx#L51-L73>)。

触发：Subscribe inspector / upgrade-run 弹层打开片源标注。

影响：没有网络请求，恒返回[]，界面错误地告诉用户没有未知片源文件，apply永远被阻止。不是“真正没有文件”，是接口已删但入口仍保留。

## 6. 配置与部署缺陷

### C01 · P2 · 启动脚本全局清空代理，破坏子进程访问外网

位置：[代理处理](<../../start-test.sh#L17-L20>)；[局部 curl](<../../start-test.sh#L58-L60>)。

条件：开发机 cargo / Node / Metadata source 需要环境代理才能访问公网。

影响：为了本地 health 直连，脚本 unset 所有代理、NO_PROXY=*，这些变量也被 cargo/backend/frontend 继承，导致外网访问失败。已经有 local_curl --noproxy，应该仅局部绕过本地请求，保留子进程需要的代理。

### C02 · P2 · TCP 监听被当 HTTP 就绪，且 Token 失败也显示成功

位置：[就绪 OR 条件](<../../start-test.sh#L153-L160>)。

影响：health失败只要TCP能连接便返回成功；jobs认证失败的else与then完全一样。脚本可能声称“就绪”，实际HTTP/认证不可用。应保留HTTP语义并输出curl exit/status/耗时，不能靠放宽成功条件掩盖原问题。

### C03 · P2 · “已等待15秒”并非真实截止时间，其他探测没有deadline

位置：[30次循环](<../../start-test.sh#L153-L171>)；[状态探测](<../../start-test.sh#L295-L301>)。

影响：每轮health可用2秒再sleep0.5，总耗时可达约75秒，不是15秒。前端/status qB请求缺少max-time也可无限等待。需要墙钟deadline与统一有限探测。

### C04 · P2 · 绝对 DATA_DIR 被拼成错误目录

位置：[传递数据目录](<../../start-test.sh#L139>)。

触发：`DATA_DIR=/tmp/cm-test ./start-test.sh start`。

影响：变成 `$ROOT_DIR//tmp/cm-test`，并非指定绝对目录；可能意外初始化另一个数据库。需明确解析相对/绝对路径。

### C05 · P2 · 提示的密码只是初始配置值，不能保证现有数据库可登录

位置：[脚本密码配置](<../../start-test.sh#L29>)；[bootstrap Keep](<../../crates/api/src/bootstrap_credentials.rs#L114-L121>)。

触发：管理员已在 UI 改密，或启动时改变 ADMIN_PASSWORD。

影响：安全现有用户走Keep，环境变量不重置密码；脚本仍把该变量打印为“密码”，容易再次误导。应提示“初始密码（仅首次初始化/迁移）”，不要承诺等于当前密码，也不要默认打印长期token。

### C06 · P2 · Docker runtime 缺少功能依赖 ffmpeg/ffprobe

位置：[运行镜像](<../../Dockerfile#L14-L17>)；[探测外部程序](<../../crates/library/src/lib.rs#L118-L146>)。

影响：镜像仅安装ca-certificates，实际探测、截图/章节等调用外部ffprobe/ffmpeg失败。探测失败部分有Release fallback，所以不是整个Transfer必停，但容器部署不能实现完整元数据/图像/章节能力。需提供工具或明确禁用相应功能并测试部署镜像。

### C07 · P2 · Compose 的部分 _FILE 支持只写在注释中

位置：[Compose env](<../../docker-compose.yml#L31-L58>)；[ServerConfig secret支持](<../../crates/api/src/config.rs#L103-L109>)。

触发：仅在主机设置 CRAWLER_MEDIA_QB_PASS_FILE / TMDB_KEY_FILE / TVDB_KEY_FILE，然后使用默认 Compose。

影响：Compose 没有把这些变量传进容器，注释不是 environment；即使挂载secret路径也不能使用，和“所有保密变量支持”的使用说明不一致。token/admin 的 FILE已实际列入，不能把这两项报为缺失。

## 7. 未实现/遗留支架，和确认 bug 分开

- Browser rendering：W12，是生产可触达的未实现能力，需接真实 opener或明确禁用。
- 片源标注：U06，是用户可见入口，优先修复/下架，不应返回假成功。
- 旧 trash/duplicate API 导出返回空列表/零回执；当前 Library manage 已无入口，属于遗留兼容债务，不作为当前可见 bug 计数。
- 部分 Site protection/boost/pause 导出没有真实写入；需要按产品保留功能逐项补齐/移除。
- Playback diagnostics/ping/stop、字幕字体/trickplay 扩展消费者与真实路由不一致；当前后端主要固定 direct-play，session_id=null、部分音轨/字幕数组为空。不能把当前不可达转码路径全部报为用户已发生错误，应作为能力差距列清。
- trailers/videos 等返回固定空数组；若产品仍展示能力需完成契约，不能仅用空列表模拟有实现。

## 8. 数据库结构与配置合理性观察（不全部归为 bug）

- Store 使用多个 SQLite 文件、WAL和busy_timeout；跨文件关系无法直接用外键，应保留明确事务/补偿与唯一归属策略。
- 只读主文件观察到 `data/live` app/catalog/library/subscribe schema_version为3/1/4/2；普通data为2/1/2/2。不要仅凭这个差异判断迁移失败，文件使用时间与WAL快照可能不同。
- live主文件表数13/1/7/8；外键数0/0/1/0。唯一看到的probe_job_units→probe_jobs CASCADE，Store初始化没有显式启用foreign_keys。需核删除调用是否总手动清理，外键声明当前不应当作自动保护。
- playback单位主键包含user/media/season/episode，sessions包含user/device，collections项包含collection/media，pending包含subscribe/enclosure；本轮未核实到这些主键造成跨User冲突。
- CLI token与管理员密码分离、Argon2id、改密原子撤销、Rotate撤销旧普通会话等近期修复在当前代码可见；没有再次报已修复旧问题。
- Settings KV和环境覆盖分散在多个层；需要文档列出env pinned、DB hot reload、restart-only三种语义。初始密码是bootstrap而非每次启动reset。

## 9. 审计覆盖矩阵

| 模块 | 本轮深入的路径 | 核实结果 |
|---|---|---|
| domain | 身份/Media kind/Coverage、调用类型 | 无独立确认bug；不表示全部不变量已证明 |
| store | 全部源模块、schema/open、users、ledger、libraries、playback、collections、probe | D01–D03；事务/主键交叉检查 |
| downloader | qB/TR/动态路由、匹配、temp、HTTP | W07–W09 |
| release | boundary/attributes/parser及fixtures | W10 |
| filter | include/exclude/score、size、调用映射 | crate未独立确认；API size非法界限静默变None需补校验 |
| subscribe | choose/add/collect/facts/sidecars | W03–W04、W06 |
| jobs | claim/recover/finish/fail/definition/schedule | W05；旧attempt CAS与active保护已排除 |
| indexer | HTTP/CDP/Browser/profiles/parser | W09、W12 |
| hooks | login/Check-in/Hook bus | W11、HTTP deadline |
| library | Transfer/watch/naming/probe、删除/organize | L01–L02、L04；Naming常规路径遍历初猜已排除 |
| media | source adapters/details/cache/Fanout | L03；缓存SQL source隔离没有误报 |
| marker | target/intro/outro/queue/persist | 未确认独立算法bug；STRM边界和probe时限列风险 |
| playback | HTTP分集/状态/marks/history/范围 | P01、D01；大量扩展能力需区分支架 |
| media-server | Jellyfin auth/stream/range/provider/visibility | 未确认独立协议安全bug；共享Library影响见D03 |
| cover-generator | Color/background/HTTP生成 | L05 |
| api | 实际路由、权限、worker/deletion/settings/config | A01、W01–W02与UI契约 |
| Web | 全route树和27页面入口、20API模块、重点组件 | U01–U06、P01；并非逐行读取全部components |
| 部署与脚本 | Dockerfile/Compose/config/runtime_downloader/start-test | C01–C07 |

## 10. 已排除的风险与测试缺口

本轮不把以下推断列为已证实 bug：

- 静态 artwork故意公共路由，不要求Bearer不是遗漏。
- STRM direct-play返回URL和NAS内网探测通常是设计行为；没有低权限写入/可见性绕过证据时，不把它们直接定为SSRF或凭据泄漏。
- Naming模板常规 ../和leading /经当前collapse处理，不能仅看root.join定路径遍历。
- 缓存hit本身不是错误；source隔离已在SQL实现。
- running Job不立即杀外部工作为策略，不等于W05允许停用后新增retry合理。
- 未运行的旧API导出不等于用户可点击错误；U06已核实真实入口，其他支架单列。

需要补的测试：

1. 破坏性操作矩阵：空集合/非法参数/嵌套库/受限库/失败回滚。
2. Library CRUD→Store重开默认不变量测试。
3. Member对所有全局写接口的403矩阵，尤其Browser设置。
4. 多Downloader、多Library的自动Transfer和删除Subscribe行为测试。
5. 多槽位低分文件、夹带视频、多集字幕与并发qB上传。
6. accepted-but-stalled HTTP/CDP fake对端，证明deadline真正生效。
7. 真实React组件/API行为测试：合集链接、分季播放、筛选续载、所选下载器限速、片源标注。
8. 部署镜像功能测试：ffprobe/ffmpeg存在并可执行，secret_FILE确实传递。
9. Shell fake server场景：health失败但TCP开、错误token、代理保留、绝对DATA_DIR、真实wall-clock截止、失败日志包含curl状态。当前用TCP替代health会掩盖问题。

前端现有api-contracts/collection-grid/search-flow等大量使用源码正则与纯函数测试，不能证明真实请求和DOM行为。Rust已有fake Catalog/oneshot测试，但没有覆盖本报告的若干跨模块异常路径。

## 11. 建议修复顺序

1. D01/D03/A01：范围删除与权限边界。
2. D02：默认库不变量及已有坏数据恢复，防止服务无法启动。
3. W01/W02/W04/L01/L02：路由、作用域、质量不降级与文件回滚。
4. W07/W08/W09：可靠Torrent身份、临时文件唯一、所有外部调用有限deadline。
5. P01/U01/U05：播放器与用户编辑错目标。
6. U02/U03/U04/U06/W10/W11/W12：补齐真实功能而非空回执。
7. C01–C07：启动/配置/部署契约；不要再凭一次本机成功宣称用户启动超时已根治。

每项应先写可触发公共接缝测试再修复，独立提交。完成以新测试证据为准，不以源码regex命中或缩短超时/放宽成功条件为准。
