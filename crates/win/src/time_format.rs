//! Clock text in the user's locale format.

use windows::core::PCWSTR;
use windows::Win32::Foundation::SYSTEMTIME;
use windows::Win32::Globalization::{
    GetDateFormatEx, GetTimeFormatEx, DATE_SHORTDATE, TIME_NOSECONDS,
};
use windows::Win32::System::SystemInformation::GetLocalTime;

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
    fn now_is_two_lines() {
        let (text, ms) = clock_now();
        assert_eq!(text.lines().count(), 2, "{text:?}");
        assert!(ms < 61_000);
    }
}
