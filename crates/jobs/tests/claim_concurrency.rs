use jobs::{JobKind, Queue};

#[test]
fn claim_atomic_across_queue_instances() {
    let tmp = tempfile::tempdir().unwrap();
    let q_path = tmp.path().join("jobs.db");
    let queue1 = Queue::open(&q_path).unwrap();
    let queue2 = Queue::open(&q_path).unwrap();

    // Enqueue 2 jobs with the same concurrency key
    let job1 = queue1
        .enqueue_with_key(JobKind::CatalogRefresh, "{}", 0, Some("exclusive_key"))
        .unwrap();
    let _job2 = queue1
        .enqueue_with_key(JobKind::CatalogRefresh, "{}", 0, Some("exclusive_key"))
        .unwrap();

    // queue1 claims 1 job
    let claimed1 = queue1.claim(0, 1).unwrap();
    assert_eq!(claimed1.len(), 1);
    assert_eq!(claimed1[0].id, job1.id);

    // queue2 tries to claim: concurrency_key is busy with job1, so queue2 must get 0 jobs!
    let claimed2 = queue2.claim(0, 1).unwrap();
    assert_eq!(
        claimed2.len(),
        0,
        "exclusive concurrency key must prevent concurrent claims"
    );

    // Once job1 finishes, queue2 can claim the second job
    queue1.succeed(job1.id, 100).unwrap();
    let claimed2_after = queue2.claim(0, 1).unwrap();
    assert_eq!(claimed2_after.len(), 1);
}
