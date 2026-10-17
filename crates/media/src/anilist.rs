use std::path::Path;

use domain::{Media, MediaId, MediaKind};
use serde::Deserialize;

use crate::cache::CatalogCache;
use crate::client::{CatalogGet, TmdbError};
use crate::parse::CatalogHit;

pub struct Anilist<H> {
    http: H,
    cache: CatalogCache,
}

impl<H: CatalogGet> Anilist<H> {
    pub fn new(http: H, catalog_db: &Path) -> Result<Self, TmdbError> {
        // now = 0 → CatalogCache 走实时时钟（生产路径缓存正常过期）。
        Self::new_at(http, catalog_db, 0)
    }

    pub fn new_at(http: H, catalog_db: &Path, now: i64) -> Result<Self, TmdbError> {
        Ok(Self {
            http,
            cache: CatalogCache::open_source(catalog_db, now, "anilist")?,
        })
    }

    pub fn set_now(&self, now: i64) {
        self.cache.set_now(now);
    }

    pub fn search_movie(&self, query: &str) -> Result<Vec<CatalogHit>, TmdbError> {
        self.cached_search(MediaKind::Movie, query)
    }

    pub fn search_tv(&self, query: &str) -> Result<Vec<CatalogHit>, TmdbError> {
        self.cached_search(MediaKind::Tv, query)
    }

    pub fn popular_movie(&self) -> Result<Vec<CatalogHit>, TmdbError> {
        Ok(Vec::new())
    }

    pub fn popular_tv(&self) -> Result<Vec<CatalogHit>, TmdbError> {
        Ok(Vec::new())
    }

    fn cached_search(&self, kind: MediaKind, query: &str) -> Result<Vec<CatalogHit>, TmdbError> {
        self.cache
            .get_or_fetch(&self.http, &search_path(kind, query), |body| {
                crate::contracts::json(body)?;
                parse_search(kind, body)
            })
    }
}

fn search_path(kind: MediaKind, query: &str) -> String {
    let format = match kind {
        MediaKind::Movie | MediaKind::Video => "MOVIE",
        MediaKind::Tv => "TV",
    };
    format!("/search?query={}&format={format}", encode(query))
}

#[derive(Deserialize)]
struct SearchResponse {
    data: Data,
}

#[derive(Deserialize)]
struct Data {
    #[serde(rename = "Page")]
    page: Page,
}

#[derive(Deserialize)]
struct Page {
    media: Vec<SearchHit>,
}

#[derive(Deserialize)]
struct SearchHit {
    id: i64,
    title: Titles,
    format: Option<String>,
    #[serde(rename = "seasonYear")]
    season_year: Option<u16>,
    #[serde(rename = "coverImage")]
    cover_image: Option<Cover>,
}

#[derive(Deserialize)]
struct Titles {
    romaji: Option<String>,
    english: Option<String>,
    native: Option<String>,
}

#[derive(Deserialize)]
struct Cover {
    large: Option<String>,
}

fn parse_search(kind: MediaKind, body: &str) -> Result<Vec<CatalogHit>, TmdbError> {
    let parsed: SearchResponse = serde_json::from_str(body)?;
    parsed
        .data
        .page
        .media
        .into_iter()
        .filter(|hit| matches_kind(kind, hit.format.as_deref()))
        .map(to_hit)
        .collect()
}

fn matches_kind(kind: MediaKind, format: Option<&str>) -> bool {
    match (kind, format) {
        (MediaKind::Movie | MediaKind::Video, Some("MOVIE")) => true,
        (MediaKind::Movie | MediaKind::Video, _) => false,
        (MediaKind::Tv, Some("MOVIE")) => false,
        (MediaKind::Tv, _) => true,
    }
}

fn to_hit(hit: SearchHit) -> Result<CatalogHit, TmdbError> {
    let kind = match hit.format.as_deref() {
        Some("MOVIE") => MediaKind::Movie,
        _ => MediaKind::Tv,
    };
    let title = hit
        .title
        .english
        .filter(|title| !title.is_empty())
        .or(hit.title.romaji)
        .or(hit.title.native)
        .ok_or_else(|| TmdbError::Parse("missing title".into()))?;
    let poster = hit
        .cover_image
        .and_then(|cover| cover.large)
        .filter(|path| !path.is_empty());
    Ok(CatalogHit {
        media: Media {
            id: MediaId::new(),
            kind,
            title,
            year: hit.season_year,
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: Some(hit.id.to_string()),
        },
        poster_path: poster,
        backdrop_path: None,
        rating: None,
        overview: None,
    })
}

fn encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(char::from(byte));
            }
            b' ' => encoded.push('+'),
            _ => {
                use std::fmt::Write;
                let _ = write!(encoded, "%{byte:02X}");
            }
        }
    }
    encoded
}
