//! Reserving the bar's strip of the screen (the work area that maximized windows respect)
//! through the shell's AppBar interface, and telling when a full-screen app covers a monitor.

use vitro_core::settings::Position;
use windows::core::{Error, Result};
use windows::Win32::Foundation::{E_FAIL, HWND, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Shell::{
    SHAppBarMessage, ABE_BOTTOM, ABE_TOP, ABM_NEW, ABM_QUERYPOS, ABM_REMOVE, ABM_SETPOS, APPBARDATA,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowLongW, GetWindowRect, GetWindowThreadProcessId, IsZoomed,
    GWL_STYLE, WM_APP, WS_CAPTION,
};

use crate::monitors::list_monitors;
use crate::window_props::class_name;

/// Callback message of every Vitrobar appbar: `wParam` = `ABN_*`, `lParam` = its argument
/// (for `ABN_FULLSCREENAPP`, non-zero when a full-screen app opened).
pub const WM_VITRO_APPBAR: u32 = WM_APP + 0x30;

/// `(left, top, right, bottom)` in physical pixels.
pub type PxRect = (i32, i32, i32, i32);

fn edge_of(p: Position) -> u32 {
    match p {
        Position::Bottom => ABE_BOTTOM,
        Position::Top => ABE_TOP,
    }
}

/// The strip to reserve: `reserved_px` along `edge` of the monitor.
pub fn reserved_rect(edge: Position, monitor: PxRect, reserved_px: i32) -> PxRect {
    let (l, t, r, b) = monitor;
    let h = reserved_px.clamp(0, b - t);
    match edge {
        Position::Bottom => (l, b - h, r, b),
        Position::Top => (l, t, r, t + h),
    }
}

fn data(hwnd: HWND) -> APPBARDATA {
    APPBARDATA {
        cbSize: size_of::<APPBARDATA>() as u32,
        hWnd: hwnd,
        uCallbackMessage: WM_VITRO_APPBAR,
        ..Default::default()
    }
}

fn message(msg: u32, abd: &mut APPBARDATA) -> usize {
    // SAFETY: `abd` is a valid, sized APPBARDATA for the duration of the call.
    unsafe { SHAppBarMessage(msg, abd) }
}

/// Unregisters the appbar of `hwnd` (harmless if it is not registered). For paths that
/// cannot reach the [`AppBar`] value, such as the end of the session.
pub fn unregister(hwnd: HWND) {
    message(ABM_REMOVE, &mut data(hwnd));
}

/// One registered appbar: the shell keeps `reserved_px` at the edge free of maximized
/// windows. Unregistered (`ABM_REMOVE`) when dropped.
#[derive(Debug)]
pub struct AppBar {
    hwnd: HWND,
    edge: Position,
    monitor: PxRect,
    reserved: i32,
}

impl AppBar {
    /// `hwnd` (one of our windows) receives [`WM_VITRO_APPBAR`] notifications.
    pub fn register(
        hwnd: HWND,
        edge: Position,
        monitor_rect_px: PxRect,
        reserved_px: i32,
    ) -> Result<Self> {
        if message(ABM_NEW, &mut data(hwnd)) == 0 {
            return Err(Error::new(E_FAIL, "ABM_NEW refused (already an appbar?)"));
        }
        let bar = Self {
            hwnd,
            edge,
            monitor: monitor_rect_px,
            reserved: reserved_px,
        };
        bar.set_pos();
        Ok(bar)
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Follows a new edge, monitor rectangle or thickness (no-op when nothing changed).
    pub fn update(&mut self, edge: Position, monitor_rect_px: PxRect, reserved_px: i32) {
        if (edge, monitor_rect_px, reserved_px) == (self.edge, self.monitor, self.reserved) {
            return;
        }
        self.edge = edge;
        self.monitor = monitor_rect_px;
        self.reserved = reserved_px;
        self.set_pos();
    }

    /// (Re)claims the strip: `ABM_QUERYPOS` then `ABM_SETPOS`. Also the answer to
    /// `ABN_POSCHANGED`. The window itself is not moved: the bar layout places it.
    pub fn set_pos(&self) {
        let mut abd = data(self.hwnd);
        abd.uEdge = edge_of(self.edge);
        let (l, t, r, b) = reserved_rect(self.edge, self.monitor, self.reserved);
        abd.rc = RECT {
            left: l,
            top: t,
            right: r,
            bottom: b,
        };
        message(ABM_QUERYPOS, &mut abd);
        // The shell may have moved the proposed edge (other appbars); keep the thickness.
        match self.edge {
            Position::Bottom => abd.rc.top = abd.rc.bottom - (b - t),
            Position::Top => abd.rc.bottom = abd.rc.top + (b - t),
        }
        message(ABM_SETPOS, &mut abd);
    }

    /// After Explorer restarted (`TaskbarCreated`): the new shell knows nothing of us.
    pub fn reregister(&mut self) -> Result<()> {
        unregister(self.hwnd);
        if message(ABM_NEW, &mut data(self.hwnd)) == 0 {
            return Err(Error::new(E_FAIL, "ABM_NEW refused"));
        }
        self.set_pos();
        Ok(())
    }
}

impl Drop for AppBar {
    fn drop(&mut self) {
        unregister(self.hwnd);
    }
}

/// What [`is_fullscreen_on`] knows about the foreground window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Foreground {
    /// Visible frame bounds in physical pixels.
    pub rect: PxRect,
    pub class: String,
    /// The window belongs to this process.
    pub own: bool,
    /// Maximized with a caption: an ordinary maximized window (it can reach the monitor
    /// edges when nothing reserves space), not a full-screen one.
    pub maximized_with_caption: bool,
}

const SHELL_CLASSES: [&str; 4] = [
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
];

/// Pure decision: the foreground window is a full-screen app covering `monitor`.
pub fn covers_as_fullscreen(fg: &Foreground, monitor: PxRect) -> bool {
    let (l, t, r, b) = fg.rect;
    let covers = l <= monitor.0 && t <= monitor.1 && r >= monitor.2 && b >= monitor.3;
    covers && !fg.own && !fg.maximized_with_caption && !SHELL_CLASSES.contains(&fg.class.as_str())
}

fn frame_rect(hwnd: HWND) -> Option<PxRect> {
    let mut r = RECT::default();
    // SAFETY: the out pointer is a local RECT whose size is passed.
    let dwm = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut r as *mut RECT as *mut _,
            size_of::<RECT>() as u32,
        )
    };
    // SAFETY: as above.
    if dwm.is_err() && unsafe { GetWindowRect(hwnd, &mut r) }.is_err() {
        return None;
    }
    Some((r.left, r.top, r.right, r.bottom))
}

/// Reads [`Foreground`] for `hwnd`.
pub fn foreground_info(hwnd: HWND) -> Option<Foreground> {
    if hwnd.0.is_null() {
        return None;
    }
    let rect = frame_rect(hwnd)?;
    let mut pid = 0u32;
    // SAFETY: plain queries; `pid` is a local out parameter.
    let (own, style, zoomed) = unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        (
            pid == GetCurrentProcessId(),
            GetWindowLongW(hwnd, GWL_STYLE) as u32,
            IsZoomed(hwnd).as_bool(),
        )
    };
    Some(Foreground {
        rect,
        class: class_name(hwnd),
        own,
        maximized_with_caption: zoomed && style & WS_CAPTION.0 == WS_CAPTION.0,
    })
}

/// The foreground window covers the whole of monitor `monitor` (a [`MonitorInfo::id`]) and
/// is not the desktop, the shell or one of our own windows.
///
/// [`MonitorInfo::id`]: crate::monitors::MonitorInfo::id
pub fn is_fullscreen_on(monitor: u64) -> bool {
    let Some(mon) = list_monitors().into_iter().find(|m| m.id == monitor) else {
        return false;
    };
    // SAFETY: plain query.
    let fg = unsafe { GetForegroundWindow() };
    foreground_info(fg).is_some_and(|f| covers_as_fullscreen(&f, mon.rect_px))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MON: PxRect = (0, 0, 2560, 1440);

    fn fg(rect: PxRect, class: &str) -> Foreground {
        Foreground {
            rect,
            class: class.into(),
            own: false,
            maximized_with_caption: false,
        }
    }

    #[test]
    fn reserved_strip_per_edge() {
        assert_eq!(
            reserved_rect(Position::Bottom, MON, 72),
            (0, 1368, 2560, 1440)
        );
        assert_eq!(reserved_rect(Position::Top, MON, 72), (0, 0, 2560, 72));
        assert_eq!(
            reserved_rect(Position::Bottom, (100, 50, 900, 650), 0),
            (100, 650, 900, 650)
        );
        assert_eq!(reserved_rect(Position::Top, MON, 5000), MON);
    }

    #[test]
    fn fullscreen_decision() {
        assert!(covers_as_fullscreen(&fg(MON, "Chrome_WidgetWin_1"), MON));
        // Bigger than the monitor still covers it (exclusive-mode games, borders).
        assert!(covers_as_fullscreen(&fg((-8, -8, 2568, 1448), "X"), MON));
        // One pixel short is not full screen.
        assert!(!covers_as_fullscreen(&fg((0, 0, 2560, 1439), "X"), MON));
        // Another monitor.
        assert!(!covers_as_fullscreen(&fg((2560, 0, 5120, 1440), "X"), MON));
        for c in SHELL_CLASSES {
            assert!(!covers_as_fullscreen(&fg(MON, c), MON), "{c}");
        }
        let mut own = fg(MON, "VitrobarBar");
        own.own = true;
        assert!(!covers_as_fullscreen(&own, MON));
        let mut maxed = fg(MON, "Notepad");
        maxed.maximized_with_caption = true;
        assert!(!covers_as_fullscreen(&maxed, MON));
    }

    #[test]
    fn foreground_info_of_null_is_none() {
        assert_eq!(foreground_info(HWND::default()), None);
    }
}
