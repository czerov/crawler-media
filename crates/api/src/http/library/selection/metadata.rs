use super::super::preferred_row;
use crate::browse_file_read::BrowseFileRead;
use crate::store::CatalogCacheRow;
use domain::{LedgerRow, Media};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

/// 目录缓存的前缀索引。
///
/// `list_catalog_cache()` 按 `fetched_at DESC` 返回，因此原实现的「第一条命中」
/// 就是最新一条；构建索引时保持同样的优先顺序（先到者胜），避免语义漂移。
/// 原实现每个 Media 都线性扫整个 cache（含 7.6MB body），这里改为一次建索引。
pub(crate) struct CacheIndex<'a> {
    by_prefix: HashMap<String, &'a CatalogCacheRow>,
}

impl<'a> CacheIndex<'a> {
    pub(crate) fn build(cache: &'a [CatalogCacheRow]) -> Self {
        let mut by_prefix: HashMap<String, &'a CatalogCacheRow> = HashMap::new();
        for row in cache {
            if row.source != "tmdb" {
                continue;
            }
            let Some(base) = row.cache_key.split('?').next() else {
                continue;
            };
            by_prefix.entry(base.to_string()).or_insert(row);
        }
        Self { by_prefix }
    }

    fn get(&self, base: &str) -> Option<&'a CatalogCacheRow> {
        self.by_prefix.get(base).copied()
    }
}

#[derive(Default)]
pub(crate) struct BrowseMetadata {
    pub genres: Vec<String>,
    pub countries: Vec<String>,
    pub rating: Option<f64>,
    pub runtime: Option<u64>,
    pub language: Option<String>,
    pub release_date: Option<String>,
    pub scraped: bool,
}

/// NFO 读取与文件 stat 走注入的 [`BrowseFileRead`]：调用方在 Store 锁之外执行。
pub(crate) fn load(
    media: &Media,
    rows: &[LedgerRow],
    cache: &CacheIndex<'_>,
    files: &dyn BrowseFileRead,
) -> BrowseMetadata {
    let mut out = BrowseMetadata::default();
    if let Some(row) = preferred_row(rows) {
        let path = Path::new(&row.path);
        let candidates = crate::scrape_metadata::nfo_candidates(path, row, media.kind);
        let existing = candidates.into_iter().find(|path| {
            files
                .metadata(path)
                .map(|meta| meta.is_file())
                .unwrap_or(false)
        });
        if let Some(path) = existing {
            if let Some(nfo) = files.read_nfo(&path) {
                out.scraped = true;
                out.genres = nfo.genres.into_iter().map(|g| genre_id(&g)).collect();
                out.countries = nfo
                    .countries
                    .into_iter()
                    .map(|c| country_code(&c))
                    .collect();
                out.rating = nfo.rating.and_then(|v| v.parse().ok());
                out.runtime = nfo.runtime_minutes.and_then(|v| v.parse().ok());
                out.language = nfo.original_language.map(|v| v.to_lowercase());
                out.release_date = nfo.premiered;
            } else {
                tracing::warn!(path = %path.display(), "Library browse NFO unreadable or invalid");
            }
        }
    }
    if let Some(id) = &media.tmdb_id {
        let prefix = format!("/{}/{id}", media.kind.as_str());
        if let Some(entry) = cache.get(&prefix) {
            match serde_json::from_str::<Value>(&entry.body) {
                Ok(value) => supplement(&mut out, &value),
                Err(error) => {
                    tracing::warn!(cache_key = %entry.cache_key, %error, "Library cached metadata invalid")
                }
            }
        }
    }
    out
}

fn supplement(out: &mut BrowseMetadata, value: &Value) {
    out.scraped = true;
    if out.genres.is_empty() {
        out.genres = value["genres"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|g| g["id"].as_u64().map(|id| id.to_string()))
            .collect();
    }
    if out.countries.is_empty() {
        out.countries = value["origin_country"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_uppercase)
            .collect();
        if out.countries.is_empty() {
            out.countries = value["production_countries"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| c["iso_3166_1"].as_str())
                .map(str::to_uppercase)
                .collect();
        }
    }
    out.rating = out.rating.or_else(|| value["vote_average"].as_f64());
    out.runtime = out
        .runtime
        .or_else(|| value["runtime"].as_u64())
        .or_else(|| value["episode_run_time"].as_array()?.first()?.as_u64());
    if out.language.is_none() {
        out.language = value["original_language"].as_str().map(str::to_lowercase);
    }
    if out.release_date.is_none() {
        out.release_date = value["release_date"]
            .as_str()
            .or_else(|| value["first_air_date"].as_str())
            .map(str::to_string);
    }
}

fn genre_id(name: &str) -> String {
    let names = [
        ("Action", "动作", "28"),
        ("Adventure", "冒险", "12"),
        ("Animation", "动画", "16"),
        ("Comedy", "喜剧", "35"),
        ("Crime", "犯罪", "80"),
        ("Documentary", "纪录", "99"),
        ("Drama", "剧情", "18"),
        ("Family", "家庭", "10751"),
        ("Fantasy", "奇幻", "14"),
        ("History", "历史", "36"),
        ("Horror", "恐怖", "27"),
        ("Music", "音乐", "10402"),
        ("Mystery", "悬疑", "9648"),
        ("Romance", "爱情", "10749"),
        ("Science Fiction", "科幻", "878"),
        ("TV Movie", "电视电影", "10770"),
        ("Thriller", "惊悚", "53"),
        ("War", "战争", "10752"),
        ("Western", "西部", "37"),
        ("Action & Adventure", "动作冒险", "10759"),
        ("Kids", "儿童", "10762"),
        ("News", "新闻", "10763"),
        ("Reality", "真人秀", "10764"),
        ("Sci-Fi & Fantasy", "科幻奇幻", "10765"),
        ("Soap", "肥皂剧", "10766"),
        ("Talk", "脱口秀", "10767"),
        ("War & Politics", "战争政治", "10768"),
    ];
    names
        .iter()
        .find(|(en, zh, _)| name.eq_ignore_ascii_case(en) || name == *zh)
        .map(|(_, _, id)| (*id).to_string())
        .unwrap_or_else(|| name.to_string())
}
fn country_code(name: &str) -> String {
    match name.to_lowercase().as_str() {
        "united states of america" | "united states" | "美国" => "US",
        "united kingdom" | "英国" => "GB",
        "china" | "中国" => "CN",
        "japan" | "日本" => "JP",
        "south korea" | "韩国" => "KR",
        "france" | "法国" => "FR",
        "germany" | "德国" => "DE",
        "hong kong" | "香港" => "HK",
        "taiwan" | "台湾" => "TW",
        "canada" | "加拿大" => "CA",
        "australia" | "澳大利亚" => "AU",
        "india" | "印度" => "IN",
        _ => return name.to_uppercase(),
    }
    .to_string()
}
