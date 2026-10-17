use super::*;

async fn get_json(app: &axum::Router, uri: &str) -> Value {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[tokio::test]
async fn tvshow_nfo_metadata_reaches_series_and_flat_episode_details() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let library_root = tmp.path().join("library/tv");
    let show_root = library_root.join("Pantheon");
    std::fs::create_dir_all(&show_root).unwrap();
    store
        .create_library(
            MediaKind::Tv,
            "Shows",
            &[library_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();

    let show = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "Pantheon".into(),
        year: Some(2022),
        original_title: Some("Pantheon".into()),
        aliases: Vec::new(),
        tmdb_id: Some("195339".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&show).unwrap();
    let episode_path = show_root.join("Pantheon S01E01.mkv");
    std::fs::write(&episode_path, b"episode").unwrap();
    let episode = LedgerRow {
        id: LedgerId::new(),
        media_id: show.id,
        path: episode_path.display().to_string(),
        season: Some(1),
        episode: Some(1),
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: None,
    };
    store.insert_ledger(&episode).unwrap();

    std::fs::write(
        show_root.join("tvshow.nfo"),
        r#"<tvshow>
  <title>Pantheon</title><year>2022</year>
  <premiered>2022-09-01</premiered><enddate>2025-02-21</enddate>
  <status>Ended</status><mpaa>TV-14</mpaa>
  <ratings><rating name="themoviedb" max="10" default="true"><value>8.5</value><votes>1200</votes></rating></ratings>
  <plot>Uploaded intelligence changes humanity.</plot>
  <tagline>What makes us human?</tagline>
  <genre>Animation</genre><genre>Science Fiction</genre>
  <studio>Netflix</studio><country>US</country><original_language>en</original_language>
  <director>Craig Silverstein</director>
  <actor><name>Paul Dano</name><role>Caspian</role><tmdbid>6384</tmdbid><thumb>https://image.example/paul.jpg</thumb><order>0</order></actor>
</tvshow>"#,
    )
    .unwrap();
    std::fs::write(
        episode_path.with_extension("nfo"),
        "<episodedetails><title>Zero Day</title><aired>2022-09-01</aired><season>1</season><episode>1</episode></episodedetails>",
    )
    .unwrap();

    let series_id = show.id.to_string().replace('-', "");
    let series = get_json(&app, &format!("/Items/{series_id}")).await;
    assert_eq!(series["ProductionYear"], 2022);
    assert_eq!(series["PremiereDate"], "2022-09-01");
    assert_eq!(series["EndDate"], "2025-02-21");
    assert_eq!(series["Status"], "Ended");
    assert_eq!(series["OfficialRating"], "TV-14");
    assert_eq!(series["CommunityRating"], 8.5);
    assert_eq!(series["VoteCount"], 1200);
    assert_eq!(series["Taglines"][0], "What makes us human?");
    assert_eq!(series["OriginalLanguage"], "en");
    assert_eq!(series["Studios"][0]["Name"], "Netflix");
    assert_eq!(series["ProductionLocations"][0], "US");
    assert_eq!(series["Genres"], json!(["Animation", "Science Fiction"]));
    assert_eq!(
        series["Overview"],
        "Uploaded intelligence changes humanity."
    );
    assert_eq!(series["People"][0]["Name"], "Paul Dano");

    let episode_id = episode.id.to_string().replace('-', "");
    let episode = get_json(&app, &format!("/Items/{episode_id}")).await;
    assert_eq!(episode["Name"], "Zero Day");
    assert_eq!(episode["PremiereDate"], "2022-09-01");
    assert_eq!(episode["OfficialRating"], "TV-14");
    assert_eq!(episode["Studios"][0]["Name"], "Netflix");
    assert_eq!(episode["People"][0]["Name"], "Paul Dano");
}
