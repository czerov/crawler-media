//! Media 身份查询索引的正确性回归（DB6）。
//!
//! 索引只应改变查询计划，不得改变身份语义：Movie/TV 相同数字别名不合并、
//! `year IS NULL` 的查找规则、以及别名白名单行为都必须逐字保持。
//! 索引实测证据见 docs/audits/2026-10-12-remediation-review-repro/media-lookup-plans.md。

use domain::{Media, MediaId, MediaKind};
use store::Store;

fn media(kind: MediaKind, title: &str, year: Option<u16>, tmdb: Option<&str>) -> Media {
    Media {
        id: MediaId::new(),
        kind,
        title: title.into(),
        year,
        original_title: Some(title.into()),
        tmdb_id: tmdb.map(str::to_string),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

#[test]
fn same_numeric_alias_across_kinds_stays_separate() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let movie = media(MediaKind::Movie, "The Matrix", Some(1999), Some("603"));
    let tv = media(MediaKind::Tv, "The Matrix", Some(1999), Some("603"));
    store.insert_media(&movie).unwrap();
    store.insert_media(&tv).unwrap();

    let found_movie = store
        .get_media_by_alias_kind("tmdb_id", "603", Some(MediaKind::Movie))
        .unwrap()
        .expect("movie by alias+kind");
    let found_tv = store
        .get_media_by_alias_kind("tmdb_id", "603", Some(MediaKind::Tv))
        .unwrap()
        .expect("tv by alias+kind");
    assert_eq!(found_movie.id, movie.id);
    assert_eq!(found_tv.id, tv.id);
    assert_ne!(found_movie.id, found_tv.id, "数字别名相同不得合并成同一 Media");

    // 不带 kind 的旧查找仍能命中其中之一，且两个 Media 都还在。
    let any = store.get_media_by_alias("tmdb_id", "603").unwrap();
    assert!(any.is_some());
    assert!(store.get_media(movie.id).unwrap().is_some());
    assert!(store.get_media(tv.id).unwrap().is_some());
}

#[test]
fn unknown_year_prefers_the_null_year_row() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let known = media(MediaKind::Movie, "Same Title", Some(2020), Some("1"));
    let unknown = media(MediaKind::Movie, "Same Title", None, Some("2"));
    store.insert_media(&known).unwrap();
    store.insert_media(&unknown).unwrap();

    let found = store
        .get_media_by_title_kind_year("Same Title", MediaKind::Movie, None)
        .unwrap()
        .expect("year 为 None 时应优先命中同样无年份的行");
    assert_eq!(found.id, unknown.id);
}

#[test]
fn unknown_year_falls_back_only_when_the_known_year_candidate_is_unique() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let only = media(MediaKind::Movie, "Only One", Some(2001), Some("10"));
    store.insert_media(&only).unwrap();
    let found = store
        .get_media_by_title_kind_year("Only One", MediaKind::Movie, None)
        .unwrap()
        .expect("唯一已知年份候选时应挂上");
    assert_eq!(found.id, only.id);

    // 第二个同标题不同年份出现后：无法区分，必须返回 None 而不是任取一个。
    let second = media(MediaKind::Movie, "Only One", Some(2002), Some("11"));
    store.insert_media(&second).unwrap();
    assert!(
        store
            .get_media_by_title_kind_year("Only One", MediaKind::Movie, None)
            .unwrap()
            .is_none(),
        "同标题多个年份候选时必须返回 None"
    );
}

#[test]
fn exact_year_still_matches_the_requested_year() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let a = media(MediaKind::Movie, "Dune", Some(1984), Some("20"));
    let b = media(MediaKind::Movie, "Dune", Some(2021), Some("21"));
    store.insert_media(&a).unwrap();
    store.insert_media(&b).unwrap();

    assert_eq!(
        store
            .get_media_by_title_kind_year("Dune", MediaKind::Movie, Some(2021))
            .unwrap()
            .unwrap()
            .id,
        b.id
    );
    assert_eq!(
        store
            .get_media_by_title_kind_year("Dune", MediaKind::Movie, Some(1984))
            .unwrap()
            .unwrap()
            .id,
        a.id
    );
}

#[test]
fn alias_whitelist_still_rejects_unknown_columns() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let inserted = media(MediaKind::Movie, "X", Some(2000), Some("30"));
    store.insert_media(&inserted).unwrap();

    assert!(
        store
            .get_media_by_alias_kind("title", "X", Some(MediaKind::Movie))
            .unwrap()
            .is_none(),
        "非别名列必须被白名单拒绝"
    );
    assert!(
        store
            .get_media_by_alias("1=1; DROP TABLE media", "30")
            .unwrap()
            .is_none(),
        "未知列名不得进入 SQL"
    );
    // 表还在，说明上面的调用没有拼接出可执行 SQL。
    assert!(store.get_media(inserted.id).unwrap().is_some());
}
