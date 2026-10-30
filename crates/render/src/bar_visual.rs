//! The visual tree of one bar surface: backdrop, Start, app items (icon, plate, running
//! dots), more, quick button and clock.
//!
//! [`BarVisual::update`] diffs the requested state against what is on screen and touches
//! only the visuals that changed; text and icons are drawn into surfaces once and reused.
//! Positions come from the layout in DIPs and are snapped to the window's physical pixels.

use std::sync::Arc;
use std::time::Duration;

use vitro_core::image::BgraImage;
use vitro_core::layout::{BarLayout, Rect};
use vitro_core::style::{HoverStyle, MaterialStyle, Rgba};
use windows::core::{Interface, Result, HSTRING};
use windows::Foundation::TimeSpan;
use windows::UI::Composition::{
    CompositionColorBrush, CompositionRoundedRectangleGeometry, Compositor, ContainerVisual,
    ShapeVisual, SpriteVisual, Visual, VisualCollection,
};
use windows_numerics::{Vector2, Vector3};

use crate::compositor::Comp;
use crate::device::GpuDevice;
use crate::material::{backdrop_visual, color};
use crate::surfaces::{FontKind, Painter};

/// What an app item shows. `dots` is the number of running windows, capped at 3.
/// The icon is shared with the icon store, never copied; a different `Arc` counts as a
/// different icon (it is redrawn), the same `Arc` as unchanged.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ItemState {
    pub icon: Option<Arc<BgraImage>>,
    pub dots: u8,
    pub active: bool,
    pub attention: bool,
    pub hovered: bool,
    pub pressed: bool,
}

/// Inputs that change rarely (monitor DPI, accent colour, the Animation effects setting).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisualEnv {
    pub dpi: u32,
    /// Straight-alpha accent colour (alpha ignored).
    pub accent: Rgba,
    pub animations: bool,
}

impl Default for VisualEnv {
    fn default() -> Self {
        Self {
            dpi: 96,
            accent: Rgba(0x00, 0x78, 0xD4, 255),
            animations: true,
        }
    }
}

pub const GLYPH_START: char = '\u{E700}';
pub const GLYPH_QUICK: char = '\u{E9E9}';
pub const GLYPH_MORE: char = '\u{E712}';

/// Font sizes and geometry in DIPs.
const GLYPH_DIP: f32 = 16.0;
const CLOCK_FONT_DIP: f32 = 12.0;
const DOT_DIP: f32 = 4.0;
const ACTIVE_DOT_DIP: f32 = 12.0;
const DOT_GAP_DIP: f32 = 3.0;
const DOT_BOTTOM_DIP: f32 = 2.0;
const PLATE_INSET_DIP: f32 = 2.0;
const PLATE_RADIUS_DIP: f32 = 6.0;
/// One attention pulse is a 250 ms fade down and a 250 ms fade back; it runs twice.
const PULSE_HALF_MS: u64 = 250;

const DOT_ALPHA: u8 = 150;
const PLATE_ACTIVE_ALPHA: u8 = 18;
const PLATE_HOVER_ALPHA: u8 = 28;
const PLATE_PRESSED_ALPHA: u8 = 42;
const PLATE_ATTENTION_ALPHA: u8 = 110;
const PLACEHOLDER_ALPHA: u8 = 51; // 20 %

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PxRect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

#[derive(Clone, Debug, PartialEq)]
enum Content {
    /// Glyph or text centred in the whole cell.
    Text {
        text: String,
        kind: FontKind,
        size_px: i32,
        color: Rgba,
        w: i32,
        h: i32,
    },
    /// Icon centred in the cell, `dy` px above centre.
    Icon { image: IconRef, dy: i32 },
    /// Rounded square of `size` px in place of an icon that has not loaded yet.
    Placeholder {
        size: i32,
        color: Rgba,
        dy: i32,
        radius: i32,
    },
}

/// An icon compared by identity (`Arc::ptr_eq`), so diffing never compares pixels.
#[derive(Clone, Debug)]
struct IconRef(Arc<BgraImage>);

impl PartialEq for IconRef {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

#[derive(Clone, Debug, PartialEq)]
struct DotsSpec {
    count: u8,
    active: bool,
    dot: Rgba,
    active_dot: Rgba,
    /// Dot size, active dot width, gap, distance from the cell bottom (px).
    metrics: (i32, i32, i32, i32),
}

#[derive(Clone, Debug, PartialEq)]
struct CellSpec {
    rect: PxRect,
    content: Content,
    /// Alpha 0 = no plate.
    plate: Rgba,
    plate_inset: i32,
    plate_radius: i32,
    attention: bool,
    dots: Option<DotsSpec>,
}

struct Shape {
    visual: ShapeVisual,
    geom: CompositionRoundedRectangleGeometry,
    brush: CompositionColorBrush,
}

impl Shape {
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

    /// Rounded rect at `(x, y)` of `w` x `h` px inside a visual of `size`.
    fn set(&self, size: Vector2, r: PxRect, radius: i32, c: Rgba) -> Result<()> {
        self.visual.SetSize(size)?;
        self.geom.SetOffset(Vector2::new(r.x as f32, r.y as f32))?;
        self.geom
            .SetSize(Vector2::new(r.w.max(0) as f32, r.h.max(0) as f32))?;
        self.geom
            .SetCornerRadius(Vector2::new(radius as f32, radius as f32))?;
        self.brush.SetColor(color(c))
    }
}

/// One item on the bar: plate (bottom), placeholder, content sprite, dots (top).
struct Cell {
    root: ContainerVisual,
    plate: Shape,
    placeholder: Shape,
    sprite: SpriteVisual,
    dots: ShapeVisual,
    /// What is on screen; `None` until first applied (or after `invalidate`).
    shown: Option<CellSpec>,
}

impl Cell {
    fn new(comp: &Compositor, parent: &VisualCollection) -> Result<Self> {
        let root = comp.CreateContainerVisual()?;
        let plate = Shape::new(comp)?;
        let placeholder = Shape::new(comp)?;
        let sprite = comp.CreateSpriteVisual()?;
        let dots = comp.CreateShapeVisual()?;
        let children = root.Children()?;
        children.InsertAtTop(&plate.visual)?;
        children.InsertAtTop(&placeholder.visual)?;
        children.InsertAtTop(&sprite)?;
        children.InsertAtTop(&dots)?;
        parent.InsertAtTop(&root)?;
        Ok(Self {
            root,
            plate,
            placeholder,
            sprite,
            dots,
            shown: None,
        })
    }

    fn remove(self, parent: &VisualCollection) {
        let _ = parent.Remove(&self.root);
    }

    /// Returns how many surfaces were drawn.
    fn apply(
        &mut self,
        comp: &Compositor,
        painter: &Painter,
        spec: CellSpec,
        animations: bool,
    ) -> Result<u32> {
        let old = self.shown.take();
        let r = spec.rect;
        let size = Vector2::new(r.w as f32, r.h as f32);
        let moved = old.as_ref().is_none_or(|o| o.rect != r);
        if moved {
            self.root
                .SetOffset(Vector3::new(r.x as f32, r.y as f32, 0.0))?;
            self.root.SetSize(size)?;
        }

        let plate_changed = old.as_ref().is_none_or(|o| {
            o.plate != spec.plate
                || o.plate_inset != spec.plate_inset
                || o.plate_radius != spec.plate_radius
        });
        if moved || plate_changed {
            let i = spec.plate_inset;
            let inner = PxRect {
                x: i,
                y: i,
                w: r.w - 2 * i,
                h: r.h - 2 * i,
            };
            self.plate.set(size, inner, spec.plate_radius, spec.plate)?;
        }
        let was_attention = old.as_ref().is_some_and(|o| o.attention);
        if spec.attention && !was_attention && animations {
            // Attention: the accent plate pulses twice, then stays.
            let anim = comp.CreateScalarKeyFrameAnimation()?;
            anim.InsertKeyFrame(0.0, 1.0)?;
            anim.InsertKeyFrame(0.5, 0.3)?;
            anim.InsertKeyFrame(1.0, 1.0)?;
            anim.SetDuration(TimeSpan::from(Duration::from_millis(2 * PULSE_HALF_MS)))?;
            anim.SetIterationCount(2)?;
            self.plate
                .visual
                .StartAnimation(&HSTRING::from("Opacity"), &anim)?;
        } else if !spec.attention && was_attention {
            self.plate.visual.StopAnimation(&HSTRING::from("Opacity"))?;
            self.plate.visual.SetOpacity(1.0)?;
        }

        let content_changed = old.as_ref().is_none_or(|o| o.content != spec.content);
        let mut drawn = 0;
        if content_changed || moved {
            drawn = self.apply_content(comp, painter, &spec, content_changed)?;
        }

        let dots_changed = old.as_ref().is_none_or(|o| o.dots != spec.dots);
        if dots_changed || moved {
            self.apply_dots(comp, size, spec.dots.as_ref())?;
        }
        self.shown = Some(spec);
        Ok(drawn)
    }

    /// Returns how many surfaces were drawn (0 or 1).
    fn apply_content(
        &mut self,
        comp: &Compositor,
        painter: &Painter,
        spec: &CellSpec,
        redraw: bool,
    ) -> Result<u32> {
        let r = spec.rect;
        let mut drawn = 0;
        match &spec.content {
            Content::Placeholder {
                size,
                color: c,
                dy,
                radius,
            } => {
                self.sprite.SetIsVisible(false)?;
                self.placeholder.visual.SetIsVisible(true)?;
                let square = PxRect {
                    x: (r.w - size) / 2,
                    y: (r.h - size) / 2 - dy,
                    w: *size,
                    h: *size,
                };
                let full = Vector2::new(r.w as f32, r.h as f32);
                self.placeholder.set(full, square, *radius, *c)?;
            }
            Content::Text {
                text,
                kind,
                size_px,
                color: c,
                w,
                h,
            } => {
                self.placeholder.visual.SetIsVisible(false)?;
                if redraw {
                    let surface =
                        painter.text(text, *kind, *size_px as f32, *c, *w as u32, *h as u32)?;
                    self.sprite
                        .SetBrush(&comp.CreateSurfaceBrushWithSurface(&surface)?)?;
                    drawn = 1;
                }
                self.sprite.SetSize(Vector2::new(*w as f32, *h as f32))?;
                self.sprite.SetOffset(Vector3::new(0.0, 0.0, 0.0))?;
                self.sprite.SetIsVisible(true)?;
            }
            Content::Icon { image, dy } => {
                self.placeholder.visual.SetIsVisible(false)?;
                if redraw {
                    let surface = painter.image(&image.0)?;
                    self.sprite
                        .SetBrush(&comp.CreateSurfaceBrushWithSurface(&surface)?)?;
                    drawn = 1;
                }
                let (w, h) = (image.0.width as i32, image.0.height as i32);
                let x = (r.w - w) / 2;
                let y = (r.h - h) / 2 - dy;
                self.sprite.SetSize(Vector2::new(w as f32, h as f32))?;
                self.sprite
                    .SetOffset(Vector3::new(x as f32, y as f32, 0.0))?;
                self.sprite.SetIsVisible(true)?;
            }
        }
        Ok(drawn)
    }

    fn apply_dots(
        &mut self,
        comp: &Compositor,
        size: Vector2,
        dots: Option<&DotsSpec>,
    ) -> Result<()> {
        self.dots.SetSize(size)?;
        let shapes = self.dots.Shapes()?;
        shapes.Clear()?;
        let Some(d) = dots else { return Ok(()) };
        let (dot, wide, gap, bottom) = d.metrics;
        let n = i32::from(d.count.min(3));
        if n == 0 {
            return Ok(());
        }
        let widths: Vec<i32> = (0..n)
            .map(|k| if d.active && k == 0 { wide } else { dot })
            .collect();
        let total: i32 = widths.iter().sum::<i32>() + gap * (n - 1);
        let mut x = (size.X as i32 - total) / 2;
        let y = size.Y as i32 - bottom - dot;
        for (k, w) in widths.into_iter().enumerate() {
            let geom = comp.CreateRoundedRectangleGeometry()?;
            geom.SetOffset(Vector2::new(x as f32, y as f32))?;
            geom.SetSize(Vector2::new(w as f32, dot as f32))?;
            geom.SetCornerRadius(Vector2::new(dot as f32 / 2.0, dot as f32 / 2.0))?;
            let c = if d.active && k == 0 {
                d.active_dot
            } else {
                d.dot
            };
            let shape = comp.CreateSpriteShapeWithGeometry(&geom)?;
            shape.SetFillBrush(&comp.CreateColorBrushWithColor(color(c))?)?;
            shapes.Append(&shape)?;
            x += w + gap;
        }
        Ok(())
    }
}

/// Keeps `slot` in sync with `spec`: creates, updates or removes the cell.
fn sync(
    slot: &mut Option<Cell>,
    spec: Option<CellSpec>,
    comp: &Compositor,
    painter: &Painter,
    parent: &VisualCollection,
    animations: bool,
) -> Result<u32> {
    match spec {
        None => {
            if let Some(cell) = slot.take() {
                cell.remove(parent);
            }
            Ok(0)
        }
        Some(spec) => {
            if slot.is_none() {
                *slot = Some(Cell::new(comp, parent)?);
            }
            let cell = slot.as_mut().expect("just created");
            if cell.shown.as_ref() == Some(&spec) {
                return Ok(0);
            }
            cell.apply(comp, painter, spec, animations)
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct BackdropKey {
    size: (i32, i32),
    radius: u32,
    style: MaterialStyle,
}

pub struct BarVisual {
    comp: Compositor,
    painter: Painter,
    index: usize,
    env: VisualEnv,
    root: ContainerVisual,
    content: ContainerVisual,
    backdrop: Option<(Visual, BackdropKey)>,
    start: Option<Cell>,
    apps: Vec<Option<Cell>>,
    more: Option<Cell>,
    quick: Option<Cell>,
    clock: Option<Cell>,
    surfaces_drawn: u64,
}

fn rgba_alpha(c: Rgba, a: u8) -> Rgba {
    Rgba(c.0, c.1, c.2, a)
}

/// True when the centre of `r` lies in `s` (items belong to exactly one surface).
fn owns(s: &Rect, r: &Rect) -> bool {
    let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
    cx >= s.x && cx < s.right() && cy >= s.y && cy < s.bottom()
}

impl BarVisual {
    /// The visual for `layout.surfaces[surface_index]`; put [`BarVisual::root`] into its window.
    pub fn new(comp: &Comp, gpu: &GpuDevice, surface_index: usize) -> Result<Self> {
        let compositor = comp.compositor.clone();
        let root = compositor.CreateContainerVisual()?;
        let content = compositor.CreateContainerVisual()?;
        root.Children()?.InsertAtTop(&content)?;
        Ok(Self {
            painter: Painter::new(gpu)?,
            comp: compositor,
            index: surface_index,
            env: VisualEnv::default(),
            root,
            content,
            backdrop: None,
            start: None,
            apps: Vec::new(),
            more: None,
            quick: None,
            clock: None,
            surfaces_drawn: 0,
        })
    }

    pub fn root(&self) -> &ContainerVisual {
        &self.root
    }

    pub fn surface_index(&self) -> usize {
        self.index
    }

    /// Number of text/icon surfaces drawn so far (diagnostics and tests: an update with
    /// unchanged input draws none).
    pub fn surfaces_drawn(&self) -> u64 {
        self.surfaces_drawn
    }

    /// Number of app items currently in the tree (diagnostics and tests).
    pub fn app_cells(&self) -> usize {
        self.apps.iter().flatten().count()
    }

    /// Takes effect on the next [`BarVisual::update`].
    pub fn set_env(&mut self, env: VisualEnv) {
        self.env = env;
    }

    /// Forgets what is on screen so the next update redraws every surface (after the GPU
    /// device was recreated, surface contents are lost).
    pub fn invalidate(&mut self) {
        self.backdrop = None;
        for cell in [
            &mut self.start,
            &mut self.more,
            &mut self.quick,
            &mut self.clock,
        ]
        .into_iter()
        .chain(self.apps.iter_mut())
        .flatten()
        {
            cell.shown = None;
        }
    }

    pub fn update(
        &mut self,
        layout: &BarLayout,
        items: &[ItemState],
        clock: &str,
        style: &MaterialStyle,
        _hover: &HoverStyle,
    ) -> Result<()> {
        let Some(surface) = layout.surfaces.get(self.index).copied() else {
            return self.root.SetIsVisible(false);
        };
        self.root.SetIsVisible(true)?;
        let scale = self.env.dpi as f32 / 96.0;
        let px = |v: f32| (v * scale).round() as i32;
        let (ox, oy) = (px(surface.x), px(surface.y));
        let size = (px(surface.right()) - ox, px(surface.bottom()) - oy);
        let cell_rect = |r: &Rect| PxRect {
            x: px(r.x) - ox,
            y: px(r.y) - oy,
            w: px(r.right()) - px(r.x),
            h: px(r.bottom()) - px(r.y),
        };

        let key = BackdropKey {
            size,
            radius: (layout.radius * scale).to_bits(),
            style: *style,
        };
        if self.backdrop.as_ref().map(|b| &b.1) != Some(&key) {
            let children = self.root.Children()?;
            if let Some((old, _)) = self.backdrop.take() {
                children.Remove(&old)?;
            }
            let size_v = Vector2::new(size.0 as f32, size.1 as f32);
            self.root.SetSize(size_v)?;
            self.content.SetSize(size_v)?;
            let visual: Visual =
                backdrop_visual(&self.comp, style, size_v, layout.radius * scale)?.cast()?;
            children.InsertAtBottom(&visual)?;
            self.backdrop = Some((visual, key));
        }

        let text = style.text;
        let plate_inset = px(PLATE_INSET_DIP);
        let plate_radius = px(PLATE_RADIUS_DIP);
        let glyph = |r: &Rect, ch: char| -> Option<CellSpec> {
            owns(&surface, r).then(|| {
                let rect = cell_rect(r);
                CellSpec {
                    rect,
                    content: Content::Text {
                        text: ch.to_string(),
                        kind: FontKind::Glyph,
                        size_px: px(GLYPH_DIP),
                        color: text,
                        w: rect.w,
                        h: rect.h,
                    },
                    plate: Rgba(0, 0, 0, 0),
                    plate_inset,
                    plate_radius,
                    attention: false,
                    dots: None,
                }
            })
        };
        let start = glyph(&layout.start, GLYPH_START);
        let more = layout.more.as_ref().and_then(|r| glyph(r, GLYPH_MORE));
        let quick = glyph(&layout.quick, GLYPH_QUICK);
        let clock_spec = owns(&surface, &layout.clock).then(|| {
            let rect = cell_rect(&layout.clock);
            CellSpec {
                rect,
                content: Content::Text {
                    text: clock.to_string(),
                    kind: FontKind::Text,
                    size_px: px(CLOCK_FONT_DIP),
                    color: text,
                    w: rect.w,
                    h: rect.h,
                },
                plate: Rgba(0, 0, 0, 0),
                plate_inset,
                plate_radius,
                attention: false,
                dots: None,
            }
        });

        let accent = rgba_alpha(self.env.accent, 255);
        let dot_metrics = (
            px(DOT_DIP),
            px(ACTIVE_DOT_DIP),
            px(DOT_GAP_DIP),
            px(DOT_BOTTOM_DIP),
        );
        let icon_dy = px(1.0);
        let app_specs: Vec<Option<CellSpec>> = layout
            .apps
            .iter()
            .zip(items)
            .map(|(r, it)| {
                owns(&surface, r).then(|| {
                    let rect = cell_rect(r);
                    let icon_px = px(r.h.min(r.w) * 0.6).clamp(1, 96);
                    let content = match &it.icon {
                        Some(img) if img.is_valid() && img.width > 0 => Content::Icon {
                            image: IconRef(Arc::clone(img)),
                            dy: icon_dy,
                        },
                        _ => Content::Placeholder {
                            size: icon_px,
                            color: rgba_alpha(text, PLACEHOLDER_ALPHA),
                            dy: icon_dy,
                            radius: icon_px / 4,
                        },
                    };
                    let plate = if it.attention {
                        rgba_alpha(accent, PLATE_ATTENTION_ALPHA)
                    } else if it.pressed {
                        rgba_alpha(text, PLATE_PRESSED_ALPHA)
                    } else if it.hovered {
                        rgba_alpha(text, PLATE_HOVER_ALPHA)
                    } else if it.active {
                        rgba_alpha(text, PLATE_ACTIVE_ALPHA)
                    } else {
                        Rgba(0, 0, 0, 0)
                    };
                    CellSpec {
                        rect,
                        content,
                        plate,
                        plate_inset,
                        plate_radius,
                        attention: it.attention,
                        dots: (it.dots > 0).then(|| DotsSpec {
                            count: it.dots.min(3),
                            active: it.active,
                            dot: rgba_alpha(text, DOT_ALPHA),
                            active_dot: accent,
                            metrics: dot_metrics,
                        }),
                    }
                })
            })
            .collect();

        let comp = &self.comp;
        let painter = &self.painter;
        let parent = self.content.Children()?;
        let anim = self.env.animations;
        let mut drawn = sync(&mut self.start, start, comp, painter, &parent, anim)?;
        while self.apps.len() > app_specs.len() {
            if let Some(cell) = self.apps.pop().flatten() {
                cell.remove(&parent);
            }
        }
        self.apps.resize_with(app_specs.len(), || None);
        for (slot, spec) in self.apps.iter_mut().zip(app_specs) {
            drawn += sync(slot, spec, comp, painter, &parent, anim)?;
        }
        drawn += sync(&mut self.more, more, comp, painter, &parent, anim)?;
        drawn += sync(&mut self.quick, quick, comp, painter, &parent, anim)?;
        drawn += sync(&mut self.clock, clock_spec, comp, painter, &parent, anim)?;
        self.surfaces_drawn += u64::from(drawn);
        Ok(())
    }
}
