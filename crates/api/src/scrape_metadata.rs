//! Shared TMDB → NFO mapping for every metadata scrape entry point.

use crate::catalog::Catalog;
use crate::scrape_store::ScrapeStoreExt;
use domain::{LedgerRow, Media};
use std::path::{Path, PathBuf};

pub(crate) mod targets;

pub(crate) fn fetch_tmdb_metadata(
    state: &crate::management::ApiState,
    kind: domain::MediaKind,
    tmdb_id: &str,
) -> Result<Option<media::ItemMeta>, String> {
    let preferences = state
        .store
        .lock()
        .get_scrape_config()
        .ok()
        .map(|config| {
            (
                config.effective.language_priority,
                config.effective.cert_country_priority,
            )
        })
        .unwrap_or_else(|| {
            (
                crate::scrape_config::default_language_priority(),
                crate::scrape_config::default_cert_country_priority(),
            )
        });
    state
        .catalog
        .metadata_with_preferences(kind, tmdb_id, &preferences.0, &preferences.1)
}

pub(crate) fn preferred_language(state: &crate::management::ApiState) -> String {
    state
        .store
        .lock()
        .get_scrape_config()
        .ok()
        .map(|config| config.effective.primary_language().to_string())
        .unwrap_or_else(|| "zh-CN".into())
}

/// NFO sidecars to read for one ledger row, most specific first. TV keeps the
/// show-level `tvshow.nfo`; movies may carry either `<file>.nfo` or `movie.nfo`.
/// Shared so browse and wall never disagree about where metadata lives.
pub(crate) fn nfo_candidates(
    path: &Path,
    row: &LedgerRow,
    kind: domain::MediaKind,
) -> Vec<PathBuf> {
    match kind {
        domain::MediaKind::Tv => vec![show_root(path, row).join("tvshow.nfo")],
        _ => {
            let directory = path.parent().unwrap_or(path);
            vec![path.with_extension("nfo"), directory.join("movie.nfo")]
        }
    }
}

pub(crate) fn show_root(path: &Path, row: &LedgerRow) -> PathBuf {
    let directory = path.parent().unwrap_or(path);
    let dir_name = directory
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.to_ascii_lowercase())
        .unwrap_or_default();
    let is_season_directory = dir_name.starts_with("season")
        || (dir_name.contains(".s0") || dir_name.contains(".s1") || dir_name.contains(".s2"));
    if row.season.is_some() && is_season_directory {
        directory.parent().unwrap_or(directory).to_path_buf()
    } else {
        directory.to_path_buf()
    }
}

pub(crate) fn write_nfo(path: &Path, media: &Media, metadata: &library::NfoMeta) -> bool {
    match library::write_nfo(path, media, Some(metadata)) {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "failed to write scraped NFO metadata");
            false
        }
    }
}

pub(crate) fn write_series_nfos(
    catalog: &dyn Catalog,
    media: &Media,
    tmdb_id: &str,
    show_root: &Path,
    rows: &[LedgerRow],
    show_metadata: &library::NfoMeta,
    language: &str,
) -> Vec<(PathBuf, Option<String>)> {
    write_nfo(&show_root.join("tvshow.nfo"), media, show_metadata);
    let mut matched_episodes = Vec::new();
    let seasons = rows
        .iter()
        .filter_map(|row| row.season)
        .collect::<std::collections::BTreeSet<_>>();
    for season in seasons {
        let episodes = match catalog.season_details_lang(tmdb_id, season, language) {
            Ok(episodes) if !episodes.is_empty() => episodes,
            Ok(_) => match catalog.season_details(tmdb_id, season) {
                Ok(episodes) => episodes,
                Err(error) => {
                    tracing::warn!(%error, tmdb_id, season, "failed to scrape TV season episode metadata");
                    continue;
                }
            },
            Err(error) => {
                tracing::warn!(%error, tmdb_id, season, "failed to scrape TV season episode metadata");
                continue;
            }
        };
        for row in rows.iter().filter(|row| row.season == Some(season)) {
            let Some(episode_number) = row.episode else {
                continue;
            };
            if !targets::video_present(Path::new(&row.path)) {
                continue;
            }
            let Some(episode) = episodes
                .iter()
                .find(|episode| episode.episode_number == episode_number)
                .cloned()
            else {
                continue;
            };
            let path = Path::new(&row.path).with_extension("nfo");
            let mut still_path = episode.still_path.clone();
            if still_path.is_none() {
                if let Ok(stills) = catalog.episode_stills(tmdb_id, season, episode_number) {
                    still_path = stills.into_iter().next();
                }
            }
            write_nfo(&path, media, &nfo_from_episode(row, episode));
            matched_episodes.push((PathBuf::from(&row.path), still_path));
        }
    }
    matched_episodes
}

pub(crate) fn nfo_from_tmdb(media: &Media, metadata: &media::ItemMeta) -> library::NfoMeta {
    library::NfoMeta {
        original_title: media.original_title.clone(),
        plot: metadata.overview.clone(),
        rating: metadata.rating.clone(),
        runtime_minutes: metadata.runtime_minutes.clone(),
        tagline: metadata.tagline.clone(),
        premiered: metadata.release_date.clone(),
        end_date: metadata.last_air_date.clone(),
        content_rating: metadata.content_rating.clone(),
        vote_count: metadata.vote_count,
        original_language: metadata.original_language.clone(),
        countries: metadata.origin_countries.clone(),
        studios: metadata.studios.clone(),
        status: metadata.status.clone(),
        number_of_seasons: metadata.number_of_seasons,
        number_of_episodes: metadata.number_of_episodes,
        directors: metadata.directors.clone(),
        creators: metadata.creators.clone(),
        genres: metadata.genres.clone(),
        cast: metadata
            .cast
            .iter()
            .map(|member| library::CastMember {
                name: member.name.clone(),
                role: member.role.clone(),
                tmdb_id: member.person_id.map(|id| id.to_string()),
                thumb: member.avatar_path.clone(),
                order: Some(member.order),
            })
            .collect(),
        ..Default::default()
    }
}

pub(crate) fn nfo_from_episode(row: &LedgerRow, episode: media::EpisodeMeta) -> library::NfoMeta {
    library::NfoMeta {
        title: episode.name,
        plot: episode.overview,
        runtime_minutes: episode.runtime_minutes.map(|value| value.to_string()),
        rating: episode.rating,
        vote_count: episode.vote_count,
        aired: episode.air_date,
        season: row.season.map(|value| value as u32),
        episode: row.episode,
        thumb: episode
            .still_path
            .map(|path| format!("https://image.tmdb.org/t/p/w300{path}")),
        ..Default::default()
    }
}

struct PosterFetchCatalogGet<'a> {
    fetch: &'a dyn crate::poster_fetch::PosterFetch,
}

impl media::CatalogGet for PosterFetchCatalogGet<'_> {
    fn get(&self, path: &str) -> Result<String, media::TmdbError> {
        let bytes = self
            .fetch
            .get(path)
            .map_err(media::TmdbError::Http)?;
        String::from_utf8(bytes).map_err(|e| media::TmdbError::Parse(e.to_string()))
    }
}

pub(crate) fn scrape_tv_sidecars(
    state: &crate::management::ApiState,
    media: &Media,
    _show_root: &Path,
    rows: &[LedgerRow],
) {
    let Some(tmdb_id) = media.tmdb_id.as_deref() else {
        return;
    };
    let groups = targets::live_groups(rows);
    if groups.is_empty() {
        tracing::debug!(media = %media.title, "没有可用的视频文件，跳过电视剧刮削");
        return;
    }
    tracing::info!(media = %media.title, "开始电视剧刮削与附属文件处理");
    let scrape_config = state.store.lock().get_scrape_config().ok();
    let (mirror_nfo, mirror_thumbs, mirror_images, still_size, fanart_api_key, fanart_lang) =
        match scrape_config {
            Some(ref c) => (
                c.effective.mirror_nfo,
                c.effective.mirror_episode_thumbs,
                c.effective.mirror_images,
                c.effective.still_size.clone(),
                c.effective.fanart_api_key.clone(),
                c.effective.fanart_language.clone(),
            ),
            None => (
                true,
                true,
                true,
                "w300".to_string(),
                Some(crate::scrape_config::DEFAULT_FANART_API_KEY.to_string()),
                vec!["zh".into(), "en".into()],
            ),
        };

    for (effective_root, group_rows) in groups {
        let matched_episodes = scrape_tv_episodes_and_nfos(
            state,
            media,
            tmdb_id,
            &effective_root,
            &group_rows,
            mirror_nfo,
        );

        if mirror_thumbs {
            scrape_tv_stills(state, matched_episodes, &still_size);
        }

        if mirror_images {
            if let Some(ref api_key) = fanart_api_key {
                scrape_tv_fanart(state, media, &effective_root, &group_rows, api_key, &fanart_lang);
            }
        }
    }
    tracing::info!(media = %media.title, "完成电视剧刮削与附属文件处理");
}

fn scrape_tv_episodes_and_nfos(
    state: &crate::management::ApiState,
    media: &Media,
    tmdb_id: &str,
    show_root: &Path,
    rows: &[LedgerRow],
    mirror_nfo: bool,
) -> Vec<(PathBuf, Option<String>)> {
    let tmdb_meta = fetch_tmdb_metadata(state, media.kind, tmdb_id).ok().flatten();
    let show_nfo = tmdb_meta.as_ref().map(|meta| nfo_from_tmdb(media, meta)).unwrap_or_default();
    let preferred_lang = preferred_language(state);

    let tv_seasons = match state.catalog.tv_seasons(tmdb_id) {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!(tmdb_id, error = %err, "获取 TV 季列表失败");
            Vec::new()
        }
    };

    let owned_seasons: std::collections::HashSet<u32> = rows.iter().filter_map(|r| r.season).collect();
    let season_artworks: Vec<crate::fanart_artwork::SeasonArtwork> = tv_seasons
        .into_iter()
        .filter(|s| owned_seasons.contains(&(s.season_number as u32)))
        .map(|s| crate::fanart_artwork::SeasonArtwork {
            number: s.season_number as u32,
            name: Some(s.name),
            overview: s.overview,
            air_date: s.air_date,
            poster_url: s
                .poster_path
                .as_deref()
                .filter(|p| p.starts_with('/'))
                .map(|p| format!("https://image.tmdb.org/t/p/w780{p}")),
        })
        .collect();

    if mirror_nfo {
        let mut ep_matches = write_series_nfos(
            state.catalog.as_ref(),
            media,
            tmdb_id,
            show_root,
            rows,
            &show_nfo,
            &preferred_lang,
        );
        crate::fanart_artwork::write_season_nfos(show_root, rows, &season_artworks);
        for row in rows {
            let row_path = PathBuf::from(&row.path);
            if !ep_matches.iter().any(|(p, _)| p == &row_path) {
                let still_path = row.season.zip(row.episode).and_then(|(s, e)| {
                    state
                        .catalog
                        .episode_stills(tmdb_id, s, e)
                        .ok()
                        .and_then(|mut v| v.drain(..).next())
                });
                ep_matches.push((row_path, still_path));
            }
        }
        ep_matches
    } else {
        let mut ep_matches = Vec::new();
        for season in &owned_seasons {
            let episodes = state.catalog.season_details(tmdb_id, *season).unwrap_or_default();
            for row in rows.iter().filter(|r| r.season == Some(*season)) {
                if let Some(ep_num) = row.episode {
                    let mut still_path = episodes
                        .iter()
                        .find(|e| e.episode_number == ep_num)
                        .and_then(|ep| ep.still_path.clone());
                    if still_path.is_none() {
                        if let Ok(stills) = state.catalog.episode_stills(tmdb_id, *season, ep_num) {
                            still_path = stills.into_iter().next();
                        }
                    }
                    ep_matches.push((PathBuf::from(&row.path), still_path));
                }
            }
        }
        ep_matches
    }
}

fn scrape_tv_stills(
    state: &crate::management::ApiState,
    matched_episodes: Vec<(PathBuf, Option<String>)>,
    still_size: &str,
) {
    scrape_tv_stills_with(state, matched_episodes, still_size, library::extract_frame);
}

fn scrape_tv_stills_with(
    state: &crate::management::ApiState,
    matched_episodes: Vec<(PathBuf, Option<String>)>,
    still_size: &str,
    extract: impl Fn(&Path, i64, &Path) -> Result<(), String>,
) {
    let is_allowed = |path: &Path| -> bool {
        let store = state.store.lock();
        if let Ok(libraries) = store.list_libraries() {
            libraries
                .into_iter()
                .filter(|lib| lib.root_paths.iter().any(|r| path.starts_with(r)))
                .max_by_key(|lib| {
                    lib.root_paths
                        .iter()
                        .filter(|r| path.starts_with(r))
                        .map(|r| r.components().count())
                        .max()
                        .unwrap_or(0)
                })
                .map(|lib| lib.generate_thumbnails)
                .unwrap_or(true)
        } else {
            true
        }
    };

    for (ep_path, still_path) in matched_episodes {
        if !targets::video_present(&ep_path) {
            continue;
        }
        let ok = crate::fanart_artwork::mirror_episode_still(
            state.poster_fetch.as_ref(),
            &ep_path,
            still_path.as_deref(),
            still_size,
        );
        if !ok && targets::video_present(&ep_path) && is_allowed(&ep_path) {
            if crate::episode_still::existing(&ep_path).is_none() {
                let still_dest = crate::episode_still::path(&ep_path);
                if let Err(err) = extract(&ep_path, 60_000, &still_dest) {
                    tracing::warn!(error = %err, path = %ep_path.display(), "ffmpeg 抽取剧照失败");
                }
            }
        }
    }
}

#[cfg(test)]
mod still_fallback_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    struct EmptyFetcher;
    impl indexer::Fetcher for EmptyFetcher {
        fn fetch(&self, _: &indexer::FetchRequest) -> Result<String, indexer::IndexerError> {
            Err(indexer::IndexerError::Fetch("unused".into()))
        }
    }

    struct RenameFetch {
        video: PathBuf,
    }
    impl crate::poster_fetch::PosterFetch for RenameFetch {
        fn get(&self, _: &str) -> Result<Vec<u8>, String> {
            let _ = std::fs::remove_file(&self.video);
            Err("download failed after rename".into())
        }
    }

    #[test]
    fn still_fallback_skips_ffmpeg_when_source_vanishes_during_download() {
        let tmp = tempfile::tempdir().unwrap();
        let video = tmp.path().join("S01E01.mkv");
        std::fs::write(&video, b"video").unwrap();
        let called = AtomicBool::new(false);
        let state = crate::management::ApiState::new(
            crate::Store::open(tmp.path().join("data")).unwrap(),
            "test-token".into(),
            indexer::ProfileSet::load(None).unwrap(),
            Arc::new(EmptyFetcher),
            Arc::new(downloader::MemoryDownloader::new(tmp.path().join("stage"))),
            tmp.path().join("library"),
        )
        .unwrap()
        .with_poster_fetch(Arc::new(RenameFetch {
            video: video.clone(),
        }));
        scrape_tv_stills_with(
            &state,
            vec![(video.clone(), Some("/ep1.jpg".into()))],
            "w300",
            |_, _, _| {
                called.store(true, Ordering::SeqCst);
                panic!("ffmpeg must not run after the source disappears");
            },
        );
        assert!(!called.load(Ordering::SeqCst));
        assert!(!video.exists());
    }
}

fn scrape_tv_fanart(
    state: &crate::management::ApiState,
    media: &Media,
    show_root: &Path,
    rows: &[LedgerRow],
    api_key: &str,
    fanart_lang: &[String],
) {
    let Some(tmdb_id) = media.tmdb_id.as_deref() else {
        return;
    };
    let tvdb_id = match media.tvdb_id.clone() {
        Some(id) if !id.is_empty() => Some(id),
        _ => match state.catalog.tvdb_id(tmdb_id) {
            Ok(Some(id)) if !id.is_empty() => {
                let mut updated = media.clone();
                updated.tvdb_id = Some(id.clone());
                let _ = state.store.lock().update_media(&updated);
                Some(id)
            }
            _ => None,
        },
    };

    let Some(tvdb_id) = tvdb_id else {
        tracing::info!(media = %media.title, "缺少 tvdb_id，跳过 Fanart.tv 抓取");
        return;
    };

    let getter = PosterFetchCatalogGet {
        fetch: state.poster_fetch.as_ref(),
    };
    let fanart_db = state.store.lock().data_dir().join("fanart.db");
    match media::fanart::FanartClient::new(getter, &fanart_db, api_key) {
        Ok(client) => {
            let lang_refs: Vec<&str> = fanart_lang.iter().map(|s| s.as_str()).collect();
            match client.tv(&tvdb_id, &lang_refs) {
                Ok(set) => {
                    crate::fanart_artwork::save_fanart_files(
                        state.poster_fetch.as_ref(),
                        show_root,
                        rows,
                        &set,
                    );
                }
                Err(error) => {
                    tracing::warn!(%error, %tvdb_id, "请求 Fanart.tv 失败");
                }
            }
        }
        Err(error) => {
            tracing::warn!(%error, "初始化 FanartClient 失败");
        }
    }
}

pub(crate) fn scrape_movie_fanart(
    state: &crate::management::ApiState,
    media: &Media,
    movie_root: &Path,
) {
    if !movie_root.is_dir() {
        return;
    }
    let Some(tmdb_id) = media.tmdb_id.as_deref() else {
        return;
    };
    let scrape_config = state.store.lock().get_scrape_config().ok();
    let (mirror_images, fanart_api_key, fanart_lang) = match scrape_config {
        Some(ref c) => (
            c.effective.mirror_images,
            c.effective.fanart_api_key.clone(),
            c.effective.fanart_language.clone(),
        ),
        None => (
            true,
            Some(crate::scrape_config::DEFAULT_FANART_API_KEY.to_string()),
            vec!["zh".into(), "en".into()],
        ),
    };
    if !mirror_images {
        return;
    }
    let Some(api_key) = fanart_api_key else {
        return;
    };

    let getter = PosterFetchCatalogGet {
        fetch: state.poster_fetch.as_ref(),
    };
    let fanart_db = state.store.lock().data_dir().join("fanart.db");
    match media::fanart::FanartClient::new(getter, &fanart_db, &api_key) {
        Ok(client) => {
            let lang_refs: Vec<&str> = fanart_lang.iter().map(|s| s.as_str()).collect();
            match client.movie(tmdb_id, &lang_refs) {
                Ok(set) => {
                    crate::fanart_artwork::save_fanart_files(
                        state.poster_fetch.as_ref(),
                        movie_root,
                        &[],
                        &set,
                    );
                }
                Err(error) => {
                    tracing::warn!(%error, %tmdb_id, "请求电影 Fanart.tv 失败");
                }
            }
        }
        Err(error) => {
            tracing::warn!(%error, "初始化 FanartClient 失败");
        }
    }
}
