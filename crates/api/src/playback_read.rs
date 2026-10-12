//! 播放读路径的请求级物化快照。
//!
//! 一次批量取 ownership / media / ledger rows / 成员名，之后所有渲染都是纯函数，
//! 不再在每条记录、每次比较里查库。

use std::collections::{HashMap, HashSet};
use std::path::Path;

use domain::{LedgerRow, Media, MediaId, UserId};
use serde_json::{Value, json};

use crate::store::{LibraryOwnership, Store, StoreError};

pub(crate) struct PlaybackRead {
    pub ownership: LibraryOwnership,
    pub media: HashMap<MediaId, Media>,
    pub rows: HashMap<MediaId, Vec<LedgerRow>>,
    pub names: HashMap<UserId, String>,
}

impl PlaybackRead {
    /// DB 失败时的保守空快照：所有 media 视为不可见、target 走 fallback、名字用占位。
    pub(crate) fn empty() -> Self {
        Self {
            ownership: LibraryOwnership::from_parts(Vec::new(), HashSet::new()),
            media: HashMap::new(),
            rows: HashMap::new(),
            names: HashMap::new(),
        }
    }

    pub(crate) fn load(
        store: &Store,
        media_ids: &[MediaId],
        user_ids: &[UserId],
    ) -> Result<Self, StoreError> {
        let ownership = store.library_ownership()?;
        let unique_media: Vec<MediaId> = dedup(media_ids);
        let media = store.media_batch(&unique_media)?;
        let mut rows: HashMap<MediaId, Vec<LedgerRow>> = HashMap::new();
        for row in store.ledger_for_media_batch(&unique_media)? {
            rows.entry(row.media_id).or_default().push(row);
        }
        let mut names = HashMap::new();
        let wanted: HashSet<UserId> = user_ids.iter().copied().collect();
        if !wanted.is_empty() {
            for user in store.list_users()? {
                if wanted.contains(&user.id) {
                    names.insert(user.id, user.login);
                }
            }
        }
        Ok(Self {
            ownership,
            media,
            rows,
            names,
        })
    }

    pub(crate) fn media(&self, id: MediaId) -> Option<&Media> {
        self.media.get(&id)
    }

    pub(crate) fn rows_for(&self, id: MediaId) -> &[LedgerRow] {
        self.rows.get(&id).map(Vec::as_slice).unwrap_or(&[])
    }

    /// 与既有 `member_name` 一致：未知用户显示占位名。
    pub(crate) fn member_name(&self, user_id: UserId) -> String {
        self.names
            .get(&user_id)
            .cloned()
            .unwrap_or_else(|| "成员".into())
    }
}

fn dedup(ids: &[MediaId]) -> Vec<MediaId> {
    let mut seen = HashSet::with_capacity(ids.len());
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        if seen.insert(*id) {
            out.push(*id);
        }
    }
    out
}

/// 纯函数：与旧 `media_target_json` 输出逐字一致，不再查库。
pub(crate) fn media_target(
    read: &PlaybackRead,
    id: MediaId,
    season: Option<i32>,
    episode: Option<i32>,
) -> Value {
    let Some(media) = read.media(id) else {
        return json!({ "media_item_id": id.to_string(), "title": "", "kind": "movie" });
    };
    let rows = read.rows_for(id);
    let library_id = library_id_for(read, media, rows);
    let poster_url = crate::http::library::preferred_row(rows)
        .and_then(crate::http::library::poster_path)
        .map(|_| {
            let row = crate::http::library::preferred_row(rows).expect("just checked");
            crate::http::library::artwork_url("posters", row.id)
        });
    json!({
        "media_item_id": media.id.to_string(),
        "library_id": library_id,
        "browsable": true,
        "kind": media.kind.as_str(),
        "title": media.title,
        "year": media.year,
        "poster_url": poster_url,
        "season_number": season.unwrap_or(0),
        "episode_number": episode.unwrap_or(0),
        "episode_title": null,
    })
}

/// 与 `library_id_for` 同义：所有已归属行指向同一 Library 才给出 id，否则 `None`。
fn library_id_for(read: &PlaybackRead, media: &Media, rows: &[LedgerRow]) -> Option<String> {
    let mut ids = rows
        .iter()
        .filter_map(|row| read.ownership.owner(Path::new(&row.path), media.kind))
        .map(|library| library.id.clone());
    let first = ids.next()?;
    ids.all(|id| id == first).then_some(first)
}
