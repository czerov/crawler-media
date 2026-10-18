# crawler-media

A self-hosted media automation system that also serves the library. The user names a movie or TV season; the system keeps searching configured PT sites, downloading matching torrents, and organizing files into a **Library** that playback clients can open as a Jellyfin server.

## Language

**Media**:
A canonical movie or TV work assembled from one or more **Metadata sources**. Identity is an internal id; TMDB / Douban / TVDB / Bangumi / AniList ids are aliases. A TV **Media** is identified at work level; a season is a range on a **Subscribe**, not a separate **Media**.
_Avoid_: 剧, show, title (as the identified object), TMDB result

**Subscribe**:
A long-running target that binds one **Media** to a coverage range (a movie, or a TV season and episode window) plus search/download policy. It records download facts (which episodes exist, and at what quality) and keeps filling or upgrading that range until it completes.
_Avoid_: 搜索任务, watchlist, request, 追更

**Site**:
A configured PT tracker the system is allowed to search. Runtime credentials (cookie, API key, RSS URL, proxy, rate limit, optional CDP endpoint) live on the **Site**; HTML/API parsing rules live on the **Indexer**. Login and **Check-in** are not search; they keep this **Site** authenticated.
_Avoid_: tracker (except when quoting BT protocol), 爬虫配置

**Indexer**:
The parsing/search adapter for one **Site**: a framework (NexusPHP HTML or API) plus a per-site YAML overlay for selectors, categories, and RSS. Fetching a search/RSS page always uses HTTP; a profile `render` flag is ignored. Built-in profiles ship with the app; a user overlay directory can replace a profile without a rebuild. An **Indexer** does not own login credentials. M-Team is a first-class API exception behind the same **Torrent** output; other API sites follow that pattern. Search across enabled **Sites** is concurrent; one **Site** failing does not fail the others.
_Avoid_: spider, crawler, scraper (as the configured object)

**Torrent**:
A normalized candidate from a **Site** search or RSS pull: title, size, seeders, enclosure/download URL, free/HR flags, and optional IMDB id. It is not yet a download task.
_Avoid_: 资源 (too vague), SearchResult (implementation name), magnet (one enclosure form)

**Downloader**:
An external BitTorrent client the system talks to (qBittorrent or Transmission). The system adds **Torrents** to it and later reads completed files; it does not download bytes itself.
_Avoid_: 下载模块 (the code package), client (too generic)

**Library**:
The authoritative on-disk collection the instance owns: which **Media** we have, at what quality, at which path. **Subscribe**, **Transfer**, **Scrape**, and **Playback** all read this, not the **Downloader** save path. A **Library** has a kind (movie or TV), one or more root paths, and a ledger of files. Ledger quality (resolution, codec, HDR) comes from probing the file at **Transfer** time; the **Release** parsed from a name is fallback only.
_Avoid_: 媒体库路径 (as the concept), collection (MoviePilot uses this for TMDB collections), Jellyfin library (that's a view over this)

**Transfer**:
Moving completed files from a **Downloader** (or an intake **Watch directory**) into the **Library**, including rename. The **Transfer mode** is user-configured: hardlink, copy, or move. Default is hardlink when source and destination share a filesystem, otherwise copy.
_Avoid_: 整理 (too broad; includes **Scrape**), 入库 (the outcome, not the action)

**Naming template**:
An instance-global relative-path pattern for **Transfer**, one for movie **Media** and one for TV **Media**. Placeholders expand from **Media**, **Release**, and the probed file. Empty values omit empty parentheses. Stored in SQLite; defaults live in `library::default_pattern`.
_Avoid_: Jinja, format string (too generic), 重命名规则 (synonym — use **Naming template**)

**Transfer mode**:
How bytes are placed in the **Library**: hardlink, copy, or move. Move removes the source; hardlink and copy do not. After a successful **Wash-cut**, the previous **Library** files are deleted regardless of mode (a leftover hardlink does not delete the **Downloader**'s seeding copy).
_Avoid_: 链接, 拷贝策略

**Wash-cut**:
A **Subscribe** mode that keeps replacing an already-owned movie or episode with a higher-quality **Torrent**. Ordinary fill and wash-cut share the same per-episode existence and quality facts; they differ in completion rules. TV wash-cut is per-episode by default; a full-season pack is an optional constraint on the same **Subscribe**, not a second product.
_Avoid_: 洗版订阅 (as a separate entity), re-subscribe, upgrade-only job

**Filter**:
A named group of atomic rules. Built-in atoms (resolution, source, free, HR, subtitle language, …) plus user-defined atoms (title match, size, seeders, site) are composed into a group. A group both *rejects* a **Torrent** and *scores* survivors: the score is the highest priority among matched atoms. Manual search and **Subscribe** bind a group; **Wash-cut** may bind a second group and otherwise reuses the same one.
_Avoid_: 优先级 (as the rule object), 质量 (as the rule object), 选种规则 (synonym — use **Filter**)

**Fetch mode**:
Per-**Subscribe** choice of how to obtain **Torrents**: keyword search, RSS, or both. Search and RSS are two fetch methods on the **Indexer**, not two subscription systems.
_Avoid_: 搜索模式 (ambiguous with media search), 爬取模式

**Metadata source**:
An external catalog used to identify and describe **Media**: TMDB, Douban, TVDB, Bangumi, AniList. Sources can alias the same **Media**; they are not themselves the **Media**.
_Avoid_: scraper (that's **Scrape**), recognizer

**Scrape**:
Writing sidecar metadata next to files — **NFO** plus artwork — so **Playback** and any external scanner can identify them. **Scrape** can run after **Transfer**, on a **Watch directory**, or on a path the user points at. It is optional and switchable; **Transfer** does not imply **Scrape**.
_Avoid_: INFO, 刮削模块 (as the concept), embed metadata into the video

**NFO**:
The XML sidecar file written beside a movie, season, or episode file. This is the Emby/Jellyfin convention.
_Avoid_: INFO, .txt metadata

**Watch directory**:
A filesystem path the system observes. It may be an intake path (new files get transferred into the **Library**) or a library path (existing files get **Scraped** in place). Those are different jobs and must not share one implicit meaning.
_Avoid_: 监控 (too vague), inotify

**Hook**:
A named extension point on a workflow step (choose **Torrent**, add to **Downloader**, transfer, rename, **Scrape**) where a **Plugin** may observe, veto, or adjust that step. A **Hook** does not replace the **Indexer** or **Downloader** module.
_Avoid_: middleware, interceptor, callback

**Plugin**:
An optional first-party (later also user-supplied) extension that registers **Hooks**. Login and **Check-in** ship as **Plugins** that drive a **Browser**. Core Indexer / Downloader / Media / Library search-and-file code is compiled-in modules, not **Plugins**. A third-party loader is still later; first-party **Plugins** compile against the same Hook + **Browser** APIs that loader will use.
_Avoid_: 插件市场 (a distribution surface, not the concept)

**User**:
A person who owns **Subscribes**, a login, and their own **Playback** progress. **Sites**, **Downloaders**, **Indexer** profiles, and the **Library** files are shared by the instance. A **User** is not a second media library.
_Avoid_: account, tenant

**Release**:
A parsed reading of a torrent title or a filename, as separate fields: title, year, season/episode range, resolution, source, codec, HDR, subtitle language, audio language, group. It is not **Media** and not a **Torrent**. Matching and **Transfer** naming both consume a **Release**. Low confidence does not guess: auto-**Subscribe** skips that **Torrent**; an intake **Watch directory** parks the file as **Unidentified**.
_Avoid_: MediaMeta (movietools type name), MetaInfo (MoviePilot type name)

**Unidentified**:
A file (or **Torrent**) whose **Release** or **Media** match is below confidence. It is listed for a **User** to claim; the system does not **Transfer** or download it on guesswork.
_Avoid_: 失败, unknown media

**Playback**:
Serving **Library** files to clients. External apps (Infuse, Fileball, and similar) talk to a Jellyfin-compatible HTTP API and treat this instance as a Jellyfin server. Playback routes, including video streams, follow Jellyfin protocol authentication and remain authenticated. Static artwork routes stay public for native image requests. **Playback** does not replace the **Downloader**.
_Avoid_: 媒体服务器 (too broad), 转码服务 (a later, separate capability)

**Browser**:
A CDP session used only to drive login / **Check-in** for **Sites** that opt in (`maintenance_via = browser`); search and RSS always use HTTP and never a **Browser**. One instance-level switch decides whether CDP is used at all: off means login / **Check-in** use HTTP, and no browser process is probed, downloaded, or started. When on, the endpoint (`http` discovery URLs only) resolves explicit **Site** `cdp_url` → instance `cdp.url` → a managed Obscura daemon; a failure before navigation falls back to HTTP with a warning, a failure after navigation is reported without retrying over HTTP. Each session runs in its own isolated browser context (created and disposed per session), so cookies never leak between Sites or accounts and no cookie is ever deleted from other contexts; endpoints without browser-context support are marked incompatible. Cookies are injected host-only and read back only for the Site URL. Same registrable-domain sessions are serialized to avoid cookie races. The Site proxy is not used. Existing data directories are upgraded in place; existing Sites default to HTTP maintenance. The managed daemon starts only when no instance endpoint is set and at least one enabled **Site** opts into the **Browser** without its own endpoint; it binds loopback on a random port. The default image ships neither Chromium nor Obscura; by default only a preinstalled, hash-verified release archive is used, and downloading requires an explicit admin choice. Saving an endpoint config is not proof of a usable browser. This is an automation channel, not a Windows remote desktop.
_Avoid_: RDP, 无头模块, Playwright (implementation)

**Check-in**:
A scheduled **Site** maintenance action (attendance / cookie refresh). Implemented as a **Plugin** on **Browser** or HTTP. It does not produce **Torrents** and is not a **Fetch mode**.
_Avoid_: 签到模块 (as core Indexer), attendance search

**Job**:
One unit of work the instance can queue, run, retry, or inspect: **Subscribe** search/RSS, **Transfer**, **Scrape**, **Check-in**, catalog refresh, later Watch-directory intake. Definitions (`job_defs`) carry schedule; each run is a `jobs` row in SQLite. **Hooks** intercept steps inside a **Job**; they are not the queue.
_Avoid_: scheduler (the loop, not the record), APScheduler, cron (the schedule string is not the concept), 任务系统 (synonym — use **Job**)

## Relationships

- A **Subscribe** is owned by one **User** and targets exactly one **Media**
- A **Subscribe** has a **Fetch mode** and a **Filter**
- A **Media** has one internal id and optional aliases from **Metadata sources**
- A **Site** is searched through its **Indexer** over HTTP
- Search and RSS both produce **Torrents**
- A **Filter** admits and scores a **Torrent** (via its **Release**) for a **Subscribe**
- A **Downloader** receives a **Torrent** and later yields files that are **Transferred** into the **Library**
- **Wash-cut** is a mode of a **Subscribe**, not a separate record
- **Scrape** writes **NFO** and artwork beside **Library** files
- **Playback** reads the **Library** ledger; it does not own files
- Login and **Check-in** are **Plugins** that may use **Browser**; they write back **Site** credentials
- A **Plugin** participates only through **Hooks**; first-party modules and first-party **Plugins** share that bus
- **Sites**, **Downloaders**, and the **Library** are instance-global, not per-**User**
- A **Job** is the runnable unit for search, **Transfer**, **Scrape**, and **Check-in**; a **Subscribe** is policy, not a thread
- Creating a **Subscribe** inserts its search **Job** definition; **Transfer** is instance-wide, not per-**Subscribe**
- **Transfer** names files with the **Naming template** for that **Media** kind; the **Downloader** filename is not kept
