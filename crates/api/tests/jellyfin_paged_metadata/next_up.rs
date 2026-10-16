//! NextUp：一次聚合后的整体分页。

use super::*;

#[tokio::test]
async fn next_up_aggregates_one_media_across_libraries() {
    let dir = tempfile::tempdir().unwrap();
    let files = Arc::new(CountingFiles::default());
    let state = new_state(dir.path(), files);
    let root_one = dir.path().join("next-up-one");
    let root_two = dir.path().join("next-up-two");
    std::fs::create_dir_all(&root_one).unwrap();
    std::fs::create_dir_all(&root_two).unwrap();
    let first_episode = {
        let store = state.store();
        let store = store.lock();
        store
            .create_library(
                MediaKind::Tv,
                "Next up one",
                &[root_one.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        store
            .create_library(
                MediaKind::Tv,
                "Next up two",
                &[root_two.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let media_id = new_media(&store, "Cross library show");
        let first = insert_episode(&store, &root_one, media_id, 1, 1);
        insert_episode(&store, &root_two, media_id, 1, 2);
        first
    };
    let app = router(state);
    let next_up = get_json(&app, "/Shows/NextUp").await;
    assert_eq!(next_up["TotalRecordCount"], 1, "{next_up}");
    assert_eq!(
        items(&next_up)[0]["Id"],
        first_episode.to_string().replace('-', ""),
        "同一 Media 只出一条，取 (季, 集) 最小的未看"
    );
}

/// NextUp 分页：`StartIndex` 在整体聚合之后生效。
#[tokio::test]
async fn next_up_pages_after_aggregating_every_series() {
    let fixture = tv_fixture(5, 2);
    let app = router(fixture.state.clone());
    let all = get_json(&app, "/Shows/NextUp").await;
    assert_eq!(all["TotalRecordCount"], 5);
    assert_eq!(items(&all).len(), 5);
    let paged = get_json(&app, "/Shows/NextUp?StartIndex=4&Limit=1").await;
    assert_eq!(paged["TotalRecordCount"], 5, "total 是聚合后的完整条数");
    assert_eq!(items(&paged).len(), 1);
    assert_eq!(items(&paged)[0]["Id"], items(&all)[4]["Id"]);
    assert_eq!(
        fixture.files.subtitles(),
        6,
        "一次聚合 + 只 enrich 本页：5 条全量 + 1 条分页，而不是每个 series 各重扫一次"
    );
    let beyond = get_json(&app, "/Shows/NextUp?StartIndex=99").await;
    assert_eq!(items(&beyond).len(), 0);
    assert_eq!(beyond["TotalRecordCount"], 5);
}

