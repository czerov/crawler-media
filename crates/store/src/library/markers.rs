mod source_guard;

use rusqlite::{OptionalExtension, params, params_from_iter};
use std::collections::BTreeSet;

use super::{Store, StoreError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredMediaMarker {
    pub media_id: domain::MediaId,
    pub season: u32,
    pub episode: u32,
    pub intro_start_ms: Option<i64>,
    pub intro_end_ms: Option<i64>,
    pub outro_start_ms: Option<i64>,
    pub outro_end_ms: Option<i64>,
    pub source: String,
    pub locked: bool,
    pub updated_at: i64,
}

#[derive(Clone, Debug)]
pub struct MarkerResultReplacement {
    pub media_id: domain::MediaId,
    pub season: u32,
    pub markers: Vec<StoredMediaMarker>,
    pub chapter_updates: Vec<(String, Vec<library::ChapterMarker>)>,
}

impl Store {
    pub fn delete_media_marker(
        &self,
        media_id: domain::MediaId,
        season: impl Into<Option<u32>>,
        episode: impl Into<Option<u32>>,
    ) -> Result<(), StoreError> {
        let season = season.into().unwrap_or(1);
        let episode = episode.into().unwrap_or(1);
        self.library.execute(
            "DELETE FROM media_markers WHERE media_id = ?1 AND season = ?2 AND episode = ?3",
            params![media_id.to_string(), season, episode],
        )?;
        Ok(())
    }

    pub fn delete_media_markers_for_media(
        &self,
        media_id: domain::MediaId,
    ) -> Result<(), StoreError> {
        self.library.execute(
            "DELETE FROM media_markers WHERE media_id = ?1",
            params![media_id.to_string()],
        )?;
        Ok(())
    }

    /// Replace a season's marker rows and chapter caches in one transaction.
    /// Existing results remain readable until this transaction commits.
    pub fn replace_marker_results(
        &self,
        media_id: domain::MediaId,
        season: u32,
        markers: &[StoredMediaMarker],
        chapter_updates: &[(String, Vec<library::ChapterMarker>)],
    ) -> Result<(), StoreError> {
        self.replace_marker_results_batch(&[MarkerResultReplacement {
            media_id,
            season,
            markers: markers.to_vec(),
            chapter_updates: chapter_updates.to_vec(),
        }])
    }

    /// Atomically replace marker rows and chapter caches across one or more seasons.
    pub fn replace_marker_results_batch(
        &self,
        replacements: &[MarkerResultReplacement],
    ) -> Result<(), StoreError> {
        self.replace_marker_results_batch_inner(replacements, None, false)
    }

    /// Commit all season results and the marker-refresh terminal state together.
    pub fn complete_marker_refresh(
        &self,
        job_id: &str,
        replacements: &[MarkerResultReplacement],
    ) -> Result<(), StoreError> {
        self.replace_marker_results_batch_inner(replacements, Some(job_id), false)
    }

    fn replace_marker_results_batch_inner(
        &self,
        replacements: &[MarkerResultReplacement],
        completed_job_id: Option<&str>,
        check_sources: bool,
    ) -> Result<(), StoreError> {
        let serialized = serialize_chapter_updates(replacements)?;
        let tx = self.library.unchecked_transaction()?;
        if check_sources {
            match completed_job_id {
                Some(job_id) => {
                    // A job-scoped refresh publishes exactly the members the job covered, so the
                    // season-wide membership equality does not apply. Each published row must still
                    // exist in the ledger and on disk, and the published set must equal the job's members.
                    source_guard::validate_published_rows(&tx, replacements)?;
                    source_guard::validate_job_members(&tx, job_id, replacements)?;
                }
                None => source_guard::validate_members(&tx, replacements)?,
            }
        }

        // A job-scoped refresh must not mutate marker facts for episodes outside the rows it leased and published.
        // Only the source-checked job path is job-scoped: the unchecked legacy entry point keeps its season-wide
        // replacement semantics and may be driven with ledger ids that are not persisted rows.
        let covered_episodes = match completed_job_id {
            Some(_) if check_sources => {
                let mut episodes = BTreeSet::new();
                let mut stmt = tx.prepare("SELECT COALESCE(episode, 1) FROM ledger WHERE id = ?1")?;
                for replacement in replacements {
                    for (id, _) in &replacement.chapter_updates {
                        let episode: i64 = stmt.query_row(params![id], |row| row.get(0))?;
                        episodes.insert(episode);
                    }
                }
                Some(episodes)
            }
            _ => None,
        };

        for replacement in replacements {
            write_replacement_markers(&tx, replacement, covered_episodes.as_ref())?;
        }
        for (ledger_id, chapters_json) in serialized {
            upsert_chapter_cache(&tx, &ledger_id, chapters_json)?;
        }
        if let Some(job_id) = completed_job_id {
            complete_refresh_job(&tx, job_id)?;
        }

        tx.commit()?;
        Ok(())
    }

    pub fn get_media_marker(
        &self,
        media_id: domain::MediaId,
        season: Option<u32>,
        episode: Option<u32>,
    ) -> Result<Option<StoredMediaMarker>, StoreError> {
        let season = season.unwrap_or(1);
        let episode = episode.unwrap_or(1);
        self.library
            .query_row(
                "SELECT intro_start_ms, intro_end_ms, outro_start_ms, outro_end_ms, source, locked, updated_at
                 FROM media_markers
                 WHERE media_id = ?1 AND season = ?2 AND episode = ?3",
                params![media_id.to_string(), season, episode],
                |row| {
                    Ok(StoredMediaMarker {
                        media_id,
                        season,
                        episode,
                        intro_start_ms: row.get(0)?,
                        intro_end_ms: row.get(1)?,
                        outro_start_ms: row.get(2)?,
                        outro_end_ms: row.get(3)?,
                        source: row.get(4)?,
                        locked: row.get::<_, i64>(5)? != 0,
                        updated_at: row.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn put_media_marker(&self, marker: &StoredMediaMarker) -> Result<(), StoreError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        self.library.execute(
            "INSERT INTO media_markers (
                media_id, season, episode, intro_start_ms, intro_end_ms, outro_start_ms, outro_end_ms, source, locked, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(media_id, season, episode) DO UPDATE SET
                intro_start_ms = excluded.intro_start_ms,
                intro_end_ms = excluded.intro_end_ms,
                outro_start_ms = excluded.outro_start_ms,
                outro_end_ms = excluded.outro_end_ms,
                source = excluded.source,
                locked = excluded.locked,
                updated_at = excluded.updated_at
             WHERE media_markers.locked = 0 OR excluded.locked = 1",
            params![
                marker.media_id.to_string(),
                marker.season,
                marker.episode,
                marker.intro_start_ms,
                marker.intro_end_ms,
                marker.outro_start_ms,
                marker.outro_end_ms,
                marker.source,
                if marker.locked { 1 } else { 0 },
                now,
            ],
        )?;
        Ok(())
    }
}

/// `(ledger_id, chapters_json)` pairs detected by a marker refresh, ready to be published.
fn serialize_chapter_updates(
    replacements: &[MarkerResultReplacement],
) -> Result<Vec<(String, String)>, StoreError> {
    replacements
        .iter()
        .flat_map(|replacement| {
            replacement
                .chapter_updates
                .iter()
                .map(|(ledger_id, chapters)| {
                    Ok((ledger_id.clone(), serde_json::to_string(chapters)?))
                })
        })
        .collect()
}

/// Replace unlocked marker rows for one season; locked rows are preserved as-is.
/// When `covered_episodes` is `Some`, only episodes in that set are deleted and re-inserted;
/// other episodes keep their marker rows untouched. When `None`, the whole season is replaced.
fn write_replacement_markers(
    tx: &rusqlite::Transaction<'_>,
    replacement: &MarkerResultReplacement,
    covered_episodes: Option<&BTreeSet<i64>>,
) -> Result<(), StoreError> {
    delete_replacement_markers(tx, replacement, covered_episodes)?;
    for marker in &replacement.markers {
        if let Some(episodes) = covered_episodes {
            if !episodes.contains(&(marker.episode as i64)) {
                continue;
            }
        }
        let is_locked: bool = tx
            .query_row(
                "SELECT locked FROM media_markers WHERE media_id = ?1 AND season = ?2 AND episode = ?3",
                params![marker.media_id.to_string(), marker.season, marker.episode],
                |row| row.get(0),
            )
            .unwrap_or(false);
        if is_locked {
            continue;
        }
        insert_marker_row(tx, marker)?;
    }
    Ok(())
}

fn delete_replacement_markers(
    tx: &rusqlite::Transaction<'_>,
    replacement: &MarkerResultReplacement,
    covered_episodes: Option<&BTreeSet<i64>>,
) -> Result<(), StoreError> {
    match covered_episodes {
        None => {
            tx.execute(
                "DELETE FROM media_markers WHERE media_id = ?1 AND season = ?2 AND locked = 0",
                params![replacement.media_id.to_string(), replacement.season],
            )?;
        }
        Some(episodes) => {
            if episodes.is_empty() {
                return Ok(());
            }
            let placeholders = vec!["?"; episodes.len()].join(", ");
            let sql = format!(
                "DELETE FROM media_markers WHERE media_id = ?1 AND season = ?2 AND locked = 0 AND episode IN ({placeholders})"
            );
            let mut params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(2 + episodes.len());
            let media_id_str = replacement.media_id.to_string();
            params.push(&media_id_str);
            params.push(&replacement.season);
            for ep in episodes {
                params.push(ep);
            }
            let mut stmt = tx.prepare(&sql)?;
            stmt.execute(params_from_iter(params))?;
        }
    }
    Ok(())
}

fn insert_marker_row(
    tx: &rusqlite::Transaction<'_>,
    marker: &StoredMediaMarker,
) -> Result<(), StoreError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    tx.execute(
        "INSERT INTO media_markers (
             media_id, season, episode, intro_start_ms, intro_end_ms,
             outro_start_ms, outro_end_ms, source, locked, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            marker.media_id.to_string(),
            marker.season,
            marker.episode,
            marker.intro_start_ms,
            marker.intro_end_ms,
            marker.outro_start_ms,
            marker.outro_end_ms,
            marker.source,
            marker.locked,
            now,
        ],
    )?;
    Ok(())
}

/// Publish the chapter cache for one ledger.
///
/// When a locked marker exists the timeline is rebuilt from the locked ranges so that
/// detection results cannot drift it; the media duration already known for that ledger is
/// passed through so the feature chapter never runs past the end of the media.
fn upsert_chapter_cache(
    tx: &rusqlite::Transaction<'_>,
    ledger_id: &str,
    detected_chapters_json: String,
) -> Result<(), StoreError> {
    let chapters_json = match locked_marker_ranges(tx, ledger_id)? {
        Some((intro, outro)) => {
            let (existing_chapters, duration_ms) = cached_chapters_and_duration(tx, ledger_id)?;
            let timeline = library::build_complete_timeline_chapters(
                &existing_chapters,
                intro,
                outro,
                duration_ms,
            );
            serde_json::to_string(&timeline)?
        }
        None => detected_chapters_json,
    };

    tx.execute(
        "INSERT INTO file_meta (ledger_id, audio_json, subtitle_json, chapters_json)
         VALUES (?1, '[]', '[]', ?2)
         ON CONFLICT(ledger_id) DO UPDATE SET chapters_json = excluded.chapters_json",
        params![ledger_id, chapters_json],
    )?;
    Ok(())
}

/// Valid intro/outro ranges of the locked marker attached to `ledger_id`, if any.
fn locked_marker_ranges(
    tx: &rusqlite::Transaction<'_>,
    ledger_id: &str,
) -> Result<Option<(Option<(i64, i64)>, Option<(i64, i64)>)>, StoreError> {
    let locked: Option<(Option<i64>, Option<i64>, Option<i64>, Option<i64>)> = tx
        .query_row(
            "SELECT m.intro_start_ms, m.intro_end_ms, m.outro_start_ms, m.outro_end_ms
             FROM ledger l
             JOIN media_markers m ON m.media_id = l.media_id
                AND m.season = COALESCE(l.season, 1)
                AND m.episode = COALESCE(l.episode, 1)
             WHERE l.id = ?1 AND m.locked = 1",
            params![ledger_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;

    Ok(locked.map(|(intro_s, intro_e, outro_s, outro_e)| {
        (valid_range(intro_s, intro_e), valid_range(outro_s, outro_e))
    }))
}

fn valid_range(start_ms: Option<i64>, end_ms: Option<i64>) -> Option<(i64, i64)> {
    match (start_ms, end_ms) {
        (Some(start), Some(end)) if start < end => Some((start, end)),
        _ => None,
    }
}

/// Cached chapters plus the format duration already stored for one ledger.
fn cached_chapters_and_duration(
    tx: &rusqlite::Transaction<'_>,
    ledger_id: &str,
) -> Result<(Vec<library::ChapterMarker>, Option<i64>), StoreError> {
    let cached: Option<(Option<String>, Option<i64>)> = tx
        .query_row(
            "SELECT chapters_json, format_duration_ms FROM file_meta WHERE ledger_id = ?1",
            params![ledger_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;

    Ok(match cached {
        Some((json, duration_ms)) => (
            json.and_then(|json| serde_json::from_str(&json).ok())
                .unwrap_or_default(),
            duration_ms,
        ),
        None => (Vec::new(), None),
    })
}

/// Terminal transition for a marker refresh job; fails when the job is no longer active.
fn complete_refresh_job(tx: &rusqlite::Transaction<'_>, job_id: &str) -> Result<(), StoreError> {
    let finished_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default();
    let changed = tx.execute(
        "UPDATE probe_jobs SET status = 'succeeded', finished_at_ms = ?2
         WHERE id = ?1 AND status IN ('queued', 'running')
           AND completed = total AND succeeded = total AND failed = 0",
        params![job_id, finished_at_ms],
    )?;
    if changed == 0 {
        return Err(StoreError::Missing(format!(
            "active completed marker refresh job {job_id}"
        )));
    }
    Ok(())
}

#[cfg(test)]
#[path = "markers/tests.rs"]
mod marker_replacement_tests;
