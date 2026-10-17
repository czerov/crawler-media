use domain::{Media, MediaId, MediaKind};
use library::{NfoMeta, parse_nfo, read_nfo, write_nfo};
use tempfile::tempdir;

/// 标题/剧情含 `& < > " '` 时，写出的 NFO 必须是合法 XML，读回必须还原。
#[test]
fn nfo_escapes_xml_special_chars_on_round_trip() {
    let tmp = tempdir().unwrap();
    let nfo_path = tmp.path().join("Law & Order.nfo");

    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "Law & Order <Special> \"Quotes\" 'Single'".into(),
        year: Some(1990),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: Some("1234".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };

    let meta = NfoMeta {
        plot: Some("A & B <C> \"D\" 'E'".into()),
        rating: Some("8.1".into()),
        runtime_minutes: Some("45".into()),
        genres: vec!["剧情 & 犯罪".into()],
        cast: vec![library::CastMember {
            name: "A&B <C>".into(),
            role: Some("R&D".into()),
            tmdb_id: Some("42".into()),
            thumb: Some("https://img.example.com/a&b.jpg".into()),
            order: None,
        }],
        season: Some(1),
        episode: Some(5),
        thumb: Some("https://img.example.com/x?a=1&b=2".into()),
        ..Default::default()
    };

    write_nfo(&nfo_path, &media, Some(&meta)).unwrap();

    // 写出的文件必须是合法 XML：特殊字符已被实体化，不能出现裸 &<。
    let text = std::fs::read_to_string(&nfo_path).unwrap();
    assert!(!text.contains("<title>Law & Order <Special>"));
    assert!(text.contains("Law &amp; Order &lt;Special&gt;"));
    assert!(text.contains("A &amp; B &lt;C&gt;"));
    // thumb URL 的 & 参数必须转义
    assert!(text.contains("a=1&amp;b=2"));
    assert!(text.contains("<episodedetails>"));

    // 读回必须还原原始字符
    let parsed = read_nfo(&nfo_path).expect("read");
    assert_eq!(
        parsed.title.as_deref(),
        Some("Law & Order <Special> \"Quotes\" 'Single'")
    );
    assert_eq!(parsed.plot.as_deref(), Some("A & B <C> \"D\" 'E'"));
    assert_eq!(parsed.genres, vec!["剧情 & 犯罪"]);
    assert_eq!(parsed.cast[0].name, "A&B <C>");
    assert_eq!(parsed.cast[0].role.as_deref(), Some("R&D"));
    assert_eq!(
        parsed.thumb.as_deref(),
        Some("https://img.example.com/x?a=1&b=2")
    );
    assert_eq!(parsed.season, Some(1));
    assert_eq!(parsed.episode, Some(5));
}

/// MoviePilot / TMM 生成的 CDATA plot 也应能被读取（反转义对 CDATA 不破坏）。
#[test]
fn nfo_parse_handles_cdata_and_entities() {
    let body = r#"<?xml version="1.0" encoding="utf-8" standalone="yes"?>
<tvshow>
  <title>一瓯春</title>
  <plot><![CDATA[“杀伐千面” & “黑莲花” <对决>]]></plot>
  <uniqueid type="tmdb">294990</uniqueid>
</tvshow>"#;
    let parsed = parse_nfo(body).expect("parse");
    assert_eq!(parsed.title.as_deref(), Some("一瓯春"));
    // CDATA 内容原样保留（不经过实体反转义）
    assert_eq!(parsed.plot.as_deref(), Some("“杀伐千面” & “黑莲花” <对决>"));
}
