use std::path::Path;

use domain::{Media, MediaId, MediaKind};
use serde::Deserialize;

use crate::cache::CatalogCache;
use crate::client::{CatalogGet, TmdbError};
use crate::parse::CatalogHit;

pub struct Bangumi<H> {
    http: H,
    cache: CatalogCache,
}

impl<H: CatalogGet> Bangumi<H> {
    pub fn new(http: H, catalog_db: &Path) -> Result<Self, TmdbError> {
        // now = 0 → CatalogCache 走实时时钟（生产路径缓存正常过期）。
        Self::new_at(http, catalog_db, 0)
    }

    pub fn new_at(http: H, catalog_db: &Path, now: i64) -> Result<Self, TmdbError> {
        Ok(Self {
            http,
            cache: CatalogCache::open_source(catalog_db, now, "bangumi")?,
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
    let ty = match kind {
        MediaKind::Movie | MediaKind::Video => 6,
        MediaKind::Tv => 2,
    };
    format!(
        "/search/subject/{}?type={ty}&responseGroup=small",
        encode(query)
    )
}

#[derive(Deserialize)]
struct SearchResponse {
    list: Vec<SearchHit>,
}

#[derive(Deserialize)]
struct SearchHit {
    id: i64,
    name: Option<String>,
    name_cn: Option<String>,
    air_date: Option<String>,
    images: Option<Images>,
}

#[derive(Deserialize)]
struct Images {
    large: Option<String>,
}

fn parse_search(kind: MediaKind, body: &str) -> Result<Vec<CatalogHit>, TmdbError> {
    let parsed: SearchResponse = serde_json::from_str(body)?;
    parsed
        .list
        .into_iter()
        .map(|hit| to_hit(kind, hit))
        .collect()
}

fn to_hit(kind: MediaKind, hit: SearchHit) -> Result<CatalogHit, TmdbError> {
    let title = hit
        .name_cn
        .filter(|title| !title.is_empty())
        .or(hit.name)
        .ok_or_else(|| TmdbError::Parse("missing title".into()))?;
    let year = hit.air_date.and_then(|date| date.get(..4)?.parse().ok());
    let poster = hit
        .images
        .and_then(|images| images.large)
        .filter(|path| !path.is_empty());
    Ok(CatalogHit {
        media: Media {
            id: MediaId::new(),
            kind,
            title,
            year,
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: Some(hit.id.to_string()),
            anilist_id: None,
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
