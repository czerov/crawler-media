#[path = "support/mod.rs"]
mod support;

use support::library_presence::PresenceFixture;

#[tokio::test]
async fn search_and_fs_budgets_are_bounded() {
    let fix = PresenceFixture::new();

    assert_eq!(fix.state.search_work.available_permits(), 4);
    assert_eq!(fix.state.fs_work.available_permits(), 2);
    assert_eq!(fix.state.http_work.available_permits(), 8);
}
