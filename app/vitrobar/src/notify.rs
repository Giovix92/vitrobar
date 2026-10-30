//! A hidden top-level window that receives everything that is not mouse input: shell hook
//! and WinEvent window events (Task 7 tracker), timers, icon results, and the system
//! broadcasts for theme, time, locale and animation-setting changes.
//!
//! The window procedure is authoritative: whatever reaches it, from the app's own loop or
//! from a nested modal loop (COM, shell, menus), is recorded in a per-thread [`Inbox`] that
//! the app drains with [`take_inbox`]. Nothing is dropped and no boxed event leaks.

use std::cell::RefCell;
use std::sync::OnceLock;

use vitro_win::window_tracker::{
    shell_hook_message_id, take_window_event, translate_shell_hook, WindowEvent,
    WM_VITRO_WINDOW_EVENT,
};
use windows::core::{w, Error, Result, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, PeekMessageW, RegisterClassExW, MSG, PM_REMOVE,
    SPI_SETCLIENTAREAANIMATION, WM_APP, WM_DWMCOLORIZATIONCOLORCHANGED, WM_SETTINGCHANGE,
    WM_THEMECHANGED, WM_TIMECHANGE, WM_TIMER, WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_POPUP,
};

/// An icon finished loading on the icon thread (posted by the worker).
pub const WM_APP_ICON: u32 = WM_APP + 0x22;

/// Timers on the notify window (ids distinct from the tracker's 0x5601/0x5602).
pub const TIMER_CLOCK: usize = 0x5701;
pub const TIMER_GPU_RETRY: usize = 0x5702;

/// Everything the notify window received since the last [`take_inbox`].
#[derive(Debug, Default, PartialEq)]
pub struct Inbox {
    /// Window events in arrival order (WinEvent hooks and the shell hook).
    pub events: Vec<WindowEvent>,
    /// Ids of the `WM_TIMER`s that fired, in order (duplicates possible).
    pub timers: Vec<usize>,
    /// Colour mode / transparency / accent / animation setting may have changed.
    pub theme: bool,
    /// System time, time zone or regional format changed.
    pub clock: bool,
    /// Icon results are waiting in the icon loader.
    pub icons: bool,
}

thread_local! {
    static INBOX: RefCell<Inbox> = RefCell::new(Inbox::default());
}

/// Takes what the notify window received on this thread.
pub fn take_inbox() -> Inbox {
    INBOX.with(|i| std::mem::take(&mut *i.borrow_mut()))
}

fn with_inbox(f: impl FnOnce(&mut Inbox)) {
    INBOX.with(|i| f(&mut i.borrow_mut()));
}

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
    match msg {
        WM_VITRO_WINDOW_EVENT => {
            // SAFETY: only the tracker posts this message to the notify window; each message is
            // delivered to this procedure exactly once, so the box is reclaimed exactly once.
            let ev = unsafe { take_window_event(lparam) };
            with_inbox(|i| i.events.push(ev));
            return LRESULT(0);
        }
        WM_TIMER => {
            with_inbox(|i| i.timers.push(wparam.0));
            return LRESULT(0);
        }
        WM_APP_ICON => {
            with_inbox(|i| i.icons = true);
            return LRESULT(0);
        }
        WM_SETTINGCHANGE => {
            let animations = wparam.0 as u32 == SPI_SETCLIENTAREAANIMATION.0;
            match setting_area(lparam).as_deref() {
                Some("ImmersiveColorSet") => with_inbox(|i| i.theme = true),
                Some("intl") => with_inbox(|i| i.clock = true),
                _ if animations => with_inbox(|i| i.theme = true),
                _ => {}
            }
        }
        WM_DWMCOLORIZATIONCOLORCHANGED | WM_THEMECHANGED => with_inbox(|i| i.theme = true),
        WM_TIMECHANGE => with_inbox(|i| i.clock = true),
        m if m == shell_hook_message_id() => {
            if let Some(ev) = translate_shell_hook(wparam, lparam) {
                with_inbox(|i| i.events.push(ev));
            }
            return LRESULT(0);
        }
        _ => {}
    }
    // SAFETY: default handling with our own arguments.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
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
        // SAFETY: every WM_VITRO_WINDOW_EVENT on this window was posted by the tracker; the
        // ones still queued were never dispatched and are reclaimed here exactly once.
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
        let _ = take_inbox();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, PostMessageW, SendMessageW, SetTimer, HSHELL_WINDOWDESTROYED,
    };

    fn pump(hwnd: HWND) {
        let mut msg = MSG::default();
        // SAFETY: standard dispatch of our own window's queued messages.
        unsafe {
            while PeekMessageW(&mut msg, Some(hwnd), 0, 0, PM_REMOVE).as_bool() {
                DispatchMessageW(&msg);
            }
        }
    }

    #[test]
    fn dispatched_messages_land_in_the_inbox() {
        let n = NotifyWindow::create().unwrap();
        let _ = take_inbox();
        let ev = Box::into_raw(Box::new(WindowEvent::Flash(42)));
        // SAFETY: the box is handed over to the message; the wndproc reclaims it.
        unsafe {
            PostMessageW(
                Some(n.hwnd),
                WM_VITRO_WINDOW_EVENT,
                WPARAM(0),
                LPARAM(ev as isize),
            )
            .unwrap();
            PostMessageW(Some(n.hwnd), WM_APP_ICON, WPARAM(0), LPARAM(0)).unwrap();
            // As a nested modal loop would: DispatchMessage, not the app's own peek path.
            pump(n.hwnd);
            SendMessageW(
                n.hwnd,
                shell_hook_message_id(),
                Some(WPARAM(HSHELL_WINDOWDESTROYED as usize)),
                Some(LPARAM(7)),
            );
            SendMessageW(n.hwnd, WM_TIMECHANGE, None, None);
            SendMessageW(
                n.hwnd,
                WM_SETTINGCHANGE,
                Some(WPARAM(SPI_SETCLIENTAREAANIMATION.0 as usize)),
                Some(LPARAM(0)),
            );
            SetTimer(Some(n.hwnd), TIMER_CLOCK, 1, None);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
        pump(n.hwnd);
        let inbox = take_inbox();
        assert_eq!(
            inbox.events,
            vec![WindowEvent::Flash(42), WindowEvent::Destroyed(7)]
        );
        assert!(inbox.icons && inbox.clock && inbox.theme);
        assert!(inbox.timers.contains(&TIMER_CLOCK));
        assert_eq!(take_inbox(), Inbox::default());
    }
}
