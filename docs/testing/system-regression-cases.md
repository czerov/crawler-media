# 全系统回归用例（前端视角）

## 目标与范围

从用户可见的入口和工作流检查 Web 工作台，以及 VidHub 通过 Jellyfin-compatible API 访问本系统时的页面呈现、图片加载、播放和进度同步。本文覆盖当前 Web 路由、设置分区和关键跨页流程；用例以可观察结果为准。

测试环境必须是隔离实例，数据写入临时目录和测试数据库。不得用生产 Site、真实 Downloader、真实 Library 文件目录或真实 User 的观看记录执行有写入的流程。

## 执行方式与判定规则

### UI 操作是主验收

所有表格用例都必须通过运行中的真实 UI 执行：

- Web 用例在真实浏览器打开本地 Web 工作台，通过鼠标/触控点击、键盘输入、滚动、浏览器前进/后退和刷新完成步骤。
- VidHub 用例在 VidHub app 中通过实际可见控件完成连接、导航和播放操作。
- 每条用例的步骤内不允许直接调用 API、改数据库、注入 React 状态或用脚本跳转到流程中间。测试数据可以在开始前用 fixture/reset 脚本准备；断言以可见 UI 为主，可额外检查服务端记录或临时目录确认结果。
- 浏览器 UI 自动化可以代替手工输入，但必须操作真实浏览器页面和真实 app 渲染后的控件；DOM/API 直调只能作为诊断证据，不能替代交互步骤。
- 每条用例都记录实际 UI 起点和结束状态。未执行的用例不得记为通过。

### 结果状态和失败原因

每条用例只能标记为 Pass、Fail、Blocked 或 Not run：

- **Pass**：所有 UI 步骤完成，页面可见结果符合预期。
- **Fail**：UI 流程已执行，但出现了与预期不符的显示或行为；必须填写失败原因。
- **Blocked**：环境、账号权限、依赖服务或测试数据阻止流程开始/继续；写清阻塞原因和恢复条件，不算作 Fail，也不算通过。
- **Not run**：没有执行；不得计入通过率。

每个 Fail 项必须记录：

1. 用例 ID 和失败步骤编号；
2. 预期显示/行为与实际显示/行为；
3. 原因判断及证据。原因可归类为显示/布局、控件交互、路由、数据呈现、权限、下游服务或环境问题；证据不足时写“原因待定位”，不得猜测；
4. 复现条件（User 角色、浏览器或 VidHub 版本、视口/设备、服务端构建号）；
5. 截图或录屏路径，以及控制台/服务端日志中的关联信息（如有）；
6. 是否可重现及缺陷链接（如已创建）。

不要只写“UI 不通过”或“功能异常”。环境阻塞写为 Blocked，并说明解除阻塞所需条件；确认是产品 UI 或交互异常后才记为 Fail。

### 当前可直接运行的检查

在 web 目录运行：

- pnpm test：运行现有前端逻辑与组件契约测试。
- pnpm typecheck：检查 TypeScript 类型。
- pnpm lint：检查前端静态规则。
- pnpm build：构建 Web 前端。

后端当前工作流要求：对本次触及的每个 Rust crate 运行对应的 cargo test -p 命令，并在该工作项结束时运行 cargo test --workspace。API 与 Playback 的测试使用 fake / fixture；不访问真实 Metadata source、Site、Downloader 或 Chromium。

### UI 流程执行顺序

1. 启动隔离实例并准备合成数据；记录服务端构建号、浏览器版本/视口，或 VidHub app/设备版本。
2. 通过真实 UI 登录并进入用例起点。每个用例按步骤从头操作，不通过 API 或地址栏直达中间步骤。
3. 观察页面可见内容、控件状态、路由和反馈；截图记录关键成功状态。检查服务端记录和文件只用于确认 UI 操作产生的结果。
4. 一旦不符合预期，保留当前页面并截图/录屏，记录最后一个成功步骤、首次失败步骤、实际表现和失败原因；再收集控制台或服务端日志辅助定位。
5. 按结果状态规则记录 Pass、Fail、Blocked 或 Not run；修复后重跑失败用例及相关回归项。

现有 web/package.json 提供单测、类型检查、lint 和 build，但没有配置 Playwright 等浏览器 E2E runner。这些命令只能作为辅助质量门槛，不能代替上述真实 UI 操作。要在 CI 中稳定重放浏览器流程，需要单独补 E2E harness 和测试数据装载入口。

VidHub 用例通过已安装的 VidHub app 连接隔离实例执行。第一次执行可通过 app 的常规服务器登录流程；测试完成后清除该隔离服务器连接。VidHub 流程记录 app 版本、设备、系统版本、服务端构建号、用例 ID、截图或录屏、结果和复现步骤。

## 测试数据和环境前置

准备以下可重置数据：

- 一个管理员 User 和两个成员 User；成员甲可见 Library A，成员乙不可见 Library A。
- 一条电影 Media、一条 TV Media，TV Media 至少含两个 Season、一个缺集、一个特别篇和一个已观看单集。
- 两个 Library（电影与 TV），分别包含有海报/背景图、缺少 artwork、多个文件版本、可播放文件和 Unidentified 文件的条目。
- 至少一个高置信度与一个低置信度的 Release；低置信度 Torrent 和文件都应进入 Unidentified 流程，不得自动 Transfer。
- 一个 fake Site/Indexer，返回匹配候选、Filter 拒绝候选、免费/HR 标记候选、空结果和单 Site 超时；另一个 Site 正常返回，用于验证并发搜索降级。
- fake Downloader 覆盖排队、下载中、暂停、完成、异常和找不到任务；准备 qBittorrent 与 Transmission 配置样例。
- 可探测的电影文件和 TV 文件：包含已知分辨率/编码、两种音轨、字幕、章节，以及可模拟的播放时长和进度。
- 可切换的 Metadata source fake、Scrape artwork/NFO fixture、Watch directory 临时目录和可回滚的 SQLite 数据库。

每条用例完成后检查 UI 展示，也检查对应的持久化事实或本地文件结果；不以内部函数调用次数或 SQL 文本作为验收。

## 用例

优先级：P0 为主链路或身份边界；P1 为常用功能与异常恢复；P2 为辅助入口和呈现细节。

### 登录、导航和首页

| ID | 优先级 | 用例与步骤 | 预期结果 |
|---|---|---|---|
| AUTH-01 | P0 | 未登录打开 /、/library 和 /settings/sites；完成登录；刷新深层路由。 | 私有页面不会短暂闪现；未登录进入登录页；登录后回到目标页面；刷新后仍能正确载入。 |
| AUTH-02 | P0 | 用有效凭据登录；输入错误凭据；退出；使用浏览器后退返回受保护页面。 | 错误状态有可读提示；退出后令牌失效且私有数据不可见；后退不会恢复已退出的工作台。 |
| AUTH-03 | P0 | 管理员和成员分别打开设置入口、成员管理、Library A 和任务操作入口。 | 管理员看到允许的全局入口；成员只看到个人信息与外观设置；直接访问越权路由时不给出数据且显示可理解的拒绝状态。 |
| NAV-01 | P1 | 从首页、详情、设置分区之间导航；用页面返回、浏览器后退和深层链接重复进入。 | 当前路由、标题、返回目标一致；查询参数和列表位置按设计恢复；不存在错误路由或空白页。 |
| NAV-02 | P1 | 在桌面与窄屏视口分别遍历侧栏、底部导航、对话框、表单和列表；切换可用主题后刷新。 | 控件不被裁切或遮挡；主题与偏好持久化；所有操作可用键盘/触控完成；加载、空数据和错误状态均有内容。 |
| HOME-01 | P1 | 打开首页，检查最近新增、继续观看、正在执行的 Job、收藏或发现入口；从每个卡片打开详情再返回。 | 卡片信息与对应数据一致；图片失败有 fallback；返回后保留合理的滚动位置和筛选。 |
| HOME-02 | P1 | 首次配置不完整时，从概览提示进入 Site、Downloader 和 Library 配置，再返回首页。 | 下一步入口指向正确页面；完成状态及时更新；未配置时有可操作说明，不显示虚假成功。 |
| MY-01 | P1 | 在 Netflix 主题移动端打开 /my；检查 User 信息和管理员快捷入口（包括新任务）；逐个打开 Subscribe、Transfer、Job、缓存、活动与设置入口；再以成员 User 检查入口；在 Silver 主题直接打开 /my。 | User 信息与活动角标正确；入口落到对应页面或创建界面；成员不显示管理员专属入口；首次加载主题偏好时页面不闪退到首页；Silver 主题按设计返回首页。 |
| MY-02 | P1 | 在 /my 打开切换账号列表并切换 User；随后退出登录；若浏览器还有其他已登录 User，再检查切换结果。 | 切换后头像、数据和权限均更新；退出后个人缓存清理且进入登录页或另一个可访问页面；不会恢复前一 User 的受保护内容。 |
| ROUTE-01 | P2 | 打开 /health、任一未知路由和已失效的详情链接。 | health 页面呈现服务状态；未知路由显示 Not Found；失效数据显示空/不可用说明并提供返回入口。 |

### 全局搜索、发现与 Media 详情

| ID | 优先级 | 用例与步骤 | 预期结果 |
|---|---|---|---|
| SEARCH-01 | P0 | 使用全局搜索分别选择 Media、Torrent、Library 垂直类别；输入命中、无命中和特殊字符查询；打开结果并返回。 | 类别切换不串数据；结果归属和摘要清楚；空结果可恢复查询；历史按 User 隔离并可清理；返回恢复查询上下文。 |
| SEARCH-02 | P1 | 在 Torrent 搜索中按 Site、类别和关键词搜索；切换列表/图览/分组，排序、筛选、翻页并打开单条结果。 | 候选字段、免费/HR 标志、做种数和体积展示正确；单 Site 出错时仍显示其他 Site 结果与降级提示；排序和展示模式符合选择。 |
| DISC-01 | P1 | 打开电影与 TV Media 发现页；调整年份、类型、评分、片长和排序；加载更多后进入 Media 详情再返回。 | 筛选可组合且结果同步更新；空结果、加载错误可恢复；返回保留筛选和滚动位置。 |
| DISC-02 | P2 | 打开高分榜、榜单提示页、人物详情、Metadata source 发现合集详情和本地合集列表/详情。 | 静态路由不会被动态路由误匹配；榜单提示与实际路由行为一致；人物与合集 artwork、条目数和可点击 Media 正确。 |
| MEDIA-01 | P0 | 从搜索和发现分别打开电影 Media 与 TV Media 详情；核对概要、年份、评分、Metadata source、artwork、Library 状态和 Subscribe 状态。 | 同一 Media 的标识和元数据一致；缺字段有 fallback；海报、背景图和静态图片可加载；操作按钮根据现状显示正确。 |
| MEDIA-02 | P1 | 从 Media 详情执行收藏/取消收藏、创建 Subscribe、打开播放入口、复制/分享链接；刷新后复核。 | 状态变更即时且持久化；重复点击不产生重复记录；分享地址可重开并落在相同 Media。 |
| MEDIA-03 | P2 | 直接打开 Douban Media 路由及别名已合并的 Media。 | 别名解析到同一个内部 Media；无法匹配时呈现可继续操作的空状态，不创建重复 Media。 |

### Subscribe、Torrent 和 Job

| ID | 优先级 | 用例与步骤 | 预期结果 |
|---|---|---|---|
| SUB-01 | P0 | 从电影 Media 创建 Subscribe，选择 Filter、Fetch mode、Downloader 和搜索策略；确认后打开列表与详情。 | 范围、策略和 User 所属关系正确；创建后出现对应搜索 Job；重复提交不会产生重复 Subscribe。 |
| SUB-02 | P0 | 为 TV Media 创建一个 Season 和 episode window 覆盖范围；分别切换关键词、RSS、两者并用；检查缺集页。 | coverage 与缺集事实逐集显示；特别篇不被误认为普通 episode；Fetch mode 选择影响执行并在详情可见。 |
| SUB-03 | P0 | 对 Subscribe 暂停、恢复、手动运行、修改 Filter/计划，再刷新列表和详情。 | 状态和计划持久化；暂停期间不会启动新搜索；手动运行有执行中的 Job 和终态；操作结果有明确反馈。 |
| SUB-04 | P0 | 对已拥有文件的 Subscribe 启用 Wash-cut；fake Indexer 返回较低、相同和更高 quality 的 Torrent，运行后检查每个 episode。 | 只接纳满足替换条件的候选；完成度按 episode 更新；新旧文件替换与清理符合 Transfer mode 规则；没有候选时保留现有 Library 文件。 |
| SUB-05 | P1 | 在 Subscribe 列表按状态/Media 查询；查看 coverage、quality、最近执行和异常；删除一个测试 Subscribe。 | 列表与详情事实一致；删除需要明确确认；删除后其 Job 定义按产品约定处理，其他 User 的 Subscribe 不受影响。 |
| FILTER-01 | P1 | 在设置中创建、编辑、复制和删除 Filter；配置内置和用户自定义 atom；在预演中输入匹配/拒绝 Torrent。 | 拒绝规则和最高命中 priority 的 score 结果清楚可解释；Filter 被 Subscribe 绑定后仍显示正确版本；无效条件给出字段提示。 |
| JOB-01 | P0 | 在任务中心切换全部、进行中、需要处理、已结束；展开 Job 与下载条目阶段；按状态筛选并刷新。 | Job 与 Downloader 状态区分清楚；数量、时间线和阶段与事实一致；刷新不会重复创建或丢失操作结果。 |
| JOB-02 | P1 | 对可重试 Job 执行重试/取消；忽略和撤销忽略；处理 fake Downloader 失联、任务缺失和 Site 超时。 | 仅允许合法状态转换；异常上下文可见且能定位 Site/Downloader；可恢复情形提供重试；不可恢复时保持原数据并说明原因。 |
| DLT-01 | P1 | 对下载条目执行暂停、恢复、取消、移除和刷新状态；选择“只移除 Downloader 任务”与“连同文件处理”的可用选项。 | 操作作用于选中的 Torrent；文件删除/保留与所选动作一致并有明确确认；失败后仍可重试，其他 Downloader 任务不受影响。 |
| ACT-01 | P1 | 打开活动页，切换 User/工作流范围，等待新的 fixture 事件；进入关联 Media、Subscribe 或 Job。 | 新事件按时间更新且不重复；范围筛选生效；每条事件跳到正确详情；空闲时不产生误报。 |

### Library、Transfer 和 Scrape

| ID | 优先级 | 用例与步骤 | 预期结果 |
|---|---|---|---|
| LIB-01 | P0 | 创建电影与 TV Library；编辑名称、kind、root paths、访问范围和收藏范围；运行扫描；刷新页面。 | 字段和 User 可见范围持久化；扫描结果正确建立 Library ledger；访问未授权的 Library 不返回条目。 |
| LIB-02 | P0 | 浏览 Library；组合类型、年代、地区、观看状态、评分、片长、语言、resolution、HDR 和库存筛选；切换排序/布局。 | 筛选组合与计数一致；已观看/未观看和库存状态正确；图片加载失败有 fallback；翻页/虚拟列表无重复或跳项。 |
| LIB-03 | P0 | 打开电影和 TV Library detail；查看 Media、Season、episode、缺集、文件版本；从详情进入 Media item 和播放。 | 详情数据与 Library ledger 一致；Season/episode 层级正确；打开的 item 与播放 URL 对应；静态 artwork 不要求浏览器携带自定义 Authorization header。 |
| LIB-04 | P1 | 打开 library/manage；扫描、重识别 Unidentified 文件、手动 claim、查看重复文件；删除测试文件并按产品恢复路径还原。 | 识别建议可复核；低置信度不会自动 Transfer；受影响路径与文件名可见；可恢复删除能恢复记录和文件。 |
| LIB-05 | P1 | 加入/移出收藏夹；切换 User；在收藏页打开 Media 并返回。 | 收藏是 User 维度；成员之间不串数据；收藏页和详情状态一致。 |
| LIB-06 | P1 | 打开 Library customize 修改可见信息、排序与卡片呈现；保存后刷新并恢复默认。 | 自定义仅影响预期视图；保存后保持；恢复默认不改变 Library 文件或其他 User 设置。 |
| LIB-07 | P0 | 准备额外 TV Library A、本地 tvshow.nfo/STRM/字幕/图片、两 User 的 S01E03 进度 750000/120000ms 与不同已看/收藏状态；另一 Library B 保留同 Media 文件。管理员从管理页删除 A，观察确认文案、删除中卡片和终态，再从 UI 用同根目录重建并扫描；分别登录两 User 查看进度与收藏。 | 202 接受不等于成功：只有 succeeded 才移除卡片；文件及侧车字节不变；旧 ledger/探测缓存清除，新 ledger id 不同但可靠 NFO 别名复用 MediaId；两 User 状态各自恢复；B 的文件、ledger 和缓存不变；新库片头/声纹默认关闭，基本媒体探测仍安排。 |
| LIB-08 | P0 | 用电影 Library 重复 LIB-07；再分别准备同名不同外部 ID、无 ID 且身份歧义的 NFO，删除后重建；检查原历史和新文件详情。 | 整片进度可在可靠 movie 身份下恢复；不同 ID 不串进度；无可靠身份不强行合并，保留旧历史但不承诺自动恢复；episode NFO 的单集 ID 不当作 TV 作品 ID。 |
| LIB-09 | P0 | fixture 在扫描、probe、侧车发布或 Transfer 边界暂停；从 UI 删除其 Library，释放 fixture；删除中刷新页面。再用排空超时 fixture 观察失败并重试。 | 删除中不能新扫描/Scrape/Transfer/Playback；只有旧租约退出且不能再发布才成功；超时显示失败并保持冻结，错误可读、可重试；旧任务不能在同根新库复活数据或改写侧车；另一库的独立工作继续。 |
| LIB-10 | P0 | 隔离实例分别在 draining 与两个数据库清理阶段之间重启，再从 UI 回到管理页；检查重复删除/重试、成员浏览和显式绑定旧库的 Subscribe。 | 恢复同一操作与快照计数，不恢复被取消 probe；冻结库成员不可见，管理卡保留状态；Subscribe 保留但提示目标 Library 不可用，不自动改绑默认库；失败不报成功。 |
| LIB-11 | P1 | fixture 建父库与独立嵌套子库；从 UI 删除非最后一个默认库，再尝试删除同类型最后一个库；检查静态封面和其他库 Playback。 | 最长唯一根归属保证子库/同 Media 其他文件不受影响；默认只提升现存库；最后同类型库受保护；只关闭命中删除快照的播放，其他库 Playback 正常；静态 artwork 继续无需自定义 Authorization header。 |
| TRF-01 | P0 | 在转移预览中选 hardlink、copy、move；以同盘/跨盘 fixture 运行 Transfer；对比来源与目标文件、Naming template 和 ledger。 | 目标名称按 Naming template 展开且空字段不留下多余括号；hardlink/copy 保留源文件，move 移除源文件；探测到的文件 quality 写入 ledger。 |
| TRF-02 | P0 | 将高置信度与低置信度文件放入 intake Watch directory；运行一次 intake；对 Unidentified 项手动 claim 后再次运行。 | 可识别文件 Transfer 到目标 Library；Unidentified 留在待处理列表，不按猜测移动；claim 后使用明确 Media 重新执行。 |
| SCR-01 | P1 | 对刚 Transfer 的文件、已有 Library 文件和单独路径分别执行 Scrape；打开输出目录检查 NFO 与 artwork。 | 每种入口都按开关生成/更新 sidecar；关闭 mirror 写入时不创建 NFO/artwork；已有文件不会被无关内容覆盖。 |
| SCR-02 | P1 | 修改 Metadata source 语言、图片候选语言/质量、Naming template、目录写入和片头片尾标记偏好；运行 Scrape 后检查 Library 与 Playback。 | 新设置只影响后续预期操作；封面/背景候选符合语言与质量偏好；标记与章节呈现在支持的接口，数据源缺失时有提示。 |
| DUP-01 | P1 | 从重复文件页比较同一 Media 的多个版本，保留一个并删除另一个。 | 标识、路径、probe 信息和选择结果清楚；删除确认对应正确文件；Library ledger、文件和容量统计同步。 |

#### Library removal 自动化辅助验收与未覆盖项

`cargo test -p api --test library_delete_reimport` 通过公开 DELETE/status/创建/scan 路由检查 TV 两 User、电影整片、不同 NFO 别名不串进度、旧缓存删除和独立 Library 同 Media 保留。fixture 在没有 Tokio runtime 时创建 ApiState，随后进入 Tokio 执行 HTTP，避免启动真实 ffprobe worker；Catalog/Indexer 为离线 fake，STRM 不访问远程源。已有 file_meta 使用有效 VideoTrack，不能用空 Tracks 的读取 miss 假装验证清理。

这个 API 测试不是浏览器 E2E，不替代 LIB-07–11。它检查新 ledger 没有复制旧缓存，不验证新 fake-probe 成功写入 file_meta，也不独立覆盖重启/取消竞态、无 ID 歧义、成员权限、Subscribe 失效或实际播放。上述 UI 用例本次新增时均为 **Not run**；必须记录实际命令退出码和 UI 证据之后才能标记 Pass。

### 系统配置、管理和运维页面

| ID | 优先级 | 用例与步骤 | 预期结果 |
|---|---|---|---|
| SET-01 | P0 | 打开设置概览，分别缺少 Site、Downloader、Library 和 Metadata source；逐条使用修复入口。 | 状态与真实配置相符；修复入口可到达正确分区；配置完成后概览及时刷新。 |
| SET-02 | P1 | 编辑管理员和成员的个人资料、头像和昵称；退出再登录检查。 | 资料持久化并按 User 隔离；密码相关 UI 不回显原值；失败时保留可修正表单。 |
| SET-03 | P1 | 切换主题、外观和玻璃质感；在桌面/移动尺寸打开设置首页和分区，刷新并用返回链回到设置。 | 偏好保持；移动端设置分区索引可滚动且选择有效；主题加载完成前不会错误跳过设置索引页。 |
| SET-04 | P0 | 管理员创建、编辑、禁用和恢复成员；切换能力开关与 Library 可见范围；成员重新登录检查。 | 成员身份、能力和可见范围即时生效；成员不能通过直接 URL 绕过限制；既有管理员不被锁出。 |
| SET-05 | P1 | 配置 Metadata source 凭据、默认 source、自动 artwork 和刷新策略；用 fake source 执行刷新及 source 超时。 | 密钥保存后不明文回显；成功时显示新状态；超时可重试且显示降级，不覆盖已有有效 artwork。 |
| SET-06 | P0 | 遍历 Scrape 设置的 Metadata、Images、Naming、Mirror、Markers 分区；保存/取消后重新进入。 | 每个分区可见且字段验证正确；保存后重载值一致；取消不会提交；错误信息定位到相关字段。 |
| SET-07 | P0 | 新增/编辑/停用 Site；填写 fake 凭据和 Indexer 分类；执行连接检查、登录/Check-in 和搜索；查看一 Site 错误时另一 Site 返回的结果。 | 凭据不会在界面泄漏；连接状态清晰；Check-in 不生成 Torrent；搜索使用对应 Indexer 且支持单 Site 降级。 |
| SET-08 | P0 | 分别配置 qBittorrent 和 Transmission fake Downloader；测试连接、路径映射、默认保存位置；切换启用项。 | 状态和路径映射正确；凭据隐藏；断连/路径不可达显示可读错误；不可用 Downloader 不接收新 Torrent。 |
| SET-09 | P1 | 打开系统日志；按等级、时间和关键字过滤；等待 fake Job 写入新记录；查看错误上下文。 | 新日志可见且顺序合理；过滤组合生效；错误含关联 Job/Site/Downloader 信息；高频刷新不会阻断操作。 |
| SET-10 | P1 | 打开定时 Job、Transfer、缓存页面；查看计划、运行记录和缓存项；对一个测试项手动运行、刷新或清理。 | 页面状态与定义/执行记录分开；操作有确认和结果；清理只影响目标测试项；记录可追踪。 |
| SET-11 | P1 | 用管理员和成员分别打开 collections、activity、tasks、transfers、jobs、cache、settings 深层路由及其操作入口。 | 菜单显隐、路由守卫和后端授权一致；没有“只隐藏按钮但 API 仍允许”的越权。 |

### Web Playback

| ID | 优先级 | 用例与步骤 | 预期结果 |
|---|---|---|---|
| PLAY-01 | P0 | 从电影和 TV Library item 启动播放；播放一段后退出，再次打开；从“继续观看”入口恢复。 | 首次播放成功；播放进度按当前 User 保存并恢复；进度、总时长和观看状态符合实际位置。 |
| PLAY-02 | P0 | 执行播放/暂停、快进/快退、拖动时间线、音量、倍速、全屏；用键盘和触屏分别操作。 | 控件状态与视频状态同步；seek 不造成持续卡顿；键盘快捷键不误触表单；移动端手势与屏幕方向呈现符合设备。 |
| PLAY-03 | P1 | 切换音轨、字幕、字幕延迟/样式；播放含章节文件并跳转章节；验证不支持的字幕/编码。 | 选中轨道有清楚标记；字幕文本与时间正确；章节跳转准确；不支持的格式给出可执行提示。 |
| PLAY-04 | P1 | 播放 TV Media 连续 episode，执行上一集/下一集、自动播放、手动标记已观看；模拟 stalled、网络恢复和无法播放文件。 | 切换目标正确；自动播放遵从偏好；用户观看状态同步；故障恢复可继续，无法恢复时提供 VidHub 等外部 Playback 入口。 |
| PLAY-05 | P1 | 打开 Playback 统计、播放设备、近期活动与播放日志；按 User、时间和 Media 查看。 | 数据只属于当前 User；客户端、Media、时间与时长对应实际播放；无数据时有空状态。 |
| PLAY-06 | P2 | 在支持的浏览器测试 PiP、返回恢复、播放会话释放和长时间播放；用 soak fixture 持续运行。 | PiP 和回到页面行为稳定；关闭播放后会话和占用释放；长时间运行无明显进度漂移或画面冻结。 |

### VidHub app：Jellyfin-compatible 接入和显示

以下用例必须在隔离实例上用 VidHub app 实测。每项同时检查浏览器/网络层结果，避免只凭页面截图判断 API 数据是否正确。

| ID | 优先级 | 用例与步骤 | 预期结果 |
|---|---|---|---|
| VH-01 | P0 | 在 VidHub 添加隔离实例地址；用管理员 User 登录；退出后用成员 User 登录。 | 服务器发现和登录成功；认证信息不会泄漏到界面或日志；成员只看到获授权的 Library。 |
| VH-02 | P0 | 浏览电影与 TV Library：首页、Library 列表、Media detail、Season 和 episode 页面；前进、返回并重进。 | 标题、简介、年份、评分、Season/episode 编号和条目数稳定显示；VidHub 常见的带 /Users/{userId}/ 前缀路径可用。 |
| VH-03 | P0 | 检查海报、背景图、人物图、episode still、章节缩略图；分别对已认证与原生图片请求检查。 | 所有有数据的 artwork 均出现且比例正确；缺失图片使用 fallback；原生图片请求不会因缺少 Authorization header 返回 401。 |
| VH-04 | P0 | 在 VidHub 播放电影和 TV episode；暂停、退出、重新进入并继续；切换 episode 或从详情再次播放。 | 视频可启动；进度恢复到正确位置；观看状态、已观看标记和下一集信息与服务端 User 状态一致。 |
| VH-05 | P1 | 播放多音轨/多字幕/含章节 fixture；切换音轨与字幕并跳转章节。 | VidHub 仅显示实际可用轨道；切换后播放声音/字幕正确；章节标题与时间对应文件。 |
| VH-06 | P1 | 由成员 User 播放同一 Media；再用另一个 User 查看进度与观看标记。 | 各 User 的 Playback 进度和观看标记互相隔离；共享 Library 文件仍可见范围一致。 |
| VH-07 | P1 | 对支持 intro/outro marks 的 fixture 在 VidHub 查看是否出现对应标记/跳过提示；从 mark 前后 seek。 | 若 VidHub 当前版本支持协议字段，则 mark 时间、章节边界和跳转准确；若 app 不支持，基础 Playback 不受影响且记录为客户端兼容性差异。 |
| VH-08 | P1 | 断开服务端网络后打开已访问详情，再恢复网络；重复进入 Media、刷新图片并继续 Playback。 | 离线状态有清楚提示且不显示错误数据；恢复后 API、图片与播放可重试，不要求重新创建 User 或 Library。 |
| VH-09 | P1 | 在 VidHub 中通过搜索/筛选（若 app 提供）查找电影与 TV Media，并打开结果；退出 server 后重连。 | 结果与可见 Library 一致；重连后仍进入正确服务器；不支持的 VidHub UI 功能记为 app 范围外，不作为服务端失败。 |

## 覆盖清单

以下当前 Web 路由均应至少由一个用例进入或直接打开：

- /login、/health、未知路由：AUTH-01、ROUTE-01
- 首页、/activity、/my：HOME-01、ACT-01、MY-01/02
- /search、/discover/:type、发现合集/人物与榜单路由：SEARCH-01/02、DISC-01/02
- /media/:type/:id、/media/douban/:id：MEDIA-01/02/03
- /subscriptions、/subscriptions/:id：SUB-01 至 SUB-05
- /tasks：JOB-01/02
- /library、/library/:id、/library/:id/item/:mediaItemId：LIB-01 至 LIB-05、PLAY-01 至 PLAY-05
- /library/manage、/library/customize、/library/favorites：LIB-04/05/06、DUP-01
- /collections、/collections/:id：DISC-02、SET-11
- /transfers、/jobs、/cache：TRF-01/02、SET-10
- /settings 与 /settings/:section：SET-01 至 SET-11
- /play/:mediaItemId/:unit：PLAY-01 至 PLAY-06
- VidHub Jellyfin-compatible routes：VH-01 至 VH-09

## 结果记录

每轮回归为每个 ID 记录：构建号、环境、User 角色、浏览器或 VidHub 版本、结果（Pass/Fail/Blocked/Not run）、实际结果和证据文件位置。Fail 必须包含失败原因、失败步骤、预期与实际结果、复现条件；Blocked 必须包含阻塞原因和解除条件；如已创建缺陷，还要记录缺陷链接。不得把 Blocked 或 Not run 计入通过率。

## 数据库／维护新增验收（2026-10-13，尚未执行）

本节是待执行用例，不计入本轮workspace通过数。缺陷和自动化任务对应 [总复核](<../audits/2026-10-13-database-api-review.md>) 与 [T1–T9](<../superpowers/plans/2026-10-13-database-api-followup.md>)。

| ID | 操作／条件 | 预期 | 当前状态 |
|---|---|---|---|
| DB8-01 | 清理第一批提交后禁用def | 已提交保留结果，不启动下一批 | 已知F01；本用例Not run |
| DB8-02 | future queued已存在，90天改365天或打开dry_run | 尚未运行child采用新策略，running不改 | 已知F02；本用例Not run |
| DB8-03 | payload＋schedule保存遭schedule写入失败／并发仅改周期 | 整笔回滚／陈旧token409，响应为实际状态 | 已知F04；本用例Not run |
| DB8-04 | 600保护行、0可删；600保护＋1200可删多批 | 截断如实标志、保护不重复计数 | 已知F03；本用例Not run |
| DB8-05 | 无过期／全部保护大历史，预览同时进行无关请求 | 真预算退出，不占async worker，lower bound明确 | F05待实现与测量 |
| DB8-06 | 改预览草稿后旧响应晚返回 | 旧结果丢弃，报告显示本策略与预算限制 | 已知F12；本用例Not run |
| DB8-07 | future queued点立即执行；开发tick与后台维护同时存在 | queued安全提前、无重复live；tick不取消维护 | 已知F12；本用例Not run |
| DB8-08 | 显式VACUUM超时／disk变化／scan在预检后开始 | 真实停止或明确best-effort超限，不谎报硬界 | F06需先批准合同 |
| DB8-09 | 升级首次调度＋重启保存后的开关／策略 | 默认警告明确，配置不复位；不把StateRunner直调当loop验收 | T9待补 |
| COVER-LEGACY | legacy封面升级中freeze／CAS失败／trigger拒绝 | 当前引用bytes不变，只清未引用版本 | 已知F08；本用例Not run |

## 当前限制

本文件是可评审的回归用例基线，不代表这些流程已执行。当前仓库有前端逻辑/契约测试，但没有浏览器 E2E runner；VidHub 也需要隔离实例和 app 实测。下一步应先用真实浏览器和 VidHub app 执行 P0 用例，逐项保留结果与失败原因，再将高频浏览器流程落成可重放的 E2E，并将 VH 用例保留为每次发布的设备验收清单。
