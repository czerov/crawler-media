#[path = "support/mod.rs"]
mod support;

use support::library_presence::PresenceFixture;

#[tokio::test]
async fn job_updates_shared_subscriber_bounded() {
    let fix = PresenceFixture::new();

    let rx1 = fix.state.job_updates.subscribe(fix.state.jobs.clone()).await.unwrap();
    let rx2 = fix.state.job_updates.subscribe(fix.state.jobs.clone()).await.unwrap();

    assert_eq!(fix.state.job_updates.subscriber_count(), 2);
    assert_eq!(rx1.borrow().count, 0);
    assert_eq!(rx2.borrow().count, 0);
}
