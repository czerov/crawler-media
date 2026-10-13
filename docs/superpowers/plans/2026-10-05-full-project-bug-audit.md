# 全项目 Bug 审计报告（2026-10-05）

> **历史状态（2026-10-13 整理）：** 广域 Bug 审计状态 UNRECONCILED：A/B/D/G 等原发现 ID 仍需逐项闭环核对。后续 second-review 是部分整改线索，当前数据库/API review 不是全审计关闭证明。
>
> 本文保留原设计、发现/任务 ID 与当时证据；日期本身不表示过时。旧执行/提交/清理命令仅作历史记录，不应直接重跑；复选框不是当前实现清单。当前依据：[数据库/API review](<../../audits/2026-10-13-database-api-review.md>) 与 [剩余计划](<2026-10-13-database-api-followup.md>)，未覆盖的旧 ID 仍须单独对账。

## 范围与证据

- 基线：main，HEAD `95498e3d1de30c6053fea80f7ad3d29617fb64d3` **加当前未提交/新增文件**。不是仅审查 diff；上一轮已修复内容计入当前实现。
- 目的：只读发现 bug，不授权本报告中的修复、提交、推送或 issue 变更。本轮仅新增此报告，未修改生产源码。
- 方法：按鉴权、文件生命周期、Downloader、Subscribe/Jobs、前端契约、Playback/Metadata分区阅读实现、调用方、DTO与相关测试；父审核查关键链路并合并同模式。
- 证据：下文默认 **S＝源码/契约确认，未端到端复现**；**E＝执行最小离线复现**。没有访问真实 Downloader/TMDB/生产数据库，没有运行危险删除或巨量分配输入，没有运行新的全仓 gates。上一轮测试通过不能证明本清单里的路径正确。
- 明确排除：正原子 OR + 最高匹配分数是当前 Filter 模型与既有测试的合同，不将其本身报告为后端 AND 缺陷。UI“只要免费”等与此模型的表达冲突应另作产品/契约澄清。静态图片 public 符合规范；未把不支持转码、多 Range 或管理员可配置路径本身当漏洞。
- P1：安全、数据损失、严重错误执行/服务不可用，优先修；P2：可确定功能错误或恢复/运行状态不一致。

## A. 安全与服务可用性

### A01 P1 — legacy Subscribe run 绕过 owner 与生命周期保护（S）
证据：[成员路由](<../../../crates/api/src/http/mod.rs#L184-L200>)、[run handler](<../../../crates/api/src/management/subscribes/run.rs#L22-L53>)。
任意成员知道另一 User 的 Subscribe UUID 后可 POST `/subscriptions/{id}/run`。handler不提取UserId、不验证owner、不取得subscribe_guard，也不检查paused/deleting；会真实执行Downloader add、Transfer和facts写入。搜索阻塞期间并发暂停、修改或删除后，旧快照仍可落地文件/孤儿facts。这与正常worker已修的guard/reload是不同入口。
建议：关闭或统一路由到现有owner-checked Job/worker seam，加入可控并发测试。

### A02 P1 — 自动 Library 路由预检重复获取同一 Store 锁（S）
证据：[持锁调用metadata](<../../../crates/api/src/http/routing_preview.rs#L48-L74>) → [metadata再次取锁](<../../../crates/api/src/scrape_metadata.rs#L13-L16>)。
管理员打开ready、有tmdb_id、未指定Library的Subscribe弹窗，非重入Mutex自锁，不依赖真实网络；预检卡住并阻塞其他Store请求。
建议：锁内只快照所需数据，metadata IO移到锁外。

### A03 P1 — 远端 Release 巨量集范围无界枚举（S，未执行危险分配）
证据：[原range枚举](<../../../crates/subscribe/src/choose.rs#L37-L55>)、[wanted枚举](<../../../crates/api/src/worker.rs#L318-L349>)、[Release解析](<../../../crates/release/src/boundary.rs#L149-L175>)。
`Test.Show.S01E01-E4294967295.1080p`可被解析；coverage overlap有限制，但choose/wanted仍枚举原始range，导致OOM或长时间阻塞。Filter拒绝候选也可能进入wanted记录路径。
建议：统一有界slot展开，所有入口限制Release而不只限制Subscribe coverage。

### A04 P2 — 普通成员读取管理员第三方 secret（S）
证据：[scrape整体序列化](<../../../crates/api/src/http/scrape_settings.rs#L89-L95>)、[返回setting](<../../../crates/api/src/http/scrape_settings.rs#L143-L147>)、[API key定义](<../../../crates/api/src/scrape_config.rs#L50-L52>)。
admin配置TheIntroDB key后，member GET `/settings/scrape`返回完整key。Downloader/Browser URL也原样返回；URL存在userinfo/query secret时同模式泄漏，不声称默认URL含secret。
建议：按role脱敏，更新DTO与成员HTTP测试。

### A05 P2 — Device revoke 不影响取流，Jellyfin换PlaySessionId绕过黑名单（S）
证据：[撤销](<../../../crates/api/src/http/playback_activity.rs#L248-L266>)、[拼接会话到device身份](<../../../crates/media-server/src/routes.rs#L668-L689>)、[取流仅visibility](<../../../crates/api/src/media_server_provider.rs#L350-L366>)。
撤销D:S1后原token仍可取流；新S2产生D:S2，不匹配撤销记录。应区分结束一次播放与撤销稳定设备身份，不能用新session逃过设备撤销。

### A06 P2 — logout/改密后既有 Jellyfin WebSocket 继续收事件（S）
证据：[只升级前认证](<../../../crates/media-server/src/routes/websocket.rs#L19-L48>)。
撤销token后旧socket仍凭捕获UserId收到新会话的收藏/已看事件。建议长期连接订阅撤销通知或重验token。

### A07 P2 — Job SSE 断开后永久后台轮询（S）
证据：[job_stream](<../../../crates/api/src/http/jobs.rs#L450-L469>)。
每次连接spawn loop，send失败被忽略，无closed/break；反复重连累积后台任务，每2秒争锁查询DB。建议接收方关闭时立即退出。

## B. 文件、Transfer、Library安全

### B01 P1 — 单季取消可删除同Media其他季和其他Library文件（S）
证据：[删除快照只按media_id](<../../../crates/api/src/http/subscriptions/deletion.rs#L256-L263>)。
管理员取消S1并勾delete_library_files，S2仍有Subscribe也会删除S2及其他库版本。预览也使用扩大范围，不能证明安全。建议明确coverage、Library、共享引用保护。

### B02 P1 — 单集 Wash-cut 删除仍被其他集引用的合并文件（S）
证据：[更新部分slots却整路径删除](<../../../crates/subscribe/src/collect.rs#L247-L275>)。
E1/E2 facts同指S01E01-E02文件；只升级E1，会删除旧整文件，但E2 fact仍指已删路径、仍算complete。建议按实际文件覆盖slot集合保护残余引用。

### B03 P1 — ledger/facts提交中断后重试不能修复（S）
证据：[删旧、写ledger、再写facts](<../../../crates/api/src/worker/transfer.rs#L22-L49>)、[已导入映射直接跳过facts](<../../../crates/subscribe/src/collect.rs#L58-L65>)。
新ledger写完、facts前断电或subscribe.db失败；重启source→dest映射令收集跳过事实更新，之后还可mark pending imported。首次导入facts空/洗版facts指已删旧文件永久残留。建议持久journal/幂等reconcile而非仅增加事务名义。

### B04 P1 — Unidentified claim 覆盖已有Library文件（S）
证据：[claim](<../../../crates/api/src/claim.rs#L83-L109>)、[底层移除已有dest](<../../../crates/library/src/file_transfer.rs#L50-L61>)。
另一个Unidentified认领为相同Media/year/resolution/ext，render相同dest，无冲突保护；旧字节被替换，ledger同path被覆盖。建议在claim建立目标ownership/collision合同，默认禁止覆盖。

### B05 P1 — Move成功后Scrape失败留下无ledger孤儿（S）
证据：[先Transfer后Scrape再facts/ledger](<../../../crates/subscribe/src/collect.rs#L210-L235>)。
Move源已删，目标同stem `.nfo`预先是目录或无写权限，scrape返回Err，事实和ledger尚未push；下一轮Downloader找不到源，不能自愈。建议Transfer成功事实先可恢复持久化，Scrape作为可重试独立阶段。

### B06 P1 — Library删除/原始ledger删除/retransfer未用strict root（S）
证据：[item删除](<../../../crates/api/src/http/library_delete.rs#L61-L75>)、[raw delete/retransfer](<../../../crates/api/src/http/library_admin.rs#L98-L124>)、[retransfer](<../../../crates/api/src/http/library_admin.rs#L163-L213>)、[默认库fallback](<../../../crates/store/src/libraries.rs#L186-L191>)。
旧root改为新root后，旧ledger库外文件被默认库接管，仍可删除或重建；根下路径换成外部symlink后retransfer可写库外。上一轮strict修复未覆盖这些入口。建议所有破坏性入口统一strict canonical owner，拒绝孤立记录的物理操作。

### B07 P1 — Watch intake无冲突保护，覆盖后保留旧ledger身份（S）
证据：[原名直接Transfer](<../../../crates/library/src/watch.rs#L110-L135>)、[已有path跳过ledger](<../../../crates/api/src/watch_ledger.rs#L12-L17>)。
intake与Library已有同名、不同内容视频；旧目标被底层替换，record_paths却跳过原row，新字节仍显示旧Media/质量。建议目标冲突预检与明确重试身份。

### B08 P1 — duplicates可删除最后在位副本/同一实体的别名（S）
证据：[仅计ledger数量](<../../../crates/api/src/http/library_duplicates.rs#L88-L103>)。
A存在、B已外删但ledger仍在，count=2允许删A；或者roots real/alias symlink扫描生成两条指同目录项记录，删一条使两条都失效。建议检查剩余在位、独立物理实体，不能仅数DB行。

### B09 P1 — 父Library duplicates可清理嵌套子Library（S）
证据：[裸prefix列表](<../../../crates/api/src/http/library_duplicates.rs#L27-L38>)、[裸prefix删除](<../../../crates/api/src/http/library_duplicates.rs#L85-L103>)。
P=/movies，C=/movies/private，C内两个版本；P duplicates列出并可删C文件，绕开最长root唯一owner。建议与正常rows_in_library共享归属。

### B10 P2 — 历史 recycle 移动后中断无法重新登记（S）
证据：[恢复顺序](<../../../crates/store/src/legacy_recycle.rs#L64-L86>)、[目标重选](<../../../crates/store/src/legacy_recycle.rs#L102-L116>)。
move到original后、insert ledger前中断；重启见original存在选新recovered路径，source又不存在，不能发现实际已恢复文件。建议持久化恢复目标与状态。

### B11 P2 — Watch intake固定Movie root，接受TV与非视频（S）
证据：[固定root](<../../../crates/api/src/watch_intake.rs#L17-L19>)、[无扩展/递归识别](<../../../crates/library/src/watch.rs#L110-L135>)。
Pantheon.S01E01.mkv被放电影根；The.Matrix.1999.srt也可入账/probe。TV ledger靠默认库fallback显示在TV库但真实文件在电影根。建议识别、路由、扩展支持共享Transfer合同。

### B12 P2 — STRM同URL修改事件删除用户手动marker（S）
证据：[无pending时直接认为变化](<../../../crates/api/src/fs_watcher.rs#L56-L80>)、[删除marker](<../../../crates/api/src/fs_watcher.rs#L285-L302>)。
仅touch或原地重写同URL，仍清掉片头/片尾。建议只在内容身份变化时失效，用户锁定标记与缓存分开。

### B13 P2 — 运行期Watch/root/realtime设置不刷新监听器（S）
证据：[启动一次快照](<../../../crates/api/src/fs_watcher.rs#L155-L190>)。
新增root不监听，关闭realtime/移除root后旧监听仍调度；30秒轮询不能完全替代STRM grace/cache语义。建议动态订阅或重建watcher。

### B14 P2 — Movie/TV scan漏格式、标准NFO布局（S）
证据：[只支持mkv/mp4/strm](<../../../crates/library/src/watch.rs#L66-L82>)。
Movie.1999.avi/ts/m2ts/mov/m4v/webm被跳过；`The Matrix (1999)/movie.mkv + movie.nfo`只按basename Low跳过，不读父目录/NFO，API仍成功。建议统一视频格式及识别fallback。

## C. Downloader身份与操作

### C01 P1 — add/collect/remove多个路径仅name+size而非精确身份（S）
证据：[qbit existing_info](<../../../crates/downloader/src/qbit.rs#L306-L323>)、[qbit完成收集](<../../../crates/downloader/src/qbit.rs#L170-L190>)、[任务救援](<../../../crates/api/src/http/downloaders/tasks.rs#L183-L216>)、[Transmission完成收集](<../../../crates/downloader/src/transmission.rs#L291-L323>)。
目标magnet hash B，Downloader仅剩同名同体积hash A：qbit add假成功不投B，收集可Transfer A文件；救援remove调用legacy remove而非remove_owned，可删A与数据。已有精确身份API未覆盖全部seams。建议分别测add/collect/remove，同名不同hash/原HTTP任务消失+外部同名。

### C02 P1 — 非默认Downloader hash操作打到默认端点（S）
证据：[delete/pause/resume](<../../../crates/api/src/http/downloaders/tasks.rs#L66-L129>)。
列表给出B的downloader_id，但操作只取state.downloader(A)。B不受影响；A也有H时delete_files删A数据。建议task handle携带端点身份并验证路由。

### C03 P1 — 无标签真实任务超过15分钟被GET列表删除pending（S）
证据：[过滤无tag](<../../../crates/api/src/http/downloaders/task_listing.rs#L45-L50>)、[absent自动清理](<../../../crates/api/src/http/downloaders/task_listing.rs#L74-L95>)。
Transmission RPC15不支持labels，真实在途快照被过滤，然后当客户端可达+absent删pending；用户去掉qbit tag同样。建议“不能证明归属”与“证明任务消失”分开，读取不破坏跟踪。

### C04 P2 — 默认DynamicDownloader不转发自定义save_path（S）
证据：[代理实现](<../../../crates/api/src/dynamic_downloader.rs#L75-L143>)、[trait默认行为](<../../../crates/downloader/src/lib.rs#L37-L46>)。
默认端点手填save_path/auto_route调用add_with_options落trait unsupported，HTTP400；非默认直连有实现。建议完整委托接口回归。

### C05 P2 — Transmission暂停/恢复没有实现（S）
证据：[默认unsupported](<../../../crates/downloader/src/lib.rs#L112-L118>)、[HTTP调用](<../../../crates/api/src/http/downloaders/tasks.rs#L89-L129>)。
有效Transmission任务pause/resume返回500，客户端状态不变。建议能力暴露+RPC实现，不能显示可操作假控件。

### C06 P2 — Transmission pack不核验单文件完成度（S）
证据：[总体完成即收集](<../../../crates/downloader/src/transmission.rs#L318-L323>)、[全部files返回](<../../../crates/downloader/src/transmission.rs#L381-L387>)。
用户deselect第二个文件，总percentDone=1、该文件bytesCompleted=0；预分配文件也被Transfer、probe失败fallback可入ledger，或不存在时永久收集错误。建议仅返回完成、选中的文件。

## D. Subscribe选择与表单

### D01 P1 — UpgradeLadder从重命名路径推旧质量，可2160p降级720p（S）
证据：[从fact.path解析](<../../../crates/subscribe/src/choose.rs#L105-L120>)、[默认TV命名不含质量](<../../../crates/library/src/naming.rs#L27-L31>)。
旧2160p事实路径`Test Show - S01E01.mkv`解析resolution空，新720p被判更高。Movie默认也丢source/codec/HDR。建议使用Library authoritative probed质量，不用命名信息当事实。

### D02 P1 — 多槽位ladder等分保护不足，牺牲已有更好集（S）
证据：[任一slot批准](<../../../crates/subscribe/src/choose.rs#L97-L128>)、[保护只比较分数](<../../../crates/subscribe/src/slot_replacement.rs#L19-L27>)、[整组替换](<../../../crates/subscribe/src/collect.rs#L247-L275>)。
E1=2160p100分、E2缺失；合并1080p100分因E2获批，E1等分未挡，旧2160p也删。建议每个已拥有slot按实际ladder评估，不能仅分数。

### D03 P1 — 整季包每个无集号文件继承全部slots（S）
证据：[file身份fallback](<../../../crates/subscribe/src/file_identity.rs#L77-L90>)、[整季slots](<../../../crates/subscribe/src/choose.rs#L229-L244>)、[facts更新](<../../../crates/subscribe/src/collect.rs#L247-L254>)。
S1E1..2 full-season，torrent Test.Show.S01.1080p，files `01.1080p.mkv/02.1080p.mkv`；每个无episode文件当整包，首文件写两集facts、第二等分不更新，错误complete。纯`01.mkv`不是本报告证明输入。建议每文件不能猜全部coverage，无可靠身份进入Unidentified。

### D04 P2 — pending未按slot占位，多轮下载同集备选（S）
证据：[仅enclosure去重](<../../../crates/api/src/worker/finish_search.rs#L122-L132>)。
A在途、facts尚空，下一轮B不同URL同集仍被choose/add，普通非Wash-cut也会重复下载。建议pending coverage纳入admission/choose。

### D05 P2 — 最高分不符合ladder时不尝试可升级候选（S）
证据：[先max score再should_take](<../../../crates/subscribe/src/choose.rs#L28-L68>)。
旧1080p，免费720p100分不可升级、2160p50分可升级；只看720p被拒后chosen空。建议先筛可接受升级，再排序。

### D06 P2 — WashTarget只作用报告，不停止实际升级（S）
证据：[选择只读ladder](<../../../crates/subscribe/src/choose.rs#L97-L143>)、[报告cutoff](<../../../crates/api/src/http/subscription_depth.rs#L102-L147>)。
已有目标1080p报告at_cutoff，但2160p候选仍升级、Wash-cut永不complete。建议cutoff合同统一worker和UI。

### D07 P2 — S02季包内E01文件错误归S01（S）
证据：[优先文件已解析季](<../../../crates/subscribe/src/file_identity.rs#L77-L90>)、[独立E默认季](<../../../crates/release/src/boundary.rs#L130-L138>)。
S02 torrent、E01.1080p.mkv解析season1覆盖torrent已知season2，合法包全跳过、pending活跃。建议保留季号是否显式的解析事实。

### D08 P1 — Wash-cut入口不启用Wash-cut且目标Filter被忽略（S）
证据：[create serializer](<../../../web/lib/subscription-form.ts#L3-L16>)、[run_upgrade不提取body](<../../../crates/api/src/http/subscription_depth.rs#L25-L35>)。
“订阅并开始洗版”/普通Subscribe“洗一轮版”选新组，create不发wash_cut，upgrade仅报告当前策略；已入库单元被判at_cutoff，HTTP200未开始实际洗版。建议显式政策变更与执行命令区分。

### D09 P1 — UI保留旧版本选项未落地，仍可删旧文件（S）
证据：[spec设置](<../../../web/components/rule-sets-panel.tsx#L674>)、[serializer](<../../../web/lib/api/subscriptions.ts#L1021-L1137>)、[删除条件](<../../../crates/subscribe/src/collect.rs#L255-L264>)。
upgrade_keep_old未序列化，也未绑定Subscribe.keep_old_versions；勾选承诺保留仍默认false。建议移到真实Subscribe policy或删除不可支持控件。

### D10 P2 — 本User/coverage预检被任意已有Subscribe挡住（S）
证据：[不限定owner/coverage](<../../../crates/api/src/http/title_ref.rs#L151-L156>)。
B打开A已订Media，返回A的UUID并替换创建表单，不能创建自己的Subscribe；同Media已有S1也挡S2。删除仍404，不能因此说DELETE越权成功。

### D11 P2 — 创建Library选项状态未更新（S）
证据：[取libs](<../../../web/components/subscribe-dialog.tsx#L146-L179>)、[渲染](<../../../web/components/subscribe-dialog.tsx#L627>)。
libraries初始[]，取回libs用于自动选id却没有setLibraries，选择器不出现。自动预检锁修复后仍独立存在。

### D12 P2 — 弹窗/inspector/手动选种旧响应可污染新scope（S）
证据：[prepare无代际取消](<../../../web/components/subscribe-dialog.tsx#L128-L211>)、[inspector reload](<../../../web/components/subscription-inspector-view.tsx#L123-L143>)、[grab target切换](<../../../web/components/search-results.tsx#L882-L896>)。
A→B旧响应后到覆盖prepared/detail；可能B配A季/库，或B地址操作A；for_sub B失败仍保留A target。合并同类异步scope缺陷，修复需独立测试三个seam。

### D13 P2 — Filter片源/顺序/目标/HR严格选项契约错位（S）
证据：[Source匹配](<../../../crates/filter/src/admission.rs#L114-L119>)、[硬权重serializer](<../../../web/lib/api/subscriptions.ts#L1024-L1137>)、[回显](<../../../web/lib/subscription-ui.ts#L17-L63>)。
UI blu-ray/rip/tv与解析BluRay/WEBRip/HDTV不匹配；“先选优先”被固定2160p>1080p分数覆盖、回显升序倒置；只写source的wash_target remux会当resolution；hr_unknown_policy不持久化也不执行。不是指OR模型本身错误。

### D14 P2 — 今日摘要UUID合成NaN、预告字段与下载关联丢失（E部分/S部分）
证据：[UUID聚合](<../../../web/lib/subscription-ui.ts#L325-L341>)、[预告serializer](<../../../web/lib/api/subscriptions.ts#L535-L554>)、[wanted infohash](<../../../crates/api/src/http/subscription_depth.rs#L327>)。
两个不同UUID执行真实groupTodayArrivals得到1组、id=NaN，链接/标题错；wanted的air_date/season/episode/release_forecast适配被置null/0，未来预告当今日；info_hash固定null，inspector实时下载无法匹配。聚合已最小Node复现，其余源码确认。

### D15 P2 — 持Store锁获取TV metadata，cache miss失效（S）
证据：[预检锁/调用](<../../../crates/api/src/http/title_ref.rs#L142-L193>)、[实时key锁超时](<../../../crates/api/src/tmdb_http.rs#L22-L35>)。
TV无缓存时metadata后台try_lock_for500ms失败，被视为无key，seasons/episodes退空；预告同类。区别A02：这里是超时退化，非无限自锁。

## E. Site、手动下载与搜索页面

### E01 P1 — UI新建Site固定空URL不能搜索（S）
证据：[serializer空url](<../../../web/lib/api/sites.ts#L129-L134>)、[原样保存](<../../../crates/api/src/http/sites.rs#L134-L147>)、[fetch构造](<../../../crates/indexer/src/fetch.rs#L29-L68>)。
添加hdsky只填Cookie，表单无URL字段；profile无base URL，形成相对/torrents.php，配置成功却无法搜索。

### E02 P2 — Site UUID当profile ID，授权无法编辑且profile可重复添加（S）
证据：[catalogMap与lookup](<../../../web/components/site-config-section.tsx#L166-L175>)、[错误site.id](<../../../web/components/site-config-section.tsx#L341>)。
UUID查profile map miss，fallback auth_types空，无授权字段且保存disabled；Add列表同profile不被排除。

### E03 P2 — Site验证负结果被丢弃，刷流状态不刷新（S）
证据：[reverify丢结果](<../../../web/lib/api/sites.ts#L160-L162>)、[boost轮询条件](<../../../web/components/site-config-section.tsx#L206-L233>)。
expired Cookie返回data.ok=false却显示enabled绿色；首次enable boost只upsert Site不更新boostStats，anyBoosting=false不启动轮询，一直到reload仍“开启刷流”。两个行为需分别测。

### E04 P2 — 手动下载过滤active，但实例GET永远pending（S）
证据：[过滤](<../../../web/components/download-target-dialog.tsx#L296-L298>)、[GET实例状态](<../../../crates/api/src/http/downloaders/instances.rs#L43-L59>)。
成功验证两端点后也列不出其他Downloader/映射路径，只能默认。搜索记忆目标也被同过滤判失效。

### E05 P2 — 智能入库选项没有识别链，永远无tmdb_id（S）
证据：[preview adapter](<../../../web/lib/api/downloaders.ts#L535-L539>)、[选项条件](<../../../web/components/download-target-dialog.tsx#L342-L346>)。
初次preview不传selected_tmdb_id，结果null、candidates=[]，无法出现智能选项；routing preview并非识别API。

### E06 P2 — 记住不持久化，忘记null又解释成默认记忆（S）
证据：[checkbox只加category](<../../../web/components/download-target-dialog.tsx#L450-L451>)、[submit无prefs写入](<../../../crates/api/src/http/downloaders/submission.rs#L154-L195>)、[null转default](<../../../web/lib/api/downloaders.ts#L624-L629>)。
记住只影响请求字段，无PUT prefs；忘记写category:null，GET adapter重新创建默认目标。建议明确保存/删除API。

### E07 P2 — 新管理员空presets覆盖内置分类（S）
证据：[provider直接setTabs](<../../../web/lib/search-prefs.tsx#L52-L53>)、[backend默认[]](<../../../crates/api/src/http/search/history.rs#L136-L143>)。
新配置无搜索分类，设置和命令栏内置movie/TV等消失。建议unset与用户显式空区分。

### E08 P2 — 非TMDB搜索结果跳TMDB详情，timeout伪装空结果（S）
证据：[详情路由](<../../../web/components/media-search-results.tsx#L214-L216>)、[强制tmdb source](<../../../web/app/(app)/media/[type]/[id]/page.tsx#L11>)、[timeout provider unknown](<../../../crates/api/src/http/search.rs#L503-L581>)。
Bangumi/AniList/TVDB数字id变成TMDB查询，错Media/404；超时provider=unknown，页面找不到原source错误，显示正常无结果。应保留source身份贯穿route/status。

## F. Library页面、图片、合集与收藏

### F01 P2 — admin_visible=false Library从管理列表消失（S）
证据：[visibility](<../../../crates/api/src/http/library.rs#L65-L71>)、[管理也使用过滤列表](<../../../web/components/library-manage-view.tsx#L99-L106>)。
关闭管理员浏览可见后GUI无法再编辑恢复；管理应有与浏览分开的全量admin接口。

### F02 P2 — 首页UUID偏好丢失，exclude_from_home被强制false（E部分/S部分）
证据：[Number UUID lookup](<../../../web/lib/home-rows.ts#L449-L454>)、[首页映射](<../../../web/components/library-view.tsx#L80-L87>)。
保存默认库行隐藏/命名/排序/拖动后被丢，再补默认行；最小Node执行确认。首页和customize忽略真实exclude_from_home，关开关也继续展示。

### F03 P2 — Library items忽略limit/offset（S）
证据：[直接全量返回](<../../../crates/api/src/http/library.rs#L226-L242>)、[FE追加](<../../../web/components/library-detail-view.tsx#L541-L557>)。
每页返全库，dedupe不增但hasMore持续true；首页limit20失效，索引offset也无效。

### F04 P2 — 自然排序默认方向与FE不一致（S）
证据：[后端默认升序](<../../../crates/api/src/http/library/selection/filters.rs#L47-L48>)、[FE约定](<../../../web/lib/wall-sort.ts#L69-L76>)。
最近添加/评分/体积/最近观看自然方向FE不发order，后端升序；反向发asc仍一样。墙、首页与gallery共同受影响。

### F05 P2 — facets/index DTO使筛选、A-Z不可用（S）
证据：[facets输出](<../../../crates/api/src/http/library_scan.rs#L403-L430>)、[index输出](<../../../crates/api/src/http/library_scan.rs#L372-L398>)、[FE适配](<../../../web/lib/api/libraries.ts#L1117-L1162>)。
resolutions字符串被当{value,label,count}，其它维度空/total0；index每ledger无count/offset，FE归0，点B也跳墙首，不尊重当前筛选。

### F06 P2 — missing rows与Unidentified操作契约错位（S）
证据：[missing返回Subscribe gaps](<../../../crates/api/src/http/library_scan.rs#L319-L369>)、[unidentified全局](<../../../crates/api/src/http/library_admin.rs#L134-L151>)、[FE清单适配](<../../../web/lib/api/libraries.ts#L1568-L1604>)。
缺失页面显示同kind其他库Subscribe缺集、0文件且清理不消失；Unidentified无file_ids、忽略library query，认领POST reidentify空id必400并跨库展示。

### F07 P2 — Favorites排序/分页与gallery口径错误（S）
证据：[favorites比较](<../../../crates/api/src/http/playback_views.rs#L396-L423>)、[gallery比较](<../../../crates/api/src/http/playback_views.rs#L497-L523>)。
除title外都updated_at，评分/上映/体积/未看优先等不生效；同Media取时间max vs最后unit不同；平局无稳定id tie-break、HashMap迭代随机，多页可重复漏项。

### F08 P2 — gallery漏TV artwork/stills/chapters且DTO字段缺失（S）
证据：[只preferred文件](<../../../crates/api/src/http/library_gallery.rs#L97-L114>)、[图片DTO](<../../../crates/api/src/http/library_gallery.rs#L131-L140>)。
series/poster.jpg不在episode parent时整组丢；不遍历其他集/chapters，缺kind/year/label/t_seconds，FE可显示undefined/NaN时间和错误跳播提示。

### F09 P2 — 图片上传12MB承诺被默认JSON 2MB拦截（S，已核依赖）
证据：[业务上传](<../../../crates/api/src/http/library_artwork.rs#L249-L265>)、[路由body limit未覆盖](<../../../crates/api/src/http/mod.rs#L96-L109>)。
base64 JSON约>1.5MiB图片即达到Axum默认2MB body；例如2MiB JPEG前端允许却HTTP413。建议路线body limit与解码后12MB上限同步。

### F10 P2 — artwork写跨Library，背景动作改poster（S）
证据：[忽略library id选目标](<../../../crates/api/src/http/library_artwork.rs#L107-L123>)、[select](<../../../crates/api/src/http/library_artwork.rs#L175-L209>)。
同Media两库，从A换图可能写全ledger preferred的B；body.kind未处理，背景恢复自动实际处理poster。应以当前库+图类型限定写入。

### F11 P2 — reidentify/extras忽略文件范围，修改其他库同Media（S）
证据：[全Media预览](<../../../crates/api/src/http/reidentify.rs#L32-L47>)、[只首id后全Media操作](<../../../crates/api/src/http/reidentify.rs#L111-L148>)、[extras](<../../../crates/api/src/http/reidentify.rs#L204-L231>)。
在A指定一组文件标extras，B同Media也被移出ledger；修正身份同样跨库。建议按明确file_ids集合执行。

### F12 P2 — Library favorite筛选漏分级收藏（S）
证据：[仅whole favorite](<../../../crates/api/src/http/library/selection.rs#L117-L135>)。
TV仅收藏季/集，marks/favorites墙识别但Library favorite只whole，漏作品。电影(0,0)旧状态未读仅作为兼容性风险记录：当前新播放已canonical化，不能概括所有正常电影观看都漏。

## G. Playback与Metadata

### G01 P2 — 正常播完logs.completed但unit仍未看（S）
证据：[played始终None](<../../../crates/api/src/http/playback/heartbeat.rs#L120-L139>)、[close只写log](<../../../crates/store/src/playback.rs#L490-L538>)、[Jellyfin同模式](<../../../crates/api/src/media_server_provider/playback.rs#L201-L223>)。
首次played=false到片尾，客户端未另发手动PlayedItems/marks，仍false；up_next TV不推进、电影继续续看。建议明确完成阈值的authoritative unit写入，保留用户已看不能替代自动完成。

### G02 P2 — up_next电影候选与DTO读不同unit（S）
证据：[候选canonical](<../../../crates/api/src/http/playback_views.rs#L83-L97>)、[组装legacy](<../../../crates/api/src/http/playback_views.rs#L256-L259>)。
(-1,-1)有进度但DTO查(0,0)，返回position0/duration null/progress null或遗留旧进度。起播接口另有兼容，不声称实际播放必从0。

### G03 P2 — self admin-ended heartbeat删除标志后可复活（S）
证据：[previous/start处理](<../../../crates/api/src/http/playback/heartbeat.rs#L165-L185>)、[close与重建](<../../../crates/api/src/http/playback/heartbeat.rs#L191-L218>)。
首progress close删session，第二progress没有previous变admin_ended=false；start还直接过滤旧标志。Jellyfin保留row，不泛化同一复活链。

### G04 P2 — 已知Douban TV alias在上游失败时转movie（S）
证据：[丢本地kind](<../../../crates/api/src/http/media.rs#L430-L449>)。
本地TV有tmdb_id+douban_id，Douban关闭/失败默认movie，查同数字TMDB movie得到错工作/空。建议canonical kind优先于远端降级猜测。

### G05 P2 — language/TVDB配置仅保存，runtime不更新（S）
证据：[KV写入](<../../../crates/api/src/http/settings.rs#L62-L70>)、[启动固定provider](<../../../crates/api/src/main.rs#L112-L134>)。
进程运行期新增/改/清TVDB key或语言，GET显示新值而catalog仍旧值；TMDB key实时读取，不属于此问题。建议重建provider或明确restart-required反馈。

### G06 P2 — TMDB测试通过不能证明TMDB凭据可用（S）
证据：[测整体catalog](<../../../crates/api/src/http/settings.rs#L90-L110>)、[fanout只要有hits成功](<../../../crates/api/src/catalog_fanout.rs#L455-L481>)、[cache fallback](<../../../crates/media/src/client.rs#L393-L407>)。
错误key+其他源有Dune/旧缓存，显示ok:true。建议绕缓存、独立测试指定源。

### G07 P2 — 字幕有发现但无远端交付，Web轨道固定空（S）
证据：[外部轨DTO本地Path](<../../../crates/media-server/src/dto/media_streams.rs#L47-L61>)、[Web decision空列表](<../../../crates/api/src/http/playback.rs#L279-L281>)、[session空URL](<../../../crates/api/src/http/playback.rs#L348>)。
旁挂SRT已探测，Jellyfin无DeliveryUrl/字幕HTTP路由，远端拿不到本地path；Web audio/subtitle菜单空。不是要求实时转码，而是已有文件缺读取交付合同。

### G08 P2 — 空章节cache直接借用同季其他集绝对marker（S）
证据：[无验证借用并缓存](<../../../crates/api/src/marker_resolver.rs#L66-L103>)。
两集时长/credits不同，缓存空的本集取得另一集intro/outro绝对时间；非空cache随后直接返回，新正确marker不能自动修复。建议仅继承有验证的intro策略、不得无条件借outro，显式版本失效。

### G09 P2 — PlayerPage路由参数变化不更新current（S）
证据：[current仅初始化](<../../../web/components/player/player-page.tsx#L63-L68>)、[unit读current](<../../../web/components/player/player-page.tsx#L101-L109>)、[路由无key](<../../../web/app/play/[mediaItemId]/[[...unit]]/page.tsx#L32-L45>)。
React Router同一路由挂载下浏览器前进/后退或导航从S1E1改S2E3，props变化但current不变，仍播放旧季集；mediaItemId变更可搭旧季集请求新Media。failed/info/episodes也未整体按scope清空。建议以URL为状态源或按media/unit key重挂，并测history导航。

## 复现记录

- 父审执行真实 `groupTodayArrivals`：输入2个不同UUID，输出`{"inputRows":2,"outputGroups":1,"ids":["NaN"],"titles":["Media 0"]}`，exit0。无文件写入。
- 分区审计执行内存Node投影：UUID首页默认Library行hidden/rating/name偏好被丢，补成hidden=false/added_at/name空默认行；exit0。
- 其余均S，不冒充真实HTTP/Downloader/浏览器实测。禁止为证明OOM执行42亿range；禁止针对生产文件验证误删。

## 修复顺序建议

1. **紧急安全/数据门禁**：A01–A03；C01–C03；B01/B02/B06–B09；D01–D03。对各破坏性HTTP入口建立授权、identity、ownership合同。
2. **持久化恢复**：B03–B05/B10；补故障注入和重启回读，验证文件、ledger、facts、pending一致。
3. **可用核心环**：E01/E02；D08–D11；C04/C05；G01/G02；使用真实serializer→HTTP→Store回读，不只HTTP200。
4. **页面契约**：F03–F11、E04–E08、D12–D15；统一分页/sort/facets/UUID/异步scope。
5. **长期连接/配置与增量监听**：A04–A07、B11–B14、G03–G09。

每个issue是独立垂直slice，不把本报告当成一个大修复提交；先补行为红灯再最小实现，相关crate及最终workspace验证。规范与功能分开：本报告未把historical size/lint当bug计入。

## 覆盖地图与限制

- 16个Rust crate按关键生产链路覆盖：domain/store/indexer/media/downloader/library/subscribe/filter/hooks/jobs/api/playback/release/marker/media-server/cover-generator；cover生成算法本体、所有parse profile与巨型页面全部渲染分支未逐函数穷尽。
- 深入：REST路由/auth/users/devices、Jellyfin认证/stream/WS；ledger/facts/recycle/collect/worker/legacy run；Downloader投递/完成/删除/列表/路由；Subscribe选择/slot/Release边界/Jobs/SSE；scan/watch/duplicates/claim；Site与手动下载；Library墙/gallery/manage/favorites/collection/reidentify/artwork；Playback状态、Range/STRM、metadata fanout/配置。
- 正常worker policy reload/guard、Job queued取消/允许running完成、Indexer单Site错误隔离、单Range/STRM关键实现阅读后没有新增确定finding；这不是证明无bug。
- 没有浏览器完整交互、real BT/TMDB、真实媒体编码/字幕渲染测试；第三方接口实际版本兼容仍需专门验收。
- 有一个最初Playback子审失败，随后独立补位重新读取并核验，不依赖其未完成结果。
- 当前未提交工作区还含上一轮变更及用户新增README.zh.md；本审计未替用户提交或丢弃这些内容。
