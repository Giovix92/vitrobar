//! A hidden top-level window that receives everything that is not mouse input: shell hook
//! and WinEvent window events (Task 7 tracker), timers, icon results, the system broadcasts
//! for theme, time, locale, animation-setting, display and work-area changes, and the
//! session changes (unlock, remote / console connect) it registers for.
//!
//! The window procedure is authoritative: whatever reaches it, from the app's own loop or
//! from a nested modal loop (COM, shell, menus), is recorded in a per-thread [`Inbox`] that
//! the app drains with [`take_inbox`]. Nothing is dropped and no boxed event leaks.

use std::cell::RefCell;
use std::sync::OnceLock;

use vitro_core::tray_wire::TrayMessage;
use vitro_win::appbar::WM_VITRO_APPBAR;
use vitro_win::stock_taskbar::taskbar_created_msg;
use vitro_win::tray_host::{take_tray_message, WM_VITRO_TRAY};
use vitro_win::window_tracker::{
    shell_hook_message_id, take_window_event, translate_shell_hook, WindowEvent,
    WM_VITRO_WINDOW_EVENT,
};
use windows::core::{w, Error, Result, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::RemoteDesktop::{
    WTSRegisterSessionNotification, WTSUnRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
};
use windows::Win32::UI::Shell::{ABN_FULLSCREENAPP, ABN_POSCHANGED};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, PeekMessageW, PostQuitMessage,
    RegisterClassExW, MSG, PM_REMOVE, SPI_SETCLIENTAREAANIMATION, SPI_SETWORKAREA, WM_APP,
    WM_CLOSE, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_DWMCOLORIZATIONCOLORCHANGED, WM_ENDSESSION,
    WM_SETTINGCHANGE, WM_THEMECHANGED, WM_TIMECHANGE, WM_TIMER, WM_WTSSESSION_CHANGE, WNDCLASSEXW,
    WS_EX_TOOLWINDOW, WS_POPUP, WTS_CONSOLE_CONNECT, WTS_REMOTE_CONNECT, WTS_SESSION_UNLOCK,
};

/// An icon finished loading on the icon thread (posted by the worker).
pub const WM_APP_ICON: u32 = WM_APP + 0x22;
/// Posted once after the first frame: take the stock taskbar over now (R12: the bar shows
/// first, the slow `ABM_SETSTATE` round trip to Explorer comes after).
pub const WM_APP_TAKE_OVER: u32 = WM_APP + 0x23;

/// Timers on the notify window (ids distinct from the tracker's 0x5601/0x5602).
pub const TIMER_CLOCK: usize = 0x5701;
pub const TIMER_GPU_RETRY: usize = 0x5702;
/// One-shot hover timers of the thumbnail strip (400 ms to open, 300 ms to close).
pub const TIMER_HOVER_SHOW: usize = 0x5703;
pub const TIMER_HOVER_HIDE: usize = 0x5704;
/// One-shot: the hover motion has finished, so the bar margins can collapse.
pub const TIMER_MARGIN_COLLAPSE: usize = 0x5705;
/// One-shot after `TaskbarCreated`: Explorer may show its taskbar again once it is up.
pub const TIMER_REASSERT: usize = 0x5706;
/// One-shot: the cursor rested on a tray icon long enough for its tooltip (500 ms).
pub const TIMER_TRAY_TIP: usize = 0x5707;

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
    /// A system setting changed: Explorer may have shown its taskbar again.
    pub shell: bool,
    /// The display configuration changed (`WM_DISPLAYCHANGE`): monitors are re-read, the
    /// appbars claim their strips again.
    pub display: bool,
    /// A monitor's DPI changed (`WM_DPICHANGED` on a bar window or this one).
    pub dpi: bool,
    /// The work area changed (`WM_SETTINGCHANGE` / `SPI_SETWORKAREA`; also caused by our own
    /// appbars): monitors are re-read, nothing else happens unless they changed.
    pub workarea: bool,
    /// The session was unlocked or (re)connected locally or remotely: monitors may differ.
    pub session: bool,
    /// Explorer (re)created its taskbar (`TaskbarCreated`).
    pub taskbar_created: bool,
    /// `ABN_POSCHANGED`: the appbar must claim its strip again.
    pub appbar_pos: bool,
    /// A full-screen app may have opened or closed (`ABN_FULLSCREENAPP`).
    pub fullscreen: bool,
    /// Tray messages the tray host accepted, in arrival order.
    pub tray: Vec<TrayMessage>,
    /// [`WM_APP_TAKE_OVER`] arrived: finish the start-up.
    pub take_over: bool,
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

/// An appbar notification ([`WM_VITRO_APPBAR`]) reached one of our windows.
pub fn record_appbar(code: u32) {
    match code {
        ABN_POSCHANGED => with_inbox(|i| i.appbar_pos = true),
        ABN_FULLSCREENAPP => with_inbox(|i| i.fullscreen = true),
        _ => {}
    }
}

/// A bar window got `WM_DPICHANGED`.
pub fn record_dpi() {
    with_inbox(|i| i.dpi = true);
}

/// The session changes (`WM_WTSSESSION_CHANGE` codes) after which the monitors are re-read.
pub fn session_change_matters(code: u32) -> bool {
    matches!(
        code,
        WTS_SESSION_UNLOCK | WTS_REMOTE_CONNECT | WTS_CONSOLE_CONNECT
    )
}

pub struct NotifyWindow {
    pub hwnd: HWND,
    /// `WTSRegisterSessionNotification` succeeded (undone on drop).
    session_registered: bool,
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
        WM_VITRO_TRAY => {
            // SAFETY: only the tray host posts this message to the notify window; each
            // message is delivered here exactly once, so the box is reclaimed exactly once.
            let m = unsafe { take_tray_message(lparam) };
            with_inbox(|i| i.tray.push(m));
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
        WM_APP_TAKE_OVER => {
            with_inbox(|i| i.take_over = true);
            return LRESULT(0);
        }
        WM_VITRO_APPBAR => {
            record_appbar(wparam.0 as u32);
            return LRESULT(0);
        }
        WM_ENDSESSION => {
            if wparam.0 != 0 {
                // The process may end as soon as this returns: restore now, synchronously.
                crate::shell::restore_now();
                // SAFETY: plain call.
                unsafe { PostQuitMessage(0) };
            }
            return LRESULT(0);
        }
        WM_CLOSE => {
            // Someone asked Vitrobar to close: quit cleanly (the loop restores the shell).
            // SAFETY: plain call.
            unsafe { PostQuitMessage(0) };
            return LRESULT(0);
        }
        WM_DISPLAYCHANGE => with_inbox(|i| i.display = true),
        WM_DPICHANGED => {
            // The layout follows the monitor's new DPI; this window is never shown or sized.
            record_dpi();
            return LRESULT(0);
        }
        WM_WTSSESSION_CHANGE => {
            if session_change_matters(wparam.0 as u32) {
                with_inbox(|i| i.session = true);
            }
            return LRESULT(0);
        }
        WM_SETTINGCHANGE => {
            with_inbox(|i| i.shell = true);
            if wparam.0 as u32 == SPI_SETWORKAREA.0 {
                with_inbox(|i| i.workarea = true);
            }
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
        m if m == taskbar_created_msg() => {
            with_inbox(|i| i.taskbar_created = true);
            return LRESULT(0);
        }
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
        // Unlock and remote / console connect can come with other monitors. Without the
        // registration the bar still follows WM_DISPLAYCHANGE.
        // SAFETY: plain registration of our own window; undone in drop.
        let session_registered =
            unsafe { WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) }
                .inspect_err(|e| eprintln!("vitrobar: no session notifications: {e}"))
                .is_ok();
        Ok(Self {
            hwnd,
            session_registered,
        })
    }
}

impl Drop for NotifyWindow {
    fn drop(&mut self) {
        // Reclaim boxed window events and tray messages still queued (the tracker and the
        // tray host must already be dropped), then destroy the window.
        let mut msg = MSG::default();
        // SAFETY: every WM_VITRO_WINDOW_EVENT on this window was posted by the tracker; the
        // ones still queued were never dispatched and are reclaimed here exactly once.
        // PeekMessage returns WM_QUIT whatever the filter (the bar windows post one when they
        // are destroyed), so only real window events are reclaimed. The same holds for the
        // tray host's WM_VITRO_TRAY (the next id up).
        const _: () = assert!(WM_VITRO_TRAY == WM_VITRO_WINDOW_EVENT + 1);
        unsafe {
            while PeekMessageW(
                &mut msg,
                Some(self.hwnd),
                WM_VITRO_WINDOW_EVENT,
                WM_VITRO_TRAY,
                PM_REMOVE,
            )
            .as_bool()
            {
                if msg.hwnd != self.hwnd {
                    continue;
                }
                if msg.message == WM_VITRO_WINDOW_EVENT {
                    let _ = take_window_event(msg.lParam);
                } else if msg.message == WM_VITRO_TRAY {
                    let _ = take_tray_message(msg.lParam);
                }
            }
            // Balances the registration in `create`, while the window still exists.
            if self.session_registered {
                let _ = WTSUnRegisterSessionNotification(self.hwnd);
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
    fn only_unlock_and_connects_reread_the_monitors() {
        assert!(session_change_matters(WTS_SESSION_UNLOCK));
        assert!(session_change_matters(WTS_REMOTE_CONNECT));
        assert!(session_change_matters(WTS_CONSOLE_CONNECT));
        // WTS_CONSOLE_DISCONNECT, WTS_REMOTE_DISCONNECT, WTS_SESSION_LOCK.
        for code in [2, 4, 7] {
            assert!(!session_change_matters(code), "{code}");
        }
    }

    #[test]
    fn a_session_change_we_do_not_follow_is_not_recorded() {
        let n = NotifyWindow::create().unwrap();
        let _ = take_inbox();
        // SAFETY: plain message to our own window.
        unsafe {
            SendMessageW(n.hwnd, WM_WTSSESSION_CHANGE, Some(WPARAM(7)), None);
        }
        assert!(!take_inbox().session);
    }

    #[test]
    fn drop_with_a_pending_quit_reclaims_only_window_events() {
        let n = NotifyWindow::create().unwrap();
        let ev = Box::into_raw(Box::new(WindowEvent::Flash(1)));
        // SAFETY: the box is handed over to the message; the drop reclaims it.
        unsafe {
            PostMessageW(
                Some(n.hwnd),
                WM_VITRO_WINDOW_EVENT,
                WPARAM(0),
                LPARAM(ev as isize),
            )
            .unwrap();
            // As a destroyed bar window does.
            PostQuitMessage(0);
        }
        drop(n);
        // The quit is consumed (or still pending); clear it so it cannot leak into other tests.
        let mut msg = MSG::default();
        // SAFETY: draining this test thread's own queue.
        unsafe { while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {} }
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
            let tray = Box::into_raw(Box::new(TrayMessage {
                op: vitro_core::tray_wire::TrayOp::Delete,
                id: vitro_core::tray_wire::TrayIconId {
                    hwnd: 1,
                    uid: 2,
                    guid: None,
                },
                callback_msg: None,
                hicon: None,
                tip: None,
                hidden: None,
                version: None,
            }));
            PostMessageW(
                Some(n.hwnd),
                WM_VITRO_TRAY,
                WPARAM(0),
                LPARAM(tray as isize),
            )
            .unwrap();
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
            SendMessageW(n.hwnd, taskbar_created_msg(), None, None);
            SendMessageW(n.hwnd, WM_DISPLAYCHANGE, None, None);
            SendMessageW(n.hwnd, WM_DPICHANGED, None, None);
            SendMessageW(
                n.hwnd,
                WM_SETTINGCHANGE,
                Some(WPARAM(SPI_SETWORKAREA.0 as usize)),
                Some(LPARAM(0)),
            );
            SendMessageW(
                n.hwnd,
                WM_WTSSESSION_CHANGE,
                Some(WPARAM(WTS_SESSION_UNLOCK as usize)),
                None,
            );
            SendMessageW(
                n.hwnd,
                WM_VITRO_APPBAR,
                Some(WPARAM(ABN_FULLSCREENAPP as usize)),
                Some(LPARAM(1)),
            );
            SendMessageW(
                n.hwnd,
                WM_VITRO_APPBAR,
                Some(WPARAM(ABN_POSCHANGED as usize)),
                None,
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
        assert!(inbox.shell && inbox.display && inbox.taskbar_created);
        assert!(inbox.dpi && inbox.workarea && inbox.session);
        assert!(inbox.fullscreen && inbox.appbar_pos);
        assert!(inbox.timers.contains(&TIMER_CLOCK));
        assert_eq!(inbox.tray.len(), 1);
        assert_eq!(inbox.tray[0].id.uid, 2);
        assert_eq!(take_inbox(), Inbox::default());
    }
}
