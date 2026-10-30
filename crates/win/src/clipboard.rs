//! Copying text (a quick-math result, a path) to the clipboard.

use std::time::Duration;

use windows::core::{Error, Result};
use windows::Win32::Foundation::{GlobalFree, E_INVALIDARG, HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
};

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
/// process; a null owner is refused). Fails if the clipboard stays busy for ~50 ms. The text
/// is copied into its memory block before the clipboard is opened and emptied, so a failure
/// to allocate never leaves the user's clipboard empty.
pub fn set_text(owner: HWND, text: &str) -> Result<()> {
    if owner.0.is_null() {
        return Err(Error::from(E_INVALIDARG));
    }
    let units: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: the block holds `units.len()` u16s and is written only while locked. It is
    // freed here on every failure until SetClipboardData succeeds; then the system owns it.
    let mem = unsafe {
        let mem = GlobalAlloc(GMEM_MOVEABLE, units.len() * size_of::<u16>())?;
        let p = GlobalLock(mem).cast::<u16>();
        if p.is_null() {
            let e = Error::from_thread();
            let _ = GlobalFree(Some(mem));
            return Err(e);
        }
        std::ptr::copy_nonoverlapping(units.as_ptr(), p, units.len());
        let _ = GlobalUnlock(mem);
        mem
    };
    let free = |e: Error| {
        // SAFETY: the block was not handed to the clipboard.
        unsafe {
            let _ = GlobalFree(Some(mem));
        }
        e
    };
    let _open = open(owner).map_err(free)?;
    // SAFETY: the clipboard is open on this thread; `mem` is a filled, unlocked block.
    unsafe {
        EmptyClipboard().map_err(free)?;
        SetClipboardData(CF_UNICODETEXT, Some(HANDLE(mem.0))).map_err(free)?;
    }
    Ok(())
}

/// The clipboard's Unicode text (for Ctrl+V into the Start search box); `None` when it holds
/// no text or stays busy. At most `max_units` UTF-16 units are read.
pub fn get_text(owner: HWND, max_units: usize) -> Option<String> {
    let _open = open(owner).ok()?;
    // SAFETY: the clipboard is open on this thread; the block is only read while locked,
    // never past its size or its terminating NUL.
    unsafe {
        let h = GetClipboardData(CF_UNICODETEXT).ok()?;
        let g = HGLOBAL(h.0);
        let p = GlobalLock(g) as *const u16;
        if p.is_null() {
            return None;
        }
        let cap = (GlobalSize(g) / size_of::<u16>()).min(max_units);
        let slice = std::slice::from_raw_parts(p, cap);
        let n = slice.iter().position(|&u| u == 0).unwrap_or(cap);
        let s = String::from_utf16_lossy(&slice[..n]);
        let _ = GlobalUnlock(g);
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_null_owner_is_refused_without_touching_the_clipboard() {
        let e = set_text(HWND::default(), "x").unwrap_err();
        assert_eq!(e.code(), E_INVALIDARG);
    }
}
