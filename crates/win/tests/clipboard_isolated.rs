//! `set_text` against another clipboard: the process switches to a non-interactive window
//! station (each has its own clipboard), so the user's clipboard (WinSta0's) is never
//! touched. Its own test binary, because the window station is per process.

use vitro_win::clipboard::set_text;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, CloseWindowStation, CreateDesktopW, CreateWindowStationW,
    GetProcessWindowStation, SetProcessWindowStation, SetThreadDesktop, DESKTOP_CONTROL_FLAGS,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE,
};

const GENERIC_ALL: u32 = 0x1000_0000;
const CF_UNICODETEXT: u32 = 13;

fn clipboard_text(owner: HWND) -> Option<String> {
    // SAFETY: the clipboard is opened and closed here; the text is read only while locked,
    // up to its terminating NUL.
    unsafe {
        OpenClipboard(Some(owner)).ok()?;
        let text = GetClipboardData(CF_UNICODETEXT).ok().and_then(|h| {
            let g = HGLOBAL(h.0);
            let p = GlobalLock(g) as *const u16;
            if p.is_null() {
                return None;
            }
            let mut n = 0;
            while *p.add(n) != 0 {
                n += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
            let _ = GlobalUnlock(g);
            Some(s)
        });
        let _ = CloseClipboard();
        text
    }
}

#[test]
fn set_text_on_a_private_clipboard() {
    // A fresh thread: SetThreadDesktop needs a thread without windows.
    std::thread::spawn(|| {
        // SAFETY: opens a non-interactive window station, creates a desktop in it, uses them on this thread
        // only, and puts the process back on its original window station before closing them.
        unsafe {
            let original = GetProcessWindowStation().expect("current window station");
            // Unnamed: the logon session's own non-interactive station ("Service-0x0-...$";
            // a normal user may not create named ones). Its clipboard is not WinSta0's, the
            // one the user sees, and may keep text from an earlier run.
            let station = CreateWindowStationW(PCWSTR::null(), 0, GENERIC_ALL, None)
                .expect("private window station");
            SetProcessWindowStation(station).expect("switch window station");
            let desktop = CreateDesktopW(
                w!("vitrobar-clipboard-test"),
                PCWSTR::null(),
                None,
                DESKTOP_CONTROL_FLAGS(0),
                GENERIC_ALL,
                None,
            )
            .expect("private desktop");
            SetThreadDesktop(desktop).expect("switch desktop");
            let owner = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!("vitrobar-clipboard-test"),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                None,
                None,
            )
            .expect("owner window");

            let sample = "Vitrobar è 日本 \"quoted\" & 100%";
            let first = set_text(owner, sample);
            let got = clipboard_text(owner);
            let second = set_text(owner, "");
            let empty = clipboard_text(owner);

            let _ = DestroyWindow(owner);
            SetProcessWindowStation(original).expect("switch back");
            let _ = CloseDesktop(desktop);
            let _ = CloseWindowStation(station);

            first.expect("set_text");
            assert_eq!(got.as_deref(), Some(sample));
            second.expect("set_text empty");
            assert_eq!(empty.as_deref(), Some(""));
        }
    })
    .join()
    .expect("test thread");
}
