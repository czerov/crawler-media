use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use domain::{Media, MediaKind};
use media::CatalogHit;
use serde::Deserialize;
use serde_json::{Value, json};

use super::management::{ApiError, ApiState};

pub use crate::catalog_fanout::{
    AnilistCatalog, BangumiCatalog, DoubanCatalog, FanoutCatalog, TvdbCatalog,
};

const TMDB_POSTER: &str = "https://image.tmdb.org/t/p/w342";
const TMDB_IMAGE: &str = "https://image.tmdb.org/t/p/";

/// Raw artwork candidate (TMDB `/images` row) for selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtworkCandidate {
    pub file_path: String,
    pub width: u32,
    pub height: u32,
    pub language: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ArtworkCandidates {
    pub posters: Vec<ArtworkCandidate>,
    pub backdrops: Vec<ArtworkCandidate>,
}

impl ArtworkCandidate {
    /// Full image URL at a TMDB size token (w780 / original / …).
    pub fn url(&self, size: &str) -> String {
        format!("{TMDB_IMAGE}{size}{}", self.file_path)
    }
}

pub trait Catalog: Send + Sync {
    /// Stable source key ("tmdb" | "douban" | "tvdb" | "bangumi" | "anilist").
    /// Fanout discovers and per-source Discover walls use it.
    fn source_name(&self) -> &'static str {
        ""
    }
    /// Resolve a single source by key. Only FanoutCatalog has children;
    /// single-source catalogs resolve via `source_name()` in the caller.
    fn source(&self, _name: &str) -> Option<&dyn Catalog> {
        None
    }
    /// 可用源清单（用于 per-source 搜索状态）；FanoutCatalog 返回全部子源。
    fn sources(&self) -> Vec<&'static str> {
        vec![self.source_name()]
    }
    fn search_movie(&self, query: &str) -> Result<Vec<CatalogHit>, String>;
    fn search_tv(&self, query: &str) -> Result<Vec<CatalogHit>, String>;
    /// TV search with an optional first-air year. Sources that ignore the year
    /// fall back to the unconstrained search.
    fn search_tv_year(&self, query: &str, _year: Option<u16>) -> Result<Vec<CatalogHit>, String> {
        self.search_tv(query)
    }
    fn popular_movie(&self) -> Result<Vec<CatalogHit>, String>;
    fn popular_tv(&self) -> Result<Vec<CatalogHit>, String>;
    fn top_rated_movie(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn top_rated_tv(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn now_playing_movie(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn on_the_air_tv(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }
    /// Filtered Discover (TMDB discover query string).
    /// Default: this source does not support filtered discover.
    fn filtered_movie(&self, _query: &str) -> Result<Vec<CatalogHit>, String> {
        Err("该数据源不支持筛选发现".into())
    }
    fn filtered_tv(&self, _query: &str) -> Result<Vec<CatalogHit>, String> {
        Err("该数据源不支持筛选发现".into())
    }
    /// Direct tag wall (Douban). Default: empty — TMDB sources don't use it.
    fn tagged_movie(&self, _tag: &str) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn tagged_tv(&self, _tag: &str) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }
    /// Paged discover for a collection wall. Default: None (not paginable).
    fn collection_paged(
        &self,
        _kind: MediaKind,
        _query: &str,
        _page: i64,
    ) -> Result<Option<(Vec<CatalogHit>, i64, i64, i64)>, String> {
        Ok(None)
    }
    fn trending_movie(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn trending_tv(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn upcoming_movie(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn airing_today_tv(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }
    /// Genre list `(id, name)` for the discover filter dialog. Default: empty.
    fn genres(&self, _kind: MediaKind) -> Result<Vec<(i64, String)>, String> {
        Ok(Vec::new())
    }
    fn details(&self, kind: MediaKind, tmdb_id: &str) -> Result<Option<Media>, String>;
    fn poster_url(&self, kind: MediaKind, tmdb_id: &str) -> Result<Option<String>, String> {
        let _ = (kind, tmdb_id);
        Ok(None)
    }
    /// Raw image candidates for artwork selection (TMDB `/images`). Default:
    /// none — consumers fall back to `poster_url`.
    fn image_candidates(
        &self,
        _kind: MediaKind,
        _tmdb_id: &str,
    ) -> Result<ArtworkCandidates, String> {
        Ok(ArtworkCandidates::default())
    }
    /// Extended item metadata (overview/rating/runtime/genres/cast). Default:
    /// none — consumers fall back to NFO sidecars.
    fn metadata(
        &self,
        _kind: MediaKind,
        _tmdb_id: &str,
    ) -> Result<Option<media::ItemMeta>, String> {
        Ok(None)
    }
    fn metadata_with_preferences(
        &self,
        kind: MediaKind,
        tmdb_id: &str,
        _language_priority: &[String],
        _cert_country_priority: &[String],
    ) -> Result<Option<media::ItemMeta>, String> {
        self.metadata(kind, tmdb_id)
    }
    /// TMDB episode still file paths. Default: none.
    fn episode_stills(
        &self,
        _tmdb_id: &str,
        _season: u32,
        _episode: u32,
    ) -> Result<Vec<String>, String> {
        Ok(Vec::new())
    }
    /// Whole-season episode details (names/overviews/stills). Default: none.
    fn season_details(
        &self,
        _tmdb_id: &str,
        _season: u32,
    ) -> Result<Vec<media::EpisodeMeta>, String> {
        Ok(Vec::new())
    }
    /// Read fresh or stale season details without making a network request.
    fn cached_season_details(
        &self,
        _tmdb_id: &str,
        _season: u32,
    ) -> Result<Option<Vec<media::EpisodeMeta>>, String> {
        Ok(None)
    }
    /// Season details with an explicit language (季级语言回落). Default: none.
    fn season_details_lang(
        &self,
        _tmdb_id: &str,
        _season: u32,
        _lang: &str,
    ) -> Result<Vec<media::EpisodeMeta>, String> {
        Ok(Vec::new())
    }
    /// Similar titles. Default: none.
    fn similar(&self, _kind: MediaKind, _tmdb_id: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    /// TMDB configuration languages. Default: none.
    fn configuration_languages(&self) -> Result<Vec<media::LanguageRow>, String> {
        Ok(Vec::new())
    }
    /// TMDB configuration countries. Default: none.
    fn configuration_countries(&self) -> Result<Vec<media::CountryRow>, String> {
        Ok(Vec::new())
    }
    /// Person + combined credits (clickable cast → person page). Default: none.
    fn person_details(
        &self,
        _tmdb_person_id: &str,
    ) -> Result<Option<media::PersonDetails>, String> {
        Ok(None)
    }
    /// Season overviews for a TV work. Default: none (non-TV sources).
    fn tv_seasons(&self, _tmdb_id: &str) -> Result<Vec<media::TvSeason>, String> {
        Ok(Vec::new())
    }
    /// 一季内的集与播出日期。Default: none。
    fn season_episodes(
        &self,
        _tmdb_id: &str,
        _season: u32,
    ) -> Result<Vec<media::SeasonEpisode>, String> {
        Ok(Vec::new())
    }
    /// TVDB id for an already-matched TMDB TV show.
    fn tvdb_id(&self, _tmdb_id: &str) -> Result<Option<String>, String> {
        Ok(None)
    }
}

pub struct EmptyCatalog;

impl Catalog for EmptyCatalog {
    fn search_movie(&self, _query: &str) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn search_tv(&self, _query: &str) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_movie(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_tv(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn details(&self, _kind: MediaKind, _tmdb_id: &str) -> Result<Option<Media>, String> {
        Ok(None)
    }
}

pub struct TmdbCatalog<H> {
    inner: media::Tmdb<H>,
}

impl<H: media::CatalogGet + Send + Sync> TmdbCatalog<H> {
    pub fn new(inner: media::Tmdb<H>) -> Arc<Self> {
        Arc::new(Self { inner })
    }
}

impl<H: media::CatalogGet + Send + Sync> Catalog for TmdbCatalog<H> {
    fn source_name(&self) -> &'static str {
        "tmdb"
    }
    fn source(&self, name: &str) -> Option<&dyn Catalog> {
        (name == "tmdb").then_some(self)
    }
    fn search_movie(&self, query: &str) -> Result<Vec<CatalogHit>, String> {
        self.inner
            .search_movie(query)
            .map_err(|err| err.to_string())
    }
    fn search_tv(&self, query: &str) -> Result<Vec<CatalogHit>, String> {
        self.search_tv_year(query, None)
    }
    fn search_tv_year(&self, query: &str, year: Option<u16>) -> Result<Vec<CatalogHit>, String> {
        self.inner
            .search_tv_year(query, year)
            .map_err(|err| err.to_string())
    }
    fn popular_movie(&self) -> Result<Vec<CatalogHit>, String> {
        self.inner.popular_movie().map_err(|err| err.to_string())
    }
    fn popular_tv(&self) -> Result<Vec<CatalogHit>, String> {
        self.inner.popular_tv().map_err(|err| err.to_string())
    }
    fn top_rated_movie(&self) -> Result<Vec<CatalogHit>, String> {
        self.inner.top_rated_movie().map_err(|err| err.to_string())
    }
    fn top_rated_tv(&self) -> Result<Vec<CatalogHit>, String> {
        self.inner.top_rated_tv().map_err(|err| err.to_string())
    }
    fn now_playing_movie(&self) -> Result<Vec<CatalogHit>, String> {
        self.inner
            .now_playing_movie()
            .map_err(|err| err.to_string())
    }
    fn on_the_air_tv(&self) -> Result<Vec<CatalogHit>, String> {
        self.inner.on_the_air_tv().map_err(|err| err.to_string())
    }
    fn filtered_movie(&self, query: &str) -> Result<Vec<CatalogHit>, String> {
        self.inner
            .discover_movie(query)
            .map_err(|err| err.to_string())
    }
    fn filtered_tv(&self, query: &str) -> Result<Vec<CatalogHit>, String> {
        self.inner.discover_tv(query).map_err(|err| err.to_string())
    }
    fn collection_paged(
        &self,
        kind: MediaKind,
        query: &str,
        page: i64,
    ) -> Result<Option<(Vec<CatalogHit>, i64, i64, i64)>, String> {
        let result = match kind {
            MediaKind::Movie | MediaKind::Video => self.inner.discover_movie_paged(query, page),
            MediaKind::Tv => self.inner.discover_tv_paged(query, page),
        }
        .map_err(|err| err.to_string())?;
        Ok(Some(result))
    }
    fn trending_movie(&self) -> Result<Vec<CatalogHit>, String> {
        self.inner.trending_movie().map_err(|err| err.to_string())
    }
    fn trending_tv(&self) -> Result<Vec<CatalogHit>, String> {
        self.inner.trending_tv().map_err(|err| err.to_string())
    }
    fn upcoming_movie(&self) -> Result<Vec<CatalogHit>, String> {
        self.inner.upcoming_movie().map_err(|err| err.to_string())
    }
    fn airing_today_tv(&self) -> Result<Vec<CatalogHit>, String> {
        self.inner.airing_today_tv().map_err(|err| err.to_string())
    }
    fn genres(&self, kind: MediaKind) -> Result<Vec<(i64, String)>, String> {
        self.inner.genres(kind).map_err(|err| err.to_string())
    }
    fn details(&self, kind: MediaKind, tmdb_id: &str) -> Result<Option<Media>, String> {
        let media = match kind {
            MediaKind::Movie | MediaKind::Video => self.inner.movie_details(tmdb_id),
            MediaKind::Tv => self.inner.tv_details(tmdb_id),
        }
        .map_err(|err| err.to_string())?;
        Ok(Some(media))
    }
    fn poster_url(&self, kind: MediaKind, tmdb_id: &str) -> Result<Option<String>, String> {
        self.inner
            .details_poster(kind, tmdb_id)
            .map(|path| path.map(|path| format!("{TMDB_POSTER}{path}")))
            .map_err(|err| err.to_string())
    }
    fn metadata(&self, kind: MediaKind, tmdb_id: &str) -> Result<Option<media::ItemMeta>, String> {
        self.inner
            .details_with_meta(kind, tmdb_id)
            .map(Some)
            .map_err(|err| err.to_string())
    }
    fn metadata_with_preferences(
        &self,
        kind: MediaKind,
        tmdb_id: &str,
        language_priority: &[String],
        cert_country_priority: &[String],
    ) -> Result<Option<media::ItemMeta>, String> {
        let languages = language_priority
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let countries = cert_country_priority
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        self.inner
            .details_with_meta_for(kind, tmdb_id, &languages, &countries)
            .map(Some)
            .map_err(|err| err.to_string())
    }
    fn episode_stills(
        &self,
        tmdb_id: &str,
        season: u32,
        episode: u32,
    ) -> Result<Vec<String>, String> {
        self.inner
            .episode_images(tmdb_id, season, episode)
            .map_err(|err| err.to_string())
    }
    fn season_details(
        &self,
        tmdb_id: &str,
        season: u32,
    ) -> Result<Vec<media::EpisodeMeta>, String> {
        self.inner
            .season_details(tmdb_id, season)
            .map_err(|err| err.to_string())
    }
    fn cached_season_details(
        &self,
        tmdb_id: &str,
        season: u32,
    ) -> Result<Option<Vec<media::EpisodeMeta>>, String> {
        self.inner
            .cached_season_details(tmdb_id, season)
            .map_err(|err| err.to_string())
    }
    fn season_details_lang(
        &self,
        tmdb_id: &str,
        season: u32,
        lang: &str,
    ) -> Result<Vec<media::EpisodeMeta>, String> {
        self.inner
            .season_details_lang(tmdb_id, season, Some(lang))
            .map_err(|err| err.to_string())
    }
    fn similar(&self, kind: MediaKind, tmdb_id: &str) -> Result<Vec<media::CatalogHit>, String> {
        self.inner
            .similar(kind, tmdb_id)
            .map_err(|err| err.to_string())
    }
    fn configuration_languages(&self) -> Result<Vec<media::LanguageRow>, String> {
        self.inner
            .configuration_languages()
            .map_err(|err| err.to_string())
    }
    fn configuration_countries(&self) -> Result<Vec<media::CountryRow>, String> {
        self.inner
            .configuration_countries()
            .map_err(|err| err.to_string())
    }
    fn person_details(&self, tmdb_person_id: &str) -> Result<Option<media::PersonDetails>, String> {
        self.inner
            .person_details(tmdb_person_id)
            .map(Some)
            .map_err(|err| err.to_string())
    }
    fn tv_seasons(&self, tmdb_id: &str) -> Result<Vec<media::TvSeason>, String> {
        self.inner
            .tv_seasons(tmdb_id)
            .map_err(|err| err.to_string())
    }
    fn season_episodes(
        &self,
        tmdb_id: &str,
        season: u32,
    ) -> Result<Vec<media::SeasonEpisode>, String> {
        self.inner
            .season_episodes(tmdb_id, season)
            .map_err(|err| err.to_string())
    }
    fn tvdb_id(&self, tmdb_id: &str) -> Result<Option<String>, String> {
        self.inner.tvdb_id(tmdb_id).map_err(|err| err.to_string())
    }
    fn image_candidates(
        &self,
        kind: MediaKind,
        tmdb_id: &str,
    ) -> Result<ArtworkCandidates, String> {
        let images = self
            .inner
            .images(kind, tmdb_id)
            .map_err(|err| err.to_string())?;
        let map = |rows: Vec<media::ImageCandidate>| {
            rows.into_iter()
                .map(|row| ArtworkCandidate {
                    file_path: row.file_path,
                    width: row.width,
                    height: row.height,
                    language: row.language,
                })
                .collect()
        };
        Ok(ArtworkCandidates {
            posters: map(images.posters),
            backdrops: map(images.backdrops),
        })
    }
}

#[derive(Deserialize)]
pub struct CatalogQuery {
    query: String,
}

#[derive(Deserialize)]
pub struct DiscoverQuery {
    kind: String,
}

pub async fn search_catalog(
    State(state): State<ApiState>,
    Query(query): Query<CatalogQuery>,
) -> Result<Json<Value>, ApiError> {
    let movies = state.catalog.search_movie(&query.query);
    let shows = state.catalog.search_tv(&query.query);
    if movies.is_err() && shows.is_err() {
        return Err(ApiError::invalid(
            "catalog.invalid",
            movies.err().unwrap_or_default(),
        ));
    }
    let mut hits = movies.unwrap_or_default();
    hits.extend(shows.unwrap_or_default());
    Ok(Json(Value::Array(hits.into_iter().map(hit_json).collect())))
}

pub async fn discover_catalog(
    State(state): State<ApiState>,
    Query(query): Query<DiscoverQuery>,
) -> Result<Json<Value>, ApiError> {
    let hits = match query.kind.as_str() {
        "movie" => state.catalog.popular_movie(),
        "tv" => state.catalog.popular_tv(),
        other => {
            return Err(ApiError::invalid(
                "catalog.invalid",
                format!("invalid Media kind {other}"),
            ));
        }
    }
    .map_err(|err| ApiError::invalid("catalog.invalid", err))?;
    Ok(Json(Value::Array(hits.into_iter().map(hit_json).collect())))
}

fn hit_json(hit: CatalogHit) -> Value {
    json!({
        "title": hit.media.title,
        "year": hit.media.year,
        "kind": hit.media.kind.as_str(),
        "tmdb_id": hit.media.tmdb_id,
        "douban_id": hit.media.douban_id,
        "tvdb_id": hit.media.tvdb_id,
        "bangumi_id": hit.media.bangumi_id,
        "anilist_id": hit.media.anilist_id,
        "poster": poster_url(hit.poster_path.as_deref()),
        "letter": letter_tile(&hit.media.title),
    })
}

fn poster_url(path: Option<&str>) -> Option<String> {
    let path = path.filter(|path| !path.is_empty())?;
    if path.starts_with("https://") || path.starts_with("http://") {
        Some(path.to_string())
    } else if path.starts_with('/') {
        Some(format!("{TMDB_POSTER}{path}"))
    } else {
        None
    }
}

fn letter_tile(title: &str) -> String {
    title
        .chars()
        .find(|ch| ch.is_alphanumeric())
        .map(|ch| ch.to_uppercase().to_string())
        .unwrap_or_else(|| "?".into())
}

pub fn require_tv_season(kind: MediaKind, has_season: bool) -> Result<(), ApiError> {
    if kind == MediaKind::Tv && !has_season {
        return Err(ApiError::invalid(
            "subscribe.invalid",
            "TV Subscribe requires season".into(),
        ));
    }
    Ok(())
}
