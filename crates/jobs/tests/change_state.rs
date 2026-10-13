use jobs::{JobKind, NewDef, Queue};

#[test]
fn job_change_state_tracks_revisions_and_finished_counts() {
    let tmp = tempfile::tempdir().unwrap();
    let queue = Queue::open(tmp.path().join("jobs.db")).unwrap();

    let initial = queue.change_state().unwrap();
    assert_eq!(initial.finished_count, 0);

    // Enqueue job (unfinished)
    let job = queue.enqueue(JobKind::Transfer, "{}", 0).unwrap();
    let after_enqueue = queue.change_state().unwrap();
    assert!(after_enqueue.revision > initial.revision);
    assert_eq!(after_enqueue.finished_count, 0);

    // Claim and finish
    let claimed = queue.claim(0, 1).unwrap().remove(0);
    queue.succeed(claimed.id, 100).unwrap();

    let after_succeed = queue.change_state().unwrap();
    assert!(after_succeed.revision > after_enqueue.revision);
    assert_eq!(after_succeed.finished_count, 1);
}
