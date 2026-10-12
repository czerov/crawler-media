//! 进程内共享的任务完成计数 watcher。
//!
//! 所有 SSE 连接共用**一个** 2 秒 poller：只在至少有一个订阅者时运行，最后一个
//! receiver 掉落后停止，下次订阅可重启。查询失败保留上一次好值；首次查询失败
//! 返回 `Err` 而不是伪造 0。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use tokio::sync::watch;

const POLL_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobCount {
    pub count: i64,
}

struct Inner {
    sender: Option<watch::Sender<JobCount>>,
    /// 每次启动新 poller 都自增：旧 poller 看到代际不符即退出，绝不清理新状态。
    generation: u64,
    poller_alive: bool,
}

pub struct JobUpdates {
    inner: Arc<Mutex<Inner>>,
    polls: Arc<AtomicUsize>,
}

impl Default for JobUpdates {
    fn default() -> Self {
        Self::new()
    }
}

impl JobUpdates {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                sender: None,
                generation: 0,
                poller_alive: false,
            })),
            polls: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// 累计轮询次数（可观测：验证多订阅者共用一个 poller、无订阅者时停止）。
    pub fn poll_count(&self) -> usize {
        self.polls.load(Ordering::SeqCst)
    }

    /// 当前订阅者数量。
    pub fn subscriber_count(&self) -> usize {
        self.inner
            .lock()
            .sender
            .as_ref()
            .map(watch::Sender::receiver_count)
            .unwrap_or(0)
    }

    /// 订阅任务计数。返回的 receiver 立即持有**真实当前计数**。
    pub async fn subscribe(
        &self,
        queue: Arc<Mutex<jobs::Queue>>,
    ) -> Result<watch::Receiver<JobCount>, String> {
        let count = self.current_count(&queue)?;
        let mut guard = self.inner.lock();
        let sender = match guard.sender.clone() {
            Some(sender) => sender,
            None => {
                let (sender, _receiver) = watch::channel(count.clone());
                guard.sender = Some(sender.clone());
                sender
            }
        };
        sender.send_if_modified(|value| {
            if *value == count {
                false
            } else {
                *value = count.clone();
                true
            }
        });
        let receiver = sender.subscribe();
        if !guard.poller_alive {
            guard.poller_alive = true;
            guard.generation += 1;
            let generation = guard.generation;
            drop(guard);
            tokio::spawn(poll_loop(
                self.inner.clone(),
                self.polls.clone(),
                queue,
                generation,
            ));
        }
        Ok(receiver)
    }

    /// 订阅时立刻查一次真实计数：失败且没有上一次好值时返回 `Err`。
    fn current_count(&self, queue: &Arc<Mutex<jobs::Queue>>) -> Result<JobCount, String> {
        match queue.lock().count_finished_since(0) {
            Ok(count) => Ok(JobCount { count }),
            Err(error) => {
                let last = self
                    .inner
                    .lock()
                    .sender
                    .as_ref()
                    .map(|sender| sender.borrow().clone());
                match last {
                    Some(value) => {
                        tracing::warn!(%error, "任务计数查询失败，保留上一次好值");
                        Ok(value)
                    }
                    None => Err(error.to_string()),
                }
            }
        }
    }
}

async fn poll_loop(
    inner: Arc<Mutex<Inner>>,
    polls: Arc<AtomicUsize>,
    queue: Arc<Mutex<jobs::Queue>>,
    generation: u64,
) {
    loop {
        tokio::time::sleep(POLL_INTERVAL).await;
        // 停止判定与订阅者计数必须在同一个临界区内完成，否则新订阅可能
        // 与“最后一个 receiver 刚掉落”竞争，导致无人轮询。
        let sender = {
            let mut guard = inner.lock();
            if guard.generation != generation {
                return;
            }
            match guard.sender.clone() {
                Some(sender) if sender.receiver_count() > 0 => sender,
                _ => {
                    guard.poller_alive = false;
                    return;
                }
            }
        };
        polls.fetch_add(1, Ordering::SeqCst);
        match queue.lock().count_finished_since(0) {
            Ok(count) => {
                let next = JobCount { count };
                sender.send_if_modified(|value| {
                    if *value == next {
                        false
                    } else {
                        *value = next;
                        true
                    }
                });
            }
            Err(error) => tracing::warn!(%error, "任务计数轮询失败，保留上一次好值"),
        }
    }
}
