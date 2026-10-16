//! Catalog（TMDB 缓存）元数据合并：单条目路径与条目选择共用一份载荷。
//!
//! 选择路径要先用合并后的 metadata 过滤（`PersonIds` 等），再只为选中的页重建
//! 重元数据，所以这里把「取载荷」与「应用载荷」拆开：同一份载荷可以在轻候选与
//! 页面重元数据上各应用一次，结果一致。

use super::*;

/// 一次 catalog 合并的载荷（幂等：对同一 metadata 再合并一次结果相同）。
pub(super) struct CatalogMerge {
    metadata: media::ItemMeta,
    episode: Option<media::EpisodeMeta>,
}

impl CatalogMerge {
    pub(super) fn apply(&self, metadata: media_server::provider::MediaItemMetadata, media: &domain::Media) -> media_server::provider::MediaItemMetadata {
        merge_catalog_metadata(metadata, media, &self.metadata, self.episode.as_ref())
    }
}

/// 锁外同步取载荷：catalog 缓存/网络都在这里，调用方不得持有 Store 锁。
pub(super) fn catalog_merge(state: &ApiState, snapshot: &MediaItemSnapshot) -> Option<CatalogMerge> {
    let tmdb_id = snapshot.media.tmdb_id.clone()?;
    let metadata = match crate::scrape_metadata::fetch_tmdb_metadata(
        state,
        snapshot.media.kind,
        &tmdb_id,
    ) {
        Ok(Some(metadata)) => metadata,
        Ok(None) => {
            tracing::debug!(%tmdb_id, "catalog returned no metadata for Jellyfin item");
            return None;
        }
        Err(error) => {
            tracing::warn!(%error, %tmdb_id, "failed to load catalog metadata for Jellyfin item");
            return None;
        }
    };
    let episode = (!snapshot.is_series && snapshot.media.kind == MediaKind::Tv)
        .then(|| snapshot.row.season.zip(snapshot.row.episode))
        .flatten()
        .and_then(|(season, episode)| catalog_episode(state, &tmdb_id, season, episode));
    Some(CatalogMerge { metadata, episode })
}

impl ApiServerProvider {
    pub(super) async fn enrich_single_item(
        &self,
        snapshot: MediaItemSnapshot,
    ) -> MediaItemSnapshot {
        let state = self.state.clone();
        let fallback = snapshot.clone();
        let task = tokio::task::spawn_blocking(move || {
            let mut snapshot = snapshot;
            enrich_snapshot_blocking(&state, &mut snapshot);
            snapshot
        })
        .await;
        match task {
            Ok(snapshot) => snapshot,
            Err(error) => {
                tracing::warn!(%error, "Jellyfin catalog metadata task failed");
                fallback
            }
        }
    }
}

/// 锁外：一次 probe 入队 + 一次 catalog 合并（单条目路径与选择路径共用）。
pub(super) fn enrich_snapshot_blocking(state: &ApiState, snapshot: &mut MediaItemSnapshot) {
    if !snapshot.is_series {
        crate::http::library::ensure_probe_enqueued(state, &snapshot.row);
    }
    let Some(merge) = catalog_merge(state, snapshot) else {
        return;
    };
    let metadata = std::mem::take(&mut snapshot.metadata);
    snapshot.metadata = merge.apply(metadata, &snapshot.media);
}

fn catalog_episode(
    state: &ApiState,
    tmdb_lookup_id: &str,
    season: u32,
    episode: u32,
) -> Option<media::EpisodeMeta> {
    let language = crate::scrape_metadata::preferred_language(state);
    let translated = match state
        .catalog
        .season_details_lang(tmdb_lookup_id, season, &language)
    {
        Ok(episodes) if !episodes.is_empty() => episodes,
        Ok(_) => Vec::new(),
        Err(error) => {
            tracing::warn!(%error, tmdb_id = %tmdb_lookup_id, season, "failed to load translated episode metadata");
            Vec::new()
        }
    };
    let episodes = if translated.is_empty() {
        match state.catalog.season_details(tmdb_lookup_id, season) {
            Ok(episodes) => episodes,
            Err(error) => {
                tracing::warn!(%error, tmdb_id = %tmdb_lookup_id, season, "failed to load episode metadata");
                Vec::new()
            }
        }
    } else {
        translated
    };
    episodes
        .into_iter()
        .find(|metadata| metadata.episode_number == episode)
}
