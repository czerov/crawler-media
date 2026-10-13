use jobs::{JobKind, JobStatus, NewDef, Queue};

#[test]
fn manual_run_links_an_unscheduled_job_to_its_definition() {
    let dir = tempfile::tempdir().unwrap();
    let queue = Queue::open(dir.path().join("jobs.db")).unwrap();
    let def = queue
        .upsert_def(NewDef {
            kind: JobKind::Transfer,
            name: "manual transfer".into(),
            enabled: true,
            schedule: None,
            payload: "{}".into(),
            timeout_secs: None,
            concurrency_key: Some("manual-transfer".into()),
        })
        .unwrap();
    assert!(queue.ensure_scheduled(1).unwrap().is_empty());
    let created = queue.ensure_scheduled_for(0, def.id).unwrap();
    assert_eq!(created.len(), 1);
    let live = queue.get_live_child(def.id).unwrap().unwrap();
    assert_eq!(live.id, created[0].id);
    assert_eq!(live.status, JobStatus::Queued);
    assert_eq!(live.run_after, 0);
}

#[test]
fn manual_run_queues_one_follow_up_behind_a_running_child() {
    let dir = tempfile::tempdir().unwrap();
    let queue = Queue::open(dir.path().join("jobs.db")).unwrap();
    let def = queue
        .upsert_def(NewDef {
            kind: JobKind::SubscribeSearch,
            name: "search".into(),
            enabled: true,
            schedule: Some(jobs::Schedule::Interval { secs: 1800 }),
            payload: r#"{"subscribe_id":"sub-1"}"#.into(),
            timeout_secs: Some(120),
            concurrency_key: None,
        })
        .unwrap();
    let initial = queue.ensure_scheduled_for(0, def.id).unwrap().remove(0);
    let running = queue.claim(0, 1).unwrap().remove(0);
    assert_eq!(running.id, initial.id);

    let follow_up = queue.ensure_scheduled_for(0, def.id).unwrap();
    assert_eq!(follow_up.len(), 1);
    assert_eq!(follow_up[0].status, JobStatus::Queued);
    assert_eq!(follow_up[0].run_after, 0);
    assert!(queue.ensure_scheduled_for(0, def.id).unwrap().is_empty());
    assert!(queue.claim(0, 2).unwrap().is_empty());

    queue.succeed(running.id, 1).unwrap();
    let claimed = queue.claim(1, 1).unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, follow_up[0].id);
}

#[test]
fn failed_running_child_does_not_conflict_with_queued_follow_up() {
    let dir = tempfile::tempdir().unwrap();
    let queue = Queue::open(dir.path().join("jobs.db")).unwrap();
    let def = queue
        .upsert_def(NewDef {
            kind: JobKind::SubscribeSearch,
            name: "search".into(),
            enabled: true,
            schedule: Some(jobs::Schedule::Interval { secs: 1800 }),
            payload: r#"{"subscribe_id":"sub-1"}"#.into(),
            timeout_secs: Some(120),
            concurrency_key: None,
        })
        .unwrap();
    let initial = queue.ensure_scheduled_for(0, def.id).unwrap().remove(0);
    let running = queue.claim(0, 1).unwrap().remove(0);
    let follow_up = queue.ensure_scheduled_for(0, def.id).unwrap().remove(0);

    let failed = queue
        .fail_claimed(&running, 1, "network error")
        .unwrap()
        .unwrap();
    assert_eq!(failed.status, JobStatus::Failed);
    assert_eq!(failed.attempt, 1);
    assert_eq!(
        queue.get_live_child(def.id).unwrap().unwrap().id,
        follow_up.id
    );

    let claimed = queue.claim(1, 1).unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, follow_up.id);
    assert_eq!(failed.id, initial.id);
}

#[test]
fn manual_run_while_running_does_not_add_another_child() {
    let dir = tempfile::tempdir().unwrap();
    let queue = Queue::open(dir.path().join("jobs.db")).unwrap();
    let def = queue
        .upsert_def(NewDef {
            kind: JobKind::JobsRetention,
            name: "jobs retention".into(),
            enabled: true,
            schedule: Some(jobs::Schedule::Interval { secs: 86_400 }),
            payload: r#"{"version":1,"retention_days":90,"dry_run":false}"#.into(),
            timeout_secs: None,
            concurrency_key: None,
        })
        .unwrap();
    let initial = queue.ensure_scheduled_for(0, def.id).unwrap().remove(0);
    let running = queue.claim(0, 1).unwrap().remove(0);
    assert_eq!(running.id, initial.id);

    let again = queue.ensure_scheduled_for(0, def.id).unwrap();
    assert!(
        again.is_empty(),
        "运行中再点立即执行不得新增子任务: {again:?}"
    );

    queue.succeed(running.id, 10).unwrap();
    let claimed = queue.claim(10, 2).unwrap();
    assert!(
        claimed.is_empty(),
        "运行中的那次立即执行不得留下 run_after=0 的新任务: {claimed:?}"
    );
    let follow_up = queue.get_live_child(def.id).unwrap();
    let immediate = follow_up.as_ref().is_some_and(|job| job.run_after <= 10);
    assert!(
        !immediate,
        "结束后只允许周期自己排下一次，不能是立即任务: {follow_up:?}"
    );
}
