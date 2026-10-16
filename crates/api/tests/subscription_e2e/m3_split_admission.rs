//! M3: with wash-cut on, missing episodes are admitted by the main Filter while
//! owned episodes are only upgraded by the wash Filter.

use serde_json::json;

use super::fake_catalog::tv_subscribe;
use super::harness::*;

const E01_1080: &str = "The.Long.Watch.S01E01.1080p.WEB-DL.x264-GRP";
const E01_2160: &str = "The.Long.Watch.S01E01.2160p.WEB-DL.x265-GRP";
const E03_1080: &str = "The.Long.Watch.S01E03.1080p.WEB-DL.x264-GRP";
const E02_720: &str = "The.Long.Watch.S01E02.720p.WEB-DL.x264-GRP";

#[tokio::test]
async fn missing_episode_uses_main_filter_owned_episode_uses_wash_filter() {
    let h = Harness::new();
    h.create_site().await;
    let atom = |rule, priority| domain::FilterAtom {
        priority,
        rule,
        exclude: false,
    };
    let main = domain::Filter::new(
        domain::FilterId::new(),
        "main-1080p",
        vec![atom(domain::AtomRule::Resolution("1080p".into()), 100)],
    );
    let wash = domain::Filter::new(
        domain::FilterId::new(),
        "wash-2160p",
        vec![atom(domain::AtomRule::Resolution("2160p".into()), 200)],
    );
    h.state.store().lock().insert_filter(&main).unwrap();
    h.state.store().lock().insert_filter(&wash).unwrap();
    let window = json!({ "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 3 });
    let id = tv_subscribe(&h, window, json!({ "filter_id": main.id.to_string() })).await;

    // Own E01 in 1080p first (wash off).
    let video = probed_video(1080, "h264", false);
    h.set_search_rows(&[(E01_1080, 1)]);
    h.scheduled_search(id).await.unwrap();
    h.qb.complete(0, E01_1080, &[(&format!("{E01_1080}.mkv"), &video)]);
    h.transfer(id).await.unwrap();

    // Turn wash-cut on with the 2160p-only wash Filter.
    {
        let store = h.state.store();
        let store = store.lock();
        let mut sub = store.get_subscribe(id).unwrap().unwrap();
        sub.wash_cut = true;
        sub.wash_cut_filter_id = Some(wash.id);
        store.update_subscribe(&sub).unwrap();
    }
    h.set_search_rows(&[(E01_2160, 2), (E03_1080, 3), (E02_720, 4)]);
    h.scheduled_search(id).await.unwrap();
    let mut titles: Vec<_> = h
        .pending(id)
        .into_iter()
        .map(|(_, p)| p.torrent.title)
        .collect();
    titles.sort();
    assert_eq!(
        titles,
        vec![E01_2160.to_string(), E03_1080.to_string()],
        "E01 upgrade via wash filter, E03 fill via main filter, E02 720p rejected by both"
    );

    // Both land: E01 replaced by the 2160p upgrade, E03 filled in 1080p.
    let uhd = probed_video(2160, "hevc", false);
    let adds = h.qb.adds.lock().clone();
    let idx = |needle: &str| adds.iter().position(|a| a.contains(needle)).unwrap();
    h.qb.complete(idx("id=2"), E01_2160, &[(&format!("{E01_2160}.mkv"), &uhd)]);
    h.qb.complete(
        idx("id=3"),
        E03_1080,
        &[(&format!("{E03_1080}.mkv"), &video)],
    );
    h.transfer(id).await.unwrap();
    let facts = h.state.store().lock().load_subscribe_facts(id).unwrap();
    let e01 = facts
        .get(Some(1), Some(1))
        .and_then(|f| f.path.clone())
        .unwrap();
    let e03 = facts.get(Some(1), Some(3)).and_then(|f| f.path.clone());
    assert!(std::path::Path::new(&e01).exists());
    assert_eq!(
        facts
            .quality(&e01)
            .and_then(|q| q.resolution.clone())
            .as_deref(),
        Some("2160p")
    );
    assert!(
        e03.is_some_and(|p| std::path::Path::new(&p).exists()),
        "E03 imported"
    );
    let e01_files = h
        .library_files()
        .into_iter()
        .filter(|p| p.to_string_lossy().contains("E01"))
        .count();
    assert_eq!(e01_files, 1, "old 1080p E01 replaced");
}
