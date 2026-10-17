//! `facets` 的测试与朴素参照实现。
//!
//! 拆成独立文件是为了让 `facets.rs` 远离 800 行硬限：这里约占 300 行，
//! 生产代码只留十维契约与单趟累积实现。朴素参照实现只服务等价性测试，
//! **不参与生产路径**。

use super::*;
use super::super::selection::{LightFacts, metadata::BrowseMetadata};

/// 朴素参照实现（等价性测试用）：每个候选值都解析一次 filter 并全量匹配。
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
    let media = Media {
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
    };
    let mut metadata = BrowseMetadata::default();
    metadata.genres = genres.iter().map(|value| (*value).to_string()).collect();
    metadata.countries = countries.iter().map(|value| (*value).to_string()).collect();
    metadata.rating = rating;
    metadata.runtime = runtime;
    metadata.language = language.map(str::to_string);
    metadata.scraped = scraped;
    LibrarySelection {
        light: LightFacts::from_rows(&media, &metadata, &rows),
        media,
        rows,
        metadata,
        favorite,
        played,
        seen,
        missing,
        added_at: None,
        size_bytes: None,
        last_played_at: None,
    }
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
