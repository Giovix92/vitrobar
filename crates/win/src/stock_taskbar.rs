//! Taking over the stock taskbar, and giving it back exactly as it was.
//!
//! Vitrobar never changes Windows permanently, but switching the stock taskbar to auto-hide
//! (`ABM_SETSTATE`) is persisted by Explorer. So before anything changes, the original state
//! goes into a restore record (`%LOCALAPPDATA%\Vitrobar\restore.json`) and a
//! `HKCU\...\RunOnce\VitrobarRestore = "<exe>" --restore` value is set, so that even a crash
//! or a power cut is undone at the next sign-in.
//!
//! Invariant: while a record exists, so does the RunOnce value (a restore that cannot finish
//! keeps both, and puts the RunOnce value back if Windows already consumed it).

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use vitro_core::settings::StockTaskbar;
use windows::core::{w, Error, Result, BOOL, HSTRING, PCWSTR};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, E_FAIL, HANDLE, HWND, LPARAM, WAIT_ABANDONED,
    WAIT_OBJECT_0,
};
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};
use windows::Win32::System::Threading::{
    CreateMutexW, GetCurrentProcessId, ReleaseMutex, WaitForSingleObject,
};
use windows::Win32::UI::Shell::{
    SHAppBarMessage, ABM_GETSTATE, ABM_SETSTATE, ABS_AUTOHIDE, APPBARDATA,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowExW, GetWindowThreadProcessId, IsWindowVisible, RegisterWindowMessageW,
    ShowWindowAsync, SW_HIDE, SW_SHOWNA,
};

use crate::window_props::class_name;

const RUNONCE_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\RunOnce");
const RUNONCE_VALUE: PCWSTR = w!("VitrobarRestore");

/// How long a take-over or a restore waits for another process's one to finish.
pub const SHELL_LOCK_TIMEOUT_MS: u32 = 10_000;

/// `Local\Vitrobar.Shell`: serializes take-overs and restores across every Vitrobar process
/// (the bar, its watchdog, a RunOnce `--restore`), so that the record, the RunOnce value and
/// the auto-hide state always change together. A Win32 mutex is recursive for its owning
/// thread: a caller that must check something under the lock (is a bar running?) holds it
/// around the call, and the call takes it again without blocking. Released when dropped, on
/// the thread that took it.
pub struct ShellLock(HANDLE);

impl ShellLock {
    /// `None` when another process held it for longer than `timeout_ms` (or the mutex could
    /// not be created). A mutex left by a process that died owning it is taken over.
    pub fn acquire(timeout_ms: u32) -> Option<Self> {
        // SAFETY: plain calls with a static name; the handle is owned by the guard (or
        // closed here when the wait fails).
        unsafe {
            let h = CreateMutexW(None, false, w!(r"Local\Vitrobar.Shell")).ok()?;
            let r = WaitForSingleObject(h, timeout_ms);
            if r == WAIT_OBJECT_0 || r == WAIT_ABANDONED {
                Some(Self(h))
            } else {
                let _ = CloseHandle(h);
                None
            }
        }
    }
}

impl Drop for ShellLock {
    fn drop(&mut self) {
        // SAFETY: the mutex is owned by this thread (taken in `acquire`), released once.
        unsafe {
            let _ = ReleaseMutex(self.0);
            let _ = CloseHandle(self.0);
        }
    }
}

/// What the stock taskbar looked like before Vitrobar took it over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestoreRecord {
    pub original_autohide: bool,
}

impl RestoreRecord {
    pub fn to_json(&self) -> String {
        format!("{{\"original_autohide\":{}}}\n", self.original_autohide)
    }

    pub fn from_json(s: &str) -> Option<Self> {
        let compact: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        let body = compact.strip_prefix('{')?.strip_suffix('}')?;
        let value = body.strip_prefix("\"original_autohide\":")?;
        let original_autohide = match value.split(',').next()? {
            "true" => true,
            "false" => false,
            _ => return None,
        };
        Some(Self { original_autohide })
    }

    /// `Ok(None)` when there is no record; `InvalidData` when it cannot be parsed.
    pub fn read(path: &Path) -> io::Result<Option<Self>> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::from_json(&text).map(Some).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "unreadable restore record")
            }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Writes to a temporary file next to `path`, flushes it, then renames it over `path`.
    pub fn write(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(self.to_json().as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, path)
    }
}

/// `%LOCALAPPDATA%\Vitrobar\restore.json`.
pub fn default_record_path() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Vitrobar").join("restore.json"))
}

fn io_err(e: io::Error) -> Error {
    Error::new(E_FAIL, e.to_string())
}

fn appbar_state() -> u32 {
    let mut abd = APPBARDATA {
        cbSize: size_of::<APPBARDATA>() as u32,
        ..Default::default()
    };
    // SAFETY: `abd` is a valid, sized APPBARDATA for the duration of the call.
    unsafe { SHAppBarMessage(ABM_GETSTATE, &mut abd) as u32 }
}

/// The stock taskbar's auto-hide setting right now.
pub fn autohide_now() -> bool {
    appbar_state() & ABS_AUTOHIDE != 0
}

fn set_autohide(on: bool) -> Result<()> {
    let cur = appbar_state();
    let want = if on {
        cur | ABS_AUTOHIDE
    } else {
        cur & !ABS_AUTOHIDE
    };
    if cur != want {
        let mut abd = APPBARDATA {
            cbSize: size_of::<APPBARDATA>() as u32,
            lParam: LPARAM(want as isize),
            ..Default::default()
        };
        // SAFETY: as in `appbar_state`; ABM_SETSTATE reads cbSize and lParam only.
        unsafe { SHAppBarMessage(ABM_SETSTATE, &mut abd) };
    }
    if autohide_now() != on {
        return Err(Error::new(E_FAIL, "the stock taskbar ignored ABM_SETSTATE"));
    }
    Ok(())
}

/// Explorer's main taskbar window, if Explorer is running. Vitrobar's own tray host also
/// has a window of class `Shell_TrayWnd` (see [`crate::tray_host`]): windows of this
/// process are skipped.
pub fn explorer_tray_window() -> Option<HWND> {
    // SAFETY: plain query.
    let own = unsafe { GetCurrentProcessId() };
    let mut after: Option<HWND> = None;
    // Bounded: there are only ever a handful of windows of this class.
    for _ in 0..64 {
        // SAFETY: plain query with static strings; `after` is a window returned by the
        // previous call (or none).
        let h = unsafe { FindWindowExW(None, after, w!("Shell_TrayWnd"), PCWSTR::null()) }
            .ok()
            .filter(|h| !h.0.is_null())?;
        let mut pid = 0u32;
        // SAFETY: out-pointer to a local; the call does not message the window.
        unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
        if pid != own {
            return Some(h);
        }
        after = Some(h);
    }
    None
}

fn primary_tray() -> Option<HWND> {
    explorer_tray_window()
}

unsafe extern "system" fn collect_secondary(hwnd: HWND, data: LPARAM) -> BOOL {
    // SAFETY: `data` is the `&mut Vec<HWND>` passed by `tray_windows`, alive for the call.
    let list = unsafe { &mut *(data.0 as *mut Vec<HWND>) };
    if class_name(hwnd) == "Shell_SecondaryTrayWnd" {
        list.push(hwnd);
    }
    true.into()
}

/// `Shell_TrayWnd` and every `Shell_SecondaryTrayWnd` (one per extra monitor).
fn tray_windows() -> Vec<HWND> {
    let mut list: Vec<HWND> = primary_tray().into_iter().collect();
    // SAFETY: the callback only touches the Vec passed through lParam, which outlives the call.
    unsafe {
        let _ = EnumWindows(
            Some(collect_secondary),
            LPARAM(&mut list as *mut Vec<HWND> as isize),
        );
    }
    list
}

/// Explorer's taskbar windows now: its `Shell_TrayWnd` and every `Shell_SecondaryTrayWnd`.
pub fn stock_taskbar_windows() -> Vec<HWND> {
    tray_windows()
}

/// Explorer is running and has its taskbar window.
pub fn stock_taskbar_present() -> bool {
    primary_tray().is_some()
}

/// The stock taskbar's main window is visible.
pub fn stock_taskbar_visible() -> bool {
    // SAFETY: plain query.
    primary_tray().is_some_and(|h| unsafe { IsWindowVisible(h) }.as_bool())
}

fn set_tray_visible(show: bool) {
    for h in tray_windows() {
        // SAFETY: plain calls. Async, so a hung Explorer can never block the caller.
        unsafe {
            // A show is always posted: a hide posted earlier (by a bar that just died, say)
            // may still be waiting in Explorer's queue while the window reads as visible,
            // and would land after a skipped show. Explorer handles posts in order.
            if show || IsWindowVisible(h).as_bool() {
                let _ = ShowWindowAsync(h, if show { SW_SHOWNA } else { SW_HIDE });
            }
        }
    }
}

/// Shows every Explorer taskbar window, whether or not a restore record exists. A Vitrobar
/// that hid them calls this on its way out even when another instance (or a RunOnce
/// `--restore`) already consumed the record.
pub fn show_stock_taskbar() {
    set_tray_visible(true);
}

fn runonce_command() -> Result<HSTRING> {
    let exe = std::env::current_exe().map_err(io_err)?;
    Ok(HSTRING::from(format!("\"{}\" --restore", exe.display())))
}

fn set_runonce() -> Result<()> {
    let cmd = runonce_command()?;
    let bytes = (cmd.len() + 1) * 2;
    // SAFETY: `cmd` is NUL-terminated and `bytes` covers its characters plus the NUL.
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            RUNONCE_KEY,
            RUNONCE_VALUE,
            REG_SZ.0,
            Some(cmd.as_ptr() as *const _),
            bytes as u32,
        )
    }
    .ok()
}

/// Deletes the `RunOnce\VitrobarRestore` value (absent is fine). Only for paths that also
/// drop the record (the restore) and for test teardown.
pub fn delete_runonce() -> Result<()> {
    // SAFETY: plain call with static strings.
    let r = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUNONCE_KEY, RUNONCE_VALUE) };
    if r == ERROR_SUCCESS || r == ERROR_FILE_NOT_FOUND {
        Ok(())
    } else {
        r.ok()
    }
}

/// Puts the `RunOnce\VitrobarRestore` value back when a restore record exists; returns
/// whether it did. Windows deletes a RunOnce value *before* it runs the command, so a
/// `--restore` that finds a live bar (and therefore leaves the record to it) calls this to
/// keep the invariant "record exists => RunOnce exists". Holds [`ShellLock`] around the
/// check and the write (recursive, so a caller may already hold it); on a lock timeout it
/// writes anyway: an extra safety net is always the safe direction.
pub fn rearm_runonce_if_record(record_path: &Path) -> Result<bool> {
    let _lock = ShellLock::acquire(SHELL_LOCK_TIMEOUT_MS);
    if !record_path.exists() {
        return Ok(false);
    }
    set_runonce()?;
    Ok(true)
}

/// The `RunOnce\VitrobarRestore` value exists.
pub fn runonce_exists() -> bool {
    // SAFETY: a size-only query (no data buffer).
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUNONCE_KEY,
            RUNONCE_VALUE,
            RRF_RT_REG_SZ,
            None,
            None,
            None,
        )
    };
    r == ERROR_SUCCESS
}

/// Takes over the stock taskbar: (1) records the current auto-hide state (a record left by a
/// run that never restored is kept: it holds the real original), (2) sets the RunOnce
/// safety net, (3) turns auto-hide on, (4) in `Hidden` mode hides every taskbar window (in
/// `AutoHide` mode shows them, in case a `Hidden` run that crashed left them hidden).
///
/// Holds [`ShellLock`]; gives up (changing nothing) if it cannot be had in
/// [`SHELL_LOCK_TIMEOUT_MS`].
pub fn take_over_stock_taskbar(mode: StockTaskbar, record_path: &Path) -> Result<()> {
    let Some(_lock) = ShellLock::acquire(SHELL_LOCK_TIMEOUT_MS) else {
        return Err(Error::new(
            E_FAIL,
            "another Vitrobar process is changing the stock taskbar",
        ));
    };
    if !stock_taskbar_present() {
        return Err(Error::new(E_FAIL, "the stock taskbar is not running"));
    }
    let original = match RestoreRecord::read(record_path) {
        Ok(Some(_)) => None,
        Ok(None) => Some(autohide_now()),
        // A record that cannot be read was left by a run that never restored: the current
        // auto-hide state is most likely Vitrobar's own "on", so it is no evidence of the
        // original. Record the Windows default instead.
        Err(e) if e.kind() == io::ErrorKind::InvalidData => Some(false),
        Err(e) => return Err(io_err(e)),
    };
    if let Some(original_autohide) = original {
        RestoreRecord { original_autohide }
            .write(record_path)
            .map_err(io_err)?;
    }
    set_runonce()?;
    set_autohide(true)?;
    set_tray_visible(mode == StockTaskbar::AutoHide);
    Ok(())
}

/// Hides the stock taskbar again after Explorer showed it (restart, display or settings
/// change). Nothing to do in `AutoHide` mode.
pub fn reassert_hidden(mode: StockTaskbar) {
    if mode == StockTaskbar::Hidden {
        set_tray_visible(false);
    }
}

/// Undoes [`take_over_stock_taskbar`] in reverse order: shows the taskbar, puts auto-hide
/// back, deletes the RunOnce value, deletes the record. Idempotent: nothing to do without a
/// record. If auto-hide cannot be put back (Explorer not running), the record and the
/// RunOnce value stay so the next sign-in finishes the job.
///
/// Holds [`ShellLock`]; restores anyway if it cannot be had in [`SHELL_LOCK_TIMEOUT_MS`]
/// (giving the taskbar back is always the safe direction).
pub fn restore_stock_taskbar(record_path: &Path) -> Result<()> {
    restore_stock_taskbar_within(record_path, SHELL_LOCK_TIMEOUT_MS)
}

/// [`restore_stock_taskbar`] with a shorter wait for the lock (the panic hook).
pub fn restore_stock_taskbar_within(record_path: &Path, lock_timeout_ms: u32) -> Result<()> {
    let _lock = ShellLock::acquire(lock_timeout_ms);
    let record = match RestoreRecord::read(record_path) {
        Ok(None) => return Ok(()),
        Ok(Some(r)) => Some(r),
        Err(e) if e.kind() == io::ErrorKind::InvalidData => None,
        Err(e) => return Err(io_err(e)),
    };
    set_tray_visible(true);
    if let Some(r) = record {
        let back = if stock_taskbar_present() {
            set_autohide(r.original_autohide)
        } else {
            Err(Error::new(E_FAIL, "the stock taskbar is not running"))
        };
        if let Err(e) = back {
            let _ = set_runonce();
            return Err(e);
        }
        // Explorer handles the (sent) auto-hide change before the (posted) show above; show
        // again in case the change re-hid a window that was still hidden.
        set_tray_visible(true);
    }
    delete_runonce()?;
    match std::fs::remove_file(record_path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(io_err(e)),
    }
    if record.is_none() {
        // The taskbar is visible again, but its original auto-hide setting is unknown.
        return Err(Error::new(
            E_FAIL,
            "the restore record was unreadable; auto-hide left as it is",
        ));
    }
    Ok(())
}

/// `RegisterWindowMessageW("TaskbarCreated")`: broadcast when Explorer (re)creates the
/// taskbar.
pub fn taskbar_created_msg() -> u32 {
    static ID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    // SAFETY: plain registration call with a static string.
    *ID.get_or_init(|| unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_round_trip() {
        for v in [true, false] {
            let r = RestoreRecord {
                original_autohide: v,
            };
            assert_eq!(RestoreRecord::from_json(&r.to_json()), Some(r));
        }
        assert_eq!(
            RestoreRecord::from_json(" { \"original_autohide\" : true } "),
            Some(RestoreRecord {
                original_autohide: true
            })
        );
        assert_eq!(RestoreRecord::from_json(""), None);
        assert_eq!(RestoreRecord::from_json("{\"original_autohide\":1}"), None);
        assert_eq!(RestoreRecord::from_json("{\"original_autohide\":tr"), None);
    }

    #[test]
    fn record_file_round_trip_and_missing() {
        let dir = std::env::temp_dir().join(format!("vitrobar-rec-{}", std::process::id()));
        let p = dir.join("sub").join("restore.json");
        let _ = std::fs::remove_file(&p);
        assert_eq!(RestoreRecord::read(&p).unwrap(), None);
        let r = RestoreRecord {
            original_autohide: true,
        };
        r.write(&p).unwrap();
        assert_eq!(RestoreRecord::read(&p).unwrap(), Some(r));
        assert!(!p.with_extension("json.tmp").exists());
        std::fs::write(&p, "garbage").unwrap();
        assert_eq!(
            RestoreRecord::read(&p).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn taskbar_created_is_registered() {
        let id = taskbar_created_msg();
        assert!(id >= 0xC000);
        assert_eq!(id, taskbar_created_msg());
    }
}
