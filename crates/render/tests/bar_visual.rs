//! BarVisual against a real compositor and GPU device (no window needed).

use std::sync::Arc;

use vitro_core::image::BgraImage;
use vitro_core::layout::{compute_layout, BarLayout, LayoutInput, Rect};
use vitro_core::settings::{HoverEffect, IconSize, Material, Position, Shape};
use vitro_core::style::{hover_style, material_style, Rgba, Theme};
use vitro_render::bar_visual::{
    hover_timing, BarFocus, BarVisual, ChevronState, ItemState, TrayItemState, VisualEnv,
    WidgetPill,
};
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
        tray_area: true,
        widgets: [0.0; 2],
        ime: 0.0,
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
        ..Default::default()
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

fn icons(n: usize) -> Vec<ItemState> {
    let img = Arc::new(icon(24));
    (0..n)
        .map(|_| ItemState {
            icon: Some(Arc::clone(&img)),
            dots: 1,
            ..Default::default()
        })
        .collect()
}

/// (offset y, scale) targets of every app item.
fn motions(bar: &BarVisual, n: usize) -> Vec<(f32, f32)> {
    (0..n)
        .map(|i| {
            let m = bar.app_motion(i).unwrap();
            (m.dy, m.scale)
        })
        .collect()
}

#[test]
fn hover_effects_set_their_targets_without_redrawing() {
    let comp = init_compositor().unwrap();
    let gpu = create_gpu_device(&comp).unwrap();
    let style = material_style(Material::Frosted, Theme::Dark, true);
    let l = layout(Shape::Pill, 4);
    let clock = "1:00
1/1/2027";
    let mut bar = BarVisual::new(&comp, &gpu, 0).unwrap();
    // 150 % scaling, animations off: final values are set at once and can be read back.
    bar.set_env(VisualEnv {
        dpi: 144,
        animations: false,
        ..Default::default()
    });
    let mut items = icons(4);

    let magnify = hover_style(HoverEffect::Magnify);
    bar.update(&l, &items, clock, &style, &magnify).unwrap();
    let drawn = bar.surfaces_drawn();
    assert_eq!(motions(&bar, 4), vec![(0.0, 1.0); 4]);
    items[1].hovered = true;
    bar.update(&l, &items, clock, &style, &magnify).unwrap();
    assert_eq!(
        motions(&bar, 4),
        vec![(0.0, 1.22), (0.0, 1.55), (0.0, 1.22), (0.0, 1.0)]
    );
    assert_eq!(bar.app_static_transform(1), Some((0.0, 1.55, 0.0)));
    assert_eq!(bar.app_static_transform(3), Some((0.0, 1.0, 0.0)));

    let lift = hover_style(HoverEffect::Lift);
    bar.update(&l, &items, clock, &style, &lift).unwrap();
    // -5 DIP at 150 % = -7.5 px, snapped to whole pixels.
    assert_eq!(motions(&bar, 4)[1], (-8.0, 1.0));
    assert_eq!(
        motions(&bar, 4)[0],
        (0.0, 1.0),
        "Lift has no neighbour effect"
    );
    assert_eq!(bar.app_static_transform(1), Some((-8.0, 1.0, 0.0)));

    let sweep = hover_style(HoverEffect::Sweep);
    bar.update(&l, &items, clock, &style, &sweep).unwrap();
    assert_eq!(motions(&bar, 4)[1], (-3.0, 1.06));

    let plate = hover_style(HoverEffect::Plate);
    bar.update(&l, &items, clock, &style, &plate).unwrap();
    assert_eq!(motions(&bar, 4)[1], (0.0, 1.0));
    assert_eq!(
        bar.app_static_transform(1),
        Some((0.0, 1.0, 1.0)),
        "plate shown"
    );
    items[1].pressed = true;
    bar.update(&l, &items, clock, &style, &plate).unwrap();
    assert_eq!(motions(&bar, 4)[1], (0.0, 0.9), "pressed icon squeezes");
    items[1].pressed = false;
    items[1].hovered = false;
    bar.update(&l, &items, clock, &style, &plate).unwrap();
    assert_eq!(
        bar.app_static_transform(1),
        Some((0.0, 1.0, 0.0)),
        "plate gone"
    );

    assert_eq!(
        bar.surfaces_drawn(),
        drawn,
        "hover effects are transforms, never redraws"
    );
}

#[test]
fn top_bars_mirror_lift() {
    let comp = init_compositor().unwrap();
    let gpu = create_gpu_device(&comp).unwrap();
    let style = material_style(Material::Frosted, Theme::Light, true);
    let l = layout(Shape::Pill, 2);
    let mut bar = BarVisual::new(&comp, &gpu, 0).unwrap();
    bar.set_env(VisualEnv {
        position: Position::Top,
        ..Default::default()
    });
    let mut items = icons(2);
    items[0].hovered = true;
    bar.update(&l, &items, "1:00", &style, &hover_style(HoverEffect::Lift))
        .unwrap();
    assert_eq!(motions(&bar, 2)[0], (5.0, 1.0));
    // With animations on, a hover change still succeeds (the compositor runs it).
    items[0].hovered = false;
    items[1].hovered = true;
    bar.update(&l, &items, "1:00", &style, &hover_style(HoverEffect::Sweep))
        .unwrap();
    assert_eq!(motions(&bar, 2), vec![(0.0, 1.0), (2.0, 1.06)]);
}

#[test]
fn motion_never_exceeds_250_ms() {
    for h in vitro_core::style::ALL_HOVERS {
        let (motion, fade, streak) = hover_timing(&hover_style(*h));
        assert!(motion <= 250 && fade <= 250, "{h:?}");
        let expected = match h {
            HoverEffect::Plate => (150, 150, 150),
            HoverEffect::Lift => (220, 220, 220),
            HoverEffect::Magnify => (180, 180, 180),
            HoverEffect::Sweep => (220, 250, 450),
        };
        assert_eq!((motion, fade, streak), expected, "{h:?}");
    }
}

#[test]
fn tray_icons_and_chevron_draw_once() {
    let comp = init_compositor().unwrap();
    let gpu = create_gpu_device(&comp).unwrap();
    let mut bar = BarVisual::new(&comp, &gpu, 0).unwrap();
    bar.set_env(VisualEnv {
        animations: false,
        ..Default::default()
    });
    let style = material_style(Material::Frosted, Theme::Dark, true);
    let hover = hover_style(HoverEffect::Plate);
    let mut input = LayoutInput {
        monitor: Rect {
            x: 0.0,
            y: 0.0,
            w: 1536.0,
            h: 864.0,
        },
        shape: Shape::Pill,
        position: Position::Bottom,
        icon_size: IconSize::Medium,
        app_count: 0,
        tray_count: 2,
        clock_w: 72.0,
        reserve_floating: true,
        tray_area: true,
        widgets: [0.0; 2],
        ime: 0.0,
    };
    let l = compute_layout(&input);
    let img = Arc::new(icon(16));
    let tray = vec![
        TrayItemState {
            icon: Some(Arc::clone(&img)),
            ..Default::default()
        },
        TrayItemState::default(), // placeholder
    ];
    let chevron = ChevronState {
        shown: true,
        ..Default::default()
    };
    bar.set_tray(tray.clone(), chevron);
    bar.update(&l, &[], "12:34", &style, &hover).unwrap();
    assert_eq!(bar.tray_cells(), 2);
    assert!(bar.chevron_shown());
    // Start, quick, clock, chevron glyph and one tray icon.
    assert_eq!(bar.surfaces_drawn(), 5);
    let mut hovered = tray.clone();
    hovered[0].hovered = true;
    bar.set_tray(
        hovered,
        ChevronState {
            open: true,
            ..chevron
        },
    );
    bar.update(&l, &[], "12:34", &style, &hover).unwrap();
    assert_eq!(bar.surfaces_drawn(), 5, "hover plates are shapes");

    // No overflow: no chevron; one icon fewer.
    input.tray_count = 1;
    let l = compute_layout(&input);
    bar.set_tray(tray[..1].to_vec(), ChevronState::default());
    bar.update(&l, &[], "12:34", &style, &hover).unwrap();
    assert_eq!(bar.tray_cells(), 1);
    assert!(!bar.chevron_shown());
}

#[test]
fn bar_without_tray_area_draws_start_and_clock_only() {
    let comp = init_compositor().unwrap();
    let gpu = create_gpu_device(&comp).unwrap();
    let mut bar = BarVisual::new(&comp, &gpu, 0).unwrap();
    bar.set_env(VisualEnv {
        animations: false,
        ..Default::default()
    });
    let style = material_style(Material::Frosted, Theme::Dark, true);
    let hover = hover_style(HoverEffect::Plate);
    let l = compute_layout(&LayoutInput {
        monitor: Rect {
            x: 1536.0,
            y: 0.0,
            w: 1536.0,
            h: 864.0,
        },
        shape: Shape::Pill,
        position: Position::Bottom,
        icon_size: IconSize::Medium,
        app_count: 0,
        tray_count: 3,
        clock_w: 72.0,
        reserve_floating: true,
        tray_area: false,
        widgets: [0.0; 2],
        ime: 0.0,
    });
    // Even if the app handed tray items over, a bar without the tray area shows none.
    bar.set_tray(vec![TrayItemState::default(); 3], ChevronState::default());
    bar.update(&l, &[], "12:34", &style, &hover).unwrap();
    assert_eq!(bar.tray_cells(), 0);
    assert!(!bar.chevron_shown());
    // Start and clock: no quick-settings glyph.
    assert_eq!(bar.surfaces_drawn(), 2);
}

#[test]
fn a_dpi_change_redraws_every_cell_at_the_new_scale() {
    let comp = init_compositor().unwrap();
    let gpu = create_gpu_device(&comp).unwrap();
    let mut bar = BarVisual::new(&comp, &gpu, 0).unwrap();
    let env = VisualEnv {
        animations: false,
        ..Default::default()
    };
    bar.set_env(env);
    let style = material_style(Material::Frosted, Theme::Dark, true);
    let hover = hover_style(HoverEffect::Plate);
    let l = layout(Shape::Pill, 0);
    bar.update(&l, &[], "12:34", &style, &hover).unwrap();
    let first = bar.surfaces_drawn();
    assert_eq!(first, 3, "start, quick, clock");
    bar.update(&l, &[], "12:34", &style, &hover).unwrap();
    assert_eq!(bar.surfaces_drawn(), first, "nothing changed");
    // 150 %: same layout in DIPs, every cell is redrawn at 1.5x.
    bar.set_env(VisualEnv { dpi: 144, ..env });
    bar.update(&l, &[], "12:34", &style, &hover).unwrap();
    assert_eq!(bar.surfaces_drawn(), 2 * first);
}

#[test]
fn focus_ring_follows_the_focused_item_without_redrawing() {
    let comp = init_compositor().unwrap();
    let gpu = create_gpu_device(&comp).unwrap();
    let mut bar = BarVisual::new(&comp, &gpu, 0).unwrap();
    bar.set_env(VisualEnv {
        animations: false,
        dpi: 144,
        ..Default::default()
    });
    let style = material_style(Material::Frosted, Theme::Dark, true);
    let hover = hover_style(HoverEffect::Plate);
    let mut input = LayoutInput {
        monitor: Rect {
            x: 0.0,
            y: 0.0,
            w: 1536.0,
            h: 864.0,
        },
        shape: Shape::Pill,
        position: Position::Bottom,
        icon_size: IconSize::Medium,
        app_count: 3,
        tray_count: 1,
        clock_w: 72.0,
        reserve_floating: true,
        tray_area: true,
        widgets: [0.0; 2],
        ime: 0.0,
    };
    let l = compute_layout(&input);
    let items = icons(3);
    let chevron = ChevronState {
        shown: true,
        ..Default::default()
    };
    bar.set_tray(vec![TrayItemState::default()], chevron);
    bar.update(&l, &items, "12:34", &style, &hover).unwrap();
    assert_eq!(bar.focus_ring(), None);
    let drawn = bar.surfaces_drawn();

    let scale = 1.5;
    let surface = l.surfaces[0];
    let inside = |ring: (i32, i32, i32, i32), r: Rect| {
        let (ox, oy) = (surface.x * scale, surface.y * scale);
        let (l, t) = (r.x * scale - ox, r.y * scale - oy);
        let (rr, b) = (r.right() * scale - ox, r.bottom() * scale - oy);
        ring.0 as f32 >= l - 1.0
            && ring.1 as f32 >= t - 1.0
            && ring.2 as f32 <= rr + 1.0
            && ring.3 as f32 <= b + 1.0
            && ring.2 > ring.0
            && ring.3 > ring.1
    };
    for (focus, rect) in [
        (BarFocus::App(1), l.apps[1]),
        (BarFocus::App(2), l.apps[2]),
        (BarFocus::Tray(0), l.tray[0]),
        (BarFocus::Chevron, l.overflow_chevron),
    ] {
        bar.set_focus(Some(focus));
        bar.update(&l, &items, "12:34", &style, &hover).unwrap();
        let ring = bar.focus_ring().unwrap_or_else(|| panic!("{focus:?}"));
        assert!(inside(ring, rect), "{focus:?}: {ring:?} vs {rect:?}");
    }
    assert_eq!(
        bar.surfaces_drawn(),
        drawn,
        "the ring is a shape: nothing redrawn"
    );

    // An item that is not there shows no ring.
    bar.set_focus(Some(BarFocus::App(9)));
    bar.update(&l, &items, "12:34", &style, &hover).unwrap();
    assert_eq!(bar.focus_ring(), None);
    bar.set_focus(Some(BarFocus::App(0)));
    bar.update(&l, &items, "12:34", &style, &hover).unwrap();
    assert!(bar.focus_ring().is_some());
    bar.set_focus(None);
    bar.update(&l, &items, "12:34", &style, &hover).unwrap();
    assert_eq!(bar.focus_ring(), None);

    // No chevron (empty overflow): focusing it shows nothing.
    input.tray_count = 1;
    let l = compute_layout(&input);
    bar.set_tray(vec![TrayItemState::default()], ChevronState::default());
    bar.set_focus(Some(BarFocus::Chevron));
    bar.update(&l, &items, "12:34", &style, &hover).unwrap();
    assert_eq!(bar.focus_ring(), None);
}

#[test]
fn widget_pills_draw_only_what_the_app_supplies() {
    let comp = init_compositor().unwrap();
    let gpu = create_gpu_device(&comp).unwrap();
    let mut bar = BarVisual::new(&comp, &gpu, 0).unwrap();
    bar.set_env(VisualEnv {
        animations: false,
        ..Default::default()
    });
    let style = material_style(Material::Frosted, Theme::Dark, true);
    let hover = hover_style(HoverEffect::Plate);
    let mut l = compute_layout(&LayoutInput {
        widgets: [120.0, 90.0],
        ime: 0.0,
        ..input_for(Shape::Pill)
    });
    // Slots without content draw nothing: the bar looks as before.
    bar.update(&l, &[], "12:34", &style, &hover).unwrap();
    assert_eq!(bar.widget_cells(), 0);
    let base = bar.surfaces_drawn();
    let pill = |t: &str| {
        Some(WidgetPill {
            glyph: '\u{E8D6}',
            text: t.to_string(),
        })
    };
    bar.set_widgets([pill("Song - Artist"), None], None);
    bar.update(&l, &[], "12:34", &style, &hover).unwrap();
    assert_eq!(bar.widget_cells(), 1);
    assert_eq!(bar.surfaces_drawn(), base + 1);
    // Hover changes the plate, not the text surface.
    bar.set_widgets([pill("Song - Artist"), None], Some(0));
    bar.update(&l, &[], "12:34", &style, &hover).unwrap();
    assert_eq!(bar.surfaces_drawn(), base + 1);
    // A pill without a layout rect (widget hidden) draws nothing.
    l.widgets = [None, None];
    bar.update(&l, &[], "12:34", &style, &hover).unwrap();
    assert_eq!(bar.widget_cells(), 0);
}

fn input_for(shape: Shape) -> LayoutInput {
    LayoutInput {
        monitor: Rect {
            x: 0.0,
            y: 0.0,
            w: 1536.0,
            h: 864.0,
        },
        shape,
        position: Position::Bottom,
        icon_size: IconSize::Medium,
        app_count: 0,
        tray_count: 0,
        clock_w: 72.0,
        reserve_floating: true,
        tray_area: true,
        widgets: [0.0; 2],
        ime: 0.0,
    }
}

#[test]
fn the_ime_indicator_draws_its_text_when_given_a_rect() {
    let comp = init_compositor().unwrap();
    let gpu = create_gpu_device(&comp).unwrap();
    let mut bar = BarVisual::new(&comp, &gpu, 0).unwrap();
    bar.set_env(VisualEnv {
        animations: false,
        ..Default::default()
    });
    let style = material_style(Material::Frosted, Theme::Dark, true);
    let hover = hover_style(HoverEffect::Plate);
    let mut l = compute_layout(&LayoutInput {
        ime: 32.0,
        ..input_for(Shape::Pill)
    });
    bar.update(&l, &[], "12:34", &style, &hover).unwrap();
    assert!(!bar.ime_shown());
    let base = bar.surfaces_drawn();
    bar.set_ime(Some("ITA".into()), false);
    bar.update(&l, &[], "12:34", &style, &hover).unwrap();
    assert!(bar.ime_shown());
    assert_eq!(bar.surfaces_drawn(), base + 1);
    // Hover is a plate, not a new text surface.
    bar.set_ime(Some("ITA".into()), true);
    bar.update(&l, &[], "12:34", &style, &hover).unwrap();
    assert_eq!(bar.surfaces_drawn(), base + 1);
    l.ime = None;
    bar.update(&l, &[], "12:34", &style, &hover).unwrap();
    assert!(!bar.ime_shown());
}
