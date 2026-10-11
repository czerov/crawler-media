use domain::{LedgerRow, Media, MediaKind, UserId};
use media_server::provider::{PlaybackClientInfo, PlaybackEvent};

use crate::store::{SessionRow, Store, StoreError, UNIT_WHOLE, UnitState};

use super::ApiServerProvider;

struct PlaybackTarget {
    media: Media,
    ledger_id: String,
    season: i32,
    episode: i32,
    duration_ms: Option<i64>,
}

fn resolve_target(
    provider: &ApiServerProvider,
    user_id: UserId,
    id: &str,
) -> Result<Option<PlaybackTarget>, String> {
    let store = provider.state.store.lock();
    let Some(row) = crate::http::media_visibility::resolve_visible_row(&store, id, Some(user_id))
    else {
        return Ok(None);
    };
    let media = store
        .get_media(row.media_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("media {} is missing", row.media_id))?;
    let (season, episode) = playback_unit(&media, &row);
    let duration_ms =
        crate::http::playback::movie_compatible_state(&store, user_id, media.id, season, episode)
            .map_err(|error| error.to_string())?
            .and_then(|unit| unit.duration_ms)
            .or_else(|| {
                store
                    .get_file_meta(&row.id.to_string())
                    .ok()
                    .flatten()
                    .and_then(|tracks| tracks.video.and_then(|video| video.duration_secs))
                    .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
                    .map(|seconds| (seconds * 1000.0) as i64)
            });
    Ok(Some(PlaybackTarget {
        media,
        ledger_id: row.id.to_string(),
        season,
        episode,
        duration_ms,
    }))
}

fn playback_unit(media: &Media, row: &LedgerRow) -> (i32, i32) {
    if media.kind == MediaKind::Tv {
        if let (Some(season), Some(episode)) = (row.season, row.episode) {
            return (season as i32, episode as i32);
        }
    }
    (UNIT_WHOLE, UNIT_WHOLE)
}

fn persist_position(
    provider: &ApiServerProvider,
    user_id: UserId,
    target: &PlaybackTarget,
    position_ms: i64,
    increment_play_count: bool,
    now: i64,
) -> Result<(), String> {
    provider
        .state
        .store
        .lock()
        .playback_write(|store| {
            persist_event_position(
                store,
                user_id,
                target,
                position_ms.max(0),
                increment_play_count,
                now,
            )
        })
        .map_err(|error| error.to_string())
}

pub(super) fn update_progress(
    provider: &ApiServerProvider,
    user_id: UserId,
    id: &str,
    position_ms: i64,
) -> Result<(), String> {
    let target = resolve_target(provider, user_id, id)?
        .ok_or_else(|| format!("Jellyfin playback item {id} is missing or not visible"))?;
    persist_position(
        provider,
        user_id,
        &target,
        position_ms,
        false,
        crate::job_loop::unix_now(),
    )
}

pub(super) fn report_event(
    provider: &ApiServerProvider,
    user_id: UserId,
    id: &str,
    position_ms: i64,
    paused: bool,
    client: PlaybackClientInfo,
    event: PlaybackEvent,
) -> Result<(), String> {
    let target = resolve_target(provider, user_id, id)?
        .ok_or_else(|| format!("Jellyfin playback item {id} is missing or not visible"))?;
    let now = crate::job_loop::unix_now();
    let position_ms = position_ms.max(0);
    let store = provider.state.store.lock();
    if store
        .is_device_revoked(user_id, &client.device_id)
        .map_err(|error| error.to_string())?
    {
        return Err("device revoked".into());
    }
    store
        .playback_write(|store| {
            persist_event(
                store,
                user_id,
                &target,
                position_ms,
                paused,
                &client,
                event,
                now,
            )
        })
        .map_err(|error| error.to_string())
}

#[allow(clippy::too_many_arguments)]
fn persist_event(
    store: &Store,
    user_id: UserId,
    target: &PlaybackTarget,
    position_ms: i64,
    paused: bool,
    client: &PlaybackClientInfo,
    event: PlaybackEvent,
    now: i64,
) -> Result<(), StoreError> {
    let current = current_session(store, user_id, &client.device_id, target, now)?;
    if current.as_ref().is_some_and(|s| s.admin_ended) {
        return Ok(());
    }
    // 验证目标所属媒体库未处于删除冻结中且台账未被清理，防止已删除文件在心跳中复活
    if let Some(row) = store.get_ledger(&target.ledger_id)? {
        if let Some(owner) = store.library_for_path_strict(std::path::Path::new(&row.path), target.media.kind)? {
            if !store.library_accepts_work(&owner.id).unwrap_or(true) {
                return Ok(());
            }
        }
    } else {
        return Ok(());
    }

    persist_event_position(
        store,
        user_id,
        target,
        position_ms,
        event == PlaybackEvent::Playing && current.is_none(),
        now,
    )?;
    let watched_ms = accumulated_watch_time(
        current.as_ref(),
        position_ms,
        paused && event != PlaybackEvent::Stopped,
    );
    if event == PlaybackEvent::Stopped {
        return stop_session(
            store,
            client,
            user_id,
            target,
            position_ms,
            watched_ms,
            current.as_ref(),
            now,
        );
    }
    store.upsert_session(&session_row(
        client,
        user_id,
        target,
        position_ms,
        paused,
        watched_ms,
        current.as_ref(),
        now,
    ))
}

fn current_session(
    store: &Store,
    user_id: UserId,
    device_id: &str,
    target: &PlaybackTarget,
    now: i64,
) -> Result<Option<SessionRow>, StoreError> {
    let prior = store.get_session(user_id, device_id)?;
    if prior
        .as_ref()
        .is_some_and(|session| same_target(session, target))
    {
        return Ok(prior);
    }
    if prior.is_some() {
        store.close_session(user_id, device_id, now)?;
    }
    Ok(None)
}

fn same_target(session: &SessionRow, target: &PlaybackTarget) -> bool {
    session.media_id == target.media.id
        && session.season == unit_value(target.season)
        && session.episode == unit_value(target.episode)
}

fn persist_event_position(
    store: &Store,
    user_id: UserId,
    target: &PlaybackTarget,
    position_ms: i64,
    increment_play_count: bool,
    now: i64,
) -> Result<(), StoreError> {
    if (target.season, target.episode) == (UNIT_WHOLE, UNIT_WHOLE) {
        store.copy_legacy_movie_unit_if_missing(user_id, target.media.id)?;
    }
    let duration = target.duration_ms.or(store
        .unit_state(user_id, target.media.id, target.season, target.episode)?
        .and_then(|unit| unit.duration_ms));
    store.upsert_unit(
        user_id,
        target.media.id,
        target.season,
        target.episode,
        position_ms,
        UnitState::playback_completed(position_ms, duration).then_some(true),
        None,
        duration,
        None,
        None,
        increment_play_count,
        now,
    )?;
    Ok(())
}

fn accumulated_watch_time(current: Option<&SessionRow>, position_ms: i64, paused: bool) -> i64 {
    let Some(session) = current else {
        return 0;
    };
    let delta = if session.paused || paused {
        0
    } else {
        (position_ms - session.position_ms).max(0)
    };
    session.watched_ms + delta
}

fn stop_session(
    store: &Store,
    client: &PlaybackClientInfo,
    user_id: UserId,
    target: &PlaybackTarget,
    position_ms: i64,
    watched_ms: i64,
    existing: Option<&SessionRow>,
    now: i64,
) -> Result<(), StoreError> {
    if existing.is_none() {
        return Ok(());
    }
    store.upsert_session(&session_row(
        client,
        user_id,
        target,
        position_ms,
        true,
        watched_ms,
        existing,
        now,
    ))?;
    store.close_session(user_id, &client.device_id, now)?;
    Ok(())
}

fn unit_value(value: i32) -> Option<i32> {
    (value != UNIT_WHOLE).then_some(value)
}

fn session_row(
    client: &PlaybackClientInfo,
    user_id: UserId,
    target: &PlaybackTarget,
    position_ms: i64,
    paused: bool,
    watched_ms: i64,
    existing: Option<&SessionRow>,
    now: i64,
) -> SessionRow {
    SessionRow {
        device_id: client.device_id.clone(),
        user_id,
        media_id: target.media.id,
        season: unit_value(target.season),
        episode: unit_value(target.episode),
        client: client
            .client
            .clone()
            .or_else(|| existing.and_then(|session| session.client.clone())),
        device_name: client
            .device_name
            .clone()
            .or_else(|| existing.and_then(|session| session.device_name.clone())),
        client_version: client
            .client_version
            .clone()
            .or_else(|| existing.and_then(|session| session.client_version.clone())),
        play_method: "DirectPlay".to_string(),
        position_ms,
        start_position_ms: existing
            .map(|session| session.start_position_ms)
            .unwrap_or(position_ms),
        duration_ms: target
            .duration_ms
            .or_else(|| existing.and_then(|session| session.duration_ms)),
        paused,
        watched_ms,
        rate_bps: existing.map(|session| session.rate_bps).unwrap_or(0),
        bytes_sent: existing.map(|session| session.bytes_sent).unwrap_or(0),
        connections: existing.map(|session| session.connections).unwrap_or(1),
        admin_ended: existing.map(|session| session.admin_ended).unwrap_or(false),
        started_at: existing.map(|session| session.started_at).unwrap_or(now),
        last_report_at: now,
        ledger_id: Some(target.ledger_id.clone()),
    }
}
