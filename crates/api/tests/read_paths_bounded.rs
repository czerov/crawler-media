#[path = "support/mod.rs"]
mod support;

use domain::UserId;
use support::library_presence::PresenceFixture;

#[tokio::test]
async fn visible_scope_loads_strictly_owned_media_ids() {
    let fix = PresenceFixture::new();

    let (m1, _) = fix.insert_media_and_ledger("vis1.mkv");
    let (m2, _) = fix.insert_media_and_ledger("vis2.mkv");

    let admin_id = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard.user_by_token(&fix.admin_token).unwrap().unwrap().id
    };

    let scope = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        api::visible_scope::VisibleScope::load(&guard, admin_id).unwrap()
    };

    assert!(scope.media_ids().contains(&m1));
    assert!(scope.media_ids().contains(&m2));
}
