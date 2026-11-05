//! The settings window's view against real DWM, on an off-screen panel this test creates
//! itself (never activated).

use vitro_core::i18n::Lang;
use vitro_core::settings::{Material, Settings};
use vitro_core::settings_ui::build_model;
use vitro_core::style::{material_style, Rgba, Theme};
use vitro_render::compositor::init_compositor;
use vitro_render::device::create_gpu_device;
use vitro_render::panel::Panel;
use vitro_render::settings_view::{
    Interaction, SettingsLook, SettingsView, Spot, WINDOW_H_DIP, WINDOW_W_DIP,
};
use vitro_render::surfaces::Painter;

#[test]
fn renders_the_page_and_redraws_only_changes() {
    let comp = init_compositor().unwrap();
    let gpu = create_gpu_device(&comp).unwrap();
    let painter = Painter::new(&gpu).unwrap();
    let style = material_style(Material::Frosted, Theme::Dark, true);
    let size = (WINDOW_W_DIP as i32, WINDOW_H_DIP as i32);
    let panel = Panel::show(
        &comp,
        (-3000, -3000, -3000 + size.0, -3000 + size.1),
        96,
        &style,
        8.0,
        "Vitrobar settings view test",
        false,
    )
    .unwrap();
    let mut view = SettingsView::new(&panel).unwrap();
    let model = build_model(&Settings::default(), WINDOW_W_DIP);
    let look = SettingsLook {
        style: &style,
        accent: Rgba(0, 0x78, 0xD4, 255),
        system_accent: Rgba(0, 0x78, 0xD4, 255),
        lang: Lang::It,
        title: "Impostazioni di Vitrobar",
        version: "0.1.0",
        hint: Some((11, "Si applica al prossimo avvio di Vitrobar")),
        page: vitro_core::settings_ui::Section::Appearance,
    };
    let calm = Interaction::default();
    view.render(&painter, &model, &look, &calm, 0.0, size, 1.0)
        .unwrap();
    let n = view.visual_count();
    assert!(n > 60, "{n} visuals");
    // Same input: nothing changes; scrolling moves a container only.
    view.render(&painter, &model, &look, &calm, 120.0, size, 1.0)
        .unwrap();
    assert_eq!(view.visual_count(), n);
    // Hovering the close button adds its red plate.
    let hot = Interaction {
        hover: Some(Spot::Close),
        ..calm
    };
    view.render(&painter, &model, &look, &hot, 120.0, size, 1.0)
        .unwrap();
    assert_eq!(view.visual_count(), n + 1);
    drop(view);
    drop(panel);
}
