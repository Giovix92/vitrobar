//! Clock text in the user's locale format.

use windows::core::PCWSTR;
use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows::Win32::Globalization::{
    GetDateFormatEx, GetLocaleInfoEx, GetTimeFormatEx, DATE_SHORTDATE, LOCALE_IFIRSTDAYOFWEEK,
    TIME_NOSECONDS,
};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};

fn wide_to_string(buf: &[u16], n: i32) -> String {
    // The returned count includes the terminating NUL; 0 means failure.
    let len = (n.max(1) as usize - 1).min(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

/// Short time and short date of `t` in the user's default locale (a null locale name is
/// `LOCALE_NAME_USER_DEFAULT`).
pub fn clock_lines_at(t: &SYSTEMTIME) -> (String, String) {
    let mut tb = [0u16; 64];
    let mut db = [0u16; 128];
    // SAFETY: the buffers are passed as slices with their lengths; null locale and format
    // pointers select the user defaults.
    unsafe {
        let tn = GetTimeFormatEx(
            PCWSTR::null(),
            TIME_NOSECONDS,
            Some(t),
            PCWSTR::null(),
            Some(&mut tb),
        );
        let dn = GetDateFormatEx(
            PCWSTR::null(),
            DATE_SHORTDATE,
            Some(t),
            PCWSTR::null(),
            Some(&mut db),
            PCWSTR::null(),
        );
        (wide_to_string(&tb, tn), wide_to_string(&db, dn))
    }
}

/// The clock text for now (`time\ndate`) and the milliseconds already elapsed in this minute.
pub fn clock_now() -> (String, u32) {
    // SAFETY: plain query.
    let t = unsafe { GetLocalTime() };
    let (time, date) = clock_lines_at(&t);
    let into_minute = u32::from(t.wSecond) * 1000 + u32::from(t.wMilliseconds);
    (format!("{time}\n{date}"), into_minute)
}

/// The user's first day of the week: 0 = Monday ... 6 = Sunday (`LOCALE_IFIRSTDAYOFWEEK`);
/// Monday if the locale does not say.
pub fn first_day_of_week() -> u32 {
    let mut b = [0u16; 8];
    // SAFETY: the buffer is passed as a slice with its length; a null locale name selects
    // the user default.
    let n = unsafe { GetLocaleInfoEx(PCWSTR::null(), LOCALE_IFIRSTDAYOFWEEK, Some(&mut b)) };
    if n <= 0 {
        return 0;
    }
    parse_first_day(&b[..(n as usize - 1).min(b.len())])
}

fn parse_first_day(s: &[u16]) -> u32 {
    String::from_utf16_lossy(s)
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|d| *d < 7)
        .unwrap_or(0)
}

/// Today's local date and the milliseconds left until the next local midnight.
pub fn today_and_midnight_ms() -> ((i32, u8, u8), u64) {
    // SAFETY: plain query.
    let t = unsafe { GetLocalTime() };
    (
        (i32::from(t.wYear), t.wMonth as u8, t.wDay as u8),
        vitro_core::calendar::ms_until_midnight(
            u32::from(t.wHour),
            u32::from(t.wMinute),
            u32::from(t.wSecond),
            u32::from(t.wMilliseconds),
        ),
    )
}

/// The local short time (no seconds) of `epoch_secs`, in the user's format; empty if it cannot
/// be converted.
pub fn local_short_time(epoch_secs: i64) -> String {
    // FILETIME: 100 ns ticks since 1601-01-01.
    const EPOCH_DIFF_S: i64 = 11_644_473_600;
    let Some(ticks) = epoch_secs
        .checked_add(EPOCH_DIFF_S)
        .and_then(|s| s.checked_mul(10_000_000))
        .filter(|t| *t >= 0)
    else {
        return String::new();
    };
    let ft = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    let mut utc = SYSTEMTIME::default();
    let mut local = SYSTEMTIME::default();
    // SAFETY: locals passed by reference; a null time zone means the current one.
    let ok = unsafe {
        FileTimeToSystemTime(&ft, &mut utc).is_ok()
            && SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).is_ok()
    };
    if !ok {
        return String::new();
    }
    clock_lines_at(&local).0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_a_fixed_time() {
        let t = SYSTEMTIME {
            wYear: 2026,
            wMonth: 10,
            wDay: 30,
            wHour: 14,
            wMinute: 7,
            ..Default::default()
        };
        let (time, date) = clock_lines_at(&t);
        assert!(time.contains("07"), "{time:?}");
        assert!(date.contains("26"), "{date:?}");
        assert!(!time.contains('\0') && !date.contains('\0'));
    }

    #[test]
    fn first_day_parses_the_locale_string() {
        assert_eq!(parse_first_day(&[b'6' as u16]), 6);
        assert_eq!(parse_first_day(&[b'0' as u16]), 0);
        assert_eq!(parse_first_day(&[b'x' as u16]), 0);
        assert_eq!(parse_first_day(&[]), 0);
        assert!(first_day_of_week() < 7);
    }

    #[test]
    fn today_is_plausible() {
        let ((y, m, d), left) = today_and_midnight_ms();
        assert!(y >= 2024 && (1..=12).contains(&m) && (1..=31).contains(&d));
        assert!((1..=86_400_000).contains(&left));
    }

    #[test]
    fn now_is_two_lines() {
        let (text, ms) = clock_now();
        assert_eq!(text.lines().count(), 2, "{text:?}");
        assert!(ms < 61_000);
    }

    #[test]
    fn epoch_times_read_as_local_short_times() {
        let a = local_short_time(1_793_469_600);
        let b = local_short_time(1_793_469_600 + 3600);
        assert!(!a.is_empty() && !b.is_empty());
        assert_ne!(a, b);
        assert_eq!(
            a,
            local_short_time(1_793_469_600 + 20),
            "seconds are not shown"
        );
    }
}
