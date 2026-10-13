# Scrape Artwork and Fanart.tv Implementation Plan

> **历史状态（2026-10-13 整理）：** 实现已交付（3971634、e913904、65eba49、93ea3bd）；季 NFO、分集 still 和 TV/movie Fanart 路径已存在。空/空白 Fanart key 回退内置 key，并非关闭，见 [有效配置](<../../../crates/api/src/scrape_config.rs#L256-L262>)。真实 TMDB/Fanart 可用性、UI/VidHub 显示仍需实际验收。
>
> 本文保留原设计、发现/任务 ID 与当时证据；日期本身不表示过时。旧执行/提交/清理命令仅作历史记录，不应直接重跑；复选框不是当前实现清单。当前依据：[数据库/API review](<../../audits/2026-10-13-database-api-review.md>) 与 [剩余计划](<2026-10-13-database-api-followup.md>)，未覆盖的旧 ID 仍须单独对账。

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** A TV scrape writes `season.nfo`, downloads each owned episode still beside the video, and — when a Fanart.tv key is configured — downloads the show logo, thumb, and banner plus each season's poster, thumb, and banner using Jellyfin file names.

**Architecture:** TMDB stays the source for `poster.jpg`, `fanart.jpg` (its backdrop), `season.nfo` text, and episode stills. Fanart.tv is a second client keyed by TVDB id for TV and TMDB id for movies. It never replaces an existing `poster.jpg` or `fanart.jpg`. Episode stills download from the TMDB `still_path` already parsed on `EpisodeMeta`; if that is missing, `library::extract_frame` seeks the URL inside a `.strm` file. The reference for Fanart field names and language ranking is MoviePilot `app/modules/fanart/__init__.py` (`_FANART_NAME_MAP`, `__pick_best_image`, `__extract_images`). Jellyfin's local names are `logo`/`clearlogo`, `banner`, `thumb`/`landscape`, and `seasonNN-poster|fanart|banner|landscape`.

**Tech Stack:** Rust workspace (`media`, `library`, `api`), existing `CatalogGet` + `CatalogCache` for HTTP, `PosterFetch::get` for image bytes, `roxmltree` for NFO, React settings form in `web/components/scrape-settings-section.tsx`.

## Global Constraints

- Vocabulary from `CONTEXT.md`. Do not invent `SearchResult`, `MetaInfo`, or `MediaMeta`. The on-disk backdrop stays `fanart.jpg`; that name is not the Fanart.tv service.
- Tests first. `cargo test` must not call TMDB, Fanart.tv, ffmpeg, or qBittorrent. Inject `CatalogGet` / `PosterFetch` and pass a fake ffmpeg path to `extract_frame_with_ffmpeg`.
- No `unwrap` / `expect` on IO or parse paths except tests.
- Logs: `info!` when a season NFO or image file is written; `warn!` with the error when a download, parse, or frame grab fails and the scrape continues. Never log the Fanart API key or a full image URL that contains `api_key=`.
- `mirror_images` and `mirror_nfo` and `mirror_episode_thumbs` default to `true`. An absent, empty, or whitespace-only Fanart API key falls back to the built-in key; it does not disable Fanart. Image writes still depend on `mirror_images`, required provider IDs and available artwork.
- Do not overwrite an image file that already exists (`poster.jpg`, `fanart.jpg`, logo, banners, thumbs, stills). Do not delete the Emby pairs `<stem>.jpg` and `<stem>-thumb.jpg`. NFO files (`tvshow.nfo`, `season.nfo`, `<stem>.nfo`) are metadata text sidecars that refresh on scrape; `write_season_nfo` safely updates `<season>` metadata while rejecting non-season roots with `InvalidData`.
- Fanart does not replace TMDB `poster.jpg` or TMDB `fanart.jpg`. Fanart `showbackground` / `moviebackground` are not written.
- Fanart has no per-episode images. Episode stills stay on TMDB, then ffmpeg.
- A TV Fanart request requires `media.tvdb_id`. A movie Fanart request requires `media.tmdb_id`, writing `logo.png`/`clearlogo.png`, `thumb.jpg`/`landscape.jpg`, `banner.jpg` into the movie directory. Missing id skips Fanart and logs `info!`.
- Production `.rs` files stay under 800 lines (`wc -l`). `crates/library/src/nfo.rs` is 792; the season writer is a new file. `crates/api/src/poster_fetch.rs` is 392; Fanart saving is a new file, not more functions in `poster_fetch.rs`.
- Image language rank, copied from MoviePilot: configured language list, then `zh`, then `en`, then the candidate with the most `likes`. `lang` of `""` or `"00"` means no text.

## File names this plan writes

Show directory (parent of a `Season N` directory, otherwise the video directory):

| File | Source | Fanart.tv key or TMDB field |
|---|---|---|
| `poster.jpg` | TMDB, already implemented | do not touch |
| `fanart.jpg` | TMDB backdrop, already implemented | do not touch |
| `logo.png` | Fanart | `hdtvlogo` or `hdmovielogo` (fallback `movielogo`) |
| `clearlogo.png` | same bytes as `logo.png` | Jellyfin reads either name |
| `thumb.jpg` | Fanart | `tvthumb` or `moviethumb` |
| `landscape.jpg` | same bytes as `thumb.jpg` | Jellyfin Thumb also reads `landscape` |
| `banner.jpg` | Fanart | `tvbanner` or `moviebanner` |
| `seasonNN-poster.jpg` | Fanart `seasonposter` where `season == NN` | also `poster.jpg` inside that season's directory when the directory exists |
| `seasonNN-thumb.jpg` | Fanart `seasonthumb` | also `thumb.jpg` and `landscape.jpg` inside the season directory |
| `seasonNN-banner.jpg` | Fanart `seasonbanner` | also `banner.jpg` inside the season directory |
| `season.nfo` | TMDB `TvSeason` | inside the season directory; flat multi-season shows use `seasonNN.nfo` at the show root |
| `<stem>-thumb.jpg` | TMDB episode `still_path`, else ffmpeg | beside the episode file |

`NN` is the season number printed with two digits (`01`). Season `0` uses the prefix `season-specials` (MoviePilot), so the files are `season-specials-poster.jpg`, `season-specials-thumb.jpg`, `season-specials-banner.jpg`.

## Out of scope

- `clearart`, `disc`, and `cdart`. MoviePilot downloads them; this scrape does not, until a caller asks.
- Replacing TMDB `poster.jpg` or `fanart.jpg` with a Fanart image.
- Deleting duplicate Emby episode jpgs.

---

### Task 1: Season NFO writer

**Files:**
- Create: `crates/library/src/nfo_season.rs`
- Modify: `crates/library/src/lib.rs` (add `mod nfo_season;` next to `mod nfo;` and re-export `write_season_nfo`)
- Test: `crates/library/tests/season_nfo.rs`

**Interfaces:**
- Consumes: `library::NfoMeta` (`title`, `plot`, `premiered`, `thumb`, `season`).
- Produces: `pub fn write_season_nfo(path: &std::path::Path, meta: &library::NfoMeta) -> std::io::Result<()>`
  - Missing file is created with root `season`.
  - Existing root `season` is replaced by a document that contains only the non-empty fields below. This writer owns the file; it does not preserve unknown children.
  - Existing root other than `season` returns `std::io::ErrorKind::InvalidData` and the bytes on disk stay as they were.
  - Elements, in order, each omitted when the source is empty: `title`, `plot`, `premiered`, `year`, `seasonnumber`, `thumb`.
  - `year` is `premiered[0..4]` when those four bytes parse as `u16`. `seasonnumber` is `meta.season`.
  - Escape `&`, `<`, `>` in text values.

- [x] **Step 1: Write the failing test**

```rust
use library::{NfoMeta, read_nfo, write_season_nfo};

#[test]
fn write_season_nfo_creates_season_root_with_plot_and_number() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("season.nfo");
    let meta = NfoMeta {
        title: Some("第 1 季".into()),
        plot: Some("第一季简介".into()),
        premiered: Some("2022-10-10".into()),
        thumb: Some("https://image.tmdb.org/t/p/w780/season.jpg".into()),
        season: Some(1),
        ..NfoMeta::default()
    };
    write_season_nfo(&path, &meta).unwrap();
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(body.contains("<season>"), "{body}");
    assert!(body.contains("<seasonnumber>1</seasonnumber>"), "{body}");
    assert!(body.contains("<year>2022</year>"), "{body}");
    let parsed = read_nfo(&path).unwrap();
    assert_eq!(parsed.title.as_deref(), Some("第 1 季"));
    assert_eq!(parsed.plot.as_deref(), Some("第一季简介"));
    assert_eq!(parsed.premiered.as_deref(), Some("2022-10-10"));
    assert_eq!(parsed.season, Some(1));
    assert_eq!(
        parsed.thumb.as_deref(),
        Some("https://image.tmdb.org/t/p/w780/season.jpg")
    );
}

#[test]
fn write_season_nfo_refuses_to_replace_tvshow_root() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("tvshow.nfo");
    std::fs::write(&path, "<tvshow><title>keep</title></tvshow>").unwrap();
    let error = write_season_nfo(&path, &NfoMeta::default()).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(std::fs::read_to_string(&path).unwrap().contains("keep"));
}
```

- [x] **Step 2: Run the test to verify it fails**

Run: `cargo test -p library --test season_nfo -- --nocapture`

Expected: compile failure, `write_season_nfo` is not found.

- [x] **Step 3: Write the minimal implementation**

`write_nfo` cannot be reused: `root_tag` in `crates/library/src/nfo.rs` returns `tvshow` for every TV `Media`. Seed a missing path with:

```xml
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<season>
</season>
```

Parse with `roxmltree`. Reject a non-`season` root before writing. Build the new body from the fields listed in Interfaces. Log `info!(path = %path.display(), "已写入 season.nfo")` after a successful write. Do not log `plot`.

`read_nfo` already accepts root `season` (`is_supported_root`). It does not read `seasonnumber`. The test asserts `parsed.season == Some(1)`, so `parse_nfo` must treat `seasonnumber` as an alias of `season`. In `crates/library/src/nfo.rs` change the season line inside `parse_nfo` from `child_u32(root, &["season"])` to `child_u32(root, &["season", "seasonnumber"])`. That is the only edit to `nfo.rs`. `wc -l crates/library/src/nfo.rs` must stay at or below 800.

- [x] **Step 4: Run the test to verify it passes**

Run: `cargo test -p library --test season_nfo -- --nocapture`

Expected: PASS. Then `cargo test -p library --test nfo -- --nocapture` still PASS.

- [x] **Step 5: Commit**

```bash
git add crates/library/src/lib.rs crates/library/src/nfo.rs crates/library/src/nfo_season.rs crates/library/tests/season_nfo.rs
git commit -m "feat: write season.nfo without replacing tvshow.nfo"
```

---

### Task 2: Frame extraction seeks the URL inside a STRM file

**Files:**
- Modify: `crates/library/src/lib.rs` (`extract_frame_with_ffmpeg`, the `apply_ffmpeg_input` call)
- Test: `crates/library/tests/extract_frame_strm.rs`

**Interfaces:**
- Consumes: `ProbeTarget::from_path`, `ProbeTarget::apply_ffmpeg_input`.
- Produces: same signature `pub fn extract_frame_with_ffmpeg(path: &Path, ms: i64, out: &Path, ffmpeg: &str) -> Result<(), String>`.
- For a `.strm` whose first non-empty line starts with `http://` or `https://`, argv contains `-ss`, then later `-i`, then that URL, then the output path last.
- For any other path, argv contains `-i`, then that path, then the output path last.
- `-ss` stays before `-i`. Do not change `ProbeTarget` in `crates/marker/src/target.rs`; playback already adds its own `-i`.

`extract_frame_with_ffmpeg` today calls `apply_ffmpeg_input` and never inserts `-i` for a local path. The remote arm of `apply_ffmpeg_input_with_seek_ms` does add `-i`, but only when `apply_ffmpeg_input` is used, and the output path is then appended after it. A missing `-i` on the local arm makes ffmpeg treat the path as an output. The remote arm happens to be correct only because `-i` is inside `apply_ffmpeg_input`. The test below locks both.

- [x] **Step 1: Write the failing test**

```rust
use std::fs;
use std::os::unix::fs::PermissionsExt;
use library::extract_frame_with_ffmpeg;

fn fake_ffmpeg(dir: &std::path::Path) -> std::path::PathBuf {
    let program = dir.join("ffmpeg");
    fs::write(
        &program,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$(dirname \"$0\")/argv.txt\"\n: > \"${@: -1}\"\nexit 0\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&program).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&program, permissions).unwrap();
    program
}

#[test]
fn extract_frame_seeks_the_url_inside_a_strm_file() {
    let tmp = tempfile::tempdir().unwrap();
    let ffmpeg = fake_ffmpeg(tmp.path());
    let strm = tmp.path().join("episode.strm");
    fs::write(&strm, "https://cdn.example/ep.mkv\n").unwrap();
    let jpeg = tmp.path().join("episode-thumb.jpg");
    extract_frame_with_ffmpeg(&strm, 60_000, &jpeg, ffmpeg.to_str().unwrap()).unwrap();
    let argv = fs::read_to_string(tmp.path().join("argv.txt")).unwrap();
    let input_at = argv.find("-i\n").expect(&argv);
    let url_at = argv.find("https://cdn.example/ep.mkv").expect(&argv);
    let out_at = argv.find("episode-thumb.jpg").expect(&argv);
    assert!(input_at < url_at && url_at < out_at, "{argv}");
    assert!(argv.contains("-ss\n60"), "{argv}");
    assert!(jpeg.is_file());
}

#[test]
fn extract_frame_passes_a_local_file_after_input_flag() {
    let tmp = tempfile::tempdir().unwrap();
    let ffmpeg = fake_ffmpeg(tmp.path());
    let video = tmp.path().join("episode.mkv");
    fs::write(&video, b"not a real video").unwrap();
    let jpeg = tmp.path().join("episode-thumb.jpg");
    extract_frame_with_ffmpeg(&video, 60_000, &jpeg, ffmpeg.to_str().unwrap()).unwrap();
    let argv = fs::read_to_string(tmp.path().join("argv.txt")).unwrap();
    let input_at = argv.find("-i\n").expect(&argv);
    let file_at = argv.find("episode.mkv").expect(&argv);
    let out_at = argv.find("episode-thumb.jpg").expect(&argv);
    assert!(input_at < file_at && file_at < out_at, "{argv}");
}
```

- [x] **Step 2: Run the test to verify it fails**

Run: `cargo test -p library --test extract_frame_strm -- --nocapture`

Expected: FAIL. The local test fails because `-i` is absent. The strm test may already pass; keep it so the remote URL cannot regress when `-i` is added for local files.

- [x] **Step 3: Write the minimal implementation**

Replace the single `target.apply_ffmpeg_input(&mut cmd);` call:

```rust
    cmd.args(["-y", "-ss", &format!("{:.3}", ms as f64 / 1000.0)]);
    match &target {
        marker::target::ProbeTarget::Local(path) => {
            cmd.arg("-i").arg(path);
        }
        marker::target::ProbeTarget::Remote(_) => {
            target.apply_ffmpeg_input(&mut cmd);
        }
    }
```

`library` already depends on `marker`, and `probe_target.rs` re-exports `ProbeTarget`. Use `crate::probe_target::ProbeTarget` if `marker::target` is not visible. Remote `apply_ffmpeg_input` uses seek `0`, so it does not add a second `-ss`. The `-ss 60` from the line above remains the seek.

- [x] **Step 4: Run the test to verify it passes**

Run: `cargo test -p library --test extract_frame_strm -- --nocapture`

Expected: both tests PASS. Then `cargo test -p library` PASS.

- [x] **Step 5: Commit**

```bash
git add crates/library/src/lib.rs crates/library/tests/extract_frame_strm.rs
git commit -m "fix: frame extraction seeks a strm URL and a local file"
```

---

### Task 3: Episode still file name

**Files:**
- Modify: `crates/api/src/episode_still.rs`
- Test: `#[cfg(test)]` inside that file. The module is `pub(crate)`, so an integration test cannot call it.

**Interfaces:**
- Produces:
  - `pub(crate) fn path(video: &Path) -> PathBuf` → `<parent>/<stem>-thumb.jpg`.
  - `pub(crate) fn existing(video: &Path) -> Option<PathBuf>` returns the first existing file among `path(video)`, `<stem>-still.jpg`, `<stem>.jpg`, `<parent>/still.jpg`.

- [x] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::{existing, path};

    #[test]
    fn path_uses_the_jellyfin_thumb_suffix() {
        let video = std::path::Path::new("/library/show/S01E01.strm");
        assert_eq!(
            path(video),
            std::path::PathBuf::from("/library/show/S01E01-thumb.jpg")
        );
    }

    #[test]
    fn existing_reads_an_emby_jpeg_already_on_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let video = tmp.path().join("S01E01.strm");
        std::fs::write(&video, b"https://cdn.example/a.mkv").unwrap();
        let bare = tmp.path().join("S01E01.jpg");
        std::fs::write(&bare, b"jpeg").unwrap();
        assert_eq!(existing(&video), Some(bare));
    }
}
```

- [x] **Step 2: Run the test to verify it fails**

Run: `cargo test -p api --lib episode_still -- --nocapture`

Expected: first test FAIL (`-still.jpg`). Second test FAIL (`existing` never checks `<stem>.jpg`).

- [x] **Step 3: Write the minimal implementation**

```rust
pub(crate) fn path(video: &Path) -> PathBuf {
    let stem = video
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default();
    video.with_file_name(format!("{stem}-thumb.jpg"))
}

pub(crate) fn existing(video: &Path) -> Option<PathBuf> {
    let stem = video
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default();
    let parent = video.parent()?;
    for candidate in [
        path(video),
        parent.join(format!("{stem}-still.jpg")),
        parent.join(format!("{stem}.jpg")),
        parent.join("still.jpg"),
    ] {
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}
```

- [x] **Step 4: Run the test to verify it passes**

Run: `cargo test -p api --lib episode_still -- --nocapture`

Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add crates/api/src/episode_still.rs
git commit -m "fix: read episode thumbs Jellyfin and Emby already store"
```

---

### Task 4: Parse Fanart.tv and rank one image per slot

**Files:**
- Create: `crates/media/src/fanart.rs`
- Modify: `crates/media/src/lib.rs` (add `pub mod fanart;`)
- Test: `crates/media/tests/fanart.rs`
- Fixture: `crates/media/tests/fixtures/fanart_tv.json`

**Interfaces:**
- Produces:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FanartImage {
    pub url: String,
    pub lang: Option<String>,
    pub likes: u32,
    pub season: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FanartSet {
    pub logo: Option<FanartImage>,
    pub thumb: Option<FanartImage>,
    pub banner: Option<FanartImage>,
    pub season_posters: Vec<FanartImage>,
    pub season_thumbs: Vec<FanartImage>,
    pub season_banners: Vec<FanartImage>,
}

pub fn parse_fanart(body: &str, languages: &[&str]) -> Result<FanartSet, crate::client::TmdbError>;
```

- `languages` is the configured list, already without the automatic `zh` / `en` tail. The function appends `zh` then `en` itself.
- Show-level keys read: `hdtvlogo`, `hdmovielogo`, `movielogo` → `logo`; `tvthumb`, `moviethumb` → `thumb`; `tvbanner`, `moviebanner` → `banner`. The first key that yields a pick wins. `showbackground`, `moviebackground`, `tvposter`, `movieposter` are ignored.
- Season keys `seasonposter`, `seasonthumb`, `seasonbanner`: group by the JSON `season` string. `"0"` becomes `FanartImage.season = Some(0)`. A missing or non-numeric `season` drops that row. Each season contributes at most one image, ranked with the same language rule inside that season.
- Rank: walk `languages`, then `zh`, then `en`. A candidate matches when `lang` equals the wanted tag. Empty `lang` and `"00"` match only when the wanted tag is `""`. Within a matching group, the highest `likes` wins. If no language matches, the highest `likes` overall wins.
- `likes` is a JSON string in the real API (`"24"`). Parse it as a string of digits. A missing or non-numeric `likes` is `0`.
- A body whose top-level `status` is `"error"` returns `FanartSet::default()` and does not return `Err`.

- [x] **Step 1: Write the fixture and the failing test**

`crates/media/tests/fixtures/fanart_tv.json`:

```json
{
  "name": "Spirit Rangers",
  "thetvdb_id": "425039",
  "hdtvlogo": [
    {"id": "1", "url": "https://assets.fanart.tv/logo-en.png", "lang": "en", "likes": "3"},
    {"id": "2", "url": "https://assets.fanart.tv/logo-zh.png", "lang": "zh", "likes": "1"}
  ],
  "tvthumb": [
    {"id": "3", "url": "https://assets.fanart.tv/thumb-en.jpg", "lang": "en", "likes": "9"}
  ],
  "tvbanner": [
    {"id": "4", "url": "https://assets.fanart.tv/banner-en.jpg", "lang": "en", "likes": "2"},
    {"id": "5", "url": "https://assets.fanart.tv/banner-blank.jpg", "lang": "", "likes": "8"}
  ],
  "showbackground": [
    {"id": "6", "url": "https://assets.fanart.tv/background.jpg", "lang": "", "likes": "20"}
  ],
  "seasonposter": [
    {"id": "7", "url": "https://assets.fanart.tv/s1-en.jpg", "lang": "en", "likes": "4", "season": "1"},
    {"id": "8", "url": "https://assets.fanart.tv/s1-zh.jpg", "lang": "zh", "likes": "1", "season": "1"}
  ],
  "seasonthumb": [
    {"id": "9", "url": "https://assets.fanart.tv/s1-thumb.jpg", "lang": "en", "likes": "2", "season": "1"}
  ],
  "seasonbanner": [
    {"id": "10", "url": "https://assets.fanart.tv/specials-banner.jpg", "lang": "en", "likes": "1", "season": "0"}
  ]
}
```

`crates/media/tests/fanart.rs`:

```rust
#[test]
fn fanart_prefers_zh_then_keeps_season_images_and_drops_background() {
    let body = include_str!("fixtures/fanart_tv.json");
    let set = media::fanart::parse_fanart(body, &["zh"]).unwrap();
    assert_eq!(
        set.logo.unwrap().url,
        "https://assets.fanart.tv/logo-zh.png"
    );
    assert_eq!(
        set.thumb.unwrap().url,
        "https://assets.fanart.tv/thumb-en.jpg"
    );
    assert_eq!(
        set.banner.unwrap().url,
        "https://assets.fanart.tv/banner-en.jpg"
    );
    assert_eq!(set.season_posters.len(), 1);
    assert_eq!(set.season_posters[0].season, Some(1));
    assert_eq!(
        set.season_posters[0].url,
        "https://assets.fanart.tv/s1-zh.jpg"
    );
    assert_eq!(set.season_thumbs[0].url, "https://assets.fanart.tv/s1-thumb.jpg");
    assert_eq!(set.season_banners[0].season, Some(0));
}

#[test]
fn fanart_error_status_is_an_empty_set() {
    let set = media::fanart::parse_fanart(r#"{"status":"error"}"#, &[]).unwrap();
    assert_eq!(set, media::fanart::FanartSet::default());
}
```

- [x] **Step 2: Run the test to verify it fails**

Run: `cargo test -p media --test fanart -- --nocapture`

Expected: compile failure, `media::fanart` is not found.

- [x] **Step 3: Write the minimal implementation**

Deserialize with `serde_json::Value` so unknown keys and the stringly `likes` field do not need one struct per image type. Read `body["status"]`. When it equals `"error"`, return the default set. For each show-level key list, collect rows that have a non-empty `url`, rank them, and store the winner in the first slot that is still `None`. For each season key, group rows by `season` before ranking. Log nothing in the parser; the caller logs the HTTP failure.

- [x] **Step 4: Run the test to verify it passes**

Run: `cargo test -p media --test fanart -- --nocapture`

Expected: PASS. `wc -l crates/media/src/fanart.rs` is under 250.

- [x] **Step 5: Commit**

```bash
git add crates/media/src/lib.rs crates/media/src/fanart.rs crates/media/tests/fanart.rs crates/media/tests/fixtures/fanart_tv.json
git commit -m "feat: parse Fanart.tv images and rank them by language"
```

---

### Task 5: Fanart.tv HTTP client and the TVDB id it needs

**Files:**
- Modify: `crates/media/src/fanart.rs` (add `FanartClient`)
- Modify: `crates/media/src/client.rs` (add `tvdb_id`)
- Modify: `crates/media/src/parse.rs` only if `details` drops `external_ids`; prefer reading the id in `client.rs` from the raw body before `parse::details`.
- Test: `crates/media/tests/fanart.rs` (append) and `crates/media/tests/tmdb.rs` if a TV details fixture must grow an `external_ids` object.

**Interfaces:**
- Consumes: `crate::cache::CatalogCache`, `crate::client::CatalogGet`, `crate::client::TmdbError`.
- Produces:

```rust
pub struct FanartClient<H> { /* http, cache, api_key */ }

impl<H: crate::client::CatalogGet> FanartClient<H> {
    pub fn new(http: H, catalog_db: &std::path::Path, api_key: &str) -> Result<Self, crate::client::TmdbError>;
    pub fn tv(&self, tvdb_id: &str, languages: &[&str]) -> Result<FanartSet, crate::client::TmdbError>;
    pub fn movie(&self, tmdb_id: &str, languages: &[&str]) -> Result<FanartSet, crate::client::TmdbError>;
}
```

- URL paths, with the key in the query and never in a log: `https://webservice.fanart.tv/v3/tv/{tvdb_id}?api_key={api_key}` and `https://webservice.fanart.tv/v3/movies/{tmdb_id}?api_key={api_key}`.
- Cache key is the path without the query: `fanart/tv/{tvdb_id}` and `fanart/movie/{tmdb_id}`. A key change must not reuse another key's body. Include a 16-hex prefix of `SHA-256(api_key)` in the cache key (`fanart/tv/{tvdb_id}/{prefix}`), matching MoviePilot's `__key_token`. Use `sha2` if the workspace already depends on it; otherwise use the `md-5` of the key via a tiny local hasher only if `sha2` is already a workspace dependency. Check `Cargo.toml` before adding a crate. If neither crate is present, add `sha2` to `media` with the same version style as the other workspace crates (`sha2.workspace = true` plus the workspace entry). Do not vendor a hasher.
- `Tmdb::tvdb_id(&self, tmdb_id: &str) -> Result<Option<String>, TmdbError>` fetches `/tv/{tmdb_id}/external_ids` through the existing cache and returns `body["tvdb_id"]` as a non-empty string. A null or missing field is `Ok(None)`.

- [x] **Step 1: Write the failing test**

Append to `crates/media/tests/fanart.rs`:

```rust
struct Scripted {
    body: String,
    seen: std::sync::Mutex<Vec<String>>,
}

impl media::CatalogGet for Scripted {
    fn get(&self, path: &str) -> Result<String, media::TmdbError> {
        self.seen.lock().unwrap().push(path.to_string());
        assert!(!path.contains("api_key=secret"), "{path}");
        Ok(self.body.clone())
    }
}

#[test]
fn fanart_client_requests_the_tv_url_without_putting_the_key_in_the_cache_key() {
    let tmp = tempfile::tempdir().unwrap();
    let http = Scripted {
        body: include_str!("fixtures/fanart_tv.json").to_string(),
        seen: std::sync::Mutex::new(Vec::new()),
    };
    let client = media::fanart::FanartClient::new(http, &tmp.path().join("cache.db"), "secret").unwrap();
    let set = client.tv("425039", &["zh"]).unwrap();
    assert!(set.logo.is_some());
}
```

The assertion that the cache key has no `api_key` belongs in the client: `CatalogCache::get_or_fetch` is called with a key that starts with `fanart/tv/425039/` and does not contain `secret`. The `Scripted` impl sees the full URL because `http.get` is the real request. Pass the full URL to `http.get` and the redacted key to `cache.get_or_fetch`. The test as written checks the URL still contains the key (otherwise the request is wrong) and that the word `secret` is not what gets asserted away. Replace the `assert!(!path.contains("api_key=secret"))` with:

```rust
assert!(path.starts_with("https://webservice.fanart.tv/v3/tv/425039?api_key=secret"), "{path}");
```

That is the request. The cache-key assertion lives in a unit test inside `fanart.rs`:

```rust
#[test]
fn cache_key_uses_a_hash_prefix_instead_of_the_raw_key() {
    let key = cache_key("tv", "425039", "secret");
    assert!(key.starts_with("fanart/tv/425039/"));
    assert!(!key.contains("secret"));
    assert_eq!(key, cache_key("tv", "425039", "secret"));
    assert_ne!(key, cache_key("tv", "425039", "other"));
}
```

`cache_key` is `pub(crate)` so the integration test cannot see it. Keep this second test in `crates/media/src/fanart.rs` under `#[cfg(test)]`.

Add a TVDB id test in `crates/media/tests/tmdb.rs` using the existing scripted HTTP pattern in that file:

```rust
#[test]
fn tv_external_ids_return_the_tvdb_id() {
    // HTTP returns {"id":119847,"tvdb_id":425039,"imdb_id":"tt13351446"}
    // for path starting with /tv/119847/external_ids
    // assert client.tvdb_id("119847") == Some("425039")
}
```

Copy the fake `CatalogGet` already used by `movie_details_poster_uses_cached_body` in that file. Do not open a network socket.

- [x] **Step 2: Run the test to verify it fails**

Run: `cargo test -p media --test fanart fanart_client_requests_the_tv_url -- --nocapture`

Expected: compile failure, `FanartClient` is not found.

Run: `cargo test -p media --lib fanart::tests::cache_key -- --nocapture`

Expected: compile failure, `cache_key` is not found.

- [x] **Step 3: Write the minimal implementation**

`FanartClient::tv` builds the URL, calls `self.cache.get_or_fetch` with `cache_key(...)`. The `CatalogGet` passed into `get_or_fetch` must be a wrapper whose `get` ignores the cache key and requests the real URL. Easiest shape: implement `CatalogGet` for a private struct that holds the URL and the inner client, and whose `get(&self, _cache_key: &str)` calls `inner.get(&self.url)`.

`Tmdb::tvdb_id`:

```rust
pub fn tvdb_id(&self, tmdb_id: &str) -> Result<Option<String>, TmdbError> {
    let path = format!("/tv/{tmdb_id}/external_ids");
    self.fetch(&path, |body| {
        let value: serde_json::Value = serde_json::from_str(body)?;
        let id = match value.get("tvdb_id") {
            Some(serde_json::Value::Number(number)) => Some(number.to_string()),
            Some(serde_json::Value::String(text)) if !text.is_empty() => Some(text.clone()),
            _ => None,
        };
        Ok(id)
    })
}
```

Log `warn!` only inside `get_or_fetch`'s existing failure path. Do not add a log that prints the request URL.

- [x] **Step 4: Run the test to verify it passes**

Run: `cargo test -p media --test fanart -- --nocapture && cargo test -p media --lib fanart -- --nocapture && cargo test -p media --test tmdb tv_external_ids -- --nocapture`

Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add crates/media/src/fanart.rs crates/media/src/client.rs crates/media/tests/fanart.rs crates/media/tests/tmdb.rs crates/media/Cargo.toml Cargo.toml
git commit -m "feat: fetch Fanart.tv by tvdb id without caching the api key"
```

Only add `Cargo.toml` if `sha2` was added. Do not stage an unrelated lockfile hunk; if `Cargo.lock` changed only for `sha2`, include it.

---

### Task 6: Fanart API key in scrape settings

**Files:**
- Modify: `crates/api/src/scrape_config.rs`
- Modify: `crates/api/src/http/scrape_settings.rs`
- Modify: `web/lib/api/scrape.ts`
- Modify: `web/components/scrape-settings-section.tsx`
- Test: the scrape settings test that already round-trips `theintrodb_api_key`. Search `crates/api/tests` for `theintrodb_api_key` and extend that test.

**Interfaces:**
- `ScrapeConfigSetting` gains `pub fanart_api_key: Option<String>` and `pub fanart_language: Vec<String>`, both `#[serde(default)]`.
- `EffectiveScrapeConfig` gains `pub fanart_api_key: Option<String>` and `pub fanart_language: Vec<String>`.
- The stored key may remain `None`/blank; effective composition trims it and falls back to the built-in key when absent or whitespace-only (see the implementation linked above). `fanart_language` empty means the effective list is `["zh", "en"]`.
- GET `/settings/scrape` sets `setting.fanart_api_key` to `None` when the caller is not an admin, same as `theintrodb_api_key` at `scrape_settings.rs` line 121. The effective object also omits the raw key for non-admins: expose `fanart_configured: bool` instead of the key in `effective`.
- The settings form puts the key next to the TheIntroDB key, label `Fanart.tv API Key`, hint semantics `电视剧用 TVDB id，电影用 TMDB id。留空使用内置 key，不表示关闭 Fanart。`（原草案的“留空不下载”文案与实际有效配置不符，不再作为要求）

- [x] **Step 1: Write the failing test**

Find the test that PUTs a scrape setting and asserts `theintrodb_api_key` is hidden from a non-admin. Add:

```rust
setting.fanart_api_key = Some("fanart-secret".into());
setting.fanart_language = vec!["zh".into()];
```

Assert the admin GET returns `fanart_api_key == "fanart-secret"` and the member GET returns `fanart_api_key == null` plus `effective.fanart_configured == true`.

- [x] **Step 2: Run the test to verify it fails**

Run: `cargo test -p api --test <that-test-file> -- --nocapture`

Expected: compile failure, unknown field, or JSON missing `fanart_configured`.

- [x] **Step 3: Write the minimal implementation**

Mirror the `theintrodb_api_key` field through `effective_of`. In `scrape_json`, add `"fanart_configured": e.fanart_api_key.is_some()` and do not put the key in `effective`. Blank the setting key for non-admins beside the existing theintrodb blanking.

In `web/lib/api/scrape.ts` add the two fields to `ScrapeSetting`, `EffectiveScrapeConfig` (`fanart_configured: boolean` only), and `SCRAPE_DEFAULTS` (`fanart_api_key: null`, `fanart_language: []`). In the mirror tab of `scrape-settings-section.tsx`, copy the TheIntroDB `<input>` block and bind it to `fanart_api_key`. A language list input is not required in this task; an empty list selects `zh, en`.

- [x] **Step 4: Run the test to verify it passes**

Run the same `cargo test -p api` command. Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add crates/api/src/scrape_config.rs crates/api/src/http/scrape_settings.rs web/lib/api/scrape.ts web/components/scrape-settings-section.tsx crates/api/tests
git commit -m "feat: store a Fanart.tv api key on scrape settings"
```

---

### Task 7: Save Fanart files and season.nfo during scrape

**Files:**
- Create: `crates/api/src/fanart_artwork.rs`
- Modify: `crates/api/src/lib.rs` (`mod fanart_artwork;`)
- Modify: `crates/api/src/scrape_metadata.rs`
- Modify: `crates/api/src/auto_resolve.rs`
- Modify: `crates/api/src/http/library_scan.rs`
- Modify: `crates/api/src/http/reidentify.rs` and `crates/api/src/http/library_organize.rs` at their `write_series_nfos` call sites
- Test: `crates/api/tests/management/auto_resolve_scan.rs`

**Interfaces:**
- Consumes: `library::write_season_nfo`, `media::fanart::FanartSet`, `media::TvSeason`, `episode_still::path`, `PosterFetch::get`, `library::extract_frame`.
- Produces:

```rust
pub(crate) struct SeasonArtwork {
    pub number: u32,
    pub name: Option<String>,
    pub overview: Option<String>,
    pub air_date: Option<String>,
    pub poster_url: Option<String>,
}

pub(crate) fn season_nfo_meta(artwork: &SeasonArtwork) -> library::NfoMeta;

pub(crate) fn write_season_nfos(show_root: &std::path::Path, rows: &[domain::LedgerRow], seasons: &[SeasonArtwork]);

pub(crate) fn mirror_episode_still(
    fetch: &dyn crate::poster_fetch::PosterFetch,
    video: &std::path::Path,
    still_file_path: Option<&str>,
    size: &str,
) -> bool;

pub(crate) struct FanartFiles;

pub(crate) fn save_fanart_files(
    fetch: &dyn crate::poster_fetch::PosterFetch,
    show_root: &std::path::Path,
    rows: &[domain::LedgerRow],
    set: &media::fanart::FanartSet,
) -> usize;
```

- `season_nfo_meta` copies `name` → `title`, `overview` → `plot`, `air_date` → `premiered`, `number` → `season`, `poster_url` → `thumb`. `poster_url` is `https://image.tmdb.org/t/p/w780{poster_path}` when `poster_path` starts with `/`. No season-poster bytes come from TMDB.
- `write_season_nfos` writes one file per `SeasonArtwork` whose `number` appears on `rows`:
  - If every row of that season has the same parent directory, that directory's name starts with `season` (ASCII case-insensitive), and the directory is not `show_root`, write `<that-dir>/season.nfo`.
  - Otherwise write `<show_root>/season.nfo` when the owned season set has length 1, or `<show_root>/seasonNN.nfo` when it has more than one. Season `0` uses `season-specials.nfo`.
- `mirror_episode_still` writes `episode_still::path(video)` only when that file is absent and `still_file_path` starts with `/`. URL is `https://image.tmdb.org/t/p/{size}{still_file_path}`. Empty body or transport error logs `warn!` and returns `false`. An existing file returns `true` without a download.
- `save_fanart_files` downloads each chosen URL once and writes every destination that does not exist. Destinations:
  - `set.logo` → `show_root/logo.png` and `show_root/clearlogo.png`.
  - `set.thumb` → `show_root/thumb.jpg` and `show_root/landscape.jpg`.
  - `set.banner` → `show_root/banner.jpg`.
  - Each season poster/thumb/banner whose `season` is owned → `show_root/seasonNN-poster.jpg` (or `seasonNN-thumb.jpg`, `seasonNN-banner.jpg`). Season `0` uses `season-specials-poster.jpg` and the matching thumb/banner names.
  - When that season's rows share a season directory (same rule as `write_season_nfos`), also write `poster.jpg`, `thumb.jpg`, `landscape.jpg`, and `banner.jpg` inside that directory. Do not write `fanart.jpg` there.
- `save_fanart_files` returns the number of new files. A failed download logs `warn!(%error, path = %target.display(), "Fanart 图片下载失败")` and continues with the next file. The log must not include the URL.
- `write_series_nfos` returns `Vec<(std::path::PathBuf, Option<String>)>`: one entry per owned episode it matched, `(PathBuf::from(&row.path), episode.still_path)`. Callers that ignore the return value still compile.

- [x] **Step 1: Write the failing test**

In `crates/api/tests/management/auto_resolve_scan.rs`, extend the catalog fake used by `auto_resolve_uses_ancestor_alias_when_filename_title_misses`.

`season_details` already returns episode 1. Set `still_path: Some("/still-1.jpg".into())`.

Add:

```rust
fn tv_seasons(&self, _id: &str) -> Result<Vec<media::TvSeason>, String> {
    Ok(vec![media::TvSeason {
        season_number: 1,
        name: "第 1 季".into(),
        episode_count: Some(1),
        air_date: Some("2022-10-10".into()),
        overview: Some("第一季简介".into()),
        poster_path: Some("/season-1.jpg".into()),
    }])
}
```

The test's `PosterFetch` map must answer `https://image.tmdb.org/t/p/w300/still-1.jpg` with `b"still-bytes".to_vec()`. Find the `bodies` map the test already builds and insert that URL.

At the bottom of the existing test:

```rust
let season_nfo = show.join("season.nfo");
let season_body = std::fs::read_to_string(&season_nfo).expect("season.nfo");
assert!(season_body.contains("<seasonnumber>1</seasonnumber>"), "{season_body}");
assert!(season_body.contains("第一季简介"), "{season_body}");
assert!(
    season_body.contains("https://image.tmdb.org/t/p/w780/season-1.jpg"),
    "{season_body}"
);
let still = show.join("Spirit.Rangers.S01E01.1080p-thumb.jpg");
assert_eq!(std::fs::read(&still).unwrap(), b"still-bytes");
```

The episode path in that test is `Spirit.Rangers.S01E01.1080p.strm`, so the stem is `Spirit.Rangers.S01E01.1080p`.

Add a second test in the same file, copying that setup, with a catalog whose `still_path` is `None` and whose `tv_seasons` still returns the overview. Assert `season.nfo` exists and no `*-thumb.jpg` was created. Do not run ffmpeg.

Add a third test that calls `save_fanart_files` only if the function is visible. It is `pub(crate)` in the binary crate, so the test belongs in `crates/api/src/fanart_artwork.rs` under `#[cfg(test)]`:

```rust
#[test]
fn save_fanart_files_writes_logo_banner_and_season_poster_without_touching_fanart_jpg() {
    let tmp = tempfile::tempdir().unwrap();
    let show = tmp.path().join("show");
    let season = show.join("Season 1");
    std::fs::create_dir_all(&season).unwrap();
    std::fs::write(show.join("fanart.jpg"), b"tmdb-backdrop").unwrap();
    let video = season.join("S01E01.strm");
    std::fs::write(&video, b"https://cdn.example/a.mkv").unwrap();
    let row = ledger_row(video.to_str().unwrap(), 1);
    let set = media::fanart::FanartSet {
        logo: Some(image("https://assets.fanart.tv/logo.png")),
        banner: Some(image("https://assets.fanart.tv/banner.jpg")),
        thumb: None,
        season_posters: vec![season_image("https://assets.fanart.tv/s1.jpg", 1)],
        season_thumbs: Vec::new(),
        season_banners: Vec::new(),
    };
    let fetch = MapFetch::new([
        ("https://assets.fanart.tv/logo.png", b"logo".to_vec()),
        ("https://assets.fanart.tv/banner.jpg", b"banner".to_vec()),
        ("https://assets.fanart.tv/s1.jpg", b"season".to_vec()),
    ]);
    let written = save_fanart_files(&fetch, &show, &[row], &set);
    assert!(written >= 4, "{written}");
    assert_eq!(std::fs::read(show.join("logo.png")).unwrap(), b"logo");
    assert_eq!(std::fs::read(show.join("clearlogo.png")).unwrap(), b"logo");
    assert_eq!(std::fs::read(show.join("banner.jpg")).unwrap(), b"banner");
    assert_eq!(std::fs::read(show.join("season01-poster.jpg")).unwrap(), b"season");
    assert_eq!(std::fs::read(season.join("poster.jpg")).unwrap(), b"season");
    assert_eq!(std::fs::read(show.join("fanart.jpg")).unwrap(), b"tmdb-backdrop");
}
```

`ledger_row`, `image`, `season_image`, and `MapFetch` are private helpers in the same test module. `MapFetch` implements `PosterFetch` by returning the mapped bytes or `Err("missing".into())`. `image` sets `url`, `lang: None`, `likes: 0`, `season: None`. `season_image` sets `season: Some(number)`.

- [x] **Step 2: Run the test to verify it fails**

Run: `cargo test -p api --lib fanart_artwork -- --nocapture`

Expected: compile failure.

Run: `cargo test -p api --test auto_resolve_scan auto_resolve_uses_ancestor_alias -- --nocapture`

Expected: FAIL because `season.nfo` or the thumb file is absent.

- [x] **Step 3: Write the minimal implementation**

`auto_resolve` after a successful TV match:

1. Read scrape config. `mirror_nfo` gates NFO writes. `mirror_episode_thumbs` gates stills. `mirror_images` gates Fanart files. Defaults stay `true` when the config read fails.
2. Call `catalog.tv_seasons(tmdb_id)`. On `Err`, log `warn!` and use an empty vec.
3. Map those seasons into `SeasonArtwork`, keeping only season numbers present on the owned rows.
4. `write_series_nfos` writes `tvshow.nfo` and episode NFOs as it does now, then calls `write_season_nfos`.
5. For each `(path, still)` returned, if `mirror_episode_thumbs` is on, call `mirror_episode_still`. When it returns `false` and the library `generate_thumbnails` flag is true (copy the lookup in `poster_fetch.rs` around `generate_thumbnails`), call `library::extract_frame(path, 60_000, &episode_still::path(path))`. Do not skip `.strm`. `warn!` on error and continue.
6. If `mirror_images` is on and `effective.fanart_api_key` is `Some`, resolve a TVDB id: use `media.tvdb_id` if present, otherwise `catalog.tvdb_id(tmdb_id)`. Persist a newly found id with `store.update_media` before the Fanart call. Then `FanartClient::tv(tvdb_id, &effective.fanart_language)` and `save_fanart_files`. The client needs a `CatalogGet`. Construct it as a newtype over `PosterFetch` is wrong: Fanart returns JSON, and `PosterFetch::get` returns bytes, which is fine — wrap `state.poster_fetch` in a `CatalogGet` whose `get` treats the URL as an absolute URL and converts bytes to a `String`. On a non-UTF8 body, return `TmdbError::Parse`.

`FanartClient` lives in `media` and expects `CatalogGet`. The api crate already depends on `media`. Build the client with `media::cache` via `FanartClient::new`. The cache database path is the same catalog cache the TMDB client uses. Search `ApiState` for the catalog db path field; if it is not public, open `state.data_dir.join("fanart.db")` using the directory `Store` already receives. Do not invent a second cache layout inside the TMDB sqlite file.

`library_scan`, `reidentify`, and `library_organize` call the same six steps. Extract them into `pub(crate) fn scrape_tv_sidecars(state: &ApiState, media: &Media, show_root: &Path, rows: &[LedgerRow])` in `scrape_metadata.rs` so the three callers do not each grow a copy. `auto_resolve` calls it too.

`scrape_tv_sidecars` is the only new public-to-the-crate entry. If `scrape_metadata.rs` crosses 400 lines, move `SeasonArtwork`, `season_nfo_meta`, `write_season_nfos`, and `mirror_episode_still` into `crates/api/src/season_sidecar.rs`.

- [x] **Step 4: Run the test to verify it passes**

Run: `cargo test -p api --lib fanart_artwork -- --nocapture`

Run: `cargo test -p api --test auto_resolve_scan -- --nocapture`

Run: `cargo test -p api --test metadata_scrape -- --nocapture`

Expected: PASS. A catalog fake that does not implement a new trait method still compiles because no new `Catalog` method is required: `tv_seasons` already has a default body. `tvdb_id` on the catalog trait does not exist; `scrape_tv_sidecars` calls `media::Tmdb` only through a new optional method. Add this default to `Catalog` so fakes keep compiling:

```rust
fn tvdb_id(&self, _tmdb_id: &str) -> Result<Option<String>, String> {
    Ok(None)
}
```

The TMDB-backed impl calls `self.inner.tvdb_id(tmdb_id)`. `catalog_fanout.rs` forwards it the same way it forwards `tv_seasons`.

- [x] **Step 5: Commit**

```bash
git add crates/api/src/fanart_artwork.rs crates/api/src/lib.rs crates/api/src/scrape_metadata.rs crates/api/src/auto_resolve.rs crates/api/src/http/library_scan.rs crates/api/src/http/reidentify.rs crates/api/src/http/library_organize.rs crates/api/src/catalog.rs crates/api/src/catalog_fanout.rs crates/api/tests/management/auto_resolve_scan.rs
git commit -m "feat: write season.nfo, episode stills, and Fanart.tv images"
```

---

### Task 8: Workspace verification

**Files:** none.

- [x] **Step 1: Run the touched crates and the workspace**

```bash
cargo test -p library
cargo test -p media
cargo test -p api
cargo test --workspace
```

Expected: all pass.

- [x] **Step 2: Check line counts**

```bash
wc -l crates/library/src/nfo.rs crates/library/src/nfo_season.rs crates/library/src/lib.rs \
  crates/media/src/fanart.rs crates/api/src/fanart_artwork.rs crates/api/src/scrape_metadata.rs \
  crates/api/src/poster_fetch.rs crates/api/src/episode_still.rs
```

Expected: `nfo.rs` at or below 800. `poster_fetch.rs` still 392. Every new file under 800. If `scrape_metadata.rs` is over 400, the split in Task 7 step 3 is required before this task ends.

- [x] **Step 3: Commit only if the size split happened in this task**

```bash
git add crates/api/src/scrape_metadata.rs crates/api/src/season_sidecar.rs
git commit -m "refactor: move season sidecar helpers out of scrape_metadata"
```

## Self-review

- Season NFO with plot, premiere, number, and poster URL: Task 1 and Task 7.
- Episode image missing from the local show directory: Task 3 names it `<stem>-thumb.jpg`; Task 7 downloads it on the auto-resolve path that produced that directory. The page currently shows a TMDB URL because `existing()` was empty; after Task 7 the local file wins.
- STRM frame grab: Task 2 fixes `-i`; Task 7 calls `extract_frame` when TMDB has no still.
- Fanart.tv is integrated for both TV and movies. MoviePilot is the field and ranking reference: Task 4 and Task 5.
- Show `logo`/`clearlogo`, `thumb`/`landscape`, `banner`: Task 7 `save_fanart_files`. Movie Fanart files: `scrape_movie_fanart`.
- Season poster, season thumb, season banner, both at the show root (`season01-*`) and inside a season directory (`poster.jpg`, `thumb.jpg`, `banner.jpg`): Task 7.
- Existing `fanart.jpg` is not replaced: the unit test in Task 7 asserts the TMDB bytes survive.
- TVDB id is filled from TMDB `external_ids` before the Fanart TV call: Task 5 and Task 7.
- API key is stored like TheIntroDB and hidden from non-admins: Task 6.
- No banner source other than Fanart. An empty/blank configured key uses the built-in key, not a configured-off mode. Missing provider IDs, unavailable upstream artwork or disabled image mirroring can still leave images absent.
