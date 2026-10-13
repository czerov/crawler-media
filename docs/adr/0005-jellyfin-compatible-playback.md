# 实例是 Jellyfin 兼容的播放服务器

**Library** 是已拥有文件的唯一事实来源。Infuse 等客户端通过 Jellyfin 兼容 HTTP API 连接，把这个进程当作 Jellyfin 服务器：浏览、直连播放、同步进度。我们只实现这些客户端实际调用的子集（认证、视图、条目、播放信息、Range 流、会话进度）。不克隆完整 Jellyfin（LiveTV、SyncPlay、DLNA、插件目录）。

这是从 MoviePilot（为外部服务器整理）向自己拥有媒体库并直接提供服务的产品分叉。v1 的这套 API 不做转码：MediaSources 只宣称直连播放。以后的 Web 播放器可以在不同路径上转码，而无需改变面向 Infuse 的响应。qBittorrent / Transmission 仍是下载器；播放永远不会拉取 Torrent。

**备选方案**：无播放，把媒体库作为 Emby/Jellyfin 的文件夹；仅海报墙 UI；实现 Emby 或 Plex。

## Jellyfin 协议认证约定

`/` 与 `/emby` 是 Jellyfin 客户端表面，认证必须兼容 Jellyfin 客户端协议；不能把自有 `/api/v1` 的 Bearer 认证规则当成唯一格式套到这里。`/api/v1` 的管理 API 仍使用自己的认证约定，两者不可混为一谈。

登录使用 `POST /Users/AuthenticateByName`，JSON body 包含 `Username` 和 `Pw`。成功响应中的 `AccessToken` 是服务端签发的会话凭证；客户端后续请求应携带该凭证，不能把用户密码当作 token。Jellyfin 的标准 Authorization 头使用 `MediaBrowser` scheme 和客户端/设备元数据，例如：

```http
Authorization: MediaBrowser Client="VidHub", Device="iPhone", DeviceId="device-id", Version="1.0", Token="<AccessToken>"
```

保持对已观察客户端的兼容：当前服务还接受 `X-Emby-Authorization` 中的 `MediaBrowser` / `Emby` token，以及 `X-Emby-Token` 和 `X-MediaBrowser-Token`。实现目前也接受 `Authorization: Bearer` 和 Jellyfin 播放 URL 中可能出现的 `api_key`、`apikey`、`token` 查询参数；这些兼容入口不能成为移除 `MediaBrowser` 认证的理由。查询参数可能进入代理访问日志，抓包、日志与错误报告必须遮盖 token 和完整敏感 URL。

认证只回答凭证对应哪个用户；媒体库可见性、用户范围和各路由的授权检查仍须执行。不能为了消除 401 而公开受保护的 Jellyfin 路由或跳过用户授权。只读静态图片的公开路由规则另见仓库 `AGENTS.md`。

`AuthenticateByName` 签发的会话 token 当前有效期为 30 天；配置的管理员 CLI token 属于另一种凭证，替换或重新登记时旧值会失效，不能把它当作 VidHub 的长期 Jellyfin 登录凭证。持久媒体客户端应通过 `AuthenticateByName` 获取自己的 `AccessToken`。任何更改 token 过期、撤销或清理逻辑的安全回归，都必须评估 VidHub 这类已登录客户端的续登行为，并补上协议层集成回归，不能只因格式不同就拒绝有效的 Jellyfin token。

鉴权回归至少覆盖：

1. `AuthenticateByName` 返回的 `AccessToken` 可访问受保护 Jellyfin 路由。
2. 标准 `Authorization: MediaBrowser ... Token=...` 以及上面列出的 VidHub 兼容头可通过鉴权。
3. 缺失、错误、过期或已撤销的 token 得到 `401`；有效 token 仍受用户范围和媒体库可见性约束。
4. 401 排查先核对脱敏后的请求认证 scheme、token 是否来自登录响应、会话年龄及是否被撤销；不得记录原始 token，也不得通过关闭鉴权来“修复”兼容性。

协议格式以 [Jellyfin TypeScript SDK 生成 Authorization 头的实现](https://github.com/jellyfin/jellyfin-sdk-typescript/blob/1ef06252cef5d1729646331e64059e230f11a08b/src/utils/authentication.ts) 为参考；URL 中携带凭证时的日志风险见 [Jellyfin 反向代理文档](https://jellyfin.org/docs/general/post-install/networking/reverse-proxy/#logging)。VidHub 实际需要的兼容形式以抓包证据和本仓库集成测试为准。

## 「最近入库」的播放状态口径

`Items/Latest` 的 `IsPlayed` 只保留协议里最硬的那条：**显式**传了 `true`/`false` 就严格筛（客户端要什么给什么，`latest_items_honors_is_played_filter` 盯着）。**没传**时按服务端的默认策略走。Jellyfin 的默认策略来自用户配置 `HidePlayedInLatest`（出厂 `true`）：它把 `isPlayed` 置为 `false`，把看过的藏掉，全看过的库更是直接返回**空列表**——客户端首页上那个库连入口都没有。

我们缺省**只排不筛**：没看完的排前面、已看完的沉底，段内仍是入库时间倒序（实现见 `crates/library/src/latest.rs` 的 `played_last`）。自有 API 那边是同一条思路、更细的分级（`WatchTier`：未看 → 在看 → 已看完），由首页库行与「我的收藏」行传 `unwatched_first=true` 打开；墙上用户手选的「未观看」（`w=unwatched`）始终是严格筛，不受影响。

理由：一个只有 3 部剧、其中 2 部看过的小库，筛完只剩 1 张卡，看起来像坏了；而"最近入库"这个名字描述的就是入库时间，把看过的藏起来反而名不副实。Jellyfin Web 的「最近添加」不传 `isPlayed`（`src/components/homesections/sections/recentlyAdded.ts` 只传 `userId/limit/fields/parentId`），所以它也会看到已看完的排在行尾。

代价（有意的）：客户端再没有开关能把已看完的藏掉——只有**显式**传 `IsPlayed=false` 才严格，而多数客户端不传。要做到"用户自己决定藏不藏"，得补用户配置表面（`UserConfiguration.HidePlayedInLatest` 与 `GET/POST /Users/{id}/Configuration`；Jellyfin Web 本来就读 `user.Configuration`），落地后可以按这个开关在"严格筛"与"只排不筛"之间切。这套表面目前**尚未实现**，`/Users/Me` 也不返回 `Configuration` 块。
## 播放会话代际与 PlaySessionId 约束 (2026-10-13 Batch 2)

Jellyfin 客户端在 `PlaybackInfo` 阶段获得由服务端生成的持久 `PlaySessionId`（绑定 User、Device、LedgerId）。

- 客户端在 `/Sessions/Playing`、`/Sessions/Playing/Progress` 和 `/Sessions/Playing/Stopped` 中回传可选的 `PlaySessionId` 和 `EventSequence`。
- 迟到的旧会话 Progress / Stop 事件（generation 低于当前会话）不会影响或关闭正在播放的新会话。
- 无 `PlaySessionId` 的旧客户端（Legacy）仅在目标 LedgerId 与当前活跃会话相同时才更新状态，避免跨文件污染。
- 观看时长累计严格按墙钟推进与播放倍率限制，超过正常播放速率与容差的快进跳转（Seek）不虚增观看时长。
