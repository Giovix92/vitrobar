//! The bar's top-level windows (one per glass surface): borderless, non-activating, topmost
//! tool windows with no redirection bitmap, whose content is a composition visual tree.
//! Also maps cursor positions to bar items (hit testing, in DIPs).
//!
//! A window can carry a transparent margin beyond the bar's outer edge (Magnify grows icons
//! past it). The margin is outside the window region, so it is click-through, except while
//! [`BarWindow::set_expanded`] says an icon may be drawn there. When the hover ends the
//! margin keeps drawing (so the shrink-back is not cut) but stops taking input: see
//! [`MarginMode`].

use std::cell::Cell;
use std::sync::Mutex;

use vitro_core::layout::{BarLayout, Rect};
use vitro_render::compositor::{window_target, Comp};
use vitro_render::{Vector2, Vector3};
use vitro_win::appbar::WM_VITRO_APPBAR;
use windows::core::{w, Error, Interface, Result, BOOL, HSTRING, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_TRANSITIONS_FORCEDISABLED, DWMWA_USE_HOSTBACKDROPBRUSH,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND,
};
use windows::Win32::Graphics::Gdi::{
    CreateRectRgn, DeleteObject, ScreenToClient, SetWindowRgn, HRGN,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetWindowLongPtrW, GetWindowRect, IsWindow,
    LoadCursorW, PostQuitMessage, RegisterClassExW, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    GWLP_USERDATA, HWND_TOPMOST, IDC_ARROW, MA_NOACTIVATE, SWP_NOACTIVATE, SWP_NOZORDER, SW_HIDE,
    SW_SHOWNOACTIVATE, WM_DESTROY, WM_DPICHANGED, WM_MOUSEACTIVATE, WM_NCHITTEST, WNDCLASSEXW,
    WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{ContainerVisual, Visual};

/// Window class of every bar window (ruling R2).
pub const BAR_CLASS: &str = "VitrobarBar";

/// `(left, top, right, bottom)` in physical pixels.
pub type PxRect = (i32, i32, i32, i32);

/// How a bar window treats its transparent margins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarginMode {
    /// Window region = the bar alone: margins neither draw nor take input.
    Clipped,
    /// Whole window: an icon may be drawn in the margins, which also take input.
    Active,
    /// The hover ended and the shrink-back is still animating: margins keep drawing but
    /// are transparent to hit testing (`WM_NCHITTEST` answers `HTTRANSPARENT`).
    Lingering,
}

impl MarginMode {
    /// Mode after a resize. A margin that just appeared starts `Clipped` (never inherits an
    /// `Active` left over from before it was removed, which would make the transparent
    /// margin swallow clicks); no margin at all is also `Clipped`.
    pub fn after_resize(self, had_margin: bool, margin: (i32, i32)) -> Self {
        if margin == (0, 0) || !had_margin {
            Self::Clipped
        } else {
            self
        }
    }

    /// Hover started (`true`) or ended without waiting (`false`).
    pub fn set_expanded(self, on: bool) -> Self {
        if on {
            Self::Active
        } else {
            Self::Clipped
        }
    }

    /// Hover ended: keep drawing, stop taking input.
    pub fn begin_linger(self) -> Self {
        if self == Self::Active {
            Self::Lingering
        } else {
            self
        }
    }

    /// The shrink-back is over.
    pub fn finish_linger(self) -> Self {
        if self == Self::Lingering {
            Self::Clipped
        } else {
            self
        }
    }

    /// No window region at all (the whole window draws).
    fn whole_window(self, margin: (i32, i32)) -> bool {
        margin == (0, 0) || self != Self::Clipped
    }
}

/// `GWLP_USERDATA` payload telling `WM_NCHITTEST` where the bar is while lingering
/// (0 = not lingering): bit 0 set, bar top in bits 1..32, bar height in bits 32..64.
fn pack_linger(top: i32, h: i32) -> isize {
    (1u64 | (((top.max(0) as u64) << 1) & 0xFFFF_FFFE) | ((h.max(0) as u64) << 32)) as isize
}

/// Whether client y lies in a margin, given a [`pack_linger`] value.
fn in_margin(packed: isize, y: i32) -> bool {
    if packed == 0 {
        return false;
    }
    let p = packed as u64;
    let top = ((p & 0xFFFF_FFFF) >> 1) as i32;
    let h = (p >> 32) as i32;
    y < top || y >= top + h
}

pub struct BarWindow {
    pub hwnd: HWND,
    _target: DesktopWindowTarget,
    root: ContainerVisual,
    /// Holds the content, offset by the top margin.
    content: ContainerVisual,
    /// The bar itself, without margins.
    rect_px: PxRect,
    /// Extra px above and below the bar.
    margin: (i32, i32),
    mode: MarginMode,
}

fn window_rect(r: PxRect, m: (i32, i32)) -> PxRect {
    (r.0, r.1 - m.0, r.2, r.3 + m.1)
}

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
        }
    })();
    if registered.is_ok() {
        *done = true;
    }
    registered
}

thread_local! {
    /// Set while [`BarWindow`]'s drop destroys its window: a bar the app removes itself
    /// (fewer surfaces after a shape change) must not end the app.
    static DROPPING: Cell<bool> = const { Cell::new(false) };
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // Mouse input is handled by the app's message loop before dispatch; theme, time and
    // locale broadcasts go to the notify window.
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_NCHITTEST => {
            // SAFETY: plain calls on our own window.
            unsafe {
                let packed = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
                if packed != 0 {
                    let mut p = POINT {
                        x: (lparam.0 & 0xFFFF) as i16 as i32,
                        y: ((lparam.0 >> 16) & 0xFFFF) as i16 as i32,
                    };
                    if ScreenToClient(hwnd, &mut p).as_bool() && in_margin(packed, p.y) {
                        return LRESULT(-1); // HTTRANSPARENT
                    }
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        }
        WM_DPICHANGED => {
            // The suggested rectangle is not applied: the app re-reads the monitors and lays
            // out this monitor's bar (only it) at the new DPI, which places this window.
            crate::notify::record_dpi();
            LRESULT(0)
        }
        WM_VITRO_APPBAR => {
            // Older bars were appbars themselves (now `AppbarHost`); kept for any late notice.
            crate::notify::record_appbar(wparam.0 as u32);
            LRESULT(0)
        }
        WM_DESTROY => {
            if !DROPPING.get() {
                // SAFETY: plain call. A bar destroyed from outside (WM_CLOSE) ends the app.
                unsafe { PostQuitMessage(0) };
            }
            LRESULT(0)
        }
        // SAFETY: default handling with the arguments we were given.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// The window starts hidden. `margin`: extra px (above, below) the bar.
pub fn create_bar_window(comp: &Comp, rect_px: PxRect, margin: (i32, i32)) -> Result<BarWindow> {
    register_class()?;
    let (l, t, r, b) = window_rect(rect_px, margin);
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
            // Hiding for a full-screen app and showing again must not animate.
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_TRANSITIONS_FORCEDISABLED,
                &on as *const BOOL as *const _,
                size_of::<BOOL>() as u32,
            )?;
        }
        let target = window_target(comp, hwnd)?;
        let root = comp.CreateContainerVisual()?;
        let content = comp.CreateContainerVisual()?;
        root.Children()?.InsertAtTop(&content)?;
        target.SetRoot(&root)?;
        let w = BarWindow {
            hwnd,
            _target: target,
            root,
            content,
            rect_px,
            margin,
            mode: MarginMode::Clipped,
        };
        w.layout_visuals()?;
        w.apply_region();
        Ok(w)
    };
    // SAFETY: destroying the window we just created.
    finish().inspect_err(|_| unsafe {
        let _ = DestroyWindow(hwnd);
    })
}

impl BarWindow {
    /// Fades the bar out in place, above the bar window replacing it (energy saver
    /// changed: see `App::glass_gen`). The owner drops it once the fade has run.
    pub fn fade_out(&self, ms: u32) -> Result<()> {
        vitro_render::panel::raise_topmost(self.hwnd);
        vitro_render::panel::fade_out(&self.root, ms)
    }

    /// Replaces everything drawn in the window with `visual`.
    pub fn set_content(&self, visual: &impl Interface) -> Result<()> {
        let children = self.content.Children()?;
        children.RemoveAll()?;
        children.InsertAtTop(&visual.cast::<Visual>()?)
    }

    /// The bar's rectangle (margins excluded).
    pub fn rect_px(&self) -> PxRect {
        self.rect_px
    }

    /// The margins currently take input (see [`BarWindow::set_expanded`]).
    pub fn expanded(&self) -> bool {
        self.mode == MarginMode::Active && self.margin != (0, 0)
    }

    pub fn mode(&self) -> MarginMode {
        self.mode
    }

    fn layout_visuals(&self) -> Result<()> {
        let (l, t, r, b) = window_rect(self.rect_px, self.margin);
        self.root
            .SetSize(Vector2::new((r - l) as f32, (b - t) as f32))?;
        self.content
            .SetOffset(Vector3::new(0.0, self.margin.0 as f32, 0.0))?;
        let (w, h) = (
            self.rect_px.2 - self.rect_px.0,
            self.rect_px.3 - self.rect_px.1,
        );
        self.content.SetSize(Vector2::new(w as f32, h as f32))
    }

    /// Whether the margins take mouse input and show content (while an icon is magnified).
    /// Collapsed, the window region is the bar alone and the margins are click-through.
    pub fn set_expanded(&mut self, on: bool) {
        self.set_mode(self.mode.set_expanded(on));
    }

    /// The hover ended: the margins keep showing content (the icon is shrinking back) but
    /// stop taking input. Follow with [`BarWindow::finish_linger`] after the motion.
    pub fn begin_linger(&mut self) {
        self.set_mode(self.mode.begin_linger());
    }

    pub fn finish_linger(&mut self) {
        self.set_mode(self.mode.finish_linger());
    }

    fn set_mode(&mut self, mode: MarginMode) {
        if self.margin == (0, 0) || mode == self.mode {
            return;
        }
        self.mode = mode;
        self.apply_region();
    }

    fn apply_region(&self) {
        let (w, h) = (
            self.rect_px.2 - self.rect_px.0,
            self.rect_px.3 - self.rect_px.1,
        );
        // SAFETY: plain calls on our own window. On success the system owns the region; on
        // failure it is still ours and is deleted. A null region means the whole window.
        unsafe {
            let lingering = self.mode == MarginMode::Lingering && self.margin != (0, 0);
            SetWindowLongPtrW(
                self.hwnd,
                GWLP_USERDATA,
                if lingering {
                    pack_linger(self.margin.0, h)
                } else {
                    0
                },
            );
            let rgn = if self.mode.whole_window(self.margin) {
                HRGN::default()
            } else {
                CreateRectRgn(0, self.margin.0, w, self.margin.0 + h)
            };
            if SetWindowRgn(self.hwnd, Some(rgn), true) == 0 && !rgn.is_invalid() {
                let _ = DeleteObject(rgn.into());
            }
        }
    }

    /// Moves / resizes the window (no activation, stays topmost).
    pub fn set_rect(&mut self, rect_px: PxRect, margin: (i32, i32)) -> Result<()> {
        if rect_px == self.rect_px && margin == self.margin {
            return Ok(());
        }
        let (l, t, r, b) = window_rect(rect_px, margin);
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
        let had_margin = self.margin != (0, 0);
        self.rect_px = rect_px;
        self.margin = margin;
        self.mode = self.mode.after_resize(had_margin, margin);
        self.layout_visuals()?;
        if margin != (0, 0) || had_margin {
            // The region follows the new size (or is removed with the margin).
            self.apply_region();
        }
        Ok(())
    }

    pub fn show(&self) {
        // SAFETY: plain call on our own window.
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
    }

    /// Hides the window at once (DWM transitions are disabled on bar windows).
    pub fn hide(&self) {
        // SAFETY: plain call on our own window.
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    /// Asks for one `WM_MOUSELEAVE` when the cursor leaves the window.
    pub fn track_leave(&self) {
        track_leave(self.hwnd);
    }

    /// Client-area pixel position -> absolute DIP position.
    pub fn client_to_dip(&self, x: i32, y: i32, scale: f32) -> (f32, f32) {
        client_to_dip(self.rect_px, self.margin, x, y, scale)
    }

    /// Puts the window back where it belongs if something else moved it (the system moves
    /// windows around while monitors come and go). True if it had to be moved.
    pub fn resync(&mut self) -> bool {
        let want = window_rect(self.rect_px, self.margin);
        let mut r = RECT::default();
        // SAFETY: plain query on our own window into a local.
        if unsafe { GetWindowRect(self.hwnd, &mut r) }.is_err()
            || (r.left, r.top, r.right, r.bottom) == want
        {
            return false;
        }
        let (l, t, rr, b) = want;
        // SAFETY: plain call on our own window.
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                l,
                t,
                rr - l,
                b - t,
                SWP_NOACTIVATE,
            );
        }
        true
    }
}

/// Client px `(x, y)` of a window showing bar `rect_px` with `margin` (above, below) ->
/// absolute DIPs at `scale` (DPI / 96).
pub fn client_to_dip(
    rect_px: PxRect,
    margin: (i32, i32),
    x: i32,
    y: i32,
    scale: f32,
) -> (f32, f32) {
    (
        (rect_px.0 + x) as f32 / scale,
        (rect_px.1 - margin.0 + y) as f32 / scale,
    )
}

/// Asks for one `WM_MOUSELEAVE` when the cursor leaves `hwnd` (one of our windows).
pub fn track_leave(hwnd: HWND) {
    let mut tme = TRACKMOUSEEVENT {
        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE,
        hwndTrack: hwnd,
        dwHoverTime: 0,
    };
    // SAFETY: `tme` is a valid local for the duration of the call.
    unsafe {
        let _ = TrackMouseEvent(&mut tme);
    }
}

impl Drop for BarWindow {
    fn drop(&mut self) {
        // SAFETY: destroying our own window if it still exists (on its own thread: the
        // drop runs on the UI thread, so WM_DESTROY is handled within the call).
        unsafe {
            if IsWindow(Some(self.hwnd)).as_bool() {
                DROPPING.set(true);
                let _ = DestroyWindow(self.hwnd);
                DROPPING.set(false);
            }
        }
    }
}

/// Window class of the appbar hosts.
const APPBAR_HOST_CLASS: &str = "VitrobarAppbarHost";

/// A hidden window that holds one monitor's appbar (the reserved strip of the screen) in
/// place of the bar window. Bar windows can then be replaced (energy saver changed: DWM
/// gives a window its glass only when it is made) without giving the strip back, which
/// would make maximized windows jump. Never shown; it sits on the strip so the shell
/// places it on the right monitor.
pub struct AppbarHost {
    pub hwnd: HWND,
}

unsafe extern "system" fn host_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_VITRO_APPBAR {
        crate::notify::record_appbar(wparam.0 as u32);
        return LRESULT(0);
    }
    // SAFETY: default handling with the arguments we were given.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn register_host_class() -> Result<()> {
    static DONE: Mutex<bool> = Mutex::new(false);
    let mut done = DONE.lock().unwrap_or_else(|e| e.into_inner());
    if *done {
        return Ok(());
    }
    let class = HSTRING::from(APPBAR_HOST_CLASS);
    // SAFETY: plain class registration with a static window procedure.
    unsafe {
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(host_wndproc),
            hInstance: GetModuleHandleW(None)?.into(),
            lpszClassName: PCWSTR(class.as_ptr()),
            ..Default::default()
        };
        if RegisterClassExW(&wc) == 0 {
            return Err(Error::from_thread());
        }
    }
    *done = true;
    Ok(())
}

impl AppbarHost {
    pub fn new() -> Result<Self> {
        register_host_class()?;
        let class = HSTRING::from(APPBAR_HOST_CLASS);
        // SAFETY: a hidden top-level window of our own class.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                PCWSTR(class.as_ptr()),
                w!("Vitrobar appbar"),
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

    /// Moves the (hidden) window onto the strip it reserves.
    pub fn place(&self, r: PxRect) {
        // SAFETY: plain call on our own window; it stays hidden.
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                None,
                r.0,
                r.1,
                r.2 - r.0,
                r.3 - r.1,
                SWP_NOACTIVATE | SWP_NOZORDER,
            );
        }
    }
}

impl Drop for AppbarHost {
    fn drop(&mut self) {
        // SAFETY: destroying our own window.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
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
    /// The tray overflow chevron.
    Chevron,
    /// Index into `layout.tray` (the promoted tray icons).
    Tray(usize),
    Quick,
    Clock,
    /// Index into `layout.widgets` (0 = Music, 1 = Claude limits).
    Widget(usize),
    /// The input-language indicator.
    Ime,
    /// The battery indicator (it opens the quick panel, as the stock one does).
    Battery,
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
    if inside(&l.overflow_chevron, x, y) {
        return Some(Hit::Chevron);
    }
    if let Some(i) = l.tray.iter().position(|r| inside(r, x, y)) {
        return Some(Hit::Tray(i));
    }
    if let Some(i) = l
        .widgets
        .iter()
        .position(|r| r.as_ref().is_some_and(|r| inside(r, x, y)))
    {
        return Some(Hit::Widget(i));
    }
    if l.ime.as_ref().is_some_and(|r| inside(r, x, y)) {
        return Some(Hit::Ime);
    }
    if l.battery.as_ref().is_some_and(|r| inside(r, x, y)) {
        return Some(Hit::Battery);
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
        compute_layout(&input(n))
    }

    fn input(n: usize) -> LayoutInput {
        LayoutInput {
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
            tray_area: true,
            widgets: [0.0; 2],
            ime: 0.0,
            battery: 0.0,
        }
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
        let (x, y) = centre(&l.overflow_chevron);
        assert_eq!(hit_test(&l, x, y), Some(Hit::Chevron));
        let (x, y) = centre(&l.clock);
        assert_eq!(hit_test(&l, x, y), Some(Hit::Clock));
        let (x, y) = centre(&l.show_desktop);
        assert_eq!(hit_test(&l, x, y), Some(Hit::ShowDesktop));
    }

    #[test]
    fn tray_icons_hit_their_slots() {
        let l = compute_layout(&LayoutInput {
            tray_count: 3,
            ..input(2)
        });
        for (i, r) in l.tray.iter().enumerate() {
            let (x, y) = centre(r);
            assert_eq!(hit_test(&l, x, y), Some(Hit::Tray(i)));
        }
        let (x, y) = centre(&l.quick);
        assert_eq!(hit_test(&l, x, y), Some(Hit::Quick));
    }

    #[test]
    fn widget_pills_hit_their_index() {
        let l = compute_layout(&LayoutInput {
            widgets: [100.0, 80.0],
            ime: 0.0,
            battery: 0.0,
            ..input(2)
        });
        for i in 0..2 {
            let (x, y) = centre(&l.widgets[i].unwrap());
            assert_eq!(hit_test(&l, x, y), Some(Hit::Widget(i)));
        }
    }

    #[test]
    fn the_ime_indicator_is_hit() {
        let l = compute_layout(&LayoutInput {
            ime: 30.0,
            battery: 0.0,
            ..input(2)
        });
        let (x, y) = centre(&l.ime.unwrap());
        assert_eq!(hit_test(&l, x, y), Some(Hit::Ime));
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
    fn margin_appearing_after_hover_starts_clipped() {
        // 0 -> non-zero margin after an Active hover: must not inherit Active, or the
        // transparent margin would swallow clicks.
        let m = MarginMode::Active.after_resize(false, (30, 0));
        assert_eq!(m, MarginMode::Clipped);
        assert!(!m.whole_window((30, 0)));
        // Margin removed: clipped state, whole window (nothing to clip).
        let m = MarginMode::Active.after_resize(true, (0, 0));
        assert_eq!(m, MarginMode::Clipped);
        assert!(m.whole_window((0, 0)));
        // A plain resize with a margin keeps an ongoing hover.
        assert_eq!(
            MarginMode::Active.after_resize(true, (30, 0)),
            MarginMode::Active
        );
    }

    #[test]
    fn linger_keeps_drawing_but_not_input() {
        let m = MarginMode::Active.begin_linger();
        assert_eq!(m, MarginMode::Lingering);
        assert!(m.whole_window((30, 0)), "margin keeps drawing");
        assert_eq!(MarginMode::Clipped.begin_linger(), MarginMode::Clipped);
        assert_eq!(m.finish_linger(), MarginMode::Clipped);
        assert_eq!(MarginMode::Active.finish_linger(), MarginMode::Active);
        assert_eq!(m.set_expanded(true), MarginMode::Active, "hover again");
    }

    #[test]
    fn linger_hit_test_covers_only_the_margins() {
        assert!(!in_margin(0, 5), "not lingering: nothing transparent");
        let p = pack_linger(30, 48);
        assert!(in_margin(p, 0) && in_margin(p, 29));
        assert!(!in_margin(p, 30) && !in_margin(p, 77));
        assert!(in_margin(p, 78));
    }

    #[test]
    fn client_to_dip_at_125_and_150_percent() {
        // 125 %: a bar at DIP (100, 800) is px (125, 1000); client (10, 5) -> DIP (108, 804).
        assert_eq!(
            client_to_dip((125, 1000, 1125, 1060), (0, 0), 10, 5, 1.25),
            (108.0, 804.0)
        );
        // 150 % on a monitor right of a 1920 px one, with a 30 px Magnify margin above:
        // client y 30 is the bar's top edge.
        let (x, y) = client_to_dip((2880, 1530, 3600, 1602), (30, 0), 15, 30, 1.5);
        assert_eq!((x, y), (1930.0, 1020.0));
        // The far corner of the bar maps to its DIP right / bottom edge.
        let (x, y) = client_to_dip((2880, 1530, 3600, 1602), (30, 0), 720, 30 + 72, 1.5);
        assert_eq!((x, y), (2400.0, 1068.0));
        // Monitors left of / above the primary have negative coordinates.
        assert_eq!(
            client_to_dip((-375, -72, 2825, 0), (0, 0), 0, 0, 1.25),
            (-300.0, -57.6)
        );
    }

    #[test]
    fn dpi_change_is_recorded_and_the_suggested_rect_ignored() {
        use windows::Win32::UI::WindowsAndMessaging::SendMessageW;
        let comp = vitro_render::compositor::init_compositor().unwrap();
        let rect = (100, 700, 600, 760);
        let mut w = create_bar_window(&comp, rect, (0, 0)).unwrap();
        let _ = crate::notify::take_inbox();
        let suggested = RECT {
            left: 0,
            top: 0,
            right: 50,
            bottom: 50,
        };
        // SAFETY: in-process send to our own window; the RECT outlives the call.
        unsafe {
            SendMessageW(
                w.hwnd,
                WM_DPICHANGED,
                Some(WPARAM(144 | (144 << 16))),
                Some(LPARAM(&suggested as *const RECT as isize)),
            );
        }
        assert!(crate::notify::take_inbox().dpi);
        // Not moved to the suggested rectangle: the layout places the window.
        let mut r = RECT::default();
        // SAFETY: plain query on our own window.
        unsafe { GetWindowRect(w.hwnd, &mut r).unwrap() };
        assert_eq!((r.left, r.top, r.right, r.bottom), rect);
        assert!(!w.resync(), "already where it belongs");
        // SAFETY: moving our own test window away, as the system does on a display change.
        unsafe { SetWindowPos(w.hwnd, None, 5, 5, 10, 10, SWP_NOACTIVATE).unwrap() };
        assert!(w.resync());
        // SAFETY: plain query on our own window.
        unsafe { GetWindowRect(w.hwnd, &mut r).unwrap() };
        assert_eq!((r.left, r.top, r.right, r.bottom), rect);
        drop(w);
        // Dropping a bar window does not end the app (carry 1): no WM_QUIT queued.
        let mut msg = windows::Win32::UI::WindowsAndMessaging::MSG::default();
        // SAFETY: draining this test thread's own queue.
        let mut quit = false;
        unsafe {
            while windows::Win32::UI::WindowsAndMessaging::PeekMessageW(
                &mut msg,
                None,
                0,
                0,
                windows::Win32::UI::WindowsAndMessaging::PM_REMOVE,
            )
            .as_bool()
            {
                quit |= msg.message == windows::Win32::UI::WindowsAndMessaging::WM_QUIT;
            }
        }
        assert!(!quit);
    }

    #[test]
    fn mouse_pos_is_signed() {
        let lp = LPARAM(((0xFFFEu32 << 16) | 0x0005) as isize);
        assert_eq!(mouse_pos(lp), (5, -2));
    }
}
