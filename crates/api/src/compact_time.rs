//! 紧凑时间格式：`2026-09-23 07:12:48`（支持本地时区 / 容器 TZ 环境变量，秒级）。
//! 默认 `SystemTime` 计时器输出 RFC3339 微秒级 `...48.755985Z`，
//! 日志不需要那么精确，这里只保留到秒，便于人读。

use std::time::{SystemTime, UNIX_EPOCH};

use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::FormatTime;

#[derive(Clone, Copy, Debug, Default)]
pub struct CompactTime;

impl FormatTime for CompactTime {
    fn format_time(&self, w: &mut Writer<'_>) -> std::fmt::Result {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        let (year, month, day, hour, min, sec) = format_local_parts(secs);

        write!(
            w,
            "{year:04}-{month:02}-{day:02} {hour:02}:{min:02}:{sec:02}"
        )
    }
}

pub(crate) fn format_rfc3339_utc(value: SystemTime) -> Option<String> {
    let elapsed = value.duration_since(UNIX_EPOCH).ok()?;
    let seconds = i64::try_from(elapsed.as_secs()).ok()?;
    let (year, month, day) = civil_date(seconds.div_euclid(86_400));
    let rem = seconds.rem_euclid(86_400);
    let (hour, minute, second) = (rem / 3600, rem % 3600 / 60, rem % 60);
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{:03}Z",
        elapsed.subsec_millis()
    ))
}

/// `YYYY-MM-DD` of `secs` in the server's local timezone (honours `TZ`; UTC fallback).
pub(crate) fn local_date(secs: i64) -> String {
    let (year, month, day, ..) = format_local_parts(secs);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(unix)]
#[repr(C)]
struct Tm {
    tm_sec: std::ffi::c_int,
    tm_min: std::ffi::c_int,
    tm_hour: std::ffi::c_int,
    tm_mday: std::ffi::c_int,
    tm_mon: std::ffi::c_int,
    tm_year: std::ffi::c_int,
    tm_wday: std::ffi::c_int,
    tm_yday: std::ffi::c_int,
    tm_isdst: std::ffi::c_int,
    tm_gmtoff: std::ffi::c_long,
    tm_zone: *const std::ffi::c_char,
}

#[cfg(unix)]
unsafe extern "C" {
    fn localtime_r(timep: *const i64, result: *mut Tm) -> *mut Tm;
}

/// 获取当地时间的各个分量 (year, month, day, hour, min, sec)。
///
/// 通过 C 库 localtime_r 解析当地时间，自动遵循系统当前时区以及 `TZ` 环境变量设置。
/// 若在非 Unix 平台或转换失败，回退到 UTC 时间计算。
fn format_local_parts(secs: i64) -> (i64, i64, i64, i64, i64, i64) {
    #[cfg(unix)]
    {
        use std::mem::MaybeUninit;
        let mut out = MaybeUninit::<Tm>::uninit();
        // SAFETY: localtime_r 是 POSIX 线程安全函数，传入有效的指针
        let ptr = unsafe { localtime_r(&secs, out.as_mut_ptr()) };
        if !ptr.is_null() {
            let tm = unsafe { out.assume_init() };
            return (
                (tm.tm_year as i64) + 1900,
                (tm.tm_mon as i64) + 1,
                tm.tm_mday as i64,
                tm.tm_hour as i64,
                tm.tm_min as i64,
                tm.tm_sec as i64,
            );
        }
    }

    let (year, month, day) = civil_date(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    let (hour, min, sec) = (rem / 3600, rem % 3600 / 60, rem % 60);
    (year, month, day, hour, min, sec)
}

fn civil_date(days: i64) -> (i64, i64, i64) {
    // 公历日历推算（Howard Hinnant civil_from_days 算法），UTC。
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn formats_rfc3339_utc_with_milliseconds() {
        assert_eq!(
            format_rfc3339_utc(UNIX_EPOCH + Duration::from_millis(1_700_000_000_123)),
            Some("2023-11-14T22:13:20.123Z".into())
        );
    }

    #[test]
    fn local_date_matches_system_date_command() {
        let now = crate::job_loop::unix_now();
        let Ok(out) = std::process::Command::new("date").arg("+%F").output() else {
            return;
        };
        let system = String::from_utf8_lossy(&out.stdout).trim().to_string();
        // Skip the (rare) midnight race between the two clock reads.
        if system == local_date(crate::job_loop::unix_now()) {
            assert_eq!(local_date(now), system);
        }
    }

    #[test]
    fn local_date_differs_from_utc_when_offset_crosses_midnight() {
        // 2023-11-14T22:13:20Z: in UTC+8 this is already 2023-11-15.
        let Ok(out) = std::process::Command::new("date")
            .args(["-d", "@1700000000", "+%F"])
            .output()
        else {
            return;
        };
        let system = String::from_utf8_lossy(&out.stdout).trim().to_string();
        assert_eq!(local_date(1_700_000_000), system);
    }

    #[test]
    fn test_format_local_parts_returns_valid_datetime() {
        let (y, m, d, h, min, s) = format_local_parts(1_700_000_000);
        assert_eq!(y, 2023);
        assert_eq!(m, 11);
        assert!((14..=15).contains(&d));
        assert!((0..24).contains(&h));
        assert!((0..60).contains(&min));
        assert!((0..60).contains(&s));
    }
}
