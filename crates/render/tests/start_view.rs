//! The Start panel's retained view against real DWM, on a panel this test creates itself
//! (never activated).

use std::sync::Arc;

use vitro_core::image::BgraImage;
use vitro_core::settings::Material;
use vitro_core::style::{material_style, Rgba, Theme};
use vitro_render::compositor::init_compositor;
use vitro_render::device::create_gpu_device;
use vitro_render::panel::Panel;
use vitro_render::start_view::{Item, Layer, Part, StartView};
use vitro_render::surfaces::{FontKind, Painter};

fn text(s: &str, rect: (i32, i32, i32, i32)) -> Item {
    Item {
        rect,
        part: Part::Text {
            text: s.into(),
            font: FontKind::Text,
            size_px: 14.0,
            color: Rgba(255, 255, 255, 255),
            centred: false,
        },
    }
}

fn plate(rect: (i32, i32, i32, i32)) -> Item {
    Item {
        rect,
        part: Part::Plate {
            color: Rgba(255, 255, 255, 30),
            radius_px: 6.0,
        },
    }
}

#[test]
fn layers_reuse_visuals_and_skip_empty_boxes() {
    let comp = init_compositor().unwrap();
    let gpu = create_gpu_device(&comp).unwrap();
    let painter = Painter::new(&gpu).unwrap();
    let style = material_style(Material::Frosted, Theme::Dark, true);
    let panel = Panel::show(
        &comp,
        (-2000, -2000, -1600, -1500),
        96,
        &style,
        12.0,
        "Vitrobar start view test",
        false,
    )
    .unwrap();
    let mut view = StartView::new(&panel).unwrap();

    // The zero-size "Recent" title of a short panel is skipped (carry 3).
    let items = vec![
        plate((10, 10, 200, 50)),
        text("Pinned", (10, 10, 200, 50)),
        text("Recent", (10, 60, 10, 60)),
        text("", (10, 100, 200, 140)),
    ];
    view.set_layer(Layer::Chrome, &painter, items).unwrap();
    assert_eq!(view.visual_count(Layer::Chrome), 2);

    // Moving the plate keeps the same number of visuals.
    let items = vec![plate((10, 60, 200, 100)), text("Pinned", (10, 10, 200, 50))];
    view.set_layer(Layer::Chrome, &painter, items).unwrap();
    assert_eq!(view.visual_count(Layer::Chrome), 2);

    // Icons and their placeholders; the list scrolls inside its viewport.
    let img = Arc::new(BgraImage {
        width: 32,
        height: 32,
        pixels: vec![255; 32 * 32 * 4],
    });
    let rows: Vec<Item> = (0..40)
        .flat_map(|i| {
            let y = i * 44;
            [
                Item {
                    rect: (0, y, 44, y + 44),
                    part: Part::Image {
                        image: (i % 2 == 0).then(|| Arc::clone(&img)),
                        placeholder: Rgba(255, 255, 255, 51),
                    },
                },
                text(&format!("App {i}"), (52, y, 300, y + 44)),
            ]
        })
        .collect();
    view.set_viewport(Some((0, 80, 380, 480))).unwrap();
    view.set_layer(Layer::List, &painter, rows).unwrap();
    assert_eq!(view.visual_count(Layer::List), 80);
    view.set_scroll(300.0).unwrap();
    view.set_layer(Layer::List, &painter, Vec::new()).unwrap();
    assert_eq!(view.visual_count(Layer::List), 0);
    view.set_viewport(None).unwrap();
    drop(view);
    drop(panel);
}
