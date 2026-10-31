//! The Start panel's content: a retained set of visuals in three layers on a [`Panel`].
//!
//! The app describes each layer as a flat list of [`Item`]s (text lines, icons, rounded
//! plates) in panel client px; [`StartView::set_layer`] turns it into visuals, reusing every
//! visual whose content and size did not change (a text surface is drawn once, then moved
//! around), so hover, selection and caret moves and the arrival of icons cost no text
//! drawing. The list layer sits in a clipped viewport and scrolls by moving its container
//! ([`StartView::set_scroll`]): no surface is redrawn while scrolling.
//!
//! Text surfaces are only as wide as their text (at most the item's box), which keeps the
//! panel's memory small. Items with an empty rectangle are skipped.

use std::collections::HashMap;
use std::sync::Arc;

use vitro_core::image::BgraImage;
use vitro_core::style::Rgba;
use windows::core::{Interface, Result};
use windows::UI::Composition::{Compositor, ContainerVisual, Visual};
use windows_numerics::{Vector2, Vector3};

use crate::flyout::PxRect;
use crate::material::color;
use crate::panel::Panel;
use crate::surfaces::{FontKind, Painter};

/// What one item shows.
#[derive(Clone, Debug, PartialEq)]
pub enum Part {
    /// One line of text, vertically centred in the box, cut with an ellipsis when too long;
    /// `centred` centres it horizontally.
    Text {
        text: String,
        font: FontKind,
        size_px: f32,
        color: Rgba,
        centred: bool,
    },
    /// An icon at its own pixel size, centred in the box; `None` (not loaded yet) draws a
    /// rounded placeholder square of `placeholder` the size of the box's shorter side.
    Image {
        image: Option<Arc<BgraImage>>,
        placeholder: Rgba,
    },
    /// A filled rounded rectangle covering the box.
    Plate { color: Rgba, radius_px: f32 },
    /// Text word-wrapped to the box's width in at most `max_lines` lines (the last ends in an
    /// ellipsis when cut), lines centred, placed at the box's top.
    Wrapped {
        text: String,
        font: FontKind,
        size_px: f32,
        color: Rgba,
        max_lines: u32,
    },
    /// The outline of a rounded rectangle covering the box (a focus ring).
    Outline {
        color: Rgba,
        radius_px: f32,
        thickness_px: f32,
    },
}

/// One thing to draw, in the layer's own px coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub rect: PxRect,
    pub part: Part,
}

/// The view's layers, bottom to top.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layer {
    /// Search box, titles, footer: panel client px.
    Chrome,
    /// The current mode's content (home or results): panel client px.
    Page,
    /// The All-apps list: content px inside the viewport ([`StartView::set_viewport`]).
    List,
}

/// Identity of a reusable visual: equal keys draw the same pixels.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum CacheKey {
    Text {
        text: String,
        font: FontKind,
        size: u32,
        color: Rgba,
        w: i32,
        h: i32,
    },
    Image(usize),
    Placeholder {
        color: Rgba,
        side: i32,
    },
    Plate {
        color: Rgba,
        radius: u32,
        w: i32,
        h: i32,
    },
    Wrapped {
        text: String,
        font: FontKind,
        size: u32,
        color: Rgba,
        w: i32,
        lines: u32,
    },
    Outline {
        color: Rgba,
        radius: u32,
        thickness: u32,
        w: i32,
        h: i32,
    },
}

struct Cached {
    visual: Visual,
    /// The visual's size (its offset inside the item box is derived from it).
    size: (i32, i32),
    /// Keeps the image alive while its address is part of the key.
    _image: Option<Arc<BgraImage>>,
}

#[derive(Default)]
struct LayerState {
    pool: HashMap<CacheKey, Vec<Cached>>,
    items: Vec<Item>,
}

/// The visuals of an open Start panel. Dropping it removes them from the panel.
pub struct StartView {
    comp: Compositor,
    parent: ContainerVisual,
    root: ContainerVisual,
    chrome: ContainerVisual,
    page: ContainerVisual,
    viewport: ContainerVisual,
    list: ContainerVisual,
    states: [LayerState; 3],
}

fn index(l: Layer) -> usize {
    match l {
        Layer::Chrome => 0,
        Layer::Page => 1,
        Layer::List => 2,
    }
}

fn size_of_rect(r: PxRect) -> (i32, i32) {
    ((r.2 - r.0).max(0), (r.3 - r.1).max(0))
}

impl StartView {
    /// Adds the (empty) layers on top of `panel`'s glass.
    pub fn new(panel: &Panel) -> Result<Self> {
        let parent = panel.root().clone();
        let comp = parent.Compositor()?;
        let root = comp.CreateContainerVisual()?;
        let chrome = comp.CreateContainerVisual()?;
        let page = comp.CreateContainerVisual()?;
        let viewport = comp.CreateContainerVisual()?;
        let list = comp.CreateContainerVisual()?;
        // An inset clip with no insets clips to the viewport's own size.
        viewport.SetClip(&comp.CreateInsetClip()?)?;
        viewport.SetIsVisible(false)?;
        viewport.Children()?.InsertAtTop(&list)?;
        let children = root.Children()?;
        children.InsertAtTop(&page)?;
        children.InsertAtTop(&viewport)?;
        children.InsertAtTop(&chrome)?;
        parent.Children()?.InsertAtTop(&root)?;
        Ok(Self {
            comp,
            parent,
            root,
            chrome,
            page,
            viewport,
            list,
            states: Default::default(),
        })
    }

    fn container(&self, l: Layer) -> &ContainerVisual {
        match l {
            Layer::Chrome => &self.chrome,
            Layer::Page => &self.page,
            Layer::List => &self.list,
        }
    }

    /// Shows `items` in layer `l` (bottom to top in list order). Visuals whose content did
    /// not change are reused; the rest are drawn now. Unchanged input does nothing.
    pub fn set_layer(&mut self, l: Layer, painter: &Painter, items: Vec<Item>) -> Result<()> {
        let k = index(l);
        if self.states[k].items == items {
            return Ok(());
        }
        let mut old = std::mem::take(&mut self.states[k].pool);
        let mut pool: HashMap<CacheKey, Vec<Cached>> = HashMap::new();
        let mut placed: Vec<Visual> = Vec::with_capacity(items.len());
        for item in &items {
            let (w, h) = size_of_rect(item.rect);
            if w == 0 || h == 0 {
                continue;
            }
            let Some(key) = self.key_for(painter, item)? else {
                continue;
            };
            let cached = match old.get_mut(&key).and_then(Vec::pop) {
                Some(c) => c,
                None => self.build(painter, &key, item)?,
            };
            let (cw, ch) = cached.size;
            let (x, y) = match &item.part {
                Part::Text { centred: false, .. } => (item.rect.0, item.rect.1),
                Part::Wrapped { .. } => (item.rect.0 + (w - cw) / 2, item.rect.1),
                _ => (item.rect.0 + (w - cw) / 2, item.rect.1 + (h - ch) / 2),
            };
            cached
                .visual
                .SetOffset(Vector3::new(x as f32, y as f32, 0.0))?;
            placed.push(cached.visual.clone());
            pool.entry(key).or_default().push(cached);
        }
        let children = self.container(l).Children()?;
        children.RemoveAll()?;
        for v in &placed {
            children.InsertAtTop(v)?;
        }
        // Whatever was not reused goes now (its surfaces with it).
        drop(old);
        self.states[k] = LayerState { pool, items };
        Ok(())
    }

    fn key_for(&self, painter: &Painter, item: &Item) -> Result<Option<CacheKey>> {
        let (w, h) = size_of_rect(item.rect);
        Ok(Some(match &item.part {
            Part::Text {
                text,
                font,
                size_px,
                color,
                ..
            } => {
                if text.is_empty() {
                    return Ok(None);
                }
                // As wide as the text, at most the box (then cut with an ellipsis).
                let (tw, _) = painter.measure(text, *font, *size_px)?;
                CacheKey::Text {
                    text: text.clone(),
                    font: *font,
                    size: size_px.to_bits(),
                    color: *color,
                    w: ((tw.ceil() as i32) + 2).min(w).max(1),
                    h,
                }
            }
            Part::Image {
                image: Some(img), ..
            } if img.is_valid() && img.width > 0 && img.height > 0 => {
                CacheKey::Image(Arc::as_ptr(img) as usize)
            }
            Part::Image { placeholder, .. } => CacheKey::Placeholder {
                color: *placeholder,
                side: w.min(h),
            },
            Part::Plate { color, radius_px } => CacheKey::Plate {
                color: *color,
                radius: radius_px.to_bits(),
                w,
                h,
            },
            Part::Wrapped {
                text,
                font,
                size_px,
                color,
                max_lines,
            } => {
                if text.is_empty() {
                    return Ok(None);
                }
                CacheKey::Wrapped {
                    text: text.clone(),
                    font: *font,
                    size: size_px.to_bits(),
                    color: *color,
                    w,
                    lines: *max_lines,
                }
            }
            Part::Outline {
                color,
                radius_px,
                thickness_px,
            } => CacheKey::Outline {
                color: *color,
                radius: radius_px.to_bits(),
                thickness: thickness_px.to_bits(),
                w,
                h,
            },
        }))
    }

    fn build(&self, painter: &Painter, key: &CacheKey, item: &Item) -> Result<Cached> {
        let c = &self.comp;
        match (key, &item.part) {
            (
                CacheKey::Text {
                    text,
                    font,
                    color,
                    w,
                    h,
                    ..
                },
                Part::Text { size_px, .. },
            ) => {
                let surface =
                    painter.text_left(text, *font, *size_px, *color, *w as u32, *h as u32)?;
                let sprite = c.CreateSpriteVisual()?;
                sprite.SetBrush(&c.CreateSurfaceBrushWithSurface(&surface)?)?;
                sprite.SetSize(Vector2::new(*w as f32, *h as f32))?;
                Ok(Cached {
                    visual: sprite.cast()?,
                    size: (*w, *h),
                    _image: None,
                })
            }
            (
                CacheKey::Image(_),
                Part::Image {
                    image: Some(img), ..
                },
            ) => {
                let surface = painter.image(img)?;
                let sprite = c.CreateSpriteVisual()?;
                sprite.SetBrush(&c.CreateSurfaceBrushWithSurface(&surface)?)?;
                let (w, h) = (img.width as i32, img.height as i32);
                sprite.SetSize(Vector2::new(w as f32, h as f32))?;
                Ok(Cached {
                    visual: sprite.cast()?,
                    size: (w, h),
                    _image: Some(Arc::clone(img)),
                })
            }
            (CacheKey::Placeholder { color, side }, _) => {
                let v = rounded(c, (*side, *side), *side as f32 / 4.0, *color)?;
                Ok(Cached {
                    visual: v,
                    size: (*side, *side),
                    _image: None,
                })
            }
            (CacheKey::Plate { color, w, h, .. }, Part::Plate { radius_px, .. }) => {
                let v = rounded(c, (*w, *h), *radius_px, *color)?;
                Ok(Cached {
                    visual: v,
                    size: (*w, *h),
                    _image: None,
                })
            }
            (
                CacheKey::Wrapped {
                    text,
                    font,
                    color,
                    w,
                    lines,
                    ..
                },
                Part::Wrapped { size_px, .. },
            ) => {
                let (surface, h) =
                    painter.text_wrapped(text, *font, *size_px, *color, *w as u32, *lines, true)?;
                let sprite = c.CreateSpriteVisual()?;
                sprite.SetBrush(&c.CreateSurfaceBrushWithSurface(&surface)?)?;
                sprite.SetSize(Vector2::new(*w as f32, h as f32))?;
                Ok(Cached {
                    visual: sprite.cast()?,
                    size: (*w, h as i32),
                    _image: None,
                })
            }
            (
                CacheKey::Outline { color, w, h, .. },
                Part::Outline {
                    radius_px,
                    thickness_px,
                    ..
                },
            ) => {
                let v = outline(c, (*w, *h), *radius_px, *thickness_px, *color)?;
                Ok(Cached {
                    visual: v,
                    size: (*w, *h),
                    _image: None,
                })
            }
            _ => Err(windows::core::Error::from(
                windows::Win32::Foundation::E_UNEXPECTED,
            )),
        }
    }

    /// Places the List layer's clipped viewport (panel client px); `None` hides it.
    pub fn set_viewport(&mut self, rect: Option<PxRect>) -> Result<()> {
        match rect {
            Some(r) => {
                let (w, h) = size_of_rect(r);
                self.viewport
                    .SetOffset(Vector3::new(r.0 as f32, r.1 as f32, 0.0))?;
                self.viewport.SetSize(Vector2::new(w as f32, h as f32))?;
                self.viewport.SetIsVisible(true)
            }
            None => self.viewport.SetIsVisible(false),
        }
    }

    /// Scrolls the List layer: content px `y` is at the viewport's top.
    pub fn set_scroll(&mut self, y: f32) -> Result<()> {
        self.list.SetOffset(Vector3::new(0.0, -y.round(), 0.0))
    }

    /// Visuals currently in layer `l` (for tests and diagnostics).
    pub fn visual_count(&self, l: Layer) -> usize {
        self.states[index(l)].pool.values().map(Vec::len).sum()
    }
}

impl Drop for StartView {
    fn drop(&mut self) {
        if let Ok(children) = self.parent.Children() {
            let _ = children.Remove(&self.root);
        }
    }
}

/// A rounded rectangle shape of `size` px.
fn rounded(c: &Compositor, size: (i32, i32), radius: f32, fill: Rgba) -> Result<Visual> {
    let geom = c.CreateRoundedRectangleGeometry()?;
    let sz = Vector2::new(size.0 as f32, size.1 as f32);
    geom.SetSize(sz)?;
    geom.SetCornerRadius(Vector2::new(radius, radius))?;
    let brush = c.CreateColorBrushWithColor(color(fill))?;
    let shape = c.CreateSpriteShapeWithGeometry(&geom)?;
    shape.SetFillBrush(&brush)?;
    let visual = c.CreateShapeVisual()?;
    visual.Shapes()?.Append(&shape)?;
    visual.SetSize(sz)?;
    visual.cast()
}

/// The outline of a rounded rectangle of `size` px, `thickness` wide, inside that size.
fn outline(
    c: &Compositor,
    size: (i32, i32),
    radius: f32,
    thickness: f32,
    stroke: Rgba,
) -> Result<Visual> {
    let geom = c.CreateRoundedRectangleGeometry()?;
    geom.SetSize(Vector2::new(
        size.0 as f32 - thickness,
        size.1 as f32 - thickness,
    ))?;
    geom.SetOffset(Vector2::new(thickness / 2.0, thickness / 2.0))?;
    geom.SetCornerRadius(Vector2::new(radius, radius))?;
    let shape = c.CreateSpriteShapeWithGeometry(&geom)?;
    shape.SetStrokeBrush(&c.CreateColorBrushWithColor(color(stroke))?)?;
    shape.SetStrokeThickness(thickness)?;
    let visual = c.CreateShapeVisual()?;
    visual.Shapes()?.Append(&shape)?;
    visual.SetSize(Vector2::new(size.0 as f32, size.1 as f32))?;
    visual.cast()
}
