use domain::{MediaId, UserId};
use rusqlite::{OptionalExtension, params};
use std::str::FromStr;

use super::{Store, StoreError};
use super::{UNIT_WHOLE, UnitRow, UnitState};

impl Store {
    pub fn unit_state(
        &self,
        user_id: UserId,
        media_id: MediaId,
        season: i32,
        episode: i32,
    ) -> Result<Option<UnitState>, StoreError> {
        self.subscribe
            .query_row(
                "SELECT position_ms, played, favorite, duration_ms, audio_track,
                        subtitle_track, play_count, updated_at, favorite_at
                 FROM playback_units
                 WHERE user_id = ?1 AND media_id = ?2 AND season = ?3 AND episode = ?4",
                params![user_id.to_string(), media_id.to_string(), season, episode],
                |row| {
                    Ok(UnitState {
                        position_ms: row.get(0)?,
                        played: row.get::<_, i64>(1)? != 0,
                        favorite: row.get::<_, i64>(2)? != 0,
                        duration_ms: row.get(3)?,
                        audio_track: row.get(4)?,
                        subtitle_track: row.get(5)?,
                        play_count: row.get(6)?,
                        updated_at: row.get(7)?,
                        favorite_at: row.get(8)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn set_unit_favorite(
        &self,
        user_id: UserId,
        media_id: MediaId,
        season: i32,
        episode: i32,
        favorite: bool,
        now: i64,
    ) -> Result<(), StoreError> {
        let favorite_at = if favorite { now } else { 0 };
        self.subscribe.execute(
            "INSERT INTO playback_units
                (user_id, media_id, season, episode, position_ms, played, favorite,
                 duration_ms, audio_track, subtitle_track, play_count, updated_at, favorite_at)
             VALUES (?1, ?2, ?3, ?4, 0, 0, ?5, NULL, NULL, NULL, 0, 0, ?6)
             ON CONFLICT(user_id, media_id, season, episode) DO UPDATE SET
                favorite = excluded.favorite,
                favorite_at = excluded.favorite_at",
            params![
                user_id.to_string(),
                media_id.to_string(),
                season,
                episode,
                favorite as i64,
                favorite_at,
            ],
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn upsert_unit(
        &self,
        user_id: UserId,
        media_id: MediaId,
        season: i32,
        episode: i32,
        position_ms: i64,
        played: Option<bool>,
        favorite: Option<bool>,
        duration_ms: Option<i64>,
        audio_track: Option<&str>,
        subtitle_track: Option<&str>,
        increment_play_count: bool,
        now: i64,
    ) -> Result<UnitState, StoreError> {
        let existing = self.unit_state(user_id, media_id, season, episode)?;
        let played = played.unwrap_or(existing.as_ref().map(|u| u.played).unwrap_or(false));
        let favorite_at = match favorite {
            Some(true) => now,
            Some(false) => 0,
            None => existing.as_ref().map(|unit| unit.favorite_at).unwrap_or(0),
        };
        let favorite = favorite.unwrap_or(existing.as_ref().map(|u| u.favorite).unwrap_or(false));
        let duration_ms = duration_ms.or(existing.as_ref().and_then(|u| u.duration_ms));
        let play_count = existing.as_ref().map(|u| u.play_count).unwrap_or(0)
            + if increment_play_count { 1 } else { 0 };
        let audio_track = audio_track
            .map(str::to_string)
            .or_else(|| existing.as_ref().and_then(|u| u.audio_track.clone()));
        let subtitle_track = subtitle_track
            .map(str::to_string)
            .or_else(|| existing.as_ref().and_then(|u| u.subtitle_track.clone()));
        self.subscribe.execute(
            "INSERT INTO playback_units
                (user_id, media_id, season, episode, position_ms, played, favorite,
                 duration_ms, audio_track, subtitle_track, play_count, updated_at, favorite_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(user_id, media_id, season, episode) DO UPDATE SET
                position_ms = excluded.position_ms,
                played = excluded.played,
                favorite = excluded.favorite,
                duration_ms = excluded.duration_ms,
                audio_track = excluded.audio_track,
                subtitle_track = excluded.subtitle_track,
                play_count = excluded.play_count,
                updated_at = excluded.updated_at,
                favorite_at = excluded.favorite_at",
            params![
                user_id.to_string(),
                media_id.to_string(),
                season,
                episode,
                position_ms,
                played as i64,
                favorite as i64,
                duration_ms,
                audio_track,
                subtitle_track,
                play_count,
                now,
                favorite_at,
            ],
        )?;
        self.unit_state(user_id, media_id, season, episode)?
            .ok_or_else(|| StoreError::Missing("unit just written".into()))
    }

    /// Copy a legacy `(0, 0)` watch unit onto the canonical `(-1, -1)` unit for
    /// whole-file media; TV keeps `(0, 0)` as a real season/episode pair.
    pub fn copy_legacy_movie_unit_if_missing(
        &self,
        user_id: UserId,
        media_id: MediaId,
    ) -> Result<(), StoreError> {
        match self.get_media(media_id)? {
            Some(media) if media.kind != domain::MediaKind::Tv => {}
            _ => return Ok(()),
        }
        self.subscribe.execute(
            "INSERT OR IGNORE INTO playback_units
                (user_id, media_id, season, episode, position_ms, played, favorite,
                 duration_ms, audio_track, subtitle_track, play_count, updated_at, favorite_at)
             SELECT user_id, media_id, -1, -1, position_ms, played, favorite,
                    duration_ms, audio_track, subtitle_track, play_count, updated_at, favorite_at
             FROM playback_units
             WHERE user_id = ?1 AND media_id = ?2 AND season = 0 AND episode = 0",
            params![user_id.to_string(), media_id.to_string()],
        )?;
        Ok(())
    }

    /// All distinct media ids and their most recent activity timestamp for a user.
    pub fn user_active_media_ids(
        &self,
        user_id: UserId,
    ) -> Result<Vec<(MediaId, i64)>, StoreError> {
        let mut stmt = self.subscribe.prepare(
            "SELECT media_id, MAX(updated_at) AS last_act
             FROM playback_units
             WHERE user_id = ?1 AND (position_ms > 0 OR played != 0 OR play_count > 0)
             GROUP BY media_id
             ORDER BY last_act DESC",
        )?;
        let rows = stmt.query_map(params![user_id.to_string()], |row| {
            let id_str: String = row.get(0)?;
            let updated: i64 = row.get(1)?;
            Ok((id_str, updated))
        })?;
        let mut result = Vec::new();
        for r in rows {
            let (id_str, updated) = r?;
            if let Ok(id) = MediaId::from_str(&id_str) {
                result.push((id, updated));
            }
        }
        Ok(result)
    }

    /// User's in-progress (resume) units, ordered by most recently updated first.
    pub fn user_resumable_units(
        &self,
        user_id: UserId,
        limit: usize,
    ) -> Result<Vec<(MediaId, i32, i32, i64, i64)>, StoreError> {
        let mut stmt = self.subscribe.prepare(
            "SELECT media_id, season, episode, position_ms, updated_at
             FROM playback_units
             WHERE user_id = ?1 AND position_ms > 0 AND played = 0
             ORDER BY updated_at DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![user_id.to_string(), limit as i64], |row| {
            let media_str: String = row.get(0)?;
            let season: i32 = row.get(1)?;
            let episode: i32 = row.get(2)?;
            let pos: i64 = row.get(3)?;
            let updated: i64 = row.get(4)?;
            Ok((media_str, season, episode, pos, updated))
        })?;
        let mut list = Vec::new();
        for r in rows {
            let (m_str, season, episode, pos, updated) = r?;
            if let Ok(m_id) = MediaId::from_str(&m_str) {
                list.push((m_id, season, episode, pos, updated));
            }
        }
        Ok(list)
    }

    /// Favourited units ordered by favourite activity, newest first.
    /// A pre-migration favourite (`favorite_at = 0`) keeps its playback
    /// timestamp as the ordering key.
    pub fn favorite_rows(&self, user_id: UserId) -> Result<Vec<(MediaId, i64, i32, i32)>, StoreError> {
        let mut stmt = self.subscribe.prepare(
            "SELECT media_id, season, episode,
                    CASE WHEN favorite_at != 0 THEN favorite_at ELSE updated_at END AS favorited
             FROM playback_units
             WHERE user_id = ?1 AND favorite != 0
             ORDER BY favorited DESC, media_id, season, episode",
        )?;
        let rows = stmt.query_map(params![user_id.to_string()], |row| {
            Ok((row.get::<_, String>(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?;
        let mut list = Vec::new();
        for row in rows {
            let (media, season, episode, favorited) = row?;
            if let Ok(media_id) = MediaId::from_str(&media) {
                list.push((media_id, favorited, season, episode));
            }
        }
        Ok(list)
    }

    /// All unit rows of a media (for marks aggregation / cascade).
    pub fn unit_rows(
        &self,
        user_id: UserId,
        media_id: MediaId,
    ) -> Result<Vec<UnitRow>, StoreError> {
        let mut stmt = self.subscribe.prepare(
            "SELECT season, episode, played, favorite, updated_at, favorite_at
             FROM playback_units WHERE user_id = ?1 AND media_id = ?2",
        )?;
        let rows = stmt.query_map(params![user_id.to_string(), media_id.to_string()], |row| {
            Ok(UnitRow {
                season: row.get(0)?,
                episode: row.get(1)?,
                played: row.get::<_, i64>(2)? != 0,
                favorite: row.get::<_, i64>(3)? != 0,
                updated_at: row.get(4)?,
                favorite_at: row.get(5)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Set played marks with cascade (whole/season marks every unit below);
    /// favorite is stored on exactly the target unit (favorite level = the
    /// most recent favorited unit).
    #[allow(clippy::too_many_arguments)]
    pub fn set_unit_marks(
        &self,
        user_id: UserId,
        media_id: MediaId,
        season: i32,
        episode: i32,
        played: Option<bool>,
        favorite: Option<bool>,
        now: i64,
    ) -> Result<(), StoreError> {
        self.playback_write(|store| {
            if (season, episode) == (-1, -1) {
                store.copy_legacy_movie_unit_if_missing(user_id, media_id)?;
            }
            store.set_unit_marks_inner(user_id, media_id, season, episode, played, favorite, now)
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn set_unit_marks_inner(
        &self,
        user_id: UserId,
        media_id: MediaId,
        season: i32,
        episode: i32,
        played: Option<bool>,
        favorite: Option<bool>,
        now: i64,
    ) -> Result<(), StoreError> {
        if let Some(favorite) = favorite {
            // Favourite activity is its own clock. Playback fields stay on the
            // row that already owns them; a missing unit is favourite-only.
            if played.is_none() {
                self.set_unit_favorite(user_id, media_id, season, episode, favorite, now)?;
            } else {
                let existing = self.unit_state(user_id, media_id, season, episode)?;
                self.upsert_unit(
                    user_id,
                    media_id,
                    season,
                    episode,
                    existing.as_ref().map(|unit| unit.position_ms).unwrap_or(0),
                    played,
                    Some(favorite),
                    existing.as_ref().and_then(|unit| unit.duration_ms),
                    existing.as_ref().and_then(|unit| unit.audio_track.as_deref()),
                    existing.as_ref().and_then(|unit| unit.subtitle_track.as_deref()),
                    false,
                    now,
                )?;
            }
            if !favorite && season == UNIT_WHOLE && episode == UNIT_WHOLE {
                self.subscribe.execute(
                    "UPDATE playback_units SET favorite = 0, favorite_at = 0
                     WHERE user_id = ?1 AND media_id = ?2",
                    params![user_id.to_string(), media_id.to_string()],
                )?;
            }
        }
        if let Some(played) = played {
            let (s_from, e_from) = match (season, episode) {
                (UNIT_WHOLE, _) => (UNIT_WHOLE, UNIT_WHOLE),
                (s, UNIT_WHOLE) => (s, UNIT_WHOLE),
                (s, e) => (s, e),
            };
            let mut stmt = self.subscribe.prepare(
                "UPDATE playback_units SET played = ?3, updated_at = ?4
                 WHERE user_id = ?1 AND media_id = ?2
                   AND season >= ?5 AND episode >= ?6
                   AND (?5 = -1 OR season = ?5)
                   AND (?6 = -1 OR episode = ?6)",
            )?;
            let _ = stmt.execute(params![
                user_id.to_string(),
                media_id.to_string(),
                played as i64,
                now,
                s_from,
                e_from,
            ])?;
            drop(stmt);
            // Ensure the target unit exists so unplayed_count is well-defined.
            if played {
                let existing = self.unit_state(user_id, media_id, season, episode)?;
                if existing.is_none() {
                    self.upsert_unit(
                        user_id,
                        media_id,
                        season,
                        episode,
                        0,
                        Some(true),
                        favorite,
                        None,
                        None,
                        None,
                        false,
                        now,
                    )?;
                }
            }
        }
        Ok(())
    }

    /// Clear viewing facts, retaining favorites, duration and track preferences.
    pub fn clear_units(
        &self,
        user_id: UserId,
        media_ids: Option<&[MediaId]>,
        since_ms: Option<i64>,
    ) -> Result<usize, StoreError> {
        let mut sql = String::from(
            "UPDATE playback_units SET position_ms = 0, played = 0, play_count = 0, updated_at = 0 WHERE user_id = ?",
        );
        let mut values: Vec<rusqlite::types::Value> =
            vec![rusqlite::types::Value::Text(user_id.to_string())];
        if let Some(since) = since_ms {
            sql.push_str(" AND updated_at >= ?");
            values.push(rusqlite::types::Value::Integer(since));
        }
        if let Some(ids) = media_ids {
            if ids.is_empty() {
                return Ok(0);
            }
            let marks = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            sql.push_str(&format!(" AND media_id IN ({marks})"));
            values.extend(
                ids.iter()
                    .map(|id| rusqlite::types::Value::Text(id.to_string())),
            );
        }
        let changed = self
            .subscribe
            .execute(&sql, rusqlite::params_from_iter(values))?;
        Ok(changed)
    }

    pub fn delete_playback_for_media(&self, media_id: domain::MediaId) -> Result<(), StoreError> {
        let id_str = media_id.to_string();
        self.subscribe.execute(
            "DELETE FROM playback_units WHERE media_id = ?1",
            params![&id_str],
        )?;
        self.subscribe.execute(
            "DELETE FROM playback_sessions WHERE media_id = ?1",
            params![&id_str],
        )?;
        self.subscribe.execute(
            "DELETE FROM playback_logs WHERE media_id = ?1",
            params![&id_str],
        )?;
        self.subscribe.execute(
            "DELETE FROM playback_metrics WHERE media_id = ?1",
            params![&id_str],
        )?;
        Ok(())
    }
}
