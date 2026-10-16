//! A3：NextUp 一次聚合（不为每个 series 重扫一遍）的协议级证据。
//!
//! 假 provider 的根列表是**分集粒度**的（一行一集）：一次列表读就拿到全部候选，
//! 聚合与分页都发生在这一次读之后。调用计数正是这个 bug 的判据——旧实现为每个
//! series 再查一次（`list_calls == 1 + series 数`）。
//!
//! 只返回 series 级快照的 provider 无法在一次读里拿到分集，因此默认实现会退回逐
//! series 查询（无界，见 `provider::query::legacy_next_up`）；生产 provider 覆盖
//! `next_up_items`，同样一次聚合。

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource, UserId};
use media_server::provider::{
    AuthenticateError, MediaItemMetadata, MediaItemSnapshot, MediaServerProvider, ServerLibrary,
    ServerUser,
};
use serde_json::Value;
use tower::ServiceExt;

const USER: &str = "00000000-0000-0000-0000-0000000000a1";
const SERIES: usize = 50;
const EPISODES: usize = 20;

fn user_id() -> UserId {
    UserId::from_str(USER).unwrap()
}

fn series_media_id(index: usize) -> MediaId {
    MediaId::from_str(&format!("00000000-0000-0000-0000-{:012}", index + 1)).unwrap()
}

fn episode_snapshot(series: usize, episode: u32, played: bool) -> MediaItemSnapshot {
    let media = Media {
        id: series_media_id(series),
        kind: MediaKind::Tv,
        title: format!("Series {series:02}"),
        year: Some(2020),
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let row = LedgerRow {
        id: LedgerId::new(),
        media_id: media.id,
        path: format!("/media/tv/Series{series:02}/S01E{episode:02}.mkv"),
        season: Some(1),
        episode: Some(episode),
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    };
    MediaItemSnapshot {
        row,
        media,
        ticks: 0,
        play_count: 0,
        played,
        unplayed_item_count: None,
        parent_id: series_media_id(series).to_string().replace('-', ""),
        is_series: false,
        chapters: Vec::new(),
        intro_start_ms: None,
        intro_end_ms: None,
        outro_start_ms: None,
        outro_end_ms: None,
        metadata: MediaItemMetadata::default(),
    }
}

/// 分集粒度的假 provider：`list_visible_items(None)` 返回全部可见分集，按 series 的
/// 父查询返回该 series 的分集。`list_calls` 是「有没有为每个 series 重扫」的判据。
struct EpisodeListProvider {
    items: Vec<MediaItemSnapshot>,
    list_calls: AtomicUsize,
    /// 这些 series 的第 1 集已看完（聚合时必须取下一集）。
    first_episode_played: Vec<usize>,
    /// 这些 series 全部看完（不出现在 NextUp）。
    fully_played: Vec<usize>,
}

impl EpisodeListProvider {
    fn new() -> Self {
        let mut items = Vec::new();
        for series in 0..SERIES {
            for episode in 1..=EPISODES as u32 {
                items.push(episode_snapshot(series, episode, false));
            }
        }
        Self {
            items,
            list_calls: AtomicUsize::new(0),
            first_episode_played: Vec::new(),
            fully_played: Vec::new(),
        }
    }

    fn list_calls(&self) -> usize {
        self.list_calls.load(Ordering::SeqCst)
    }

    fn is_played(&self, item: &MediaItemSnapshot) -> bool {
        let series = self.series_index(item);
        let episode = item.row.episode.unwrap_or_default();
        self.fully_played.contains(&series)
            || (self.first_episode_played.contains(&series) && episode == 1)
    }

    fn series_index(&self, item: &MediaItemSnapshot) -> usize {
        (0..SERIES)
            .find(|index| series_media_id(*index) == item.media.id)
            .unwrap_or(usize::MAX)
    }
}

#[async_trait::async_trait]
impl MediaServerProvider for EpisodeListProvider {
    fn server_id(&self) -> String {
        "next-up-server".into()
    }

    async fn authenticate_password(
        &self,
        _username: &str,
        _password: &str,
    ) -> Result<Option<(String, ServerUser)>, AuthenticateError> {
        Ok(None)
    }

    async fn user_id_by_token(&self, token: &str) -> Result<Option<UserId>, String> {
        Ok((token == "token-a").then(user_id))
    }

    async fn get_user(&self, requested: UserId) -> Result<Option<ServerUser>, String> {
        Ok((requested == user_id()).then(|| ServerUser {
            id: requested,
            login: "alice".into(),
            is_admin: false,
        }))
    }

    async fn list_visible_libraries(&self, _user_id: UserId) -> Result<Vec<ServerLibrary>, String> {
        Ok(vec![ServerLibrary {
            id: "lib-tv".into(),
            name: "TV".into(),
            kind: MediaKind::Tv,
            exclude_from_home: false,
            root_paths: vec![PathBuf::from("/media/tv")],
            cover_path: None,
            cover_tag: String::new(),
        }])
    }

    async fn list_visible_items(
        &self,
        _user_id: UserId,
        parent_id: Option<&str>,
    ) -> Result<Vec<MediaItemSnapshot>, String> {
        self.list_calls.fetch_add(1, Ordering::SeqCst);
        Ok(match parent_id {
            None => self
                .items
                .iter()
                .map(|item| MediaItemSnapshot {
                    played: self.is_played(item),
                    ..item.clone()
                })
                .collect(),
            Some(parent) => {
                let compact = parent.replace('-', "");
                self.items
                    .iter()
                    .filter(|item| item.media.id.to_string().replace('-', "") == compact)
                    .map(|item| MediaItemSnapshot {
                        played: self.is_played(item),
                        ..item.clone()
                    })
                    .collect()
            }
        })
    }

    async fn resolve_single_item(
        &self,
        _user_id: UserId,
        _id: &str,
    ) -> Result<Option<MediaItemSnapshot>, String> {
        Ok(None)
    }

    async fn resolve_cover_bytes(
        &self,
        _user_id: Option<UserId>,
        _id: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        Ok(None)
    }

    async fn resolve_poster_bytes(
        &self,
        _user_id: Option<UserId>,
        _id: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        Ok(None)
    }

    async fn resolve_stream_source(
        &self,
        _user_id: UserId,
        _id: &str,
    ) -> Result<Option<(PathBuf, bool)>, String> {
        Ok(None)
    }

    async fn set_user_marks(
        &self,
        _user_id: UserId,
        _id: &str,
        _played: Option<bool>,
        _favorite: Option<bool>,
    ) -> Result<bool, String> {
        Ok(true)
    }

    async fn update_progress(
        &self,
        _user_id: UserId,
        _id: &str,
        _position_ms: i64,
        _paused: bool,
    ) -> Result<(), String> {
        Ok(())
    }
}

async fn get(provider: Arc<EpisodeListProvider>, uri: &str) -> (StatusCode, Value) {
    let app = media_server::routes(provider);
    let request = Request::builder()
        .method("GET")
        .uri(uri)
        .header("authorization", "Bearer token-a")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

#[tokio::test]
async fn next_up_does_not_rescan_for_each_series() {
    let provider = Arc::new(EpisodeListProvider::new());
    let (status, body) = get(provider.clone(), "/Shows/NextUp?Limit=1").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["TotalRecordCount"], SERIES);
    assert_eq!(body["Items"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        provider.list_calls(),
        1,
        "一次聚合只能有一次列表读（旧实现为每个 series 再查一次：{} 次）",
        provider.list_calls()
    );
}

#[tokio::test]
async fn next_up_picks_the_first_unwatched_episode_per_series() {
    let mut provider = EpisodeListProvider::new();
    provider.first_episode_played = vec![0, 7];
    provider.fully_played = vec![3];
    let provider = Arc::new(provider);
    let (status, body) = get(provider.clone(), "/Shows/NextUp").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["TotalRecordCount"], SERIES - 1, "整部看完的剧不出条目");
    let items = body["Items"].as_array().unwrap();
    let first = &items[0];
    assert_eq!(first["ParentId"], series_media_id(0).to_string().replace('-', ""));
    assert_eq!(first["IndexNumber"], 2, "第 1 集已看完时取下一集");
    let seventh = items
        .iter()
        .find(|item| item["ParentId"] == series_media_id(7).to_string().replace('-', ""))
        .expect("series 7 entry");
    assert_eq!(seventh["IndexNumber"], 2);
    assert_eq!(provider.list_calls(), 1);
}

#[tokio::test]
async fn next_up_pages_after_aggregating_every_series() {
    let provider = Arc::new(EpisodeListProvider::new());
    let (status, body) = get(provider.clone(), "/Shows/NextUp?StartIndex=48&Limit=5").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["TotalRecordCount"], SERIES, "total 是聚合后的完整条数");
    assert_eq!(body["Items"].as_array().map(Vec::len), Some(2));
    let (status, beyond) = get(provider.clone(), "/Shows/NextUp?StartIndex=500").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(beyond["Items"].as_array().map(Vec::len), Some(0));
    assert_eq!(beyond["TotalRecordCount"], SERIES);
    assert_eq!(provider.list_calls(), 2, "每次请求一次聚合读");
}

#[tokio::test]
async fn next_up_with_series_id_reads_only_that_series() {
    let provider = Arc::new(EpisodeListProvider::new());
    let series = series_media_id(11).to_string().replace('-', "");
    let (status, body) = get(provider.clone(), &format!("/Shows/NextUp?SeriesId={series}")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["TotalRecordCount"], 1);
    assert_eq!(
        body["Items"][0]["ParentId"],
        series_media_id(11).to_string().replace('-', "")
    );
    assert_eq!(body["Items"][0]["IndexNumber"], 1);
    assert_eq!(provider.list_calls(), 1);
}
