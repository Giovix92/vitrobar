//! The glass panel view against real DWM: one row of each kind, hit testing, scrolling,
//! the Custom slot and in-place updates.

use vitro_core::panel_kit::{Button, Part, Row, Tile};
use vitro_core::settings::{Material, Position};
use vitro_core::style::{material_style, Rgba, Theme};
use vitro_render::compositor::init_compositor;
use vitro_render::device::create_gpu_device;
use vitro_render::panel_view::PanelView;
use vitro_render::surfaces::Painter;
use windows::Win32::UI::WindowsAndMessaging::{IsWindow, IsWindowVisible};

fn tile(id: u32, on: bool, more: bool) -> Tile {
    Tile {
        id,
        glyph: '\u{E701}',
        label: format!("Tile {id}"),
        on,
        more,
    }
}

fn rows(volume: f32, custom_h: f32) -> Vec<Row> {
    vec![
        Row::Header {
            text: "Quick".into(),
            trailing: Some("85 %".into()),
        },
        Row::Tiles(vec![tile(1, true, true), tile(2, false, false)]),
        Row::Slider {
            id: 10,
            glyph: '\u{E767}',
            value: volume,
            trailing: Some((11, '\u{E74F}')),
        },
        Row::Item {
            id: 20,
            glyph: Some('\u{E701}'),
            text: "Network".into(),
            detail: Some("Connected".into()),
            checked: true,
            busy: false,
        },
        Row::Separator,
        Row::Text("Some explanation".into()),
        Row::Custom {
            id: 30,
            h: custom_h,
        },
        Row::Buttons(vec![Button {
            id: 40,
            glyph: None,
            text: "Settings".into(),
        }]),
    ]
}

#[test]
fn panel_draws_every_row_kind_and_hit_tests() {
    let comp = init_compositor().unwrap();
    let gpu = create_gpu_device(&comp).unwrap();
    let painter = Painter::new(&gpu).unwrap();
    let style = material_style(Material::Frosted, Theme::Dark, true);
    let anchor = (500, 900, 540, 948);
    let mut view = PanelView::show(
        &comp,
        &painter,
        &style,
        96,
        anchor,
        Position::Bottom,
        Rgba(0, 0x78, 0xD4, 255),
        &rows(0.5, 60.0),
    )
    .unwrap();
    let (l, t, r, b) = view.rect_px();
    assert_eq!(r - l, 360);
    assert_eq!(b, 900 - 8, "8 DIP above the anchor");
    // SAFETY: plain queries on our own window.
    unsafe {
        assert!(IsWindowVisible(view.hwnd()).as_bool());
    }

    // Tile 1 is a `more` tile: its left part hits Tile, its right part TileMore.
    // Header 12..48, tiles 56..120.
    assert_eq!(view.hit_px((30, 80)), Some(Part::Tile(1)));
    assert_eq!(view.hit_px((12 + 112 - 10, 80)), Some(Part::TileMore(1)));
    assert_eq!(view.hit_px((12 + 112 + 8 + 50, 80)), Some(Part::Tile(2)));
    assert_eq!(view.hit_px((5, 80)), None);

    // The slider (128..168) maps a pointer to a value.
    assert_eq!(view.hit_px((100, 148)), Some(Part::Slider(10)));
    assert_eq!(view.slider_value(10, 0), Some(0.0));
    assert_eq!(view.slider_value(10, 10_000), Some(1.0));

    // The Custom slot: its rect, and a slot that survives an update that keeps the row.
    let rect = view.custom_rect_px(30).unwrap();
    assert_eq!(rect.2 - rect.0, 336);
    assert_eq!(rect.3 - rect.1, 60);
    let slot = view.custom_slot(30).unwrap();
    assert!(view.custom_slot(31).is_none());
    view.set_hot(Some(Part::Tile(2))).unwrap();
    view.set_pressed(Some(Part::Tile(2))).unwrap();
    view.set_focus(Some(Part::Item(20))).unwrap();
    view.set_hot(None).unwrap();
    view.set_pressed(None).unwrap();

    view.update(&comp, &painter, &rows(0.9, 60.0)).unwrap();
    assert!(view.custom_slot(30).unwrap() == slot, "slot is stable");
    assert_eq!(view.rect_px(), (l, t, r, b), "same size, same place");

    // A taller Custom row reshapes the window (it keeps its bottom edge above the anchor).
    view.update(&comp, &painter, &rows(0.9, 120.0)).unwrap();
    let (_, t2, _, b2) = view.rect_px();
    assert_eq!(b2, b);
    assert_eq!(t2, t - 60);

    let light = material_style(Material::Frosted, Theme::Light, true);
    view.restyle(
        &comp,
        &painter,
        &light,
        Rgba(200, 30, 90, 255),
        &rows(0.9, 120.0),
    )
    .unwrap();
    assert!(view.custom_slot(30).is_some());

    // No scrolling when everything fits.
    view.scroll_by(50.0).unwrap();
    assert_eq!(view.scroll(), 0.0);

    let h = view.hwnd();
    drop(view);
    // SAFETY: plain query.
    unsafe { assert!(!IsWindow(Some(h)).as_bool()) };
}

#[test]
fn tall_content_scrolls_and_hit_follows() {
    let comp = init_compositor().unwrap();
    let gpu = create_gpu_device(&comp).unwrap();
    let painter = Painter::new(&gpu).unwrap();
    let style = material_style(Material::Frosted, Theme::Dark, true);
    let items: Vec<Row> = (0..30)
        .map(|i| Row::Item {
            id: i,
            glyph: None,
            text: format!("Item {i}"),
            detail: None,
            checked: false,
            busy: i == 3,
        })
        .collect();
    let mut view = PanelView::show(
        &comp,
        &painter,
        &style,
        96,
        (500, 900, 540, 948),
        Position::Bottom,
        Rgba(0, 0x78, 0xD4, 255),
        &items,
    )
    .unwrap();
    let (_, t, _, b) = view.rect_px();
    assert_eq!(b - t, 640);
    assert_eq!(view.hit_px((100, 20)), Some(Part::Item(0)));
    view.scroll_by(104.0).unwrap();
    assert_eq!(view.scroll(), 104.0);
    assert_eq!(view.hit_px((100, 20)), Some(Part::Item(2)));
    // Clamped at both ends.
    view.scroll_by(1.0e6).unwrap();
    assert!(view.scroll() < 30.0 * 52.0);
    view.scroll_by(-1.0e6).unwrap();
    assert_eq!(view.scroll(), 0.0);
}
