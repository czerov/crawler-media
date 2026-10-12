//! GET /libraries/{id}/facets —— 十维筛选契约与「排除自身维」计数。
//!
//! 维度 D 的值 v 的 `count` = 满足「**除 D 以外**全部已选条件」且 D 取 v 的
//! **distinct Media** 数；候选全集来自该 Library 的全部可见 Media（含 count 0，
//! 前端置灰）。计数直接复用 [`LibraryFilter`] 的匹配语义（把 D 的键替换成 v 再
//! 匹配），因此与 items 接口的口径不可能分叉——`decades` 等档位不会二次实现。

use std::collections::HashMap;

use serde::Serialize;

use super::selection::{LibraryFilter, LibrarySelection};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub(crate) enum FacetDimension {
    Genre,
    Country,
    Decade,
    Watch,
    Rating,
    Runtime,
    Language,
    Resolution,
    Hdr,
    Stock,
}

const PRIMARY: [FacetDimension; 4] = [
    FacetDimension::Genre,
    FacetDimension::Country,
    FacetDimension::Decade,
    FacetDimension::Watch,
];

const ALL: [FacetDimension; 10] = [
    FacetDimension::Genre,
    FacetDimension::Country,
    FacetDimension::Decade,
    FacetDimension::Watch,
    FacetDimension::Rating,
    FacetDimension::Runtime,
    FacetDimension::Language,
    FacetDimension::Resolution,
    FacetDimension::Hdr,
    FacetDimension::Stock,
];

#[derive(Serialize)]
pub(crate) struct FacetValue {
    pub value: String,
    pub label: String,
    pub count: usize,
}

#[derive(Serialize)]
pub(crate) struct Facets {
    pub total: usize,
    pub genres: Vec<FacetValue>,
    pub countries: Vec<FacetValue>,
    pub decades: Vec<FacetValue>,
    pub watch: Vec<FacetValue>,
    pub ratings: Vec<FacetValue>,
    pub runtimes: Vec<FacetValue>,
    pub languages: Vec<FacetValue>,
    pub resolutions: Vec<FacetValue>,
    pub hdr: Vec<FacetValue>,
    pub stock: Vec<FacetValue>,
    /// 兼容：既有响应里有这个键，保持空数组，不破坏未知消费者。
    pub years: Vec<FacetValue>,
}

/// `tier`：`primary` 只填四个主维，其余空数组；`all` 十维全填。
///
/// **单趟累积**：先给每个 Media 算一次各维取值，再对每维做「其余维过滤 + 该维分组」
/// 的一次扫描（`O(维数 × Media + 候选值数)`），而不是每个候选值都全量匹配一遍。
pub(crate) fn build_facets(
    items: &[LibrarySelection],
    query: &HashMap<String, String>,
    tier: &str,
) -> Result<Facets, String> {
    // 参数校验与 items 完全一致：非法筛选值在这里变成 400。
    LibraryFilter::parse(query)?;
    let total = count_matching(items, query)?;
    let dimensions: &[FacetDimension] = if tier == "primary" { &PRIMARY } else { &ALL };
    // 1) 每个 Media 只算一次各维取值。
    let per_item: Vec<ItemFacetValues> = items.iter().map(ItemFacetValues::of).collect();
    // 2) 候选全集（含 count 0 的候选来自别的 Media）。
    let candidates = candidate_universe(&per_item, dimensions);
    // 3) 每维一次扫描：其余维过滤 + 该维分组累加。
    let mut built: HashMap<FacetDimension, Vec<FacetValue>> = HashMap::new();
    for dimension in dimensions {
        built.insert(
            *dimension,
            dimension_values(items, &per_item, &candidates, query, *dimension)?,
        );
    }
    let mut take = |dimension: FacetDimension| built.remove(&dimension).unwrap_or_default();
    Ok(Facets {
        total,
        genres: take(FacetDimension::Genre),
        countries: take(FacetDimension::Country),
        decades: take(FacetDimension::Decade),
        watch: take(FacetDimension::Watch),
        ratings: take(FacetDimension::Rating),
        runtimes: take(FacetDimension::Runtime),
        languages: take(FacetDimension::Language),
        resolutions: take(FacetDimension::Resolution),
        hdr: take(FacetDimension::Hdr),
        stock: take(FacetDimension::Stock),
        years: Vec::new(),
    })
}

/// 一个 Media 在各维上的取值（每维最多算一次）。
struct ItemFacetValues {
    values: [Vec<String>; 10],
}

impl ItemFacetValues {
    fn of(item: &LibrarySelection) -> Self {
        Self {
            values: std::array::from_fn(|index| ALL[index].candidates(item)),
        }
    }

    fn of_dimension(&self, dimension: FacetDimension) -> &[String] {
        &self.values[dimension.index()]
    }
}

/// 每维的候选全集：全部可见 Media 的并集，排序去重（含 count 0）。
fn candidate_universe(
    per_item: &[ItemFacetValues],
    dimensions: &[FacetDimension],
) -> HashMap<FacetDimension, Vec<String>> {
    let mut universe: HashMap<FacetDimension, Vec<String>> = HashMap::new();
    for dimension in dimensions {
        let mut values: Vec<String> = per_item
            .iter()
            .flat_map(|item| item.of_dimension(*dimension).iter().cloned())
            .collect();
        values.sort();
        values.dedup();
        universe.insert(*dimension, values);
    }
    universe
}

/// 单维计数：先把「除该维以外」的条件解析成一次 filter，再对全部 Media 扫一遍，
/// 命中者按该维取值累加。空候选维直接跳过。
fn dimension_values(
    items: &[LibrarySelection],
    per_item: &[ItemFacetValues],
    candidates: &HashMap<FacetDimension, Vec<String>>,
    query: &HashMap<String, String>,
    dimension: FacetDimension,
) -> Result<Vec<FacetValue>, String> {
    let candidates = candidates.get(&dimension).cloned().unwrap_or_default();
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let base = base_filter(query, dimension)?;
    let mut counts: HashMap<String, usize> =
        candidates.iter().map(|value| (value.clone(), 0)).collect();
    for (item, values) in items.iter().zip(per_item) {
        if !base.matches(item) {
            continue;
        }
        for value in values.of_dimension(dimension) {
            if let Some(count) = counts.get_mut(value) {
                *count += 1;
            }
        }
    }
    let mut built: Vec<FacetValue> = candidates
        .into_iter()
        .map(|value| FacetValue {
            count: counts.get(&value).copied().unwrap_or(0),
            label: dimension.label(&value),
            value,
        })
        .collect();
    built.sort_by(|left, right| {
        dimension
            .rank(&left.value)
            .cmp(&dimension.rank(&right.value))
            .then_with(|| right.count.cmp(&left.count))
            .then_with(|| left.value.cmp(&right.value))
    });
    Ok(built)
}

/// 「除该维以外」的过滤条件：去掉该维全部键（含别名）后解析。
fn base_filter(
    query: &HashMap<String, String>,
    dimension: FacetDimension,
) -> Result<LibraryFilter, String> {
    let mut scoped = query.clone();
    for key in dimension.keys() {
        scoped.remove(*key);
    }
    LibraryFilter::parse(&scoped)
}

/// 朴素参照实现（等价性测试用）：每个候选值都解析一次 filter 并全量匹配。
#[cfg(test)]
fn dimension_values_naive(
    items: &[LibrarySelection],
    query: &HashMap<String, String>,
    dimension: FacetDimension,
) -> Result<Vec<FacetValue>, String> {
    let mut candidates: Vec<String> = items
        .iter()
        .flat_map(|item| dimension.candidates(item))
        .collect();
    candidates.sort();
    candidates.dedup();
    let mut values = Vec::with_capacity(candidates.len());
    for value in candidates {
        let scoped = scope_query(query, dimension, &value);
        let count = count_matching(items, &scoped)?;
        values.push(FacetValue {
            label: dimension.label(&value),
            value,
            count,
        });
    }
    values.sort_by(|left, right| {
        dimension
            .rank(&left.value)
            .cmp(&dimension.rank(&right.value))
            .then_with(|| right.count.cmp(&left.count))
            .then_with(|| left.value.cmp(&right.value))
    });
    Ok(values)
}

/// 朴素参照实现（等价性测试用）：与 [`build_facets`] 同语义，但每候选一次全量匹配。
#[cfg(test)]
fn build_facets_naive(
    items: &[LibrarySelection],
    query: &HashMap<String, String>,
    tier: &str,
) -> Result<Facets, String> {
    LibraryFilter::parse(query)?;
    let total = count_matching(items, query)?;
    let dimensions: &[FacetDimension] = if tier == "primary" { &PRIMARY } else { &ALL };
    let mut built: HashMap<FacetDimension, Vec<FacetValue>> = HashMap::new();
    for dimension in dimensions {
        built.insert(
            *dimension,
            dimension_values_naive(items, query, *dimension)?,
        );
    }
    let mut take = |dimension: FacetDimension| built.remove(&dimension).unwrap_or_default();
    Ok(Facets {
        total,
        genres: take(FacetDimension::Genre),
        countries: take(FacetDimension::Country),
        decades: take(FacetDimension::Decade),
        watch: take(FacetDimension::Watch),
        ratings: take(FacetDimension::Rating),
        runtimes: take(FacetDimension::Runtime),
        languages: take(FacetDimension::Language),
        resolutions: take(FacetDimension::Resolution),
        hdr: take(FacetDimension::Hdr),
        stock: take(FacetDimension::Stock),
        years: Vec::new(),
    })
}

/// 把维度 D 的键整体替换成单个候选值（含别名键），其余维度原样保留。
#[cfg(test)]
fn scope_query(
    query: &HashMap<String, String>,
    dimension: FacetDimension,
    value: &str,
) -> HashMap<String, String> {
    let mut scoped = query.clone();
    for key in dimension.keys() {
        scoped.remove(*key);
    }
    scoped.insert(dimension.key().to_string(), value.to_string());
    scoped
}

fn count_matching(
    items: &[LibrarySelection],
    query: &HashMap<String, String>,
) -> Result<usize, String> {
    let filter = LibraryFilter::parse(query)?;
    Ok(items.iter().filter(|item| filter.matches(item)).count())
}

impl FacetDimension {
    fn key(self) -> &'static str {
        match self {
            Self::Genre => "g",
            Self::Country => "c",
            Self::Decade => "d",
            Self::Watch => "w",
            Self::Rating => "rating_gte",
            Self::Runtime => "rt",
            Self::Language => "lang",
            Self::Resolution => "res",
            Self::Hdr => "hdr",
            Self::Stock => "stock",
        }
    }

    /// 该维在查询串里可能出现的全部键（别名也要清掉）。
    fn keys(self) -> &'static [&'static str] {
        match self {
            Self::Genre => &["g"],
            Self::Country => &["c"],
            Self::Decade => &["d"],
            Self::Watch => &["w", "watch"],
            Self::Rating => &["rating_gte"],
            Self::Runtime => &["rt"],
            Self::Language => &["lang"],
            Self::Resolution => &["res", "resolutions"],
            Self::Hdr => &["hdr"],
            Self::Stock => &["stock"],
        }
    }

    fn index(self) -> usize {
        ALL.iter()
            .position(|dimension| *dimension == self)
            .unwrap_or_default()
    }

    /// 与 `LibraryFilter` 的比较口径对齐：country 大小写不敏感、language 小写，
    /// 其余维是精确比较。分组键用同一口径，计数才与逐候选匹配逐字一致。
    fn canonical(self, value: &str) -> String {
        match self {
            Self::Country => value.to_uppercase(),
            Self::Language => value.to_lowercase(),
            _ => value.to_string(),
        }
    }

    /// 该 item 在这个维度上占的候选值（决定候选全集，含 count 0 的候选来自别的 item）。
    /// 取值先过 [`FacetDimension::canonical`]，分组键与 `LibraryFilter` 的比较口径一致。
    fn candidates(self, item: &LibrarySelection) -> Vec<String> {
        self.raw_candidates(item)
            .into_iter()
            .map(|value| self.canonical(&value))
            .collect()
    }

    fn raw_candidates(self, item: &LibrarySelection) -> Vec<String> {
        match self {
            Self::Genre => item.metadata.genres.clone(),
            Self::Country => item.metadata.countries.clone(),
            Self::Decade => decade_of(item.media.year)
                .map(str::to_string)
                .into_iter()
                .collect(),
            Self::Watch => {
                let mut values = vec![watch_key(item).to_string()];
                if item.favorite {
                    values.push("favorite".into());
                }
                values
            }
            Self::Rating => [6_u32, 7, 8, 9]
                .into_iter()
                .filter(|threshold| {
                    item.metadata
                        .rating
                        .is_some_and(|rating| rating >= f64::from(*threshold))
                })
                .map(|threshold| threshold.to_string())
                .collect(),
            Self::Runtime => runtime_key(item.metadata.runtime)
                .map(str::to_string)
                .into_iter()
                .collect(),
            Self::Language => item.metadata.language.clone().into_iter().collect(),
            Self::Resolution => item
                .rows
                .iter()
                .filter_map(|row| row.resolution.clone())
                .collect(),
            Self::Hdr => vec![if item_has_hdr(item) { "true" } else { "false" }.to_string()],
            Self::Stock => {
                let mut values = Vec::new();
                if item.missing {
                    values.push("missing".into());
                }
                if !item.metadata.scraped {
                    values.push("unscraped".into());
                }
                values
            }
        }
    }

    fn label(self, value: &str) -> String {
        match self {
            Self::Genre => lookup(GENRE_LABELS, value),
            Self::Country => lookup(COUNTRY_LABELS, value),
            Self::Watch => match value {
                "unwatched" => "未观看".into(),
                "watching" => "在看".into(),
                "played" => "已看完".into(),
                "favorite" => "收藏".into(),
                _ => value.to_string(),
            },
            Self::Rating => format!("{value}分以上"),
            Self::Runtime => match value {
                "lte60" => "60分钟以内".into(),
                "60to90" => "60-90分钟".into(),
                "90to120" => "90-120分钟".into(),
                "gt120" => "120分钟以上".into(),
                _ => value.to_string(),
            },
            Self::Hdr => match value {
                "true" => "HDR".into(),
                _ => "SDR".into(),
            },
            Self::Stock => match value {
                "missing" => "文件失联".into(),
                "unscraped" => "未刮削".into(),
                _ => value.to_string(),
            },
            Self::Decade | Self::Language | Self::Resolution => value.to_string(),
        }
    }

    /// 固定档位维度按语义顺序；数据驱动维度按 count 倒序（rank 恒 0）。
    fn rank(self, value: &str) -> usize {
        let canonical: &[&str] = match self {
            Self::Decade => &["earlier", "1990s", "2000s", "2010s", "2020s"],
            Self::Watch => &["unwatched", "watching", "played", "favorite"],
            Self::Rating => &["6", "7", "8", "9"],
            Self::Runtime => &["lte60", "60to90", "90to120", "gt120"],
            Self::Hdr => &["true", "false"],
            Self::Stock => &["missing", "unscraped"],
            _ => return 0,
        };
        canonical
            .iter()
            .position(|candidate| *candidate == value)
            .unwrap_or(usize::MAX)
    }
}

/// 与 `LibraryFilter` 的 `d` 档严格一致：未知年份与 >=2030 不产生任何候选。
fn decade_of(year: Option<u16>) -> Option<&'static str> {
    let year = year?;
    match year {
        2020..=2029 => Some("2020s"),
        2010..=2019 => Some("2010s"),
        2000..=2009 => Some("2000s"),
        1990..=1999 => Some("1990s"),
        0..=1989 => Some("earlier"),
        _ => None,
    }
}

fn runtime_key(runtime: Option<u64>) -> Option<&'static str> {
    let runtime = runtime?;
    Some(match runtime {
        0..=60 => "lte60",
        61..=90 => "60to90",
        91..=120 => "90to120",
        _ => "gt120",
    })
}

/// 与 items 的观看匹配一致：未看 / 在看 / 已看完是一个划分，收藏正交。
fn watch_key(item: &LibrarySelection) -> &'static str {
    if item.played {
        "played"
    } else if item.seen {
        "watching"
    } else {
        "unwatched"
    }
}

fn item_has_hdr(item: &LibrarySelection) -> bool {
    item.rows
        .iter()
        .any(|row| row.hdr.as_deref().is_some_and(|value| !value.is_empty()))
}

fn lookup(table: &[(&str, &str)], value: &str) -> String {
    table
        .iter()
        .find(|(key, _)| *key == value)
        .map(|(_, label)| (*label).to_string())
        .unwrap_or_else(|| value.to_string())
}

const GENRE_LABELS: &[(&str, &str)] = &[
    ("28", "动作"),
    ("12", "冒险"),
    ("16", "动画"),
    ("35", "喜剧"),
    ("80", "犯罪"),
    ("99", "纪录"),
    ("18", "剧情"),
    ("10751", "家庭"),
    ("14", "奇幻"),
    ("36", "历史"),
    ("27", "恐怖"),
    ("10402", "音乐"),
    ("9648", "悬疑"),
    ("10749", "爱情"),
    ("878", "科幻"),
    ("10770", "电视电影"),
    ("53", "惊悚"),
    ("10752", "战争"),
    ("37", "西部"),
    ("10759", "动作冒险"),
    ("10762", "儿童"),
    ("10763", "新闻"),
    ("10764", "真人秀"),
    ("10765", "科幻奇幻"),
    ("10766", "肥皂剧"),
    ("10767", "脱口秀"),
    ("10768", "战争政治"),
];

const COUNTRY_LABELS: &[(&str, &str)] = &[
    ("US", "美国"),
    ("GB", "英国"),
    ("CN", "中国大陆"),
    ("JP", "日本"),
    ("KR", "韩国"),
    ("FR", "法国"),
    ("DE", "德国"),
    ("HK", "中国香港"),
    ("TW", "中国台湾"),
    ("CA", "加拿大"),
    ("AU", "澳大利亚"),
    ("IN", "印度"),
    ("IT", "意大利"),
    ("ES", "西班牙"),
    ("RU", "俄罗斯"),
    ("TH", "泰国"),
    ("SG", "新加坡"),
    ("NZ", "新西兰"),
    ("SE", "瑞典"),
    ("DK", "丹麦"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};

    fn selection(
        index: usize,
        year: Option<u16>,
        genres: &[&str],
        countries: &[&str],
        language: Option<&str>,
        rating: Option<f64>,
        runtime: Option<u64>,
        resolutions: &[&str],
        hdr: Option<&str>,
        scraped: bool,
        missing: bool,
        played: bool,
        seen: bool,
        favorite: bool,
    ) -> LibrarySelection {
        let media_id = MediaId::new();
        let rows: Vec<LedgerRow> = resolutions
            .iter()
            .enumerate()
            .map(|(file, resolution)| LedgerRow {
                id: LedgerId::new(),
                media_id,
                path: format!("/fixture/{index}/file-{file}.mkv"),
                season: None,
                episode: None,
                resolution: Some((*resolution).to_string()),
                codec: Some("h264".into()),
                hdr: hdr.map(str::to_string),
                quality_source: QualitySource::Probe,
                confidence: Confidence::High,
                filter_score: None,
            })
            .collect();
        let mut item = LibrarySelection {
            media: Media {
                id: media_id,
                kind: MediaKind::Movie,
                title: format!("Fixture {index}"),
                year,
                original_title: None,
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            },
            rows,
            metadata: Default::default(),
            favorite,
            played,
            seen,
            missing,
            added_at: None,
            size_bytes: None,
            last_played_at: None,
        };
        item.metadata.genres = genres.iter().map(|value| (*value).to_string()).collect();
        item.metadata.countries = countries.iter().map(|value| (*value).to_string()).collect();
        item.metadata.rating = rating;
        item.metadata.runtime = runtime;
        item.metadata.language = language.map(str::to_string);
        item.metadata.scraped = scraped;
        item
    }

    /// 覆盖每个维度，含大小写不敏感的 country/language 变体与多值维度。
    fn fixture() -> Vec<LibrarySelection> {
        vec![
            selection(
                0,
                Some(2024),
                &["28"],
                &["US"],
                Some("en"),
                Some(8.4),
                Some(95),
                &["2160p"],
                Some("HDR"),
                true,
                false,
                false,
                false,
                true,
            ),
            selection(
                1,
                Some(2015),
                &["28", "18"],
                &["us"],
                Some("EN"),
                Some(6.1),
                Some(61),
                &["1080p"],
                None,
                true,
                false,
                true,
                true,
                false,
            ),
            selection(
                2,
                Some(1995),
                &["18"],
                &["JP"],
                Some("ja"),
                Some(7.9),
                Some(121),
                &["720p"],
                Some("hdr10"),
                true,
                false,
                false,
                true,
                false,
            ),
            selection(
                3,
                None,
                &[],
                &[],
                None,
                None,
                None,
                &[],
                None,
                false,
                true,
                false,
                false,
                false,
            ),
            selection(
                4,
                Some(2030),
                &["16"],
                &["CN"],
                Some("zh"),
                Some(9.1),
                Some(45),
                &["1080p", "2160p"],
                Some("HDR"),
                false,
                true,
                false,
                false,
                false,
            ),
            selection(
                5,
                Some(2003),
                &["28"],
                &["US", "GB"],
                Some("en"),
                Some(7.0),
                Some(90),
                &["1080p"],
                None,
                true,
                false,
                false,
                true,
                false,
            ),
        ]
    }

    fn cases() -> Vec<(HashMap<String, String>, &'static str)> {
        let mut cases = vec![(HashMap::new(), "all"), (HashMap::new(), "primary")];
        for (key, value) in [
            ("g", "28"),
            ("c", "US"),
            ("d", "2010s"),
            ("w", "unwatched"),
            ("w", "favorite"),
            ("rating_gte", "7"),
            ("rt", "60to90"),
            ("lang", "en"),
            ("res", "1080p"),
            ("hdr", "true"),
            ("stock", "missing"),
            ("stock", "unscraped"),
        ] {
            cases.push((HashMap::from([(key.to_string(), value.to_string())]), "all"));
        }
        cases.push((
            HashMap::from([
                ("g".to_string(), "28,18".to_string()),
                ("c".to_string(), "US,JP".to_string()),
                ("w".to_string(), "unwatched".to_string()),
                ("rating_gte".to_string(), "7".to_string()),
            ]),
            "all",
        ));
        cases.push((
            HashMap::from([
                ("d".to_string(), "2010s,1990s".to_string()),
                ("rt".to_string(), "90to120,gt120".to_string()),
                ("hdr".to_string(), "false".to_string()),
            ]),
            "primary",
        ));
        cases
    }

    #[test]
    fn single_pass_matches_the_naive_reference() {
        let items = fixture();
        for (query, tier) in cases() {
            let fast = build_facets(&items, &query, tier).expect("single pass");
            let naive = build_facets_naive(&items, &query, tier).expect("naive");
            assert_eq!(
                serde_json::to_value(&fast).unwrap(),
                serde_json::to_value(&naive).unwrap(),
                "query={query:?} tier={tier}"
            );
        }
    }
}
