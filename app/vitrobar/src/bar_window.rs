//! The bar's top-level window: a borderless, non-activating, topmost tool window with no
//! redirection bitmap, whose content is a composition visual tree.

use std::sync::Once;

use vitro_render::compositor::{window_target, Comp};
use vitro_render::Vector2;
use windows::core::{w, Interface, Result, BOOL, HSTRING, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_USE_HOSTBACKDROPBRUSH, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_DONOTROUND,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, IsWindow, LoadCursorW, PostMessageW,
    PostQuitMessage, RegisterClassExW, ShowWindow, IDC_ARROW, MA_NOACTIVATE, SW_SHOWNOACTIVATE,
    WM_APP, WM_DESTROY, WM_DWMCOLORIZATIONCOLORCHANGED, WM_MOUSEACTIVATE, WM_SETTINGCHANGE,
    WM_THEMECHANGED, WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{ContainerVisual, Visual};

/// Window class of every bar window (ruling R2).
pub const BAR_CLASS: &str = "VitrobarBar";

/// Posted to the bar window when the system colour mode / transparency / accent may have
/// changed. The message loop coalesces these and re-reads the theme once.
pub const WM_APP_THEME: u32 = WM_APP + 1;

pub struct BarWindow {
    pub hwnd: HWND,
    _target: DesktopWindowTarget,
    root: ContainerVisual,
}

fn register_class() -> Result<()> {
    static ONCE: Once = Once::new();
    let mut result = Ok(());
    ONCE.call_once(|| {
        result = (|| unsafe {
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
                return Err(windows::core::Error::from_thread());
            }
            Ok(())
        })();
    });
    result
}

fn is_theme_setting(lparam: LPARAM) -> bool {
    if lparam.0 == 0 {
        return false;
    }
    // SAFETY: for WM_SETTINGCHANGE a non-null lParam is a NUL-terminated wide string.
    let area = unsafe { PCWSTR(lparam.0 as *const u16).to_string() };
    area.is_ok_and(|s| s == "ImmersiveColorSet")
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let theme_changed = match msg {
        WM_SETTINGCHANGE => is_theme_setting(lparam),
        WM_DWMCOLORIZATIONCOLORCHANGED | WM_THEMECHANGED => true,
        _ => false,
    };
    if theme_changed {
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_APP_THEME, WPARAM(0), LPARAM(0));
        }
    }
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// `rect_px` is `(left, top, right, bottom)` in physical pixels. The window starts hidden.
pub fn create_bar_window(comp: &Comp, rect_px: (i32, i32, i32, i32)) -> Result<BarWindow> {
    register_class()?;
    let (l, t, r, b) = rect_px;
    let class = HSTRING::from(BAR_CLASS);
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
        })
    };
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

    pub fn show(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
    }
}

impl Drop for BarWindow {
    fn drop(&mut self) {
        unsafe {
            if IsWindow(Some(self.hwnd)).as_bool() {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }
}
