//! `Queue::children_for_defs` 与逐个 `get_live_child`/`get_last_child` 逐条一致。

use domain::JobDefId;
use jobs::{JobKind, NewDef, Queue};

fn new_def(queue: &Queue, name: &str) -> JobDefId {
    queue
        .upsert_def(NewDef {
            kind: JobKind::CatalogRefresh,
            name: name.into(),
            enabled: true,
            schedule: None,
            payload: format!(r#"{{"name":"{name}"}}"#),
            timeout_secs: None,
            concurrency_key: None,
        })
        .unwrap()
        .id
}

/// 混合状态 fixture：A 同时有 running + queued；B 有两个终态；C 无子任务；D 只有 running。
fn seed_mixed(
    queue: &Queue,
) -> (
    JobDefId,
    JobDefId,
    JobDefId,
    JobDefId,
    domain::JobId,
    domain::JobId,
) {
    let a = new_def(queue, "alpha");
    let b = new_def(queue, "bravo");
    let c = new_def(queue, "charlie");
    let d = new_def(queue, "delta");

    // D: running only
    let d1 = queue.ensure_scheduled_for(0, d).unwrap().remove(0);
    assert_eq!(queue.claim(1, 1).unwrap()[0].id, d1.id);

    // B: 两个终态（succeeded@10 与 failed；失败要耗尽重试才落终态）
    let b1 = queue.ensure_scheduled_for(0, b).unwrap().remove(0);
    assert_eq!(queue.claim(1, 1).unwrap()[0].id, b1.id);
    queue.succeed(b1.id, 10).unwrap();
    let b2 = queue.ensure_scheduled_for(0, b).unwrap().remove(0);
    let mut now = 20;
    loop {
        let claimed = queue.claim(now, 1).unwrap();
        assert_eq!(claimed[0].id, b2.id);
        match queue.fail_claimed(&claimed[0], now, "boom").unwrap() {
            Some(job) if job.status == jobs::JobStatus::Failed => break,
            Some(_) => now += 86_400, // 跳过重试退避，直到耗尽 MAX_ATTEMPTS
            None => panic!("fail_claimed lost its claim"),
        }
    }

    // A: running + queued（queued 必须优先）
    let a1 = queue.ensure_scheduled_for(0, a).unwrap().remove(0);
    assert_eq!(queue.claim(1, 1).unwrap()[0].id, a1.id);
    let a2 = queue.ensure_scheduled_for(0, a).unwrap().remove(0);

    (a, b, c, d, a2.id, b2.id)
}

#[test]
fn children_for_defs_matches_individual_queries() {
    let tmp = tempfile::tempdir().unwrap();
    let queue = Queue::open(tmp.path().join("jobs.db")).unwrap();
    let (a, b, c, d, a_queued, b_last) = seed_mixed(&queue);

    let ids = vec![a, b, c, d];
    let batch = queue.children_for_defs(&ids).unwrap();
    assert_eq!(batch.len(), 4);

    for child in &batch {
        let live = queue.get_live_child(child.def_id).unwrap();
        let last = queue.get_last_child(child.def_id).unwrap();
        assert_eq!(
            child.live.as_ref().map(|job| job.id),
            live.as_ref().map(|job| job.id),
            "live mismatch for {}",
            child.def_id
        );
        assert_eq!(
            child.last.as_ref().map(|job| job.id),
            last.as_ref().map(|job| job.id),
            "last mismatch for {}",
            child.def_id
        );
    }

    let find = |id: JobDefId| batch.iter().find(|child| child.def_id == id).unwrap();
    assert_eq!(
        find(a).live.as_ref().unwrap().id,
        a_queued,
        "queued 优先于 running"
    );
    assert!(find(a).last.is_none());
    assert_eq!(
        find(b).last.as_ref().unwrap().id,
        b_last,
        "finished_at DESC"
    );
    assert!(find(c).live.is_none() && find(c).last.is_none());
    assert!(find(d).live.is_some() && find(d).last.is_none());
}

#[test]
fn children_for_defs_handles_empty_and_duplicate_input() {
    let tmp = tempfile::tempdir().unwrap();
    let queue = Queue::open(tmp.path().join("jobs.db")).unwrap();
    assert!(queue.children_for_defs(&[]).unwrap().is_empty());

    let def = new_def(&queue, "solo");
    queue.ensure_scheduled_for(0, def).unwrap();
    let duplicated = queue.children_for_defs(&[def, def]).unwrap();
    assert_eq!(duplicated.len(), 1, "duplicate ids must not duplicate rows");
    assert!(duplicated[0].live.is_some());
}

#[test]
fn children_for_defs_covers_chunk_boundary() {
    let tmp = tempfile::tempdir().unwrap();
    let queue = Queue::open(tmp.path().join("jobs.db")).unwrap();
    let ids: Vec<JobDefId> = (0..501)
        .map(|index| {
            let def = new_def(&queue, &format!("bulk-{index}"));
            queue.ensure_scheduled_for(0, def).unwrap();
            def
        })
        .collect();

    let batch = queue.children_for_defs(&ids).unwrap();
    assert_eq!(batch.len(), 501);
    for child in &batch {
        assert_eq!(
            child.live.as_ref().map(|job| job.id),
            queue
                .get_live_child(child.def_id)
                .unwrap()
                .map(|job| job.id)
        );
    }
}
