//! Batch read facts: `Store::media_batch` and
//! `Store::playback_units_for_user` (full `UnitState`, not a reduced row).

use domain::{Media, MediaId, MediaKind, UserId};
use store::Store;

fn media(kind: MediaKind, title: &str, tmdb_id: Option<&str>) -> Media {
    Media {
        id: MediaId::new(),
        kind,
        title: title.into(),
        year: None,
        original_title: None,
        tmdb_id: tmdb_id.map(str::to_string),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

#[test]
fn media_batch_returns_requested_ids_and_skips_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let movie = media(MediaKind::Movie, "Shared alias", Some("603"));
    let show = media(MediaKind::Tv, "Shared alias", Some("603"));
    store.insert_media(&movie).unwrap();
    store.insert_media(&show).unwrap();
    let missing = MediaId::new();

    let found = store.media_batch(&[movie.id, show.id, missing]).unwrap();

    assert_eq!(found.len(), 2, "same numeric alias must not merge rows");
    assert_eq!(found.get(&movie.id).unwrap().kind, MediaKind::Movie);
    assert_eq!(found.get(&show.id).unwrap().kind, MediaKind::Tv);
    assert_eq!(found.get(&show.id).unwrap().tmdb_id.as_deref(), Some("603"));
    assert!(!found.contains_key(&missing));
}

#[test]
fn media_batch_empty_input_returns_empty_map() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    assert!(store.media_batch(&[]).unwrap().is_empty());
}

#[test]
fn media_batch_spans_the_500_id_chunk_boundary() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let mut ids = Vec::new();
    for index in 0..501 {
        let item = media(MediaKind::Movie, &format!("item {index}"), None);
        store.insert_media(&item).unwrap();
        ids.push(item.id);
    }

    let found = store.media_batch(&ids).unwrap();
    assert_eq!(found.len(), 501);
    for id in ids {
        assert!(found.contains_key(&id), "missing {id}");
    }
}

#[test]
fn playback_units_for_user_keeps_full_state_for_partial_progress() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let movie = media(MediaKind::Movie, "Partial", Some("603"));
    store.insert_media(&movie).unwrap();
    let user = UserId::new();
    store
        .upsert_unit(
            user,
            movie.id,
            1,
            2,
            123_456,
            Some(false),
            Some(true),
            Some(3_600_000),
            Some("jpn"),
            Some("zh"),
            false,
            42,
        )
        .unwrap();
    store
        .upsert_unit(user, movie.id, -1, -1, 0, Some(true), Some(false), None, None, None, true, 43)
        .unwrap();

    let units = store.playback_units_for_user(user).unwrap();
    assert_eq!(units.len(), 2);
    let partial = units
        .iter()
        .find(|unit| unit.season == 1 && unit.episode == 2)
        .expect("partial unit");
    assert_eq!(partial.media_id, movie.id);
    assert_eq!(partial.state.position_ms, 123_456);
    assert!(!partial.state.played, "watched mark must not be implied");
    assert!(partial.state.favorite);
    assert_eq!(partial.state.duration_ms, Some(3_600_000));
    assert_eq!(partial.state.audio_track.as_deref(), Some("jpn"));
    assert_eq!(partial.state.subtitle_track.as_deref(), Some("zh"));
    assert_eq!(partial.state.play_count, 0);
    assert_eq!(partial.state.updated_at, 42);
    assert_eq!(
        partial.state,
        store.unit_state(user, movie.id, 1, 2).unwrap().unwrap()
    );
}

#[test]
fn favorite_only_update_preserves_played_activity_timestamp() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let user = UserId::new();
    let media = media(MediaKind::Tv, "Favorite timestamp", None);
    store.insert_media(&media).unwrap();
    store.upsert_unit(user, media.id, 1, -1, 0, Some(true), Some(false), None, None, None, false, 10).unwrap();
    store.set_unit_favorite(user, media.id, 1, -1, true, 99).unwrap();
    let state = store.unit_state(user, media.id, 1, -1).unwrap().unwrap();
    assert!(state.played);
    assert!(state.favorite);
    assert_eq!(state.updated_at, 10);
}

#[test]
fn whole_unit_unfavorite_preserves_other_units_activity_timestamps() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let user = UserId::new();
    let media = media(MediaKind::Tv, "Cascade favorite timestamp", None);
    store.insert_media(&media).unwrap();
    store.upsert_unit(user, media.id, -1, -1, 0, Some(false), Some(true), None, None, None, false, 5).unwrap();
    store.upsert_unit(user, media.id, 1, -1, 0, Some(false), Some(true), None, None, None, false, 10).unwrap();
    store.upsert_unit(user, media.id, 1, 1, 0, Some(false), Some(true), None, None, None, false, 15).unwrap();

    store.set_unit_marks(user, media.id, -1, -1, None, Some(false), 99).unwrap();

    assert_eq!(store.unit_state(user, media.id, -1, -1).unwrap().unwrap().updated_at, 5);
    assert_eq!(store.unit_state(user, media.id, 1, -1).unwrap().unwrap().updated_at, 10);
    assert_eq!(store.unit_state(user, media.id, 1, 1).unwrap().unwrap().updated_at, 15);
    assert!(!store.unit_state(user, media.id, 1, 1).unwrap().unwrap().favorite);
}

#[test]
fn first_movie_favorite_copies_legacy_whole_file_playback_state() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let user = UserId::new();
    let movie = media(MediaKind::Movie, "Legacy favorite", None);
    store.insert_media(&movie).unwrap();
    store
        .upsert_unit(
            user,
            movie.id,
            0,
            0,
            600_000,
            Some(true),
            Some(false),
            Some(7_200_000),
            Some("audio-old"),
            Some("subtitle-old"),
            true,
            42,
        )
        .unwrap();

    store
        .set_unit_marks(user, movie.id, -1, -1, None, Some(true), 99)
        .unwrap();

    let canonical = store.unit_state(user, movie.id, -1, -1).unwrap().unwrap();
    assert!(canonical.favorite);
    assert_eq!(canonical.position_ms, 600_000);
    assert!(canonical.played);
    assert_eq!(canonical.play_count, 1);
    assert_eq!(canonical.duration_ms, Some(7_200_000));
    assert_eq!(canonical.audio_track.as_deref(), Some("audio-old"));
    assert_eq!(canonical.subtitle_track.as_deref(), Some("subtitle-old"));
    assert_eq!(canonical.updated_at, 42, "legacy activity must move with the state");
    assert_eq!(canonical.favorite_at, 99);
}

#[test]
fn favorite_activity_orders_independently_of_played_activity() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let user = UserId::new();
    let older = media(MediaKind::Movie, "Older favorite", None);
    let newer = media(MediaKind::Movie, "Newer favorite", None);
    store.insert_media(&older).unwrap();
    store.insert_media(&newer).unwrap();
    store
        .upsert_unit(user, older.id, -1, -1, 0, Some(true), Some(true), None, None, None, false, 10)
        .unwrap();
    store
        .upsert_unit(user, newer.id, -1, -1, 0, Some(true), Some(false), None, None, None, false, 50)
        .unwrap();

    store
        .set_unit_marks(user, newer.id, -1, -1, None, Some(true), 80)
        .unwrap();

    let rows = store.favorite_rows(user).unwrap();
    assert_eq!(rows[0].0, newer.id, "a new favourite must lead recent favourites");
    assert_eq!(rows[0].1, 80);
    let played = store.user_active_media_ids(user).unwrap();
    assert_eq!(played[0].0, newer.id);
    assert_eq!(played[0].1, 50, "favouriting must not refresh the played clock");
    assert_eq!(store.unit_state(user, newer.id, -1, -1).unwrap().unwrap().updated_at, 50);
}

#[test]
fn favorite_only_insert_does_not_materialize_inherited_played() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let user = UserId::new();
    let show = media(MediaKind::Tv, "Inherited played", None);
    store.insert_media(&show).unwrap();
    store
        .upsert_unit(user, show.id, 1, -1, 0, Some(true), None, None, None, None, false, 10)
        .unwrap();

    store
        .set_unit_favorite(user, show.id, 1, 1, true, 20)
        .unwrap();
    store.clear_units(user, None, Some(1)).unwrap();

    let episode = store.unit_state(user, show.id, 1, 1).unwrap().unwrap();
    assert!(episode.favorite);
    assert!(!episode.played, "inherited played must not survive as an explicit fact");
    assert_eq!(episode.updated_at, 0);
}

#[test]
fn jellyfin_first_favorite_keeps_legacy_resume_played_and_play_count() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let user = UserId::new();
    let movie = media(MediaKind::Movie, "Jellyfin legacy favorite", None);
    store.insert_media(&movie).unwrap();
    store
        .upsert_unit(
            user,
            movie.id,
            0,
            0,
            600_000,
            Some(true),
            Some(false),
            Some(7_200_000),
            Some("audio-old"),
            Some("subtitle-old"),
            true,
            42,
        )
        .unwrap();

    store
        .set_unit_favorite(user, movie.id, -1, -1, true, 99)
        .unwrap();

    let canonical = store.unit_state(user, movie.id, -1, -1).unwrap().unwrap();
    assert!(canonical.favorite);
    assert_eq!(canonical.position_ms, 600_000, "first favourite must keep the legacy resume");
    assert!(canonical.played, "first favourite must keep the legacy played mark");
    assert_eq!(canonical.play_count, 1, "first favourite must keep the legacy play count");
    assert_eq!(canonical.duration_ms, Some(7_200_000));
    assert_eq!(canonical.audio_track.as_deref(), Some("audio-old"));
    assert_eq!(canonical.subtitle_track.as_deref(), Some("subtitle-old"));
    assert_eq!(canonical.updated_at, 42);
    assert_eq!(canonical.favorite_at, 99);
}

#[test]
fn favoriting_legacy_movie_fills_existing_favorite_only_canonical_row() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let user = UserId::new();
    let movie = media(MediaKind::Movie, "Blocked legacy favorite", None);
    store.insert_media(&movie).unwrap();
    store
        .upsert_unit(
            user,
            movie.id,
            0,
            0,
            600_000,
            Some(false),
            Some(false),
            Some(7_200_000),
            Some("audio-old"),
            Some("subtitle-old"),
            true,
            42,
        )
        .unwrap();
    // A previous round created the canonical key as favourite-only before the
    // legacy copy could run. That row has no playback authority.
    {
        let db = rusqlite::Connection::open(tmp.path().join("data/subscribe.db")).unwrap();
        db.execute(
            "INSERT INTO playback_units
                (user_id, media_id, season, episode, position_ms, played, favorite,
                 duration_ms, audio_track, subtitle_track, play_count, updated_at, favorite_at)
             VALUES (?1, ?2, -1, -1, 0, 0, 1, NULL, NULL, NULL, 0, 0, 15)",
            rusqlite::params![user.to_string(), movie.id.to_string()],
        )
        .unwrap();
    }

    store
        .set_unit_favorite(user, movie.id, -1, -1, true, 99)
        .unwrap();

    let canonical = store.unit_state(user, movie.id, -1, -1).unwrap().unwrap();
    assert_eq!(canonical.position_ms, 600_000, "the only playback position must transfer");
    assert_eq!(canonical.play_count, 1, "the only play count must transfer");
    assert_eq!(canonical.duration_ms, Some(7_200_000));
    assert_eq!(canonical.audio_track.as_deref(), Some("audio-old"));
    assert_eq!(canonical.subtitle_track.as_deref(), Some("subtitle-old"));
    assert_eq!(canonical.updated_at, 42, "legacy activity must move with the playback state");
    assert!(canonical.favorite);
    assert_eq!(canonical.favorite_at, 15, "an independently recorded favourite clock must survive");
    assert!(!canonical.played);
    assert!(
        store.unit_state(user, movie.id, 0, 0).unwrap().is_none(),
        "the legacy row retires only after its playback fields move"
    );
}

#[test]
fn favoriting_legacy_movie_does_not_duplicate_resume_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let user = UserId::new();
    let legacy = media(MediaKind::Movie, "Legacy resume", None);
    let other = media(MediaKind::Movie, "Other resume", None);
    let show = media(MediaKind::Tv, "Real episode zero", None);
    store.insert_media(&legacy).unwrap();
    store.insert_media(&other).unwrap();
    store.insert_media(&show).unwrap();
    store
        .upsert_unit(user, legacy.id, 0, 0, 120_000, Some(false), Some(false), None, None, None, false, 10)
        .unwrap();
    store
        .upsert_unit(user, other.id, -1, -1, 30_000, Some(false), Some(false), None, None, None, false, 20)
        .unwrap();
    store
        .upsert_unit(user, show.id, 0, 0, 45_000, Some(false), Some(false), None, None, None, false, 5)
        .unwrap();

    store.set_unit_marks(user, legacy.id, -1, -1, None, Some(true), 30).unwrap();

    let rows = store.user_resumable_units(user).unwrap();
    assert_eq!(
        rows.iter().filter(|row| row.0 == legacy.id).count(),
        1,
        "a favourited legacy movie must not leave two resumable rows: {rows:?}"
    );
    assert!(rows.iter().any(|row| row.0 == other.id), "the other title must remain resumable: {rows:?}");
    assert!(
        rows.iter().any(|row| row.0 == show.id && row.1 == 0 && row.2 == 0),
        "a real TV (0, 0) episode must stay resumable: {rows:?}"
    );
}

#[test]
fn playback_units_for_user_is_scoped_to_one_user() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let movie = media(MediaKind::Movie, "Scoped", None);
    store.insert_media(&movie).unwrap();
    let owner = UserId::new();
    store
        .upsert_unit(owner, movie.id, -1, -1, 5_000, Some(false), Some(false), None, None, None, false, 7)
        .unwrap();

    assert_eq!(store.playback_units_for_user(owner).unwrap().len(), 1);
    assert!(store.playback_units_for_user(UserId::new()).unwrap().is_empty());
}
