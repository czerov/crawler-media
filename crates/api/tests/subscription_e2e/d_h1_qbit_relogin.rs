//! D-H1: an expired qBittorrent session (403) is renewed by one re-login and
//! the request retried; a failing re-login is a clear, bounded error.

use std::sync::atomic::Ordering;

use super::harness::*;

#[tokio::test]
async fn expired_session_is_renewed_for_submit_and_collect() {
    let h = Harness::new();
    h.create_site().await;
    let id = h.create_movie_subscribe(serde_json::json!({})).await;
    let logins = h.qb.logins.load(Ordering::SeqCst);

    h.qb.forbid_next.store(1, Ordering::SeqCst); // SID expired
    h.run_subscribe(id).await;
    assert_eq!(h.qb.add_count(), 1, "submission succeeded after re-login");
    assert_eq!(h.qb.logins.load(Ordering::SeqCst), logins + 1, "exactly one re-login");
    assert_eq!(h.pending_state(id, "active"), 1);

    let video = probed_video(2160, "hevc", false);
    h.qb.complete(0, MATRIX_TITLE, &[(&format!("{MATRIX_TITLE}.mkv"), &video)]);
    h.qb.forbid_next.store(1, Ordering::SeqCst); // expired again before Transfer
    h.transfer(id).await.unwrap();
    assert_eq!(h.library_files().len(), 1, "collection succeeded after re-login");
    assert_eq!(h.qb.logins.load(Ordering::SeqCst), logins + 2);
}

#[tokio::test]
async fn failing_relogin_is_reported_without_retry_loops() {
    let h = Harness::new();
    h.create_site().await;
    let id = h.create_movie_subscribe(serde_json::json!({})).await;
    let logins = h.qb.logins.load(Ordering::SeqCst);

    h.qb.reject_login.store(true, Ordering::SeqCst); // password changed
    h.qb.forbid_next.store(usize::MAX / 2, Ordering::SeqCst);
    let (status, body) = h.run_subscribe_raw(id).await;
    assert!(!status.is_success(), "{body}");
    assert!(body.to_string().contains("login"), "clear authentication error: {body}");
    assert_eq!(h.qb.add_count(), 0);
    assert_eq!(h.pending_state(id, "active"), 0, "nothing recorded as submitted: {body}");
    let relogins = h.qb.logins.load(Ordering::SeqCst) - logins;
    assert!((1..=3).contains(&relogins), "bounded re-login attempts, got {relogins}");
}
