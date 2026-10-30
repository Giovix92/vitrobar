//! The thumbnail flyout: a glass top-level window next to a bar item, holding live DWM
//! thumbnails of the item's windows with their titles underneath.
//!
//! The thumbnails are composited by DWM straight into the flyout window: they stay live
//! (a window that repaints updates its thumbnail) and cost Vitrobar no redraws. The glass
//! and the titles are drawn once when the strip opens.

use std::cell::RefCell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

use vitro_core::layout::Rect;
use vitro_core::settings::Position;
use vitro_core::style::{MaterialStyle, Rgba};
use windows::core::{w, Error, Interface, Result, BOOL, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DwmQueryThumbnailSourceSize, DwmRegisterThumbnail,
    DwmSetWindowAttribute, DwmUnregisterThumbnail, DwmUpdateThumbnailProperties,
    DWMWA_EXTENDED_FRAME_BOUNDS, DWMWA_USE_HOSTBACKDROPBRUSH, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_DONOTROUND, DWM_THUMBNAIL_PROPERTIES, DWM_TNP_OPACITY, DWM_TNP_RECTDESTINATION,
    DWM_TNP_RECTSOURCE, DWM_TNP_SOURCECLIENTAREAONLY, DWM_TNP_VISIBLE,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromRect, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetWindowPlacement, GetWindowRect, IsIconic,
    IsWindow, LoadCursorW, RegisterClassExW, SetForegroundWindow, SetWindowPos, ShowWindow,
    HWND_TOPMOST, IDC_ARROW, MA_NOACTIVATE, SWP_NOACTIVATE, SW_SHOWNOACTIVATE, WA_INACTIVE,
    WINDOWPLACEMENT, WM_ACTIVATE, WM_MOUSEACTIVATE, WNDCLASSEXW, WS_EX_NOACTIVATE,
    WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{ContainerVisual, SpriteVisual, Visual};
use windows_numerics::{Vector2, Vector3};

use crate::compositor::{window_target, Comp};
use crate::material::backdrop_visual;
use crate::surfaces::Painter;

/// Window class of the flyout (found by tests and tools with `FindWindow`).
pub const FLYOUT_CLASS: PCWSTR = w!("VitrobarFlyout");

/// Geometry in DIPs.
pub const RADIUS_DIP: f32 = 12.0;
/// Between the bar and the flyout.
pub const ANCHOR_GAP_DIP: f32 = 8.0;
pub const THUMB_W_DIP: f32 = 200.0;
pub const THUMB_MAX_H_DIP: f32 = 120.0;
/// Smallest thumbnail row (very wide windows).
const THUMB_MIN_H_DIP: f32 = 40.0;
/// Between thumbnails.
pub const STRIP_GAP_DIP: f32 = 8.0;
pub const STRIP_PAD_DIP: f32 = 10.0;
const TITLE_GAP_DIP: f32 = 6.0;
const TITLE_H_DIP: f32 = 16.0;
const TITLE_FONT_DIP: f32 = 12.0;
/// When a window's size cannot be read.
const DEFAULT_ASPECT: f32 = 16.0 / 10.0;

/// `(left, top, right, bottom)` in physical pixels.
pub type PxRect = (i32, i32, i32, i32);

/// DWM thumbnails registered by this process and not yet unregistered (diagnostics).
static LIVE_THUMBNAILS: AtomicUsize = AtomicUsize::new(0);

pub fn live_thumbnails() -> usize {
    LIVE_THUMBNAILS.load(Ordering::SeqCst)
}

/// Where the strip's parts go, in DIPs relative to the flyout's top-left corner.
#[derive(Clone, Debug, PartialEq)]
pub struct StripLayout {
    /// Size of the whole flyout.
    pub size: (f32, f32),
    /// One box per window: the thumbnail is fitted inside, aspect kept.
    pub boxes: Vec<Rect>,
    /// The title line under each box.
    pub titles: Vec<Rect>,
}

/// Lays out one thumbnail per window, `aspects` being width / height of each window.
pub fn strip_layout(aspects: &[f32]) -> StripLayout {
    let n = aspects.len();
    let row_h = aspects
        .iter()
        .map(|&a| {
            let a = if a.is_finite() && a > 0.0 {
                a
            } else {
                DEFAULT_ASPECT
            };
            (THUMB_W_DIP / a).min(THUMB_MAX_H_DIP)
        })
        .fold(THUMB_MIN_H_DIP, f32::max);
    let mut boxes = Vec::with_capacity(n);
    let mut titles = Vec::with_capacity(n);
    for k in 0..n {
        let x = STRIP_PAD_DIP + k as f32 * (THUMB_W_DIP + STRIP_GAP_DIP);
        boxes.push(Rect {
            x,
            y: STRIP_PAD_DIP,
            w: THUMB_W_DIP,
            h: row_h,
        });
        titles.push(Rect {
            x,
            y: STRIP_PAD_DIP + row_h + TITLE_GAP_DIP,
            w: THUMB_W_DIP,
            h: TITLE_H_DIP,
        });
    }
    let w =
        2.0 * STRIP_PAD_DIP + n as f32 * THUMB_W_DIP + n.saturating_sub(1) as f32 * STRIP_GAP_DIP;
    let h = 2.0 * STRIP_PAD_DIP + row_h + TITLE_GAP_DIP + TITLE_H_DIP;
    StripLayout {
        size: (w, h),
        boxes,
        titles,
    }
}

/// Top-left corner of a `size` px flyout: centred on the anchor, `gap` px above it (below it
/// for a top bar), kept inside the work area horizontally.
pub fn place(
    anchor: PxRect,
    position: Position,
    size: (i32, i32),
    gap: i32,
    work: PxRect,
) -> (i32, i32) {
    let (l, t, r, b) = anchor;
    let x = (l + r) / 2 - size.0 / 2;
    let x = x.min(work.2 - size.0).max(work.0);
    let y = match position {
        Position::Bottom => t - gap - size.1,
        Position::Top => b + gap,
    };
    (x, y)
}

/// `dst` px box, `src` source size: the largest centred rectangle of `src`'s aspect inside
/// `dst`, never enlarged past 1:1.
pub fn fit(dst: PxRect, src: (i32, i32)) -> PxRect {
    let (bw, bh) = ((dst.2 - dst.0).max(0), (dst.3 - dst.1).max(0));
    if src.0 <= 0 || src.1 <= 0 {
        return dst;
    }
    let s = (bw as f32 / src.0 as f32)
        .min(bh as f32 / src.1 as f32)
        .min(1.0);
    let (w, h) = (
        ((src.0 as f32 * s).round() as i32).max(1),
        ((src.1 as f32 * s).round() as i32).max(1),
    );
    let x = dst.0 + (bw - w) / 2;
    let y = dst.1 + (bh - h) / 2;
    (x, y, x + w, y + h)
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
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                lpszClassName: FLYOUT_CLASS,
                ..Default::default()
            };
            if RegisterClassExW(&wc) == 0 {
                return Err(Error::from_thread());
            }
            Ok(())
        })
        .clone()
}

thread_local! {
    /// Flyouts that lost activation since the last [`take_deactivated`] (this thread's).
    static DEACTIVATED: RefCell<Vec<HWND>> = const { RefCell::new(Vec::new()) };
}

/// Flyouts (made active with [`Flyout::activate`]) that were deactivated since the last
/// call: the user clicked elsewhere or switched apps.
pub fn take_deactivated() -> Vec<HWND> {
    DEACTIVATED.with(|d| std::mem::take(&mut *d.borrow_mut()))
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // Mouse input is read by the app's message loop before dispatch.
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_ACTIVATE => {
            if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE {
                DEACTIVATED.with(|d| d.borrow_mut().push(hwnd));
            }
            // SAFETY: default handling with the arguments we were given.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        // SAFETY: default handling with the arguments we were given.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn work_area(anchor: PxRect) -> PxRect {
    let rc = RECT {
        left: anchor.0,
        top: anchor.1,
        right: anchor.2,
        bottom: anchor.3,
    };
    let mut mi = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: pointers to locals for the duration of the calls.
    let ok = unsafe {
        let hmon = MonitorFromRect(&rc, MONITOR_DEFAULTTONEAREST);
        GetMonitorInfoW(hmon, &mut mi).as_bool()
    };
    if ok {
        let w = mi.rcWork;
        (w.left, w.top, w.right, w.bottom)
    } else {
        (i32::MIN / 2, i32::MIN / 2, i32::MAX / 2, i32::MAX / 2)
    }
}

/// A glass top-level window next to a bar item: same window styles and material as the bar.
pub struct Flyout {
    // Field order = drop order: the target before the window it draws into.
    _target: DesktopWindowTarget,
    root: ContainerVisual,
    glass: Visual,
    hwnd: HWND,
    rect_px: PxRect,
    scale: f32,
}

impl Flyout {
    /// Opens a `size_dip` flyout next to `anchor_px` (screen px: the item's width and the bar's
    /// height) on a monitor of `dpi`, filled with the bar's `style`.
    pub fn show(
        comp: &Comp,
        anchor_px: PxRect,
        position: Position,
        size_dip: (f32, f32),
        dpi: u32,
        style: &MaterialStyle,
    ) -> Result<Self> {
        register_class()?;
        let scale = dpi as f32 / 96.0;
        let size = (
            (size_dip.0 * scale).round() as i32,
            (size_dip.1 * scale).round() as i32,
        );
        let gap = (ANCHOR_GAP_DIP * scale).round() as i32;
        let (x, y) = place(anchor_px, position, size, gap, work_area(anchor_px));
        // SAFETY: the class is registered; static strings.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOREDIRECTIONBITMAP | WS_EX_NOACTIVATE,
                FLYOUT_CLASS,
                w!("Vitrobar"),
                WS_POPUP,
                x,
                y,
                size.0,
                size.1,
                None,
                None,
                Some(GetModuleHandleW(None)?.into()),
                None,
            )?
        };
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
            let glass: Visual = backdrop_visual(comp, style, size_v, RADIUS_DIP * scale)?.cast()?;
            root.Children()?.InsertAtBottom(&glass)?;
            target.SetRoot(&root)?;
            // SAFETY: plain call on our own window.
            unsafe {
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            }
            Ok(Self {
                _target: target,
                root,
                glass,
                hwnd,
                rect_px: (x, y, x + size.0, y + size.1),
                scale,
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

    /// The flyout's visual tree: the glass is its bottom child; content goes on top.
    pub fn root(&self) -> &ContainerVisual {
        &self.root
    }

    /// Makes the flyout the foreground window (it then hears keys, and a click elsewhere
    /// deactivates it: see [`take_deactivated`]). Only allowed right after the user clicked
    /// one of our windows; false if Windows refused.
    pub fn activate(&self) -> bool {
        // SAFETY: plain call on our own window.
        unsafe { SetForegroundWindow(self.hwnd) }.as_bool()
    }

    /// Gives the flyout a new size (and place next to `anchor_px`), redrawing the glass.
    pub fn reshape(
        &mut self,
        comp: &Comp,
        anchor_px: PxRect,
        position: Position,
        size_dip: (f32, f32),
        style: &MaterialStyle,
    ) -> Result<()> {
        let s = self.scale;
        let size = (
            (size_dip.0 * s).round() as i32,
            (size_dip.1 * s).round() as i32,
        );
        let gap = (ANCHOR_GAP_DIP * s).round() as i32;
        let (x, y) = place(anchor_px, position, size, gap, work_area(anchor_px));
        let rect = (x, y, x + size.0, y + size.1);
        if rect == self.rect_px {
            return Ok(());
        }
        // SAFETY: plain call on our own window.
        unsafe {
            SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                x,
                y,
                size.0,
                size.1,
                SWP_NOACTIVATE,
            )?
        };
        let size_v = Vector2::new(size.0 as f32, size.1 as f32);
        self.root.SetSize(size_v)?;
        let glass: Visual = backdrop_visual(comp, style, size_v, RADIUS_DIP * s)?.cast()?;
        let children = self.root.Children()?;
        children.Remove(&self.glass)?;
        children.InsertAtBottom(&glass)?;
        self.glass = glass;
        self.rect_px = rect;
        Ok(())
    }
}

/// Dropping the flyout is hiding it: the window is destroyed. There is deliberately no
/// separate `hide`, which would leave the [`ThumbnailStrip`] (a separate value) with
/// thumbnails registered against a hidden window. Drop the strip first, then the flyout.
impl Drop for Flyout {
    fn drop(&mut self) {
        // SAFETY: destroying our own window if it still exists.
        unsafe {
            if IsWindow(Some(self.hwnd)).as_bool() {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }
}

/// Width / height of `h` as it would be thumbnailed (its restored size when minimized).
pub fn window_aspect(h: HWND) -> f32 {
    let (w, ht) = visible_bounds(h)
        .map(|(_, r)| (r.2 - r.0, r.3 - r.1))
        .unwrap_or((0, 0));
    if w > 0 && ht > 0 {
        w as f32 / ht as f32
    } else {
        DEFAULT_ASPECT
    }
}

/// (window rect, visible frame rect) in screen px; for a minimized window both are its
/// restored rectangle (relative positions only).
fn visible_bounds(h: HWND) -> Option<(PxRect, PxRect)> {
    // SAFETY: out-pointers to locals whose sizes are passed alongside; `h` may be any value
    // (the calls fail on a destroyed window).
    unsafe {
        if IsIconic(h).as_bool() {
            let mut wp = WINDOWPLACEMENT {
                length: size_of::<WINDOWPLACEMENT>() as u32,
                ..Default::default()
            };
            GetWindowPlacement(h, &mut wp).ok()?;
            let r = wp.rcNormalPosition;
            let r = (r.left, r.top, r.right, r.bottom);
            return Some((r, r));
        }
        let mut wr = RECT::default();
        GetWindowRect(h, &mut wr).ok()?;
        let mut fr = RECT::default();
        let frame = DwmGetWindowAttribute(
            h,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut fr as *mut RECT as *mut _,
            size_of::<RECT>() as u32,
        )
        .map(|()| fr)
        .unwrap_or(wr);
        Some((
            (wr.left, wr.top, wr.right, wr.bottom),
            (frame.left, frame.top, frame.right, frame.bottom),
        ))
    }
}

struct Thumb {
    hwnd: HWND,
    id: isize,
    /// Client px of the flyout.
    dest: PxRect,
    /// Box and title together: what a click selects.
    slot: PxRect,
}

/// Live thumbnails of `windows` inside a [`Flyout`]; unregistered when dropped.
pub struct ThumbnailStrip {
    thumbs: Vec<Thumb>,
    titles: Option<(ContainerVisual, SpriteVisual)>,
}

fn px_rect(r: &Rect, s: f32) -> PxRect {
    let p = |v: f32| (v * s).round() as i32;
    (p(r.x), p(r.y), p(r.right()), p(r.bottom()))
}

impl ThumbnailStrip {
    /// Size of the flyout for `windows` (pass it to [`Flyout::show`]).
    pub fn size_dip(windows: &[HWND]) -> (f32, f32) {
        let aspects: Vec<f32> = windows.iter().map(|&h| window_aspect(h)).collect();
        strip_layout(&aspects).size
    }

    /// Registers one DWM thumbnail per window and draws the titles (`text` colour) under
    /// them. A window that cannot be thumbnailed (already gone) keeps an empty box.
    pub fn show(
        owner: &Flyout,
        painter: &Painter,
        text: Rgba,
        windows: &[(HWND, String)],
    ) -> Result<Self> {
        let s = owner.scale;
        let aspects: Vec<f32> = windows.iter().map(|(h, _)| window_aspect(*h)).collect();
        let layout = strip_layout(&aspects);
        let mut strip = Self {
            thumbs: Vec::with_capacity(windows.len()),
            titles: None,
        };
        for (k, (h, _)) in windows.iter().enumerate() {
            let boxed = px_rect(&layout.boxes[k], s);
            let title = px_rect(&layout.titles[k], s);
            let slot = (boxed.0, boxed.1, boxed.2, title.3);
            // SAFETY: both windows are valid or the call fails; the id is unregistered
            // exactly once, in Drop.
            let id = match unsafe { DwmRegisterThumbnail(owner.hwnd, *h) } {
                Ok(id) => id,
                Err(_) => continue,
            };
            LIVE_THUMBNAILS.fetch_add(1, Ordering::SeqCst);
            strip.thumbs.push(Thumb {
                hwnd: *h,
                id,
                dest: boxed,
                slot,
            });
            let thumb = strip.thumbs.last_mut().expect("just pushed");
            // Crop the invisible resize borders: the source is the visible frame.
            let bounds = visible_bounds(*h);
            let source =
                bounds.map(|(wr, fr)| (fr.0 - wr.0, fr.1 - wr.1, fr.2 - wr.0, fr.3 - wr.1));
            // SAFETY: `id` is a live thumbnail of ours.
            let full = unsafe { DwmQueryThumbnailSourceSize(id) }
                .map(|sz| (sz.cx, sz.cy))
                .unwrap_or((0, 0));
            let src = match source {
                Some(r)
                    if r.0 >= 0
                        && r.1 >= 0
                        && r.2 > r.0
                        && r.3 > r.1
                        && r.2 <= full.0
                        && r.3 <= full.1 =>
                {
                    Some(r)
                }
                _ => None,
            };
            let src_size = src.map(|r| (r.2 - r.0, r.3 - r.1)).unwrap_or(full);
            thumb.dest = fit(boxed, src_size);
            let mut flags = DWM_TNP_RECTDESTINATION
                | DWM_TNP_VISIBLE
                | DWM_TNP_OPACITY
                | DWM_TNP_SOURCECLIENTAREAONLY;
            if src.is_some() {
                flags |= DWM_TNP_RECTSOURCE;
            }
            let rs = src.unwrap_or_default();
            let props = DWM_THUMBNAIL_PROPERTIES {
                dwFlags: flags,
                rcDestination: RECT {
                    left: thumb.dest.0,
                    top: thumb.dest.1,
                    right: thumb.dest.2,
                    bottom: thumb.dest.3,
                },
                rcSource: RECT {
                    left: rs.0,
                    top: rs.1,
                    right: rs.2,
                    bottom: rs.3,
                },
                opacity: 255,
                fVisible: BOOL(1),
                fSourceClientAreaOnly: BOOL(0),
            };
            // SAFETY: `props` is a local valid for the call; `id` is live.
            let _ = unsafe { DwmUpdateThumbnailProperties(id, &props) };
        }

        // All titles in one surface, drawn once.
        if let Some(first) = layout.titles.first() {
            let (w, h) = (
                (layout.size.0 * s).round() as u32,
                (first.h * s).round() as u32,
            );
            let boxes: Vec<(&str, f32, f32)> = windows
                .iter()
                .zip(&layout.titles)
                .map(|((_, t), r)| (t.as_str(), (r.x * s).round(), (r.w * s).round()))
                .collect();
            let surface = painter.text_row(&boxes, TITLE_FONT_DIP * s, text, w, h)?;
            let sprite = owner.root.Compositor()?.CreateSpriteVisual()?;
            sprite.SetBrush(
                &owner
                    .root
                    .Compositor()?
                    .CreateSurfaceBrushWithSurface(&surface)?,
            )?;
            sprite.SetSize(Vector2::new(w as f32, h as f32))?;
            sprite.SetOffset(Vector3::new(0.0, (first.y * s).round(), 0.0))?;
            owner.root.Children()?.InsertAtTop(&sprite)?;
            strip.titles = Some((owner.root.clone(), sprite));
        }
        Ok(strip)
    }

    /// The window whose thumbnail or title is at `pt_px` (flyout client px).
    pub fn hit(&self, pt_px: (i32, i32)) -> Option<HWND> {
        let (x, y) = pt_px;
        self.thumbs
            .iter()
            .find(|t| x >= t.slot.0 && x < t.slot.2 && y >= t.slot.1 && y < t.slot.3)
            .map(|t| t.hwnd)
    }

    /// `(window, thumbnail rectangle in flyout client px)` per registered thumbnail.
    pub fn thumbnails(&self) -> Vec<(HWND, PxRect)> {
        self.thumbs.iter().map(|t| (t.hwnd, t.dest)).collect()
    }
}

impl Drop for ThumbnailStrip {
    fn drop(&mut self) {
        for t in self.thumbs.drain(..) {
            // SAFETY: each id was registered by us and is unregistered exactly once.
            let _ = unsafe { DwmUnregisterThumbnail(t.id) };
            LIVE_THUMBNAILS.fetch_sub(1, Ordering::SeqCst);
        }
        if let Some((root, sprite)) = self.titles.take() {
            if let Ok(children) = root.Children() {
                let _ = children.Remove(&sprite);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_sizes_follow_the_brief() {
        // 16:10 windows: 200 x 125 would be too tall, so the row is capped at 120.
        let l = strip_layout(&[1.6, 1.6]);
        assert_eq!(l.boxes.len(), 2);
        assert_eq!(l.boxes[0].w, 200.0);
        assert_eq!(l.boxes[0].h, 120.0);
        assert_eq!(l.boxes[0].x, 10.0);
        assert_eq!(l.boxes[1].x, 10.0 + 200.0 + 8.0);
        assert_eq!(l.size.0, 10.0 + 200.0 + 8.0 + 200.0 + 10.0);
        assert_eq!(l.size.1, 10.0 + 120.0 + 6.0 + 16.0 + 10.0);
        // A wide window gives a lower row; the row follows the tallest thumbnail.
        let wide = strip_layout(&[4.0]);
        assert_eq!(wide.boxes[0].h, 50.0);
        let mixed = strip_layout(&[4.0, 2.0]);
        assert_eq!(mixed.boxes[0].h, 100.0);
        assert_eq!(mixed.titles[1].y, 10.0 + 100.0 + 6.0);
        // Nonsense aspects fall back instead of producing NaN.
        let odd = strip_layout(&[0.0, f32::NAN]);
        assert!(odd.size.1.is_finite());
    }

    #[test]
    fn placement_sits_above_or_below_and_stays_on_screen() {
        let work = (0, 0, 1920, 1040);
        let anchor = (900, 1000, 950, 1060);
        assert_eq!(
            place(anchor, Position::Bottom, (400, 200), 10, work),
            (925 - 200, 1000 - 10 - 200)
        );
        assert_eq!(
            place(anchor, Position::Top, (400, 200), 10, work),
            (725, 1070)
        );
        // Clamped at both edges.
        assert_eq!(
            place((0, 1000, 40, 1060), Position::Bottom, (400, 200), 10, work).0,
            0
        );
        assert_eq!(
            place(
                (1900, 1000, 1920, 1060),
                Position::Bottom,
                (400, 200),
                10,
                work
            )
            .0,
            1520
        );
    }

    #[test]
    fn fit_keeps_the_aspect_and_never_enlarges() {
        let b = (10, 10, 210, 130); // 200 x 120
        assert_eq!(fit(b, (1600, 1000)), (14, 10, 206, 130));
        let r = fit(b, (500, 1000));
        assert_eq!((r.2 - r.0, r.3 - r.1), (60, 120));
        assert_eq!(fit(b, (100, 50)), (60, 45, 160, 95));
        assert_eq!(fit(b, (0, 0)), b);
    }
}
