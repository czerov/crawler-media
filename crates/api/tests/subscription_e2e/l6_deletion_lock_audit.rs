//! L6: subscription paths delete replaced files through the M6 queue (file IO
//! outside the Store mutex) and pending cleanup holds the Subscribe guard.
//! Source audit so a regression to in-lock deletion is caught.

fn source(rel: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn subscription_paths_do_not_delete_files_under_the_store_lock() {
    for rel in [
        "src/worker/transfer.rs",
        "src/management/subscribes/run.rs",
        "src/http/downloaders/task_listing.rs",
    ] {
        let text = source(rel);
        assert!(
            !text.contains("remove_file_and_ledger"),
            "{rel} must enqueue deletions (M6) instead of deleting under the Store lock"
        );
    }
    let queue = source("src/http/file_delete.rs");
    let body = queue
        .split("pub(crate) fn retry_queued_deletions")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n").next())
        .expect("retry_queued_deletions");
    let delete_at = body.find("remove_physical_file").expect("deletes files");
    let lock_at = body[..delete_at]
        .rfind("store.lock()")
        .expect("lists queue");
    assert!(
        body[lock_at..delete_at].contains("};"),
        "the Store lock must be released before removing the file"
    );
}

#[test]
fn pending_cleanup_holds_the_subscribe_guard() {
    let text = source("src/http/downloaders/task_listing.rs");
    let body = text
        .split("fn delete_absent_pending")
        .nth(1)
        .expect("delete_absent_pending");
    let guard = body.find("subscribe_guard").expect("takes the guard");
    let store = body.find("state.store.lock()").expect("locks the store");
    assert!(guard < store, "guard → store lock order");
}
