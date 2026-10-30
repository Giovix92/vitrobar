//! The app side of the stock taskbar takeover: a guard that takes it over at start-up and
//! gives it back on every way out (normal quit, `WM_CLOSE`, Ctrl+C, an error from `run`, a
//! panic unwinding through `run`, the end of the session).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::Mutex;

use vitro_core::settings::StockTaskbar;
use vitro_win::appbar::unregister;
use vitro_win::stock_taskbar::{
    default_record_path, reassert_hidden, restore_stock_taskbar, take_over_stock_taskbar,
};
use windows::Win32::Foundation::HWND;

/// The registered appbar window (0 = none), for paths that cannot reach the `AppBar` value.
static APPBAR_HWND: AtomicIsize = AtomicIsize::new(0);
/// Set once [`restore_now`] ran: no later event takes the taskbar over again.
static RESTORED: AtomicBool = AtomicBool::new(false);
/// Serializes restores (the console handler runs on its own thread).
static RESTORE_LOCK: Mutex<()> = Mutex::new(());

pub fn record_path() -> Option<PathBuf> {
    default_record_path()
}

/// Remembers the window registered as the appbar (`None` once it is unregistered).
pub fn set_appbar_hwnd(hwnd: Option<HWND>) {
    APPBAR_HWND.store(hwnd.map_or(0, |h| h.0 as isize), Ordering::SeqCst);
}

/// Unregisters the appbar, then gives the stock taskbar back. Safe to call from any thread
/// and any number of times.
pub fn restore_now() {
    let _lock = RESTORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    RESTORED.store(true, Ordering::SeqCst);
    let h = APPBAR_HWND.swap(0, Ordering::SeqCst);
    if h != 0 {
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
}

impl StockGuard {
    pub fn take_over(mode: StockTaskbar) -> Self {
        let mut g = Self {
            mode,
            active: false,
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
            Ok(()) => self.active = true,
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
    pub fn reassert(&self) {
        if self.active && !RESTORED.load(Ordering::SeqCst) {
            reassert_hidden(self.mode);
        }
    }
}

impl Drop for StockGuard {
    fn drop(&mut self) {
        restore_now();
    }
}
