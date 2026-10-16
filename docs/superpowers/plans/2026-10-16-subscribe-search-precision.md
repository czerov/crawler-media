# 订阅搜索精度实现计划

> **给执行的 agent：** 必须使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans，按任务逐个实现。步骤用 `- [ ]` 跟踪。

**目标：** 订阅搜索不再漏掉落在第 1 页之后的那一集，同时不拿短别名去搜，也不把日期当成集号。

**做法：** 每个站点请求仍以一页为单位。集数范围已经写明的电视剧订阅，在现有标题之外再加一条季关键词（`醉梦 S01`）。只有这一页满了、而且没有一条对得上这部剧的这一季时，才用同一条季关键词要第 2 页。集号只接受 1 到 999；8 位 `YYYYMMDD` 不是集号。

**技术栈：** Rust workspace 的 `release`、`api`、`indexer`、`subscribe`、`domain`。测试沿用现有假 Indexer 和 fixture。`cargo test` 不访问真实站点或 TMDB。

**规格：** 本文就是规格。它记录 2026-10-16 的决定：要补上的是「订阅只看第 1 页所以会漏」；要避开的是 MoviePilot 那种「别名 + 按最大集号深翻」。

## 明确不做

### 不拿 TMDB 别名去搜

别名搜索会直接重现这次事故。TMDB 别名里有 `Dream` 这种单词。站点用它会返回成百上千条无关种子（BanG Dream!、Grand Blue Dreaming）。我们的标题核对是「词连续出现就算」，`Dream` 也能对上这些名字。结果被拉回来再丢掉，请求已经发出去了。

现在已经搜主标题、原标题、冒号或括号前的短标题，以及「标题 + 年份」。这覆盖中文名、英文名和副标题差异。别名列表、别名长度门槛都不在本计划里。`Media` 继续只有 `title` 和 `original_title`。

### 不做 IMDb / TMDB ID 检索

这条和别名不是同一个理由。ID 检索是精确的，不会造成 `Dream` 那种放量。

不做是因为当前 Indexer 只支持文本查询：NexusPHP 用 `search`，馒头用 `keyword`。没有一个 profile 声明支持 `imdb` 或 `tmdb` 参数。`Media.tmdb_id` 是作品身份，不是站点搜索入口。要做 ID 检索，得先给每种站点定义另一套请求，这是独立的 Indexer 改造，也补不上「只支持文本搜索的站点第 2 页之后」的漏。

所以 ID 检索以后可以单独做，不放进这次。

### 不记录「站点已见最大集」，也不按集号决定翻多少页

原则：**翻页只看这一页的结果像不像这部剧，不看集号推出来的「站点已经出到第几集」。**

集号是从种子标题里猜的。猜错一次，如果还拿它当翻页开关，就会一直翻。MoviePilot 把 `20261002` 当成第 1 集，又记成「已见最大集 = 20261002」，于是认为 1 到 18 集都在后面，翻到第 20 页。

因此：

- 不保存站点已见最大集，下一轮也不从上次的页码接着翻。
- 不用「还缺 E04，站点最新才到 E03，可以停」这种判断。我们没有一份可靠的站点进度。
- 集号仍然用来决定要不要下载某一集。它不决定要向站点要多少页。

翻页规则只有这些：

- 这一页里已经有对得上的这一季，停。
- 这一页不满 30 条，说明没有更多，停。
- 这一页至少 30 条，而且全是别的剧，或者全是这部剧的别季，才用同一条季关键词要第 2 页。
- 没有第 3 页。下一轮重新从第 1 页开始。

## 全局约束

- 用 `CONTEXT.md` 的词：**Subscribe**、**Torrent**、**Release**、**Media**、**Site**、**Indexer**、**Fetch mode**。不要引入 `SearchResult`、`MetaInfo`。
- `domain` 只有类型。`indexer` 只做站点适配。关键词策略放在 `api`。
- `cargo test` 不开网络、qBittorrent 或 Chromium。
- 订阅搜索继续遵守 `Site.rate_limit_per_minute`。一轮搜索里不得自己 sleep 再重试，也不得做冷却循环。
- 沿用现有上限：`Coverage::MAX_EPISODES` 是 1000。解析出的集号必须在 `1..=999`。
- 一轮订阅搜索最多 2 页，而且只有季关键词可以要第 2 页。
- 一个 `.rs` 文件不超过 800 行。一个新函数不超过 120 行。
- 日志：关键词和第 2 页决定用 `info!`；把日期拒绝为集号用 `warn!`；本可要第 2 页但被规则挡住用 `debug!`。

## 审阅时重点看

- 第 1 页满 30 条、全是别的剧，要第 2 页。全是这部剧的别季，也要第 2 页。已经有这一季的任一条，不要第 2 页。
- `20261002`、`2026.10.02`、`E20261002` 不能变成第 20261002 集。`S01E01` 和 `E05` 必须照常解析。
- `episode_to = None` 的追更订阅不加 `S01`，也不为它翻第 2 页。
- 电影订阅保持今天的标题和年份关键词，永远只要第 1 页。
- 同一进程里两个订阅搜同一个站点，仍共用 `rate_limit_per_minute`。第 2 页是另一次请求，不是绕过限速。

---

### 任务 1：日期不再当成集号

**文件：**

- 修改：`crates/release/src/boundary.rs`
- 修改：`crates/release/src/lib.rs`（`scan_tokens_for_season_episode`、`parse_ep_numbers`）
- 测试：`crates/release/tests/parse.rs`

**接口：**

- 使用：`release::parse(raw) -> domain::Release`
- 给出：签名不变。集号不在 `1..=999` 则为 `None`。8 位数字若是合法的 `20YYMMDD`（月 01–12，日 01–31），即使前面有 `E` 或 `EP`，也不是集号。

- [ ] **步骤 1：写会失败的测试**

在 `crates/release/tests/parse.rs` 用现有的 `parse` 辅助函数加上：

```rust
#[test]
fn calendar_dates_are_not_episodes() {
    for raw in [
        "Chinese Dream Program 20261002 1080p",
        "Chinese.Dream.Family.20261002.1080p.WEB-DL",
        "Chinese Dream 2026.10.02 1080p",
        "Show.Name.E20261002.1080p",
        "Show.Name.EP20261002.1080p",
    ] {
        let release = parse(raw);
        assert_eq!(release.episode, None, "{raw}");
        assert_eq!(release.episode_to, None, "{raw}");
    }
}

#[test]
fn ordinary_episode_tokens_still_parse() {
    let dotted = parse("Show.Name.20261002.E01.1080p");
    assert_eq!((dotted.season, dotted.episode), (Some(1), Some(1)));
    let range = parse("The.Long.Watch.2024.S01E03-E04.1080p.WEB-DL");
    assert_eq!(
        (range.season, range.episode, range.episode_to),
        (Some(1), Some(3), Some(4))
    );
}
```

- [ ] **步骤 2：跑测试，确认 `E20261002` 失败**

运行：`cargo test -p release --test parse calendar_dates_are_not_episodes ordinary_episode_tokens_still_parse`

预期：`E20261002` / `EP20261002` 失败，因为集号是 `Some(20261002)`。裸的 `20261002` 现在已经通过；保留它们，防止修的时候退回去。

- [ ] **步骤 3：两处解析共用同一个集号上限**

在 `crates/release/src/boundary.rs` 增加 `fn plausible_episode(episode: u32) -> bool`。只有 `1..=999` 返回 true。数字原文若是 8 位，前四位能被 `parse_year` 接受，中间两位是 `01..=12`，末两位是 `01..=31`，返回 false。

`find_s_e_pattern` 和 `scan_tokens_for_season_episode` 里，每次写入 `episode` / `episode_to` 之前都走这个判断。被拒绝的日期不得顺手把季设成 1。拒绝 `E` / `EP` 日期时 `warn!`，带上原始 token。

- [ ] **步骤 4：重跑 release 测试**

运行：`cargo test -p release`

预期：通过，包括两个新测试和原有的 `S01E03-E04`、中文集号用例。

- [ ] **步骤 5：提交**

```bash
git add crates/release/src/boundary.rs crates/release/src/lib.rs crates/release/tests/parse.rs
git commit -m "fix: do not parse a calendar date as an episode"
```

### 任务 2：只加一条季关键词

**文件：**

- 修改：`crates/api/src/worker/search.rs`
- 测试：若 `crates/api/tests/management/legacy_run_parity.rs` 断言了完整关键词列表，就改那里；否则在 `crates/api/tests/` 现有搜索关键词测试旁新增。

**接口：**

- 使用：`domain::Media`、`domain::Coverage`
- 给出：`search_torrents_for_subscribe(state, sites, media, coverage) -> (Vec<Torrent>, Vec<String>, bool, Vec<String>)`。电影保持今天的关键词。集数有上界的电视剧再加恰好一条：`{短标题或主标题} S{季:02}`。有短标题时用短标题。不加别名，不加 `Season 1`，关键词里不带集号。

- [ ] **步骤 1：写会失败的测试**

电视剧：主标题 `醉梦`，原标题 `Dream`，年份 `2026`，覆盖第 1 季第 1 到 3 集。关键词必须恰好是：

```text
Dream
醉梦
Dream 2026
醉梦 2026
醉梦 S01
```

同一部电影订阅不得包含 `醉梦 S01`。`episode_to = None` 的追更订阅也不得包含 `醉梦 S01`。

- [ ] **步骤 2：跑测试，确认失败**

运行：`cargo test -p api --test <测试文件名> season_keyword -- --nocapture`

预期：失败，因为没有 `醉梦 S01`，而且函数还不接收 `Coverage`。

- [ ] **步骤 3：实现 `search_torrents_for_subscribe`**

`search_torrents_for_media` 继续负责标题和年份。新函数先调用它，仅当覆盖是 `Tv { episode_to: Some(_), season, .. }` 时追加季关键词。季号格式化为两位。用现有 `HashSet` 去重。`crates/api/src/worker.rs` 和 `crates/api/src/management/subscribes/run.rs` 的调用处传入 `subscribe.coverage`。最终关键词列表打 `info!`。

- [ ] **步骤 4：跑 api 搜索测试**

运行：`cargo test -p api season_keyword legacy_run`

预期：通过。

- [ ] **步骤 5：提交**

```bash
git add crates/api/src/worker/search.rs crates/api/src/worker.rs crates/api/src/management/subscribes/run.rs crates/api/tests
git commit -m "feat: search a bounded TV subscribe with its season token"
```

### 任务 3：有条件地再要一页

**文件：**

- 修改：`crates/api/src/worker/search.rs`
- 仅当槽位判断还不能从外部调用时，才改 `crates/subscribe/src/choose.rs`。优先在 `matches_media` 旁边加 `subscribe::wanted_slots_visible`，不要把逻辑塞进 `choose`。
- 测试：`crates/api/tests/` 里用会记录请求页码的假 Indexer。

**接口：**

- 使用：`Indexer::search_page(sites, keyword, page)`、任务 1 的 `Release`、任务 2 的季关键词
- 给出：只有下面全部成立才请求第 2 页：覆盖是有上界的电视剧；季关键词的第 1 页至少有一条；每一条都没能用这条季关键词对上这部剧的这一季；第 1 页条数 `>= 30`。第 2 页用同一条季关键词。没有第 3 页。

- [ ] **步骤 1：写会失败的测试**

用 api crate 的假 Indexer，覆盖四种情况：

1. `醉梦 S01` 的第 1 页含有 `醉梦 S01E02`。请求页码是 `[1]`。
2. 第 1 页是 30 条对不上的 `Other Show S01E01`，第 2 页有 `醉梦 S01E02`。请求页码是 `[1, 2]`，合并结果包含第 2 页的 enclosure。
3. 第 1 页是 30 条 `醉梦 S09E01`（剧名对，季不对）。请求页码是 `[1, 2]`。
4. 第 1 页只有 5 条对不上的结果。请求页码仍是 `[1]`。

另外断言：电影关键词即使第 1 页有 100 条，也不得请求第 2 页。

- [ ] **步骤 2：跑测试，确认失败**

运行：`cargo test -p api --test <测试文件名> second_page`

预期：失败，因为现在只请求第 1 页。

- [ ] **步骤 3：在 `search_keywords` 里做这个门**

季关键词第 1 页回来后，调用私有的 `page_is_full_and_irrelevant`。它用 `release::parse` 解析每条种子标题，再问 `subscribe` 这条 Release 是否通过季关键词对上这部剧。「满」的定义是 `torrents.len() >= 30`。只有通过才调用 `indexer.search_page(sites, season_keyword, 2)`，并按 enclosure 合并。`info!` 记下关键词、第 1 页条数、第 2 页是否发出。不要写循环。

- [ ] **步骤 4：跑 api 测试**

运行：`cargo test -p api second_page`

预期：通过。`crates/indexer` 里的限速测试不用改：第 2 页仍走 `Indexer::fetch`，原有的每分钟限额会把它隔开。

- [ ] **步骤 5：提交**

```bash
git add crates/api/src/worker/search.rs crates/subscribe/src/choose.rs crates/api/tests
git commit -m "feat: fetch one more page when the season page is full of other shows"
```

### 任务 4：全仓验证

**文件：**

- 不新增生产代码。

- [ ] **步骤 1：先跑改过的 crate，再跑整个 workspace**

运行：`cargo test -p release && cargo test -p api && cargo test --workspace`

预期：全部通过。没有测试去打开网络连接。

- [ ] **步骤 2：确认非目标没有漏进来**

在 diff 里搜 `aka`、`alias`、`imdb`、`page 3`。这些搜索路径都不该出现。`Media` 仍然只有 `title` 和 `original_title`。

- [ ] **步骤 3：只有任务 4 改了文档或测试才提交；否则停**
