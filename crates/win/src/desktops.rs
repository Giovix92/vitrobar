//! Virtual desktop membership through the documented `IVirtualDesktopManager`.

use std::cell::Cell;

use windows::core::{Result, GUID};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows::Win32::UI::Shell::{IVirtualDesktopManager, VirtualDesktopManager};
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

pub struct Desktops {
    mgr: IVirtualDesktopManager,
    current: Cell<Option<GUID>>,
}

impl Desktops {
    /// Creates the manager. The calling thread must already have initialised COM (the UI
    /// thread does, in `app::run`); this never initialises it, so there is nothing to balance.
    pub fn new() -> Result<Self> {
        Ok(Self {
            mgr: create_manager()?,
            current: Cell::new(None),
        })
    }

    /// Creates the manager again (Explorer restarted: the old proxy points at a process that
    /// is gone, and every query on it would fail open forever). On failure the old one stays.
    pub fn recreate(&mut self) -> Result<()> {
        self.mgr = create_manager()?;
        self.invalidate();
        Ok(())
    }

    /// Cold cache: learns the current desktop id from the foreground window, and only from
    /// it (other windows may be pinned to all desktops and report their home desktop).
    fn learn_from_foreground(&self) {
        // SAFETY: read-only queries on a handle that is only passed through.
        unsafe {
            let fg = GetForegroundWindow();
            if fg.0.is_null() {
                return;
            }
            let cur = self
                .mgr
                .IsWindowOnCurrentVirtualDesktop(fg)
                .map(|b| b.as_bool());
            self.current
                .set(learn(cur, self.mgr.GetWindowDesktopId(fg)));
        }
    }

    /// Forget the cached current-desktop id. Call before re-checking every window after a
    /// virtual desktop switch.
    pub fn invalidate(&self) {
        self.current.set(None);
    }

    /// True when the window is on the current virtual desktop. Fails open: any API error
    /// reports `true`, so a window is never hidden because of a failed query.
    ///
    /// A just-created window is assigned to a desktop by the shell asynchronously (30-60 ms,
    /// measured). In that gap `IsWindowOnCurrentVirtualDesktop` answers `false` while
    /// `GetWindowDesktopId` first returns `GUID_NULL` (or `TYPE_E_ELEMENTNOTFOUND`) and then
    /// the real id of the current desktop. A window on another desktop also answers `false`
    /// but with a different id. So `false` only counts when the window has a real id that
    /// differs from the current desktop id, which is learned from windows the shell
    /// confirms as current.
    pub fn is_on_current(&self, hwnd: HWND) -> bool {
        // SAFETY: `hwnd` is only passed through; the API validates it.
        let (cur, id) = unsafe {
            (
                self.mgr
                    .IsWindowOnCurrentVirtualDesktop(hwnd)
                    .map(|b| b.as_bool()),
                self.mgr.GetWindowDesktopId(hwnd),
            )
        };
        // Only a "false" with a real id needs the reference; the cache is never written
        // from the window being asked about.
        if matches!(cur, Ok(false))
            && matches!(&id, Ok(i) if *i != GUID::zeroed())
            && self.current.get().is_none()
        {
            self.learn_from_foreground();
        }
        decide(cur, id, self.current.get())
    }
}

fn create_manager() -> Result<IVirtualDesktopManager> {
    // SAFETY: plain COM activation on a thread that initialised COM.
    unsafe { CoCreateInstance(&VirtualDesktopManager, None, CLSCTX_ALL) }
}

/// The reference id a foreground window yields: only when the shell confirms it is on the
/// current desktop and it has a real id.
fn learn(is_current: Result<bool>, id: Result<GUID>) -> Option<GUID> {
    match (is_current, id) {
        (Ok(true), Ok(id)) if id != GUID::zeroed() => Some(id),
        _ => None,
    }
}

/// Pure decision for [`Desktops::is_on_current`]. Fails open everywhere the answer is not
/// certain: query errors, a window not yet assigned to a desktop (`GUID_NULL`), and an
/// unknown current desktop. Only a real id that differs from the known current id is "no".
fn decide(is_current: Result<bool>, window_id: Result<GUID>, cached: Option<GUID>) -> bool {
    match is_current {
        Err(_) | Ok(true) => true,
        Ok(false) => match (window_id, cached) {
            (Ok(id), Some(cur)) if id != GUID::zeroed() => id == cur,
            _ => true,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::core::{Error, HRESULT};

    const HOME: GUID = GUID::from_u128(0x1111);
    const OTHER: GUID = GUID::from_u128(0x2222);

    fn err<T>() -> Result<T> {
        Err(Error::from(HRESULT(-1)))
    }

    #[test]
    fn current_window_is_current() {
        assert!(decide(Ok(true), Ok(HOME), Some(HOME)));
    }

    #[test]
    fn cold_cache_fails_open() {
        assert!(decide(Ok(false), Ok(OTHER), None));
    }

    #[test]
    fn unassigned_window_is_current() {
        assert!(decide(Ok(false), Ok(GUID::zeroed()), Some(HOME)));
        assert!(decide(Ok(false), Ok(GUID::zeroed()), None));
    }

    #[test]
    fn other_desktop_is_not_current() {
        assert!(!decide(Ok(false), Ok(OTHER), Some(HOME)));
    }

    #[test]
    fn assigned_but_not_yet_current_matches_cache() {
        assert!(decide(Ok(false), Ok(HOME), Some(HOME)));
    }

    #[test]
    fn query_errors_fail_open() {
        assert!(decide(Ok(false), err(), Some(HOME)));
        assert!(decide(err(), Ok(OTHER), Some(HOME)));
        assert!(decide(err(), err(), None));
    }

    #[test]
    fn pinned_window_reporting_true_changes_nothing() {
        // A pinned window answers true with its home desktop id: decide is pure and
        // learning only ever comes from the foreground window, never from this answer.
        assert!(decide(Ok(true), Ok(OTHER), Some(HOME)));
        assert_eq!(learn(Ok(true), Ok(GUID::zeroed())), None);
        assert_eq!(learn(Ok(false), Ok(OTHER)), None);
        assert_eq!(learn(err(), Ok(OTHER)), None);
        assert_eq!(learn(Ok(true), err()), None);
        assert_eq!(learn(Ok(true), Ok(HOME)), Some(HOME));
    }
}
