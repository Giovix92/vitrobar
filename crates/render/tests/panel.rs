//! Activatable glass panel and text helpers against real DWM, with windows this test
//! creates itself.

use vitro_core::settings::Material;
use vitro_core::style::{material_style, Rgba, Theme};
use vitro_render::compositor::init_compositor;
use vitro_render::device::create_gpu_device;
use vitro_render::panel::{take_panel_deactivated, Panel, PANEL_CLASS};
use vitro_render::surfaces::{FontKind, Painter};
use windows::core::w;
use windows::Win32::Foundation::{HWND, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, GetClassNameW, GetForegroundWindow, GetWindowRect, IsWindow,
    SendMessageW, SetForegroundWindow, ShowWindow, SW_SHOW, WM_ACTIVATE, WS_EX_TOOLWINDOW,
    WS_POPUP,
};

/// A plain popup of our own (system STATIC class).
struct TestWindow(HWND);

impl TestWindow {
    fn new() -> Self {
        // SAFETY: system class and static strings; the window is destroyed in Drop.
        let h = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                w!("STATIC"),
                w!("vitrobar panel test"),
                WS_POPUP,
                50,
                50,
                200,
                100,
                None,
                None,
                None,
                None,
            )
            .unwrap()
        };
        // SAFETY: our own window.
        unsafe {
            let _ = ShowWindow(h, SW_SHOW);
        }
        Self(h)
    }
}

impl Drop for TestWindow {
    fn drop(&mut self) {
        // SAFETY: our own window.
        unsafe {
            let _ = DestroyWindow(self.0);
        }
    }
}

#[test]
fn panel_shows_at_rect_and_activates() {
    let comp = init_compositor().unwrap();
    let style = material_style(Material::Frosted, Theme::Dark, true);
    let rect = (200, 150, 700, 650);
    let panel = Panel::show(&comp, rect, 96, &style, 8.0, "Vitrobar panel test", false).unwrap();
    assert_eq!(panel.rect_px(), rect);
    assert_eq!(panel.scale(), 1.0);
    let h = panel.hwnd();
    // SAFETY: our own window; buffers are locals.
    unsafe {
        assert!(IsWindow(Some(h)).as_bool());
        let mut name = [0u16; 64];
        let n = GetClassNameW(h, &mut name) as usize;
        assert_eq!(String::from_utf16_lossy(&name[..n]), PANEL_CLASS);
        let mut r = Default::default();
        GetWindowRect(h, &mut r).unwrap();
        assert_eq!((r.left, r.top, r.right, r.bottom), rect);
    }
    let active = panel.activate();
    // Windows may deny foreground to a background test process; the window must exist
    // either way, and when the call says it succeeded it must really be foreground.
    if active {
        // SAFETY: plain query.
        assert_eq!(unsafe { GetForegroundWindow() }, h);
    } else {
        eprintln!("foreground denied in this environment; asserted existence only");
    }
}

#[test]
fn deactivation_is_reported() {
    let comp = init_compositor().unwrap();
    let style = material_style(Material::Frosted, Theme::Dark, true);
    let panel = Panel::show(&comp, (200, 150, 500, 450), 96, &style, 8.0, "p", false).unwrap();
    let other = TestWindow::new();
    let _ = take_panel_deactivated();
    if !panel.activate() {
        eprintln!("foreground denied; sending WM_ACTIVATE(WA_INACTIVE) to the panel instead");
        // SAFETY: our own window; the message is handled synchronously on this thread.
        unsafe {
            SendMessageW(panel.hwnd(), WM_ACTIVATE, Some(WPARAM(0)), None);
        }
        assert!(take_panel_deactivated().contains(&panel.hwnd()));
        return;
    }
    // SAFETY: our own window.
    unsafe {
        let _ = SetForegroundWindow(other.0);
    }
    assert!(take_panel_deactivated().contains(&panel.hwnd()));
    assert!(take_panel_deactivated().is_empty(), "drained");
}

#[test]
fn caret_x_monotonic() {
    let comp = init_compositor().unwrap();
    let gpu = create_gpu_device(&comp).unwrap();
    let p = Painter::new(&gpu).unwrap();
    let s = "Vitrobar";
    let xs: Vec<f32> = (0..=s.chars().count())
        .map(|i| p.caret_x(s, FontKind::Text, 16.0, i).unwrap())
        .collect();
    assert_eq!(xs[0], 0.0);
    assert!(xs.windows(2).all(|w| w[1] > w[0]), "{xs:?}");
    assert_eq!(p.caret_x("", FontKind::Text, 16.0, 0).unwrap(), 0.0);
    // A surrogate pair is one char: index 2 (after the emoji) is right of index 1.
    let e = "a\u{1F600}b";
    let a1 = p.caret_x(e, FontKind::Text, 16.0, 1).unwrap();
    let a2 = p.caret_x(e, FontKind::Text, 16.0, 2).unwrap();
    assert!(a2 > a1, "{a1} {a2}");
}

#[test]
fn text_left_draws() {
    let comp = init_compositor().unwrap();
    let gpu = create_gpu_device(&comp).unwrap();
    let p = Painter::new(&gpu).unwrap();
    p.set_locale("it-IT");
    let c = Rgba(255, 255, 255, 255);
    let surf = p
        .text_left("Cerca", FontKind::Text, 14.0, c, 240, 32)
        .unwrap();
    let sz = surf.SizeInt32().unwrap();
    assert_eq!((sz.Width, sz.Height), (240, 32));
    let long = "x".repeat(1000);
    p.text_left(&long, FontKind::Text, 14.0, c, 240, 32)
        .unwrap();
}

#[test]
fn text_wrapped_limits_lines() {
    let comp = init_compositor().unwrap();
    let gpu = create_gpu_device(&comp).unwrap();
    let p = Painter::new(&gpu).unwrap();
    let c = Rgba(255, 255, 255, 255);
    let text = "the quick brown fox jumps over the lazy dog ".repeat(10);
    let (_s1, h1) = p
        .text_wrapped("one line", FontKind::Text, 14.0, c, 300, 3)
        .unwrap();
    let (s3, h3) = p
        .text_wrapped(&text, FontKind::Text, 14.0, c, 300, 3)
        .unwrap();
    assert!(h1 > 0 && h3 > h1, "{h1} {h3}");
    assert!(h3 <= h1 * 3 + 3, "capped at 3 lines: {h1} {h3}");
    assert_eq!(s3.SizeInt32().unwrap().Height as u32, h3);
}
