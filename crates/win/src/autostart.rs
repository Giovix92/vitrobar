//! "Start with Windows": the `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\Vitrobar`
//! value, which starts the watchdog (`"<exe>"`, no arguments) at sign-in.

use windows::core::{w, Error, Result, HSTRING, PCWSTR};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, E_FAIL};
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};
use windows::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, IDYES, MB_ICONQUESTION, MB_SETFOREGROUND, MB_TOPMOST, MB_YESNO,
};

/// Asks, with a plain Yes/No message box, whether to start with Windows (`question` is the
/// localized text). Blocks until answered; closing the box counts as "No", so nothing is
/// written to the Run key without a yes.
pub fn ask_start_with_windows(question: &str) -> bool {
    let text = HSTRING::from(question);
    // SAFETY: both strings outlive the call; no owner window.
    let r = unsafe {
        MessageBoxW(
            None,
            PCWSTR(text.as_ptr()),
            w!("Vitrobar"),
            MB_YESNO | MB_ICONQUESTION | MB_SETFOREGROUND | MB_TOPMOST,
        )
    };
    r == IDYES
}

const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const RUN_VALUE: PCWSTR = w!("Vitrobar");

/// The command the Run value holds: the quoted path of this executable.
pub fn autostart_command() -> Result<String> {
    let exe = std::env::current_exe().map_err(|e| Error::new(E_FAIL, e.to_string()))?;
    Ok(format!("\"{}\"", exe.display()))
}

/// Writes (`enabled`) or deletes the Run value. Writes only when the value differs.
pub fn set_autostart(enabled: bool) -> Result<()> {
    if enabled {
        let cmd = autostart_command()?;
        set_value(RUN_KEY, RUN_VALUE, &cmd)
    } else {
        delete_value(RUN_KEY, RUN_VALUE)
    }
}

/// The executable a Run command starts: the quoted path, or everything up to the first
/// space when it is not quoted.
fn command_exe(cmd: &str) -> &str {
    let cmd = cmd.trim();
    match cmd.strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or(rest),
        None => cmd.split(' ').next().unwrap_or(cmd),
    }
}

/// The Run value must be pointed at `current_exe`: it names another executable, and that
/// one no longer exists (Vitrobar was moved or updated to another folder).
fn is_stale(value: &str, current_exe: &str, exists: impl Fn(&str) -> bool) -> bool {
    let exe = command_exe(value);
    !exe.eq_ignore_ascii_case(current_exe) && !exists(exe)
}

/// At a normal start-up: corrects an existing Run value only when it names another
/// executable that no longer exists. Never creates the value (so a dev build started by hand
/// does not register itself); the menu toggle and the first run do that through
/// [`set_autostart`]. `Ok(true)` when the value was rewritten.
pub fn repair_autostart() -> Result<bool> {
    let Some(value) = autostart_value() else {
        return Ok(false);
    };
    let exe = std::env::current_exe().map_err(|e| Error::new(E_FAIL, e.to_string()))?;
    let current = exe.display().to_string();
    if !is_stale(&value, &current, |p| std::path::Path::new(p).exists()) {
        return Ok(false);
    }
    set_autostart(true)?;
    Ok(true)
}

/// The Run value as it is now (`None` when there is none).
pub fn autostart_value() -> Option<String> {
    get_value(RUN_KEY, RUN_VALUE)
}

fn get_value(key: PCWSTR, value: PCWSTR) -> Option<String> {
    let mut bytes = 0u32;
    // SAFETY: a size-only query (no data buffer).
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key,
            value,
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut bytes),
        )
    };
    if r != ERROR_SUCCESS {
        return None;
    }
    let mut buf = vec![0u16; (bytes as usize).div_ceil(2) + 1];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: `buf` holds `size` bytes, alive for the call.
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key,
            value,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut size),
        )
    };
    if r != ERROR_SUCCESS {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

fn set_value(key: PCWSTR, value: PCWSTR, data: &str) -> Result<()> {
    if get_value(key, value).as_deref() == Some(data) {
        return Ok(());
    }
    let h = HSTRING::from(data);
    let bytes = (h.len() + 1) * 2;
    // SAFETY: `h` is NUL-terminated and `bytes` covers its characters plus the NUL.
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            key,
            value,
            REG_SZ.0,
            Some(h.as_ptr() as *const _),
            bytes as u32,
        )
    }
    .ok()
}

fn delete_value(key: PCWSTR, value: PCWSTR) -> Result<()> {
    // SAFETY: plain call with NUL-terminated strings.
    let r = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key, value) };
    if r == ERROR_SUCCESS || r == ERROR_FILE_NOT_FOUND {
        Ok(())
    } else {
        r.ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Registry::RegDeleteTreeW;

    #[test]
    fn command_quotes_this_exe() {
        let c = autostart_command().unwrap();
        assert!(c.starts_with('"') && c.ends_with(".exe\""), "{c}");
        assert!(!c.contains("--"), "the watchdog takes no arguments: {c}");
    }

    #[test]
    fn stale_only_when_another_exe_is_gone() {
        let cur = r"C:\Apps\Vitrobar\vitrobar.exe";
        let gone = |_: &str| false;
        let there = |_: &str| true;
        // The same executable (any case): never stale, even if the check says missing.
        assert!(!is_stale(r#""C:\Apps\Vitrobar\vitrobar.exe""#, cur, gone));
        assert!(!is_stale(r#""c:\apps\vitrobar\VITROBAR.EXE""#, cur, gone));
        // Another executable that still exists (a dev build, another install): left alone.
        assert!(!is_stale(
            r#""D:\dev\target\release\vitrobar.exe""#,
            cur,
            there
        ));
        // Another executable that is gone: stale.
        assert!(is_stale(r#""D:\old\vitrobar.exe""#, cur, gone));
        assert!(is_stale(r"D:\old\vitrobar.exe --x", cur, gone));
        assert_eq!(command_exe(r#" "C:\a b\v.exe" --x"#), r"C:\a b\v.exe");
        assert_eq!(command_exe(r"C:\v.exe --x"), r"C:\v.exe");
    }

    #[test]
    fn value_round_trip_in_a_scratch_key() {
        // A key of its own, never the real Run key.
        let key = HSTRING::from(format!(
            r"Software\VitrobarTest\autostart-{}",
            std::process::id()
        ));
        let k = PCWSTR(key.as_ptr());
        let v = w!("Vitrobar");
        assert_eq!(get_value(k, v), None);
        set_value(k, v, r#""C:\x y\vitrobar.exe""#).unwrap();
        assert_eq!(get_value(k, v).as_deref(), Some(r#""C:\x y\vitrobar.exe""#));
        set_value(k, v, r#""C:\x y\vitrobar.exe""#).unwrap();
        delete_value(k, v).unwrap();
        assert_eq!(get_value(k, v), None);
        delete_value(k, v).unwrap();
        // SAFETY: deletes only the scratch key created above.
        unsafe {
            let _ = RegDeleteTreeW(HKEY_CURRENT_USER, k);
            let _ = windows::Win32::System::Registry::RegDeleteKeyW(
                HKEY_CURRENT_USER,
                w!(r"Software\VitrobarTest"),
            );
        }
    }
}
