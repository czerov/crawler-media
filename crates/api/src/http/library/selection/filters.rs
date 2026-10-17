use super::LibrarySelection;
use library::WatchTier;
use std::{cmp::Ordering, collections::HashMap};

/// 无 watch、无过滤、且排序有索引列时可直接用 SQL 分页的顺序。
///
/// 其余查询（任何过滤/watch/非常用排序）必须先取整库轻候选完整筛选排序，
/// 再读本页重事实：选页后过滤会漏项。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IndexPageOrder {
    Title,
    AddedAt,
    ReleaseDate,
}

pub(crate) struct LibraryFilter {
    lists: HashMap<String, Vec<String>>,
    watch: Option<String>,
    /// 未看优先：观看分级（未看 → 在看 → 已看完）参与排序，**不筛掉任何条目**
    /// （首页库行与「我的收藏」行的口径，默认关）。墙上用户手选的「未观看」是严格筛，
    /// 不带这个开关。
    unwatched_first: bool,
    hdr: Option<bool>,
    rating: Option<f64>,
    identity: Option<String>,
    pub sort: String,
    desc: bool,
}

impl LibraryFilter {
    pub fn parse(query: &HashMap<String, String>) -> Result<Self, String> {
        let mut lists: HashMap<String, Vec<String>> = HashMap::new();
        for key in ["g", "c", "d", "rt", "lang", "res", "stock"] {
            let raw = query
                .get(key)
                .or_else(|| (key == "res").then(|| query.get("resolutions")).flatten());
            lists.insert(
                key.to_string(),
                raw.map(|s| {
                    s.split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            );
        }
        let hdr = query
            .get("hdr")
            .map(|s| s.parse::<bool>())
            .transpose()
            .map_err(|_| "hdr must be true or false")?;
        let rating = query
            .get("rating_gte")
            .map(|s| s.parse::<f64>())
            .transpose()
            .map_err(|_| "rating_gte must be numeric")?;
        if rating.is_some_and(|v| !v.is_finite() || !(0.0..=10.0).contains(&v)) {
            return Err("rating_gte must be between 0 and 10".into());
        }
        let watch = query.get("w").or_else(|| query.get("watch")).cloned();
        if watch.as_deref().is_some_and(|w| {
            ![
                "favorite",
                "seen",
                "watching",
                "played",
                "watched",
                "unwatched",
                "unplayed",
                "unseen",
            ]
            .contains(&w)
        }) {
            return Err("invalid watch filter".into());
        }
        let unwatched_first = query
            .get("unwatched_first")
            .map(|s| s.parse::<bool>())
            .transpose()
            .map_err(|_| "unwatched_first must be true or false")?
            .unwrap_or(false);
        let requested = query.get("sort").cloned().unwrap_or_else(|| "title".into());
        // Every key the wall can ask for must stay reachable; an unrecognised one
        // falls back to the stable title order instead of failing the request.
        let sort = if SORTS.contains(&requested.as_str()) {
            requested
        } else {
            tracing::warn!(sort = %requested, "未知的 Library 排序，回退到标题顺序");
            "title".into()
        };
        for (key, allowed) in [
            ("d", &["2020s", "2010s", "2000s", "1990s", "earlier"][..]),
            ("rt", &["lte60", "60to90", "90to120", "gt120"][..]),
            ("stock", &["missing", "unscraped"][..]),
        ] {
            if lists[key].iter().any(|v| !allowed.contains(&v.as_str())) {
                return Err(format!("invalid {key} filter"));
            }
        }
        let desc = match query.get("order").map(|s| s.as_str()) {
            Some("desc") => true,
            Some("asc") => false,
            _ => matches!(
                sort.as_str(),
                "added_at" | "release_date" | "rating" | "size" | "last_played"
            ),
        };
        Ok(Self {
            lists,
            hdr,
            rating,
            watch,
            unwatched_first,
            sort,
            identity: query.get("identity").cloned(),
            desc,
        })
    }

    fn any(&self, key: &str, mut predicate: impl FnMut(&str) -> bool) -> bool {
        self.lists[key].is_empty() || self.lists[key].iter().any(|value| predicate(value))
    }

    /// 观看状态那一档：筛选条里的「未观看 / 在看 / 已看完」三档由它回答，
    /// 与 [`LibraryFilter::compare`] 里的「未看优先」分级用的是同一套判定。
    pub fn matches_watch(&self, item: &LibrarySelection) -> bool {
        match self.watch.as_deref() {
            Some("favorite") => item.favorite,
            Some("seen") => item.seen,
            Some("watching") => item.seen && !item.played,
            Some("played" | "watched") => item.played,
            Some("unwatched" | "unplayed" | "unseen") => !item.seen,
            _ => true,
        }
    }

    pub fn matches(&self, item: &LibrarySelection) -> bool {
        self.matches_ignoring_watch(item) && self.matches_watch(item)
    }

    fn matches_ignoring_watch(&self, item: &LibrarySelection) -> bool {
        let meta = &item.metadata;
        let identity = match self.identity.as_deref() {
            Some("provisional") => item.light.provisional,
            Some("confirmed") => !item.light.provisional,
            _ => true,
        };
        identity
            && self.any("g", |g| meta.genres.iter().any(|v| v == g))
            && self.any("c", |c| {
                meta.countries.iter().any(|v| v.eq_ignore_ascii_case(c))
            })
            && self.any("lang", |lang| {
                meta.language
                    .as_deref()
                    .is_some_and(|v| v.eq_ignore_ascii_case(lang))
            })
            && self.any("d", |d| {
                item.media.year.is_some_and(|y| match d {
                    "earlier" => y < 1990,
                    _ => d
                        .strip_suffix('s')
                        .and_then(|d| d.parse::<u16>().ok())
                        .is_some_and(|start| y >= start && y < start + 10),
                })
            })
            && self.any("rt", |rt| {
                meta.runtime.is_some_and(|r| match rt {
                    "lte60" => r <= 60,
                    "60to90" => r > 60 && r <= 90,
                    "90to120" => r > 90 && r <= 120,
                    "gt120" => r > 120,
                    _ => false,
                })
            })
            && self.any("res", |res| {
                item.light.resolutions.iter().any(|r| r.as_str() == res)
            })
            && self.any("stock", |stock| match stock {
                "missing" => item.missing,
                "unscraped" => !meta.scraped,
                _ => false,
            })
            && self.hdr.is_none_or(|want| item.light.hdr == want)
            && self
                .rating
                .is_none_or(|min| meta.rating.is_some_and(|rating| rating >= min))
    }

    /// Watch state is a per-unit store read; only requests that depend on it pay
    /// that cost, so ordinary wall browsing keeps its previous profile.
    pub fn needs_watch_state(&self) -> bool {
        self.watch.is_some() || self.sort == "last_played" || self.unwatched_first
    }

    pub fn compare(&self, a: &LibrarySelection, b: &LibrarySelection) -> Ordering {
        // 未看优先：分级在前，段内仍是这一档自己的排序（「最近添加」就是入库时间倒序）。
        // 只排不筛——筛完只剩一张卡的小库看起来像坏了（见 library::latest 的说明）。
        let tier = if self.unwatched_first {
            WatchTier::of(a.seen, a.played).cmp(&WatchTier::of(b.seen, b.played))
        } else {
            Ordering::Equal
        };
        let primary = match self.sort.as_str() {
            "added_at" => a.added_at.cmp(&b.added_at),
            "release_date" | "release_date_asc" => a.light.release_date.cmp(&b.light.release_date),
            "rating" => a
                .metadata
                .rating
                .unwrap_or(-1.0)
                .total_cmp(&b.metadata.rating.unwrap_or(-1.0)),
            "runtime" => a
                .metadata
                .runtime
                .unwrap_or(0)
                .cmp(&b.metadata.runtime.unwrap_or(0)),
            "size" => a.size_bytes.unwrap_or(0).cmp(&b.size_bytes.unwrap_or(0)),
            "last_played" => a
                .last_played_at
                .unwrap_or(0)
                .cmp(&b.last_played_at.unwrap_or(0)),
            "resolution" => a.light.resolution_rank.cmp(&b.light.resolution_rank),
            // 稳定的伪随机序：同一 Media 每次落同一位置，翻页/EOF 才不会重复或漏项。
            "random" => a.light.shuffle_key.cmp(&b.light.shuffle_key),
            _ => a.media.title.cmp(&b.media.title),
        };
        let primary = if self.desc {
            primary.reverse()
        } else {
            primary
        };
        tier.then(primary)
            .then_with(|| a.media.id.to_string().cmp(&b.media.id.to_string()))
    }

    /// 无 watch、无过滤且排序有索引列 → 可直接 SQL 分页的顺序。
    ///
    /// `sort=release_date_asc` 与 `release_date` 走同一个排序键（`desc` 的默认值
    /// 不同），因此两者共用 `ReleaseDate`。
    pub(crate) fn index_page_order(&self) -> Option<IndexPageOrder> {
        if self.watch.is_some()
            || self.unwatched_first
            || self.hdr.is_some()
            || self.rating.is_some()
            || self.identity.is_some()
            || self.lists.values().any(|values| !values.is_empty())
        {
            return None;
        }
        match self.sort.as_str() {
            "title" => Some(IndexPageOrder::Title),
            "added_at" => Some(IndexPageOrder::AddedAt),
            "release_date" | "release_date_asc" => Some(IndexPageOrder::ReleaseDate),
            _ => None,
        }
    }

    /// 排序方向（`order=asc|desc` 或该排序的默认方向），SQL 索引分页与
    /// [`Self::compare`] 必须用同一个值。
    pub(crate) fn descending(&self) -> bool {
        self.desc
    }
}

/// 未知或系统临时档（`probing`）的排序是视频探测状态，只有 ApiState 能回答，
/// 这里按标题稳定回退——不崩、不重复、不静默改变名单，只是顺序退回默认。
const SORTS: &[&str] = &[
    "title",
    "added_at",
    "release_date",
    "release_date_asc",
    "rating",
    "runtime",
    "size",
    "last_played",
    "resolution",
    "probing",
    "random",
];
