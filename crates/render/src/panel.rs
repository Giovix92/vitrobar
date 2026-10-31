//! An activatable glass top-level window: the Start panel and the settings window.
//!
//! Unlike the [`crate::flyout::Flyout`] it takes focus when clicked (it hears the keyboard)
//! and reports when it loses activation, so the app can close it on a click elsewhere.

use std::cell::RefCell;
use std::sync::Mutex;

use vitro_core::style::MaterialStyle;
use windows::core::{h, w, Error, Interface, Result, BOOL, HSTRING, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_USE_HOSTBACKDROPBRUSH, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_DONOTROUND,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetForegroundWindow, GetWindowLongPtrW,
    GetWindowRect, IsWindow, LoadCursorW, RegisterClassExW, SetForegroundWindow, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, GWLP_USERDATA, HTCAPTION, HTCLIENT, HWND_TOPMOST, IDC_ARROW,
    MA_ACTIVATE, SWP_NOACTIVATE, SWP_NOZORDER, SW_SHOW, WA_INACTIVE, WM_ACTIVATE, WM_MOUSEACTIVATE,
    WM_NCHITTEST, WNDCLASSEXW, WS_EX_APPWINDOW, WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{ContainerVisual, Visual};
use windows_numerics::{Vector2, Vector3};

use crate::compositor::{window_target, Comp};
use crate::flyout::PxRect;
use crate::material::backdrop_visual;

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
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wndproc),
                hInstance: GetModuleHandleW(None)?.into(),
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
}

/// Panels that were deactivated since the last call: the user clicked elsewhere or
/// switched apps.
pub fn take_panel_deactivated() -> Vec<HWND> {
    PANEL_DEACTIVATED.with(|d| std::mem::take(&mut *d.borrow_mut()))
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        // The panel takes focus when clicked.
        WM_MOUSEACTIVATE => LRESULT(MA_ACTIVATE as isize),
        WM_ACTIVATE => {
            if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE {
                PANEL_DEACTIVATED.with(|d| d.borrow_mut().push(hwnd));
            }
            // SAFETY: default handling with the arguments we were given.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_NCHITTEST => {
            // SAFETY: the user data is the drag band height in px, set by `set_drag_height`
            // (0 when never set); GetWindowRect writes into a local.
            let drag = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as i32;
            let mut r = RECT::default();
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            if drag > 0 && unsafe { GetWindowRect(hwnd, &mut r) }.is_ok() && y - r.top < drag {
                LRESULT(HTCAPTION as isize)
            } else {
                LRESULT(HTCLIENT as isize)
            }
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

    /// Fades the panel in over `ms` and scales it from 98 % to 100 % about its centre: a
    /// composition animation (the compositor runs it, no timer). Call right after `show`.
    pub fn animate_open(&self, ms: u32) -> Result<()> {
        use windows::Foundation::TimeSpan;
        let comp = self.root.Compositor()?;
        let size = self.root.Size()?;
        self.root
            .SetCenterPoint(Vector3::new(size.X / 2.0, size.Y / 2.0, 0.0))?;
        let ease =
            comp.CreateCubicBezierEasingFunction(Vector2::new(0.1, 0.9), Vector2::new(0.2, 1.0))?;
        let span = TimeSpan::from(std::time::Duration::from_millis(ms as u64));
        let fade = comp.CreateScalarKeyFrameAnimation()?;
        fade.InsertKeyFrame(0.0, 0.0)?;
        fade.InsertKeyFrameWithEasingFunction(1.0, 1.0, &ease)?;
        fade.SetDuration(span)?;
        let grow = comp.CreateVector3KeyFrameAnimation()?;
        grow.InsertKeyFrame(0.0, Vector3::new(0.98, 0.98, 1.0))?;
        grow.InsertKeyFrameWithEasingFunction(1.0, Vector3::new(1.0, 1.0, 1.0), &ease)?;
        grow.SetDuration(span)?;
        self.root.SetOpacity(0.0)?;
        self.root.StartAnimation(h!("Opacity"), &fade)?;
        self.root.StartAnimation(h!("Scale"), &grow)
    }

    /// Makes the top `dip` band of the panel a window caption (drag to move). Default 0.
    pub fn set_drag_height(&self, dip: f32) {
        let px = (dip.max(0.0) * self.scale).round() as isize;
        // SAFETY: plain call on our own window; the value is read back by `wndproc`.
        unsafe {
            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, px);
        }
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
        Ok(())
    }
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
