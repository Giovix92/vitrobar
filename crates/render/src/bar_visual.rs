//! The visual tree of one bar surface: backdrop, Start, app items (icon, plate, running
//! dots), more, the tray (overflow chevron and promoted tray icons), quick button and clock.
//!
//! [`BarVisual::update`] diffs the requested state against what is on screen and touches
//! only the visuals that changed; text and icons are drawn into surfaces once and reused.
//! Positions come from the layout in DIPs and are snapped to the window's physical pixels.
//!
//! Hover effects ([`HoverStyle`]) are composition animations on the app items: the
//! compositor runs them, so a hover costs one `update` and no surface redraw. With
//! [`VisualEnv::animations`] off, the final values are set without animation.

use std::sync::Arc;
use std::time::Duration;

use vitro_core::image::BgraImage;
use vitro_core::layout::{BarLayout, Rect};
use vitro_core::settings::Position;
use vitro_core::style::{HoverStyle, MaterialStyle, Rgba};
use windows::core::{h, Interface, Result, HSTRING};
use windows::Foundation::TimeSpan;
use windows::UI::Composition::{
    CompositionColorBrush, CompositionDrawingSurface, CompositionEasingFunction,
    CompositionMappingMode, CompositionRoundedRectangleGeometry, Compositor, ContainerVisual,
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

/// A tray icon in the bar. The icon is shared with the icon store, like [`ItemState::icon`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrayItemState {
    pub icon: Option<Arc<BgraImage>>,
    pub hovered: bool,
    pub pressed: bool,
}

/// The overflow chevron: shown only while some icons live in the overflow.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChevronState {
    pub shown: bool,
    pub hovered: bool,
    pub pressed: bool,
    /// The overflow is open.
    pub open: bool,
}

/// Inputs that change rarely (monitor DPI, accent colour, the Animation effects setting,
/// the screen edge the bar sits on).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisualEnv {
    pub dpi: u32,
    /// Straight-alpha accent colour (alpha ignored).
    pub accent: Rgba,
    pub animations: bool,
    /// Magnify grows icons away from this edge and Lift moves them away from it.
    pub position: Position,
}

impl Default for VisualEnv {
    fn default() -> Self {
        Self {
            dpi: 96,
            accent: Rgba(0x00, 0x78, 0xD4, 255),
            animations: true,
            position: Position::Bottom,
        }
    }
}

/// Motion (lift / scale) never runs longer than this (ruling R1).
pub const MAX_MOTION_MS: u32 = 250;
/// Sweep's lift and scale; its 450 ms `duration_ms` is the light streak only (ruling R1).
pub const SWEEP_MOTION_MS: u32 = 220;
/// Icon scale while pressed with the Plate effect.
pub const PRESSED_SCALE: f32 = 0.9;
/// Plate effect: white at 28 % behind the icon, with a 60 % white inner top highlight.
const HOVER_PLATE: Rgba = Rgba(255, 255, 255, 71);
const HOVER_PLATE_HIGHLIGHT: Rgba = Rgba(255, 255, 255, 153);
/// Sweep streak: white at 75 % in the middle of a 115 degree linear gradient.
const STREAK: Rgba = Rgba(255, 255, 255, 191);

/// Durations of the hover animations of `h` in ms: (motion, plate fade, streak).
pub fn hover_timing(h: &HoverStyle) -> (u32, u32, u32) {
    let motion = if h.sweep {
        SWEEP_MOTION_MS
    } else {
        h.duration_ms.min(MAX_MOTION_MS)
    };
    (motion, h.duration_ms.min(MAX_MOTION_MS), h.duration_ms)
}

/// Where a scaled icon stays put.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Anchor {
    Centre,
    /// Bottom-centre (top-centre when the bar is at the top).
    Edge,
}

/// Target transform of an item's icon: vertical offset in px and uniform scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Motion {
    pub dy: f32,
    pub scale: f32,
}

impl Default for Motion {
    fn default() -> Self {
        Self {
            dy: 0.0,
            scale: 1.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct HoverFx {
    motion: Motion,
    anchor: Anchor,
    top: bool,
    plate: bool,
    sweep: bool,
    /// Motion, fade and streak durations (ms).
    timing: (u32, u32, u32),
}

/// The hover effect of app item `i`, given the hovered item (if any).
fn hover_fx(
    h: &HoverStyle,
    i: usize,
    hovered: Option<usize>,
    pressed: bool,
    px_per_dip: f32,
    position: Position,
) -> HoverFx {
    let top = position == Position::Top;
    let is_hovered = hovered == Some(i);
    let neighbour = hovered.is_some_and(|k| k.abs_diff(i) == 1);
    let mut motion = Motion::default();
    if is_hovered {
        // Away from the screen edge: up on a bottom bar, down on a top bar.
        let lift = (h.lift * px_per_dip).round();
        motion.dy = if top { lift } else { -lift };
        motion.scale = h.scale;
    } else if neighbour {
        motion.scale = h.neighbour_scale;
    }
    if h.plate && pressed {
        motion.scale = PRESSED_SCALE;
    }
    HoverFx {
        motion,
        anchor: if h.neighbour_scale > 1.0 {
            Anchor::Edge
        } else {
            Anchor::Centre
        },
        top,
        plate: h.plate && is_hovered,
        sweep: h.sweep && is_hovered,
        timing: hover_timing(h),
    }
}

/// The springy curve of the mockup, cubic-bezier(0.3, 1.6, 0.5, 1).
fn spring(comp: &Compositor) -> Result<CompositionEasingFunction> {
    comp.CreateCubicBezierEasingFunction(Vector2::new(0.3, 1.6), Vector2::new(0.5, 1.0))?
        .cast()
}

/// CSS `ease`, cubic-bezier(0.25, 0.1, 0.25, 1).
fn ease(comp: &Compositor) -> Result<CompositionEasingFunction> {
    comp.CreateCubicBezierEasingFunction(Vector2::new(0.25, 0.1), Vector2::new(0.25, 1.0))?
        .cast()
}

fn duration(ms: u32) -> TimeSpan {
    TimeSpan::from(Duration::from_millis(u64::from(ms)))
}

#[derive(Clone, Copy)]
enum Value {
    Scalar(f32),
    Vec3(Vector3),
}

/// Animates `prop` (`Opacity`, `Offset` or `Scale`) of `v` towards `to` from wherever it is
/// now (a running animation is taken over smoothly); without animations, sets it at once.
fn animate(
    comp: &Compositor,
    v: &Visual,
    prop: &HSTRING,
    to: Value,
    ms: u32,
    springy: bool,
    animations: bool,
) -> Result<()> {
    if !animations || ms == 0 {
        v.StopAnimation(prop)?;
        return match (prop.to_string().as_str(), to) {
            ("Opacity", Value::Scalar(x)) => v.SetOpacity(x),
            ("Offset", Value::Vec3(x)) => v.SetOffset(x),
            ("Scale", Value::Vec3(x)) => v.SetScale(x),
            _ => Ok(()),
        };
    }
    let easing = if springy { spring(comp)? } else { ease(comp)? };
    let anim: windows::UI::Composition::CompositionAnimation = match to {
        Value::Scalar(x) => {
            let a = comp.CreateScalarKeyFrameAnimation()?;
            a.InsertExpressionKeyFrame(0.0, h!("this.StartingValue"))?;
            a.InsertKeyFrameWithEasingFunction(1.0, x, &easing)?;
            a.SetDuration(duration(ms))?;
            a.cast()?
        }
        Value::Vec3(x) => {
            let a = comp.CreateVector3KeyFrameAnimation()?;
            a.InsertExpressionKeyFrame(0.0, h!("this.StartingValue"))?;
            a.InsertKeyFrameWithEasingFunction(1.0, x, &easing)?;
            a.SetDuration(duration(ms))?;
            a.cast()?
        }
    };
    v.StartAnimation(prop, &anim)
}

pub const GLYPH_START: char = '\u{E700}';
pub const GLYPH_QUICK: char = '\u{E9E9}';
pub const GLYPH_MORE: char = '\u{E712}';
pub const GLYPH_CHEVRON_UP: char = '\u{E70E}';
pub const GLYPH_CHEVRON_DOWN: char = '\u{E70D}';
/// Tray icons are drawn at this size.
pub const TRAY_ICON_DIP: f32 = crate::tray_flyout::TRAY_ICON_DIP;

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
const PLATE_PRESSED_ALPHA: u8 = 42;
/// Tray icons and the chevron under the cursor (or the chevron while the overflow is open).
const PLATE_HOVER_ALPHA: u8 = 30;
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
    /// Hover effect (app items only).
    fx: Option<HoverFx>,
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

/// The Plate effect's tile: a clipped white sprite with a 1 px highlight row at the top.
struct HoverPlate {
    visual: SpriteVisual,
    clip: CompositionRoundedRectangleGeometry,
    highlight: SpriteVisual,
}

impl HoverPlate {
    fn new(comp: &Compositor) -> Result<Self> {
        let visual = comp.CreateSpriteVisual()?;
        visual.SetBrush(&comp.CreateColorBrushWithColor(color(HOVER_PLATE))?)?;
        let clip = comp.CreateRoundedRectangleGeometry()?;
        visual.SetClip(&comp.CreateGeometricClipWithGeometry(&clip)?)?;
        let highlight = comp.CreateSpriteVisual()?;
        highlight.SetBrush(&comp.CreateColorBrushWithColor(color(HOVER_PLATE_HIGHLIGHT))?)?;
        visual.Children()?.InsertAtTop(&highlight)?;
        visual.SetOpacity(0.0)?;
        Ok(Self {
            visual,
            clip,
            highlight,
        })
    }

    fn set_rect(&self, r: PxRect, radius: i32) -> Result<()> {
        let size = Vector2::new(r.w.max(0) as f32, r.h.max(0) as f32);
        self.visual
            .SetOffset(Vector3::new(r.x as f32, r.y as f32, 0.0))?;
        self.visual.SetSize(size)?;
        self.clip.SetSize(size)?;
        self.clip
            .SetCornerRadius(Vector2::new(radius as f32, radius as f32))?;
        self.highlight.SetSize(Vector2::new(size.X, 1.0))
    }
}

/// One item on the bar: plate (bottom), hover plate, the moving part (placeholder, content
/// sprite, sweep streak), dots (top).
struct Cell {
    root: ContainerVisual,
    plate: Shape,
    hover_plate: Option<HoverPlate>,
    /// Lifted and scaled by the hover effects.
    motion: ContainerVisual,
    placeholder: Shape,
    sprite: SpriteVisual,
    /// The drawn icon (the sweep streak is masked by it).
    icon_surface: Option<CompositionDrawingSurface>,
    streak: Option<SpriteVisual>,
    dots: ShapeVisual,
    /// What is on screen; `None` until first applied (or after `invalidate`).
    shown: Option<CellSpec>,
}

impl Cell {
    fn new(comp: &Compositor, parent: &VisualCollection) -> Result<Self> {
        let root = comp.CreateContainerVisual()?;
        let plate = Shape::new(comp)?;
        let motion = comp.CreateContainerVisual()?;
        let placeholder = Shape::new(comp)?;
        let sprite = comp.CreateSpriteVisual()?;
        let dots = comp.CreateShapeVisual()?;
        let moving = motion.Children()?;
        moving.InsertAtTop(&placeholder.visual)?;
        moving.InsertAtTop(&sprite)?;
        let children = root.Children()?;
        children.InsertAtTop(&plate.visual)?;
        children.InsertAtTop(&motion)?;
        children.InsertAtTop(&dots)?;
        parent.InsertAtTop(&root)?;
        Ok(Self {
            root,
            plate,
            hover_plate: None,
            motion,
            placeholder,
            sprite,
            icon_surface: None,
            streak: None,
            dots,
            shown: None,
        })
    }

    /// The icon's (or placeholder's, or text's) box inside the cell, in px.
    fn content_box(spec: &CellSpec) -> PxRect {
        let r = spec.rect;
        match &spec.content {
            Content::Icon { image, dy } => {
                let (w, h) = (image.0.width as i32, image.0.height as i32);
                PxRect {
                    x: (r.w - w) / 2,
                    y: (r.h - h) / 2 - dy,
                    w,
                    h,
                }
            }
            Content::Placeholder { size, dy, .. } => PxRect {
                x: (r.w - size) / 2,
                y: (r.h - size) / 2 - dy,
                w: *size,
                h: *size,
            },
            Content::Text { w, h, .. } => PxRect {
                x: 0,
                y: 0,
                w: *w,
                h: *h,
            },
        }
    }

    /// Hover effect: plate fade, lift / scale, sweep streak.
    fn apply_fx(
        &mut self,
        comp: &Compositor,
        old: Option<&CellSpec>,
        spec: &CellSpec,
        animations: bool,
    ) -> Result<()> {
        let Some(fx) = spec.fx else { return Ok(()) };
        let old_fx = old.and_then(|o| o.fx);
        let geometry_changed = old.is_none_or(|o| {
            o.rect != spec.rect
                || Self::content_box(o) != Self::content_box(spec)
                || o.fx.map(|f| (f.anchor, f.top)) != Some((fx.anchor, fx.top))
        });
        let (motion_ms, fade_ms, streak_ms) = fx.timing;
        let r = spec.rect;
        let b = Self::content_box(spec);

        // The hover plate is created on first use, then only faded in and out.
        if fx.plate && self.hover_plate.is_none() {
            let plate = HoverPlate::new(comp)?;
            self.root
                .Children()?
                .InsertAbove(&plate.visual, &self.plate.visual)?;
            self.hover_plate = Some(plate);
        }
        if let Some(plate) = &self.hover_plate {
            let was = old_fx.is_some_and(|o| o.plate);
            if geometry_changed || (fx.plate && !was) {
                let i = spec.plate_inset;
                let inner = PxRect {
                    x: i,
                    y: i,
                    w: r.w - 2 * i,
                    h: r.h - 2 * i,
                };
                plate.set_rect(inner, spec.plate_radius)?;
            }
            if was != fx.plate {
                let to = Value::Scalar(if fx.plate { 1.0 } else { 0.0 });
                let v: Visual = plate.visual.cast()?;
                animate(comp, &v, h!("Opacity"), to, fade_ms, false, animations)?;
            }
        }

        // Motion: the (static) centre point, then offset and scale.
        let v: Visual = self.motion.cast()?;
        if geometry_changed {
            self.motion.SetSize(Vector2::new(r.w as f32, r.h as f32))?;
            let cx = b.x as f32 + b.w as f32 / 2.0;
            let cy = match (fx.anchor, fx.top) {
                (Anchor::Centre, _) => b.y as f32 + b.h as f32 / 2.0,
                (Anchor::Edge, false) => (b.y + b.h) as f32,
                (Anchor::Edge, true) => b.y as f32,
            };
            self.motion.SetCenterPoint(Vector3::new(cx, cy, 0.0))?;
        }
        // Without a record of what is on screen (new cell, invalidate) both are set.
        let was = old_fx.map(|o| o.motion);
        let m = fx.motion;
        if was.is_none_or(|w| w.dy != m.dy) {
            let to = Value::Vec3(Vector3::new(0.0, m.dy, 0.0));
            animate(comp, &v, h!("Offset"), to, motion_ms, true, animations)?;
        }
        if was.is_none_or(|w| w.scale != m.scale) {
            let to = Value::Vec3(Vector3::new(m.scale, m.scale, 1.0));
            animate(comp, &v, h!("Scale"), to, motion_ms, true, animations)?;
        }

        // Sweep: one pass of the streak when the hover starts (none without animations).
        let sweep_started = fx.sweep && !old_fx.is_some_and(|o| o.sweep);
        if sweep_started && animations && matches!(spec.content, Content::Icon { .. }) {
            if let Some(surface) = self.icon_surface.clone() {
                self.run_streak(comp, &surface, b, streak_ms)?;
            }
        }
        Ok(())
    }

    fn run_streak(
        &mut self,
        comp: &Compositor,
        surface: &CompositionDrawingSurface,
        b: PxRect,
        ms: u32,
    ) -> Result<()> {
        if self.streak.is_none() {
            let s = comp.CreateSpriteVisual()?;
            self.motion.Children()?.InsertAtTop(&s)?;
            self.streak = Some(s);
        }
        let Some(streak) = &self.streak else {
            return Ok(());
        };
        let (w, h) = (b.w as f32, b.h as f32);
        streak.SetOffset(Vector3::new(b.x as f32, b.y as f32, 0.0))?;
        streak.SetSize(Vector2::new(w, h))?;
        // CSS 115deg: towards the right and 25 degrees down, through the box centre.
        let grad = comp.CreateLinearGradientBrush()?;
        grad.SetMappingMode(CompositionMappingMode::Absolute)?;
        let (dx, dy) = (0.906 * w / 2.0, 0.423 * h / 2.0);
        grad.SetStartPoint(Vector2::new(w / 2.0 - dx, h / 2.0 - dy))?;
        grad.SetEndPoint(Vector2::new(w / 2.0 + dx, h / 2.0 + dy))?;
        let stops = grad.ColorStops()?;
        let clear = color(Rgba(STREAK.0, STREAK.1, STREAK.2, 0));
        stops.Append(&comp.CreateColorGradientStopWithOffsetAndColor(0.3, clear)?)?;
        stops.Append(&comp.CreateColorGradientStopWithOffsetAndColor(0.5, color(STREAK))?)?;
        stops.Append(&comp.CreateColorGradientStopWithOffsetAndColor(0.7, clear)?)?;
        // The streak lights only the icon's own pixels.
        let mask = comp.CreateMaskBrush()?;
        mask.SetSource(&grad)?;
        mask.SetMask(&comp.CreateSurfaceBrushWithSurface(surface)?)?;
        streak.SetBrush(&mask)?;
        // Ends past the right edge, where it shows nothing.
        let end = Vector2::new(1.2 * w, 0.0);
        grad.SetOffset(end)?;
        let anim = comp.CreateVector2KeyFrameAnimation()?;
        anim.InsertKeyFrame(0.0, Vector2::new(-1.2 * w, 0.0))?;
        anim.InsertKeyFrameWithEasingFunction(1.0, end, &ease(comp)?)?;
        anim.SetDuration(duration(ms))?;
        grad.StartAnimation(h!("Offset"), &anim)
    }

    /// Current transform target of the moving part (diagnostics and tests).
    fn motion(&self) -> Option<Motion> {
        self.shown.as_ref()?.fx.map(|f| f.motion)
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
        self.apply_fx(comp, old.as_ref(), &spec, animations)?;
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
                    self.icon_surface = Some(surface);
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
    chevron: Option<Cell>,
    tray: Vec<Option<Cell>>,
    quick: Option<Cell>,
    clock: Option<Cell>,
    tray_items: Vec<TrayItemState>,
    chevron_state: ChevronState,
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
            chevron: None,
            tray: Vec::new(),
            quick: None,
            clock: None,
            tray_items: Vec::new(),
            chevron_state: ChevronState::default(),
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

    /// Hover transform target of app item `i` (diagnostics and tests).
    pub fn app_motion(&self, i: usize) -> Option<Motion> {
        self.apps.get(i)?.as_ref()?.motion()
    }

    /// What the moving part of app item `i` holds right now as static values (not the
    /// animated ones): `(offset y, scale, hover plate opacity)`. Tests use it with
    /// animations off, where the final values are set directly.
    pub fn app_static_transform(&self, i: usize) -> Option<(f32, f32, f32)> {
        let cell = self.apps.get(i)?.as_ref()?;
        let plate = match &cell.hover_plate {
            Some(p) => p.visual.Opacity().ok()?,
            None => 0.0,
        };
        Some((
            cell.motion.Offset().ok()?.Y,
            cell.motion.Scale().ok()?.X,
            plate,
        ))
    }

    /// Takes effect on the next [`BarVisual::update`].
    pub fn set_env(&mut self, env: VisualEnv) {
        self.env = env;
    }

    /// The promoted tray icons (one per `layout.tray` slot) and the chevron; take effect on
    /// the next [`BarVisual::update`].
    pub fn set_tray(&mut self, items: Vec<TrayItemState>, chevron: ChevronState) {
        self.tray_items = items;
        self.chevron_state = chevron;
    }

    /// Number of tray icons currently in the tree (diagnostics and tests).
    pub fn tray_cells(&self) -> usize {
        self.tray.iter().flatten().count()
    }

    /// The chevron is in the tree (diagnostics and tests).
    pub fn chevron_shown(&self) -> bool {
        self.chevron.is_some()
    }

    /// Forgets what is on screen so the next update redraws every surface (after the GPU
    /// device was recreated, surface contents are lost).
    pub fn invalidate(&mut self) {
        self.backdrop = None;
        for cell in [
            &mut self.start,
            &mut self.more,
            &mut self.chevron,
            &mut self.quick,
            &mut self.clock,
        ]
        .into_iter()
        .chain(self.apps.iter_mut())
        .chain(self.tray.iter_mut())
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
        hover: &HoverStyle,
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
                    fx: None,
                }
            })
        };
        let start = glyph(&layout.start, GLYPH_START);
        let more = layout.more.as_ref().and_then(|r| glyph(r, GLYPH_MORE));
        let quick = glyph(&layout.quick, GLYPH_QUICK);
        let cs = self.chevron_state;
        let chevron_glyph = match self.env.position {
            Position::Bottom => GLYPH_CHEVRON_UP,
            Position::Top => GLYPH_CHEVRON_DOWN,
        };
        let chevron = cs
            .shown
            .then(|| glyph(&layout.overflow_chevron, chevron_glyph))
            .flatten()
            .map(|mut spec| {
                spec.plate = if cs.pressed {
                    rgba_alpha(text, PLATE_PRESSED_ALPHA)
                } else if cs.hovered || cs.open {
                    rgba_alpha(text, PLATE_HOVER_ALPHA)
                } else {
                    Rgba(0, 0, 0, 0)
                };
                spec
            });
        let tray_px = px(TRAY_ICON_DIP).max(1);
        let tray_specs: Vec<Option<CellSpec>> = layout
            .tray
            .iter()
            .zip(&self.tray_items)
            .map(|(r, it)| {
                owns(&surface, r).then(|| {
                    let rect = cell_rect(r);
                    let content = match &it.icon {
                        Some(img) if img.is_valid() && img.width > 0 => Content::Icon {
                            image: IconRef(Arc::clone(img)),
                            dy: 0,
                        },
                        _ => Content::Placeholder {
                            size: tray_px,
                            color: rgba_alpha(text, PLACEHOLDER_ALPHA),
                            dy: 0,
                            radius: tray_px / 4,
                        },
                    };
                    let plate = if it.pressed {
                        rgba_alpha(text, PLATE_PRESSED_ALPHA)
                    } else if it.hovered {
                        rgba_alpha(text, PLATE_HOVER_ALPHA)
                    } else {
                        Rgba(0, 0, 0, 0)
                    };
                    CellSpec {
                        rect,
                        content,
                        plate,
                        plate_inset,
                        plate_radius,
                        attention: false,
                        dots: None,
                        fx: None,
                    }
                })
            })
            .collect();
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
                fx: None,
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
        let hovered = items.iter().position(|it| it.hovered);
        let position = self.env.position;
        let app_specs: Vec<Option<CellSpec>> = layout
            .apps
            .iter()
            .zip(items)
            .enumerate()
            .map(|(i, (r, it))| {
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
                    // Hover feedback comes from the hover effect. With Plate, a press shows
                    // as the icon squeezing on the white plate instead of a darker plate.
                    let plate = if it.attention {
                        rgba_alpha(accent, PLATE_ATTENTION_ALPHA)
                    } else if it.pressed && !hover.plate {
                        rgba_alpha(text, PLATE_PRESSED_ALPHA)
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
                        fx: Some(hover_fx(hover, i, hovered, it.pressed, scale, position)),
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
        drawn += sync(&mut self.chevron, chevron, comp, painter, &parent, anim)?;
        while self.tray.len() > tray_specs.len() {
            if let Some(cell) = self.tray.pop().flatten() {
                cell.remove(&parent);
            }
        }
        self.tray.resize_with(tray_specs.len(), || None);
        for (slot, spec) in self.tray.iter_mut().zip(tray_specs) {
            drawn += sync(slot, spec, comp, painter, &parent, anim)?;
        }
        drawn += sync(&mut self.quick, quick, comp, painter, &parent, anim)?;
        drawn += sync(&mut self.clock, clock_spec, comp, painter, &parent, anim)?;
        self.surfaces_drawn += u64::from(drawn);
        Ok(())
    }
}
