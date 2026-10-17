//! A3-04 / A3-06：默认（未覆盖）选择路径的 folder 归属与 NextUp 种类规则。
//!
//! 生产 provider 覆盖了这两条路径；这里直接打 trait 默认实现，证明旧实现不再
//! 用折叠后的代表行决定 folder 成员，也不再把显式请求的电影丢掉。

use std::path::PathBuf;
use std::str::FromStr;

use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource, UserId};
use media_server::provider::query::{folder_id_for_root, legacy_next_up, legacy_select};
use media_server::provider::{
    AuthenticateError, MediaItemMetadata, MediaItemSnapshot, MediaServerProvider, ServerItemQuery,
    ServerLibrary, ServerUser,
};

const USER: &str = "00000000-0000-0000-0000-0000000000a1";

fn user() -> UserId {
    UserId::from_str(USER).unwrap()
}

fn media(kind: MediaKind, title: &str) -> Media {
    Media {
        id: MediaId::new(),
        kind,
        title: title.into(),
        year: Some(2020),
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

fn row(media_id: MediaId, path: &str, season: Option<u32>, episode: Option<u32>) -> LedgerRow {
    LedgerRow {
        id: LedgerId::new(),
        media_id,
        path: path.into(),
        season,
        episode,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    }
}

fn snapshot(item_media: Media, item_row: LedgerRow, is_series: bool) -> MediaItemSnapshot {
    MediaItemSnapshot {
        parent_id: "lib".into(),
        row: item_row,
        media: item_media,
        ticks: 0,
        play_count: 0,
        played: false,
        unplayed_item_count: None,
        is_series,
        chapters: Vec::new(),
        intro_start_ms: None,
        intro_end_ms: None,
        outro_start_ms: None,
        outro_end_ms: None,
        metadata: MediaItemMetadata::default(),
    }
}

/// 根列表只给折叠后的剧（代表行在 root A）；按 series 查询才给出两集。
struct SplitRootProvider {
    show: Media,
    episode_a: MediaItemSnapshot,
    episode_b: MediaItemSnapshot,
    movie: MediaItemSnapshot,
    collapsed_b: MediaItemSnapshot,
}

impl SplitRootProvider {
    fn new() -> Self {
        let show = media(MediaKind::Tv, "Split show");
        let episode_a = snapshot(
            show.clone(),
            row(show.id, "/media/root-a/Show/S01E01.mkv", Some(1), Some(1)),
            false,
        );
        let episode_b = snapshot(
            show.clone(),
            row(show.id, "/media/root-b/Show/S01E02.mkv", Some(1), Some(2)),
            false,
        );
        let movie_media = media(MediaKind::Movie, "Requested movie");
        let movie = snapshot(
            movie_media.clone(),
            row(movie_media.id, "/media/movies/Requested.mkv", None, None),
            false,
        );
        let other = media(MediaKind::Tv, "Collapsed other");
        let collapsed_b = snapshot(
            other.clone(),
            row(other.id, "/media/root-b/Other", None, None),
            true,
        );
        Self {
            show,
            episode_a,
            episode_b,
            movie,
            collapsed_b,
        }
    }
}

#[async_trait::async_trait]
impl MediaServerProvider for SplitRootProvider {
    fn server_id(&self) -> String {
        "legacy".into()
    }

    async fn authenticate_password(
        &self,
        _: &str,
        _: &str,
    ) -> Result<Option<(String, ServerUser)>, AuthenticateError> {
        Ok(None)
    }

    async fn user_id_by_token(&self, _: &str) -> Result<Option<UserId>, String> {
        Ok(Some(user()))
    }

    async fn get_user(&self, id: UserId) -> Result<Option<ServerUser>, String> {
        Ok(Some(ServerUser {
            id,
            login: "me".into(),
            is_admin: true,
        }))
    }

    async fn list_visible_libraries(&self, _: UserId) -> Result<Vec<ServerLibrary>, String> {
        Ok(vec![ServerLibrary {
            id: "lib-tv".into(),
            name: "TV".into(),
            kind: MediaKind::Tv,
            exclude_from_home: false,
            root_paths: vec![PathBuf::from("/media/root-a"), PathBuf::from("/media/root-b")],
            cover_path: None,
            cover_tag: "tag".into(),
        }])
    }

    async fn list_visible_items(
        &self,
        _: UserId,
        parent_id: Option<&str>,
    ) -> Result<Vec<MediaItemSnapshot>, String> {
        let series_id = self.show.id.to_string().replace('-', "");
        if parent_id.is_some_and(|id| id.replace('-', "") == series_id) {
            return Ok(vec![self.episode_a.clone(), self.episode_b.clone()]);
        }
        if parent_id.is_some_and(|id| id.replace('-', "") == self.movie.media.id.to_string().replace('-', ""))
        {
            return Ok(vec![self.movie.clone()]);
        }
        if parent_id.is_some_and(|id| {
            id.replace('-', "") == self.collapsed_b.media.id.to_string().replace('-', "")
        }) {
            return Ok(vec![snapshot(
                self.collapsed_b.media.clone(),
                row(
                    self.collapsed_b.media.id,
                    "/media/root-b/Other/S01E01.mkv",
                    Some(1),
                    Some(1),
                ),
                false,
            )]);
        }
        // 根列表：代表行在 root A 的折叠剧 + root B 的另一部折叠剧 + 电影。
        let mut series = self.episode_a.clone();
        series.is_series = true;
        series.row.path = "/media/root-a/Show".into();
        Ok(vec![series, self.collapsed_b.clone(), self.movie.clone()])
    }

    async fn resolve_single_item(
        &self,
        _: UserId,
        id: &str,
    ) -> Result<Option<MediaItemSnapshot>, String> {
        let compact = id.replace('-', "");
        if compact == self.show.id.to_string().replace('-', "") {
            let mut series = self.episode_a.clone();
            series.is_series = true;
            return Ok(Some(series));
        }
        if compact == self.movie.media.id.to_string().replace('-', "") {
            return Ok(Some(self.movie.clone()));
        }
        Ok(None)
    }

    async fn resolve_cover_bytes(
        &self,
        _: Option<UserId>,
        _: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        Ok(None)
    }

    async fn resolve_poster_bytes(
        &self,
        _: Option<UserId>,
        _: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        Ok(None)
    }

    async fn resolve_stream_source(
        &self,
        _: UserId,
        _: &str,
    ) -> Result<Option<(PathBuf, bool)>, String> {
        Ok(None)
    }

    async fn set_user_marks(
        &self,
        _: UserId,
        _: &str,
        _: Option<bool>,
        _: Option<bool>,
    ) -> Result<bool, String> {
        Ok(true)
    }

    async fn update_progress(&self, _: UserId, _: &str, _: i64, _: bool) -> Result<(), String> {
        Ok(())
    }
}

#[tokio::test]
async fn legacy_folder_keeps_series_whose_representative_is_elsewhere() {
    let provider = SplitRootProvider::new();
    let folder = folder_id_for_root(std::path::Path::new("/media/root-b"));
    let page = legacy_select(
        &provider,
        user(),
        ServerItemQuery {
            parent_id: Some(folder),
            include_item_types: Some("Series,Episode".into()),
            recursive: true,
            limit: 20,
            ..Default::default()
        },
    )
    .await
    .expect("legacy select");
    let names: Vec<&str> = page.items.iter().map(|item| item.media.title.as_str()).collect();
    assert!(
        page.items.iter().any(|item| item.is_series && item.media.id == provider.show.id),
        "folder B 必须保留代表行在 root A 的剧：{names:?}"
    );
    assert!(
        page.items.iter().any(|item| {
            !item.is_series && item.row.path == "/media/root-b/Show/S01E02.mkv"
        }),
        "folder B 必须保留该剧在 B 的分集：{names:?}"
    );
    assert!(
        page.items.iter().all(|item| item.row.path.starts_with("/media/root-b")),
        "不得带回 root A 的代表行：{:?}",
        page.items.iter().map(|item| &item.row.path).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn legacy_next_up_returns_explicit_movie_and_expands_collapsed_media() {
    let provider = SplitRootProvider::new();
    let movie_id = provider.movie.media.id.to_string();
    let movie = legacy_next_up(&provider, user(), Some(&movie_id), 0, 20)
        .await
        .expect("explicit movie");
    assert_eq!(movie.total, 1, "显式请求的未看电影必须返回");
    assert_eq!(movie.items[0].media.id, provider.movie.media.id);

    let mixed = legacy_next_up(&provider, user(), None, 0, 20)
        .await
        .expect("mixed granularity");
    let titles: Vec<&str> = mixed.items.iter().map(|item| item.media.title.as_str()).collect();
    assert!(
        titles.contains(&"Split show"),
        "已有分集的剧必须留下：{titles:?}"
    );
    assert!(
        titles.contains(&"Collapsed other"),
        "缺分集的折叠剧必须各自展开，不能因为列表里已有别的分集就跳过：{titles:?}"
    );
}
