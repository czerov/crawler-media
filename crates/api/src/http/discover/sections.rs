//! Fixed discover section orchestration: the section metadata tables and the
//! four source-specific wall builders (TMDB movie/TV, Douban movie/TV), in the
//! source and section order the wall renders.

use serde_json::Value;

use crate::catalog::Catalog;

use super::row;

/// TMDB discover query for a section id, when the section is paginable.
/// `None` = hero/featured only (no full listing). Owned `String`: sections are
/// fixed at compile time, but the composed query must not be leaked.
pub(super) fn section_query(kind: &str, id: &str) -> Option<String> {
    let (base, sort) = match id {
        "trending-day" => ("", "sort_by=popularity.desc"),
        "now-playing" if kind == "movie" => ("", "sort_by=popularity.desc&with_release_type=3"),
        "upcoming" if kind == "movie" => ("", "sort_by=popularity.desc&with_release_type=2"),
        "airing-today" if kind == "tv" => ("", "sort_by=popularity.desc"),
        "on-the-air" if kind == "tv" => ("", "sort_by=popularity.desc"),
        "popular" => ("", "sort_by=popularity.desc"),
        "top-rated" => ("", "sort_by=vote_average.desc"),
        "recent-acclaimed" => (
            if kind == "movie" {
                "primary_release_date.gte=2024-09-20"
            } else {
                "first_air_date.gte=2024-09-20"
            },
            "sort_by=vote_average.desc&vote_count.gte=100",
        ),
        "this-year" if kind == "movie" => ("primary_release_year=2026", "sort_by=popularity.desc"),
        "short-runtime" if kind == "movie" => ("with_runtime.lte=90", "sort_by=vote_average.desc"),
        "chinese" => ("with_original_language=zh", "sort_by=popularity.desc"),
        "japanese" if kind == "movie" => ("with_original_language=ja", "sort_by=popularity.desc"),
        "korean" if kind == "movie" => ("with_original_language=ko", "sort_by=popularity.desc"),
        "us" if kind == "tv" => ("with_origin_country=US", "sort_by=popularity.desc"),
        "japanese" if kind == "tv" => ("with_original_language=ja", "sort_by=popularity.desc"),
        "korean" if kind == "tv" => ("with_original_language=ko", "sort_by=popularity.desc"),
        "netflix" if kind == "tv" => ("with_networks=213", "sort_by=popularity.desc"),
        "hbo" if kind == "tv" => ("with_networks=49", "sort_by=popularity.desc"),
        "scifi" if kind == "movie" => ("with_genres=878", "sort_by=popularity.desc"),
        "scifi-fantasy" if kind == "tv" => ("with_genres=10765", "sort_by=popularity.desc"),
        "scifi" | "scifi-fantasy" => ("with_genres=878", "sort_by=popularity.desc"),
        "action" => ("with_genres=28", "sort_by=popularity.desc"),
        "thriller" => ("with_genres=53", "sort_by=popularity.desc"),
        "crime-mystery" => ("with_genres=80", "sort_by=popularity.desc"),
        "comedy" => ("with_genres=35", "sort_by=popularity.desc"),
        "romance" => ("with_genres=10749", "sort_by=popularity.desc"),
        "horror" => ("with_genres=27", "sort_by=popularity.desc"),
        "animation" => ("with_genres=16", "sort_by=popularity.desc"),
        "documentary" => ("with_genres=99", "sort_by=popularity.desc"),
        "reality" => ("with_genres=10764", "sort_by=popularity.desc"),
        _ => return None,
    };
    if base.is_empty() {
        Some(sort.to_string())
    } else {
        Some(format!("{base}&{sort}"))
    }
}

/// Whether a section id supports a full listing page.
/// Whether a section id supports a full listing page.
///
/// TMDB sources: every discover-backed section is paginable (hero 除外).
/// Douban: only sections Douban's search_subjects can paginate get a
/// listing link — 豆瓣高分 / Top250 类。其他分区（实时热门/类型榜…）豆瓣
/// 无法翻页，不给入口，避免点进去却落到 TMDB 数据。
pub(super) fn supports_full_listing(source: &str, kind: &str, id: &str) -> bool {
    if source == "douban" {
        return id == "top-rated"; // 豆瓣高分经典
    }
    section_query(kind, id).is_some()
}

/// Chinese title for a section id (full listing page header).
fn section_title(kind: &str, id: &str) -> Option<&'static str> {
    let title = match id {
        "trending-day" => "今日热榜",
        "now-playing" if kind == "movie" => "正在热映",
        "upcoming" if kind == "movie" => "即将上映",
        "airing-today" if kind == "tv" => "今日首播",
        "on-the-air" if kind == "tv" => "正在播出",
        "popular" if kind == "movie" => "热门电影",
        "popular" => "热门剧集",
        "top-rated" => "高分经典",
        "recent-acclaimed" => "近两年高口碑",
        "this-year" if kind == "movie" => "今年新片",
        "short-runtime" if kind == "movie" => "90 分钟以内",
        "chinese" => "华语佳片",
        "japanese" if kind == "movie" => "日本电影",
        "korean" if kind == "movie" => "韩国电影",
        "us" if kind == "tv" => "美国剧集",
        "japanese" if kind == "tv" => "日本剧集",
        "korean" if kind == "tv" => "韩国剧集",
        "netflix" => "Netflix 剧集",
        "hbo" => "HBO 剧集",
        "scifi" if kind == "movie" => "科幻巨制",
        "scifi-fantasy" if kind == "tv" => "科幻与奇幻",
        "scifi" | "scifi-fantasy" => "科幻巨制",
        "action" => "动作与冒险",
        "thriller" => "悬疑惊悚",
        "crime-mystery" => "犯罪悬疑",
        "comedy" => "喜剧佳作",
        "romance" => "爱情电影",
        "horror" => "恐怖片",
        "animation" if kind == "movie" => "动画电影",
        "animation" => "动画剧集",
        "documentary" => "纪录片",
        "reality" => "真人秀",
        _ => return None,
    };
    Some(title)
}

/// Per-kind full-listing titles (used by the listing page header).
pub(super) fn collection_title(kind: &str, id: &str) -> String {
    section_title(kind, id)
        .map(str::to_string)
        .unwrap_or_else(|| id.to_string())
}

pub(super) fn movie_sections(_kind: &str, catalog: &dyn Catalog) -> Vec<Value> {
    vec![
        row(
            catalog.source_name(),
            "movie",
            "featured-weekly",
            "本周精选",
            "hero",
            catalog.trending_movie(),
        ),
        row(
            catalog.source_name(),
            "movie",
            "trending-day",
            "今日热榜",
            "ranked-row",
            catalog.trending_movie(),
        ),
        row(
            catalog.source_name(),
            "movie",
            "now-playing",
            "正在热映",
            "poster-row",
            catalog.now_playing_movie(),
        ),
        row(
            catalog.source_name(),
            "movie",
            "upcoming",
            "即将上映",
            "poster-row",
            catalog.upcoming_movie(),
        ),
        row(
            catalog.source_name(),
            "movie",
            "popular",
            "热门电影",
            "poster-row",
            catalog.popular_movie(),
        ),
        row(
            catalog.source_name(),
            "movie",
            "recent-acclaimed",
            "近两年高口碑",
            "poster-row",
            catalog.filtered_movie(
                "primary_release_date.gte=2024-09-20&sort_by=vote_average.desc&vote_count.gte=500",
            ),
        ),
        row(
            catalog.source_name(),
            "movie",
            "this-year",
            "今年新片",
            "poster-row",
            catalog.filtered_movie("primary_release_year=2026&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "movie",
            "short-runtime",
            "90 分钟以内",
            "poster-row",
            catalog.filtered_movie("with_runtime.lte=90&sort_by=vote_average.desc"),
        ),
        row(
            catalog.source_name(),
            "movie",
            "top-rated",
            "高分经典",
            "poster-row",
            catalog.top_rated_movie(),
        ),
        row(
            catalog.source_name(),
            "movie",
            "chinese",
            "华语佳片",
            "poster-row",
            catalog.filtered_movie("with_original_language=zh&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "movie",
            "japanese",
            "日本电影",
            "poster-row",
            catalog.filtered_movie("with_original_language=ja&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "movie",
            "korean",
            "韩国电影",
            "poster-row",
            catalog.filtered_movie("with_original_language=ko&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "movie",
            "scifi",
            "科幻巨制",
            "poster-row",
            catalog.filtered_movie("with_genres=878&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "movie",
            "action",
            "动作与冒险",
            "poster-row",
            catalog.filtered_movie("with_genres=28&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "movie",
            "thriller",
            "悬疑惊悚",
            "poster-row",
            catalog.filtered_movie("with_genres=53&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "movie",
            "comedy",
            "喜剧佳作",
            "poster-row",
            catalog.filtered_movie("with_genres=35&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "movie",
            "romance",
            "爱情电影",
            "poster-row",
            catalog.filtered_movie("with_genres=10749&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "movie",
            "horror",
            "恐怖片",
            "poster-row",
            catalog.filtered_movie("with_genres=27&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "movie",
            "animation",
            "动画电影",
            "poster-row",
            catalog.filtered_movie("with_genres=16&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "movie",
            "documentary",
            "纪录片",
            "poster-row",
            catalog.filtered_movie("with_genres=99&sort_by=popularity.desc"),
        ),
    ]
    .into_iter()
    .filter(|section| !section.is_null())
    .collect()
}

pub(super) fn tv_sections(_kind: &str, catalog: &dyn Catalog) -> Vec<Value> {
    vec![
        row(
            catalog.source_name(),
            "tv",
            "featured-weekly",
            "本周精选",
            "hero",
            catalog.trending_tv(),
        ),
        row(
            catalog.source_name(),
            "tv",
            "trending-day",
            "今日热榜",
            "ranked-row",
            catalog.trending_tv(),
        ),
        row(
            catalog.source_name(),
            "tv",
            "airing-today",
            "今日更新",
            "poster-row",
            catalog.airing_today_tv(),
        ),
        row(
            catalog.source_name(),
            "tv",
            "on-the-air",
            "本周更新",
            "poster-row",
            catalog.on_the_air_tv(),
        ),
        row(
            catalog.source_name(),
            "tv",
            "popular",
            "热门剧集",
            "poster-row",
            catalog.popular_tv(),
        ),
        row(
            catalog.source_name(),
            "tv",
            "recent-acclaimed",
            "近两年高口碑",
            "poster-row",
            catalog.filtered_tv(
                "first_air_date.gte=2024-09-20&sort_by=vote_average.desc&vote_count.gte=300",
            ),
        ),
        row(
            catalog.source_name(),
            "tv",
            "completed-miniseries",
            "已完结迷你剧",
            "poster-row",
            catalog.filtered_tv("with_type=2&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "tv",
            "top-rated",
            "高分神剧",
            "poster-row",
            catalog.top_rated_tv(),
        ),
        row(
            catalog.source_name(),
            "tv",
            "chinese",
            "华语剧集",
            "poster-row",
            catalog.filtered_tv("with_original_language=zh&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "tv",
            "us",
            "热门美剧",
            "poster-row",
            catalog.filtered_tv("with_origin_country=US&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "tv",
            "japanese",
            "热门日剧",
            "poster-row",
            catalog.filtered_tv("with_original_language=ja&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "tv",
            "korean",
            "热门韩剧",
            "poster-row",
            catalog.filtered_tv("with_original_language=ko&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "tv",
            "netflix",
            "Netflix 出品",
            "poster-row",
            catalog.filtered_tv("with_networks=213&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "tv",
            "hbo",
            "HBO 出品",
            "poster-row",
            catalog.filtered_tv("with_networks=49&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "tv",
            "scifi-fantasy",
            "科幻与奇幻",
            "poster-row",
            catalog.filtered_tv("with_genres=10765&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "tv",
            "crime-mystery",
            "悬疑罪案",
            "poster-row",
            catalog.filtered_tv("with_genres=80&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "tv",
            "comedy",
            "喜剧剧集",
            "poster-row",
            catalog.filtered_tv("with_genres=35&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "tv",
            "animation",
            "动画剧集",
            "poster-row",
            catalog.filtered_tv("with_genres=16&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "tv",
            "reality",
            "真人秀",
            "poster-row",
            catalog.filtered_tv("with_genres=10764&sort_by=popularity.desc"),
        ),
        row(
            catalog.source_name(),
            "tv",
            "documentary",
            "纪录片",
            "poster-row",
            catalog.filtered_tv("with_genres=99&sort_by=popularity.desc"),
        ),
    ]
    .into_iter()
    .filter(|section| !section.is_null())
    .collect()
}

/// 豆瓣源独立分区表（电影）：直接用豆瓣 tag，标题用豆瓣风格，全真实数据。
pub(super) fn douban_movie_sections(catalog: &dyn Catalog) -> Vec<Value> {
    vec![
        row(
            "douban",
            "movie",
            "featured-weekly",
            "本周精选",
            "hero",
            catalog.tagged_movie("热门"),
        ),
        row(
            "douban",
            "movie",
            "trending-day",
            "实时热榜",
            "ranked-row",
            catalog.trending_movie(),
        ),
        row(
            "douban",
            "movie",
            "now-playing",
            "正在热映",
            "poster-row",
            catalog.now_playing_movie(),
        ),
        row(
            "douban",
            "movie",
            "popular",
            "热门电影",
            "poster-row",
            catalog.tagged_movie("热门"),
        ),
        row(
            "douban",
            "movie",
            "top-rated",
            "豆瓣高分",
            "poster-row",
            catalog.tagged_movie("豆瓣高分"),
        ),
        row(
            "douban",
            "movie",
            "classic",
            "经典电影",
            "poster-row",
            catalog.tagged_movie("经典"),
        ),
        row(
            "douban",
            "movie",
            "chinese",
            "华语佳片",
            "poster-row",
            catalog.tagged_movie("华语"),
        ),
        row(
            "douban",
            "movie",
            "japanese",
            "日本电影",
            "poster-row",
            catalog.tagged_movie("日本"),
        ),
        row(
            "douban",
            "movie",
            "korean",
            "韩国电影",
            "poster-row",
            catalog.tagged_movie("韩国"),
        ),
        row(
            "douban",
            "movie",
            "us",
            "欧美佳片",
            "poster-row",
            catalog.tagged_movie("欧美"),
        ),
        row(
            "douban",
            "movie",
            "cold",
            "冷门佳片",
            "poster-row",
            catalog.tagged_movie("冷门佳片"),
        ),
        row(
            "douban",
            "movie",
            "drama",
            "剧情片",
            "poster-row",
            catalog.tagged_movie("剧情"),
        ),
        row(
            "douban",
            "movie",
            "crime",
            "犯罪片",
            "poster-row",
            catalog.tagged_movie("犯罪"),
        ),
        row(
            "douban",
            "movie",
            "fantasy",
            "奇幻片",
            "poster-row",
            catalog.tagged_movie("奇幻"),
        ),
        row(
            "douban",
            "movie",
            "scifi",
            "科幻巨制",
            "poster-row",
            catalog.tagged_movie("科幻"),
        ),
        row(
            "douban",
            "movie",
            "action",
            "动作与冒险",
            "poster-row",
            catalog.tagged_movie("动作"),
        ),
        row(
            "douban",
            "movie",
            "thriller",
            "悬疑惊悚",
            "poster-row",
            catalog.tagged_movie("悬疑"),
        ),
        row(
            "douban",
            "movie",
            "comedy",
            "喜剧佳作",
            "poster-row",
            catalog.tagged_movie("喜剧"),
        ),
        row(
            "douban",
            "movie",
            "romance",
            "爱情电影",
            "poster-row",
            catalog.tagged_movie("爱情"),
        ),
        row(
            "douban",
            "movie",
            "horror",
            "恐怖片",
            "poster-row",
            catalog.tagged_movie("恐怖"),
        ),
        row(
            "douban",
            "movie",
            "animation",
            "动画电影",
            "poster-row",
            catalog.tagged_movie("动画"),
        ),
        row(
            "douban",
            "movie",
            "documentary",
            "纪录片",
            "poster-row",
            catalog.tagged_movie("纪录片"),
        ),
        row(
            "douban",
            "movie",
            "war",
            "战争片",
            "poster-row",
            catalog.tagged_movie("战争"),
        ),
        row(
            "douban",
            "movie",
            "music",
            "音乐片",
            "poster-row",
            catalog.tagged_movie("音乐"),
        ),
        row(
            "douban",
            "movie",
            "imax",
            "IMAX 精选",
            "poster-row",
            catalog.tagged_movie("IMAX"),
        ),
        row(
            "douban",
            "movie",
            "4k",
            "4K 片单",
            "poster-row",
            catalog.tagged_movie("4K"),
        ),
    ]
    .into_iter()
    .filter(|section| !section.is_null())
    .collect()
}

/// 豆瓣源独立分区表（剧集）：直接用豆瓣 tag 与榜单，全真实数据。
pub(super) fn douban_tv_sections(catalog: &dyn Catalog) -> Vec<Value> {
    vec![
        row(
            "douban",
            "tv",
            "featured-weekly",
            "本周精选",
            "hero",
            catalog.tagged_tv("热门"),
        ),
        row(
            "douban",
            "tv",
            "trending-day",
            "实时热榜",
            "ranked-row",
            catalog.trending_tv(),
        ),
        row(
            "douban",
            "tv",
            "popular",
            "热门剧集",
            "poster-row",
            catalog.tagged_tv("热门"),
        ),
        row(
            "douban",
            "tv",
            "chinese",
            "国产剧",
            "poster-row",
            catalog.tagged_tv("国产剧"),
        ),
        row(
            "douban",
            "tv",
            "us",
            "美剧",
            "poster-row",
            catalog.tagged_tv("美剧"),
        ),
        row(
            "douban",
            "tv",
            "japanese",
            "日剧",
            "poster-row",
            catalog.tagged_tv("日剧"),
        ),
        row(
            "douban",
            "tv",
            "korean",
            "韩剧",
            "poster-row",
            catalog.tagged_tv("韩剧"),
        ),
        row(
            "douban",
            "tv",
            "uk",
            "英剧",
            "poster-row",
            catalog.tagged_tv("英剧"),
        ),
        row(
            "douban",
            "tv",
            "animation",
            "动漫",
            "poster-row",
            catalog.tagged_tv("动漫"),
        ),
        row(
            "douban",
            "tv",
            "documentary",
            "纪录片",
            "poster-row",
            catalog.tagged_tv("纪录片"),
        ),
        row(
            "douban",
            "tv",
            "variety",
            "综艺",
            "poster-row",
            catalog.tagged_tv("综艺"),
        ),
    ]
    .into_iter()
    .filter(|section| !section.is_null())
    .collect()
}
