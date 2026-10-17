//! A4 浏览页工作量边界：热页只解码选中 Media 的 facts，且不随无关数据增长。
//!
//! 断言解码行数、文件 IO 与「页输出 == 旧全量选择输出」逐字段等价，而不只比响应大小。

#[path = "support/mod.rs"]
mod support;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use api::ApiState;
use api::browse_file_read::BrowseFileRead;
use api::browse_projection::ProjectionService;
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
use serde_json::Value;
use support::library_presence::PresenceFixture;

/// 计数浏览文件 seam：热页必须让它保持 0（构建投影本身的 IO 不计）。
#[derive(Default)]
struct CountingFiles {
    metadata: AtomicUsize,
    read_nfo: AtomicUsize,
    is_file: AtomicUsize,
}

impl CountingFiles {
    fn total(&self) -> usize {
        self.metadata.load(Ordering::SeqCst)
            + self.read_nfo.load(Ordering::SeqCst)
            + self.is_file.load(Ordering::SeqCst)
    }

    fn reset(&self) {
        self.metadata.store(0, Ordering::SeqCst);
        self.read_nfo.store(0, Ordering::SeqCst);
        self.is_file.store(0, Ordering::SeqCst);
    }
}

impl BrowseFileRead for CountingFiles {
    fn read_nfo(&self, path: &Path) -> Option<library::NfoMeta> {
        self.read_nfo.fetch_add(1, Ordering::SeqCst);
        library::read_nfo(path)
    }

    fn metadata(&self, path: &Path) -> std::io::Result<std::fs::Metadata> {
        self.metadata.fetch_add(1, Ordering::SeqCst);
        std::fs::metadata(path)
    }

    fn is_file(&self, path: &Path) -> bool {
        self.is_file.fetch_add(1, Ordering::SeqCst);
        path.is_file()
    }
}

/// 一个被种下的 Media：标题、kind、每 Media 的 ledger 行数。
struct Seeded {
    fixture: PresenceFixture,
    files: Arc<CountingFiles>,
    titles: Vec<String>,
}

/// TV 库：`count` 个 Media，每个 `episodes` 集（行不落盘：热页断言的是解码行数
/// 与浏览 IO，不是真实媒体文件）。每个 Media 都有 tmdb 缓存体，投影与旧路径
/// 因此都能拿到同一份元数据（过滤维度可用）。
fn seed(count: usize, episodes: u32) -> Seeded {
    let fixture = PresenceFixture::new();
    let mut titles = Vec::new();
    {
        let store = fixture.state.store();
        let guard = store.lock();
        let mut previous_title = String::new();
        for index in 0..count {
            let media_id = MediaId::new();
            // 每第四个 Media 与上一个同名：索引分页的并列键（media_id 升序）才被测到。
            let title = if index % 4 == 3 {
                previous_title.clone()
            } else {
                format!("Show {index:05}")
            };
            previous_title = title.clone();
            titles.push(title.clone());
            let tmdb_id = format!("{}", 100_000 + index);
            // 每第二个 Media 落真实文件（有 added_at/大小），其余留空（NULL added_at）；
            // 每第三个落一张海报（gallery 先剔空组再分页）。
            let dir = fixture.root.join(format!("{title}-{index:05}"));
            if index % 2 == 0 || index % 3 == 0 {
                std::fs::create_dir_all(&dir).unwrap();
            }
            if index % 3 == 0 {
                std::fs::write(dir.join("poster.jpg"), b"poster").unwrap();
            }
            if index % 2 == 0 {
                std::fs::write(dir.join("S01E01.mkv"), b"video-bytes").unwrap();
            }
            // 年份缺失（index % 7 == 0）→ release_date 物化键为空串；
            // catalog 有完整发行日期（index % 3 == 0）→ 完整日期优先。
            let year = (index % 7 != 0).then(|| 2000 + (index % 20) as u16);
            guard
                .insert_media(&Media {
                    id: media_id,
                    kind: MediaKind::Tv,
                    title: title.clone(),
                    year,
                    original_title: Some(title.clone()),
                    aliases: Vec::new(),
                    tmdb_id: Some(tmdb_id.clone()),
                    douban_id: None,
                    tvdb_id: None,
                    bangumi_id: None,
                    anilist_id: None,
                })
                .unwrap();
            for episode in 1..=episodes {
                guard
                    .insert_ledger(&LedgerRow {
                        id: LedgerId::new(),
                        media_id,
                        path: dir
                            .join(format!("S01E{episode:02}.mkv"))
                            .display()
                            .to_string(),
                        season: Some(1),
                        episode: Some(episode),
                        resolution: Some(if episode == 1 { "1080p" } else { "720p" }.into()),
                        codec: Some("H264".into()),
                        hdr: (episode == 1).then(|| "HDR".to_string()),
                        quality_source: QualitySource::Release,
                        confidence: Confidence::High,
                        filter_score: None,
                    })
                    .unwrap();
            }
            let release_date = (index % 3 == 0)
                .then(|| format!(",\"release_date\":\"2015-06-{:02}\"", 1 + (index % 28)));
            guard
                .put_catalog_cache(
                    "tmdb",
                    &format!("/tv/{tmdb_id}?language=zh-CN"),
                    &format!(
                        "{{\"vote_average\":8.5,\"runtime\":101,\"original_language\":\"en\",\
                          \"genres\":[{{\"id\":{}}}],\"origin_country\":[\"US\"]{}}}",
                        if index % 2 == 0 { 28 } else { 35 },
                        release_date.unwrap_or_default()
                    ),
                    1000,
                    None,
                )
                .unwrap();
        }
    }
    let files = Arc::new(CountingFiles::default());
    let state = fixture
        .state
        .clone()
        .with_browse_file_read(files.clone())
        .with_projection_service(Arc::new(ProjectionService::new()));
    Seeded {
        fixture: PresenceFixture {
            state,
            ..fixture
        },
        files,
        titles,
    }
}

impl Seeded {
    fn library(&self) -> String {
        self.fixture.library_id.clone()
    }

    /// 预热公共投影：先把变更 journal 消费到完整（每轮有界），再让投影构建/复用。
    /// 构建允许文件 IO，测试随后重置计数。
    async fn warm(&self) -> Vec<MediaId> {
        for _ in 0..512 {
            if self
                .fixture
                .state
                .projection
                .poll_changes(&self.fixture.state)
                .unwrap_or(false)
            {
                break;
            }
        }
        let snapshot = self
            .fixture
            .state
            .projection
            .ensure(self.fixture.state.clone(), self.library())
            .await
            .expect("projection build");
        snapshot.media.into_iter().map(|row| row.media_id).collect()
    }

    async fn get(&self, url: &str) -> Value {
        let (status, body) = self
            .fixture
            .request("GET", url, Some(&self.fixture.admin_token))
            .await;
        assert_eq!(status, 200, "{url}: {body}");
        body["data"].clone()
    }

    fn decoded_files(&self) -> usize {
        self.fixture
            .state
            .browse_page_decoded_files()
            .load(Ordering::SeqCst)
    }

    fn decoded_media(&self) -> usize {
        self.fixture
            .state
            .browse_page_decoded_media()
            .load(Ordering::SeqCst)
    }

    fn candidate_rows(&self) -> usize {
        self.fixture
            .state
            .browse_page_candidate_rows()
            .load(Ordering::SeqCst)
    }

    fn dependency_rows(&self) -> usize {
        self.fixture
            .state
            .browse_page_dependency_rows()
            .load(Ordering::SeqCst)
    }
}

fn reset_counters(seeded: &Seeded) {
    seeded.files.reset();
    seeded
        .fixture
        .state
        .browse_page_decoded_files()
        .store(0, Ordering::SeqCst);
    seeded
        .fixture
        .state
        .browse_page_decoded_media()
        .store(0, Ordering::SeqCst);
    seeded
        .fixture
        .state
        .browse_page_candidate_rows()
        .store(0, Ordering::SeqCst);
    seeded
        .fixture
        .state
        .browse_page_dependency_rows()
        .store(0, Ordering::SeqCst);
}

/// 对任意 `ApiState`（含投影关闭的对照态）发一次 GET。
async fn get_with(state: &ApiState, url: &str, token: &str) -> (axum::http::StatusCode, Value) {
    use tower::ServiceExt;

    let app = api::router(state.clone());
    let request = axum::http::Request::builder()
        .method("GET")
        .uri(url)
        .header("Authorization", format!("Bearer {token}"))
        .body(axum::body::Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// 热页（无过滤、常用排序）：只解码本页 Media 的 facts，且零文件 IO。
#[tokio::test]
async fn hot_page_decodes_only_selected_file_facts() {
    let seeded = seed(600, 2);
    seeded.warm().await;
    let url = format!("/api/v1/libraries/{}/items?limit=20&offset=0", seeded.library());
    reset_counters(&seeded);
    let page = seeded.get(&url).await;
    let items = page.as_array().expect("items must be an array");
    assert_eq!(items.len(), 20);
    assert_eq!(
        seeded.files.total(),
        0,
        "热页不得做任何 NFO/stat/is_file"
    );
    assert_eq!(
        seeded.decoded_files(),
        40,
        "只解码本页 20 个 Media × 2 集的 file facts（计数在读取处，含被丢弃的行）"
    );
    assert_eq!(seeded.decoded_media(), 20, "只解码本页 Media 的 facts");
    assert_eq!(
        seeded.candidate_rows(),
        0,
        "常用排序走 SQL 索引分页，不读轻候选 JSON"
    );
    assert_eq!(
        seeded.dependency_rows(),
        0,
        "依赖覆盖在预热时已完成，本次请求不再读依赖"
    );
    // 页内容与旧全量选择逐字段一致。
    let legacy = seeded
        .fixture
        .state
        .clone()
        .with_projection_service(Arc::new(ProjectionService::disabled()));
    let (status, live) = get_with(&legacy, &url, &seeded.fixture.admin_token).await;
    assert_eq!(status, 200, "{live}");
    assert_eq!(page, live["data"], "投影页输出必须与旧全量选择逐字段一致");
}

/// 高级过滤：先完整筛选排序再选页，仍只解码选中页的 file facts。
#[tokio::test]
async fn advanced_filter_selects_before_paging() {
    let seeded = seed(400, 2);
    seeded.warm().await;
    let urls = [
        // (查询, 是否常用排序的 SQL 索引分页：是 → 不读轻候选)
        ("items?g=28&limit=20&offset=0", false),
        ("items?hdr=true&limit=20&offset=0", false),
        ("items?stock=unscraped&limit=20&offset=0", false),
        ("items?stock=missing&limit=20&offset=0", false),
        ("items?identity=provisional&limit=20&offset=0", false),
        ("items?rating_gte=8&limit=20&offset=20", false),
        ("items?sort=size&order=desc&limit=20&offset=0", false),
        ("items?sort=random&limit=20&offset=0", false),
        ("items?w=unwatched&limit=20&offset=0", false),
        ("items?unwatched_first=true&sort=added_at&limit=20&offset=0", false),
        ("gallery?limit=20&offset=0", false),
        // 索引列分页与全量选择同序：不同/NULL added_at、完整日期/年份兜底/缺失日期、
        // 重复标题并列，两个方向且跨页边界。
        ("items?sort=release_date&order=desc&limit=20&offset=0", true),
        ("items?sort=added_at&order=asc&limit=20&offset=0", true),
        ("items?sort=added_at&order=asc&limit=20&offset=20", true),
        ("items?sort=added_at&order=desc&limit=20&offset=0", true),
        ("items?sort=release_date&order=desc&limit=20&offset=20", true),
        ("items?sort=release_date_asc&limit=20&offset=0", true),
        ("items?limit=20&offset=20", true),
    ];
    let legacy = seeded
        .fixture
        .state
        .clone()
        .with_projection_service(Arc::new(ProjectionService::disabled()));
    for (suffix, indexed) in urls {
        let url = format!("/api/v1/libraries/{}/{suffix}", seeded.library());
        reset_counters(&seeded);
        let page = seeded.get(&url).await;
        let items = page.as_array().expect("must be an array");
        assert_eq!(seeded.files.total(), 0, "{suffix} 不得做浏览文件 IO");
        assert_eq!(
            seeded.candidate_rows(),
            if indexed { 0 } else { 400 },
            "{suffix} 只有高级查询读整库轻候选（400 条）；索引分页不读候选"
        );
        assert_eq!(
            seeded.decoded_files(),
            items.len() * 2,
            "{suffix} 只解码本页 file facts（每 Media 2 集），实际 {}",
            seeded.decoded_files()
        );
        assert_eq!(seeded.decoded_media(), items.len());
        let (status, live) = get_with(&legacy, &url, &seeded.fixture.admin_token).await;
        assert_eq!(status, 200, "{suffix}: {live}");
        assert_eq!(page, live["data"], "{suffix} 投影页与旧全量选择必须一致");
        let _ = items;
    }
}

/// 省略 limit 仍是「返回全部」（既有行为不变），显式越界 limit 仍是 400。
#[tokio::test]
async fn omitted_limit_returns_every_visible_item() {
    let seeded = seed(30, 1);
    let all = seeded.warm().await;
    let url = format!("/api/v1/libraries/{}/items", seeded.library());
    let body = seeded.get(&url).await;
    assert_eq!(
        body.as_array().expect("array").len(),
        all.len(),
        "省略 limit 必须返回全部可见 Media"
    );
    let (status, _) = seeded
        .fixture
        .request(
            "GET",
            &format!("/api/v1/libraries/{}/items?limit=201", seeded.library()),
            Some(&seeded.fixture.admin_token),
        )
        .await;
    assert_eq!(status, 400, "limit 越界仍报错，不静默 clamp");
}

/// 页外损坏的 facts 不影响本页；选中页损坏时安全回退到旧全量读取。
#[tokio::test]
async fn unrelated_corrupt_facts_do_not_break_the_hot_page() {
    let seeded = seed(40, 1);
    let ids = seeded.warm().await;
    let library = seeded.library();
    let url = format!("/api/v1/libraries/{library}/items?limit=5&offset=0");

    // 页外（标题顺序的后半段）一条 facts 损坏：本页仍应零 IO 直接成功。
    let corrupt_media = *ids.last().expect("seeded");
    republish_with_corrupt_file(&seeded, corrupt_media);
    reset_counters(&seeded);
    let page = seeded.get(&url).await;
    assert_eq!(page.as_array().expect("array").len(), 5);
    assert_eq!(
        seeded.files.total(),
        0,
        "页外损坏的 facts 不得让热页回退到全量读取"
    );

    // 选中页内一条 facts 损坏：安全回退（200 + 旧语义），不返回错误页。
    let selected = page[0]["media_item_id"].as_str().expect("id").to_string();
    let selected_id: MediaId = selected.parse().expect("uuid");
    republish_with_corrupt_file(&seeded, selected_id);
    let (status, body) = seeded
        .fixture
        .request("GET", &url, Some(&seeded.fixture.admin_token))
        .await;
    assert_eq!(status, 200, "选中页 facts 损坏时必须安全回退：{body}");
    assert_eq!(body["data"].as_array().expect("array").len(), 5);
}

/// 把某个 Media 的 file facts 换成不可解析的旧版 JSON（其余行保持投影原值）。
fn republish_with_corrupt_file(seeded: &Seeded, media_id: MediaId) {
    let library = seeded.library();
    let store = seeded.fixture.state.store();
    let guard = store.lock();
    let snapshot = guard
        .browse_projection(&library)
        .unwrap()
        .expect("warm projection");
    let candidates = guard.browse_media_candidates(&library).unwrap();
    let mut files = snapshot.files;
    for file in &mut files {
        if file.media_id == media_id {
            file.facts = serde_json::json!({ "version": 0 });
        }
    }
    let build = store::ProjectionBuild {
        library_id: library,
        generation: snapshot.generation + 1,
        scope_signature: snapshot.scope_signature,
        revisions: snapshot.revisions,
        expected_dirty_token: None,
        files,
        media: snapshot.media,
        candidates,
    };
    assert!(guard.publish_browse_projection(&build).unwrap());
}

/// 固定页在整库（含无关 Media）从 2k 增长到 10k 时，解码行数与浏览 IO 都不增。
#[tokio::test]
async fn fixed_page_work_does_not_grow_with_unrelated_media() {
    let seeded = seed(2_000, 2);
    seeded.warm().await;
    let url = format!("/api/v1/libraries/{}/items?limit=20&offset=0", seeded.library());

    reset_counters(&seeded);
    let before = seeded.get(&url).await;
    let before_files = seeded.decoded_files();
    let before_media = seeded.decoded_media();
    assert_eq!(seeded.files.total(), 0, "2k 规模热页零浏览 IO");

    grow(&seeded, 2_000, 10_000, 2);
    seeded.warm().await;
    reset_counters(&seeded);
    let after = seeded.get(&url).await;
    assert_eq!(before, after, "无关 Media 增长不改变固定页输出");
    assert_eq!(
        seeded.decoded_files(),
        before_files,
        "固定页解码行数不随无关 files 增长: {before_files} -> {}",
        seeded.decoded_files()
    );
    assert_eq!(seeded.decoded_media(), before_media);
    assert_eq!(seeded.files.total(), 0, "10k 规模热页仍零浏览 IO");
}

/// 追加 `count` 个无关 TV Media（每个 `episodes` 集），不动已有行。
fn grow(seeded: &Seeded, from: usize, to: usize, episodes: u32) {
    let store = seeded.fixture.state.store();
    let guard = store.lock();
    for index in from..to {
        let media_id = MediaId::new();
        let title = format!("Show {index:05}");
        let tmdb_id = format!("{}", 100_000 + index);
        guard
            .insert_media(&Media {
                id: media_id,
                kind: MediaKind::Tv,
                title: title.clone(),
                year: Some(2000 + (index % 20) as u16),
                original_title: Some(title.clone()),
                aliases: Vec::new(),
                tmdb_id: Some(tmdb_id.clone()),
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .unwrap();
        for episode in 1..=episodes {
            guard
                .insert_ledger(&LedgerRow {
                    id: LedgerId::new(),
                    media_id,
                    path: seeded
                        .fixture
                        .root
                        .join(&title)
                        .join(format!("S01E{episode:02}.mkv"))
                        .display()
                        .to_string(),
                    season: Some(1),
                    episode: Some(episode),
                    resolution: Some("1080p".into()),
                    codec: Some("H264".into()),
                    hdr: None,
                    quality_source: QualitySource::Release,
                    confidence: Confidence::High,
                    filter_score: None,
                })
                .unwrap();
        }
        guard
            .put_catalog_cache(
                "tmdb",
                &format!("/tv/{tmdb_id}?language=zh-CN"),
                "{\"vote_average\":8.5,\"runtime\":101,\"original_language\":\"en\",\"genres\":[{\"id\":28}],\"origin_country\":[\"US\"]}",
                1000,
                None,
            )
            .unwrap();
    }
}

/// scope 变更后不得返回旧投影页：改根后原根下的 Media 不再属于该 Library。
#[tokio::test]
async fn scope_change_does_not_serve_the_old_page() {
    let seeded = seed(10, 1);
    seeded.warm().await;
    let url = format!("/api/v1/libraries/{}/items?limit=5&offset=0", seeded.library());
    assert_eq!(seeded.get(&url).await.as_array().expect("array").len(), 5);

    // 让 fixture 的 Library 不再是该 kind 的默认库，否则「不在任何根下」的行会
    // 回退到默认库、仍被返回，测不出 scope 变更。
    let other_root = seeded.fixture.dir.path().join("other");
    let third_root = seeded.fixture.dir.path().join("third");
    std::fs::create_dir_all(&other_root).unwrap();
    std::fs::create_dir_all(&third_root).unwrap();
    {
        let store = seeded.fixture.state.store();
        let guard = store.lock();
        let second = guard
            .create_library(
                MediaKind::Movie,
                "Other",
                &[other_root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        guard.set_default_library(&second.id).unwrap();
        guard
            .update_library(
                &seeded.library(),
                None,
                Some(&[third_root.to_str().unwrap()]),
                None,
            )
            .unwrap();
    }

    let after = seeded.get(&url).await;
    assert!(
        after.as_array().expect("array").is_empty(),
        "scope 变更后不得返回旧页: {after}"
    );
}
/// 用户 unit 变更不重建公共投影：热页仍只解码本页且零 IO。
#[tokio::test]
async fn playback_unit_write_keeps_the_hot_page_bounded() {
    let seeded = seed(50, 1);
    let ids = seeded.warm().await;
    let media_id = ids[0];
    let user_id = domain::UserId::new();
    let token = "page-member-token".to_string();
    {
        let store = seeded.fixture.state.store();
        let guard = store.lock();
        guard
            .insert_user(&domain::User {
                id: user_id,
                login: "page-member".into(),
                enabled: true,
                role: domain::UserRole::Member,
            })
            .unwrap();
        guard.set_user_token(user_id, &token).unwrap();
        guard
            .set_playback_state(user_id, media_id, 5_000, Some(true), Some(true), None, None)
            .unwrap();
    }
    let url = format!(
        "/api/v1/libraries/{}/items?w=watched&limit=10&offset=0",
        seeded.library()
    );
    reset_counters(&seeded);
    let (status, body) = get_with(&seeded.fixture.state, &url, &token).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(seeded.files.total(), 0, "watch 过滤热页零浏览 IO");
    assert!(
        seeded.decoded_files() <= 10,
        "watch 过滤也只解码本页 file facts，实际 {}",
        seeded.decoded_files()
    );
    assert_eq!(
        body["data"].as_array().expect("array").len(),
        1,
        "只有写了观看状态的那一条命中 w=watched"
    );
}

/// A4-02：批准与按页读取之间本库被标脏时，必须回退（重建/全量读取），
/// 不能用仍留在表里的旧 facts 出页。
#[tokio::test]
async fn page_read_rejects_a_head_that_changed_after_approval() {
    let seeded = seed(60, 2);
    seeded.warm().await;
    let url = format!("/api/v1/libraries/{}/items?limit=20&offset=0", seeded.library());

    // 测试 seam：head 批准之后、按页读取之前消费一条影响本库的变更（head 标脏、
    // dirty_token+1），旧 facts 仍留在 browse_files/browse_media 里。
    let barrier: api::browse_projection::PageApprovalBarrier = Arc::new(
        |state: &ApiState, library_id: &str| {
            let store = state.store();
            let guard = store.lock();
            guard
                .apply_browse_changes(
                    store::BrowseChangeSource::Library,
                    1,
                    &[library_id.to_string()],
                )
                .unwrap();
        },
    );
    let state = seeded.fixture.state.clone().with_projection_service(Arc::new(
        ProjectionService::new().with_page_approval_barrier(barrier),
    ));
    let seeded = Seeded {
        fixture: support::library_presence::PresenceFixture {
            state,
            ..seeded.fixture
        },
        files: seeded.files,
        titles: seeded.titles,
    };

    reset_counters(&seeded);
    let page = seeded.get(&url).await;
    assert_eq!(page.as_array().expect("array").len(), 20);
    assert_eq!(
        seeded.decoded_media(),
        60,
        "批准后 head 变了必须回退到全量路径（整库 media facts），不得按页用旧 facts 出页"
    );
    assert_eq!(seeded.candidate_rows(), 0, "回退路径不读轻候选");
}

/// A4-03：服务重启后的首个小页只用轻量读取覆盖依赖，不整库解码 file/media facts。
#[tokio::test]
async fn restart_hydrates_dependencies_without_decoding_whole_projections() {
    let seeded = seed(120, 2);
    seeded.warm().await;
    let url = format!("/api/v1/libraries/{}/items?limit=20&offset=0", seeded.library());

    // 重启：全新的 ProjectionService（内存依赖覆盖为空，持久投影仍 ready）。
    let state = seeded
        .fixture
        .state
        .clone()
        .with_projection_service(Arc::new(ProjectionService::new()));
    let seeded = Seeded {
        fixture: support::library_presence::PresenceFixture {
            state,
            ..seeded.fixture
        },
        files: seeded.files,
        titles: seeded.titles,
    };

    reset_counters(&seeded);
    let page = seeded.get(&url).await;
    assert_eq!(page.as_array().expect("array").len(), 20);
    assert_eq!(seeded.files.total(), 0, "重启后的热页仍零浏览文件 IO");
    assert_eq!(
        seeded.decoded_files(),
        40,
        "重启后的首个小页只解码本页 file facts，不整库解码"
    );
    assert_eq!(seeded.decoded_media(), 20);
    assert!(
        seeded.dependency_rows() >= 120 * 3,
        "依赖覆盖必须走轻量读取（120 个 Media 的 file 依赖列 + 轻候选），实际 {}",
        seeded.dependency_rows()
    );
}
