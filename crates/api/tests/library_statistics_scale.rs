//! A5 Library 统计：一次归属聚合与可见刷新状态（API-03）。
//!
//! 断言的是**工作量**与**统计值**本身，不只比响应大小：全局账本读取不随库数重复、
//! 热统计不逐文件 `stat`、体积只来自投影 facts（`.strm` 指针记 0）、刷新状态取自
//! 投影 head。
//!
//! 注入的 [`FakeSizedFiles`] 把每个媒体文件的元数据固定成一个 1 MiB 填充文件：旧
//! 实现直接 `std::fs::metadata(path)`（这些媒体文件在磁盘上并不存在 → 0 字节），
//! 所以「体积必须来自投影 facts」在旧实现上是红的；同时统计它的调用次数，用来
//! 证明热请求不再逐文件 `stat`。

#[path = "support/mod.rs"]
mod support;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use api::browse_file_read::BrowseFileRead;
use api::browse_projection::ProjectionService;
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
use serde_json::Value;
use support::library_presence::PresenceFixture;

/// 注入 seam 里每个媒体文件的固定体积。
const FAKE_SIZE: u64 = 1024 * 1024;

/// 固定假大小 + 调用计数的文件读取 seam（详见模块注释）。
struct FakeSizedFiles {
    filler: PathBuf,
    metadata_calls: AtomicUsize,
}

impl FakeSizedFiles {
    fn new(filler: PathBuf) -> Self {
        Self {
            filler,
            metadata_calls: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.metadata_calls.load(Ordering::SeqCst)
    }

    fn reset(&self) {
        self.metadata_calls.store(0, Ordering::SeqCst);
    }
}

impl BrowseFileRead for FakeSizedFiles {
    fn read_nfo(&self, _path: &Path) -> Option<library::NfoMeta> {
        None
    }

    fn metadata(&self, _path: &Path) -> std::io::Result<std::fs::Metadata> {
        self.metadata_calls.fetch_add(1, Ordering::SeqCst);
        std::fs::metadata(&self.filler)
    }

    fn is_file(&self, _path: &Path) -> bool {
        false
    }
}

/// 一个 Library 的期望统计（由 fixture 自己算出，不读被测实现）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Expected {
    item_count: u64,
    file_count: u64,
    total_size_bytes: u64,
    strm_file_count: u64,
}

/// 一次 `/libraries` 请求观察到的文件/DB 工作量。
#[derive(Clone, Copy, Debug)]
struct Work {
    /// 无 WHERE 的根账本扫描步数（`list_ledger`）。
    ledger_scans: usize,
    /// 媒体文件 `stat` 次数（注入 seam）。
    media_stats: usize,
}

struct StatsFixture {
    fixture: PresenceFixture,
    files: Arc<FakeSizedFiles>,
    expected: HashMap<String, Expected>,
}

/// 建库：注入假大小 seam 与启用投影服务，但只种一个空库（无台账）。
fn empty_fixture() -> StatsFixture {
    let fixture = PresenceFixture::new();
    let filler = fixture.dir.path().join("filler.bin");
    std::fs::write(&filler, vec![0u8; FAKE_SIZE as usize]).unwrap();
    let files = Arc::new(FakeSizedFiles::new(filler));
    let state = fixture
        .state
        .clone()
        .with_browse_file_read(files.clone())
        .with_projection_service(Arc::new(ProjectionService::new()));
    StatsFixture {
        fixture: PresenceFixture { state, ..fixture },
        files,
        expected: HashMap::new(),
    }
}

/// 在 `root` 下种 `media_count` 部 Movie、每部 `files_per_media` 个文件；第
/// `strm_every` 个（1-based）写成 `.strm` 指针（`0` = 全部写成真实文件）。
fn seed_rows(
    store: &store::Store,
    root: &Path,
    tag: &str,
    kind: MediaKind,
    media_count: usize,
    files_per_media: usize,
    strm_every: usize,
) -> Expected {
    let mut expected = Expected {
        item_count: media_count as u64,
        file_count: 0,
        total_size_bytes: 0,
        strm_file_count: 0,
    };
    for index in 0..media_count {
        let media_id = MediaId::new();
        let title = format!("{tag}-{index:05}");
        store
            .insert_media(&Media {
                id: media_id,
                kind,
                title: title.clone(),
                original_title: None,
                aliases: Vec::new(),
                year: Some(2000),
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .unwrap();
        for file in 0..files_per_media {
            let ordinal = index * files_per_media + file + 1;
            let strm = strm_every > 0 && ordinal % strm_every == 0;
            let name = if strm {
                format!("{title}.{file}.strm")
            } else {
                format!("{title}.{file}.mkv")
            };
            store
                .insert_ledger(&LedgerRow {
                    id: LedgerId::new(),
                    media_id,
                    path: root.join(name).display().to_string(),
                    season: None,
                    episode: None,
                    resolution: Some("1080p".into()),
                    codec: Some("H264".into()),
                    hdr: None,
                    quality_source: QualitySource::Release,
                    confidence: Confidence::High,
                    filter_score: None,
                })
                .unwrap();
            expected.file_count += 1;
            if strm {
                expected.strm_file_count += 1;
            } else {
                expected.total_size_bytes += FAKE_SIZE;
            }
        }
    }
    expected
}

/// 建一个新建库：`name`、`kind`、单个 `root`（目录会建出来）。
fn new_library(
    store: &store::Store,
    dir: &Path,
    name: &str,
    kind: MediaKind,
    access_mode: &str,
    admin_visible: bool,
) -> (String, PathBuf) {
    let root = dir.join(name.replace(' ', "-").to_lowercase());
    std::fs::create_dir_all(&root).unwrap();
    let root_str = root.display().to_string();
    let library = store
        .create_library(kind, name, &[root_str.as_str()], access_mode, admin_visible, &[])
        .unwrap();
    (library.id, root)
}

/// `libraries` 个 Movie 库，每库 `media_per_library` 部作品 × `files_per_media` 个文件。
fn seed(libraries: usize, media_per_library: usize, files_per_media: usize, strm_every: usize) -> StatsFixture {
    let mut seeded = empty_fixture();
    let mut expected = HashMap::new();
    {
        let store_arc = seeded.fixture.state.store();
        let guard = store_arc.lock();
        expected.insert(
            seeded.fixture.library_id.clone(),
            seed_rows(
                &guard,
                &seeded.fixture.root,
                "A",
                MediaKind::Movie,
                media_per_library,
                files_per_media,
                strm_every,
            ),
        );
        for index in 1..libraries {
            let (id, root) = new_library(
                &guard,
                seeded.fixture.dir.path(),
                &format!("Movies {index}"),
                MediaKind::Movie,
                "everyone",
                true,
            );
            expected.insert(
                id,
                seed_rows(
                    &guard,
                    &root,
                    &format!("B{index}"),
                    MediaKind::Movie,
                    media_per_library,
                    files_per_media,
                    strm_every,
                ),
            );
        }
    }
    seeded.expected = expected;
    seeded
}

impl StatsFixture {
    async fn list(&self) -> Value {
        let (status, body) = self
            .fixture
            .request("GET", "/api/v1/libraries", Some(&self.fixture.admin_token))
            .await;
        assert_eq!(status, 200, "{body}");
        body["data"].clone()
    }

    /// 重置计数器 → 一次 `/libraries` → 读数。
    async fn list_with_work(&self) -> (Value, Work) {
        self.fixture.state.store().lock().reset_root_ledger_scans();
        self.files.reset();
        let body = self.list().await;
        let work = Work {
            ledger_scans: self.fixture.state.store().lock().root_ledger_scans(),
            media_stats: self.files.calls(),
        };
        eprintln!(
            "work: ledger_scans={} media_stats={}",
            work.ledger_scans, work.media_stats
        );
        (body, work)
    }

    fn stats(body: &Value, library_id: &str) -> Value {
        body.as_array()
            .expect("library list must be an array")
            .iter()
            .find(|library| library["id"] == library_id)
            .unwrap_or_else(|| panic!("library {library_id} must be listed"))["stats"]
            .clone()
    }

    /// 每库统计必须与 fixture 自己算出的期望逐字段一致。
    fn assert_stats(&self, body: &Value) {
        for (id, expected) in &self.expected {
            let stats = Self::stats(body, id);
            assert_eq!(stats["item_count"].as_u64(), Some(expected.item_count), "item_count {id}");
            assert_eq!(stats["file_count"].as_u64(), Some(expected.file_count), "file_count {id}");
            assert_eq!(
                stats["total_size_bytes"].as_u64(),
                Some(expected.total_size_bytes),
                "total_size_bytes {id}"
            );
            assert_eq!(
                stats["strm_file_count"].as_u64(),
                Some(expected.strm_file_count),
                "strm_file_count {id}"
            );
            assert_eq!(stats["stale"], false, "投影已就绪且无变化时不得标 stale {id}");
            assert!(
                stats["refreshed_at_ms"].as_i64().is_some_and(|ms| ms > 0),
                "refreshed_at_ms 必须是真实发布时间 {id}: {stats}"
            );
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// 一次聚合：同样的账本总量（600 行）下，20 个库不得比 1 个库多扫全局账本。
#[tokio::test]
async fn library_list_does_not_repeat_global_reads_per_library() {
    let one = seed(1, 600, 1, 0);
    let many = seed(20, 30, 1, 0);
    let (one_body, one_work) = one.list_with_work().await;
    let (many_body, many_work) = many.list_with_work().await;
    one.assert_stats(&one_body);
    many.assert_stats(&many_body);
    assert_eq!(
        many_work.ledger_scans,
        one_work.ledger_scans,
        "20 个库不得比 1 个库多扫全局账本：1 库 {} 步、20 库 {} 步",
        one_work.ledger_scans,
        many_work.ledger_scans
    );
}

/// 热统计不逐文件 stat：冷请求一次有界构建后，第二次请求零媒体文件 stat，统计不变。
#[tokio::test]
async fn hot_statistics_do_not_stat_each_media_file() {
    let seeded = seed(1, 10, 4, 4);
    let library = seeded.fixture.library_id.clone();
    let before_ms = now_ms();
    let (cold_body, cold_work) = seeded.list_with_work().await;
    let after_ms = now_ms();
    assert!(
        cold_work.media_stats >= 40,
        "首次无 facts 必须走一次有界构建（逐文件 stat），实际 {}",
        cold_work.media_stats
    );
    let cold = StatsFixture::stats(&cold_body, &library);
    assert_eq!(cold["item_count"].as_u64(), Some(10));
    assert_eq!(cold["file_count"].as_u64(), Some(40));
    assert_eq!(
        cold["total_size_bytes"].as_u64(),
        Some(30 * FAKE_SIZE),
        "体积必须来自投影 facts，且 .strm 指针记 0：{cold}"
    );
    assert_eq!(cold["strm_file_count"].as_u64(), Some(10));
    assert_eq!(cold["stale"], false, "构建完成后不得标 stale：{cold}");
    let refreshed = cold["refreshed_at_ms"].as_i64().expect("refreshed_at_ms");
    assert!(
        refreshed >= before_ms && refreshed <= after_ms,
        "refreshed_at_ms 必须是本次发布的真实时间：{refreshed}"
    );

    // 两次请求之间文件没有任何变化：热请求必须零媒体文件 stat。
    let (hot_body, hot_work) = seeded.list_with_work().await;
    assert_eq!(hot_work.media_stats, 0, "热统计不得 stat 每个媒体文件");
    let hot = StatsFixture::stats(&hot_body, &library);
    assert_eq!(hot, cold, "无变化时两次请求的统计必须逐字段一致");
}

/// `.strm` 指针文件只有几十字节 URL 文本，不计入物理体积。
#[tokio::test]
async fn strm_pointer_files_do_not_add_physical_size() {
    let seeded = seed(1, 4, 5, 5);
    let library = seeded.fixture.library_id.clone();
    let body = seeded.list().await;
    let stats = StatsFixture::stats(&body, &library);
    assert_eq!(stats["file_count"].as_u64(), Some(20));
    assert_eq!(stats["strm_file_count"].as_u64(), Some(4));
    assert_eq!(stats["total_size_bytes"].as_u64(), Some(16 * FAKE_SIZE));
}

/// 同一部作品在多库各有拷贝：每库各自计入，不做全局去重后串库。
#[tokio::test]
async fn same_media_in_two_libraries_is_counted_per_library() {
    let seeded = empty_fixture();
    let first = seeded.fixture.library_id.clone();
    let second;
    {
        let store_arc = seeded.fixture.state.store();
        let guard = store_arc.lock();
        let (id, root) = new_library(
            &guard,
            seeded.fixture.dir.path(),
            "Second",
            MediaKind::Movie,
            "everyone",
            true,
        );
        second = id;
        let media_id = MediaId::new();
        guard
            .insert_media(&Media {
                id: media_id,
                kind: MediaKind::Movie,
                title: "Shared".into(),
                original_title: None,
                aliases: Vec::new(),
                year: Some(1999),
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .unwrap();
        for path in [seeded.fixture.root.join("Shared.mkv"), root.join("Shared.mkv")] {
            guard
                .insert_ledger(&LedgerRow {
                    id: LedgerId::new(),
                    media_id,
                    path: path.display().to_string(),
                    season: None,
                    episode: None,
                    resolution: Some("1080p".into()),
                    codec: Some("H264".into()),
                    hdr: None,
                    quality_source: QualitySource::Release,
                    confidence: Confidence::High,
                    filter_score: None,
                })
                .unwrap();
        }
    }
    let body = seeded.list().await;
    let first_stats = StatsFixture::stats(&body, &first);
    let second_stats = StatsFixture::stats(&body, &second);
    for (id, stats) in [(&first, &first_stats), (&second, &second_stats)] {
        assert_eq!(stats["item_count"].as_u64(), Some(1), "每库各自计入同一部作品 {id}");
        assert_eq!(stats["file_count"].as_u64(), Some(1), "每库文件数必须独立 {id}");
        assert_eq!(stats["total_size_bytes"].as_u64(), Some(FAKE_SIZE), "每库体积必须独立 {id}");
    }
}

/// 隐藏 / 冻结（removal 中）的库不得把自己的行混进可见库统计；管理员管理范围仍
/// 能看到冻结库的真实计数。
#[tokio::test]
async fn hidden_and_blocked_libraries_do_not_leak_into_visible_stats() {
    let seeded = empty_fixture();
    let visible = seeded.fixture.library_id.clone();
    let hidden;
    let blocked;
    {
        let store_arc = seeded.fixture.state.store();
        let guard = store_arc.lock();
        seed_rows(&guard, &seeded.fixture.root, "V", MediaKind::Movie, 2, 1, 0);
        let (id, root) = new_library(
            &guard,
            seeded.fixture.dir.path(),
            "Hidden",
            MediaKind::Movie,
            "selected",
            false,
        );
        hidden = id;
        seed_rows(&guard, &root, "H", MediaKind::Movie, 3, 1, 0);
        let (id, root) = new_library(
            &guard,
            seeded.fixture.dir.path(),
            "Blocked",
            MediaKind::Movie,
            "everyone",
            true,
        );
        blocked = id;
        seed_rows(&guard, &root, "B", MediaKind::Movie, 4, 1, 0);
        guard.begin_library_removal(&blocked).unwrap();
    }
    let (status, body) = seeded
        .fixture
        .request("GET", "/api/v1/libraries", Some(&seeded.fixture.member_token))
        .await;
    assert_eq!(status, 200, "{body}");
    let listed: Vec<&str> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|library| library["id"].as_str())
        .collect();
    assert!(!listed.contains(&hidden.as_str()), "非成员库不得出现在成员列表");
    assert!(!listed.contains(&blocked.as_str()), "removal 中的库不得出现在成员列表");
    assert_eq!(
        StatsFixture::stats(&body["data"], &visible)["item_count"].as_u64(),
        Some(2),
        "不可见库的行不得混进可见库统计"
    );
    let (status, admin_body) = seeded
        .fixture
        .request(
            "GET",
            "/api/v1/libraries?scope=admin",
            Some(&seeded.fixture.admin_token),
        )
        .await;
    assert_eq!(status, 200, "{admin_body}");
    assert_eq!(
        StatsFixture::stats(&admin_body["data"], &blocked)["item_count"].as_u64(),
        Some(4),
        "管理范围仍要给出冻结库的真实计数"
    );
}

/// 嵌套根由最长根拥有；并列根（同一路径两个库）谁也不拥有，不串库。
#[tokio::test]
async fn nested_and_tied_roots_do_not_steal_each_other() {
    let seeded = empty_fixture();
    let parent = seeded.fixture.library_id.clone();
    let child;
    let tied_a;
    let tied_b;
    {
        let store_arc = seeded.fixture.state.store();
        let guard = store_arc.lock();
        seed_rows(&guard, &seeded.fixture.root, "P", MediaKind::Movie, 1, 1, 0);
        let nested = seeded.fixture.root.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        let nested_str = nested.display().to_string();
        child = guard
            .create_library(
                MediaKind::Movie,
                "Nested",
                &[nested_str.as_str()],
                "everyone",
                true,
                &[],
            )
            .unwrap()
            .id;
        seed_rows(&guard, &nested, "C", MediaKind::Movie, 1, 1, 0);
        let tied = seeded.fixture.dir.path().join("tied");
        std::fs::create_dir_all(&tied).unwrap();
        let tied_str = tied.display().to_string();
        let mut ids = Vec::new();
        for name in ["Tied A", "Tied B"] {
            ids.push(
                guard
                    .create_library(
                        MediaKind::Movie,
                        name,
                        &[tied_str.as_str()],
                        "everyone",
                        true,
                        &[],
                    )
                    .unwrap()
                    .id,
            );
        }
        tied_a = ids.remove(0);
        tied_b = ids.remove(0);
        seed_rows(&guard, &tied, "T", MediaKind::Movie, 2, 1, 0);
    }
    let body = seeded.list().await;
    assert_eq!(StatsFixture::stats(&body, &parent)["file_count"].as_u64(), Some(1), "父库只拥有自己的行");
    assert_eq!(StatsFixture::stats(&body, &child)["file_count"].as_u64(), Some(1), "嵌套子库拥有更深根下的行");
    for id in [&tied_a, &tied_b] {
        assert_eq!(
            StatsFixture::stats(&body, id)["file_count"].as_u64(),
            Some(0),
            "并列根谁也不拥有 {id}"
        );
    }
}

/// 投影不可用时统计不得返回假 0：计数仍来自台账，只是标 stale。
#[tokio::test]
async fn cold_statistics_are_not_fake_zero() {
    let seeded = empty_fixture();
    let library = seeded.fixture.library_id.clone();
    {
        let store_arc = seeded.fixture.state.store();
        let guard = store_arc.lock();
        seed_rows(&guard, &seeded.fixture.root, "A", MediaKind::Movie, 3, 2, 0);
    }
    // 不先构建投影：库从未发布过事实，才谈得上「无可用投影」。
    let state = seeded
        .fixture
        .state
        .clone()
        .with_projection_service(Arc::new(ProjectionService::disabled()));
    let fixture = PresenceFixture { state, ..seeded.fixture };
    let (status, body) = fixture
        .request("GET", "/api/v1/libraries", Some(&fixture.admin_token))
        .await;
    assert_eq!(status, 200, "{body}");
    let stats = StatsFixture::stats(&body["data"], &library);
    assert_eq!(stats["item_count"].as_u64(), Some(3), "冷统计作品数必须来自台账：{stats}");
    assert_eq!(stats["file_count"].as_u64(), Some(6), "冷统计文件数必须来自台账：{stats}");
    assert_eq!(stats["strm_file_count"].as_u64(), Some(0), "冷统计 strm 文件数必须精确：{stats}");
    assert_eq!(stats["total_size_bytes"].as_u64(), Some(0), "无可用投影时体积必须为零：{stats}");
    assert_eq!(stats["stale"], true, "投影不可用时必须标记 stale：{stats}");
    // 从未发布过投影的库没有发布时间；有历史发布的库由 scope 变更用例覆盖。
    assert_eq!(stats["refreshed_at_ms"].as_i64(), Some(0), "从未发布的库发布时间必须为 0：{stats}");
}

/// 配置范围改变后不得复用旧统计：计数仍来自台账，但旧 scope 的体积一律不复用。
#[tokio::test]
async fn library_scope_change_does_not_reuse_old_statistics() {
    let seeded = empty_fixture();
    let tv_other;
    let old_root;
    {
        let store_arc = seeded.fixture.state.store();
        let guard = store_arc.lock();
        let (id, root) = new_library(
            &guard,
            seeded.fixture.dir.path(),
            "TV Other",
            MediaKind::Tv,
            "everyone",
            true,
        );
        tv_other = id;
        seed_rows(&guard, &root, "T", MediaKind::Tv, 2, 1, 0);
        old_root = root;
    }
    let before = seeded.list().await;
    let other_before = StatsFixture::stats(&before, &tv_other);
    assert_eq!(other_before["file_count"].as_u64(), Some(2));
    assert_eq!(other_before["total_size_bytes"].as_u64(), Some(2 * FAKE_SIZE));
    assert_eq!(other_before["stale"], false, "{other_before}");
    let refreshed_before = other_before["refreshed_at_ms"].as_i64().unwrap();

    // 改范围：给同一个库再加一个空根（既有根保留，台账行不动）。
    let extra = seeded.fixture.dir.path().join("tv-other-extra");
    std::fs::create_dir_all(&extra).unwrap();
    let old_str = old_root.display().to_string();
    let extra_str = extra.display().to_string();
    seeded
        .fixture
        .state
        .store()
        .lock()
        .update_library_full(
            &tv_other,
            None,
            Some(&[old_str.as_str(), extra_str.as_str()]),
            None,
            None,
            None,
        )
        .unwrap();

    let after = seeded.list().await;
    let other_after = StatsFixture::stats(&after, &tv_other);
    assert_eq!(other_after["item_count"].as_u64(), Some(2), "scope 变化后作品数仍必须来自台账：{other_after}");
    assert_eq!(other_after["file_count"].as_u64(), Some(2), "scope 变化后文件数仍必须来自台账：{other_after}");
    assert_eq!(other_after["strm_file_count"].as_u64(), Some(0), "scope 变化后 strm 文件数仍必须来自台账：{other_after}");
    assert_eq!(other_after["total_size_bytes"].as_u64(), Some(0), "旧 scope 的体积不得复用：{other_after}");
    assert_eq!(other_after["stale"], true, "scope 变化必须标 stale：{other_after}");
    assert!(
        other_after["refreshed_at_ms"].as_i64().unwrap() >= refreshed_before,
        "refreshed_at_ms 必须是真实刷新时间，不得回退"
    );
}
/// A same-cardinality ledger path change invalidates the old physical-byte facts.
#[tokio::test]
#[ignore = "open A5-02: statistics do not yet consume the ledger journal for same-count path changes (journal_ready does not mark this library dirty); product fix pending"]
async fn same_count_strm_change_does_not_reuse_physical_bytes_as_fresh() {
    let seeded = seed(1, 1, 1, 0);
    let before = seeded.list().await;
    let library_id = &seeded.fixture.library_id;
    let old = StatsFixture::stats(&before, library_id);
    assert_eq!(old["total_size_bytes"].as_u64(), Some(FAKE_SIZE));
    assert_eq!(old["stale"], false, "initial published facts are current");

    let from = seeded.fixture.root.join("A-00000.0.mkv");
    let to = seeded.fixture.root.join("A-00000.0.strm");
    let store = seeded.fixture.state.store();
    assert!(store.lock().rename_ledger_path(&from.display().to_string(), &to.display().to_string()).unwrap());

    let after = seeded.list().await;
    let stats = StatsFixture::stats(&after, library_id);
    assert_eq!(stats["file_count"].as_u64(), Some(1));
    assert_eq!(stats["strm_file_count"].as_u64(), Some(1));
    assert_ne!(
        (stats["total_size_bytes"].as_u64(), stats["stale"].as_bool()),
        (Some(FAKE_SIZE), Some(false)),
        "same-count .strm replacement must not present prior physical bytes as fresh: {stats}"
    );
}

/// Roots changing after collection cannot pair the old observation with fresh stats.
#[tokio::test]
#[ignore = "open A5-03: fixture cannot reconfigure roots while ledger files exist (Protected); needs an empty-library fixture before this can run"]
async fn roots_change_between_plan_and_totals_never_returns_fresh_old_stats() {
    let mut seeded = seed(1, 1, 1, 0);
    let new_root = seeded.fixture.dir.path().join("replacement-root");
    std::fs::create_dir_all(&new_root).unwrap();
    let store = seeded.fixture.state.store();
    let library_id = seeded.fixture.library_id.clone();
    let root_text = new_root.display().to_string();
    let state = seeded.fixture.state.clone();
    let barrier_store = store.clone();
    seeded.fixture.state.statistics_phase_barrier = Some(Arc::new(move || {
        barrier_store.lock().update_library_full(
            &library_id,
            None,
            Some(&[root_text.as_str()]),
            None,
            None,
            None,
        ).unwrap();
    }));

    let response = seeded.list().await;
    let library = response.as_array().unwrap().iter()
        .find(|library| library["id"] == seeded.fixture.library_id).unwrap();
    assert_eq!(library["root_paths"][0], seeded.fixture.root.display().to_string());
    assert_eq!(library["stats"]["stale"], true, "roots changed after plan collection: {}", library["stats"]);
    drop(state);
}

/// A library created and populated after roster collection is not rendered as fresh zeroes.
#[tokio::test]
async fn library_created_after_roster_collection_is_not_rendered_as_fresh_zeroes() {
    let mut seeded = empty_fixture();
    let state = seeded.fixture.state.clone();
    let dir = seeded.fixture.dir.path().to_path_buf();
    seeded.fixture.state.statistics_phase_barrier = Some(Arc::new(move || {
        let store = state.store();
        let guard = store.lock();
        let (id, root) = new_library(
            &guard, &dir, "Concurrent Library", MediaKind::Movie, "everyone", true,
        );
        let expected = seed_rows(&guard, &root, "Concurrent", MediaKind::Movie, 1, 1, 0);
        assert_eq!(expected.file_count, 1);
        assert!(!id.is_empty());
    }));
    let response = seeded.list().await;
    assert!(
        !response.as_array().unwrap().iter().any(|library| library["name"] == "Concurrent Library"),
        "newly created library was not in the request's captured roster"
    );
}
/// The authenticated ordinary roster resolves the caller role once, independent of N.
#[tokio::test]
async fn ordinary_library_list_performs_one_role_lookup_for_many_libraries() {
    let seeded = seed(8, 0, 0, 0);
    {
        let store = seeded.fixture.state.store();
        store.lock().reset_role_lookups();
    }
    let (status, body) = seeded.fixture.request(
        "GET", "/api/v1/libraries", Some(&seeded.fixture.member_token),
    ).await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    assert_eq!(seeded.fixture.state.store().lock().role_lookups(), 1);
}
