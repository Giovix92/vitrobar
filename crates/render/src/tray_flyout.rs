//! The tray overflow (a grid of icons, four per row, in a glass [`Flyout`] above the
//! chevron) and the tray tooltip (a small glass flyout holding the icon's tip text).

use std::sync::Arc;

use vitro_core::image::BgraImage;
use vitro_core::layout::Rect;
use vitro_core::settings::Position;
use vitro_core::style::{MaterialStyle, Rgba};
use windows::core::Result;
use windows::UI::Composition::{
    CompositionColorBrush, CompositionRoundedRectangleGeometry, Compositor, ContainerVisual,
    ShapeVisual, SpriteVisual,
};
use windows_numerics::{Vector2, Vector3};

use crate::compositor::Comp;
use crate::flyout::{Flyout, PxRect};
use crate::material::color;
use crate::surfaces::{FontKind, Painter};

/// Icons per row in the overflow.
pub const TRAY_COLS: usize = 4;
/// Around the grid.
pub const GRID_PAD_DIP: f32 = 8.0;
/// Tray icons are drawn at this size (the stock taskbar's small icon size).
pub const TRAY_ICON_DIP: f32 = 16.0;
const PLATE_INSET_DIP: f32 = 2.0;
const PLATE_RADIUS_DIP: f32 = 6.0;
const TIP_FONT_DIP: f32 = 12.0;
const TIP_PAD_X_DIP: f32 = 10.0;
const TIP_PAD_Y_DIP: f32 = 6.0;
/// Longest tooltip line before it is cut (the text itself is at most 127 characters).
const TIP_MAX_W_DIP: f32 = 360.0;

/// Size of the overflow and one slot per icon, in DIPs relative to its top-left corner.
pub fn grid_layout(n: usize, slot: f32) -> ((f32, f32), Vec<Rect>) {
    let cols = n.clamp(1, TRAY_COLS);
    let rows = n.div_ceil(TRAY_COLS).max(1);
    let slots = (0..n)
        .map(|k| Rect {
            x: GRID_PAD_DIP + (k % TRAY_COLS) as f32 * slot,
            y: GRID_PAD_DIP + (k / TRAY_COLS) as f32 * slot,
            w: slot,
            h: slot,
        })
        .collect();
    let size = (
        2.0 * GRID_PAD_DIP + cols as f32 * slot,
        2.0 * GRID_PAD_DIP + rows as f32 * slot,
    );
    (size, slots)
}

/// A rounded rectangle shape (hover plate, placeholder).
struct RoundRect {
    visual: ShapeVisual,
    geom: CompositionRoundedRectangleGeometry,
    brush: CompositionColorBrush,
}

impl RoundRect {
    fn new(comp: &Compositor) -> Result<Self> {
        let geom = comp.CreateRoundedRectangleGeometry()?;
        let brush = comp.CreateColorBrush()?;
        let shape = comp.CreateSpriteShapeWithGeometry(&geom)?;
        shape.SetFillBrush(&brush)?;
        let visual = comp.CreateShapeVisual()?;
        visual.Shapes()?.Append(&shape)?;
        Ok(Self {
            visual,
            geom,
            brush,
        })
    }

    fn set(&self, r: PxRect, radius: f32, c: Rgba) -> Result<()> {
        let (w, h) = ((r.2 - r.0).max(0) as f32, (r.3 - r.1).max(0) as f32);
        self.visual
            .SetOffset(Vector3::new(r.0 as f32, r.1 as f32, 0.0))?;
        self.visual.SetSize(Vector2::new(w, h))?;
        self.geom.SetSize(Vector2::new(w, h))?;
        self.geom.SetCornerRadius(Vector2::new(radius, radius))?;
        self.brush.SetColor(color(c))
    }
}

fn px(r: &Rect, s: f32) -> PxRect {
    let p = |v: f32| (v * s).round() as i32;
    (p(r.x), p(r.y), p(r.right()), p(r.bottom()))
}

/// The overflow's icons inside a [`Flyout`] (whose size comes from [`grid_layout`]).
pub struct TrayGrid {
    comp: Compositor,
    container: ContainerVisual,
    plate: RoundRect,
    cells: Vec<ContainerVisual>,
    /// What each cell shows (`None` = placeholder), compared by identity.
    shown: Vec<Option<Arc<BgraImage>>>,
    /// Slots in flyout client px.
    slots: Vec<PxRect>,
    scale: f32,
    hover: Option<usize>,
    pressed: bool,
}

impl TrayGrid {
    pub fn new(flyout: &Flyout) -> Result<Self> {
        let comp = flyout.root().Compositor()?;
        let container = comp.CreateContainerVisual()?;
        let plate = RoundRect::new(&comp)?;
        plate.visual.SetIsVisible(false)?;
        container.Children()?.InsertAtTop(&plate.visual)?;
        flyout.root().Children()?.InsertAtTop(&container)?;
        Ok(Self {
            comp,
            container,
            plate,
            cells: Vec::new(),
            shown: Vec::new(),
            slots: Vec::new(),
            scale: flyout.scale(),
            hover: None,
            pressed: false,
        })
    }

    /// Shows `icons` (in slot order) in slots of `slot_dip`; only cells whose image changed
    /// are redrawn. `text` is the theme's text colour (placeholders and the hover plate).
    pub fn set(
        &mut self,
        painter: &Painter,
        slot_dip: f32,
        icons: &[Option<Arc<BgraImage>>],
        text: Rgba,
    ) -> Result<()> {
        let s = self.scale;
        let (_, slots) = grid_layout(icons.len(), slot_dip);
        let slots: Vec<PxRect> = slots.iter().map(|r| px(r, s)).collect();
        let relayout = slots != self.slots;
        let children = self.container.Children()?;
        while self.cells.len() > icons.len() {
            if let Some(c) = self.cells.pop() {
                children.Remove(&c)?;
            }
            self.shown.pop();
        }
        let icon_px = (TRAY_ICON_DIP * s).round() as i32;
        for (k, img) in icons.iter().enumerate() {
            let fresh = k >= self.cells.len();
            if fresh {
                let c = self.comp.CreateContainerVisual()?;
                children.InsertAtTop(&c)?;
                self.cells.push(c);
                self.shown.push(None);
            }
            let same = !fresh
                && match (&self.shown[k], img) {
                    (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                    (None, None) => true,
                    _ => false,
                };
            let cell = &self.cells[k];
            let slot = slots[k];
            if fresh || !same {
                cell.Children()?.RemoveAll()?;
                match img {
                    Some(img) if img.is_valid() && img.width > 0 => {
                        let surface = painter.image(img)?;
                        let sprite = self.comp.CreateSpriteVisual()?;
                        sprite.SetBrush(&self.comp.CreateSurfaceBrushWithSurface(&surface)?)?;
                        sprite.SetSize(Vector2::new(img.width as f32, img.height as f32))?;
                        cell.Children()?.InsertAtTop(&sprite)?;
                    }
                    _ => {
                        let ph = RoundRect::new(&self.comp)?;
                        ph.set(
                            (0, 0, icon_px, icon_px),
                            icon_px as f32 / 4.0,
                            Rgba(text.0, text.1, text.2, 51),
                        )?;
                        cell.Children()?.InsertAtTop(&ph.visual)?;
                    }
                }
                self.shown[k] = img.clone();
            }
            if fresh || !same || relayout {
                let (w, h) = match img {
                    Some(i) if i.is_valid() && i.width > 0 => (i.width as i32, i.height as i32),
                    _ => (icon_px, icon_px),
                };
                let x = slot.0 + (slot.2 - slot.0 - w) / 2;
                let y = slot.1 + (slot.3 - slot.1 - h) / 2;
                cell.SetOffset(Vector3::new(x as f32, y as f32, 0.0))?;
            }
        }
        self.slots = slots;
        if self.hover.is_some_and(|h| h >= self.slots.len()) {
            self.hover = None;
        }
        self.apply_hover(text)
    }

    /// Hover (and press) highlight on slot `i`.
    pub fn set_hover(&mut self, i: Option<usize>, pressed: bool, text: Rgba) -> Result<()> {
        if (i, pressed) == (self.hover, self.pressed) {
            return Ok(());
        }
        self.hover = i;
        self.pressed = pressed;
        self.apply_hover(text)
    }

    fn apply_hover(&self, text: Rgba) -> Result<()> {
        match self.hover.and_then(|i| self.slots.get(i)) {
            Some(&r) => {
                let inset = (PLATE_INSET_DIP * self.scale).round() as i32;
                let alpha = if self.pressed { 42 } else { 30 };
                self.plate.set(
                    (r.0 + inset, r.1 + inset, r.2 - inset, r.3 - inset),
                    PLATE_RADIUS_DIP * self.scale,
                    Rgba(text.0, text.1, text.2, alpha),
                )?;
                self.plate.visual.SetIsVisible(true)
            }
            None => self.plate.visual.SetIsVisible(false),
        }
    }

    /// The slot under `pt_px` (flyout client px).
    pub fn hit(&self, pt_px: (i32, i32)) -> Option<usize> {
        let (x, y) = pt_px;
        self.slots
            .iter()
            .position(|r| x >= r.0 && x < r.2 && y >= r.1 && y < r.3)
    }

    /// Slot `i` in flyout client px.
    pub fn slot_px(&self, i: usize) -> Option<PxRect> {
        self.slots.get(i).copied()
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
}

/// A tray icon's tip text in a small glass flyout next to the icon; hidden when dropped.
pub struct Tooltip {
    // Field order = drop order: the text sprite's tree goes with the window.
    _text: SpriteVisual,
    flyout: Flyout,
    tip: String,
}

impl Tooltip {
    pub fn show(
        comp: &Comp,
        painter: &Painter,
        anchor_px: PxRect,
        position: Position,
        dpi: u32,
        style: &MaterialStyle,
        tip: &str,
    ) -> Result<Self> {
        let s = dpi as f32 / 96.0;
        let (tw, th) = painter.measure(tip, FontKind::Text, TIP_FONT_DIP)?;
        let size_dip = (
            tw.ceil().min(TIP_MAX_W_DIP) + 2.0 * TIP_PAD_X_DIP,
            th.ceil() + 2.0 * TIP_PAD_Y_DIP,
        );
        let flyout = Flyout::show(comp, anchor_px, position, size_dip, dpi, style)?;
        let (l, t, r, b) = flyout.rect_px();
        let (w, h) = ((r - l).max(1) as u32, (b - t).max(1) as u32);
        let surface = painter.text(tip, FontKind::Text, TIP_FONT_DIP * s, style.text, w, h)?;
        let sprite = comp.CreateSpriteVisual()?;
        sprite.SetBrush(&comp.CreateSurfaceBrushWithSurface(&surface)?)?;
        sprite.SetSize(Vector2::new(w as f32, h as f32))?;
        flyout.root().Children()?.InsertAtTop(&sprite)?;
        Ok(Self {
            _text: sprite,
            flyout,
            tip: tip.to_string(),
        })
    }

    pub fn tip(&self) -> &str {
        &self.tip
    }

    pub fn hwnd(&self) -> windows::Win32::Foundation::HWND {
        self.flyout.hwnd()
    }

    pub fn rect_px(&self) -> PxRect {
        self.flyout.rect_px()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_is_four_wide() {
        let (size, slots) = grid_layout(6, 40.0);
        assert_eq!(size, (8.0 * 2.0 + 160.0, 8.0 * 2.0 + 80.0));
        assert_eq!((slots[3].x, slots[3].y), (8.0 + 120.0, 8.0));
        assert_eq!((slots[4].x, slots[4].y), (8.0, 48.0));
        let (size, _) = grid_layout(2, 40.0);
        assert_eq!(size, (96.0, 56.0));
        let (size, slots) = grid_layout(0, 40.0);
        assert!(slots.is_empty());
        assert_eq!(size, (56.0, 56.0));
    }
}
