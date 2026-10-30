//! Bar backdrop: frosted (host backdrop + tint) or solid, rounded, with a 1 px border and
//! an inner top highlight. Sizes are in the target window's pixels.

use vitro_core::style::{MaterialStyle, Rgba};
use windows::core::{Interface, Result};
use windows::UI::Color;
use windows::UI::Composition::{CompositionBrush, Compositor, ContainerVisual, ShapeVisual};
use windows_numerics::Vector2;

pub(crate) fn color(c: Rgba) -> Color {
    Color {
        A: c.3,
        R: c.0,
        G: c.1,
        B: c.2,
    }
}

/// 1 px stroke along a rounded rectangle inset by `inset` px (0.5 puts it on the
/// outermost pixel row, crisp and fully inside the clip).
fn stroke_shape(
    comp: &Compositor,
    size: Vector2,
    radius: f32,
    inset: f32,
    brush: &CompositionBrush,
) -> Result<ShapeVisual> {
    let geom = comp.CreateRoundedRectangleGeometry()?;
    geom.SetOffset(Vector2::new(inset, inset))?;
    geom.SetSize(Vector2::new(
        (size.X - 2.0 * inset).max(0.0),
        (size.Y - 2.0 * inset).max(0.0),
    ))?;
    let r = (radius - inset).max(0.0);
    geom.SetCornerRadius(Vector2::new(r, r))?;
    let shape = comp.CreateSpriteShapeWithGeometry(&geom)?;
    shape.SetStrokeBrush(brush)?;
    shape.SetStrokeThickness(1.0)?;
    let visual = comp.CreateShapeVisual()?;
    visual.SetSize(size)?;
    visual.Shapes()?.Append(&shape)?;
    Ok(visual)
}

/// `comp` may be a `&Comp` too (it derefs to the compositor).
pub fn backdrop_visual(
    comp: &Compositor,
    style: &MaterialStyle,
    size: Vector2,
    radius: f32,
) -> Result<ContainerVisual> {
    let radius = radius.min(size.X / 2.0).min(size.Y / 2.0).max(0.0);

    let root = comp.CreateContainerVisual()?;
    root.SetSize(size)?;
    let clip_geom = comp.CreateRoundedRectangleGeometry()?;
    clip_geom.SetSize(size)?;
    clip_geom.SetCornerRadius(Vector2::new(radius, radius))?;
    root.SetClip(&comp.CreateGeometricClipWithGeometry(&clip_geom)?)?;
    let children = root.Children()?;

    if style.backdrop_blur {
        let backdrop = comp.CreateSpriteVisual()?;
        backdrop.SetSize(size)?;
        backdrop.SetBrush(&comp.CreateHostBackdropBrush()?)?;
        children.InsertAtTop(&backdrop)?;
    }

    let tint = comp.CreateSpriteVisual()?;
    tint.SetSize(size)?;
    tint.SetBrush(&comp.CreateColorBrushWithColor(color(style.tint))?)?;
    children.InsertAtTop(&tint)?;

    if style.highlight.3 > 0 {
        // Inner highlight: 1 px line just inside the border, bright at the top, gone by mid-height.
        let grad = comp.CreateLinearGradientBrush()?;
        grad.SetStartPoint(Vector2::new(0.0, 0.0))?;
        grad.SetEndPoint(Vector2::new(0.0, 1.0))?;
        let stops = grad.ColorStops()?;
        let h = style.highlight;
        stops.Append(&comp.CreateColorGradientStopWithOffsetAndColor(0.0, color(h))?)?;
        let clear = color(Rgba(h.0, h.1, h.2, 0));
        stops.Append(&comp.CreateColorGradientStopWithOffsetAndColor(0.5, clear)?)?;
        children.InsertAtTop(&stroke_shape(comp, size, radius, 1.5, &grad.cast()?)?)?;
    }

    if style.border.3 > 0 {
        let brush = comp.CreateColorBrushWithColor(color(style.border))?;
        children.InsertAtTop(&stroke_shape(comp, size, radius, 0.5, &brush.cast()?)?)?;
    }

    Ok(root)
}
