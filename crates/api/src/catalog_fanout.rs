use std::sync::Arc;

use domain::{Media, MediaKind};
use media::CatalogHit;

use super::catalog::Catalog;

pub struct DoubanCatalog<H> {
    inner: media::Douban<H>,
}

impl<H: media::CatalogGet + Send + Sync> DoubanCatalog<H> {
    pub fn new(inner: media::Douban<H>) -> Arc<Self> {
        Arc::new(Self { inner })
    }
}

impl<H: media::CatalogGet + Send + Sync> Catalog for DoubanCatalog<H> {
    fn source_name(&self) -> &'static str {
        "douban"
    }
    fn source(&self, name: &str) -> Option<&dyn Catalog> {
        (name == "douban").then_some(self)
    }
    fn search_movie(&self, query: &str) -> Result<Vec<CatalogHit>, String> {
        self.inner
            .search_movie(query)
            .map_err(|err| err.to_string())
    }
    fn search_tv(&self, query: &str) -> Result<Vec<CatalogHit>, String> {
        self.inner.search_tv(query).map_err(|err| err.to_string())
    }
    fn popular_movie(&self) -> Result<Vec<CatalogHit>, String> {
        self.inner.popular_movie().map_err(|err| err.to_string())
    }
    fn popular_tv(&self) -> Result<Vec<CatalogHit>, String> {
        self.inner.popular_tv().map_err(|err| err.to_string())
    }
    fn top_rated_movie(&self) -> Result<Vec<CatalogHit>, String> {
        self.inner.top_rated_movie().map_err(|err| err.to_string())
    }
    fn top_rated_tv(&self) -> Result<Vec<CatalogHit>, String> {
        self.inner.top_rated_tv().map_err(|err| err.to_string())
    }
    fn now_playing_movie(&self) -> Result<Vec<CatalogHit>, String> {
        self.inner
            .now_playing_movie()
            .map_err(|err| err.to_string())
    }
    fn on_the_air_tv(&self) -> Result<Vec<CatalogHit>, String> {
        self.inner.on_the_air_tv().map_err(|err| err.to_string())
    }
    fn trending_movie(&self) -> Result<Vec<CatalogHit>, String> {
        self.inner.trending_movie().map_err(|err| err.to_string())
    }
    fn trending_tv(&self) -> Result<Vec<CatalogHit>, String> {
        self.inner.trending_tv().map_err(|err| err.to_string())
    }
    fn upcoming_movie(&self) -> Result<Vec<CatalogHit>, String> {
        self.inner.upcoming_movie().map_err(|err| err.to_string())
    }
    fn filtered_movie(&self, query: &str) -> Result<Vec<CatalogHit>, String> {
        self.inner
            .filtered(domain::MediaKind::Movie, query)
            .map_err(|err| err.to_string())
    }
    fn filtered_tv(&self, query: &str) -> Result<Vec<CatalogHit>, String> {
        self.inner
            .filtered(domain::MediaKind::Tv, query)
            .map_err(|err| err.to_string())
    }
    fn tagged_movie(&self, tag: &str) -> Result<Vec<CatalogHit>, String> {
        self.inner
            .tagged(domain::MediaKind::Movie, tag)
            .map_err(|err| err.to_string())
    }
    fn tagged_tv(&self, tag: &str) -> Result<Vec<CatalogHit>, String> {
        self.inner
            .tagged(domain::MediaKind::Tv, tag)
            .map_err(|err| err.to_string())
    }
    fn collection_paged(
        &self,
        kind: MediaKind,
        query: &str,
        page: i64,
    ) -> Result<Option<(Vec<CatalogHit>, i64, i64, i64)>, String> {
        let hits = self
            .inner
            .filtered_paged(kind, query, page)
            .map_err(|err| err.to_string())?;
        if hits.is_empty() && page == 1 {
            return Ok(None); // 豆瓣没有可翻页的 tag → 非可翻页分区
        }
        // 豆瓣 search_subjects 不返回总数；按「还有一页可拉」估算。
        let has_more = hits.len() >= 20;
        let count = hits.len() as i64;
        Ok(Some((
            hits,
            page,
            if has_more { page + 1 } else { page },
            count,
        )))
    }
    fn details(&self, _kind: MediaKind, douban_id: &str) -> Result<Option<Media>, String> {
        // 豆瓣详情按 subject id 抓取；kind 由页面「集数」字段自判。
        self.inner.detail(douban_id).map_err(|err| err.to_string())
    }
}

pub struct TvdbCatalog<H> {
    inner: media::Tvdb<H>,
}

impl<H: media::CatalogGet + Send + Sync> TvdbCatalog<H> {
    pub fn new(inner: media::Tvdb<H>) -> Arc<Self> {
        Arc::new(Self { inner })
    }
}

impl<H: media::CatalogGet + Send + Sync> Catalog for TvdbCatalog<H> {
    fn source_name(&self) -> &'static str {
        "tvdb"
    }
    fn source(&self, name: &str) -> Option<&dyn Catalog> {
        (name == "tvdb").then_some(self)
    }
    fn search_movie(&self, query: &str) -> Result<Vec<CatalogHit>, String> {
        self.inner
            .search_movie(query)
            .map_err(|err| err.to_string())
    }
    fn search_tv(&self, query: &str) -> Result<Vec<CatalogHit>, String> {
        self.inner.search_tv(query).map_err(|err| err.to_string())
    }
    fn popular_movie(&self) -> Result<Vec<CatalogHit>, String> {
        self.inner.popular_movie().map_err(|err| err.to_string())
    }
    fn popular_tv(&self) -> Result<Vec<CatalogHit>, String> {
        self.inner.popular_tv().map_err(|err| err.to_string())
    }
    fn details(&self, _kind: MediaKind, _tmdb_id: &str) -> Result<Option<Media>, String> {
        Ok(None)
    }
}

pub struct BangumiCatalog<H> {
    inner: media::Bangumi<H>,
}

impl<H: media::CatalogGet + Send + Sync> BangumiCatalog<H> {
    pub fn new(inner: media::Bangumi<H>) -> Arc<Self> {
        Arc::new(Self { inner })
    }
}

impl<H: media::CatalogGet + Send + Sync> Catalog for BangumiCatalog<H> {
    fn source_name(&self) -> &'static str {
        "bangumi"
    }
    fn source(&self, name: &str) -> Option<&dyn Catalog> {
        (name == "bangumi").then_some(self)
    }
    fn search_movie(&self, query: &str) -> Result<Vec<CatalogHit>, String> {
        self.inner
            .search_movie(query)
            .map_err(|err| err.to_string())
    }
    fn search_tv(&self, query: &str) -> Result<Vec<CatalogHit>, String> {
        self.inner.search_tv(query).map_err(|err| err.to_string())
    }
    fn popular_movie(&self) -> Result<Vec<CatalogHit>, String> {
        self.inner.popular_movie().map_err(|err| err.to_string())
    }
    fn popular_tv(&self) -> Result<Vec<CatalogHit>, String> {
        self.inner.popular_tv().map_err(|err| err.to_string())
    }
    fn details(&self, _kind: MediaKind, _tmdb_id: &str) -> Result<Option<Media>, String> {
        Ok(None)
    }
}

pub struct AnilistCatalog<H> {
    inner: media::Anilist<H>,
}

impl<H: media::CatalogGet + Send + Sync> AnilistCatalog<H> {
    pub fn new(inner: media::Anilist<H>) -> Arc<Self> {
        Arc::new(Self { inner })
    }
}

impl<H: media::CatalogGet + Send + Sync> Catalog for AnilistCatalog<H> {
    fn source_name(&self) -> &'static str {
        "anilist"
    }
    fn source(&self, name: &str) -> Option<&dyn Catalog> {
        (name == "anilist").then_some(self)
    }
    fn search_movie(&self, query: &str) -> Result<Vec<CatalogHit>, String> {
        self.inner
            .search_movie(query)
            .map_err(|err| err.to_string())
    }
    fn search_tv(&self, query: &str) -> Result<Vec<CatalogHit>, String> {
        self.inner.search_tv(query).map_err(|err| err.to_string())
    }
    fn popular_movie(&self) -> Result<Vec<CatalogHit>, String> {
        self.inner.popular_movie().map_err(|err| err.to_string())
    }
    fn popular_tv(&self) -> Result<Vec<CatalogHit>, String> {
        self.inner.popular_tv().map_err(|err| err.to_string())
    }
    fn details(&self, _kind: MediaKind, _tmdb_id: &str) -> Result<Option<Media>, String> {
        Ok(None)
    }
}

pub struct FanoutCatalog {
    sources: Vec<Arc<dyn Catalog>>,
}

impl FanoutCatalog {
    pub fn new(sources: Vec<Arc<dyn Catalog>>) -> Arc<Self> {
        Arc::new(Self { sources })
    }
}

impl Catalog for FanoutCatalog {
    fn source_name(&self) -> &'static str {
        "fanout"
    }
    fn source(&self, name: &str) -> Option<&dyn Catalog> {
        self.sources
            .iter()
            .find(|source| source.source_name() == name)
            .map(|arc| arc.as_ref())
    }
    fn sources(&self) -> Vec<&'static str> {
        self.sources
            .iter()
            .map(|source| source.source_name())
            .collect()
    }
    fn search_movie(&self, query: &str) -> Result<Vec<CatalogHit>, String> {
        merge(self.sources.iter().map(|source| source.search_movie(query)))
    }
    fn search_tv(&self, query: &str) -> Result<Vec<CatalogHit>, String> {
        self.search_tv_year(query, None)
    }
    fn search_tv_year(&self, query: &str, year: Option<u16>) -> Result<Vec<CatalogHit>, String> {
        merge(
            self.sources
                .iter()
                .map(|source| source.search_tv_year(query, year)),
        )
    }
    fn popular_movie(&self) -> Result<Vec<CatalogHit>, String> {
        merge(self.sources.iter().map(|source| source.popular_movie()))
    }
    fn popular_tv(&self) -> Result<Vec<CatalogHit>, String> {
        merge(self.sources.iter().map(|source| source.popular_tv()))
    }
    fn top_rated_movie(&self) -> Result<Vec<CatalogHit>, String> {
        merge(self.sources.iter().map(|source| source.top_rated_movie()))
    }
    fn top_rated_tv(&self) -> Result<Vec<CatalogHit>, String> {
        merge(self.sources.iter().map(|source| source.top_rated_tv()))
    }
    fn now_playing_movie(&self) -> Result<Vec<CatalogHit>, String> {
        merge(self.sources.iter().map(|source| source.now_playing_movie()))
    }
    fn on_the_air_tv(&self) -> Result<Vec<CatalogHit>, String> {
        merge(self.sources.iter().map(|source| source.on_the_air_tv()))
    }
    fn filtered_movie(&self, query: &str) -> Result<Vec<CatalogHit>, String> {
        merge(
            self.sources
                .iter()
                .map(|source| source.filtered_movie(query)),
        )
    }
    fn filtered_tv(&self, query: &str) -> Result<Vec<CatalogHit>, String> {
        merge(self.sources.iter().map(|source| source.filtered_tv(query)))
    }
    fn collection_paged(
        &self,
        kind: MediaKind,
        query: &str,
        page: i64,
    ) -> Result<Option<(Vec<CatalogHit>, i64, i64, i64)>, String> {
        // Only TMDB implements paging; other sources return None → not paginable.
        if let Some(tmdb) = self.source("tmdb") {
            return tmdb.collection_paged(kind, query, page);
        }
        Ok(None)
    }
    fn trending_movie(&self) -> Result<Vec<CatalogHit>, String> {
        merge(self.sources.iter().map(|source| source.trending_movie()))
    }
    fn trending_tv(&self) -> Result<Vec<CatalogHit>, String> {
        merge(self.sources.iter().map(|source| source.trending_tv()))
    }
    fn upcoming_movie(&self) -> Result<Vec<CatalogHit>, String> {
        merge(self.sources.iter().map(|source| source.upcoming_movie()))
    }
    fn airing_today_tv(&self) -> Result<Vec<CatalogHit>, String> {
        merge(self.sources.iter().map(|source| source.airing_today_tv()))
    }
    fn genres(&self, kind: MediaKind) -> Result<Vec<(i64, String)>, String> {
        let mut seen = std::collections::HashSet::new();
        let mut rows = Vec::new();
        for source in &self.sources {
            if let Ok(list) = source.genres(kind) {
                for (id, name) in list {
                    if seen.insert(id) {
                        rows.push((id, name));
                    }
                }
            }
        }
        Ok(rows)
    }
    fn season_details(
        &self,
        tmdb_id: &str,
        season: u32,
    ) -> Result<Vec<media::EpisodeMeta>, String> {
        if let Some(tmdb) = self.source("tmdb") {
            return tmdb.season_details(tmdb_id, season);
        }
        Ok(Vec::new())
    }
    fn cached_season_details(
        &self,
        tmdb_id: &str,
        season: u32,
    ) -> Result<Option<Vec<media::EpisodeMeta>>, String> {
        if let Some(tmdb) = self.source("tmdb") {
            return tmdb.cached_season_details(tmdb_id, season);
        }
        Ok(None)
    }
    fn season_details_lang(
        &self,
        tmdb_id: &str,
        season: u32,
        lang: &str,
    ) -> Result<Vec<media::EpisodeMeta>, String> {
        if let Some(tmdb) = self.source("tmdb") {
            return tmdb.season_details_lang(tmdb_id, season, lang);
        }
        Ok(Vec::new())
    }
    fn similar(&self, kind: MediaKind, tmdb_id: &str) -> Result<Vec<media::CatalogHit>, String> {
        if let Some(tmdb) = self.source("tmdb") {
            return tmdb.similar(kind, tmdb_id);
        }
        Ok(Vec::new())
    }
    fn configuration_languages(&self) -> Result<Vec<media::LanguageRow>, String> {
        if let Some(tmdb) = self.source("tmdb") {
            return tmdb.configuration_languages();
        }
        Ok(Vec::new())
    }
    fn configuration_countries(&self) -> Result<Vec<media::CountryRow>, String> {
        if let Some(tmdb) = self.source("tmdb") {
            return tmdb.configuration_countries();
        }
        Ok(Vec::new())
    }
    fn image_candidates(
        &self,
        kind: MediaKind,
        tmdb_id: &str,
    ) -> Result<crate::catalog::ArtworkCandidates, String> {
        if let Some(tmdb) = self.source("tmdb") {
            return tmdb.image_candidates(kind, tmdb_id);
        }
        Ok(crate::catalog::ArtworkCandidates::default())
    }
    fn metadata(&self, kind: MediaKind, tmdb_id: &str) -> Result<Option<media::ItemMeta>, String> {
        if let Some(tmdb) = self.source("tmdb") {
            return tmdb.metadata(kind, tmdb_id);
        }
        Ok(None)
    }
    fn metadata_with_preferences(
        &self,
        kind: MediaKind,
        tmdb_id: &str,
        language_priority: &[String],
        cert_country_priority: &[String],
    ) -> Result<Option<media::ItemMeta>, String> {
        if let Some(tmdb) = self.source("tmdb") {
            return tmdb.metadata_with_preferences(
                kind,
                tmdb_id,
                language_priority,
                cert_country_priority,
            );
        }
        Ok(None)
    }
    fn person_details(&self, tmdb_person_id: &str) -> Result<Option<media::PersonDetails>, String> {
        if let Some(tmdb) = self.source("tmdb") {
            return tmdb.person_details(tmdb_person_id);
        }
        Ok(None)
    }
    fn episode_stills(
        &self,
        tmdb_id: &str,
        season: u32,
        episode: u32,
    ) -> Result<Vec<String>, String> {
        if let Some(tmdb) = self.source("tmdb") {
            return tmdb.episode_stills(tmdb_id, season, episode);
        }
        Ok(Vec::new())
    }
    fn tv_seasons(&self, tmdb_id: &str) -> Result<Vec<media::TvSeason>, String> {
        if let Some(tmdb) = self.source("tmdb") {
            return tmdb.tv_seasons(tmdb_id);
        }
        Ok(Vec::new())
    }
    fn season_episodes(
        &self,
        tmdb_id: &str,
        season: u32,
    ) -> Result<Vec<media::SeasonEpisode>, String> {
        if let Some(tmdb) = self.source("tmdb") {
            return tmdb.season_episodes(tmdb_id, season);
        }
        Ok(Vec::new())
    }
    fn tvdb_id(&self, tmdb_id: &str) -> Result<Option<String>, String> {
        if let Some(tmdb) = self.source("tmdb") {
            return tmdb.tvdb_id(tmdb_id);
        }
        Ok(None)
    }
    fn poster_url(&self, kind: MediaKind, tmdb_id: &str) -> Result<Option<String>, String> {
        if let Some(tmdb) = self.source("tmdb") {
            return tmdb.poster_url(kind, tmdb_id);
        }
        Ok(None)
    }
    fn details(&self, kind: MediaKind, tmdb_id: &str) -> Result<Option<Media>, String> {
        // details 的参数是 TMDB 专有的数字 ID。TMDB 失败时绝不能将 TMDB 数字 ID 传给
        // 豆瓣或其它源（豆瓣的 subject id 与 TMDB id 属于完全不同的命名空间，会导致错抓或污染）。
        if let Some(tmdb) = self.source("tmdb") {
            return tmdb.details(kind, tmdb_id);
        }
        let mut last_err = None;
        for source in &self.sources {
            if source.source_name() == "tmdb" {
                match source.details(kind, tmdb_id) {
                    Ok(Some(media)) => return Ok(Some(media)),
                    Ok(None) => {}
                    Err(err) => last_err = Some(err),
                }
            }
        }
        match last_err {
            Some(err) => Err(err),
            None => Ok(None),
        }
    }
}

fn is_unsupported(err: &str) -> bool {
    err.contains("不支持")
}

fn merge(
    results: impl Iterator<Item = Result<Vec<CatalogHit>, String>>,
) -> Result<Vec<CatalogHit>, String> {
    let mut hits = Vec::new();
    let mut last_real_err = None;
    let mut unsupported_count = 0;
    let mut total_sources = 0;
    for result in results {
        total_sources += 1;
        match result {
            Ok(mut rows) => hits.append(&mut rows),
            Err(err) if is_unsupported(&err) => {
                unsupported_count += 1;
            }
            Err(err) => last_real_err = Some(err),
        }
    }
    if hits.is_empty() {
        if let Some(err) = last_real_err {
            return Err(err);
        }
        // 若全部参与合并的源都显式返回「不支持」，且没有获得任何 hits，向调用方报不支持 (502)
        if total_sources > 0 && unsupported_count == total_sources {
            return Err("该数据源不支持筛选发现".into());
        }
    }
    Ok(hits)
}
