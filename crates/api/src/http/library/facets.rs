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
pub(crate) fn build_facets(
    items: &[LibrarySelection],
    query: &HashMap<String, String>,
    tier: &str,
) -> Result<Facets, String> {
    // 参数校验与 items 完全一致：非法筛选值在这里变成 400。
    LibraryFilter::parse(query)?;
    let total = count_matching(items, query)?;
    let dimensions: &[FacetDimension] = if tier == "primary" { &PRIMARY } else { &ALL };
    let mut built: HashMap<FacetDimension, Vec<FacetValue>> = HashMap::new();
    for dimension in dimensions {
        built.insert(*dimension, dimension_values(items, query, *dimension)?);
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

fn dimension_values(
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

/// 把维度 D 的键整体替换成单个候选值（含别名键），其余维度原样保留。
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

    /// 该 item 在这个维度上占的候选值（决定候选全集，含 count 0 的候选来自别的 item）。
    fn candidates(self, item: &LibrarySelection) -> Vec<String> {
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
