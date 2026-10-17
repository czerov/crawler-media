use domain::{Media, MediaId, MediaKind};
use media::{Source, attach, details, merge};

fn blank(title: &str) -> Media {
    Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: title.into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

#[test]
fn each_source_can_attach_an_alias() {
    let media = blank("The Matrix");
    let media = attach(media, Source::Douban, "1291843");
    let media = attach(media, Source::Tvdb, "169");
    let media = attach(media, Source::Bangumi, "123");
    let media = attach(media, Source::Anilist, "999");
    assert_eq!(media.douban_id.as_deref(), Some("1291843"));
    assert_eq!(media.tvdb_id.as_deref(), Some("169"));
    assert_eq!(media.bangumi_id.as_deref(), Some("123"));
    assert_eq!(media.anilist_id.as_deref(), Some("999"));
}

#[test]
fn merge_keeps_one_internal_id() {
    let mut left = blank("The Matrix");
    left.tmdb_id = Some("603".into());
    let id = left.id;
    let mut right = blank("黑客帝国");
    right.douban_id = Some("1291843".into());
    let merged = merge(left, right);
    assert_eq!(merged.id, id);
    assert_eq!(merged.tmdb_id.as_deref(), Some("603"));
    assert_eq!(merged.douban_id.as_deref(), Some("1291843"));
}

#[test]
fn details_keep_alternative_titles_in_source_order() {
    let body = r#"{
        "id": 1,
        "name": "醉梦",
        "alternative_titles": {"results": [
            {"title": "Dream"},
            {"title": "Chinese Dream"},
            {"title": "醉梦"},
            {"title": "国庆特别节目"}
        ]},
        "translations": {"translations": [
            {"iso_639_1": "zh", "data": {"name": "Chinese Dream"}}
        ]}
    }"#;
    let media = details(MediaKind::Tv, body).unwrap();
    assert_eq!(media.title, "醉梦");
    assert_eq!(
        media.aliases,
        vec![
            "Dream".to_string(),
            "Chinese Dream".to_string(),
            "国庆特别节目".to_string(),
        ]
    );
}
