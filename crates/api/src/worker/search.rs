use domain::{Coverage, Media};

use crate::management::ApiState;

/// 订阅搜索实际发给站点的词。别名先过门槛，再最多取 3 条。
pub fn search_keywords_for(media: &Media, coverage: &Coverage) -> Vec<String> {
    let mut keywords = Vec::new();
    if let Some(original) = media
        .original_title
        .as_deref()
        .filter(|title| !title.is_empty())
    {
        keywords.push(original.to_string());
    }
    if !media.title.is_empty() {
        keywords.push(media.title.clone());
        if let Some(short) = short_title(&media.title) {
            keywords.push(short);
        }
    }
    if let Some(year) = media.year {
        let title = media.title.trim();
        if !title.is_empty() {
            keywords.push(format!("{title} {year}"));
        }
        if let Some(original) = media
            .original_title
            .as_deref()
            .filter(|title| !title.is_empty())
        {
            keywords.push(format!("{} {year}", original.trim()));
        }
    }
    let mut accepted_aliases = 0;
    for alias in &media.aliases {
        if !distinctive_alias(alias) {
            continue;
        }
        if accepted_aliases == 3 {
            break;
        }
        if keywords.iter().any(|existing| same_keyword(existing, alias)) {
            continue;
        }
        tracing::debug!(alias, "别名进入订阅搜索");
        keywords.push(alias.clone());
        accepted_aliases += 1;
    }
    if let Coverage::Tv {
        season,
        episode_to: Some(_),
        ..
    } = coverage
    {
        let base = short_title(&media.title).unwrap_or_else(|| media.title.clone());
        if !base.is_empty() {
            keywords.push(format!("{base} S{season:02}"));
        }
    }
    let mut seen = std::collections::HashSet::new();
    keywords.retain(|keyword| seen.insert(normalize_keyword(keyword)));
    tracing::info!(keywords = ?keywords, "订阅搜索关键词");
    keywords
}

fn distinctive_alias(alias: &str) -> bool {
    let text = alias.trim();
    if text.is_empty() {
        return false;
    }
    let latin_words = text
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty() && word.chars().all(|ch| ch.is_ascii()))
        .count();
    let non_latin = text.chars().filter(|ch| !ch.is_ascii() && !ch.is_whitespace()).count();
    if latin_words >= 2 || non_latin >= 2 {
        true
    } else {
        tracing::debug!(alias = text, "别名过短，不发给站点");
        false
    }
}

fn same_keyword(left: &str, right: &str) -> bool {
    normalize_keyword(left) == normalize_keyword(right)
}

fn normalize_keyword(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

/// Search using original and display names, their year variants, and a useful
/// short title. Results are de-duplicated across keywords.
pub(crate) fn search_torrents_for_subscribe(
    state: &ApiState,
    sites: &[domain::Site],
    media: &domain::Media,
    coverage: &Coverage,
) -> (Vec<domain::Torrent>, Vec<String>, bool, Vec<String>) {
    let keywords = search_keywords_for(media, coverage);
    search_keywords(state, sites, media, coverage, keywords)
}

pub(crate) fn search_torrents_for_media(
    state: &ApiState,
    sites: &[domain::Site],
    media: &domain::Media,
) -> (Vec<domain::Torrent>, Vec<String>, bool, Vec<String>) {
    search_torrents_for_subscribe(state, sites, media, &Coverage::Movie)
}

fn search_keywords(
    state: &ApiState,
    sites: &[domain::Site],
    media: &Media,
    coverage: &Coverage,
    keywords: Vec<String>,
) -> (Vec<domain::Torrent>, Vec<String>, bool, Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    let mut merged = Vec::new();
    let mut succeeded = keywords.is_empty() || sites.is_empty();
    let mut failures = Vec::new();
    for keyword in &keywords {
        let outcome = state.indexer.search_page(sites, keyword, 1);
        succeeded |=
            report_site_failures(&outcome, sites, &format!("search {keyword}"), &mut failures);
        let continue_page = page_is_full_and_irrelevant(media, coverage, keyword, &outcome.torrents);
        absorb(&mut seen, &mut merged, outcome.torrents);
        if continue_page {
            let second = state.indexer.search_page(sites, keyword, 2);
            succeeded |= report_site_failures(
                &second,
                sites,
                &format!("search {keyword} page 2"),
                &mut failures,
            );
            absorb(&mut seen, &mut merged, second.torrents);
        }
    }
    (merged, keywords, succeeded, failures)
}

fn absorb(
    seen: &mut std::collections::HashSet<String>,
    merged: &mut Vec<domain::Torrent>,
    torrents: Vec<domain::Torrent>,
) {
    for torrent in torrents {
        if seen.insert(torrent.enclosure.clone()) {
            merged.push(torrent);
        }
    }
}

pub(crate) fn report_site_failures(
    outcome: &indexer::SearchOutcome,
    sites: &[domain::Site],
    operation: &str,
    failures: &mut Vec<String>,
) -> bool {
    for failure in &outcome.failures {
        let site_name = sites
            .iter()
            .find(|s| s.id == failure.site_id)
            .map(|s| s.name.as_str())
            .unwrap_or("未知站点");
        let detail = format!("{}: {}", site_name, failure.error);
        tracing::warn!(site_id = %failure.site_id, site = %site_name, operation, error = %failure.error, "站点搜索失败");
        failures.push(detail);
    }
    sites.is_empty() || outcome.failures.len() < sites.len()
}

/// 只有带季号的关键词，第一页满 30 条且没有本剧这一季，才再要一页。
pub fn page_is_full_and_irrelevant(
    media: &Media,
    coverage: &Coverage,
    keyword: &str,
    page: &[domain::Torrent],
) -> bool {
    let Coverage::Tv {
        season,
        episode_to: Some(_),
        ..
    } = coverage
    else {
        tracing::debug!(keyword, "不是有界剧集，不翻第二页");
        return false;
    };
    if !keyword.contains(&format!("S{season:02}")) {
        tracing::debug!(keyword, "不是季关键词，不翻第二页");
        return false;
    }
    if page.len() < 30 {
        tracing::debug!(keyword, count = page.len(), "第一页不满，不翻第二页");
        return false;
    }
    let relevant = page.iter().any(|torrent| {
        let release = release::parse(&torrent.title);
        subscribe::media_identity::media_title_matches(media, &release.title)
            && release.season == Some(*season)
    });
    if relevant {
        tracing::debug!(keyword, "第一页已有本季，不翻第二页");
        false
    } else {
        tracing::info!(keyword, "第一页满且没有本季，再要一页");
        true
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use domain::{Coverage, Media, MediaId, MediaKind, Site, SiteId};
    use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};

    use super::search_torrents_for_subscribe;
    use crate::management::ApiState;

    struct Scripted {
        pages: Mutex<Vec<u32>>,
    }

    impl Fetcher for Scripted {
        fn fetch(&self, request: &FetchRequest) -> Result<String, IndexerError> {
            let page = request
                .url
                .split("page=")
                .nth(1)
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            self.pages.lock().unwrap().push(page);
            let mut rows = String::from("<a href='logout.php'></a><table class='torrents'>");
            for index in 0..30 {
                rows.push_str(&format!(
                    "<tr><td class='rowfollow'><a href='details.php' title='Other Show S02E{index:02}'>t</a><a href='download.php?id={page}{index}'>dl</a></td><td class='rowfollow'>1GB</td><td class='rowfollow'>1</td></tr>"
                ));
            }
            rows.push_str("</table>");
            Ok(rows)
        }
    }

    #[test]
    fn season_keyword_fetches_page_two_and_never_page_three() {
        let tmp = tempfile::tempdir().unwrap();
        let scripted = Arc::new(Scripted {
            pages: Mutex::new(Vec::new()),
        });
        let state = ApiState::new(
            crate::Store::open(tmp.path()).unwrap(),
            "token".into(),
            ProfileSet::load(None).unwrap(),
            scripted.clone(),
            Arc::new(downloader::MemoryDownloader::new(tmp.path().join("stage"))),
            tmp.path().join("library"),
        )
        .unwrap();
        let site = Site {
            id: SiteId::new(),
            name: "猫站".into(),
            url: "https://example.test".into(),
            profile_id: "pterclub".into(),
            cookie: None,
            api_key: None,
            rss_url: None,
            proxy: None,
            rate_limit_per_minute: None,
            cdp_url: None,
            downloader_id: None,
            enabled: true,
            maintenance_via: domain::MaintenanceVia::Http,
        };
        let media = Media {
            id: MediaId::new(),
            kind: MediaKind::Tv,
            title: "醉梦".into(),
            year: None,
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        let (_torrents, keywords, succeeded, failures) = search_torrents_for_subscribe(
            &state,
            &[site],
            &media,
            &Coverage::Tv {
                season: 1,
                episode_from: 1,
                episode_to: Some(3),
            },
        );
        assert!(succeeded, "{failures:?}");
        assert!(keywords.iter().any(|keyword| keyword == "醉梦 S01"));
        let pages = scripted.pages.lock().unwrap().clone();
        assert!(pages.contains(&0), "{pages:?}");
        assert!(pages.contains(&1), "{pages:?}");
        assert!(!pages.iter().any(|page| *page > 1), "{pages:?}");
    }
}

fn short_title(title: &str) -> Option<String> {
    let cut = title
        .split(['：', ':', '（', '(', '「', '『'])
        .next()
        .unwrap_or(title)
        .trim();
    (cut != title && !cut.is_empty()).then(|| cut.to_string())
}
