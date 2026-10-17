use std::path::Path;

#[test]
fn parallel_versions_match_exact_source_not_first_episode_destination() {
    let tmp = tempfile::tempdir().unwrap();
    let subtitle = tmp.path().join("Media.S01E01.2160p.zh.srt");
    std::fs::write(&subtitle, b"2160 subtitle").unwrap();
    let first = tmp.path().join("1080.mkv");
    let second = tmp.path().join("2160.mkv");
    let mappings = [
        DestinationMapping::new(tmp.path().join("Media.S01E01.1080p.mkv").display().to_string(), &first),
        DestinationMapping::new(tmp.path().join("Media.S01E01.2160p.mkv").display().to_string(), &second),
    ];
    subscribe::sidecars::place_mapped_subtitle(&subtitle, &mappings, Some(library::TransferMode::Copy)).unwrap();
    assert!(!first.with_extension("zh.srt").exists());
    assert_eq!(std::fs::read(second.with_extension("zh.srt")).unwrap(), b"2160 subtitle");
}

#[test]
fn unmatched_episode_never_attaches_to_singleton_video() {
    let tmp = tempfile::tempdir().unwrap();
    let subtitle = tmp.path().join("Media.S01E01.zh.srt");
    std::fs::write(&subtitle, b"E1").unwrap();
    let destination = tmp.path().join("02.mkv");
    let mapping = DestinationMapping::new(tmp.path().join("Media.S01E02.mkv").display().to_string(), &destination);
    assert!(subscribe::sidecars::place_mapped_subtitle(&subtitle, &[mapping], Some(library::TransferMode::Copy)).is_err());
    assert!(!destination.with_extension("zh.srt").exists());
}

#[test]
fn ambiguous_episode_versions_refuse_guessed_binding() {
    let tmp = tempfile::tempdir().unwrap();
    let subtitle = tmp.path().join("Media.S01E01.zh.srt");
    std::fs::write(&subtitle, b"E1").unwrap();
    let mappings = ["1080p", "2160p"].map(|quality| DestinationMapping::new(
        tmp.path().join(format!("Media.S01E01.{quality}.mkv")).display().to_string(),
        tmp.path().join(format!("{quality}.mkv")),
    ));
    assert!(subscribe::sidecars::place_mapped_subtitle(&subtitle, &mappings, Some(library::TransferMode::Copy)).is_err());
    assert!(!Path::new(&tmp.path().join("1080p.zh.srt")).exists());
    assert!(!tmp.path().join("2160p.zh.srt").exists());
}

#[test]
fn unrelated_unidentified_subtitle_must_not_overwrite_owned_movie() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("unrelated/Other.Movie.en.srt");
    std::fs::create_dir_all(src.parent().unwrap()).unwrap();
    std::fs::write(&src, b"wrong subtitle").unwrap();
    let owned = tmp.path().join("library/Owned.Movie.mkv");
    std::fs::create_dir_all(owned.parent().unwrap()).unwrap();
    std::fs::write(&owned, b"video").unwrap();
    let sub = owned.with_extension("en.srt");
    std::fs::write(&sub, b"correct subtitle").unwrap();
    let mapping = DestinationMapping::new(tmp.path().join("owned/Owned.Movie.mkv").display().to_string(), &owned);
    let result = subscribe::sidecars::place_mapped_subtitle(&src, &[mapping], Some(library::TransferMode::Copy));
    assert_eq!(std::fs::read(&sub).unwrap(), b"correct subtitle");
    assert!(result.is_err());
}

#[test]
fn move_mode_retry_is_idempotent_when_source_already_moved() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("stage/Movie.2024.zh.srt");
    let dest_video = tmp.path().join("library/Movie.2024.mkv");
    std::fs::create_dir_all(dest_video.parent().unwrap()).unwrap();
    std::fs::write(&dest_video, b"video").unwrap();
    let dest_sub = dest_video.with_extension("zh.srt");
    // Simulate first round: src was moved to dest_sub and deleted
    std::fs::write(&dest_sub, b"subtitle content").unwrap();
    assert!(!src.exists());
    assert!(dest_sub.is_file());

    let mapping = DestinationMapping::new(tmp.path().join("stage/Movie.2024.mkv").display().to_string(), &dest_video);
    let result = subscribe::sidecars::place_mapped_subtitle(&src, &[mapping], Some(library::TransferMode::Move));
    assert!(result.is_ok(), "Move mode retry should succeed when dest exists: {:?}", result);
    assert_eq!(std::fs::read(&dest_sub).unwrap(), b"subtitle content");
}

#[test]
fn move_mode_retry_fails_when_destination_is_empty_file() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("stage/Movie.2024.zh.srt");
    let dest_video = tmp.path().join("library/Movie.2024.mkv");
    std::fs::create_dir_all(dest_video.parent().unwrap()).unwrap();
    std::fs::write(&dest_video, b"video").unwrap();
    let dest_sub = dest_video.with_extension("zh.srt");
    std::fs::write(&dest_sub, b"").unwrap(); // 0 byte file
    assert!(!src.exists());

    let mapping = DestinationMapping::new(tmp.path().join("stage/Movie.2024.mkv").display().to_string(), &dest_video);
    let result = subscribe::sidecars::place_mapped_subtitle(&src, &[mapping], Some(library::TransferMode::Move));
    assert!(result.is_err(), "Move mode retry must not accept 0-byte destination file: {:?}", result);
}

#[test]
fn same_directory_prefix_collision_must_not_bind_unrelated_movie_subtitle() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("stage/Alien.en.srt");
    let video = tmp.path().join("stage/Aliens.mkv");
    let dest_video = tmp.path().join("library/Aliens.mkv");
    std::fs::create_dir_all(dest_video.parent().unwrap()).unwrap();
    std::fs::create_dir_all(src.parent().unwrap()).unwrap();
    let dest_sub = dest_video.with_extension("en.srt");
    std::fs::write(&src, b"Alien subtitle").unwrap();
    std::fs::write(&dest_sub, b"Aliens correct subtitle").unwrap();

    let mapping = DestinationMapping::new(video.display().to_string(), &dest_video);
    let result = subscribe::sidecars::place_mapped_subtitle(&src, &[mapping], Some(library::TransferMode::Copy));
    assert!(result.is_err(), "Unrelated shared prefix movie must be rejected: {:?}", result);
    assert_eq!(std::fs::read(&dest_sub).unwrap(), b"Aliens correct subtitle");
}

#[test]
fn same_directory_different_tv_show_same_episode_must_not_bind() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("stage/Other.Show.S01E01.1080p.zh.srt");
    let video = tmp.path().join("stage/Owned.Show.S01E01.1080p.mkv");
    let dest_video = tmp.path().join("library/Owned.Show.S01E01.mkv");
    std::fs::create_dir_all(dest_video.parent().unwrap()).unwrap();
    std::fs::create_dir_all(src.parent().unwrap()).unwrap();
    let dest_sub = dest_video.with_extension("zh.srt");
    std::fs::write(&src, b"Other show subtitle").unwrap();
    std::fs::write(&dest_sub, b"Owned show correct subtitle").unwrap();

    let mapping = DestinationMapping::new(video.display().to_string(), &dest_video);
    let result = subscribe::sidecars::place_mapped_subtitle(&src, &[mapping], Some(library::TransferMode::Copy));
    assert!(result.is_err(), "Different show with same S01E01 must be rejected: {:?}", result);
    assert_eq!(std::fs::read(&dest_sub).unwrap(), b"Owned show correct subtitle");
}

#[test]
fn tv_prefix_collision_preserves_owned_subtitle_in_both_directions() {
    for (subtitle_title, video_title) in [("Alien", "Aliens"), ("Aliens", "Alien")] {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join(format!("{subtitle_title}.S01E01.en.srt"));
        let video = tmp.path().join(format!("{video_title}.S01E01.mkv"));
        let destination = tmp.path().join("library/owned.mkv");
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        let subtitle = destination.with_extension("en.srt");
        std::fs::write(&src, b"wrong subtitle").unwrap();
        std::fs::write(&subtitle, b"correct subtitle").unwrap();
        let mapping = DestinationMapping::new(video.display().to_string(), destination);
        let result = subscribe::sidecars::place_mapped_subtitle(
            &src, &[mapping], Some(library::TransferMode::Copy),
        );
        assert_eq!(std::fs::read(subtitle).unwrap(), b"correct subtitle");
        assert!(result.is_err());
    }
}

#[test]
fn season_pack_without_explicit_episodes_matches_season_pack_subscription() {
    let rel = release::parse("The.Expanse.S01.1080p.BluRay");
    let mut sub = domain::Subscribe {
        id: domain::SubscribeId::new(),
        user_id: domain::UserId::new(),
        media_id: domain::MediaId::new(),
        coverage: domain::Coverage::Tv { season: 1, episode_from: 1, episode_to: Some(10) },
        fetch_mode: domain::FetchMode::Search,
        filter_id: domain::FilterId::new(),
        wash_cut: false,
        keep_old_versions: false,
        wash_cut_filter_id: None,
        full_season_pack: true,
        downloader_id: None,
        library_id: None,
        tracking_state: "active".into(),
        follow_future: false,
        search_interval_secs: 1800,
    };
    let media = domain::Media {
        id: sub.media_id,
        kind: domain::MediaKind::Tv,
        title: "The Expanse".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    assert!(subscribe::candidate_matches_subscribe(&sub, &media, &rel), "S01 pack must match full_season_pack TV coverage");

    sub.full_season_pack = false;
    assert!(!subscribe::candidate_matches_subscribe(&sub, &media, &rel), "S01 pack without full_season_pack should not match regular episode subscription");
}
