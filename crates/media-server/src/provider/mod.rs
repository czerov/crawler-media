use domain::{LedgerRow, Media, MediaKind, UserId};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub mod query;

pub use query::{ServerItemPage, ServerItemQuery};

#[derive(Clone, Debug)]
pub struct ServerLibrary {
    pub id: String,
    pub name: String,
    pub kind: MediaKind,
    pub exclude_from_home: bool,
    pub root_paths: Vec<PathBuf>,
    pub cover_path: Option<String>,
    pub cover_tag: String,
}

#[derive(Clone, Debug)]
pub struct ServerUser {
    pub id: UserId,
    pub login: String,
    pub is_admin: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackEvent {
    Playing,
    Progress,
    Stopped,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlaybackClientInfo {
    pub device_id: String,
    pub client: Option<String>,
    pub device_name: Option<String>,
    pub client_version: Option<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlaybackEventIdentity {
    pub play_session_id: Option<String>,
    pub event_sequence: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct MediaItemSnapshot {
    pub row: LedgerRow,
    pub media: Media,
    pub ticks: i64,
    pub play_count: i64,
    pub played: bool,
    pub unplayed_item_count: Option<usize>,
    pub parent_id: String,
    pub is_series: bool,
    pub chapters: Vec<library::ChapterMarker>,
    pub intro_start_ms: Option<i64>,
    pub intro_end_ms: Option<i64>,
    pub outro_start_ms: Option<i64>,
    pub outro_end_ms: Option<i64>,
    pub metadata: MediaItemMetadata,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct UserItemData {
    pub key: String,
    pub item_id: String,
    pub playback_position_ticks: i64,
    pub play_count: i64,
    pub played: bool,
    pub is_favorite: bool,
    pub unplayed_item_count: Option<usize>,
    pub played_percentage: Option<f64>,
}

impl UserItemData {
    pub fn json_value(&self) -> serde_json::Value {
        let mut value = serde_json::json!({
            "PlaybackPositionTicks": self.playback_position_ticks,
            "PlayCount": self.play_count,
            "IsFavorite": self.is_favorite,
            "Played": self.played,
            "Key": self.key,
            "ItemId": self.item_id,
        });
        if let Some(count) = self.unplayed_item_count {
            value["UnplayedItemCount"] = serde_json::json!(count);
        }
        if let Some(percentage) = self.played_percentage {
            value["PlayedPercentage"] = serde_json::json!(percentage);
        }
        value
    }
}

impl MediaItemSnapshot {
    pub fn user_item_data(&self) -> UserItemData {
        let item_id = if self.is_series {
            self.media.id.to_string().replace('-', "")
        } else {
            self.row.id.to_string().replace('-', "")
        };
        let playback_position_ticks = if self.is_series { 0 } else { self.ticks };
        let played_percentage = if let Some(unplayed) = self.unplayed_item_count {
            self.metadata
                .child_count
                .filter(|total| *total > 0)
                .map(|total| total.saturating_sub(unplayed) as f64 / total as f64 * 100.0)
        } else if playback_position_ticks > 0 {
            self.metadata
                .runtime_ticks
                .filter(|runtime| *runtime > 0)
                .map(|runtime| playback_position_ticks as f64 / runtime as f64 * 100.0)
        } else {
            None
        };
        UserItemData {
            key: item_id.clone(),
            item_id,
            playback_position_ticks,
            play_count: if self.is_series { 0 } else { self.play_count },
            played: self.played,
            is_favorite: self.metadata.is_favorite,
            unplayed_item_count: self.unplayed_item_count,
            played_percentage,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct MediaItemMetadata {
    pub overview: Option<String>,
    pub community_rating: Option<f64>,
    pub vote_count: Option<u64>,
    pub genres: Vec<String>,
    pub production_year: Option<i32>,
    pub original_title: Option<String>,
    pub premiere_date: Option<String>,
    pub date_created: Option<String>,
    pub end_date: Option<String>,
    pub official_rating: Option<String>,
    pub status: Option<String>,
    pub original_language: Option<String>,
    pub taglines: Vec<String>,
    pub studios: Vec<String>,
    pub production_locations: Vec<String>,
    pub number_of_episodes: Option<u32>,
    pub runtime_ticks: Option<i64>,
    pub provider_ids: BTreeMap<String, String>,
    pub episode_name: Option<String>,
    pub people: Vec<MediaPerson>,
    pub is_favorite: bool,
    pub resume_updated_at: i64,
    pub child_count: Option<usize>,
    pub season_count: Option<usize>,
    pub has_primary_image: bool,
    pub has_backdrop_image: bool,
    pub primary_image_tag: Option<String>,
    pub primary_image_url: Option<String>,
    pub tracks: library::Tracks,
}

#[derive(Clone, Debug, Default)]
pub struct MediaPerson {
    pub id: String,
    pub name: String,
    pub role: Option<String>,
    pub person_type: String,
    pub tmdb_id: Option<String>,
    pub primary_image_url: Option<String>,
}

/// 密码登录失败原因。`Busy` 由协议层映射 503，`Internal` 映射 500；
/// 内部故障绝不能退化成“密码错误”(401) 返回给客户端。
#[derive(Debug, thiserror::Error)]
pub enum AuthenticateError {
    #[error("authentication service busy")]
    Busy,
    #[error("{0}")]
    Internal(String),
}

/// 字幕交付失败原因。协议层据此映射 503/504/409/500；`Ok(None)` 表示没有
/// 可见的字幕轨或文件（映射 404）。
#[derive(Debug, thiserror::Error)]
pub enum SubtitleDeliveryError {
    #[error("subtitle delivery unsupported")]
    Unsupported,
    #[error("subtitle service busy")]
    Busy,
    #[error("subtitle operation timed out")]
    Timeout,
    #[error("Library no longer available")]
    Cancelled,
    #[error("subtitle delivery failed: {0}")]
    Failed(String),
}

#[async_trait::async_trait]
pub trait MediaServerProvider: Send + Sync + 'static {
    fn server_id(&self) -> String;

    async fn authenticate_password(
        &self,
        username: &str,
        password: &str,
    ) -> Result<Option<(String, ServerUser)>, AuthenticateError>;

    async fn user_id_by_token(&self, token: &str) -> Result<Option<UserId>, String>;

    /// 通过共享 single-flight 服务交付字幕轨。
    ///
    /// 默认不支持：实现方必须显式 override，**绝不默认回退到同步提取**
    /// （那会在 async worker 上阻塞）。`Ok(None)` 表示没有可见的字幕轨/文件。
    async fn deliver_subtitle(
        &self,
        _user_id: UserId,
        _item_id: &str,
        _index: u32,
        _wants_vtt: bool,
    ) -> Result<Option<library::SubtitlePayload>, SubtitleDeliveryError> {
        Err(SubtitleDeliveryError::Unsupported)
    }

    /// Persist credentials observed on authenticated protocol requests. Mock providers
    /// may omit persistence; production uses this binding for secure device revocation.
    async fn bind_playback_credential(
        &self,
        _user_id: UserId,
        _device_id: &str,
        _token: &str,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn get_user(&self, user_id: UserId) -> Result<Option<ServerUser>, String>;

    /// Lists libraries visible specifically to this user
    async fn list_visible_libraries(&self, user_id: UserId) -> Result<Vec<ServerLibrary>, String>;

    /// Lists media item snapshots visible to this user
    async fn list_visible_items(
        &self,
        user_id: UserId,
        parent_id: Option<&str>,
    ) -> Result<Vec<MediaItemSnapshot>, String>;
    /// Lists items visible to the user and owned by the requested library.
    /// Providers without library-aware filtering retain their existing listing behavior.
    async fn list_visible_items_in_library(
        &self,
        user_id: UserId,
        parent_id: Option<&str>,
        _library_id: &str,
    ) -> Result<Vec<MediaItemSnapshot>, String> {
        self.list_visible_items(user_id, parent_id).await
    }

    /// Typed 条目查询：筛选/排序/计数在**选择页之前**完整执行，返回的一页条目
    /// 必须已经带上该页所需的全部 metadata（tracks/chapters/NFO 细节）。
    ///
    /// 默认实现按旧方法等价过滤，**不承诺有界**（分集展开仍是逐 series 查询）；
    /// 生产 provider 必须覆盖它，用轻候选先选页、再只为该页构建重元数据。
    async fn query_visible_items(
        &self,
        user_id: UserId,
        query: ServerItemQuery,
    ) -> Result<ServerItemPage, String> {
        query::legacy_select(self, user_id, query).await
    }

    /// NextUp：一次候选读 + 按 Media 分组选第一条未看，再整体分页。
    ///
    /// 默认实现用一次 `list_visible_items` 做聚合；旧式（只返回 series 级快照）
    /// provider 会退回逐 series 查询，因此**不承诺有界**。生产 provider 覆盖它。
    async fn next_up_items(
        &self,
        user_id: UserId,
        series_id: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> Result<ServerItemPage, String> {
        query::legacy_next_up(self, user_id, series_id, offset, limit).await
    }

    async fn list_resumable_items(
        &self,
        user_id: UserId,
        limit: usize,
    ) -> Result<Vec<MediaItemSnapshot>, String> {
        let items = self.list_visible_items(user_id, None).await?;
        let mut resumable = Vec::new();
        for item in items {
            if item.ticks > 0 && !item.played {
                resumable.push(item);
                if resumable.len() >= limit {
                    break;
                }
            }
        }
        Ok(resumable)
    }
    async fn resolve_single_item(
        &self,
        user_id: UserId,
        id: &str,
    ) -> Result<Option<MediaItemSnapshot>, String>;

    async fn resolve_user_item_data(
        &self,
        user_id: UserId,
        id: &str,
    ) -> Result<Option<UserItemData>, String> {
        Ok(self
            .resolve_single_item(user_id, id)
            .await?
            .map(|snapshot| snapshot.user_item_data()))
    }

    async fn enrich_visible_item(&self, item: MediaItemSnapshot) -> MediaItemSnapshot {
        item
    }

    async fn resolve_cover_bytes(
        &self,
        user_id: Option<UserId>,
        id: &str,
    ) -> Result<Option<Vec<u8>>, String>;

    async fn resolve_poster_bytes(
        &self,
        user_id: Option<UserId>,
        id: &str,
    ) -> Result<Option<Vec<u8>>, String>;

    async fn resolve_primary_image_url(
        &self,
        _user_id: Option<UserId>,
        _id: &str,
    ) -> Result<Option<String>, String> {
        Ok(None)
    }

    async fn resolve_person_image_url(
        &self,
        _user_id: Option<UserId>,
        _name: &str,
    ) -> Result<Option<String>, String> {
        Ok(None)
    }

    async fn resolve_person(
        &self,
        user_id: UserId,
        query: &str,
    ) -> Result<Option<MediaPerson>, String> {
        let compact_query = query.replace('-', "");
        let items = self.list_visible_items(user_id, None).await?;
        Ok(items
            .iter()
            .flat_map(|item| &item.metadata.people)
            .find(|person| {
                person.name.eq_ignore_ascii_case(query)
                    || person
                        .id
                        .replace('-', "")
                        .eq_ignore_ascii_case(&compact_query)
                    || person
                        .tmdb_id
                        .as_deref()
                        .is_some_and(|id| id.eq_ignore_ascii_case(query))
            })
            .cloned())
    }

    async fn resolve_backdrop_bytes(
        &self,
        _user_id: Option<UserId>,
        _id: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        Ok(None)
    }

    async fn resolve_stream_source(
        &self,
        user_id: UserId,
        id: &str,
    ) -> Result<Option<(PathBuf, bool)>, String>;

    /// A05: 检查特定设备是否已被用户撤销
    async fn is_device_revoked(&self, _user_id: UserId, _device_id: &str) -> Result<bool, String> {
        Ok(false)
    }

    async fn set_user_marks(
        &self,
        user_id: UserId,
        id: &str,
        played: Option<bool>,
        favorite: Option<bool>,
    ) -> Result<bool, String>;

    async fn update_progress(
        &self,
        user_id: UserId,
        id: &str,
        position_ms: i64,
        paused: bool,
    ) -> Result<(), String>;
    async fn issue_play_session(
        &self,
        _user_id: UserId,
        _device_id: &str,
        _item_id: &str,
    ) -> Result<String, String> {
        Ok(uuid::Uuid::new_v4().to_string())
    }

    async fn report_playback_event(
        &self,
        user_id: UserId,
        id: &str,
        position_ms: i64,
        paused: bool,
        _client: PlaybackClientInfo,
        _event: PlaybackEvent,
        _identity: PlaybackEventIdentity,
    ) -> Result<(), String> {
        self.update_progress(user_id, id, position_ms, paused).await
    }
}
