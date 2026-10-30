//! Virtual desktop membership through the documented `IVirtualDesktopManager`.

use windows::core::Result;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::{IVirtualDesktopManager, VirtualDesktopManager};

pub struct Desktops {
    mgr: IVirtualDesktopManager,
}

impl Desktops {
    /// Initialises COM (apartment) on the calling thread if needed and creates the manager.
    pub fn new() -> Result<Self> {
        // SAFETY: plain COM calls; an already-initialised or changed-mode result is fine,
        // CoCreateInstance reports real problems.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let mgr = CoCreateInstance(&VirtualDesktopManager, None, CLSCTX_ALL)?;
            Ok(Self { mgr })
        }
    }

    /// True when the window is on the current virtual desktop. Fails open: any API error
    /// reports `true`, so a window is never hidden because of a failed query.
    pub fn is_on_current(&self, hwnd: HWND) -> bool {
        // SAFETY: `hwnd` is only passed through; the API validates it.
        unsafe { self.mgr.IsWindowOnCurrentVirtualDesktop(hwnd) }
            .map(|b| b.as_bool())
            .unwrap_or(true)
    }
}
