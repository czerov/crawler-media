use domain::UserId;
use rusqlite::{OptionalExtension, params};

use super::{PlayLogRow, SessionRow, UnitState, media_id_from, user_id_from};
use super::{Store, StoreError};

impl Store {
    pub fn upsert_session(&self, session: &SessionRow) -> Result<(), StoreError> {
        self.subscribe.execute(
            "INSERT OR REPLACE INTO playback_sessions
                (device_id, user_id, media_id, season, episode, client, device_name,
                 client_version, play_method, position_ms, start_position_ms, duration_ms,
                 paused, watched_ms, rate_bps, bytes_sent, connections, admin_ended,
                 started_at, last_report_at, ledger_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                     ?16, ?17, ?18, ?19, ?20, ?21)",
            params![
                session.device_id,
                session.user_id.to_string(),
                session.media_id.to_string(),
                session.season,
                session.episode,
                session.client,
                session.device_name,
                session.client_version,
                session.play_method,
                session.position_ms,
                session.start_position_ms,
                session.duration_ms,
                session.paused as i64,
                session.watched_ms,
                session.rate_bps,
                session.bytes_sent,
                session.connections,
                session.admin_ended as i64,
                session.started_at,
                session.last_report_at,
                session.ledger_id,
            ],
        )?;
        Ok(())
    }

    pub fn get_session(
        &self,
        user_id: UserId,
        device_id: &str,
    ) -> Result<Option<SessionRow>, StoreError> {
        self.subscribe
            .query_row(
                "SELECT device_id, user_id, media_id, season, episode, client, device_name,
                        client_version, play_method, position_ms, start_position_ms,
                        duration_ms, paused, watched_ms, rate_bps, bytes_sent, connections,
                        admin_ended, started_at, last_report_at, ledger_id
                 FROM playback_sessions WHERE user_id = ?1 AND device_id = ?2",
                params![user_id.to_string(), device_id],
                map_session,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn sessions_for_device(&self, device_id: &str) -> Result<Vec<SessionRow>, StoreError> {
        let mut stmt = self.subscribe.prepare(
            "SELECT device_id, user_id, media_id, season, episode, client, device_name,
                    client_version, play_method, position_ms, start_position_ms,
                    duration_ms, paused, watched_ms, rate_bps, bytes_sent, connections,
                    admin_ended, started_at, last_report_at, ledger_id
             FROM playback_sessions WHERE device_id = ?1
             ORDER BY last_report_at DESC",
        )?;
        let rows = stmt.query_map(params![device_id], map_session)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Sessions with a heartbeat inside the timeout window.
    pub fn active_sessions(
        &self,
        now: i64,
        timeout_secs: i64,
    ) -> Result<Vec<SessionRow>, StoreError> {
        let cutoff = now - timeout_secs;
        let mut stmt = self.subscribe.prepare(
            "SELECT device_id, user_id, media_id, season, episode, client, device_name,
                    client_version, play_method, position_ms, start_position_ms,
                    duration_ms, paused, watched_ms, rate_bps, bytes_sent, connections,
                    admin_ended, started_at, last_report_at, ledger_id
             FROM playback_sessions WHERE last_report_at >= ?1
             ORDER BY last_report_at DESC",
        )?;
        let rows = stmt.query_map(params![cutoff], map_session)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Close a session into a play log; returns the log row. Missing session = None.
    pub fn close_session(
        &self,
        user_id: UserId,
        device_id: &str,
        now: i64,
    ) -> Result<Option<PlayLogRow>, StoreError> {
        self.playback_write(|store| store.close_session_inner(user_id, device_id, now))
    }

    fn close_session_inner(
        &self,
        user_id: UserId,
        device_id: &str,
        now: i64,
    ) -> Result<Option<PlayLogRow>, StoreError> {
        let Some(session) = self.get_session(user_id, device_id)? else {
            return Ok(None);
        };
        self.subscribe.execute(
            "DELETE FROM playback_sessions WHERE user_id = ?1 AND device_id = ?2",
            params![user_id.to_string(), device_id],
        )?;
        let completed = UnitState::playback_completed(session.position_ms, session.duration_ms);
        let log = PlayLogRow {
            id: uuid::Uuid::new_v4().to_string(),
            user_id: session.user_id,
            media_id: session.media_id,
            season: session.season,
            episode: session.episode,
            device_id: Some(session.device_id),
            client: session.client,
            device_name: session.device_name,
            play_method: session.play_method,
            started_at: session.started_at,
            ended_at: now,
            watched_ms: session.watched_ms,
            start_position_ms: session.start_position_ms,
            end_position_ms: session.position_ms,
            duration_ms: session.duration_ms,
            completed,
        };
        self.subscribe.execute(
            "INSERT INTO playback_logs
                (id, user_id, media_id, season, episode, client, device_name, play_method,
                 started_at, ended_at, watched_ms, start_position_ms, end_position_ms,
                 duration_ms, completed, device_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                log.id,
                log.user_id.to_string(),
                log.media_id.to_string(),
                log.season,
                log.episode,
                log.client,
                log.device_name,
                log.play_method,
                log.started_at,
                log.ended_at,
                log.watched_ms,
                log.start_position_ms,
                log.end_position_ms,
                log.duration_ms,
                log.completed as i64,
                log.device_id,
            ],
        )?;
        Ok(Some(log))
    }

    pub fn delete_session(&self, user_id: UserId, device_id: &str) -> Result<bool, StoreError> {
        let n = self.subscribe.execute(
            "DELETE FROM playback_sessions WHERE user_id = ?1 AND device_id = ?2",
            params![user_id.to_string(), device_id],
        )?;
        Ok(n > 0)
    }

    pub fn end_device_session(
        &self,
        user_id: UserId,
        device_id: &str,
        now: i64,
    ) -> Result<bool, StoreError> {
        Ok(self.close_session(user_id, device_id, now)?.is_some())
    }

    /// Close every session whose heartbeat is older than the timeout.
    pub fn sweep_stale_sessions(&self, now: i64, timeout_secs: i64) -> Result<usize, StoreError> {
        let cutoff = now - timeout_secs;
        let session_keys: Vec<(UserId, String)> = {
            let mut stmt = self.subscribe.prepare(
                "SELECT user_id, device_id FROM playback_sessions WHERE last_report_at < ?1",
            )?;
            let rows = stmt.query_map(params![cutoff], |row| {
                Ok((user_id_from(row, 0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut closed = 0;
        for (user_id, device_id) in session_keys {
            if self.close_session(user_id, &device_id, now)?.is_some() {
                closed += 1;
            }
        }
        Ok(closed)
    }
}

fn map_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRow> {
    Ok(SessionRow {
        device_id: row.get(0)?,
        user_id: user_id_from(row, 1)?,
        media_id: media_id_from(row, 2)?,
        season: row.get(3)?,
        episode: row.get(4)?,
        client: row.get(5)?,
        device_name: row.get(6)?,
        client_version: row.get(7)?,
        play_method: row.get(8)?,
        position_ms: row.get(9)?,
        start_position_ms: row.get(10)?,
        duration_ms: row.get(11)?,
        paused: row.get::<_, i64>(12)? != 0,
        watched_ms: row.get(13)?,
        rate_bps: row.get(14)?,
        bytes_sent: row.get(15)?,
        connections: row.get(16)?,
        admin_ended: row.get::<_, i64>(17)? != 0,
        started_at: row.get(18)?,
        last_report_at: row.get(19)?,
        ledger_id: row.get(20)?,
    })
}
