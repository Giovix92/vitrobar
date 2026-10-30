//! The app side of the stock taskbar takeover: a guard that takes it over at start-up and
//! gives it back on every way out (normal quit, `WM_CLOSE`, Ctrl+C, an error from `run`, a
//! panic unwinding through `run`, the end of the session).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use vitro_core::settings::StockTaskbar;
use vitro_win::appbar::unregister;
use vitro_win::stock_taskbar::{
    default_record_path, reassert_hidden, restore_stock_taskbar, stock_taskbar_windows,
    take_over_stock_taskbar,
};
use windows::Win32::Foundation::HWND;

/// The windows registered as appbars (one per monitor's bar), for paths that cannot reach
/// the `AppBar` values.
static APPBAR_HWNDS: Mutex<Vec<isize>> = Mutex::new(Vec::new());
/// Set once [`restore_now`] ran: no later event takes the taskbar over again.
static RESTORED: AtomicBool = AtomicBool::new(false);
/// Serializes restores (the console handler runs on its own thread).
static RESTORE_LOCK: Mutex<()> = Mutex::new(());

pub fn record_path() -> Option<PathBuf> {
    default_record_path()
}

fn appbar_hwnds() -> std::sync::MutexGuard<'static, Vec<isize>> {
    APPBAR_HWNDS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Remembers a window just registered as an appbar.
pub fn add_appbar_hwnd(hwnd: HWND) {
    let mut list = appbar_hwnds();
    if !list.contains(&(hwnd.0 as isize)) {
        list.push(hwnd.0 as isize);
    }
}

/// Forgets a window whose appbar was unregistered.
pub fn remove_appbar_hwnd(hwnd: HWND) {
    appbar_hwnds().retain(|&h| h != hwnd.0 as isize);
}

/// Destroys Vitrobar's `Shell_TrayWnd` (and asks every app to register its tray icons with
/// Explorer again), unregisters the appbars, then gives the stock taskbar back. Safe to call
/// from any thread and any number of times.
pub fn restore_now() {
    let _lock = RESTORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    RESTORED.store(true, Ordering::SeqCst);
    // First: while our tray window lives, the appbar and auto-hide calls below pass through
    // it (it forwards them); afterwards they go straight to Explorer.
    vitro_win::tray_host::shutdown_global();
    let hwnds = std::mem::take(&mut *appbar_hwnds());
    for h in hwnds {
        unregister(HWND(h as *mut _));
    }
    if let Some(p) = record_path() {
        if let Err(e) = restore_stock_taskbar(&p) {
            eprintln!("vitrobar: could not restore the stock taskbar: {e}");
        }
    }
}

/// Owns the takeover: restores the stock taskbar when dropped.
pub struct StockGuard {
    mode: StockTaskbar,
    active: bool,
    /// Explorer's taskbar windows as of the last takeover or reassert (raw handles).
    windows: Vec<isize>,
}

impl StockGuard {
    pub fn take_over(mode: StockTaskbar) -> Self {
        let mut g = Self {
            mode,
            active: false,
            windows: Vec::new(),
        };
        g.try_take_over();
        g
    }

    fn try_take_over(&mut self) {
        if RESTORED.load(Ordering::SeqCst) {
            return;
        }
        let Some(p) = record_path() else {
            eprintln!("vitrobar: LOCALAPPDATA is not set; the stock taskbar stays");
            return;
        };
        match take_over_stock_taskbar(self.mode, &p) {
            Ok(()) => {
                self.active = true;
                self.remember_windows();
            }
            Err(e) => {
                eprintln!("vitrobar: could not take over the stock taskbar: {e}");
                // Undo whatever part of it happened.
                if let Err(e) = restore_stock_taskbar(&p) {
                    eprintln!("vitrobar: could not restore the stock taskbar: {e}");
                }
            }
        }
    }

    /// The stock taskbar is currently taken over.
    pub fn active(&self) -> bool {
        self.active && !RESTORED.load(Ordering::SeqCst)
    }

    /// Explorer restarted: hide its new taskbar (or take over now if Explorer was not
    /// running at start-up).
    pub fn on_taskbar_created(&mut self) {
        if self.active {
            self.reassert();
        } else {
            self.try_take_over();
        }
    }

    /// Hides the stock taskbar again if Explorer showed it.
    pub fn reassert(&mut self) {
        if self.active && !RESTORED.load(Ordering::SeqCst) {
            reassert_hidden(self.mode);
            self.remember_windows();
        }
    }

    fn remember_windows(&mut self) {
        self.windows = stock_taskbar_windows()
            .into_iter()
            .map(|h| h.0 as isize)
            .collect();
    }

    /// `hwnd` is one of Explorer's taskbar windows and they are kept hidden: when it moves
    /// (Explorer slides an auto-hide taskbar back in after the takeover, or after a display
    /// change) it must be hidden again.
    pub fn watches(&self, hwnd: u64) -> bool {
        self.active && self.mode == StockTaskbar::Hidden && self.windows.contains(&(hwnd as isize))
    }
}

impl Drop for StockGuard {
    fn drop(&mut self) {
        restore_now();
    }
}
