use domain::{Media, MediaId, MediaKind};
use serde::Deserialize;

use crate::client::TmdbError;

pub use crate::metadata::{
    CastRow, EpisodeMeta, ItemMeta, SeasonEpisode, TvSeason, details_meta_for, season_details,
    season_episodes, tv_seasons,
};

#[derive(Clone, Debug, PartialEq)]
pub struct CatalogHit {
    pub media: Media,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
    /// 0–10 评分（TMDB vote_average / 豆瓣 rate）。
    pub rating: Option<f32>,
    /// 一句话简介（hover 信息层 / 详情用）。
    pub overview: Option<String>,
}

#[derive(Deserialize)]
struct SearchResponse {
    results: Vec<SearchHit>,
}

/// Paged search/list response: carries page totals so consumers can paginate.
#[derive(Deserialize)]
struct PagedSearchResponse {
    #[serde(default)]
    results: Vec<SearchHit>,
    #[serde(default)]
    page: i64,
    #[serde(default)]
    total_pages: i64,
    #[serde(default)]
    total_results: i64,
}

/// Parse a paged search body into `(hits, page, total_pages, total_results)`.
pub fn paged_search_results(
    kind: MediaKind,
    body: &str,
) -> Result<(Vec<CatalogHit>, i64, i64, i64), TmdbError> {
    let parsed: PagedSearchResponse = serde_json::from_str(body)?;
    let hits = parsed
        .results
        .into_iter()
        .map(|hit| {
            let year = hit
                .release_date
                .as_deref()
                .or(hit.first_air_date.as_deref())
                .and_then(|date| date.split('-').next())
                .and_then(|year| year.parse::<u16>().ok());
            let original_title = hit.original_title.or(hit.original_name);
            Ok(CatalogHit {
                media: to_media(kind, hit.id, hit.title.or(hit.name), year, original_title)?,
                poster_path: hit.poster_path.filter(|path| !path.is_empty()),
                backdrop_path: hit.backdrop_path.filter(|path| !path.is_empty()),
                rating: hit.vote_average,
                overview: hit.overview.filter(|o| !o.is_empty()),
            })
        })
        .collect::<Result<Vec<_>, TmdbError>>()?;
    Ok((hits, parsed.page, parsed.total_pages, parsed.total_results))
}

#[derive(Deserialize)]
struct SearchHit {
    id: i64,
    title: Option<String>,
    name: Option<String>,
    original_title: Option<String>,
    original_name: Option<String>,
    poster_path: Option<String>,
    backdrop_path: Option<String>,
    #[serde(default)]
    vote_average: Option<f32>,
    #[serde(default)]
    release_date: Option<String>,
    #[serde(default)]
    first_air_date: Option<String>,
    #[serde(default)]
    overview: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageCandidate {
    pub file_path: String,
    pub width: u32,
    pub height: u32,
    pub language: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Images {
    pub posters: Vec<ImageCandidate>,
    pub backdrops: Vec<ImageCandidate>,
}

#[derive(Deserialize)]
struct ImagesResponse {
    #[serde(default)]
    posters: Vec<ImageRow>,
    #[serde(default)]
    backdrops: Vec<ImageRow>,
}

#[derive(Deserialize)]
struct ImageRow {
    file_path: String,
    #[serde(default)]
    width: u32,
    #[serde(default)]
    height: u32,
    #[serde(default)]
    iso_639_1: Option<String>,
}

pub fn episode_stills(body: &str) -> Result<Vec<String>, TmdbError> {
    #[derive(Deserialize)]
    struct Stills {
        #[serde(default)]
        stills: Vec<StillRow>,
    }
    #[derive(Deserialize)]
    struct StillRow {
        file_path: String,
    }
    let parsed: Stills = serde_json::from_str(body)?;
    Ok(parsed
        .stills
        .into_iter()
        .map(|row| row.file_path)
        .filter(|path| !path.is_empty())
        .collect())
}

/// TMDB configuration language row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CountryRow {
    pub code: String,
    pub english_name: String,
    pub native_name: String,
}

pub fn configuration_countries(body: &str) -> Result<Vec<CountryRow>, TmdbError> {
    #[derive(Deserialize)]
    struct Row {
        iso_3166_1: Option<String>,
        english_name: Option<String>,
        native_name: Option<String>,
    }
    let rows: Vec<Row> = serde_json::from_str(body)?;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let code = row.iso_3166_1?;
            if code.is_empty() {
                return None;
            }
            Some(CountryRow {
                code,
                english_name: row.english_name.unwrap_or_default(),
                native_name: row.native_name.unwrap_or_default(),
            })
        })
        .collect())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanguageRow {
    pub code: String,
    pub english_name: String,
    pub name: String,
}

pub fn configuration_languages(body: &str) -> Result<Vec<LanguageRow>, TmdbError> {
    #[derive(Deserialize)]
    struct Row {
        iso_639_1: Option<String>,
        english_name: Option<String>,
        name: Option<String>,
    }
    let rows: Vec<Row> = serde_json::from_str(body)?;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let code = row.iso_639_1?;
            if code.is_empty() {
                return None;
            }
            Some(LanguageRow {
                code,
                english_name: row.english_name.unwrap_or_default(),
                name: row.name.unwrap_or_default(),
            })
        })
        .collect())
}

pub fn images(body: &str) -> Result<Images, TmdbError> {
    let parsed: ImagesResponse = serde_json::from_str(body)?;
    let map = |rows: Vec<ImageRow>| {
        rows.into_iter()
            .map(|row| ImageCandidate {
                file_path: row.file_path,
                width: row.width,
                height: row.height,
                language: row.iso_639_1.filter(|lang| !lang.is_empty()),
            })
            .collect()
    };
    Ok(Images {
        posters: map(parsed.posters),
        backdrops: map(parsed.backdrops),
    })
}

#[derive(Deserialize)]
struct Details {
    id: i64,
    title: Option<String>,
    name: Option<String>,
    original_title: Option<String>,
    original_name: Option<String>,
    #[serde(default)]
    release_date: Option<String>,
    #[serde(default)]
    first_air_date: Option<String>,
    #[serde(default)]
    translations: Option<TranslationsWrapper>,
    #[serde(default)]
    alternative_titles: Option<AlternativeTitles>,
}

#[derive(Deserialize)]
struct TranslationsWrapper {
    #[serde(default)]
    translations: Vec<TranslationItem>,
}

#[derive(Deserialize)]
struct TranslationItem {
    iso_639_1: Option<String>,
    #[serde(default)]
    data: TranslationData,
}

#[derive(Deserialize, Default)]
struct TranslationData {
    name: Option<String>,
    title: Option<String>,
}

#[derive(Deserialize, Default)]
struct AlternativeTitles {
    #[serde(default)]
    titles: Vec<AlternativeTitle>,
    #[serde(default)]
    results: Vec<AlternativeTitle>,
}

#[derive(Deserialize, Default)]
struct AlternativeTitle {
    title: Option<String>,
}

pub fn search_results(kind: MediaKind, body: &str) -> Result<Vec<CatalogHit>, TmdbError> {
    let parsed: SearchResponse = serde_json::from_str(body)?;
    parsed
        .results
        .into_iter()
        .map(|hit| {
            let year = hit
                .release_date
                .as_deref()
                .or(hit.first_air_date.as_deref())
                .and_then(|date| date.split('-').next())
                .and_then(|year| year.parse::<u16>().ok());
            let original_title = hit.original_title.or(hit.original_name);
            Ok(CatalogHit {
                media: to_media(kind, hit.id, hit.title.or(hit.name), year, original_title)?,
                poster_path: hit.poster_path.filter(|path| !path.is_empty()),
                backdrop_path: hit.backdrop_path.filter(|path| !path.is_empty()),
                rating: hit.vote_average,
                overview: hit.overview.filter(|o| !o.is_empty()),
            })
        })
        .collect()
}

pub fn details(kind: MediaKind, body: &str) -> Result<Media, TmdbError> {
    let parsed: Details = serde_json::from_str(body)?;
    let year = parsed
        .release_date
        .as_deref()
        .or(parsed.first_air_date.as_deref())
        .and_then(|date| date.split('-').next())
        .and_then(|year| year.parse::<u16>().ok());
    let mut original_title = parsed
        .original_title
        .clone()
        .or_else(|| parsed.original_name.clone());
    // 如果原名与中文名相同（如国产剧），尝试从英文翻译中提取英文译名作为 alias/original_title
    if let Some(wrapper) = &parsed.translations {
        for tr in &wrapper.translations {
            if tr.iso_639_1.as_deref() == Some("en") {
                let en_name = tr.data.name.as_ref().or(tr.data.title.as_ref());
                if let Some(en) = en_name.filter(|s| !s.is_empty()) {
                    original_title = Some(en.clone());
                    break;
                }
            }
        }
    }
    let title = parsed.title.clone().or_else(|| parsed.name.clone());
    let mut media = to_media(kind, parsed.id, title, year, original_title)?;
    media.aliases = aliases_from_details(&parsed, &media);
    Ok(media)
}

fn aliases_from_details(parsed: &Details, media: &Media) -> Vec<String> {
    let mut aliases = Vec::new();
    let mut push = |title: &Option<String>| {
        let Some(title) = title
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty())
        else {
            return;
        };
        if title == media.title || media.original_title.as_deref() == Some(title) {
            return;
        }
        if aliases.iter().all(|existing| existing != title) {
            aliases.push(title.to_string());
        }
    };
    if let Some(alternative) = &parsed.alternative_titles {
        for title in &alternative.titles {
            push(&title.title);
        }
        for title in &alternative.results {
            push(&title.title);
        }
    }
    if let Some(wrapper) = &parsed.translations {
        for translation in &wrapper.translations {
            push(&translation.data.title);
            push(&translation.data.name);
        }
    }
    aliases
}

pub fn details_poster_path(body: &str) -> Result<Option<String>, TmdbError> {
    #[derive(Deserialize)]
    struct Poster {
        poster_path: Option<String>,
    }
    let parsed: Poster = serde_json::from_str(body)?;
    Ok(parsed.poster_path.filter(|path| !path.is_empty()))
}

fn to_media(
    kind: MediaKind,
    tmdb_id: i64,
    title: Option<String>,
    year: Option<u16>,
    original_title: Option<String>,
) -> Result<Media, TmdbError> {
    let title = title.ok_or_else(|| TmdbError::Parse("missing title".into()))?;
    let orig = original_title.filter(|t| !t.is_empty() && t != &title);
    Ok(Media {
        id: MediaId::new(),
        kind,
        title,
        year,
        original_title: orig,
        aliases: Vec::new(),
        tmdb_id: Some(tmdb_id.to_string()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    })
}
