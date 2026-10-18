//! M1: an episode range pack and a higher-scoring single episode inside it
//! must not both be downloaded; the overlap policy decides which one wins.

use serde_json::json;

use super::fake_catalog::tv_subscribe;
use super::harness::*;

const PACK: &str = "The.Long.Watch.S01E01-E10.1080p.WEB-DL.x264-GRP";
const SINGLE: &str = "The.Long.Watch.S01E06.1080p.WEB-DL.x265-GRP";

async fn run(policy: Option<&str>) -> Vec<String> {
    let h = Harness::new();
    if let Some(policy) = policy {
        h.state
            .store()
            .lock()
            .put_setting(api::settings_keys::SUBSCRIBE_OVERLAP_POLICY, policy)
            .unwrap();
    }
    h.create_site().await;
    let window = json!({ "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 10 });
    let atom = |rule, priority| domain::FilterAtom {
        priority,
        rule,
        exclude: false,
    };
    let filter = domain::Filter::new(
        domain::FilterId::new(),
        "long-watch-x265",
        vec![
            atom(domain::AtomRule::Resolution("1080p".into()), 100),
            atom(domain::AtomRule::TitleMatch("x265".into()), 150),
        ],
    );
    h.state.store().lock().insert_filter(&filter).unwrap();
    let filter = json!({ "filter_id": filter.id.to_string() });
    let id = tv_subscribe(&h, window, filter).await;
    h.set_search_rows(&[(PACK, 1), (SINGLE, 2)]);
    h.scheduled_search(id).await.unwrap();
    h.pending(id)
        .into_iter()
        .map(|(_, p)| p.torrent.title)
        .collect()
}

#[tokio::test]
async fn default_prefer_pack_submits_only_the_pack() {
    assert_eq!(run(None).await, vec![PACK.to_string()]);
}

#[tokio::test]
async fn prefer_episode_submits_only_the_single() {
    assert_eq!(run(Some("prefer_episode")).await, vec![SINGLE.to_string()]);
}
