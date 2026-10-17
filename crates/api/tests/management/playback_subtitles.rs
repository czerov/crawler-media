use super::*;

fn movie_with_subtitle_tracks(tmp: &tempfile::TempDir) -> (domain::Media, domain::LedgerRow) {
    let movie_file = tmp.path().join("library/movies/TestFilm.mkv");
    let srt_file = tmp.path().join("library/movies/TestFilm.zh.srt");
    std::fs::create_dir_all(movie_file.parent().unwrap()).unwrap();
    std::fs::write(&movie_file, b"video-data").unwrap();
    std::fs::write(
        &srt_file,
        b"1\n00:00:01,000 --> 00:00:02,000\nHello Subtitle",
    )
    .unwrap();

    let store = api::Store::open(tmp.path().join("data")).unwrap();
    let media = store
        .ensure_media(domain::Media {
            id: domain::MediaId::new(),
            kind: domain::MediaKind::Movie,
            title: "TestFilm".into(),
            year: Some(2025),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: Some("77777".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();

    let row = domain::LedgerRow {
        id: domain::LedgerId::new(),
        media_id: media.id,
        path: movie_file.display().to_string(),
        season: None,
        episode: None,
        resolution: Some("1080p".into()),
        codec: Some("h264".into()),
        hdr: None,
        quality_source: domain::QualitySource::Release,
        confidence: domain::Confidence::High,
        filter_score: None,
    };
    store.insert_ledger(&row).unwrap();

    // 写入该文件的探测轨道数据（包含音轨与外部字幕）
    store
        .put_file_meta(
            &row.id.to_string(),
            &library::Tracks {
                video: Some(library::VideoTrack {
                    stream_index: Some(0),
                    codec: Some("h264".into()),
                    ..Default::default()
                }),
                audio: vec![library::AudioTrack {
                    stream_index: Some(1),
                    codec: Some("aac".into()),
                    profile: None,
                    language: Some("chi".into()),
                    title: Some("Chinese Audio".into()),
                    channels: Some(2),
                    bit_rate: None,
                    is_default: true,
                    ..Default::default()
                }],
                subtitles: vec![library::SubtitleTrack {
                    stream_index: Some(2),
                    codec: Some("subrip".into()),
                    profile: None,
                    language: Some("chi".into()),
                    title: Some("Chinese Subtitle".into()),
                    bit_rate: None,
                    is_default: true,
                    forced: false,
                    is_external: true,
                    path: Some(srt_file.display().to_string()),
                }],
            },
        )
        .unwrap();
    (media, row)
}

#[tokio::test]
async fn playback_decide_and_subtitle_delivery_returns_tracks_and_content() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);

    let (media, row) = movie_with_subtitle_tracks(&tmp);

    // 1. 调用 POST /api/v1/playback/decide
    let decide_res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/playback/decide",
            Some("management-secret"),
            json!({
                "media_item_id": media.id.to_string(),
                "file_id": row.id.to_string(),
            }),
        ))
        .await
        .unwrap();
    assert_eq!(decide_res.status(), StatusCode::OK);
    let decision = json_data(decide_res).await;

    // 验证 audio_tracks 和 subtitles 不为空 (G07)
    let audio_tracks = decision["audio_tracks"].as_array().unwrap();
    let subtitles = decision["subtitles"].as_array().unwrap();
    assert_eq!(audio_tracks.len(), 1);
    assert_eq!(audio_tracks[0]["language"], "chi");
    assert_eq!(subtitles.len(), 1);
    assert_eq!(subtitles[0]["language"], "chi");
    let delivery_url = subtitles[0]["delivery_url"].as_str().unwrap();
    assert!(
        delivery_url.contains(&row.id.to_string()),
        "字幕对象必须包含正确的交付 URL"
    );

    // 2. 调用交付 URL 获取字幕文件内容
    let sub_res = app
        .clone()
        .oneshot(request(
            "GET",
            delivery_url,
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(sub_res.status(), StatusCode::OK);
    let body_bytes = axum::body::to_bytes(sub_res.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8(body_bytes.to_vec()).unwrap();
    assert!(
        text.contains("Hello Subtitle"),
        "必须成功返回字幕文件内容 (G07)"
    );

    // 3. 验证内封字幕轨交付与缓存命中 (无 external 路径时走内封提取或缓存)
    // cache 维度包含 source_version：预置与 service 相同的版本化布局。
    let source_version =
        api::fingerprint_job::current_source_version(std::path::Path::new(&row.path));
    let cache_dir = api::subtitle_delivery::subtitle_cache_dir(row.id, &source_version);
    std::fs::create_dir_all(&cache_dir).unwrap();
    std::fs::write(
        cache_dir.join("sub_3.vtt"),
        b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nEmbedded Subtitle Cached",
    )
    .unwrap();

    let store = api::Store::open(tmp.path().join("data")).unwrap();
    store
        .put_file_meta(
            &row.id.to_string(),
            &library::Tracks {
                video: Some(library::VideoTrack {
                    stream_index: Some(0),
                    codec: Some("h264".into()),
                    ..Default::default()
                }),
                audio: vec![],
                subtitles: vec![library::SubtitleTrack {
                    stream_index: Some(3),
                    codec: Some("subrip".into()),
                    profile: None,
                    language: Some("eng".into()),
                    title: Some("English Subtitle".into()),
                    bit_rate: None,
                    is_default: false,
                    forced: false,
                    is_external: false,
                    path: None,
                }],
            },
        )
        .unwrap();
    drop(store);

    let embedded_sub_url = format!("/api/v1/playback/subtitles/{}/3?format=vtt", row.id);
    let embedded_res = app
        .clone()
        .oneshot(request(
            "GET",
            &embedded_sub_url,
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(embedded_res.status(), StatusCode::OK);
    let body_bytes = axum::body::to_bytes(embedded_res.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8(body_bytes.to_vec()).unwrap();
    assert!(
        text.contains("Embedded Subtitle Cached"),
        "必须成功交付内封字幕 (G07)"
    );
}
