//! The glass panel view: a [`Flyout`] (same glass as the window preview and the menus)
//! drawing the rows of a [`vitro_core::panel_kit`] panel: tiles, sliders, list rows,
//! headers, buttons and holes the owner draws into.
//!
//! Every row is its own little visual tree (plates, text sprites). [`PanelView::update`]
//! keeps the trees of rows whose content did not change and only moves them, so a volume
//! key held down redraws one slider and nothing else. Hover, press and keyboard focus are
//! two overlay shapes that move; they never redraw text. Content taller than the window
//! scrolls inside a clipped viewport.

use vitro_core::layout::Rect;
use vitro_core::panel_kit::{self, layout, PanelLayout, Part, Row, PAD_DIP, THUMB_DIP, WIDTH_DIP};
use vitro_core::settings::Position;
use vitro_core::style::{MaterialStyle, Rgba};
use windows::core::Result;
use windows::Win32::Foundation::HWND;
use windows::UI::Composition::{
    CompositionColorBrush, CompositionRoundedRectangleGeometry, CompositionSpriteShape, Compositor,
    ContainerVisual, ShapeVisual,
};
use windows_numerics::{Vector2, Vector3};

use crate::compositor::Comp;
use crate::flyout::{Flyout, PxRect};
use crate::material::color;
use crate::surfaces::{FontKind, Painter};
use crate::tray_flyout::RoundRect;

/// Accent of the "on" tiles, slider fills and the focus ring until the owner knows better.
pub const ACCENT: Rgba = Rgba(0x00, 0x78, 0xD4, 255);
const WHITE: Rgba = Rgba(255, 255, 255, 255);

const TILE_RADIUS_DIP: f32 = 8.0;
const PLATE_RADIUS_DIP: f32 = 6.0;
const TRACK_H_DIP: f32 = 4.0;
const RING_DIP: f32 = 2.0;
const FONT_DIP: f32 = 14.0;
const SMALL_FONT_DIP: f32 = 12.0;
const HEADER_FONT_DIP: f32 = 16.0;
const GLYPH_DIP: f32 = 18.0;
/// Alpha of the hover and press plates, of an "off" tile and of separators and tracks.
const HOT_ALPHA: u8 = 30;
const PRESSED_ALPHA: u8 = 60;
const TILE_ALPHA: u8 = 22;
const BUTTON_ALPHA: u8 = 26;
const TRACK_ALPHA: u8 = 70;
const SEP_ALPHA: u8 = 51;
const DIM: f32 = 0.7;
/// Segoe Fluent Icons "ChevronRight" and "CheckMark".
const CHEVRON: &str = "\u{E76C}";
const CHECK: &str = "\u{E73E}";
const BUSY: &str = "\u{2026}";

fn alpha(c: Rgba, a: u8) -> Rgba {
    Rgba(c.0, c.1, c.2, a)
}

fn dim(c: Rgba) -> Rgba {
    alpha(c, (c.3 as f32 * DIM).round() as u8)
}

fn round(v: f32) -> i32 {
    v.round() as i32
}

/// What drawing a row needs.
struct Ctx<'a> {
    comp: &'a Compositor,
    painter: &'a Painter,
    s: f32,
    text: Rgba,
    accent: Rgba,
}

/// `(x, y, w, h)` in px.
type BoxF = (f32, f32, f32, f32);

/// Texts of one row, collected by (size, colour) so each group is one surface.
#[derive(Default)]
struct Texts(Vec<TextGroup>);

struct TextGroup {
    size_dip: f32,
    color: Rgba,
    boxes: Vec<(String, FontKind, BoxF)>,
}

impl Texts {
    fn add(&mut self, size_dip: f32, color: Rgba, text: &str, kind: FontKind, rect: BoxF) {
        let i = match self
            .0
            .iter()
            .position(|g| g.size_dip == size_dip && g.color == color)
        {
            Some(i) => i,
            None => {
                self.0.push(TextGroup {
                    size_dip,
                    color,
                    boxes: Vec::new(),
                });
                self.0.len() - 1
            }
        };
        self.0[i].boxes.push((text.to_string(), kind, rect));
    }

    /// One sprite per group over the whole `w` x `h` px row.
    fn flush(&self, ctx: &Ctx, into: &ContainerVisual, w: u32, h: u32) -> Result<()> {
        for g in &self.0 {
            let boxes: Vec<_> = g
                .boxes
                .iter()
                .map(|(t, k, r)| (t.as_str(), *k, *r))
                .collect();
            let surface = ctx
                .painter
                .text_boxes(&boxes, g.size_dip * ctx.s, g.color, w, h)?;
            let sprite = ctx.comp.CreateSpriteVisual()?;
            sprite.SetBrush(&ctx.comp.CreateSurfaceBrushWithSurface(&surface)?)?;
            sprite.SetSize(Vector2::new(w as f32, h as f32))?;
            into.Children()?.InsertAtTop(&sprite)?;
        }
        Ok(())
    }
}

/// A filled rounded rectangle added to `into`.
fn plate(ctx: &Ctx, into: &ContainerVisual, r: PxRect, radius_px: f32, c: Rgba) -> Result<()> {
    let rr = RoundRect::new(ctx.comp)?;
    rr.set(r, radius_px, c)?;
    into.Children()?.InsertAtTop(&rr.visual)
}

/// `r` (content DIPs) in px relative to a row starting at `band_y`.
fn local(r: &Rect, band_y: f32, s: f32) -> PxRect {
    (
        round(r.x * s),
        round((r.y - band_y) * s),
        round(r.right() * s),
        round((r.bottom() - band_y) * s),
    )
}

fn boxf(r: PxRect) -> BoxF {
    (
        r.0 as f32,
        r.1 as f32,
        (r.2 - r.0) as f32,
        (r.3 - r.1) as f32,
    )
}

fn part_rect(l: &PanelLayout, p: Part) -> Option<Rect> {
    l.parts.iter().find(|(q, _)| *q == p).map(|(_, r)| *r)
}

/// One row's visuals.
struct RowVis {
    row: Row,
    y: f32,
    container: ContainerVisual,
    /// A Custom row's slot (the same visual for as long as the row keeps its id and height).
    slot: Option<(u32, ContainerVisual)>,
}

fn build_row(ctx: &Ctx, l: &PanelLayout, index: usize, row: &Row) -> Result<RowVis> {
    let s = ctx.s;
    let band = l.rows[index];
    let (w, h) = (
        round(WIDTH_DIP * s).max(1) as u32,
        round(band.h * s).max(1) as u32,
    );
    let container = ctx.comp.CreateContainerVisual()?;
    container.SetSize(Vector2::new(w as f32, h as f32))?;
    container.SetOffset(Vector3::new(0.0, round(band.y * s) as f32, 0.0))?;
    let mut texts = Texts::default();
    let mut slot = None;
    let text = ctx.text;
    let inner = (WIDTH_DIP - 2.0 * PAD_DIP) * s;
    match row {
        Row::Header { text: t, trailing } => {
            let mut room = inner;
            if let Some(tr) = trailing {
                let tw = ctx
                    .painter
                    .measure(tr, FontKind::Text, SMALL_FONT_DIP * s)
                    .map(|(tw, _)| tw.ceil() + 2.0)
                    .unwrap_or(60.0 * s);
                let x = (WIDTH_DIP - PAD_DIP) * s - tw;
                texts.add(
                    SMALL_FONT_DIP,
                    dim(text),
                    tr,
                    FontKind::Text,
                    (x, 0.0, tw, h as f32),
                );
                room = (x - PAD_DIP * s) - PAD_DIP * s;
            }
            texts.add(
                HEADER_FONT_DIP,
                text,
                t,
                FontKind::Text,
                (PAD_DIP * s, 0.0, room.max(1.0), h as f32),
            );
        }
        Row::Tiles(tiles) => {
            for t in tiles {
                let Some(a) = part_rect(l, Part::Tile(t.id)) else {
                    continue;
                };
                let more = t.more.then(|| part_rect(l, Part::TileMore(t.id))).flatten();
                let whole = Rect {
                    w: a.w + more.map_or(0.0, |m| m.w),
                    ..a
                };
                let fg = if t.on { WHITE } else { text };
                let fill = if t.on {
                    ctx.accent
                } else {
                    alpha(text, TILE_ALPHA)
                };
                let wr = local(&whole, band.y, s);
                plate(ctx, &container, wr, TILE_RADIUS_DIP * s, fill)?;
                let label_w = (local(&a, band.y, s).2 - wr.0) as f32 - 2.0 * PAD_DIP * s;
                let glyph = t.glyph.to_string();
                texts.add(
                    GLYPH_DIP + 2.0,
                    fg,
                    &glyph,
                    FontKind::Glyph,
                    (
                        wr.0 as f32 + PAD_DIP * s,
                        wr.1 as f32 + 8.0 * s,
                        24.0 * s,
                        24.0 * s,
                    ),
                );
                texts.add(
                    SMALL_FONT_DIP,
                    fg,
                    &t.label,
                    FontKind::Text,
                    (
                        wr.0 as f32 + PAD_DIP * s,
                        wr.1 as f32 + 36.0 * s,
                        label_w.max(1.0),
                        20.0 * s,
                    ),
                );
                if let Some(m) = more {
                    let mr = local(&m, band.y, s);
                    let line_c = if t.on {
                        alpha(WHITE, 90)
                    } else {
                        alpha(text, SEP_ALPHA)
                    };
                    let inset = round(12.0 * s);
                    plate(
                        ctx,
                        &container,
                        (mr.0, mr.1 + inset, mr.0 + round(s).max(1), mr.3 - inset),
                        0.0,
                        line_c,
                    )?;
                    texts.add(SMALL_FONT_DIP, fg, CHEVRON, FontKind::Glyph, boxf(mr));
                }
            }
        }
        Row::Slider {
            id,
            glyph,
            value,
            trailing,
        } => {
            let g = glyph.to_string();
            texts.add(
                16.0,
                text,
                &g,
                FontKind::Glyph,
                (
                    PAD_DIP * s,
                    0.0,
                    panel_kit::SLIDER_GLYPH_W_DIP * s,
                    h as f32,
                ),
            );
            if let Some(r) = part_rect(l, Part::Slider(*id)) {
                let r = local(&r, band.y, s);
                let (a, b) = (
                    r.0 as f32 + THUMB_DIP / 2.0 * s,
                    r.2 as f32 - THUMB_DIP / 2.0 * s,
                );
                let v = if value.is_finite() {
                    value.clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let cy = h as f32 / 2.0;
                let th = round(TRACK_H_DIP * s).max(1);
                let top = round(cy) - th / 2;
                plate(
                    ctx,
                    &container,
                    (round(a), top, round(b), top + th),
                    th as f32 / 2.0,
                    alpha(text, TRACK_ALPHA),
                )?;
                let x = a + v * (b - a);
                if v > 0.0 {
                    plate(
                        ctx,
                        &container,
                        (round(a), top, round(x), top + th),
                        th as f32 / 2.0,
                        ctx.accent,
                    )?;
                }
                let d = round(THUMB_DIP * s);
                let (tx, ty) = (round(x) - d / 2, round(cy) - d / 2);
                plate(
                    ctx,
                    &container,
                    (tx, ty, tx + d, ty + d),
                    d as f32 / 2.0,
                    ctx.accent,
                )?;
            }
            if let Some((tid, g)) = trailing {
                if let Some(r) = part_rect(l, Part::SliderTrailing(*tid)) {
                    let g = g.to_string();
                    texts.add(16.0, text, &g, FontKind::Glyph, boxf(local(&r, band.y, s)));
                }
            }
        }
        Row::Item {
            id,
            glyph,
            text: t,
            detail,
            checked,
            busy,
        } => {
            if let Some(r) = part_rect(l, Part::Item(*id)) {
                let r = local(&r, band.y, s);
                let x = r.0 as f32 + 12.0 * s + if glyph.is_some() { 32.0 * s } else { 0.0 };
                if let Some(g) = glyph {
                    let g = g.to_string();
                    texts.add(
                        GLYPH_DIP,
                        text,
                        &g,
                        FontKind::Glyph,
                        (r.0 as f32 + 12.0 * s, 0.0, 24.0 * s, h as f32),
                    );
                }
                let tw = (r.2 as f32 - 12.0 * s - 28.0 * s - x).max(1.0);
                match detail {
                    Some(d) => {
                        texts.add(
                            FONT_DIP,
                            text,
                            t,
                            FontKind::Text,
                            (x, 3.0 * s, tw, 20.0 * s),
                        );
                        texts.add(
                            SMALL_FONT_DIP,
                            dim(text),
                            d,
                            FontKind::Text,
                            (x, 22.0 * s, tw, 18.0 * s),
                        );
                    }
                    None => texts.add(FONT_DIP, text, t, FontKind::Text, (x, 0.0, tw, h as f32)),
                }
                let mark = (r.2 as f32 - 12.0 * s - 16.0 * s, 0.0, 16.0 * s, h as f32);
                if *busy {
                    texts.add(FONT_DIP, dim(text), BUSY, FontKind::Text, mark);
                } else if *checked {
                    texts.add(SMALL_FONT_DIP + 2.0, text, CHECK, FontKind::Glyph, mark);
                }
            }
        }
        Row::Buttons(btns) => {
            for b in btns {
                let Some(r) = part_rect(l, Part::Button(b.id)) else {
                    continue;
                };
                let r = local(&r, band.y, s);
                plate(
                    ctx,
                    &container,
                    r,
                    PLATE_RADIUS_DIP * s,
                    alpha(text, BUTTON_ALPHA),
                )?;
                let tw = ctx
                    .painter
                    .measure(&b.text, FontKind::Text, FONT_DIP * s)
                    .map(|(tw, _)| tw.ceil() + 4.0)
                    .unwrap_or(b.text.chars().count() as f32 * FONT_DIP * s * 0.6);
                let gw = if b.glyph.is_some() { 22.0 * s } else { 0.0 };
                let avail = (r.2 - r.0) as f32 - 8.0 * s;
                let tw = tw.min((avail - gw).max(1.0));
                let mut x = r.0 as f32 + ((r.2 - r.0) as f32 - tw - gw) / 2.0;
                let (y, bh) = (r.1 as f32, (r.3 - r.1) as f32);
                if let Some(g) = b.glyph {
                    let g = g.to_string();
                    texts.add(16.0, text, &g, FontKind::Glyph, (x, y, 16.0 * s, bh));
                    x += gw;
                }
                texts.add(FONT_DIP, text, &b.text, FontKind::Text, (x, y, tw, bh));
            }
        }
        Row::Text(t) => texts.add(
            SMALL_FONT_DIP,
            dim(text),
            t,
            FontKind::Text,
            (PAD_DIP * s, 0.0, inner, h as f32),
        ),
        Row::Separator => {
            let y = h as i32 / 2;
            let inset = round(PAD_DIP * s);
            plate(
                ctx,
                &container,
                (inset, y, w as i32 - inset, y + round(s).max(1)),
                0.0,
                alpha(text, SEP_ALPHA),
            )?;
        }
        Row::Custom { id, .. } => {
            if let Some(r) = part_rect(l, Part::Custom(*id)) {
                let r = local(&r, band.y, s);
                let v = ctx.comp.CreateContainerVisual()?;
                v.SetOffset(Vector3::new(r.0 as f32, r.1 as f32, 0.0))?;
                v.SetSize(Vector2::new((r.2 - r.0) as f32, (r.3 - r.1) as f32))?;
                container.Children()?.InsertAtTop(&v)?;
                slot = Some((*id, v));
            }
        }
    }
    texts.flush(ctx, &container, w, h)?;
    Ok(RowVis {
        row: row.clone(),
        y: band.y,
        container,
        slot,
    })
}

/// The keyboard focus ring: an accent stroke around a part.
struct Ring {
    visual: ShapeVisual,
    geom: CompositionRoundedRectangleGeometry,
    shape: CompositionSpriteShape,
    brush: CompositionColorBrush,
}

impl Ring {
    fn new(comp: &Compositor) -> Result<Self> {
        let geom = comp.CreateRoundedRectangleGeometry()?;
        let brush = comp.CreateColorBrush()?;
        let shape = comp.CreateSpriteShapeWithGeometry(&geom)?;
        shape.SetStrokeBrush(&brush)?;
        let visual = comp.CreateShapeVisual()?;
        visual.Shapes()?.Append(&shape)?;
        visual.SetIsVisible(false)?;
        Ok(Self {
            visual,
            geom,
            shape,
            brush,
        })
    }

    fn set(&self, r: PxRect, radius: f32, t: f32, c: Rgba) -> Result<()> {
        let (w, h) = ((r.2 - r.0).max(0) as f32, (r.3 - r.1).max(0) as f32);
        self.visual
            .SetOffset(Vector3::new(r.0 as f32, r.1 as f32, 0.0))?;
        self.visual.SetSize(Vector2::new(w, h))?;
        // The stroke is centred on the geometry: inset by half its width.
        let half = t / 2.0;
        self.geom.SetOffset(Vector2::new(half, half))?;
        self.geom
            .SetSize(Vector2::new((w - t).max(0.0), (h - t).max(0.0)))?;
        let rad = (radius - half).max(0.0);
        self.geom.SetCornerRadius(Vector2::new(rad, rad))?;
        self.shape.SetStrokeThickness(t)?;
        self.brush.SetColor(color(c))?;
        self.visual.SetIsVisible(true)
    }
}

/// An open panel; the window goes when it is dropped.
pub struct PanelView {
    // Field order = drop order: the visuals before the flyout (window) they live in.
    ring: Ring,
    hot_plate: RoundRect,
    rows: Vec<RowVis>,
    rows_layer: ContainerVisual,
    content: ContainerVisual,
    viewport: ContainerVisual,
    layout: PanelLayout,
    anchor: PxRect,
    position: Position,
    style: MaterialStyle,
    accent: Rgba,
    scale: f32,
    scroll: f32,
    hot: Option<Part>,
    pressed: Option<Part>,
    focus: Option<Part>,
    comp: Compositor,
    flyout: Flyout,
}

impl PanelView {
    /// Opens `rows` as a panel anchored like the window preview: above `anchor_px` (below it
    /// for a top bar), centred on it and kept inside the work area.
    pub fn show(
        comp: &Comp,
        painter: &Painter,
        style: &MaterialStyle,
        dpi: u32,
        anchor_px: PxRect,
        position: Position,
        rows: &[Row],
    ) -> Result<Self> {
        let l = layout(rows);
        let flyout = Flyout::show(comp, anchor_px, position, l.size, dpi, style)?;
        let s = flyout.scale();
        let compositor: Compositor = (**comp).clone();

        let viewport = compositor.CreateContainerVisual()?;
        let clip = compositor.CreateInsetClip()?;
        viewport.SetClip(&clip)?;
        let content = compositor.CreateContainerVisual()?;
        let rows_layer = compositor.CreateContainerVisual()?;
        let hot_plate = RoundRect::new(&compositor)?;
        hot_plate.visual.SetIsVisible(false)?;
        let ring = Ring::new(&compositor)?;
        let kids = content.Children()?;
        kids.InsertAtTop(&rows_layer)?;
        kids.InsertAtTop(&hot_plate.visual)?;
        kids.InsertAtTop(&ring.visual)?;
        viewport.Children()?.InsertAtTop(&content)?;
        flyout.root().Children()?.InsertAtTop(&viewport)?;

        let mut view = Self {
            ring,
            hot_plate,
            rows: Vec::new(),
            rows_layer,
            content,
            viewport,
            layout: PanelLayout {
                size: (0.0, 0.0),
                parts: Vec::new(),
                content_h: 0.0,
                rows: Vec::new(),
            },
            anchor: anchor_px,
            position,
            style: *style,
            accent: ACCENT,
            scale: s,
            scroll: 0.0,
            hot: None,
            pressed: None,
            focus: None,
            comp: compositor,
            flyout,
        };
        view.apply(painter, l, rows)?;
        Ok(view)
    }

    /// Shows `rows`: rows that did not change keep their visuals (and a Custom row its
    /// slot); a new size reshapes the window, keeping it next to the anchor.
    pub fn update(&mut self, comp: &Comp, painter: &Painter, rows: &[Row]) -> Result<()> {
        let l = layout(rows);
        if l.size != self.layout.size {
            self.flyout
                .reshape(comp, self.anchor, self.position, l.size, &self.style)?;
        }
        self.apply(painter, l, rows)
    }

    fn apply(&mut self, painter: &Painter, l: PanelLayout, rows: &[Row]) -> Result<()> {
        let s = self.scale;
        let ctx = Ctx {
            comp: &self.comp,
            painter,
            s,
            text: self.style.text,
            accent: self.accent,
        };
        let mut old: Vec<Option<RowVis>> = std::mem::take(&mut self.rows)
            .into_iter()
            .map(Some)
            .collect();
        let mut fresh: Vec<RowVis> = Vec::with_capacity(rows.len());
        let layer = self.rows_layer.Children()?;
        for (i, row) in rows.iter().enumerate() {
            let same = old
                .iter()
                .position(|o| o.as_ref().is_some_and(|o| o.row == *row));
            let vis = match same.and_then(|k| old[k].take()) {
                Some(mut v) => {
                    let band_y = l.rows[i].y;
                    if v.y != band_y {
                        v.container
                            .SetOffset(Vector3::new(0.0, round(band_y * s) as f32, 0.0))?;
                        v.y = band_y;
                    }
                    v
                }
                None => {
                    let v = build_row(&ctx, &l, i, row)?;
                    layer.InsertAtTop(&v.container)?;
                    v
                }
            };
            fresh.push(vis);
        }
        for gone in old.into_iter().flatten() {
            let _ = layer.Remove(&gone.container);
        }
        self.rows = fresh;
        self.layout = l;

        let (w, h) = (
            round(self.layout.size.0 * s) as f32,
            round(self.layout.size.1 * s) as f32,
        );
        self.viewport.SetSize(Vector2::new(w, h))?;
        self.content
            .SetSize(Vector2::new(w, round(self.layout.content_h * s) as f32))?;
        self.scroll = self.scroll.clamp(0.0, self.max_scroll());
        self.place_content()?;
        self.refresh_overlays()
    }

    fn max_scroll(&self) -> f32 {
        (self.layout.content_h - self.layout.size.1).max(0.0)
    }

    fn place_content(&self) -> Result<()> {
        self.content.SetOffset(Vector3::new(
            0.0,
            -round(self.scroll * self.scale) as f32,
            0.0,
        ))
    }

    /// Moves the hover/press plate and the focus ring to their parts.
    fn refresh_overlays(&self) -> Result<()> {
        let s = self.scale;
        let px = |r: &Rect| {
            (
                round(r.x * s),
                round(r.y * s),
                round(r.right() * s),
                round(r.bottom() * s),
            )
        };
        let text = self.style.text;
        // Sliders and holes have no plate: the thumb and the owner's drawing react instead.
        let plate_for = |p: Option<Part>| {
            p.filter(|p| !matches!(p, Part::Slider(_) | Part::Custom(_)))
                .and_then(|p| part_rect(&self.layout, p).map(|r| (p, r)))
        };
        let plate = plate_for(self.pressed)
            .map(|x| (x, PRESSED_ALPHA))
            .or_else(|| plate_for(self.hot).map(|x| (x, HOT_ALPHA)));
        match plate {
            Some(((p, r), a)) => {
                let radius = match p {
                    Part::Tile(_) | Part::TileMore(_) => TILE_RADIUS_DIP,
                    _ => PLATE_RADIUS_DIP,
                };
                self.hot_plate.set(px(&r), radius * s, alpha(text, a))?;
                self.hot_plate.visual.SetIsVisible(true)?;
            }
            None => self.hot_plate.visual.SetIsVisible(false)?,
        }
        match self
            .focus
            .and_then(|p| part_rect(&self.layout, p).map(|r| (p, r)))
        {
            Some((p, r)) => {
                let radius = match p {
                    Part::Tile(_) | Part::TileMore(_) => TILE_RADIUS_DIP,
                    _ => PLATE_RADIUS_DIP,
                };
                self.ring.set(px(&r), radius * s, RING_DIP * s, self.accent)
            }
            None => self.ring.visual.SetIsVisible(false),
        }
    }

    /// Highlights `p` (hover); only the plate moves.
    pub fn set_hot(&mut self, p: Option<Part>) -> Result<()> {
        if p == self.hot {
            return Ok(());
        }
        self.hot = p;
        self.refresh_overlays()
    }

    /// Shows `p` as pressed (a stronger plate).
    pub fn set_pressed(&mut self, p: Option<Part>) -> Result<()> {
        if p == self.pressed {
            return Ok(());
        }
        self.pressed = p;
        self.refresh_overlays()
    }

    /// Draws the keyboard focus ring around `p`.
    pub fn set_focus(&mut self, p: Option<Part>) -> Result<()> {
        if p == self.focus {
            return Ok(());
        }
        self.focus = p;
        self.refresh_overlays()
    }

    /// Scrolls the content by `dy` DIPs (positive = down), clamped to the content.
    pub fn scroll_by(&mut self, dy: f32) -> Result<()> {
        let to = (self.scroll + dy).clamp(0.0, self.max_scroll());
        if to == self.scroll {
            return Ok(());
        }
        self.scroll = to;
        self.place_content()
    }

    /// Current scroll offset in DIPs.
    pub fn scroll(&self) -> f32 {
        self.scroll
    }

    /// The part under `client_px` (panel client px).
    pub fn hit_px(&self, client_px: (i32, i32)) -> Option<Part> {
        let s = self.scale;
        panel_kit::hit(
            &self.layout,
            (client_px.0 as f32 / s, client_px.1 as f32 / s),
            self.scroll,
        )
    }

    /// Value (0..=1) of slider `id` for a pointer at `x_px` (panel client px).
    pub fn slider_value(&self, id: u32, x_px: i32) -> Option<f32> {
        panel_kit::slider_value(&self.layout, id, x_px as f32 / self.scale)
    }

    /// The layout of what is shown now.
    pub fn layout(&self) -> &PanelLayout {
        &self.layout
    }

    /// The visual of Custom row `id`, to attach sprites to. It scrolls with the content and
    /// stays the same visual across [`update`](Self::update) while the row keeps its id and
    /// height. Its origin is the top-left corner of [`custom_rect_px`](Self::custom_rect_px).
    pub fn custom_slot(&self, id: u32) -> Option<ContainerVisual> {
        self.rows.iter().find_map(|r| {
            r.slot
                .as_ref()
                .filter(|(i, _)| *i == id)
                .map(|(_, v)| v.clone())
        })
    }

    /// Custom row `id`'s rectangle in panel client px, for the content scrolled to the top.
    pub fn custom_rect_px(&self, id: u32) -> Option<PxRect> {
        let r = part_rect(&self.layout, Part::Custom(id))?;
        let s = self.scale;
        Some((
            round(r.x * s),
            round(r.y * s),
            round(r.right() * s),
            round(r.bottom() * s),
        ))
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    pub fn hwnd(&self) -> HWND {
        self.flyout.hwnd()
    }

    /// Screen rectangle in px.
    pub fn rect_px(&self) -> PxRect {
        self.flyout.rect_px()
    }

    /// Takes the foreground (the panel then hears keys; see `flyout::take_deactivated_of`).
    pub fn activate(&self) -> bool {
        self.flyout.activate()
    }
}
