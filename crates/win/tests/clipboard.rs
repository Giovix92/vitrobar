//! Writes the real clipboard, so it is ignored by default. It saves the previous text and
//! puts it back, and does nothing when the clipboard holds anything it could not restore.

use vitro_win::clipboard::set_text;
use windows::core::w;
use windows::Win32::Foundation::{HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData, OpenClipboard,
};
use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE,
};

const CF_TEXT: u32 = 1;
const CF_OEMTEXT: u32 = 7;
const CF_UNICODETEXT: u32 = 13;
const CF_LOCALE: u32 = 16;

fn open(owner: HWND) {
    for _ in 0..50 {
        // SAFETY: plain call; balanced by CloseClipboard in the callers.
        if unsafe { OpenClipboard(Some(owner)) }.is_ok() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("clipboard busy");
}

/// The formats on the clipboard and its Unicode text, if any.
fn snapshot(owner: HWND) -> (Vec<u32>, Option<String>) {
    open(owner);
    let mut formats = Vec::new();
    let mut f = 0;
    // SAFETY: the clipboard is open on this thread; the text handle is only read while
    // locked, up to its terminating NUL.
    unsafe {
        loop {
            f = EnumClipboardFormats(f);
            if f == 0 {
                break;
            }
            formats.push(f);
        }
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
        (formats, text)
    }
}

#[test]
#[ignore = "overwrites the user's clipboard"]
fn set_text_roundtrip() {
    // SAFETY: a message-only STATIC window owned by this test, destroyed below.
    let owner = unsafe {
        CreateWindowExW(
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
    }
    .expect("owner window");
    let (formats, saved) = snapshot(owner);
    let restorable = formats
        .iter()
        .all(|f| matches!(*f, CF_TEXT | CF_OEMTEXT | CF_UNICODETEXT | CF_LOCALE));
    if !restorable {
        println!("skipped: the clipboard holds formats {formats:?} that could not be restored");
        // SAFETY: our own window.
        unsafe { DestroyWindow(owner) }.ok();
        return;
    }
    let sample = "Vitrobar è 日本 \"quoted\" & 100%";
    let result = set_text(owner, sample);
    let (_, got) = snapshot(owner);
    // Restore before asserting, so a failure still leaves the user's clipboard as it was.
    match &saved {
        Some(s) => set_text(owner, s).expect("restore previous text"),
        None => {
            open(owner);
            // SAFETY: the clipboard is open on this thread.
            unsafe {
                let _ = EmptyClipboard();
                let _ = CloseClipboard();
            }
        }
    }
    assert_eq!(snapshot(owner).1, saved, "previous clipboard restored");
    // SAFETY: our own window.
    unsafe { DestroyWindow(owner) }.ok();
    result.expect("set_text");
    assert_eq!(got.as_deref(), Some(sample));
}
