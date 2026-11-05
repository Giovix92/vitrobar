//! An activatable glass top-level window: the Start panel and the settings window.
//!
//! Unlike the [`crate::flyout::Flyout`] it takes focus when clicked (it hears the keyboard)
//! and reports when it loses activation, so the app can close it on a click elsewhere.

use std::cell::RefCell;
use std::sync::Mutex;

use vitro_core::settings::Position;
use vitro_core::style::MaterialStyle;
use windows::core::{h, w, Error, Interface, Result, BOOL, HSTRING, PCWSTR};
use windows::Foundation::TimeSpan;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_USE_HOSTBACKDROPBRUSH, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_DONOTROUND,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetForegroundWindow, GetWindowLongPtrW,
    GetWindowRect, IsIconic, IsWindow, LoadCursorW, LoadIconW, RegisterClassExW,
    SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, SetWindowTextW, ShowWindow,
    GWLP_USERDATA, HTCAPTION, HTCLIENT, HWND_TOPMOST, IDC_ARROW, MA_ACTIVATE, SWP_NOACTIVATE,
    SWP_NOZORDER, SW_RESTORE, SW_SHOW, WA_INACTIVE, WM_ACTIVATE, WM_CLOSE, WM_DPICHANGED,
    WM_LBUTTONDOWN, WM_MBUTTONDOWN, WM_MOUSEACTIVATE, WM_NCHITTEST, WM_RBUTTONDOWN, WNDCLASSEXW,
    WS_EX_APPWINDOW, WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{
    Compositor, ContainerVisual, CubicBezierEasingFunction, Vector3KeyFrameAnimation, Visual,
};
use windows_numerics::{Vector2, Vector3};

use crate::compositor::{window_target, Comp};
use crate::flyout::PxRect;
use crate::material::backdrop_visual;

/// How far a rising panel starts from its place, in DIP.
const RISE_DIP: f32 = 12.0;

/// How a panel appears and disappears.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Motion {
    /// Fades while scaling between 98 % and 100 % about its centre (the app grid).
    Grow,
    /// Fades while sliding [`RISE_DIP`] from the bar's side (the Start panel).
    Rise { from: Position },
}

/// Where a rising panel starts, as an offset from its place in px at `scale` px per DIP:
/// towards the bar it came from (a bottom bar's panel rises from below).
fn rise_offset(from: Position, scale: f32) -> (f32, f32) {
    let d = RISE_DIP * scale;
    match from {
        Position::Bottom => (0.0, d),
        Position::Top => (0.0, -d),
    }
}

fn duration(ms: u32) -> TimeSpan {
    TimeSpan::from(std::time::Duration::from_millis(ms as u64))
}

/// Window class of the panel (found by tests and tools with `FindWindow`).
pub const PANEL_CLASS: &str = "VitrobarPanel";

fn register_class() -> Result<()> {
    // Only a success is remembered: a failed registration is tried again by the next
    // caller (a class registered in this process stays registered).
    static DONE: Mutex<bool> = Mutex::new(false);
    let mut done = DONE.lock().unwrap_or_else(|e| e.into_inner());
    if *done {
        return Ok(());
    }
    let registered = (|| -> Result<()> {
        unsafe {
            // SAFETY: plain class registration with a static window procedure.
            let instance = GetModuleHandleW(None)?;
            // The exe's icon (resource 1) for the settings window's taskbar button and
            // Alt+Tab; none (the default) when the exe has no icon (tests).
            let icon = LoadIconW(Some(instance.into()), PCWSTR(1 as _)).unwrap_or_default();
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wndproc),
                hInstance: instance.into(),
                hIcon: icon,
                hIconSm: icon,
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                lpszClassName: w!("VitrobarPanel"),
                ..Default::default()
            };
            if RegisterClassExW(&wc) == 0 {
                return Err(Error::from_thread());
            }
            Ok(())
        }
    })();
    if registered.is_ok() {
        *done = true;
    }
    registered
}

thread_local! {
    /// Panels that lost activation since the last [`take_panel_deactivated`] (this thread's).
    static PANEL_DEACTIVATED: RefCell<Vec<HWND>> = const { RefCell::new(Vec::new()) };
    /// Panels asked to close (`WM_CLOSE`: Alt+F4, a taskbar's "Close window") since the last
    /// [`take_panel_close_requests`].
    static PANEL_CLOSE: RefCell<Vec<HWND>> = const { RefCell::new(Vec::new()) };
    /// Panels that got a mouse button press since the last [`take_panel_clicks`].
    static PANEL_CLICKS: RefCell<Vec<HWND>> = const { RefCell::new(Vec::new()) };
    /// Panels moved to a monitor of another DPI: (window, new DPI, suggested rect).
    static PANEL_DPI: RefCell<Vec<(HWND, u32, PxRect)>> = const { RefCell::new(Vec::new()) };
}

/// Drag-band packing in `GWLP_USERDATA`: the band height in px in the low 16 bits, the width
/// of the band's right end that stays client area (a close button) in the next 16.
fn pack_drag(height_px: isize, right_px: isize) -> isize {
    height_px.clamp(0, 0xFFFF) | (right_px.clamp(0, 0xFFFF) << 16)
}

/// Whether a point `(x, y)` px from the window's top-left corner is in the drag band of a
/// window `width` px wide, with `packed` from [`pack_drag`].
fn in_drag_band(packed: isize, width: i32, x: i32, y: i32) -> bool {
    let height = (packed & 0xFFFF) as i32;
    let right = ((packed >> 16) & 0xFFFF) as i32;
    height > 0 && y >= 0 && y < height && x < width - right
}

/// Panels that were deactivated since the last call: the user clicked elsewhere or
/// switched apps.
pub fn take_panel_deactivated() -> Vec<HWND> {
    PANEL_DEACTIVATED.with(|d| std::mem::take(&mut *d.borrow_mut()))
}

/// Records that panel `h` was left for another app, for a panel whose own deactivation was
/// held back while one of the glass menus was up.
pub fn note_deactivated(h: HWND) {
    PANEL_DEACTIVATED.with(|d| d.borrow_mut().push(h));
}

/// Panels that got a mouse button press (or were about to be activated by one) since the
/// last call. The owner of a fading panel drops it on the first one.
pub fn take_panel_clicks() -> Vec<HWND> {
    PANEL_CLICKS.with(|d| std::mem::take(&mut *d.borrow_mut()))
}

/// Panels that were asked to close since the last call. A panel never destroys itself on
/// `WM_CLOSE`: its owner drops it (and its visuals) instead.
pub fn take_panel_close_requests() -> Vec<HWND> {
    PANEL_CLOSE.with(|d| std::mem::take(&mut *d.borrow_mut()))
}

/// Panels that moved to a monitor of another DPI since the last call: the window, its new
/// DPI and the rectangle Windows suggests for it (`WM_DPICHANGED`). The owner resizes it.
pub fn take_panel_dpi_changes() -> Vec<(HWND, u32, PxRect)> {
    PANEL_DPI.with(|d| std::mem::take(&mut *d.borrow_mut()))
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        // The panel takes focus when clicked.
        WM_MOUSEACTIVATE => {
            PANEL_CLICKS.with(|d| d.borrow_mut().push(hwnd));
            LRESULT(MA_ACTIVATE as isize)
        }
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
            PANEL_CLICKS.with(|d| d.borrow_mut().push(hwnd));
            // SAFETY: default handling with the arguments we were given.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_ACTIVATE => {
            // `lparam` is the window taking the activation: one of our glass menus (opened
            // from this panel) is not a click elsewhere.
            if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE
                && !crate::menu_view::is_menu_window(HWND(lparam.0 as *mut _))
            {
                PANEL_DEACTIVATED.with(|d| d.borrow_mut().push(hwnd));
            }
            // SAFETY: default handling with the arguments we were given.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_NCHITTEST => {
            // SAFETY: the user data is the packed drag band, set by `set_drag_height` /
            // `set_drag_exclude_right` (0 when never set); GetWindowRect writes into a local.
            let drag = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
            let mut r = RECT::default();
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            if drag != 0
                && unsafe { GetWindowRect(hwnd, &mut r) }.is_ok()
                && in_drag_band(drag, r.right - r.left, x - r.left, y - r.top)
            {
                LRESULT(HTCAPTION as isize)
            } else {
                LRESULT(HTCLIENT as isize)
            }
        }
        // Never destroyed behind its owner's back: the owner drops the panel.
        WM_CLOSE => {
            PANEL_CLOSE.with(|d| d.borrow_mut().push(hwnd));
            LRESULT(0)
        }
        WM_DPICHANGED => {
            // SAFETY: for WM_DPICHANGED, lParam points to the suggested RECT (documented).
            let r = unsafe { *(lparam.0 as *const RECT) };
            let dpi = (wparam.0 & 0xFFFF) as u32;
            let rect = (r.left, r.top, r.right, r.bottom);
            PANEL_DPI.with(|d| d.borrow_mut().push((hwnd, dpi, rect)));
            LRESULT(0)
        }
        // SAFETY: default handling with the arguments we were given.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// A glass top-level window that can be activated. Dropping it destroys the window.
pub struct Panel {
    // Field order = drop order: the target before the window it draws into.
    _target: DesktopWindowTarget,
    root: ContainerVisual,
    glass: Visual,
    hwnd: HWND,
    rect_px: PxRect,
    scale: f32,
    radius_dip: f32,
    topmost: bool,
    /// Drag band height and its excluded right end, in DIP (re-applied after a DPI change).
    drag_dip: (f32, f32),
}

impl Panel {
    /// Opens a panel at `rect_px` on a monitor of `dpi`, filled with `style` and corners of
    /// `radius_dip`. `title` is the window text. `app_window` gives the settings window a
    /// taskbar button and normal z-order instead of the tool-window/topmost Start panel.
    pub fn show(
        comp: &Comp,
        rect_px: PxRect,
        dpi: u32,
        style: &MaterialStyle,
        radius_dip: f32,
        title: &str,
        app_window: bool,
    ) -> Result<Self> {
        register_class()?;
        let size = (rect_px.2 - rect_px.0, rect_px.3 - rect_px.1);
        let ex = if app_window {
            WS_EX_APPWINDOW | WS_EX_NOREDIRECTIONBITMAP
        } else {
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOREDIRECTIONBITMAP
        };
        let title = HSTRING::from(title);
        // SAFETY: the class is registered; the title outlives the call.
        let hwnd = unsafe {
            CreateWindowExW(
                ex,
                w!("VitrobarPanel"),
                PCWSTR(title.as_ptr()),
                WS_POPUP,
                rect_px.0,
                rect_px.1,
                size.0,
                size.1,
                None,
                None,
                Some(GetModuleHandleW(None)?.into()),
                None,
            )?
        };
        let scale = dpi as f32 / 96.0;
        let finish = || -> Result<Self> {
            // SAFETY: the attribute values are locals whose sizes are passed alongside.
            unsafe {
                let on = BOOL(1);
                DwmSetWindowAttribute(
                    hwnd,
                    DWMWA_USE_HOSTBACKDROPBRUSH,
                    &on as *const BOOL as *const _,
                    size_of::<BOOL>() as u32,
                )?;
                let corners = DWMWCP_DONOTROUND;
                DwmSetWindowAttribute(
                    hwnd,
                    DWMWA_WINDOW_CORNER_PREFERENCE,
                    &corners as *const _ as *const _,
                    size_of_val(&corners) as u32,
                )?;
            }
            let target = window_target(comp, hwnd)?;
            let root = comp.CreateContainerVisual()?;
            let size_v = Vector2::new(size.0 as f32, size.1 as f32);
            root.SetSize(size_v)?;
            let glass: Visual = backdrop_visual(comp, style, size_v, radius_dip * scale)?.cast()?;
            root.Children()?.InsertAtBottom(&glass)?;
            target.SetRoot(&root)?;
            // SAFETY: plain call on our own window.
            unsafe {
                let _ = ShowWindow(hwnd, SW_SHOW);
            }
            Ok(Self {
                _target: target,
                root,
                glass,
                hwnd,
                rect_px,
                scale,
                radius_dip,
                topmost: !app_window,
                drag_dip: (0.0, 0.0),
            })
        };
        // SAFETY: destroying the window we just created.
        finish().inspect_err(|_| unsafe {
            let _ = DestroyWindow(hwnd);
        })
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Screen rectangle in px.
    pub fn rect_px(&self) -> PxRect {
        self.rect_px
    }

    /// Physical px per DIP.
    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// The panel's visual tree: the glass is its bottom child; content goes on top.
    pub fn root(&self) -> &ContainerVisual {
        &self.root
    }

    /// Opens the panel over `ms` with `motion`: a composition animation (the compositor runs
    /// it, no timer). Call right after `show`.
    pub fn animate_open(&self, ms: u32, motion: Motion) -> Result<()> {
        animate_open(&self.root, self.scale, ms, motion)
    }

    /// The reverse of [`Panel::animate_open`] (see [`animate_close`]). The window still takes
    /// mouse input while it fades: the owner drops it on the first click
    /// ([`take_panel_clicks`]) and once `ms` has passed.
    pub fn animate_close(&self, ms: u32, motion: Motion) -> Result<()> {
        animate_close(&self.root, self.scale, ms, motion)
    }

    /// Puts the panel in its final, fully visible state (see [`show_static`]).
    pub fn show_static(&self) {
        show_static(&self.root)
    }

    /// Makes the top `dip` band of the panel a window caption (drag to move). Default 0.
    pub fn set_drag_height(&mut self, dip: f32) {
        self.drag_dip.0 = dip.max(0.0);
        self.store_drag();
    }

    /// Keeps the right `dip` of the drag band client area (a close button there hears
    /// clicks). Default 0.
    pub fn set_drag_exclude_right(&mut self, dip: f32) {
        self.drag_dip.1 = dip.max(0.0);
        self.store_drag();
    }

    fn store_drag(&self) {
        let px = |dip: f32| (dip * self.scale).round() as isize;
        let packed = pack_drag(px(self.drag_dip.0), px(self.drag_dip.1));
        // SAFETY: plain call on our own window; the value is read back by `wndproc`.
        unsafe {
            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, packed);
        }
    }

    /// Changes the window text (the taskbar button and Alt+Tab show it).
    pub fn set_title(&self, title: &str) {
        let title = HSTRING::from(title);
        // SAFETY: plain call on our own window; the text is copied.
        unsafe {
            let _ = SetWindowTextW(self.hwnd, PCWSTR(title.as_ptr()));
        }
    }

    /// Brings a minimised panel back, then [`Panel::activate`]s it.
    pub fn restore_and_activate(&self) -> bool {
        // SAFETY: plain call on our own window.
        unsafe {
            if IsIconic(self.hwnd).as_bool() {
                let _ = ShowWindow(self.hwnd, SW_RESTORE);
            }
        }
        self.activate()
    }

    /// Shows the panel and makes it the foreground, focused window (it then hears keys, and
    /// a click elsewhere deactivates it: see [`take_panel_deactivated`]). False if Windows
    /// refused to give it the foreground.
    pub fn activate(&self) -> bool {
        // SAFETY: plain calls on our own window.
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOW);
            let _ = SetForegroundWindow(self.hwnd);
            let _ = SetFocus(Some(self.hwnd));
            GetForegroundWindow() == self.hwnd
        }
    }

    /// Moves and resizes the panel, redrawing the glass for the new size and `style`.
    pub fn move_to(
        &mut self,
        comp: &Comp,
        rect_px: PxRect,
        dpi: u32,
        style: &MaterialStyle,
    ) -> Result<()> {
        let size = (rect_px.2 - rect_px.0, rect_px.3 - rect_px.1);
        let scale = dpi as f32 / 96.0;
        // SAFETY: plain call on our own window; the z-order is left as it is.
        unsafe {
            SetWindowPos(
                self.hwnd,
                if self.topmost {
                    Some(HWND_TOPMOST)
                } else {
                    None
                },
                rect_px.0,
                rect_px.1,
                size.0,
                size.1,
                SWP_NOACTIVATE | SWP_NOZORDER,
            )?
        };
        let size_v = Vector2::new(size.0 as f32, size.1 as f32);
        self.root.SetSize(size_v)?;
        let glass: Visual =
            backdrop_visual(comp, style, size_v, self.radius_dip * scale)?.cast()?;
        let children = self.root.Children()?;
        children.Remove(&self.glass)?;
        children.InsertAtBottom(&glass)?;
        self.glass = glass;
        self.rect_px = rect_px;
        self.scale = scale;
        self.store_drag();
        Ok(())
    }
}

/// Runs `attempt`; if it fails, runs `restore` before handing the error back.
fn or_restore<T>(attempt: impl FnOnce() -> Result<T>, restore: impl FnOnce()) -> Result<T> {
    attempt().inspect_err(|_| restore())
}

/// Opens a window's `root` visual over `ms` with `motion` (`scale` px per DIP): it fades in
/// while it rises from the bar's side or grows. A composition animation: the compositor runs
/// it, no timer. Shared by the panels and the flyouts.
pub fn animate_open(root: &ContainerVisual, scale: f32, ms: u32, motion: Motion) -> Result<()> {
    let comp = root.Compositor()?;
    let ease =
        comp.CreateCubicBezierEasingFunction(Vector2::new(0.1, 0.9), Vector2::new(0.2, 1.0))?;
    let span = duration(ms);
    let fade = comp.CreateScalarKeyFrameAnimation()?;
    fade.InsertKeyFrame(0.0, 0.0)?;
    fade.InsertKeyFrameWithEasingFunction(1.0, 1.0, &ease)?;
    fade.SetDuration(span)?;
    let mv = motion_animation(root, scale, &comp, motion, true, &ease, span)?;
    root.SetOpacity(0.0)?;
    // Once the root is transparent, any failure must put it back: an invisible panel
    // that still takes the foreground would hold the keyboard with nothing on screen.
    or_restore(
        || {
            root.StartAnimation(h!("Opacity"), &fade)?;
            root.StartAnimation(mv.0, &mv.1)
        },
        || show_static(root),
    )
}

/// The reverse of [`animate_open`]: fades `root` out over `ms` while it slides back /
/// shrinks, starting from what is on screen now (a close during the open animation does not
/// pop).
pub fn animate_close(root: &ContainerVisual, scale: f32, ms: u32, motion: Motion) -> Result<()> {
    // Stop the open animation where it is; the close starts from that state.
    let _ = root.StopAnimation(h!("Opacity"));
    let _ = root.StopAnimation(h!("Scale"));
    let _ = root.StopAnimation(h!("Offset"));
    let opacity = root.Opacity()?;
    let comp = root.Compositor()?;
    let ease =
        comp.CreateCubicBezierEasingFunction(Vector2::new(0.4, 0.0), Vector2::new(1.0, 1.0))?;
    let span = duration(ms);
    let fade = comp.CreateScalarKeyFrameAnimation()?;
    fade.InsertKeyFrame(0.0, opacity)?;
    fade.InsertKeyFrameWithEasingFunction(1.0, 0.0, &ease)?;
    fade.SetDuration(span)?;
    let mv = motion_animation(root, scale, &comp, motion, false, &ease, span)?;
    root.StartAnimation(h!("Opacity"), &fade)?;
    root.StartAnimation(mv.0, &mv.1)
}

/// The position / scale animation of `motion`, opening (`opening`) or closing, with the
/// property it drives.
fn motion_animation(
    root: &ContainerVisual,
    scale: f32,
    comp: &Compositor,
    motion: Motion,
    opening: bool,
    ease: &CubicBezierEasingFunction,
    span: TimeSpan,
) -> Result<(&'static HSTRING, Vector3KeyFrameAnimation)> {
    let anim = comp.CreateVector3KeyFrameAnimation()?;
    let (prop, rest, away) = match motion {
        Motion::Grow => {
            let size = root.Size()?;
            root.SetCenterPoint(Vector3::new(size.X / 2.0, size.Y / 2.0, 0.0))?;
            (
                h!("Scale"),
                Vector3::new(1.0, 1.0, 1.0),
                Vector3::new(0.98, 0.98, 1.0),
            )
        }
        Motion::Rise { from } => {
            // The root never sits anywhere but 0 (as `show_static` assumes).
            let (dx, dy) = rise_offset(from, scale);
            (
                h!("Offset"),
                Vector3::new(0.0, 0.0, 0.0),
                Vector3::new(dx, dy, 0.0),
            )
        }
    };
    let (start, end) = if opening {
        (away, rest)
    } else {
        // From the current state (an open animation may have been cut short).
        let now = if prop == h!("Scale") {
            root.Scale()?
        } else {
            root.Offset()?
        };
        (now, away)
    };
    anim.InsertKeyFrame(0.0, start)?;
    anim.InsertKeyFrameWithEasingFunction(1.0, end, ease)?;
    anim.SetDuration(span)?;
    Ok((prop, anim))
}

/// Puts `root` in its final, fully visible state: animations stopped, opacity 1, scale 1,
/// offset 0 (the root never sits anywhere else). Best effort (each step is tried even if an
/// earlier one fails).
pub fn show_static(root: &ContainerVisual) {
    let _ = root.StopAnimation(h!("Opacity"));
    let _ = root.StopAnimation(h!("Scale"));
    let _ = root.StopAnimation(h!("Offset"));
    let _ = root.SetOpacity(1.0);
    let _ = root.SetScale(Vector3::new(1.0, 1.0, 1.0));
    let _ = root.SetOffset(Vector3::new(0.0, 0.0, 0.0));
}

impl Drop for Panel {
    fn drop(&mut self) {
        // SAFETY: destroying our own window if it still exists.
        unsafe {
            if IsWindow(Some(self.hwnd)).as_bool() {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_animation_start_restores_the_panel() {
        use std::cell::Cell;
        let restored = Cell::new(false);
        let r: Result<()> =
            or_restore(|| Err(windows::core::Error::empty()), || restored.set(true));
        assert!(r.is_err());
        assert!(restored.get(), "a failure must restore opacity and scale");
        restored.set(false);
        let r = or_restore(|| Ok(7), || restored.set(true));
        assert_eq!(r.unwrap(), 7);
        assert!(
            !restored.get(),
            "a success must leave the animation running"
        );
    }

    #[test]
    fn rise_starts_on_the_bar_side() {
        assert_eq!(rise_offset(Position::Bottom, 2.0), (0.0, 24.0));
        assert_eq!(rise_offset(Position::Top, 2.0), (0.0, -24.0));
    }

    #[test]
    fn drag_band_leaves_the_close_button_to_the_client() {
        let packed = pack_drag(40, 46);
        // Inside the band, left of the button.
        assert!(in_drag_band(packed, 720, 10, 10));
        assert!(in_drag_band(packed, 720, 673, 39));
        // The button, and below the band.
        assert!(!in_drag_band(packed, 720, 674, 10));
        assert!(!in_drag_band(packed, 720, 10, 40));
        assert!(!in_drag_band(packed, 720, 10, -1));
        // No band at all.
        assert!(!in_drag_band(0, 720, 10, 10));
        assert!(in_drag_band(pack_drag(40, 0), 720, 719, 0));
    }
}
