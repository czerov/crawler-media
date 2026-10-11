use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;

use api::router;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use tower::ServiceExt;

use super::super::common::{Fixtures, request, state};

pub struct RenameCatalog;

impl api::catalog::Catalog for RenameCatalog {
    fn search_movie(&self, _query: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn search_tv(&self, _query: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn details(
        &self,
        _kind: domain::MediaKind,
        _id: &str,
    ) -> Result<Option<domain::Media>, String> {
        Ok(None)
    }
    fn season_details(&self, _id: &str, _season: u32) -> Result<Vec<media::EpisodeMeta>, String> {
        Ok((1..=2)
            .map(|episode_number| media::EpisodeMeta {
                episode_number,
                name: Some(format!("Episode {episode_number}")),
                overview: Some("Episode plot".into()),
                still_path: Some(format!("/ep{episode_number}.jpg")),
                ..Default::default()
            })
            .collect())
    }
    fn season_details_lang(
        &self,
        id: &str,
        season: u32,
        _lang: &str,
    ) -> Result<Vec<media::EpisodeMeta>, String> {
        self.season_details(id, season)
    }
    fn tv_seasons(&self, _tmdb_id: &str) -> Result<Vec<media::TvSeason>, String> {
        Ok(vec![
            media::TvSeason {
                season_number: 1,
                name: "第 1 季".into(),
                episode_count: Some(2),
                air_date: Some("2022-10-10".into()),
                overview: Some("Season plot".into()),
                poster_path: None,
            },
            media::TvSeason {
                season_number: 2,
                name: "第 2 季".into(),
                episode_count: Some(2),
                air_date: Some("2023-10-10".into()),
                overview: Some("Season 2 plot".into()),
                poster_path: None,
            },
        ])
    }
    fn episode_stills(&self, _id: &str, _season: u32, episode: u32) -> Result<Vec<String>, String> {
        Ok(vec![format!("/ep{episode}.jpg")])
    }
}

pub struct RenameBytes;

impl api::PosterFetch for RenameBytes {
    fn get(&self, url: &str) -> Result<Vec<u8>, String> {
        if url.contains("/ep1.jpg") {
            Ok(b"one".to_vec())
        } else if url.contains("/ep2.jpg") {
            Ok(b"two".to_vec())
        } else {
            Err(format!("no fixture for {url}"))
        }
    }
}

pub struct RenameFixture {
    pub _tmp: TempDir,
    pub state: api::ApiState,
    pub app: axum::Router,
    pub media: domain::Media,
    pub library_id: String,
    pub old_root: PathBuf,
    pub live_root: PathBuf,
}

fn row(
    media_id: domain::MediaId,
    path: &std::path::Path,
    season: u32,
    episode: u32,
) -> domain::LedgerRow {
    domain::LedgerRow {
        id: domain::LedgerId::new(),
        media_id,
        path: path.display().to_string(),
        season: Some(season),
        episode: Some(episode),
        resolution: Some("1080p".into()),
        codec: None,
        hdr: None,
        quality_source: domain::QualitySource::Release,
        confidence: domain::Confidence::High,
        filter_score: None,
    }
}

fn seed_layout(store: &api::Store, media_id: domain::MediaId, layout: &str, root: &std::path::Path) {
    if layout == "two_seasons" {
        seed_season(store, media_id, &root.join("Season 01"), 1);
        seed_season(store, media_id, &root.join("Season 02"), 2);
        return;
    }
    let dir = if layout.is_empty() {
        root.to_path_buf()
    } else {
        root.join(layout)
    };
    seed_season(store, media_id, &dir, 1);
}

fn seed_season(
    store: &api::Store,
    media_id: domain::MediaId,
    dir: &std::path::Path,
    season: u32,
) {
    std::fs::create_dir_all(dir).unwrap();
    for episode in 1..=2 {
        let path = dir.join(format!("Spirit.Rangers.S{season:02}E{episode:02}.strm"));
        std::fs::write(&path, b"https://example.com/stream").unwrap();
        store
            .insert_ledger(&row(media_id, &path, season, episode))
            .unwrap();
    }
}

impl RenameFixture {
    pub fn new(layout: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let api_state = state(
            tmp.path(),
            Arc::new(Fixtures {
                requests: Mutex::new(Vec::new()),
                bodies: HashMap::new(),
            }),
            Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
        )
        .with_catalog(Arc::new(RenameCatalog))
        .with_poster_fetch(Arc::new(RenameBytes));

        let app = router(api_state.clone());
        let store_guard = api_state.store();
        let store = store_guard.lock();

        let tv_lib = store
            .list_libraries()
            .unwrap()
            .into_iter()
            .find(|lib| lib.kind == domain::MediaKind::Tv)
            .expect("TV library must exist");
        let library_id = tv_lib.id.clone();
        let tv_root = tv_lib.root_paths[0].clone();

        let old_root = tv_root.join("动灵守护者 (2022)");
        let live_root = tv_root.join("动灵守护者1 (2022)");

        let media_id = domain::MediaId::new();
        let media = domain::Media {
            id: media_id,
            kind: domain::MediaKind::Tv,
            title: "Spirit Rangers".into(),
            year: Some(2022),
            original_title: None,
            tmdb_id: Some("119847".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        store.insert_media(&media).unwrap();
        seed_layout(&store, media_id, layout, &old_root);
        std::fs::rename(&old_root, &live_root).unwrap();
        seed_layout(&store, media_id, layout, &live_root);
        let media_clone = media.clone();

        drop(store);
        drop(store_guard);

        Self {
            _tmp: tmp,
            state: api_state,
            app,
            media: media_clone,
            library_id,
            old_root,
            live_root,
        }
    }

    pub async fn refresh(&self) {
        let response = self
            .app
            .clone()
            .oneshot(request(
                "POST",
                &format!("/api/v1/libraries/{}/metadata/refresh", self.library_id),
                Some("management-secret"),
                serde_json::Value::Null,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
    }

    pub fn assert_live_sidecars(&self, layout: &str) {
        let episodes = if layout.is_empty() {
            self.live_root.clone()
        } else {
            self.live_root.join(layout)
        };
        assert!(
            self.live_root.join("tvshow.nfo").is_file(),
            "tvshow.nfo must exist at show root"
        );
        assert!(
            episodes.join("season.nfo").is_file(),
            "season.nfo must exist at season directory"
        );
        for episode in 1..=2 {
            let stem = format!("Spirit.Rangers.S01E{episode:02}");
            let nfo = std::fs::read_to_string(episodes.join(format!("{stem}.nfo"))).unwrap();
            assert!(nfo.contains("Episode plot"));
            let bytes = std::fs::read(episodes.join(format!("{stem}-thumb.jpg"))).unwrap();
            assert_eq!(bytes, if episode == 1 { b"one" } else { b"two" });
        }
        if !layout.is_empty() {
            assert!(
                !episodes.join("tvshow.nfo").exists(),
                "tvshow.nfo must NOT exist in season subfolder"
            );
        }
        assert!(
            !self.old_root.exists(),
            "scrape must not recreate the renamed-away directory: {}",
            self.old_root.display()
        );
        let store_guard = self.state.store();
        let store = store_guard.lock();
        assert_eq!(
            store.ledger_for_media(self.media.id).unwrap().len(),
            4,
            "both 2 old rows and 2 new rows should remain in ledger during grace period"
        );
        assert!(store.get_media(self.media.id).unwrap().is_some());
    }
}
