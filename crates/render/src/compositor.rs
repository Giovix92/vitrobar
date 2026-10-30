//! Windows.UI.Composition compositor bound to the current (UI) thread.

use std::ops::Deref;

use windows::core::{Interface, Result};
use windows::System::DispatcherQueueController;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::Win32::System::WinRT::{
    CreateDispatcherQueueController, DispatcherQueueOptions, RoInitialize, RoUninitialize,
    DQTAT_COM_STA, DQTYPE_THREAD_CURRENT, RO_INIT_SINGLETHREADED,
};
use windows::UI::Composition::Compositor;
use windows::UI::Composition::Desktop::DesktopWindowTarget;

/// Balances a successful `RoInitialize` (S_OK or S_FALSE) when dropped.
struct RoGuard;

impl Drop for RoGuard {
    fn drop(&mut self) {
        // SAFETY: only constructed after RoInitialize succeeded on this thread.
        unsafe { RoUninitialize() };
    }
}

/// The compositor plus the dispatcher queue that keeps it alive on this thread.
/// Fields drop in declaration order, so the apartment is released last.
pub struct Comp {
    pub compositor: Compositor,
    _queue: DispatcherQueueController,
    _ro: Option<RoGuard>,
}

impl Deref for Comp {
    type Target = Compositor;
    fn deref(&self) -> &Compositor {
        &self.compositor
    }
}

/// Creates a dispatcher queue on the current thread (COM STA) and a compositor.
/// The thread must pump messages for composition to run.
pub fn init_compositor() -> Result<Comp> {
    // Make sure the thread is an STA; S_FALSE (already initialised) is fine and is balanced
    // like S_OK. If the thread is already MTA, the queue creation below reports the error.
    // SAFETY: plain apartment initialisation of the current thread.
    let ro = unsafe { RoInitialize(RO_INIT_SINGLETHREADED) }
        .is_ok()
        .then_some(RoGuard);
    let options = DispatcherQueueOptions {
        dwSize: size_of::<DispatcherQueueOptions>() as u32,
        threadType: DQTYPE_THREAD_CURRENT,
        apartmentType: DQTAT_COM_STA,
    };
    let queue = unsafe { CreateDispatcherQueueController(options)? };
    let compositor = Compositor::new()?;
    Ok(Comp {
        compositor,
        _queue: queue,
        _ro: ro,
    })
}

/// Composition target for a window (drawn below any child windows).
pub fn window_target(c: &Comp, hwnd: HWND) -> Result<DesktopWindowTarget> {
    let interop: ICompositorDesktopInterop = c.compositor.cast()?;
    unsafe { interop.CreateDesktopWindowTarget(hwnd, false) }
}
