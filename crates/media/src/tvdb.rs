use std::path::Path;

use domain::{Media, MediaId, MediaKind};
use serde::Deserialize;

use crate::cache::CatalogCache;
use crate::client::{CatalogGet, TmdbError};
use crate::parse::CatalogHit;

pub struct Tvdb<H> {
    http: H,
    cache: CatalogCache,
}

impl<H: CatalogGet> Tvdb<H> {
    pub fn new(http: H, catalog_db: &Path) -> Result<Self, TmdbError> {
        // now = 0 → CatalogCache 走实时时钟（生产路径缓存正常过期）。
        Self::new_at(http, catalog_db, 0)
    }

    pub fn new_at(http: H, catalog_db: &Path, now: i64) -> Result<Self, TmdbError> {
        Ok(Self {
            http,
            cache: CatalogCache::open_source(catalog_db, now, "tvdb")?,
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
        MediaKind::Movie | MediaKind::Video => "movie",
        MediaKind::Tv => "series",
    };
    format!("/search?query={}&type={ty}", encode(query))
}

#[derive(Deserialize)]
struct SearchResponse {
    data: Vec<SearchHit>,
}

#[derive(Deserialize)]
struct SearchHit {
    tvdb_id: serde_json::Value,
    name: Option<String>,
    year: Option<String>,
    image_url: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
}

fn parse_search(kind: MediaKind, body: &str) -> Result<Vec<CatalogHit>, TmdbError> {
    let parsed: SearchResponse = serde_json::from_str(body)?;
    parsed
        .data
        .into_iter()
        .filter(|hit| matches_kind(kind, hit.kind.as_deref()))
        .map(|hit| {
            to_hit(
                kind,
                id_string(hit.tvdb_id),
                hit.name,
                hit.year,
                hit.image_url,
            )
        })
        .collect()
}

fn matches_kind(kind: MediaKind, raw: Option<&str>) -> bool {
    match (kind, raw) {
        (MediaKind::Movie, Some("series")) => false,
        (MediaKind::Tv, Some("movie")) => false,
        _ => true,
    }
}

fn id_string(value: serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(id) if !id.is_empty() => Some(id),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn to_hit(
    kind: MediaKind,
    id: Option<String>,
    title: Option<String>,
    year: Option<String>,
    poster: Option<String>,
) -> Result<CatalogHit, TmdbError> {
    let id = id.ok_or_else(|| TmdbError::Parse("missing tvdb_id".into()))?;
    let title = title.ok_or_else(|| TmdbError::Parse("missing title".into()))?;
    Ok(CatalogHit {
        media: Media {
            id: MediaId::new(),
            kind,
            title,
            year: year.and_then(|value| value.parse().ok()),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: Some(id),
            bangumi_id: None,
            anilist_id: None,
        },
        poster_path: poster.filter(|path| !path.is_empty()),
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
