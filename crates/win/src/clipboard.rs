//! Copying text (a quick-math result, a path) to the clipboard.

use std::time::Duration;

use windows::core::{Error, Result};
use windows::Win32::Foundation::{GlobalFree, HANDLE, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};

/// `CF_UNICODETEXT` (a standard clipboard format; its constant lives in the large OLE module).
const CF_UNICODETEXT: u32 = 13;
/// Another program may hold the clipboard open for a moment.
const OPEN_RETRIES: usize = 5;
const RETRY_DELAY: Duration = Duration::from_millis(10);

/// Closes the clipboard on drop.
struct Opened;

impl Drop for Opened {
    fn drop(&mut self) {
        // SAFETY: this thread opened the clipboard.
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

fn open(owner: HWND) -> Result<Opened> {
    let mut attempt = 0;
    loop {
        // SAFETY: plain call; balanced by `Opened`'s drop.
        match unsafe { OpenClipboard(Some(owner)) } {
            Ok(()) => return Ok(Opened),
            Err(e) if attempt >= OPEN_RETRIES => return Err(e),
            Err(_) => {
                attempt += 1;
                std::thread::sleep(RETRY_DELAY);
            }
        }
    }
}

/// Replaces the clipboard's contents with `text`, owned by `owner` (a window of this
/// process). Fails if the clipboard stays busy for ~50 ms.
pub fn set_text(owner: HWND, text: &str) -> Result<()> {
    let units: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let _open = open(owner)?;
    // SAFETY: the clipboard is open on this thread. The block holds `units.len()` u16s and
    // is written only while locked. Once SetClipboardData succeeds the system owns it;
    // before that it is freed here on every failure.
    unsafe {
        EmptyClipboard()?;
        let mem = GlobalAlloc(GMEM_MOVEABLE, units.len() * size_of::<u16>())?;
        let p = GlobalLock(mem).cast::<u16>();
        if p.is_null() {
            let e = Error::from_thread();
            let _ = GlobalFree(Some(mem));
            return Err(e);
        }
        std::ptr::copy_nonoverlapping(units.as_ptr(), p, units.len());
        let _ = GlobalUnlock(mem);
        if let Err(e) = SetClipboardData(CF_UNICODETEXT, Some(HANDLE(mem.0))) {
            let _ = GlobalFree(Some(mem));
            return Err(e);
        }
    }
    Ok(())
}
