//! The input language for the bar's indicator: how many keyboard layouts the user has, the
//! foreground window's current one, and switching it (ruling S3-4: the same request the
//! language hotkey makes, posted to the foreground window).

use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::Globalization::LCIDToLocaleName;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyboardLayout, GetKeyboardLayoutList};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowThreadProcessId, PostMessageW, INPUTLANGCHANGE_FORWARD,
    WM_INPUTLANGCHANGEREQUEST,
};

/// Number of keyboard layouts installed for the user.
pub fn layout_count() -> usize {
    // SAFETY: a null buffer asks for the count only.
    usize::try_from(unsafe { GetKeyboardLayoutList(None) }).unwrap_or(0)
}

/// The language id and locale name of the layout `thread` uses (0: this thread).
pub fn thread_language(thread: u32) -> (u16, String) {
    // SAFETY: plain query.
    let hkl = unsafe { GetKeyboardLayout(thread) };
    let langid = (hkl.0 as usize & 0xFFFF) as u16;
    let mut buf = [0u16; 85];
    // SAFETY: the buffer is passed with its length; flags 0 is the documented default.
    let n = unsafe { LCIDToLocaleName(u32::from(langid), Some(&mut buf), 0) };
    let len = (n.max(1) as usize - 1).min(buf.len());
    (langid, String::from_utf16_lossy(&buf[..len]))
}

/// The foreground window's input language; `None` without a foreground window.
pub fn foreground_language() -> Option<(u16, String)> {
    // SAFETY: plain queries.
    let thread = unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            return None;
        }
        GetWindowThreadProcessId(fg, None)
    };
    (thread != 0).then(|| thread_language(thread))
}

/// Asks the foreground window to move to the next keyboard layout.
pub fn next_layout_for_foreground() {
    // SAFETY: plain query and a post with plain values.
    unsafe {
        let fg = GetForegroundWindow();
        if !fg.0.is_null() {
            let _ = PostMessageW(
                Some(fg),
                WM_INPUTLANGCHANGEREQUEST,
                WPARAM(INPUTLANGCHANGE_FORWARD as usize),
                LPARAM(0),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_layouts_without_changing_them() {
        assert!(layout_count() >= 1);
        let (langid, name) = thread_language(0);
        assert_ne!(langid, 0);
        assert!(!name.is_empty());
    }
}
