use std::path::Path;

use domain::{Media, MediaId, MediaKind};
use serde::Deserialize;

use crate::cache::CatalogCache;
use crate::client::{CatalogGet, TmdbError};
use crate::parse::CatalogHit;

pub struct Douban<H> {
    http: H,
    cache: CatalogCache,
}

impl<H: CatalogGet> Douban<H> {
    pub fn new(http: H, catalog_db: &Path) -> Result<Self, TmdbError> {
        // now = 0 → CatalogCache 走实时时钟（生产路径缓存正常过期）。
        Self::new_at(http, catalog_db, 0)
    }

    pub fn new_at(http: H, catalog_db: &Path, now: i64) -> Result<Self, TmdbError> {
        Ok(Self {
            http,
            cache: CatalogCache::open_source(catalog_db, now, "douban")?,
        })
    }

    pub fn set_now(&self, now: i64) {
        self.cache.set_now(now);
    }

    pub fn search_movie(&self, query: &str) -> Result<Vec<CatalogHit>, TmdbError> {
        self.cached_suggest(MediaKind::Movie, query)
    }

    pub fn search_tv(&self, query: &str) -> Result<Vec<CatalogHit>, TmdbError> {
        self.cached_suggest(MediaKind::Tv, query)
    }

    pub fn popular_movie(&self) -> Result<Vec<CatalogHit>, TmdbError> {
        self.cached_popular(MediaKind::Movie, &popular_path("movie"))
    }

    pub fn popular_tv(&self) -> Result<Vec<CatalogHit>, TmdbError> {
        self.cached_popular(MediaKind::Tv, &popular_path("tv"))
    }

    pub fn top_rated_movie(&self) -> Result<Vec<CatalogHit>, TmdbError> {
        self.cached_popular(MediaKind::Movie, &tagged_path("movie", "豆瓣高分"))
    }

    pub fn top_rated_tv(&self) -> Result<Vec<CatalogHit>, TmdbError> {
        self.cached_popular(MediaKind::Tv, &tagged_path("tv", "豆瓣高分"))
    }

    pub fn now_playing_movie(&self) -> Result<Vec<CatalogHit>, TmdbError> {
        self.cached_subject_collection(
            MediaKind::Movie,
            "/rexxar/api/v2/subject_collection/movie_showing/items?start=0&count=20",
        )
    }

    pub fn on_the_air_tv(&self) -> Result<Vec<CatalogHit>, TmdbError> {
        self.cached_popular(MediaKind::Tv, &tagged_path("tv", "最新"))
    }

    pub fn trending_movie(&self) -> Result<Vec<CatalogHit>, TmdbError> {
        self.cached_subject_collection(
            MediaKind::Movie,
            "/rexxar/api/v2/subject_collection/movie_hot_gaia/items?start=0&count=20",
        )
    }

    pub fn trending_tv(&self) -> Result<Vec<CatalogHit>, TmdbError> {
        self.cached_subject_collection(
            MediaKind::Tv,
            "/rexxar/api/v2/subject_collection/tv_hot/items?start=0&count=20",
        )
    }

    /// 豆瓣榜单集合（例如实时热门榜 tv_hot / tv_real_time_hotest 等）
    pub fn subject_collection(
        &self,
        kind: MediaKind,
        collection_id: &str,
    ) -> Result<Vec<CatalogHit>, TmdbError> {
        let path =
            format!("/rexxar/api/v2/subject_collection/{collection_id}/items?start=0&count=20");
        self.cached_subject_collection(kind, &path)
    }

    pub fn upcoming_movie(&self) -> Result<Vec<CatalogHit>, TmdbError> {
        self.cached_popular(MediaKind::Movie, &tagged_path("movie", "最新"))
    }

    /// Map a TMDB discover query onto Douban tags where one exists.
    /// Unmapped queries (networks, runtime windows, votes) return empty.
    pub fn filtered(&self, kind: MediaKind, query: &str) -> Result<Vec<CatalogHit>, TmdbError> {
        let Some(tag) = douban_tag_for_query(kind, query) else {
            return Ok(Vec::new());
        };
        self.cached_popular(kind, &tagged_path(kind_key(kind), tag))
    }

    /// Direct tag wall — the douban wall's own section table passes the real
    /// Douban tag (实时热榜/经典/冷门佳片/IMAX…) instead of a TMDB query.
    pub fn tagged(&self, kind: MediaKind, tag: &str) -> Result<Vec<CatalogHit>, TmdbError> {
        self.cached_popular(kind, &tagged_path(kind_key(kind), tag))
    }

    /// Paginated direct tag wall.
    pub fn tagged_paged(
        &self,
        kind: MediaKind,
        tag: &str,
        page: i64,
    ) -> Result<Vec<CatalogHit>, TmdbError> {
        self.cached_popular(kind, &tagged_path_paged(kind_key(kind), tag, page))
    }

    /// Paginated tag wall for a collection listing (page 1-based, 20/page).
    pub fn filtered_paged(
        &self,
        kind: MediaKind,
        query: &str,
        page: i64,
    ) -> Result<Vec<CatalogHit>, TmdbError> {
        let Some(tag) = douban_tag_for_query(kind, query) else {
            return Ok(Vec::new());
        };
        self.cached_popular(kind, &tagged_path_paged(kind_key(kind), tag, page))
    }

    fn cached_suggest(&self, kind: MediaKind, query: &str) -> Result<Vec<CatalogHit>, TmdbError> {
        self.cache.get_or_fetch(
            &self.http,
            &format!("/j/subject_suggest?q={}", encode(query)),
            |body| {
                crate::contracts::json(body)?;
                parse_suggest(kind, body)
            },
        )
    }

    fn cached_popular(&self, kind: MediaKind, path: &str) -> Result<Vec<CatalogHit>, TmdbError> {
        self.cache.get_or_fetch(&self.http, path, |body| {
            crate::contracts::json(body)?;
            parse_popular(kind, body)
        })
    }

    fn cached_subject_collection(
        &self,
        kind: MediaKind,
        path: &str,
    ) -> Result<Vec<CatalogHit>, TmdbError> {
        self.cache.get_or_fetch(&self.http, path, |body| {
            crate::contracts::object(body, &["subject_collection_items"])?;
            parse_subject_collection(kind, body)
        })
    }

    /// 豆瓣 subject 详情：抓 `/subject/{id}/` 解析标题/年份/类型。
    /// 详情页对 kind 无参数，类型从页面「集数」字段自判（剧集 → Tv）。
    pub fn detail(&self, id: &str) -> Result<Option<Media>, TmdbError> {
        let (title, year, is_tv) =
            self.cache
                .get_or_fetch(&self.http, &format!("/subject/{id}/"), |body| {
                    parse_detail(body).ok_or_else(|| {
                        TmdbError::Parse(
                            "missing Douban subject details; response may require authentication"
                                .into(),
                        )
                    })
                })?;
        Ok(Some(Media {
            id: MediaId::new(),
            kind: if is_tv {
                MediaKind::Tv
            } else {
                MediaKind::Movie
            },
            title,
            year,
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: Some(id.to_string()),
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        }))
    }
}

fn popular_path(kind: &str) -> String {
    tagged_path(kind, "热门")
}

fn kind_key(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Movie | MediaKind::Video => "movie",
        MediaKind::Tv => "tv",
    }
}

/// TMDB discover query → Douban tag name. Returns None when Douban has no
/// equivalent row (networks, vote thresholds, runtime, date windows).
fn douban_tag_for_query(kind: MediaKind, query: &str) -> Option<&'static str> {
    let lang = |key: &str| {
        query
            .split('&')
            .find_map(|pair| pair.strip_prefix(key))
            .map(str::to_string)
    };
    match kind {
        MediaKind::Movie | MediaKind::Video => {
            if let Some(language) = lang("with_original_language=") {
                return match language.as_str() {
                    "zh" => Some("华语"),
                    "ja" => Some("日本"),
                    "ko" => Some("韩国"),
                    "en" => Some("欧美"),
                    _ => None,
                };
            }
            if let Some(genres) = lang("with_genres=") {
                return match genres.as_str() {
                    "878" => Some("科幻"),
                    "28" => Some("动作"),
                    "53" => Some("悬疑"),
                    "35" => Some("喜剧"),
                    "10749" => Some("爱情"),
                    "27" => Some("恐怖"),
                    "16" => Some("动画"),
                    "99" => Some("纪录片"),
                    _ => None,
                };
            }
            // 高分经典：豆瓣 tag「豆瓣高分」（可翻页）。
            if query.contains("vote_average.desc") && !query.contains("with_") {
                return Some("豆瓣高分");
            }
            None
        }
        MediaKind::Tv => {
            if let Some(language) = lang("with_original_language=") {
                return match language.as_str() {
                    "zh" => Some("国产剧"),
                    "ja" => Some("日剧"),
                    "ko" => Some("韩剧"),
                    "en" => Some("美剧"),
                    _ => None,
                };
            }
            if let Some(genres) = lang("with_genres=") {
                return match genres.as_str() {
                    "16" => Some("动漫"),
                    "99" => Some("纪录片"),
                    _ => None,
                };
            }
            if let Some(country) = lang("with_origin_country=") {
                return match country.as_str() {
                    "US" => Some("美剧"),
                    "GB" => Some("英剧"),
                    _ => None,
                };
            }
            None
        }
    }
}

/// Douban tag name → `/j/search_subjects` path.
fn tagged_path(kind: &str, tag: &str) -> String {
    format!(
        "/j/search_subjects?type={kind}&tag={}&page_limit=20&page_start=0",
        encode(tag)
    )
}

/// Paginated `/j/search_subjects` path (page 1-based, 20 per page).
fn tagged_path_paged(kind: &str, tag: &str, page: i64) -> String {
    let start = (page - 1).max(0) * 20;
    format!(
        "/j/search_subjects?type={kind}&tag={}&page_limit=20&page_start={start}",
        encode(tag)
    )
}

#[derive(Deserialize)]
struct SuggestHit {
    id: String,
    title: Option<String>,
    year: Option<String>,
    img: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
}

#[derive(Deserialize)]
struct PopularResponse {
    subjects: Vec<PopularHit>,
}

#[derive(Deserialize)]
struct PopularHit {
    id: String,
    title: String,
    cover: Option<String>,
    #[serde(default)]
    rate: Option<String>,
}

#[derive(Deserialize)]
struct SubjectCollectionResponse {
    #[serde(default)]
    subject_collection_items: Vec<SubjectCollectionItem>,
}

#[derive(Deserialize)]
struct SubjectCollectionItem {
    id: String,
    title: String,
    year: Option<String>,
    #[serde(default)]
    card_subtitle: Option<String>,
    #[serde(default)]
    pic: Option<SubjectCollectionPic>,
    #[serde(default)]
    cover: Option<SubjectCollectionCover>,
    #[serde(default)]
    cover_url: Option<String>,
    #[serde(default)]
    rating: Option<SubjectCollectionRating>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Deserialize)]
struct SubjectCollectionPic {
    normal: Option<String>,
    large: Option<String>,
}

#[derive(Deserialize)]
struct SubjectCollectionCover {
    url: Option<String>,
}

#[derive(Deserialize)]
struct SubjectCollectionRating {
    value: Option<f32>,
}

fn parse_suggest(kind: MediaKind, body: &str) -> Result<Vec<CatalogHit>, TmdbError> {
    let parsed: Vec<SuggestHit> = serde_json::from_str(body)?;
    parsed
        .into_iter()
        .filter(|hit| matches_kind(kind, hit.kind.as_deref()))
        .map(|hit| to_hit(kind, hit.id, hit.title, hit.year, hit.img, None))
        .collect()
}

fn parse_popular(kind: MediaKind, body: &str) -> Result<Vec<CatalogHit>, TmdbError> {
    let parsed: PopularResponse = serde_json::from_str(body)?;
    parsed
        .subjects
        .into_iter()
        .map(|hit| to_hit(kind, hit.id, Some(hit.title), None, hit.cover, hit.rate))
        .collect()
}

fn parse_subject_collection(kind: MediaKind, body: &str) -> Result<Vec<CatalogHit>, TmdbError> {
    let parsed: SubjectCollectionResponse = serde_json::from_str(body)?;
    parsed
        .subject_collection_items
        .into_iter()
        .map(|item| {
            let poster = item
                .pic
                .as_ref()
                .and_then(|p| p.normal.clone().or_else(|| p.large.clone()))
                .or_else(|| item.cover.as_ref().and_then(|c| c.url.clone()))
                .or(item.cover_url);
            let year = item
                .year
                .or_else(|| {
                    item.card_subtitle
                        .as_deref()
                        .and_then(|sub| sub.split('/').next())
                        .map(|y| y.trim().to_string())
                })
                .and_then(|y| y.parse::<u16>().ok());
            let rating = item.rating.and_then(|r| r.value).filter(|&v| v > 0.0);
            Ok(CatalogHit {
                media: Media {
                    id: MediaId::new(),
                    kind,
                    title: item.title,
                    year,
                    original_title: None,
                    aliases: Vec::new(),
                    tmdb_id: None,
                    douban_id: Some(item.id),
                    tvdb_id: None,
                    bangumi_id: None,
                    anilist_id: None,
                },
                poster_path: poster.filter(|path| !path.is_empty()),
                backdrop_path: None,
                rating,
                overview: item.description.filter(|d| !d.is_empty()),
            })
        })
        .collect()
}

fn matches_kind(kind: MediaKind, raw: Option<&str>) -> bool {
    match (kind, raw) {
        (MediaKind::Movie, Some("tv")) => false,
        (MediaKind::Tv, Some("movie")) => false,
        (MediaKind::Tv, None) => false,
        _ => true,
    }
}

fn to_hit(
    kind: MediaKind,
    id: String,
    title: Option<String>,
    year: Option<String>,
    poster: Option<String>,
    rate: Option<String>,
) -> Result<CatalogHit, TmdbError> {
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
            douban_id: Some(id),
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        },
        poster_path: poster.filter(|path| !path.is_empty()),
        backdrop_path: None,
        rating: rate.and_then(|value| value.parse::<f32>().ok()),
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

/// 从豆瓣详情页 HTML 提取 (title, year, is_tv)。
/// 结构：`<h1><span property="v:itemreviewed">片名</span> <span class="year">(1994)</span></h1>`
/// 剧集页面含「集数:」字段。
fn parse_detail(body: &str) -> Option<(String, Option<u16>, bool)> {
    let title = body
        .split(r#"property="v:itemreviewed">"#)
        .nth(1)?
        .split('<')
        .next()?
        .trim()
        .to_string();
    if title.is_empty() {
        return None;
    }
    let year = body
        .split(r#"class="year">"#)
        .nth(1)?
        .split('<')
        .next()?
        .trim()
        .trim_start_matches('(')
        .trim_end_matches(')')
        .parse::<u16>()
        .ok();
    let is_tv = body.contains("集数:");
    Some((title, year, is_tv))
}

#[cfg(test)]
mod tests {
    use super::parse_detail;

    #[test]
    fn parses_movie_detail_page() {
        let html = r#"<html><body>
            <h1><span property="v:itemreviewed">肖申克的救赎</span> <span class="year">(1994)</span></h1>
            <div id="info"><span class="pl">类型:</span> 剧情 / 犯罪</div>
        </body></html>"#;
        let (title, year, is_tv) = parse_detail(html).unwrap();
        assert_eq!(title, "肖申克的救赎");
        assert_eq!(year, Some(1994));
        assert!(!is_tv);
    }

    #[test]
    fn parses_tv_detail_page() {
        let html = r#"<html><body>
            <h1><span property="v:itemreviewed">权力的游戏</span> <span class="year">(2011)</span></h1>
            <div id="info"><span class="pl">集数:</span> 73</div>
        </body></html>"#;
        let (title, year, is_tv) = parse_detail(html).unwrap();
        assert_eq!(title, "权力的游戏");
        assert_eq!(year, Some(2011));
        assert!(is_tv);
    }

    #[test]
    fn missing_title_is_none() {
        assert!(parse_detail("<html><body>no subject here</body></html>").is_none());
    }
}
