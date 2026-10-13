//! Off-lock browse projection builder.

use std::collections::HashMap;
use std::path::Path;

use domain::{Media, MediaId, MediaKind};
use store::{
    BrowseFileFacts, BrowseMediaFacts, LibraryPathScope, ProjectionBuild, StrictOwner,
};

use crate::management::ApiState;
use super::ProjectionError;

pub fn build(state: &ApiState, library_id: &str) -> Result<ProjectionBuild, ProjectionError> {
    // 1. In Store lock, take owned facts
    let (library, snapshot, candidate_rows, revisions) = {
        let store = state.store.lock();
        let snapshot = store.library_ownership().map_err(|e| ProjectionError::Failed(e.to_string()))?;
        let lib = snapshot
            .library(library_id)
            .cloned()
            .ok_or_else(|| ProjectionError::Failed("Library not found".into()))?;
        if !snapshot.accepts_work(library_id) {
            return Err(ProjectionError::Cancelled);
        }
        let rows = store.list_ledger().map_err(|e| ProjectionError::Failed(e.to_string()))?;
        let revs = store.read_revisions().map_err(|e| ProjectionError::Failed(e.to_string()))?;
        (lib, snapshot, rows, revs)
    };

    // 2. Off-lock scope resolution
    let scope = LibraryPathScope::resolve(snapshot).map_err(|e| ProjectionError::Failed(e.to_string()))?;

    // Filter rows strictly owned by this library
    let mut owned_rows = Vec::new();
    let mut media_ids = Vec::new();
    for row in candidate_rows {
        let Ok(resolved) = LibraryPathScope::resolve_path(Path::new(&row.path)) else {
            continue;
        };
        match scope.owner(&resolved, library.kind) {
            StrictOwner::Owned(lib) if lib.id == library_id => {
                if !media_ids.contains(&row.media_id) {
                    media_ids.push(row.media_id);
                }
                owned_rows.push(row);
            }
            _ => {}
        }
    }

    // 3. Batch load media for owned rows
    let media_map: HashMap<MediaId, Media> = {
        let store = state.store.lock();
        let mut map = HashMap::new();
        for chunk in media_ids.chunks(500) {
            if let Ok(batch) = store.media_batch(chunk) {
                for (mid, m) in batch {
                    map.insert(mid, m);
                }
            }
        }
        map
    };

    // 4. Build file facts and media facts
    let mut file_facts = Vec::with_capacity(owned_rows.len());
    for row in &owned_rows {
        let file_path = Path::new(&row.path);
        let exists = state.library_files.presence(file_path).unwrap_or(false);
        let size = if exists {
            std::fs::metadata(file_path).map(|m| m.len()).unwrap_or(0)
        } else {
            0
        };

        let facts_json = serde_json::json!({
            "version": 1,
            "path": row.path,
            "size": size,
            "exists": exists,
            "season": row.season,
            "episode": row.episode,
            "resolution": row.resolution,
            "codec": row.codec,
            "quality_source": row.quality_source.as_str(),
            "confidence": row.confidence.as_str(),
            "filter_score": row.filter_score,
        });

        file_facts.push(BrowseFileFacts {
            ledger_id: row.id,
            media_id: row.media_id,
            facts: facts_json,
        });
    }

    let mut media_facts = Vec::with_capacity(media_ids.len());
    for mid in media_ids {
        if let Some(media) = media_map.get(&mid) {
            let facts_json = serde_json::json!({
                "version": 1,
                "title": media.title,
                "year": media.year,
                "original_title": media.original_title,
                "tmdb_id": media.tmdb_id,
                "douban_id": media.douban_id,
                "tvdb_id": media.tvdb_id,
                "bangumi_id": media.bangumi_id,
                "anilist_id": media.anilist_id,
            });

            media_facts.push(BrowseMediaFacts {
                media_id: mid,
                title: media.title.clone(),
                year: media.year,
                facts: facts_json,
            });
        }
    }

    let scope_signature = format!("{}:{}", library.kind.as_str(), library.root_paths.len());

    Ok(ProjectionBuild {
        library_id: library_id.to_string(),
        generation: 1,
        scope_signature,
        revisions,
        files: file_facts,
        media: media_facts,
    })
}
