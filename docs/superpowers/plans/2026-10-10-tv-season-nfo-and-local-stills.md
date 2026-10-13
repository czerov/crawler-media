# TV Season NFO and Local Episode Stills Implementation Plan

> **历史状态（2026-10-13 整理）：** 设计已由后续 [Scrape Artwork/Fanart 计划](<2026-10-10-scrape-artwork-and-fanart.md>) 和 2b76704/3971634/e913904 实现并扩展；本文“暂不下载”只限定原切片，不表示当前缺少 Fanart 图。真实来源图片完整性与播放器显示未在本次验证。
>
> 本文保留原设计、发现/任务 ID 与当时证据；日期本身不表示过时。旧执行/提交/清理命令仅作历史记录，不应直接重跑；复选框不是当前实现清单。当前依据：[数据库/API review](<../../audits/2026-10-13-database-api-review.md>) 与 [剩余计划](<2026-10-13-database-api-followup.md>)，未覆盖的旧 ID 仍须单独对账。

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When a TV show is scraped, write a real `season.nfo` and download each owned episode's TMDB still beside the video as `<stem>-thumb.jpg`, falling back to an ffmpeg frame (including a `.strm` remote URL) when TMDB has no still.

**Architecture:** Season facts already exist on `media::TvSeason` (`name`, `overview`, `air_date`, `poster_path`) from `/tv/{id}`. Episode still file paths already exist on `media::EpisodeMeta.still_path` from `/tv/{id}/season/{n}`. The library NFO writer already accepts a `<season>` root but `write_nfo` always picks `tvshow` for a TV `Media`, so season files need a sibling writer. Episode still bytes are downloaded through the existing `PosterFetch` and written by `episode_still::path`. `library::extract_frame` already resolves a `.strm` to its URL via `ProbeTarget` and seeks ffmpeg there; the scrape path must call it instead of skipping `.strm`.

**Tech Stack:** Rust workspace (`library`, `media`, `api`), `roxmltree` NFO edits, TMDB image CDN `https://image.tmdb.org/t/p/{size}{file_path}`, ffmpeg via `library::extract_frame`.

## Global Constraints

- Vocabulary from `CONTEXT.md`: NFO sidecar, Library ledger, STRM. Do not invent `SearchResult`, `MetaInfo`, or `MediaMeta`.
- Tests first at the public seam. `cargo test` must not hit TMDB, qBittorrent, or a real ffmpeg. Inject `Catalog` and `PosterFetch`; pass a fake ffmpeg binary into `extract_frame_with_ffmpeg`.
- No `unwrap` / `expect` on IO or parse paths except tests.
- Structured logs: `info!` when a season NFO or episode still is written; `warn!` with the error when a download or frame grab fails and the scrape continues.
- Do not add a TV banner (`banner.jpg`). TMDB `/tv/{id}/images` only returns posters, backdrops, and logos. Jellyfin's TMDB provider never fetches banners.
- Do not download season posters, season fanart, show `thumb.jpg`, or `clearlogo.png` in this plan. Those are a follow-up once a source is chosen.
- Do not rewrite an existing still file. Do not delete the Emby-style `<stem>.jpg` copies already on disk.
- `mirror_episode_thumbs` default stays `true`. `mirror_nfo` default stays `true`. `still_size` default stays `w300`.
- File size hard limit is 800 lines. `crates/library/src/nfo.rs` is 792 lines; the season writer goes in a new sibling module, not into `nfo.rs`.
- Count lines with `wc -l` before finishing a crate. Soft limit 200 is a review prompt, not a split requirement.

## Out of scope (reviewed and rejected)

- **Banner.** No TMDB field. Needs TheTVDB or Fanart.tv, which this crate does not call for images.
- **Show landscape `thumb.jpg` and `clearlogo.png`.** TMDB backdrops and logos exist but are not wired as `Images` variants today. Separate change.
- **Season poster / season fanart download.** `TvSeason` only carries `poster_path` (no backdrop). Writing the path into `season.nfo` `<thumb>` is in scope; saving `season01-poster.jpg` is not, because the show-level `poster.jpg` writer would otherwise be the wrong place to grow that logic.
- **Renaming or deleting** the duplicate `<stem>.jpg` and `<stem>-thumb.jpg` pairs already written by Emby.

## File structure

- Modify `crates/library/src/lib.rs`: declare `mod nfo_season;` and re-export `write_season_nfo`.
- Create `crates/library/src/nfo_season.rs`: the only writer that emits `<season>`.
- Create `crates/library/tests/season_nfo.rs`: round-trip of a new season file and refusal to clobber a `tvshow` file.
- Modify `crates/library/src/lib.rs` `extract_frame_with_ffmpeg`: one `-i` before the output path. Behavior covered by a library test with a fake ffmpeg.
- Create `crates/library/tests/extract_frame_strm.rs`: fake ffmpeg records argv; a `.strm` fixture must put the URL after `-i`.
- Modify `crates/api/src/episode_still.rs`: `path()` returns `<stem>-thumb.jpg`. `existing()` checks that name, then `<stem>-still.jpg`, then `<stem>.jpg`, then directory `still.jpg`.
- Modify `crates/api/src/scrape_metadata.rs`: add `SeasonNfo`, `season_nfo_from_tmdb`, and `write_season_nfo_file`. `write_series_nfos` writes one season file per owned season.
- Modify `crates/api/src/http/library_scan.rs`: after the existing TMDB still download, call `extract_frame` when the still file is still missing and `generate_thumbnails` allows it. Delete the `.strm` skip that lives in `poster_fetch.rs` only for the show poster; do not change show-poster behavior in this plan.
- Modify `crates/api/src/auto_resolve.rs`: after `write_series_nfos`, download stills for the rows just written (same helper the scan path uses).
- Test `crates/api/tests/management/auto_resolve_scan.rs`: extend the existing catalog fake so one owned episode gets `<stem>-thumb.jpg` and `season.nfo`.

---

### Task 1: Season NFO writer

**Files:**
- Create: `crates/library/src/nfo_season.rs`
- Modify: `crates/library/src/lib.rs` (module declaration near the other `mod` lines, and the `nfo` re-export)
- Test: `crates/library/tests/season_nfo.rs`

**Interfaces:**
- Consumes: `library::NfoMeta` fields `title`, `plot`, `premiered`, `thumb`, `season`.
- Produces:
  - `pub fn write_season_nfo(path: &std::path::Path, meta: &NfoMeta) -> std::io::Result<()>`
  - Root element is always `season`. A missing file is created. An existing `<season>` file is updated in place using the same scalar rules as `write_nfo` (replace `title`, `plot`, `premiered`, `year`, `seasonnumber`, `thumb` when the new value is non-empty).
  - An existing file whose root is not `season` returns `std::io::ErrorKind::InvalidData` and is left unchanged.
  - `year` is the first four digits of `premiered` when they parse as a number. Empty `premiered` omits both `premiered` and `year`.
  - `seasonnumber` is written only when `meta.season` is `Some`.

- [ ] **Step 1: Write the failing test**

```rust
use std::fs;
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
    let body = fs::read_to_string(&path).unwrap();
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
    fs::write(&path, "<tvshow><title>keep</title></tvshow>").unwrap();
    let error = write_season_nfo(&path, &NfoMeta::default()).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(fs::read_to_string(&path).unwrap().contains("keep"));
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p library --test season_nfo -- --nocapture`

Expected: FAIL compiling, `write_season_nfo` is not in scope.

- [ ] **Step 3: Write the minimal implementation**

Add `mod nfo_season;` to `crates/library/src/lib.rs` and re-export:

```rust
pub use nfo_season::write_season_nfo;
```

`nfo_season.rs` owns the file. It does not call `write_nfo`, because that function selects `tvshow` for `MediaKind::Tv`. Seed a missing file as:

```xml
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<season>
</season>
```

Parse with `roxmltree`. If the root is not `season`, return `InvalidData`. Build the replacement body from the non-empty fields rather than a general XML editor. On update, preserve no other children: a season file this writer owns only has `title`, `plot`, `premiered`, `year`, `seasonnumber`, and `thumb`. Escape `&`, `<`, `>` in text the same way `nfo.rs` does; copy that function into this module only if making it `pub(crate)` would touch the 800-line file. A four-line escape helper here is cheaper than growing `nfo.rs`.

Log `info!` with the path when the file is written. Do not log the plot text.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p library --test season_nfo -- --nocapture`

Expected: PASS. `wc -l crates/library/src/nfo.rs` is still 792. `wc -l crates/library/src/nfo_season.rs` is under 200.

- [ ] **Step 5: Commit**

```bash
git add crates/library/src/lib.rs crates/library/src/nfo_season.rs crates/library/tests/season_nfo.rs
git commit -m "feat: write season.nfo without clobbering tvshow.nfo"
```

---

### Task 2: ffmpeg frame grab already follows a STRM URL

**Files:**
- Modify: `crates/library/src/lib.rs` (`extract_frame_with_ffmpeg`, around the `apply_ffmpeg_input` call)
- Test: `crates/library/tests/extract_frame_strm.rs`

**Interfaces:**
- Consumes: `ProbeTarget::from_path` and `ProbeTarget::apply_ffmpeg_input`.
- Produces: unchanged signature `pub fn extract_frame_with_ffmpeg(path: &Path, ms: i64, out: &Path, ffmpeg: &str) -> Result<(), String>`.
- A `.strm` whose first non-empty line is an `http://` or `https://` URL is passed to ffmpeg as that URL after `-i`. A non-strm path is passed as the path after `-i`. `-ss` stays before `-i`. The jpeg path is the last argument and is not attached to `-i`.

This is the correction to the earlier note that STRM files cannot be framed. `ProbeTarget` already does the URL read. The bug is only that `extract_frame_with_ffmpeg` never inserts `-i`, so a remote URL is currently a trailing argument after the output path and ffmpeg treats it as another output.

- [ ] **Step 1: Write the failing test**

```rust
use std::fs;
use std::os::unix::fs::PermissionsExt;
use library::extract_frame_with_ffmpeg;

fn fake_ffmpeg(dir: &std::path::Path) -> std::path::PathBuf {
    let program = dir.join("ffmpeg");
    fs::write(
        &program,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$(dirname \"$0\")/argv.txt\"\ntouch \"$1\" 2>/dev/null || true\n# last arg is the output; create it\n: > \"${@: -1}\"\nexit 0\n",
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
```

The fake shell's `touch "$1"` is wrong for argv that starts with `-y`. The `: > "${@: -1}"` line is the one that creates the output. Keep the script exactly as written so a missing `-i` fails the position asserts, not the file create.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p library --test extract_frame_strm -- --nocapture`

Expected: FAIL because `-i` is absent, so `argv.find("-i\n")` is `None`.

- [ ] **Step 3: Write the minimal implementation**

In `extract_frame_with_ffmpeg`, replace `target.apply_ffmpeg_input(&mut cmd);` with an input insertion that matches `ProbeTarget::apply_ffmpeg_input` but prefixes `-i` for both arms. Do not edit `ProbeTarget`: playback and fingerprint call `apply_ffmpeg_input` and already add `-i` themselves (`marker/src/target.rs` remote arm ends in `.args(["-i", url])` only inside `apply_ffmpeg_input_with_seek_ms`).

```rust
    cmd.args(["-y", "-ss", &format!("{:.3}", ms as f64 / 1000.0)]);
    match &target {
        library::ProbeTarget::Local(path) => {
            cmd.arg("-i").arg(path);
        }
        library::ProbeTarget::Remote(url) => {
            // Reuse the public applier, which already places `-i <url>`.
            target.apply_ffmpeg_input(&mut cmd);
        }
    }
```

`ProbeTarget` is re-exported from `library`. For `Remote`, `apply_ffmpeg_input` also repeats `-ss` only when `start_ms > 0`, and this call passes `0`, so the earlier `-ss` remains the only seek. For `Local`, do not call `apply_ffmpeg_input`, because that arm does not add `-i`.

If `library::ProbeTarget` is not public at the call site inside `lib.rs`, match on `marker::target::ProbeTarget` via the existing `probe_target` re-export. The crate already depends on `marker`.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p library --test extract_frame_strm -- --nocapture`

Expected: PASS. Then run `cargo test -p library` and confirm no existing probe test failed.

- [ ] **Step 5: Commit**

```bash
git add crates/library/src/lib.rs crates/library/tests/extract_frame_strm.rs
git commit -m "fix: frame extraction seeks the URL stored in a strm file"
```

---

### Task 3: Episode still filename matches Jellyfin

**Files:**
- Modify: `crates/api/src/episode_still.rs`
- Test: `crates/api/tests/episode_still_names.rs` (new integration test; the module is `pub(crate)`, so assert through the scan/auto-resolve behavior in Task 4 if a direct test cannot see it)

`episode_still` is `pub(crate)` inside the `api` binary crate. A `tests/*.rs` crate cannot call it. Put the assertions in `crates/api/src/episode_still.rs` under `#[cfg(test)]` instead. That is the same-module seam for a path helper.

**Interfaces:**
- Produces:
  - `pub(crate) fn path(video: &Path) -> PathBuf` → `<parent>/<stem>-thumb.jpg`.
  - `pub(crate) fn existing(video: &Path) -> Option<PathBuf>` returns the first file that exists, in order: `path(video)`, `<stem>-still.jpg`, `<stem>.jpg`, `<parent>/still.jpg`.

- [ ] **Step 1: Write the failing test**

Append to `episode_still.rs`:

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
    fn existing_reads_an_emby_thumb_or_bare_jpeg_already_on_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let video = tmp.path().join("S01E01.strm");
        std::fs::write(&video, b"https://cdn.example/a.mkv").unwrap();
        let bare = tmp.path().join("S01E01.jpg");
        std::fs::write(&bare, b"jpeg").unwrap();
        assert_eq!(existing(&video), Some(bare));
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p api --lib episode_still -- --nocapture`

Expected: `path_uses_the_jellyfin_thumb_suffix` FAIL (`-still.jpg` actual). The second test also FAIL because `existing` never checks `<stem>.jpg`.

- [ ] **Step 3: Write the minimal implementation**

```rust
pub(crate) fn path(video: &Path) -> PathBuf {
    video.with_file_name(format!(
        "{}-thumb.jpg",
        video.file_stem().and_then(|stem| stem.to_str()).unwrap_or_default()
    ))
}

pub(crate) fn existing(video: &Path) -> Option<PathBuf> {
    let stem = video.file_stem().and_then(|stem| stem.to_str()).unwrap_or_default();
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

Callers of `path()` (`library_scan.rs`, `auto_resolve.rs`, `media_server_provider.rs`) start writing `<stem>-thumb.jpg` without further edits. Callers of `existing()` (`library_artwork.rs`, `playback/episodes.rs`, `library_gallery.rs`) start seeing Emby files that were previously invisible.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p api --lib episode_still -- --nocapture`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/api/src/episode_still.rs
git commit -m "fix: recognize episode thumbs Jellyfin and Emby already store"
```

---

### Task 4: Write season.nfo and download the still during scrape

**Files:**
- Modify: `crates/api/src/scrape_metadata.rs`
- Modify: `crates/api/src/http/library_scan.rs` (the `mirror_episode_thumbs` block near line 387, and `scrape_library_metadata`)
- Modify: `crates/api/src/auto_resolve.rs` (`write_auto_resolved_nfo` after `write_series_nfos`)
- Modify: `crates/api/src/catalog.rs` only if a season-list method is missing on the trait used by `write_series_nfos`. Prefer not to add one: `write_series_nfos` already knows the owned season numbers from `LedgerRow.season`.
- Test: `crates/api/tests/management/auto_resolve_scan.rs` and `crates/api/tests/jellyfin/metadata_scrape.rs`

**Interfaces:**
- Consumes: `library::write_season_nfo`, `episode_still::path`, `episode_still::existing`, `Catalog::season_details_lang`, `Catalog::episode_stills`, `PosterFetch::get`, `library::extract_frame`.
- Produces:
  - `pub(crate) struct SeasonArtwork { pub number: u32, pub name: Option<String>, pub overview: Option<String>, pub air_date: Option<String>, pub poster_url: Option<String> }`
  - `pub(crate) fn season_nfo_meta(artwork: &SeasonArtwork) -> library::NfoMeta`
  - `pub(crate) fn write_owned_season_nfos(show_root: &Path, seasons: &[SeasonArtwork])`
  - `pub(crate) fn mirror_episode_still(fetch: &dyn PosterFetch, video: &Path, still_file_path: Option<&str>, size: &str) -> bool`
    Downloads `https://image.tmdb.org/t/p/{size}{still_file_path}` to `episode_still::path(video)` when that file is absent and `still_file_path` is `Some`. Returns `true` when a new file exists afterwards. Does not call ffmpeg.
  - `write_series_nfos` gains a `seasons: &[SeasonArtwork]` argument. Each owned season (from `rows`) is written to:
    - `<show_root>/<season-dir>/season.nfo` when every row of that season lives in one directory whose name starts with `season` (case-insensitive), or whose parent is `show_root` and the directory is not `show_root` itself.
    - otherwise `<show_root>/season.nfo` only when every owned row is season 1 and there is a single season. If the show root holds more than one season flat, write `<show_root>/season01.nfo`, `<show_root>/season02.nfo`, never a single ambiguous `season.nfo`.
  - The poster URL stored in `<thumb>` is `https://image.tmdb.org/t/p/w780{poster_path}` when `poster_path` starts with `/`. No bytes are downloaded.

Season artwork is supplied by the caller from data it already fetched. `auto_resolve` and `library_scan` both already call `fetch_tmdb_metadata`. They do not currently call `tv_seasons`. Add one catalog method rather than overloading metadata:

- Consumes on the trait: add `fn tv_seasons(&self, tmdb_id: &str) -> Result<Vec<media::TvSeason>, String>` next to `season_details`, defaulting to `Ok(Vec::new())` so existing fakes keep compiling.
- The TMDB catalog impl delegates to `tmdb.tv_seasons(tmdb_id)`.

- [ ] **Step 1: Write the failing test**

In `crates/api/tests/management/auto_resolve_scan.rs`, extend `AliasYearCatalog` (the fake used by `auto_resolve_uses_ancestor_alias_when_filename_title_misses`):

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
    fn episode_stills(&self, _id: &str, season: u32, episode: u32) -> Result<Vec<String>, String> {
        if season == 1 && episode == 1 {
            Ok(vec!["/still-1.jpg".into()])
        } else {
            Ok(Vec::new())
        }
    }
```

`season_details` in that fake already returns episode 1 with `still_path: None`. Change that one field to `Some("/still-1.jpg".into())` so the writer can use the season payload without a second HTTP call. Keep `episode_stills` as the fallback the scan path already uses.

The test fixture's `PosterFetch` is `Fixtures`. Find its `get` implementation in the same file (or the shared test helper it uses) and make `get("https://image.tmdb.org/t/p/w300/still-1.jpg")` return `Ok(b"still-bytes".to_vec())`. If `Fixtures::get` returns a body from a `HashMap`, insert that URL in the test's `bodies` map instead of editing the fake.

At the end of `auto_resolve_uses_ancestor_alias_when_filename_title_misses`:

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

The episode file in that test is `Spirit.Rangers.S01E01.1080p.strm`, so the stem is `Spirit.Rangers.S01E01.1080p`.

Add a second test in the same file: when `episode_stills` and `still_path` are both empty, and `library::extract_frame` cannot run because no ffmpeg exists, auto-resolve still writes `season.nfo` and does not create a zero-byte jpeg. Do not invoke a real ffmpeg. The frame fallback is covered by Task 2; this task only asserts the scrape does not fail closed.

```rust
#[test]
fn auto_resolve_writes_season_nfo_when_the_episode_has_no_still() {
    // same setup as the alias test, but season_details.still_path = None
    // and episode_stills returns Ok(vec![])
    // assert season.nfo exists and no *-thumb.jpg was created
}
```

Copy the setup from `auto_resolve_uses_ancestor_alias_when_filename_title_misses` into this test. A duplicated 40-line setup is better than a shared helper nobody else calls. Swap in a `NoStillCatalog` that is a copy of `AliasYearCatalog` with empty stills and a season overview.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p api --test auto_resolve_scan auto_resolve_uses_ancestor_alias -- --nocapture`

Expected: FAIL because `season.nfo` does not exist, or the new trait method fails to compile until the default impl exists. Add the default `tv_seasons` first only if the compile error is the missing method; the assertion failure is the red test that matters.

- [ ] **Step 3: Write the minimal implementation**

`season_nfo_meta`:

```rust
pub(crate) fn season_nfo_meta(artwork: &SeasonArtwork) -> library::NfoMeta {
    let year = artwork.air_date.as_deref().and_then(|date| {
        let year = date.get(0..4)?;
        year.parse::<u16>().ok()?;
        Some(year.to_string())
    });
    library::NfoMeta {
        title: artwork.name.clone(),
        plot: artwork.overview.clone(),
        premiered: artwork.air_date.clone(),
        year,
        season: Some(artwork.number),
        thumb: artwork.poster_url.clone(),
        ..library::NfoMeta::default()
    }
}
```

`NfoMeta.year` is `Option<String>` (`crates/library/src/nfo.rs`). `write_season_nfo` reads `premiered` for the `<year>` element (Task 1). Passing `year` on `NfoMeta` is unused by that writer; do not also invent a second year field. Delete the `year` local above if Task 1 derives `<year>` only from `premiered`. Keep `premiered` set.

`write_series_nfos` after the existing episode loop:

```rust
    for season in seasons {
        let dir = season_nfo_dir(show_root, rows, season.number);
        let path = dir.join(season_nfo_name(show_root, rows, season.number));
        if let Err(error) = library::write_season_nfo(&path, &season_nfo_meta(season)) {
            tracing::warn!(%error, path = %path.display(), season = season.number, "写入 season.nfo 失败");
        } else {
            tracing::info!(path = %path.display(), season = season.number, "已写入 season.nfo");
        }
    }
```

`season_nfo_name` returns `season.nfo` when `dir != show_root` or when the owned seasons set has length 1. Otherwise it returns `format!("season{number:02}.nfo")`.

`mirror_episode_still` writes only when `episode_still::path(video)` is not already a file:

```rust
pub(crate) fn mirror_episode_still(
    fetch: &dyn crate::poster_fetch::PosterFetch,
    video: &std::path::Path,
    still_file_path: Option<&str>,
    size: &str,
) -> bool {
    let target = crate::episode_still::path(video);
    if target.is_file() {
        return true;
    }
    let Some(file_path) = still_file_path.filter(|path| path.starts_with('/')) else {
        return false;
    };
    let url = format!("https://image.tmdb.org/t/p/{size}{file_path}");
    match fetch.get(&url) {
        Ok(bytes) if !bytes.is_empty() => std::fs::write(&target, bytes).is_ok(),
        Ok(_) => {
            tracing::warn!(url, path = %target.display(), "分集剧照响应为空");
            false
        }
        Err(error) => {
            tracing::warn!(%error, url, path = %target.display(), "分集剧照下载失败");
            false
        }
    }
}
```

`PosterFetch::get` returns `Result<Vec<u8>, String>` (see `poster_fetch.rs` call sites `state.poster_fetch.get(&url)`). Use that exact type.

In `library_scan.rs`, the existing per-episode block already calls `episode_stills` and writes `episode_still::path`. After Task 3 that path is `<stem>-thumb.jpg`. Change it to call `mirror_episode_still` so the empty-body and log behavior stay in one function. When `mirror_episode_still` returns `false` and the library's `generate_thumbnails` is true (same lookup `poster_fetch.rs` uses around the `is_strm` check), call `library::extract_frame(video, 60_000, &episode_still::path(video))`. Do not skip `.strm`. Log `warn!` on `Err` and continue the episode loop.

In `auto_resolve.rs`, `write_series_nfos` does not download images. After it returns, iterate `rows_to_write` and call `mirror_episode_still` with `still_path` from the `EpisodeMeta` that `write_series_nfos` already resolved. To avoid a second season fetch, change `write_series_nfos` to return the `(LedgerRow path, Option<still file path>)` pairs it matched, and let the caller download. Signature:

```rust
pub(crate) fn write_series_nfos(...) -> Vec<(std::path::PathBuf, Option<String>)>
```

Each entry is `(row.path, episode.still_path)`. Rows that do not match an episode are omitted. Existing callers that ignore the return value keep compiling.

`auto_resolve` then:

```rust
let mirrored = crate::scrape_metadata::write_series_nfos(...);
if mirror_episode_thumbs {
    for (path, still) in mirrored {
        crate::scrape_metadata::mirror_episode_still(
            state.poster_fetch.as_ref(),
            std::path::Path::new(&path),
            still.as_deref(),
            &still_size,
        );
    }
}
```

`still_size` and `mirror_episode_thumbs` come from `get_scrape_config().effective`, defaulting to `"w300"` and `true` when the config read fails. That matches `library_scan.rs`.

Load `TvSeason` in both callers:

```rust
let seasons = state.catalog.tv_seasons(tmdb_id).unwrap_or_else(|error| {
    tracing::warn!(%error, tmdb_id, "读取 TMDB 季列表失败");
    Vec::new()
});
```

Map `media::TvSeason` into `SeasonArtwork` with `poster_url = poster_path.map(|path| format!("https://image.tmdb.org/t/p/w780{path}"))`. Filter to season numbers present on `rows`.

`write_series_nfos` callers to update: `auto_resolve.rs`, `library_scan.rs` `scrape_library_metadata`, `http/reidentify.rs`, `http/library_organize.rs`. The last two pass `&[]` for seasons only if they do not have a catalog season list yet; they should call `catalog.tv_seasons` the same way so a reidentify also refreshes `season.nfo`. Do not leave them on `&[]`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p api --test auto_resolve_scan -- --nocapture`

Expected: PASS, including the new no-still test.

Run: `cargo test -p api --test metadata_scrape -- --nocapture`

Expected: PASS. If a fake `Catalog` in that file no longer compiles, add `fn tv_seasons(&self, _: &str) -> Result<Vec<media::TvSeason>, String> { Ok(Vec::new()) }` to that fake only. The trait default should already cover fakes that do not override it; a compile error means the method was not given a default body.

- [ ] **Step 5: Commit**

```bash
git add crates/api/src/scrape_metadata.rs crates/api/src/http/library_scan.rs crates/api/src/auto_resolve.rs crates/api/src/catalog.rs crates/api/src/catalog_fanout.rs crates/api/src/http/reidentify.rs crates/api/src/http/library_organize.rs crates/api/tests/management/auto_resolve_scan.rs crates/api/tests/jellyfin/metadata_scrape.rs
git commit -m "feat: write season.nfo and mirror episode stills beside the video"
```

---

### Task 5: Workspace verification

**Files:** none new.

- [ ] **Step 1: Run the touched crates, then the workspace**

```bash
cargo test -p library
cargo test -p media
cargo test -p api
cargo test --workspace
```

Expected: all pass. `media` did not change behavior; the run is the workspace rule, not a logic check.

- [ ] **Step 2: Check file size**

```bash
wc -l crates/library/src/nfo.rs crates/library/src/nfo_season.rs crates/library/src/lib.rs crates/api/src/scrape_metadata.rs crates/api/src/episode_still.rs crates/api/src/auto_resolve.rs
```

Expected: `nfo.rs` unchanged at 792. Every other touched production file is under 800. `scrape_metadata.rs` started at 184; if the season helper pushes it past 400, move `SeasonArtwork`, `season_nfo_meta`, and `mirror_episode_still` into `crates/api/src/season_sidecar.rs` and `pub(crate) use` them from `scrape_metadata.rs`. Do that only if the file crosses 400, so the split has a reason.

- [ ] **Step 3: No commit unless a size split was required**

If Step 2 forced a move, commit that move alone:

```bash
git commit -m "refactor: split season sidecar helpers out of scrape_metadata"
```

## Self-review

Spec coverage:

- User asked why `/Users/jianxiong.cao/Downloads/crawler-media-test/.../动灵守护者 (2022)` has no episode images. Task 4 writes them on the auto-resolve path that produced that directory's NFO. The on-disk URL in `<thumb>` stays; the new file is `<stem>-thumb.jpg`.
- User asked for banner, thumb, season poster, season fanart. This plan explicitly does not download those. Banner has no TMDB source. The other three need an image-type extension that is a different plan.
- User corrected the STRM frame-grab claim. Task 2 fixes the missing `-i` so `extract_frame` seeks the URL. Task 4 calls it when TMDB has no still.
- Page images today come from `playback/episodes.rs` substituting `tmdb_still` (`https://image.tmdb.org/t/p/w300...`) when `existing()` is `None`. After Task 3 and Task 4, `existing()` finds the local file and the page uses `/stills/{ledger_id}`.

Placeholder scan: no TBD. Trait method, file names, and URL shape are fixed above.

Type consistency: `write_season_nfo(&Path, &NfoMeta)`, `SeasonArtwork`, `mirror_episode_still`, `write_series_nfos -> Vec<(PathBuf, Option<String>)>`, `tv_seasons -> Result<Vec<TvSeason>, String>` are the same names in every task.
