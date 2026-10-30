//! Virtual desktop membership through the documented `IVirtualDesktopManager`.

use std::cell::Cell;

use windows::core::{Result, GUID};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::{IVirtualDesktopManager, VirtualDesktopManager};
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

pub struct Desktops {
    mgr: IVirtualDesktopManager,
    current: Cell<Option<GUID>>,
}

impl Desktops {
    /// Initialises COM (apartment) on the calling thread if needed and creates the manager.
    pub fn new() -> Result<Self> {
        // SAFETY: plain COM calls; an already-initialised or changed-mode result is fine,
        // CoCreateInstance reports real problems.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let mgr = CoCreateInstance(&VirtualDesktopManager, None, CLSCTX_ALL)?;
            Ok(Self {
                mgr,
                current: Cell::new(None),
            })
        }
    }

    /// Cold cache: the foreground window is on the current desktop whenever the shell
    /// confirms it, which gives a reference id without touching any other window.
    fn learn_from_foreground(&self) {
        // SAFETY: read-only queries on a handle that is only passed through.
        unsafe {
            let fg = GetForegroundWindow();
            if fg.0.is_null() {
                return;
            }
            if matches!(self.mgr.IsWindowOnCurrentVirtualDesktop(fg), Ok(b) if b.as_bool()) {
                if let Ok(id) = self.mgr.GetWindowDesktopId(fg) {
                    if id != GUID::zeroed() {
                        self.current.set(Some(id));
                    }
                }
            }
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
        unsafe {
            match self.mgr.IsWindowOnCurrentVirtualDesktop(hwnd) {
                Ok(b) if b.as_bool() => {
                    if let Ok(id) = self.mgr.GetWindowDesktopId(hwnd) {
                        if id != GUID::zeroed() {
                            self.current.set(Some(id));
                        }
                    }
                    true
                }
                Ok(_) => match self.mgr.GetWindowDesktopId(hwnd) {
                    Ok(id) if id == GUID::zeroed() => true,
                    Ok(id) => {
                        if self.current.get().is_none() {
                            self.learn_from_foreground();
                        }
                        self.current.get() == Some(id)
                    }
                    Err(_) => true,
                },
                Err(_) => true,
            }
        }
    }
}
