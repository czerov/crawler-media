use domain::{Media, MediaId, MediaKind};
use library::{NfoMeta, read_nfo, write_nfo};
use tempfile::tempdir;

#[test]
fn write_and_read_episode_nfo_with_plot() {
    let tmp = tempdir().unwrap();
    let nfo_path = tmp.path().join("一瓯春 - S01E05 - 第 5 集.nfo");

    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "一瓯春".into(),
        year: Some(2026),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: Some("294990".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };

    let meta = NfoMeta {
        title: Some("春日宴？修罗场！".into()),
        plot: Some("严瑞夜闯谢府救下受刑的清圆，当众羞辱荣嬷嬷后强行带其离开。".into()),
        season: Some(1),
        episode: Some(5),
        thumb: Some("https://image.tmdb.org/t/p/w300/9SeMJ0eDUIKROuT6lH7sXhMMjjJ.jpg".into()),
        ..Default::default()
    };

    // 写入标准单集 NFO
    write_nfo(&nfo_path, &media, Some(&meta)).unwrap();

    let text = std::fs::read_to_string(&nfo_path).unwrap();
    assert!(text.contains("<episodedetails>"));
    assert!(text.contains("<title>春日宴？修罗场！</title>"));
    assert!(text.contains("<season>1</season>"));
    assert!(text.contains("<episode>5</episode>"));
    assert!(
        text.contains("<plot>严瑞夜闯谢府救下受刑的清圆，当众羞辱荣嬷嬷后强行带其离开。</plot>")
    );

    // 验证反向读取解析
    let parsed = read_nfo(&nfo_path).expect("NFO must be readable");
    assert_eq!(parsed.title.as_deref(), Some("春日宴？修罗场！"));
    assert_eq!(parsed.season, Some(1));
    assert_eq!(parsed.episode, Some(5));
    assert_eq!(
        parsed.plot.as_deref(),
        Some("严瑞夜闯谢府救下受刑的清圆，当众羞辱荣嬷嬷后强行带其离开。")
    );
}
