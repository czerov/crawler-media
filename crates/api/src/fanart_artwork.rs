use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

use domain::LedgerRow;
use media::fanart::FanartSet;

pub(crate) struct SeasonArtwork {
    pub number: u32,
    pub name: Option<String>,
    pub overview: Option<String>,
    pub air_date: Option<String>,
    pub poster_url: Option<String>,
}

pub(crate) fn season_nfo_meta(artwork: &SeasonArtwork) -> library::NfoMeta {
    library::NfoMeta {
        title: artwork.name.clone(),
        plot: artwork.overview.clone(),
        premiered: artwork.air_date.clone(),
        thumb: artwork.poster_url.clone(),
        season: Some(artwork.number),
        ..Default::default()
    }
}

fn season_dir(show_root: &Path, rows: &[LedgerRow], season_number: u32) -> Option<PathBuf> {
    let season_rows: Vec<&LedgerRow> = rows
        .iter()
        .filter(|r| r.season == Some(season_number))
        .collect();
    if season_rows.is_empty() {
        return None;
    }
    let first_parent = Path::new(&season_rows[0].path).parent()?;
    if first_parent == show_root {
        return None;
    }
    let dir_name = first_parent
        .file_name()?
        .to_str()?
        .to_ascii_lowercase();
    if !dir_name.starts_with("season") {
        return None;
    }
    for row in &season_rows[1..] {
        if Path::new(&row.path).parent()? != first_parent {
            return None;
        }
    }
    Some(first_parent.to_path_buf())
}

pub(crate) fn write_season_nfos(
    show_root: &Path,
    rows: &[LedgerRow],
    seasons: &[SeasonArtwork],
) {
    let owned_seasons: HashSet<u32> = rows
        .iter()
        .filter_map(|r| r.season)
        .collect();

    let target_seasons: Vec<&SeasonArtwork> = seasons
        .iter()
        .filter(|s| owned_seasons.contains(&s.number))
        .collect();

    for artwork in &target_seasons {
        let meta = season_nfo_meta(artwork);
        let path = if let Some(s_dir) = season_dir(show_root, rows, artwork.number) {
            s_dir.join("season.nfo")
        } else if owned_seasons.len() <= 1 {
            show_root.join("season.nfo")
        } else if artwork.number == 0 {
            show_root.join("season-specials.nfo")
        } else {
            show_root.join(format!("season{:02}.nfo", artwork.number))
        };

        if let Err(error) = library::write_season_nfo(&path, &meta) {
            warn!(%error, path = %path.display(), "写入 season.nfo 失败");
        }
    }
}

pub(crate) fn mirror_episode_still(
    fetch: &dyn crate::poster_fetch::PosterFetch,
    video: &Path,
    still_file_path: Option<&str>,
    size: &str,
) -> bool {
    if !crate::scrape_metadata::targets::video_present(video) {
        return false;
    }
    let still_dest = crate::episode_still::path(video);
    if crate::episode_still::existing(video).is_some() {
        return true;
    }
    let Some(file_path) = still_file_path else {
        return false;
    };
    if !file_path.starts_with('/') {
        return false;
    }

    let url = format!("https://image.tmdb.org/t/p/{size}{file_path}");
    match fetch.get(&url) {
        Ok(bytes) if !bytes.is_empty() => {
            // Re-check after fetch in case the directory/file was renamed while downloading
            if !crate::scrape_metadata::targets::video_present(video) {
                return false;
            }
            if let Err(error) = fs::write(&still_dest, &bytes) {
                if error.kind() == std::io::ErrorKind::NotFound {
                    tracing::debug!(path = %still_dest.display(), "目标目录已不存在，跳过分集剧照保存");
                } else {
                    warn!(%error, path = %still_dest.display(), "保存分集剧照失败");
                }
                false
            } else {
                info!(path = %still_dest.display(), "已下载分集剧照");
                true
            }
        }
        Ok(_) => {
            warn!(path = %still_dest.display(), "分集剧照响应为空");
            false
        }
        Err(error) => {
            warn!(%error, path = %still_dest.display(), "下载分集剧照失败");
            false
        }
    }
}

fn write_destination_if_missing(
    fetch: &dyn crate::poster_fetch::PosterFetch,
    url: &str,
    cached_bytes: &mut Option<Vec<u8>>,
    dest: &Path,
    written_count: &mut usize,
    source_rows: &[LedgerRow],
) {
    if dest.is_file() {
        return;
    }
    let live_source = source_rows.iter().any(|row| {
        crate::scrape_metadata::targets::video_present(Path::new(&row.path))
    });
    if !live_source {
        return;
    }

    if cached_bytes.is_none() {
        match fetch.get(url) {
            Ok(bytes) if !bytes.is_empty() => {
                *cached_bytes = Some(bytes);
            }
            Ok(_) => {
                warn!(path = %dest.display(), "Fanart 图片响应为空");
                return;
            }
            Err(error) => {
                warn!(%error, path = %dest.display(), "Fanart 图片下载失败");
                return;
            }
        }
    }

    let live_source_after = source_rows.iter().any(|row| {
        crate::scrape_metadata::targets::video_present(Path::new(&row.path))
    });
    if !live_source_after {
        return;
    }

    if let Some(bytes) = cached_bytes.as_deref() {
        if let Err(error) = fs::write(dest, bytes) {
            if error.kind() == std::io::ErrorKind::NotFound {
                tracing::debug!(path = %dest.display(), "目标目录不存在，跳过 Fanart 图片保存");
            } else {
                warn!(%error, path = %dest.display(), "写入 Fanart 图片失败");
            }
        } else {
            info!(path = %dest.display(), "已保存 Fanart 图片");
            *written_count += 1;
        }
    }
}

fn season_prefix(season: u32) -> String {
    if season == 0 {
        "season-specials-".to_string()
    } else {
        format!("season{:02}-", season)
    }
}

pub(crate) fn save_fanart_files(
    fetch: &dyn crate::poster_fetch::PosterFetch,
    show_root: &Path,
    rows: &[LedgerRow],
    set: &FanartSet,
) -> usize {
    let mut written = 0;

    // Show-level logo
    if let Some(ref logo) = set.logo {
        let mut bytes = None;
        write_destination_if_missing(fetch, &logo.url, &mut bytes, &show_root.join("logo.png"), &mut written, rows);
        write_destination_if_missing(fetch, &logo.url, &mut bytes, &show_root.join("clearlogo.png"), &mut written, rows);
    }

    // Show-level thumb
    if let Some(ref thumb) = set.thumb {
        let mut bytes = None;
        write_destination_if_missing(fetch, &thumb.url, &mut bytes, &show_root.join("thumb.jpg"), &mut written, rows);
        write_destination_if_missing(fetch, &thumb.url, &mut bytes, &show_root.join("landscape.jpg"), &mut written, rows);
    }

    // Show-level banner
    if let Some(ref banner) = set.banner {
        let mut bytes = None;
        write_destination_if_missing(fetch, &banner.url, &mut bytes, &show_root.join("banner.jpg"), &mut written, rows);
    }

    let owned_seasons: HashSet<u32> = rows
        .iter()
        .filter(|r| crate::scrape_metadata::targets::video_present(Path::new(&r.path)))
        .filter_map(|r| r.season)
        .collect();

    // Season posters
    for sp in &set.season_posters {
        let Some(s_num) = sp.season else { continue };
        if !owned_seasons.contains(&s_num) { continue };
        let prefix = season_prefix(s_num);
        let mut bytes = None;
        write_destination_if_missing(fetch, &sp.url, &mut bytes, &show_root.join(format!("{prefix}poster.jpg")), &mut written, rows);
        if let Some(s_dir) = season_dir(show_root, rows, s_num) {
            write_destination_if_missing(fetch, &sp.url, &mut bytes, &s_dir.join("poster.jpg"), &mut written, rows);
        }
    }

    // Season thumbs
    for st in &set.season_thumbs {
        let Some(s_num) = st.season else { continue };
        if !owned_seasons.contains(&s_num) { continue };
        let prefix = season_prefix(s_num);
        let mut bytes = None;
        write_destination_if_missing(fetch, &st.url, &mut bytes, &show_root.join(format!("{prefix}thumb.jpg")), &mut written, rows);
        if let Some(s_dir) = season_dir(show_root, rows, s_num) {
            write_destination_if_missing(fetch, &st.url, &mut bytes, &s_dir.join("thumb.jpg"), &mut written, rows);
            write_destination_if_missing(fetch, &st.url, &mut bytes, &s_dir.join("landscape.jpg"), &mut written, rows);
        }
    }

    // Season banners
    for sb in &set.season_banners {
        let Some(s_num) = sb.season else { continue };
        if !owned_seasons.contains(&s_num) { continue };
        let prefix = season_prefix(s_num);
        let mut bytes = None;
        write_destination_if_missing(fetch, &sb.url, &mut bytes, &show_root.join(format!("{prefix}banner.jpg")), &mut written, rows);
        if let Some(s_dir) = season_dir(show_root, rows, s_num) {
            write_destination_if_missing(fetch, &sb.url, &mut bytes, &s_dir.join("banner.jpg"), &mut written, rows);
        }
    }

    written
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use media::fanart::FanartImage;

    struct MapFetch {
        bodies: HashMap<String, Vec<u8>>,
    }

    impl MapFetch {
        fn new<const N: usize>(items: [(&str, Vec<u8>); N]) -> Self {
            Self {
                bodies: items.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
            }
        }
    }

    impl crate::poster_fetch::PosterFetch for MapFetch {
        fn get(&self, url: &str) -> Result<Vec<u8>, String> {
            self.bodies
                .get(url)
                .cloned()
                .ok_or_else(|| format!("missing {url}"))
        }
    }

    pub(super) fn ledger_row(path: &str, season: u32) -> domain::LedgerRow {
        domain::LedgerRow {
            id: domain::LedgerId::new(),
            media_id: domain::MediaId::new(),
            path: path.to_string(),
            season: Some(season),
            episode: Some(1),
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: domain::QualitySource::Release,
            confidence: domain::Confidence::High,
            filter_score: None,
        }
    }

    pub(super) fn image(url: &str) -> FanartImage {
        FanartImage {
            url: url.to_string(),
            lang: None,
            likes: 0,
            season: None,
        }
    }

    fn season_image(url: &str, season: u32) -> FanartImage {
        FanartImage {
            url: url.to_string(),
            lang: None,
            likes: 0,
            season: Some(season),
        }
    }

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
        let set = FanartSet {
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
}

#[cfg(test)]
mod rename_tests;
