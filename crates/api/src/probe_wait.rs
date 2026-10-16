//! Review N-1: a wash-cut arrival whose source cannot be probed waits a
//! bounded number of rounds (and at most 24 hours); then it is rejected and
//! the user is notified, so the slot never stays occupied forever.

use crate::ApiState;

const DEFAULT_MAX_ATTEMPTS: i64 = 6;
const MAX_WAIT_SECS: i64 = 24 * 60 * 60;

fn max_attempts(state: &ApiState) -> i64 {
    state
        .store
        .lock()
        .get_setting(crate::settings_keys::SUBSCRIBE_PROBE_WAIT_MAX_ATTEMPTS)
        .ok()
        .flatten()
        .and_then(|value| value.trim().parse::<i64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_MAX_ATTEMPTS)
}

/// Record one more waiting round for each awaiting enclosure and return the
/// ones whose wait is exhausted (to be marked rejected). Notifies per item.
pub(crate) fn exhausted_probe_waits(
    state: &ApiState,
    subscribe_id: domain::SubscribeId,
    awaiting: &[String],
) -> Vec<String> {
    if awaiting.is_empty() {
        return Vec::new();
    }
    let limit = max_attempts(state);
    let now = crate::job_loop::unix_now();
    let mut exhausted = Vec::new();
    for enclosure in awaiting {
        let noted = state
            .store
            .lock()
            .note_probe_wait(subscribe_id, enclosure, now);
        match noted {
            Ok(Some((attempts, since))) if attempts >= limit || now - since >= MAX_WAIT_SECS => {
                tracing::warn!(%subscribe_id, %enclosure, attempts, "源文件多次 probe 失败，放弃替换并标记为 rejected");
                exhausted.push(enclosure.clone());
            }
            Ok(_) => {}
            Err(error) => tracing::error!(%subscribe_id, %error, "记录 probe 等待次数失败"),
        }
    }
    for enclosure in &exhausted {
        crate::worker::notify(
            state,
            "洗版文件无法识别质量",
            &format!(
                "下载完成的文件多次 probe 失败（请检查 ffprobe 或文件是否损坏），已放弃替换在位版本：{enclosure}"
            ),
        );
    }
    exhausted
}
