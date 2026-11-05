//! The glass right-click menu: a [`Flyout`] (same glass and radius as the window preview and
//! the tray overflow) holding the rows of a [`vitro_core::menu_model`] menu.
//!
//! All row texts and check marks are drawn once into one surface; hovering only moves a
//! rounded plate underneath, so it costs no text redraw. The app's modal loop in
//! `menus.rs` feeds it the mouse and keys.

use std::cell::RefCell;

use vitro_core::layout::Rect;
use vitro_core::menu_model::{
    hit, layout, place, Entry, MenuLayout, PAD_DIP, TEXT_LEFT_DIP, TEXT_RIGHT_DIP,
};
use vitro_core::style::{MaterialStyle, Rgba};
use windows::core::Result;
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::UI::Composition::ContainerVisual;
use windows_numerics::Vector2;

use crate::compositor::Comp;
use crate::flyout::{Flyout, PxRect};
use crate::surfaces::{FontKind, Painter, TextBox};
use crate::tray_flyout::RoundRect;

const FONT_DIP: f32 = 14.0;
const PLATE_RADIUS_DIP: f32 = 6.0;
/// Alpha of the hover plate and of the separator line (the text colour at this strength).
const PLATE_ALPHA: u8 = 30;
const SEP_ALPHA: u8 = 51;
/// Segoe Fluent Icons "CheckMark".
const CHECK: &str = "\u{E73E}";

thread_local! {
    /// Menus on screen (this thread's), for [`is_menu_window`].
    static MENUS: RefCell<Vec<HWND>> = const { RefCell::new(Vec::new()) };
}

/// True for the window of a glass menu that is open now. A panel that loses activation to
/// one of these has not been left for "somewhere else".
pub fn is_menu_window(h: HWND) -> bool {
    MENUS.with(|m| m.borrow().contains(&h))
}

/// An open menu; the window goes when it is dropped.
pub struct MenuView {
    // Field order = drop order: the visuals before the flyout (window) they live in.
    plate: RoundRect,
    _seps: Vec<RoundRect>,
    _text: ContainerVisual,
    layout: MenuLayout,
    entries: Vec<Entry>,
    /// Row rectangles in menu client px.
    rows_px: Vec<PxRect>,
    scale: f32,
    text: Rgba,
    hot: Option<usize>,
    flyout: Flyout,
}

fn px(r: &Rect, s: f32) -> PxRect {
    let p = |v: f32| (v * s).round() as i32;
    (p(r.x), p(r.y), p(r.right()), p(r.bottom()))
}

/// Work area of the monitor nearest to `pt`.
fn work_at(pt: (i32, i32)) -> PxRect {
    let mut mi = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: pointer to a local for the duration of the calls.
    let ok = unsafe {
        let hmon = MonitorFromPoint(POINT { x: pt.0, y: pt.1 }, MONITOR_DEFAULTTONEAREST);
        GetMonitorInfoW(hmon, &mut mi).as_bool()
    };
    if ok {
        let w = mi.rcWork;
        (w.left, w.top, w.right, w.bottom)
    } else {
        (i32::MIN / 2, i32::MIN / 2, i32::MAX / 2, i32::MAX / 2)
    }
}

impl MenuView {
    /// Opens `entries` with the pointer at `at_px` (screen px) on a monitor of `dpi`: the
    /// menu opens away from the screen edge it is nearest and stays inside the work area.
    pub fn show(
        comp: &Comp,
        painter: &Painter,
        style: &MaterialStyle,
        dpi: u32,
        at_px: (i32, i32),
        entries: &[Entry],
    ) -> Result<Self> {
        let s = dpi as f32 / 96.0;
        let text_w = |t: &str| {
            painter
                .measure(t, FontKind::Text, FONT_DIP)
                .map(|(w, _)| w.ceil())
                .unwrap_or_else(|e| {
                    eprintln!("vitrobar: could not measure a menu text: {e}");
                    t.chars().count() as f32 * FONT_DIP * 0.6
                })
        };
        let lay = layout(entries, &text_w);
        let size_px = (
            (lay.size.0 * s).round() as i32,
            (lay.size.1 * s).round() as i32,
        );
        let origin = place(at_px, size_px, work_at(at_px));
        let flyout = Flyout::show_at(comp, origin, lay.size, dpi, style)?;
        let rows_px: Vec<PxRect> = lay.rows.iter().map(|r| px(r, s)).collect();

        let children = flyout.root().Children()?;
        let text = style.text;
        let plate = RoundRect::new(comp)?;
        plate.visual.SetIsVisible(false)?;
        children.InsertAtTop(&plate.visual)?;
        let mut seps = Vec::new();
        for (e, r) in entries.iter().zip(&rows_px) {
            if matches!(e, Entry::Separator) {
                let line = RoundRect::new(comp)?;
                let y = (r.1 + r.3) / 2;
                let inset = (PAD_DIP * s).round() as i32;
                line.set(
                    (r.0 + inset, y, r.2 - inset, y + (s.round() as i32).max(1)),
                    0.0,
                    Rgba(text.0, text.1, text.2, SEP_ALPHA),
                )?;
                children.InsertAtTop(&line.visual)?;
                seps.push(line);
            }
        }

        // Every row's text and check mark, in one surface.
        let (w, h) = (size_px.0.max(1) as u32, size_px.1.max(1) as u32);
        let text_x = TEXT_LEFT_DIP * s;
        let text_w_px = (lay.size.0 - TEXT_LEFT_DIP - TEXT_RIGHT_DIP) * s;
        let check_x = PAD_DIP * s;
        let check_w = (TEXT_LEFT_DIP - PAD_DIP) * s;
        let mut boxes: Vec<TextBox> = Vec::new();
        for (e, r) in entries.iter().zip(&rows_px) {
            if let Entry::Command { text, checked, .. } = e {
                let (y, rh) = (r.1 as f32, (r.3 - r.1) as f32);
                if *checked {
                    boxes.push((CHECK, FontKind::Glyph, (check_x, y, check_w, rh)));
                }
                boxes.push((text, FontKind::Text, (text_x, y, text_w_px, rh)));
            }
        }
        let surface = painter.text_boxes(&boxes, FONT_DIP * s, text, w, h)?;
        let sprite = comp.CreateSpriteVisual()?;
        sprite.SetBrush(&comp.CreateSurfaceBrushWithSurface(&surface)?)?;
        sprite.SetSize(Vector2::new(w as f32, h as f32))?;
        let holder = comp.CreateContainerVisual()?;
        holder.SetSize(Vector2::new(w as f32, h as f32))?;
        holder.Children()?.InsertAtTop(&sprite)?;
        children.InsertAtTop(&holder)?;

        MENUS.with(|m| m.borrow_mut().push(flyout.hwnd()));
        Ok(Self {
            plate,
            _seps: seps,
            _text: holder,
            layout: lay,
            entries: entries.to_vec(),
            rows_px,
            scale: s,
            text,
            hot: None,
            flyout,
        })
    }

    /// Fades the menu in while it grows from 98 % (a quick pop, as Windows' own menus).
    pub fn animate_open(&self, ms: u32) -> Result<()> {
        crate::panel::animate_open(
            self.flyout.root(),
            self.flyout.scale(),
            ms,
            crate::panel::Motion::Grow,
        )
    }

    pub fn hwnd(&self) -> HWND {
        self.flyout.hwnd()
    }

    /// Screen rectangle in px.
    pub fn rect_px(&self) -> PxRect {
        self.flyout.rect_px()
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Takes the foreground (the menu then hears keys; losing it dismisses the menu).
    pub fn activate(&self) -> bool {
        self.flyout.activate()
    }

    pub fn hot(&self) -> Option<usize> {
        self.hot
    }

    /// Highlights command row `i` (none clears it); only the plate moves.
    pub fn set_hot(&mut self, i: Option<usize>) -> Result<()> {
        if i == self.hot {
            return Ok(());
        }
        self.hot = i;
        match i.and_then(|i| self.rows_px.get(i)) {
            Some(&r) => {
                let t = self.text;
                self.plate.set(
                    r,
                    PLATE_RADIUS_DIP * self.scale,
                    Rgba(t.0, t.1, t.2, PLATE_ALPHA),
                )?;
                self.plate.visual.SetIsVisible(true)
            }
            None => self.plate.visual.SetIsVisible(false),
        }
    }

    /// The command row under `client_px` (menu client px).
    pub fn hit_px(&self, client_px: (i32, i32)) -> Option<usize> {
        let s = self.scale;
        hit(
            &self.layout,
            &self.entries,
            (client_px.0 as f32 / s, client_px.1 as f32 / s),
        )
    }

    /// Whether `client_px` (menu client px) lies inside the menu. Only matters while the mouse
    /// is captured (the menu then also gets clicks that land outside it).
    pub fn contains_px(&self, client_px: (i32, i32)) -> bool {
        let (l, t, r, b) = self.flyout.rect_px();
        client_px.0 >= 0 && client_px.1 >= 0 && client_px.0 < r - l && client_px.1 < b - t
    }
}

impl Drop for MenuView {
    fn drop(&mut self) {
        let h = self.flyout.hwnd();
        MENUS.with(|m| m.borrow_mut().retain(|x| *x != h));
    }
}
