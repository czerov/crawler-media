//! T7 测试：legacy 封面就地升级必须保持不可变版本文件与 CAS 保证 (F08)。
//! 若在升级过程中发生并发手动更新、或者数据库写入失败，原有固定路径的图片内容不得被损坏或篡改。

use std::path::Path;
use std::sync::Arc;

use api::library_cover_service::{CoverRenderer, LibraryCoverService};
use api::{ApiState, Store};
use cover_generator::CoverOptions;
use domain::{LedgerId, LedgerRow, Media, MediaId, MediaKind};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline".into()))
    }
}

fn make_state(root: &Path) -> ApiState {
    ApiState::new(
        Store::open(root.join("data")).unwrap(),
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
        root.join("library"),
    )
    .unwrap()
}

struct TriggerRenderer {
    output: Vec<u8>,
}

impl CoverRenderer for TriggerRenderer {
    fn render(
        &self,
        _library: &store::Library,
        _rows: &[domain::LedgerRow],
        _options: &CoverOptions,
    ) -> Result<Option<Vec<u8>>, String> {
        Ok(Some(self.output.clone()))
    }
}

#[tokio::test]
async fn legacy_cover_upgrade_failure_preserves_existing_legacy_file() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    let store = state.store();

    let movie_dir = tmp.path().join("fake_movie");
    std::fs::create_dir_all(&movie_dir).unwrap();
    let video_path = movie_dir.join("movie.mkv");
    std::fs::write(&video_path, b"video").unwrap();

    let lib = store
        .lock()
        .create_library(
            MediaKind::Movie,
            "test_lib",
            &[movie_dir.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();

    let covers_dir = store.lock().library_covers_dir();
    std::fs::create_dir_all(&covers_dir).unwrap();
    let legacy_file = covers_dir.join(format!("library-{}.jpg", lib.id));
    let initial_bytes = b"ORIGINAL_LEGACY_BYTES";
    std::fs::write(&legacy_file, initial_bytes).unwrap();

    let fanart_path = movie_dir.join("fanart.jpg");
    std::fs::write(&fanart_path, initial_bytes).unwrap();

    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Test Movie".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.lock().insert_media(&media).unwrap();

    store
        .lock()
        .insert_ledger(&LedgerRow {
            id: LedgerId::new(),
            media_id: media.id,
            path: video_path.display().to_string(),
            season: None,
            episode: None,
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: domain::QualitySource::Release,
            confidence: domain::Confidence::High,
            filter_score: None,
        })
        .unwrap();

    // 注入一个 trigger，使更新 libraries 表的 cover_path 时必定抛出异常失败！
    {
        let conn = rusqlite::Connection::open(tmp.path().join("data/app.db")).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER fail_cover_update BEFORE UPDATE OF cover_path ON libraries
             BEGIN
                 SELECT RAISE(FAIL, 'database failure simulated');
             END;",
        )
        .unwrap();
    }

    let service = Arc::new(LibraryCoverService::new(Arc::new(TriggerRenderer {
        output: b"NEW_RENDERED_BYTES".to_vec(),
    })));

    let state = state.with_cover_service(service.clone());

    // 调用 ensure_cover：由于 DB 更新会被 trigger 拦截，CAS 发布将失败。
    let res = service.ensure_cover(&state, &lib.id).await;
    assert!(res.is_err(), "数据库发布失败必须报错");

    // 核心 F08 断言：原有 legacy 文件的内容绝不得被就地覆盖为 NEW_RENDERED_BYTES！
    let current_bytes = std::fs::read(&legacy_file).unwrap();
    assert_eq!(
        current_bytes, initial_bytes,
        "升级发布失败时，原有旧文件的内容必须完好保留，不得被破坏！"
    );
}
