//! DB4：wanted 批量更新必须在 subscribe 单连接上原子生效。

use std::collections::HashMap;
use std::path::Path;

use domain::SubscribeId;
use store::{Store, WantedHistory};

type Units = Vec<(Option<u32>, Option<u32>)>;

fn history(store: &Store, id: SubscribeId) -> HashMap<(Option<u32>, Option<u32>), WantedHistory> {
    store.load_wanted_history(id).unwrap()
}

fn reject_episode_two(path: &Path) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER reject_episode_two_insert BEFORE INSERT ON subscribe_wanted
         WHEN NEW.episode = 2 BEGIN SELECT RAISE(ABORT, 'fixture failure'); END;
         CREATE TRIGGER reject_episode_two_update BEFORE UPDATE ON subscribe_wanted
         WHEN NEW.episode = 2 BEGIN SELECT RAISE(ABORT, 'fixture failure'); END;",
    )
    .unwrap();
}

#[test]
fn touch_and_reset_apply_to_every_unit() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let id = SubscribeId::new();
    let units: Units = vec![(Some(1), Some(1)), (Some(1), Some(2)), (None, None)];

    store.touch_wanted_searches(id, &units, 100).unwrap();
    store.touch_wanted_searches(id, &units, 200).unwrap();

    let rows = history(&store, id);
    assert_eq!(rows.len(), 3);
    for unit in &units {
        let row = &rows[unit];
        assert_eq!(row.search_attempts, 2, "unit {unit:?}");
        assert_eq!(row.last_search_at, Some(200), "unit {unit:?}");
    }

    // 只重置一个 unit：其余保持原样，计数语义不变。
    let reset = store
        .reset_wanted_search_cooldowns(id, &[(Some(1), Some(1))])
        .unwrap();
    assert_eq!(reset, 1);
    let rows = history(&store, id);
    assert_eq!(rows[&(Some(1), Some(1))].search_attempts, 0);
    assert_eq!(rows[&(Some(1), Some(1))].last_search_at, None);
    assert_eq!(rows[&(Some(1), Some(2))].search_attempts, 2);

    // 没有可重置的行 → 0。
    assert_eq!(
        store
            .reset_wanted_search_cooldowns(id, &[(Some(1), Some(1))])
            .unwrap(),
        0
    );
}

#[test]
fn failing_unit_rolls_back_the_earlier_units() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let id = SubscribeId::new();
    store
        .touch_wanted_searches(id, &[(Some(1), Some(1))], 100)
        .unwrap();
    assert_eq!(history(&store, id)[&(Some(1), Some(1))].search_attempts, 1);

    // 第二个 unit 会在写入时失败：批量调用必须整体回滚。
    reject_episode_two(&tmp.path().join("subscribe.db"));

    let result = store.touch_wanted_searches(id, &[(Some(1), Some(1)), (Some(1), Some(2))], 200);
    assert!(result.is_err(), "the failing unit must surface an error");

    let rows = history(&store, id);
    assert_eq!(
        rows[&(Some(1), Some(1))].search_attempts, 1,
        "the first unit's update must roll back"
    );
    assert_eq!(rows[&(Some(1), Some(1))].last_search_at, Some(100));
    assert!(
        !rows.contains_key(&(Some(1), Some(2))),
        "the failing unit must not leave a row"
    );
}

#[test]
fn batch_inside_an_existing_transaction_does_not_nest() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let id = SubscribeId::new();
    let units: Units = vec![(Some(2), Some(1)), (Some(2), Some(2))];

    // playback_write 在 subscribe 连接上已经开了事务；批量方法必须沿用而不是
    // 嵌套开事务（嵌套会直接报错），并且仍在同一事务里生效。
    store
        .playback_write(|store| {
            store.touch_wanted_searches(id, &units, 10)?;
            store.reset_wanted_search_cooldowns(id, &units)?;
            Ok(())
        })
        .unwrap();

    let rows = history(&store, id);
    assert_eq!(rows.len(), 2);
    for unit in &units {
        assert_eq!(rows[unit].search_attempts, 0, "touch 之后又被 reset");
        assert_eq!(rows[unit].last_search_at, None);
    }
}
