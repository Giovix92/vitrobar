//! The signed-in user, as the Start panel's account button shows it.

use windows::core::PWSTR;
use windows::Win32::Security::Authentication::Identity::{GetUserNameExW, NameDisplay};
use windows::Win32::System::WindowsProgramming::GetUserNameW;

/// The user's display name ("Ann Smith"); for accounts without one (most local accounts),
/// the logon name. Empty only if both lookups fail.
pub fn display_name() -> String {
    full_name()
        .filter(|s| !s.trim().is_empty())
        .or_else(logon_name)
        .unwrap_or_default()
}

fn full_name() -> Option<String> {
    let mut len = 0u32;
    // SAFETY: size query: no buffer, `len` receives the size needed (with the NUL).
    unsafe { GetUserNameExW(NameDisplay, None, &mut len) };
    if len == 0 || len > 4096 {
        return None;
    }
    let mut buf = vec![0u16; len as usize];
    // SAFETY: `buf` holds `len` units, as passed alongside.
    let ok = unsafe { GetUserNameExW(NameDisplay, Some(PWSTR(buf.as_mut_ptr())), &mut len) };
    if !ok {
        return None;
    }
    // On success `len` is the length without the NUL.
    Some(String::from_utf16_lossy(
        &buf[..(len as usize).min(buf.len())],
    ))
}

fn logon_name() -> Option<String> {
    // UNLEN (256) + NUL.
    let mut buf = [0u16; 257];
    let mut len = buf.len() as u32;
    // SAFETY: `buf` holds `len` units; on success `len` counts the copied units with the NUL.
    unsafe { GetUserNameW(Some(PWSTR(buf.as_mut_ptr())), &mut len) }.ok()?;
    let n = buf.iter().position(|&u| u == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..n])).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_name_is_not_empty() {
        let n = display_name();
        assert!(!n.trim().is_empty());
        assert_eq!(n, n.trim_end_matches('\0'));
    }
}
