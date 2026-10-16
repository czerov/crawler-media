//! L5: a rejected candidate with an absurd episode number does not stretch
//! the wanted units of an open window.

use serde_json::json;

use super::fake_catalog::tv_subscribe;
use super::harness::*;

const E01: &str = "The.Long.Watch.S01E01.1080p.WEB-DL.x264-GRP";
const E999: &str = "The.Long.Watch.S01E999.1080p.WEB-DL.x264-REJECTME";

#[tokio::test]
async fn rejected_e999_does_not_extend_wanted_units() {
    let h = Harness::new();
    h.create_site().await;
    let filter = domain::Filter::new(
        domain::FilterId::new(),
        "no-rejectme",
        vec![
            domain::FilterAtom {
                priority: 100,
                rule: domain::AtomRule::Resolution("1080p".into()),
                exclude: false,
            },
            domain::FilterAtom {
                priority: 100,
                rule: domain::AtomRule::TitleMatch("REJECTME".into()),
                exclude: true,
            },
        ],
    );
    h.state.store().lock().insert_filter(&filter).unwrap();
    let window = json!({ "kind": "tv", "season": 1, "episode_from": 1, "episode_to": null });
    let extra = json!({ "filter_id": filter.id.to_string(), "follow_future": true });
    let id = tv_subscribe(&h, window, extra).await;
    h.set_search_rows(&[(E01, 1), (E999, 2)]);
    h.scheduled_search(id).await.unwrap();
    let history = h.state.store().lock().load_wanted_history(id).unwrap();
    let highest = history.keys().filter_map(|(_, e)| *e).max().unwrap_or(0);
    assert!(highest <= 1 + 3, "wanted units stretched to E{highest}");
}
