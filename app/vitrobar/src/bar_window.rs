//! The bar's top-level windows (one per glass surface): borderless, non-activating, topmost
//! tool windows with no redirection bitmap, whose content is a composition visual tree.
//! Also maps cursor positions to bar items (hit testing, in DIPs).

use std::sync::OnceLock;

use vitro_core::layout::{BarLayout, Rect};
use vitro_render::compositor::{window_target, Comp};
use vitro_render::Vector2;
use windows::core::{w, Error, Interface, Result, BOOL, HSTRING, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_USE_HOSTBACKDROPBRUSH, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_DONOTROUND,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, IsWindow, LoadCursorW, PostQuitMessage,
    RegisterClassExW, SetWindowPos, ShowWindow, HWND_TOPMOST, IDC_ARROW, MA_NOACTIVATE,
    SWP_NOACTIVATE, SW_SHOWNOACTIVATE, WM_DESTROY, WM_MOUSEACTIVATE, WNDCLASSEXW, WS_EX_NOACTIVATE,
    WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{ContainerVisual, Visual};

/// Window class of every bar window (ruling R2).
pub const BAR_CLASS: &str = "VitrobarBar";

/// `(left, top, right, bottom)` in physical pixels.
pub type PxRect = (i32, i32, i32, i32);

pub struct BarWindow {
    pub hwnd: HWND,
    _target: DesktopWindowTarget,
    root: ContainerVisual,
    rect_px: PxRect,
}

fn register_class() -> Result<()> {
    // The outcome of the first registration is kept, failure included.
    static RESULT: OnceLock<Result<()>> = OnceLock::new();
    RESULT
        .get_or_init(|| unsafe {
            // SAFETY: plain class registration with a static window procedure.
            let class = HSTRING::from(BAR_CLASS);
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wndproc),
                hInstance: GetModuleHandleW(None)?.into(),
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                lpszClassName: PCWSTR(class.as_ptr()),
                ..Default::default()
            };
            if RegisterClassExW(&wc) == 0 {
                return Err(Error::from_thread());
            }
            Ok(())
        })
        .clone()
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // Mouse input is handled by the app's message loop before dispatch; theme, time and
    // locale broadcasts go to the notify window.
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_DESTROY => {
            // SAFETY: plain call. A bar destroyed from outside (WM_CLOSE) ends the app.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // SAFETY: default handling with the arguments we were given.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// The window starts hidden.
pub fn create_bar_window(comp: &Comp, rect_px: PxRect) -> Result<BarWindow> {
    register_class()?;
    let (l, t, r, b) = rect_px;
    let class = HSTRING::from(BAR_CLASS);
    // SAFETY: the class is registered and the strings outlive the call.
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOREDIRECTIONBITMAP | WS_EX_NOACTIVATE,
            PCWSTR(class.as_ptr()),
            w!("Vitrobar"),
            WS_POPUP,
            l,
            t,
            r - l,
            b - t,
            None,
            None,
            Some(GetModuleHandleW(None)?.into()),
            None,
        )?
    };
    let finish = || -> Result<BarWindow> {
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
        root.SetSize(Vector2::new((r - l) as f32, (b - t) as f32))?;
        target.SetRoot(&root)?;
        Ok(BarWindow {
            hwnd,
            _target: target,
            root,
            rect_px,
        })
    };
    // SAFETY: destroying the window we just created.
    finish().inspect_err(|_| unsafe {
        let _ = DestroyWindow(hwnd);
    })
}

impl BarWindow {
    /// Replaces everything drawn in the window with `visual`.
    pub fn set_content(&self, visual: &impl Interface) -> Result<()> {
        let children = self.root.Children()?;
        children.RemoveAll()?;
        children.InsertAtTop(&visual.cast::<Visual>()?)
    }

    pub fn rect_px(&self) -> PxRect {
        self.rect_px
    }

    /// Moves / resizes the window (no activation, stays topmost).
    pub fn set_rect(&mut self, rect_px: PxRect) -> Result<()> {
        if rect_px == self.rect_px {
            return Ok(());
        }
        let (l, t, r, b) = rect_px;
        // SAFETY: plain call on our own window.
        unsafe {
            SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                l,
                t,
                r - l,
                b - t,
                SWP_NOACTIVATE,
            )?
        };
        self.root
            .SetSize(Vector2::new((r - l) as f32, (b - t) as f32))?;
        self.rect_px = rect_px;
        Ok(())
    }

    pub fn show(&self) {
        // SAFETY: plain call on our own window.
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
    }

    /// Asks for one `WM_MOUSELEAVE` when the cursor leaves the window.
    pub fn track_leave(&self) {
        let mut tme = TRACKMOUSEEVENT {
            cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: self.hwnd,
            dwHoverTime: 0,
        };
        // SAFETY: `tme` is a valid local for the duration of the call.
        unsafe {
            let _ = TrackMouseEvent(&mut tme);
        }
    }

    /// Client-area pixel position -> absolute DIP position.
    pub fn client_to_dip(&self, x: i32, y: i32, scale: f32) -> (f32, f32) {
        (
            (self.rect_px.0 + x) as f32 / scale,
            (self.rect_px.1 + y) as f32 / scale,
        )
    }
}

impl Drop for BarWindow {
    fn drop(&mut self) {
        // SAFETY: destroying our own window if it still exists.
        unsafe {
            if IsWindow(Some(self.hwnd)).as_bool() {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }
}

/// What the cursor is over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Start,
    /// Index into `layout.apps` (= index into the app groups).
    App(usize),
    More,
    Quick,
    Clock,
    ShowDesktop,
}

fn inside(r: &Rect, x: f32, y: f32) -> bool {
    x >= r.x && x < r.right() && y >= r.y && y < r.bottom()
}

/// Maps an absolute DIP position to the bar item under it.
pub fn hit_test(l: &BarLayout, x: f32, y: f32) -> Option<Hit> {
    if inside(&l.show_desktop, x, y) {
        return Some(Hit::ShowDesktop);
    }
    if inside(&l.start, x, y) {
        return Some(Hit::Start);
    }
    if let Some(i) = l.apps.iter().position(|r| inside(r, x, y)) {
        return Some(Hit::App(i));
    }
    if l.more.as_ref().is_some_and(|r| inside(r, x, y)) {
        return Some(Hit::More);
    }
    if inside(&l.quick, x, y) {
        return Some(Hit::Quick);
    }
    if inside(&l.clock, x, y) {
        return Some(Hit::Clock);
    }
    None
}

/// Signed client coordinates packed in a mouse message's `lParam`.
pub fn mouse_pos(lparam: LPARAM) -> (i32, i32) {
    let v = lparam.0 as u32;
    (
        i32::from((v & 0xFFFF) as u16 as i16),
        i32::from((v >> 16) as u16 as i16),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use vitro_core::layout::{compute_layout, LayoutInput};
    use vitro_core::settings::{IconSize, Position, Shape};

    fn layout(n: usize) -> BarLayout {
        compute_layout(&LayoutInput {
            monitor: Rect {
                x: 0.0,
                y: 0.0,
                w: 1536.0,
                h: 864.0,
            },
            shape: Shape::Pill,
            position: Position::Bottom,
            icon_size: IconSize::Medium,
            app_count: n,
            tray_count: 0,
            clock_w: 70.0,
            reserve_floating: true,
        })
    }

    fn centre(r: &Rect) -> (f32, f32) {
        (r.x + r.w / 2.0, r.y + r.h / 2.0)
    }

    #[test]
    fn centres_hit_their_items() {
        let l = layout(3);
        let (x, y) = centre(&l.start);
        assert_eq!(hit_test(&l, x, y), Some(Hit::Start));
        for (i, r) in l.apps.iter().enumerate() {
            let (x, y) = centre(r);
            assert_eq!(hit_test(&l, x, y), Some(Hit::App(i)));
        }
        let (x, y) = centre(&l.quick);
        assert_eq!(hit_test(&l, x, y), Some(Hit::Quick));
        let (x, y) = centre(&l.clock);
        assert_eq!(hit_test(&l, x, y), Some(Hit::Clock));
        let (x, y) = centre(&l.show_desktop);
        assert_eq!(hit_test(&l, x, y), Some(Hit::ShowDesktop));
    }

    #[test]
    fn outside_and_gaps_hit_nothing() {
        let l = layout(2);
        assert_eq!(hit_test(&l, 0.0, 0.0), None);
        // Above the slots but inside the surface (the padding row).
        let s = l.surfaces[0];
        let (x, _) = centre(&l.apps[0]);
        assert_eq!(hit_test(&l, x, s.y + 1.0), None);
    }

    #[test]
    fn adjacent_apps_split_at_the_boundary() {
        let l = layout(2);
        let edge = l.apps[1].x;
        let y = l.apps[0].y + 1.0;
        assert_eq!(hit_test(&l, edge - 0.01, y), Some(Hit::App(0)));
        assert_eq!(hit_test(&l, edge, y), Some(Hit::App(1)));
    }

    #[test]
    fn mouse_pos_is_signed() {
        let lp = LPARAM(((0xFFFEu32 << 16) | 0x0005) as isize);
        assert_eq!(mouse_pos(lp), (5, -2));
    }
}
