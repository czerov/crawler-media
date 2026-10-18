use serde::Deserialize;

#[derive(Deserialize)]
pub(crate) struct CreateSubscriptionInput {
    /// 稳定引用（`tmdb:movie:603` / `tmdb:603` / 裸标题）。提供时后端解析出
    /// Media 身份，`media` 字段可省略。
    #[serde(default)]
    pub(super) title_ref: Option<String>,
    #[serde(default)]
    pub(super) media: Option<MediaInput>,
    #[serde(default)]
    pub(super) coverage: Option<CoverageInput>,
    #[serde(default = "default_fetch")]
    pub(super) fetch_mode: String,
    #[serde(default)]
    pub(super) filter_id: Option<String>,
    #[serde(default)]
    pub(super) wash_cut: bool,
    #[serde(default)]
    pub(super) keep_old_versions: Option<bool>,
    #[serde(default)]
    pub(super) wash_cut_filter_id: Option<String>,
    #[serde(default)]
    pub(super) full_season_pack: bool,
    #[serde(default)]
    pub(super) downloader_id: Option<String>,
    #[serde(default)]
    pub(super) library_id: Option<String>,
    #[serde(default = "default_active")]
    pub(super) tracking_state: String,
    #[serde(default)]
    pub(super) follow_future: bool,
    /// G-M3: create the next season's subscription once this one completes.
    #[serde(default)]
    pub(super) auto_next_season: bool,
    /// G-M4: full-season-pack only; fall back to single episodes after N days.
    #[serde(default)]
    pub(super) pack_fallback_after_days: Option<u32>,
    #[serde(default)]
    pub(super) search_interval_secs: Option<u32>,
}

fn default_active() -> String {
    "active".into()
}

fn default_fetch() -> String {
    "search".into()
}

#[derive(Clone, Deserialize)]
pub(crate) struct MediaInput {
    pub(super) kind: String,
    pub(super) title: String,
    #[serde(default)]
    pub(super) year: Option<u16>,
    #[serde(default)]
    pub(super) original_title: Option<String>,
    #[serde(default)]
    pub(super) tmdb_id: Option<String>,
    #[serde(default)]
    pub(super) douban_id: Option<String>,
    #[serde(default)]
    pub(super) tvdb_id: Option<String>,
    #[serde(default)]
    pub(super) bangumi_id: Option<String>,
    #[serde(default)]
    pub(super) anilist_id: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub(crate) enum CoverageInput {
    Movie,
    Tv {
        season: u32,
        episode_from: u32,
        episode_to: Option<u32>,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PatchSubscriptionInput {
    #[serde(default)]
    pub(super) fetch_mode: Option<String>,
    /// 规则组绑定：None 表示不修改；Some(None) 或空值表示显式传 null，需拒绝；Some(Some(id)) 表示更新
    #[serde(default, deserialize_with = "double_option")]
    pub(super) filter_id: Option<Option<String>>,
    #[serde(default)]
    pub(super) wash_cut: Option<bool>,
    #[serde(default)]
    pub(super) full_season_pack: Option<bool>,
    #[serde(default)]
    pub(super) downloader_id: Option<String>,
    #[serde(default)]
    pub(super) tracking_state: Option<String>,
    #[serde(default)]
    pub(super) follow_future: Option<bool>,
    /// G-M3: see `CreateSubscriptionInput::auto_next_season`.
    #[serde(default)]
    pub(super) auto_next_season: Option<bool>,
    /// G-M4: days before falling back to single episodes; 0 clears it.
    #[serde(default)]
    pub(super) pack_fallback_after_days: Option<u32>,
    #[serde(default)]
    pub(super) search_interval_secs: Option<u32>,
    /// 调整订阅的季范围（前端 selected_seasons）：取最小季为 coverage 季，
    /// 集窗口保持开放（episode_to=null）。空数组 = 不变。
    #[serde(default)]
    pub(super) selected_seasons: Option<Vec<u32>>,
    /// 洗版时保留旧版本（新旧并存）。
    #[serde(default)]
    pub(super) keep_old_versions: Option<bool>,
    /// 目标媒体库绑定：None 表示不修改；Some(None) 表示清空重置为默认库；Some(Some(id)) 表示绑定指定库
    #[serde(default, deserialize_with = "double_option")]
    pub(super) library_id: Option<Option<String>>,
}

fn double_option<'de, D>(deserializer: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    serde::Deserialize::deserialize(deserializer).map(Some)
}
