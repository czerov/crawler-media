//! H4: an in-flight pure season pack must placehold every episode of the
//! subscribed window on both the manual-run and the scheduled path.

use domain::SubscribeId;
use serde_json::json;

use super::harness::*;

const PACK: &str = "The.Long.Watch.S01.1080p.WEB-DL.x264-GRP";

async fn tv_subscribe(h: &Harness, full_season_pack: bool) -> SubscribeId {
    let response = tower::ServiceExt::oneshot(
        h.app.clone(),
        super::common::request(
            "POST",
            "/api/v1/subscriptions",
            Some(TOKEN),
            json!({
                "media": { "kind": "tv", "title": "The Long Watch" },
                "coverage": { "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 3 },
                "fetch_mode": "search",
                "filter": {
                    "name": "long-watch-1080p",
                    "atoms": [
                        { "kind": "resolution", "value": "1080p", "priority": 100 },
                        { "kind": "title_match", "value": "Long", "priority": 80 }
                    ]
                },
                "wash_cut": false,
                "full_season_pack": full_season_pack
            }),
        ),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::CREATED);
    super::common::json_data(response).await["id"].as_str().unwrap().parse().unwrap()
}

async fn pack_in_flight_blocks_more_downloads(full_season_pack: bool) {
    let h = Harness::new();
    h.create_site().await;
    let id = tv_subscribe(&h, full_season_pack).await;
    h.set_search_rows(&[(PACK, 1)]);
    if full_season_pack {
        h.run_subscribe(id).await;
    } else {
        // Without full_season_pack the chooser never picks a pure pack; the
        // user admits it by hand from search results.
        let response = tower::ServiceExt::oneshot(
            h.app.clone(),
            super::common::request(
                "POST",
                "/api/v1/search/admit",
                Some(TOKEN),
                json!({ "subscribe_id": id.to_string(), "enclosure": enclosure(1) }),
            ),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
    }
    assert_eq!(h.qb.add_count(), 1, "season pack submitted (fsp={full_season_pack})");
    h.qb.set_progress(0, 0.2);

    h.set_search_rows(&[
        ("The.Long.Watch.S01.1080p.WEB-DL.x264-OTHER", 2),
        ("The.Long.Watch.S01E02.1080p.WEB-DL.x264-GRP", 3),
    ]);
    h.run_subscribe(id).await;
    assert_eq!(h.qb.add_count(), 1, "manual run: pack in flight covers E01..E03 (fsp={full_season_pack})");
    h.scheduled_search(id).await.unwrap();
    assert_eq!(h.qb.add_count(), 1, "scheduled: pack in flight covers E01..E03 (fsp={full_season_pack})");
}

#[tokio::test]
async fn season_pack_pending_blocks_with_full_season_pack() {
    pack_in_flight_blocks_more_downloads(true).await;
}

#[tokio::test]
async fn season_pack_pending_blocks_without_full_season_pack() {
    pack_in_flight_blocks_more_downloads(false).await;
}
