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
