//! BarVisual against a real compositor and GPU device (no window needed).

use std::sync::Arc;

use vitro_core::image::BgraImage;
use vitro_core::layout::{compute_layout, BarLayout, LayoutInput, Rect};
use vitro_core::settings::{HoverEffect, IconSize, Material, Position, Shape};
use vitro_core::style::{hover_style, material_style, Rgba, Theme};
use vitro_render::bar_visual::{BarVisual, ItemState, VisualEnv};
use vitro_render::compositor::init_compositor;
use vitro_render::device::create_gpu_device;
use vitro_render::surfaces::{FontKind, Painter};

fn layout(shape: Shape, apps: usize) -> BarLayout {
    compute_layout(&LayoutInput {
        monitor: Rect {
            x: 0.0,
            y: 0.0,
            w: 1536.0,
            h: 864.0,
        },
        shape,
        position: Position::Bottom,
        icon_size: IconSize::Medium,
        app_count: apps,
        tray_count: 0,
        clock_w: 72.0,
        reserve_floating: true,
    })
}

fn icon(px: u32) -> BgraImage {
    let mut img = BgraImage::new(px, px);
    for p in img.pixels.chunks_mut(4) {
        p.copy_from_slice(&[200, 100, 50, 255]);
    }
    img
}

#[test]
fn measures_text_and_glyphs() {
    let comp = init_compositor().unwrap();
    let gpu = create_gpu_device(&comp).unwrap();
    let p = Painter::new(&gpu).unwrap();
    let (w1, h1) = p.measure("12:34", FontKind::Text, 12.0).unwrap();
    let (w2, h2) = p
        .measure("12:34\n30/10/2026", FontKind::Text, 12.0)
        .unwrap();
    assert!(w1 > 10.0 && w1 < 60.0, "{w1}");
    assert!(w2 > w1 && h2 > h1 * 1.5, "{w2}x{h2} vs {w1}x{h1}");
    let (gw, _) = p.measure("\u{E700}", FontKind::Glyph, 16.0).unwrap();
    assert!(gw > 0.0);
    println!("glyph font: {}", p.glyph_family());
}

#[test]
fn update_draws_only_what_changed() {
    let comp = init_compositor().unwrap();
    let gpu = create_gpu_device(&comp).unwrap();
    let mut bar = BarVisual::new(&comp, &gpu, 0).unwrap();
    bar.set_env(VisualEnv {
        dpi: 120,
        accent: Rgba(0, 120, 212, 255),
        animations: false,
    });
    let style = material_style(Material::Frosted, Theme::Dark, true);
    let hover = hover_style(HoverEffect::Plate);
    let l = layout(Shape::Pill, 2);
    let mut items = vec![
        ItemState {
            icon: Some(Arc::new(icon(30))),
            dots: 2,
            active: true,
            ..Default::default()
        },
        ItemState {
            icon: None, // placeholder: a shape, no surface
            dots: 1,
            attention: true,
            ..Default::default()
        },
    ];
    bar.update(&l, &items, "12:34\n30/10/2026", &style, &hover)
        .unwrap();
    // Start + quick glyphs, the clock text and one icon.
    assert_eq!(bar.surfaces_drawn(), 4);

    bar.update(&l, &items, "12:34\n30/10/2026", &style, &hover)
        .unwrap();
    assert_eq!(bar.surfaces_drawn(), 4, "identical input must draw nothing");

    bar.update(&l, &items, "12:35\n30/10/2026", &style, &hover)
        .unwrap();
    assert_eq!(bar.surfaces_drawn(), 5, "only the clock is redrawn");

    items[0].hovered = true;
    items[1].dots = 3;
    bar.update(&l, &items, "12:35\n30/10/2026", &style, &hover)
        .unwrap();
    assert_eq!(
        bar.surfaces_drawn(),
        5,
        "plate and dots are shapes, not surfaces"
    );

    items[1].icon = Some(Arc::new(icon(30)));
    bar.update(&l, &items, "12:35\n30/10/2026", &style, &hover)
        .unwrap();
    assert_eq!(bar.surfaces_drawn(), 6, "the arriving icon is drawn once");

    // Same pixels in a new Arc count as a new icon (identity, not pixel comparison) ...
    items[1].icon = Some(Arc::new(icon(30)));
    bar.update(&l, &items, "12:35\n30/10/2026", &style, &hover)
        .unwrap();
    assert_eq!(bar.surfaces_drawn(), 7);
    // ... and the same Arc again is unchanged.
    bar.update(&l, &items, "12:35\n30/10/2026", &style, &hover)
        .unwrap();
    assert_eq!(bar.surfaces_drawn(), 7);

    bar.invalidate();
    bar.update(&l, &items, "12:35\n30/10/2026", &style, &hover)
        .unwrap();
    assert_eq!(
        bar.surfaces_drawn(),
        12,
        "after invalidate everything is redrawn"
    );
    assert_eq!(bar.app_cells(), 2);

    // Fewer apps: the extra cell is removed, the rest only moves (no surface redrawn).
    let l1 = layout(Shape::Pill, 1);
    bar.update(&l1, &items[..1], "12:35\n30/10/2026", &style, &hover)
        .unwrap();
    assert_eq!(bar.app_cells(), 1, "the second app's cell is removed");
    assert_eq!(bar.surfaces_drawn(), 12, "moving cells redraws no surface");
}

#[test]
fn islands_split_items_between_surfaces() {
    let comp = init_compositor().unwrap();
    let gpu = create_gpu_device(&comp).unwrap();
    let style = material_style(Material::Smoked, Theme::Light, true);
    let hover = hover_style(HoverEffect::Lift);
    let l = layout(Shape::Islands, 3);
    let items = vec![ItemState::default(); 3];
    let mut drawn = Vec::new();
    for i in 0..3 {
        let mut bar = BarVisual::new(&comp, &gpu, i).unwrap();
        bar.update(&l, &items, "1:00\n1/1/2027", &style, &hover)
            .unwrap();
        drawn.push(bar.surfaces_drawn());
    }
    // Start glyph | three placeholders (no surfaces) | quick glyph + clock.
    assert_eq!(drawn, vec![1, 0, 2]);
    // A surface index beyond the layout hides itself instead of failing.
    let mut extra = BarVisual::new(&comp, &gpu, 5).unwrap();
    extra
        .update(&l, &items, "1:00\n1/1/2027", &style, &hover)
        .unwrap();
    assert_eq!(extra.surfaces_drawn(), 0);
}
