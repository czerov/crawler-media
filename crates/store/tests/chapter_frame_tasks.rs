//! Durable chapter frame obligation lifecycle: idempotent enqueue, FIFO claim,
//! terminal ack, and crash recovery back to `queued`.

use domain::LedgerId;
use store::{ChapterFrameTask, Store};

fn task(ledger: LedgerId, version: &str, hash: &str) -> ChapterFrameTask {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    ChapterFrameTask {
        id: format!("task-{id:08}"),
        ledger_id: ledger,
        source_version: version.into(),
        targets: vec![(0, 10_000), (10_000, 20_000)],
        targets_hash: hash.into(),
    }
}

fn counts(store: &Store) -> store::ChapterFrameTaskCounts {
    store.chapter_frame_task_counts().unwrap()
}

#[test]
fn duplicate_version_and_targets_are_idempotent() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let ledger = LedgerId::new();

    assert!(store
        .enqueue_chapter_frame_task(&task(ledger, "v1", "hash-a"))
        .unwrap());
    assert!(
        !store
            .enqueue_chapter_frame_task(&task(ledger, "v1", "hash-a"))
            .unwrap(),
        "same version/hash must not create a second obligation"
    );
    assert!(store
        .enqueue_chapter_frame_task(&task(ledger, "v1", "hash-b"))
        .unwrap());
    assert!(store
        .enqueue_chapter_frame_task(&task(ledger, "v2", "hash-a"))
        .unwrap());

    let pending = store.pending_chapter_frame_tasks(10).unwrap();
    assert_eq!(pending.len(), 3);
    assert_eq!(counts(&store).queued, 3);
}

#[test]
fn pending_is_fifo_and_bounded_by_limit() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    for index in 0..5 {
        let mut entry = task(LedgerId::new(), "v1", "hash");
        entry.targets = vec![(index, index + 1)];
        store.enqueue_chapter_frame_task(&entry).unwrap();
    }

    let first = store.pending_chapter_frame_tasks(2).unwrap();
    assert_eq!(first.len(), 2);
    let all = store.pending_chapter_frame_tasks(10).unwrap();
    assert_eq!(all.len(), 5);
    assert_eq!(
        all.iter().take(2).map(|task| &task.id).collect::<Vec<_>>(),
        first.iter().map(|task| &task.id).collect::<Vec<_>>()
    );
    assert_eq!(all[0].targets, vec![(0, 1)]);
}

#[test]
fn claim_is_cas_and_terminal_ack_is_visible() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let ledger = LedgerId::new();
    let entry = task(ledger, "v1", "hash");
    store.enqueue_chapter_frame_task(&entry).unwrap();

    assert!(store.claim_chapter_frame_task(&entry.id).unwrap());
    assert!(
        !store.claim_chapter_frame_task(&entry.id).unwrap(),
        "a running obligation must not be claimed twice"
    );
    assert_eq!(counts(&store).running, 1);
    assert!(store.pending_chapter_frame_tasks(10).unwrap().is_empty());

    store
        .finish_chapter_frame_task(&entry.id, true, None)
        .unwrap();
    assert_eq!(counts(&store).completed, 1);
    assert_eq!(counts(&store).running, 0);
}

#[test]
fn failed_ack_records_the_error() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let entry = task(LedgerId::new(), "v1", "hash");
    store.enqueue_chapter_frame_task(&entry).unwrap();
    assert!(store.claim_chapter_frame_task(&entry.id).unwrap());
    store
        .finish_chapter_frame_task(&entry.id, false, Some("ffmpeg failed"))
        .unwrap();
    assert_eq!(counts(&store).failed, 1);
    assert_eq!(
        store
            .chapter_frame_task(&entry.id)
            .unwrap()
            .unwrap()
            .targets,
        entry.targets
    );
}

#[test]
fn running_work_recovers_to_queued_after_restart() {
    let temp = tempfile::tempdir().unwrap();
    let entry;
    {
        let store = Store::open(temp.path()).unwrap();
        entry = task(LedgerId::new(), "v1", "hash");
        store.enqueue_chapter_frame_task(&entry).unwrap();
        assert!(store.claim_chapter_frame_task(&entry.id).unwrap());
        assert_eq!(counts(&store).running, 1);
    }

    let reopened = Store::open(temp.path()).unwrap();
    assert_eq!(reopened.recover_chapter_frame_tasks().unwrap(), 1);
    assert_eq!(counts(&reopened).queued, 1);
    assert_eq!(counts(&reopened).running, 0);
    assert!(reopened.claim_chapter_frame_task(&entry.id).unwrap());
    reopened
        .finish_chapter_frame_task(&entry.id, true, None)
        .unwrap();
    assert_eq!(counts(&reopened).completed, 1);
}

#[test]
fn terminal_ack_is_not_overwritten_by_a_late_running_finish() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let entry = task(LedgerId::new(), "v1", "hash");
    store.enqueue_chapter_frame_task(&entry).unwrap();
    assert!(store.claim_chapter_frame_task(&entry.id).unwrap());
    store.recover_chapter_frame_tasks().unwrap();
    // Recovery returned it to queued; a stale finish must not mark it done.
    store
        .finish_chapter_frame_task(&entry.id, true, None)
        .unwrap();
    assert_eq!(counts(&store).queued, 1);
    assert_eq!(counts(&store).completed, 0);
}
