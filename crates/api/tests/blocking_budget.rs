//! `BlockingBudget` 的行为回归：准入、等待预算与 permit 寿命。

use std::sync::{Arc, Condvar, Mutex as StdMutex};
use std::time::Duration;

use api::blocking_budget::{BlockingBudget, BlockingError};

struct Gate {
    entered: StdMutex<bool>,
    entered_cv: Condvar,
    open: StdMutex<bool>,
    open_cv: Condvar,
}

impl Gate {
    fn new() -> Self {
        Self {
            entered: StdMutex::new(false),
            entered_cv: Condvar::new(),
            open: StdMutex::new(false),
            open_cv: Condvar::new(),
        }
    }

    fn enter_and_wait(&self) {
        *self.entered.lock().unwrap() = true;
        self.entered_cv.notify_all();
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.open_cv.wait(open).unwrap();
        }
    }

    fn wait_entered(&self) {
        let mut entered = self.entered.lock().unwrap();
        while !*entered {
            entered = self.entered_cv.wait(entered).unwrap();
        }
    }

    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.open_cv.notify_all();
    }
}

/// 断言失败时也要放行阻塞 closure，避免 runtime 关闭时卡住。
struct ReleaseOnDrop(Arc<Gate>);

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}

#[test]
#[should_panic(expected = "concurrency")]
fn zero_concurrency_is_a_configuration_error() {
    let _ = BlockingBudget::new(0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deadline_timeout_then_busy_then_ok_after_release() {
    let gate = Arc::new(Gate::new());
    let budget = Arc::new(BlockingBudget::new(1));

    let first = tokio::spawn({
        let budget = budget.clone();
        let gate = gate.clone();
        async move {
            budget
                .run(Duration::from_millis(50), move || {
                    gate.enter_and_wait();
                    1
                })
                .await
        }
    });
    gate.wait_entered();
    let _release = ReleaseOnDrop(gate.clone());

    let first = first.await.unwrap();
    assert!(
        matches!(first, Err(BlockingError::Timeout)),
        "deadline must time out while the closure blocks: {first:?}"
    );

    // permit 仍被阻塞 closure 持有，所以新请求必须立刻 Busy。
    let second = budget.run(Duration::from_secs(1), || 2).await;
    assert!(
        matches!(second, Err(BlockingError::Busy)),
        "permit must not be released before the closure ends: {second:?}"
    );

    gate.release();
    let third = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match budget.run(Duration::from_millis(100), || 3).await {
                Ok(value) => break value,
                Err(BlockingError::Busy) => tokio::time::sleep(Duration::from_millis(5)).await,
                Err(error) => panic!("unexpected blocking error: {error}"),
            }
        }
    })
    .await
    .expect("permit must be released once the closure finishes");
    assert_eq!(third, 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_to_completion_ignores_wait_budget_and_reports_worker_panic() {
    let budget = BlockingBudget::new(1);
    let value = budget
        .run_to_completion(|| {
            std::thread::sleep(Duration::from_millis(60));
            7
        })
        .await
        .unwrap();
    assert_eq!(value, 7);

    let panicked = budget.run_to_completion(|| panic!("worker exploded")).await;
    assert!(
        matches!(panicked, Err(BlockingError::Worker(_))),
        "a panicking worker must surface as Worker: {panicked:?}"
    );
}
