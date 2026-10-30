//! A hidden top-level window that receives everything that is not mouse input: shell hook
//! and WinEvent window events (Task 7 tracker), timers, icon results, and the system
//! broadcasts for theme, time and locale changes.

use std::sync::OnceLock;

use vitro_win::window_tracker::{take_window_event, WM_VITRO_WINDOW_EVENT};
use windows::core::{w, Error, Result, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, PeekMessageW, PostMessageW, RegisterClassExW,
    MSG, PM_REMOVE, WM_APP, WM_DWMCOLORIZATIONCOLORCHANGED, WM_SETTINGCHANGE, WM_THEMECHANGED,
    WM_TIMECHANGE, WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_POPUP,
};

/// Colour mode / transparency / accent may have changed (coalesced by the loop).
pub const WM_APP_THEME: u32 = WM_APP + 0x20;
/// System time, time zone or regional format changed: redraw the clock and re-arm its timer.
pub const WM_APP_CLOCK: u32 = WM_APP + 0x21;
/// An icon finished loading on the icon thread.
pub const WM_APP_ICON: u32 = WM_APP + 0x22;

/// One-shot timers on the notify window (ids distinct from the tracker's 0x5601/0x5602).
pub const TIMER_CLOCK: usize = 0x5701;
pub const TIMER_GPU_RETRY: usize = 0x5702;

pub struct NotifyWindow {
    pub hwnd: HWND,
}

fn setting_area(lparam: LPARAM) -> Option<String> {
    if lparam.0 == 0 {
        return None;
    }
    // SAFETY: for WM_SETTINGCHANGE a non-null lParam is a NUL-terminated wide string.
    unsafe { PCWSTR(lparam.0 as *const u16).to_string() }.ok()
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let post = match msg {
        WM_SETTINGCHANGE => match setting_area(lparam).as_deref() {
            Some("ImmersiveColorSet") => Some(WM_APP_THEME),
            Some("intl") => Some(WM_APP_CLOCK),
            _ => None,
        },
        WM_DWMCOLORIZATIONCOLORCHANGED | WM_THEMECHANGED => Some(WM_APP_THEME),
        WM_TIMECHANGE => Some(WM_APP_CLOCK),
        _ => None,
    };
    // SAFETY: posting to our own window; default handling with our own arguments.
    unsafe {
        if let Some(m) = post {
            let _ = PostMessageW(Some(hwnd), m, WPARAM(0), LPARAM(0));
        }
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }
}

fn register_class() -> Result<()> {
    static RESULT: OnceLock<Result<()>> = OnceLock::new();
    RESULT
        .get_or_init(|| unsafe {
            // SAFETY: plain class registration with a static window procedure.
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wndproc),
                hInstance: GetModuleHandleW(None)?.into(),
                lpszClassName: w!("VitrobarNotify"),
                ..Default::default()
            };
            if RegisterClassExW(&wc) == 0 {
                return Err(Error::from_thread());
            }
            Ok(())
        })
        .clone()
}

impl NotifyWindow {
    /// Never shown. A real (not message-only) top-level window, because broadcasts and shell
    /// hook messages only reach those.
    pub fn create() -> Result<Self> {
        register_class()?;
        // SAFETY: the class is registered; static strings.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                w!("VitrobarNotify"),
                w!("Vitrobar"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(GetModuleHandleW(None)?.into()),
                None,
            )?
        };
        Ok(Self { hwnd })
    }
}

impl Drop for NotifyWindow {
    fn drop(&mut self) {
        // Reclaim boxed window events still queued (the tracker must already be dropped),
        // then destroy the window.
        let mut msg = MSG::default();
        // SAFETY: every WM_VITRO_WINDOW_EVENT on this window was posted by the tracker and is
        // removed from the queue here exactly once.
        unsafe {
            while PeekMessageW(
                &mut msg,
                Some(self.hwnd),
                WM_VITRO_WINDOW_EVENT,
                WM_VITRO_WINDOW_EVENT,
                PM_REMOVE,
            )
            .as_bool()
            {
                let _ = take_window_event(msg.lParam);
            }
            let _ = DestroyWindow(self.hwnd);
        }
    }
}
