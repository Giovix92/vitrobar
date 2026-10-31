//! Flyout + thumbnail strip against real DWM, with a window this test creates itself.

use vitro_core::settings::{Material, Position};
use vitro_core::style::{material_style, Rgba, Theme};
use vitro_render::compositor::init_compositor;
use vitro_render::device::create_gpu_device;
use vitro_render::flyout::{live_thumbnails, Flyout, ThumbnailStrip, FLYOUT_CLASS};
use vitro_render::surfaces::Painter;
use windows::core::w;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, GetClassNameW, IsWindow, IsWindowVisible, ShowWindow,
    SW_SHOWNOACTIVATE, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
};

/// A plain 320 x 200 popup of our own (system STATIC class), shown without activation.
struct TestWindow(HWND);

impl TestWindow {
    fn new(x: i32) -> Self {
        // SAFETY: system class and static strings; the window is destroyed in Drop.
        let h = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                w!("STATIC"),
                w!("vitrobar flyout test"),
                WS_POPUP,
                x,
                100,
                320,
                200,
                None,
                None,
                None,
                None,
            )
            .unwrap()
        };
        // SAFETY: our own window.
        unsafe {
            let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
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
fn strip_registers_live_thumbnails_and_hit_tests() {
    let comp = init_compositor().unwrap();
    let gpu = create_gpu_device(&comp).unwrap();
    let painter = Painter::new(&gpu).unwrap();
    let style = material_style(Material::Frosted, Theme::Dark, true);
    let a = TestWindow::new(60);
    let b = TestWindow::new(420);
    let windows = vec![
        (
            a.0,
            "first test window with a title far too long to fit".to_string(),
        ),
        (b.0, "second".to_string()),
    ];
    let raw: Vec<HWND> = windows.iter().map(|(h, _)| *h).collect();
    let size = ThumbnailStrip::size_dip(&raw);
    // 320 x 200 = 1.6: two 200 x 120 boxes (DPI 96).
    assert_eq!(size, (428.0, 166.0));

    let anchor = (500, 900, 540, 948);
    let flyout = Flyout::show(&comp, anchor, Position::Bottom, size, 96, &style).unwrap();
    let (l, t, r, bottom) = flyout.rect_px();
    assert_eq!((r - l, bottom - t), (428, 166));
    assert_eq!(bottom, 900 - 8, "8 DIP above the anchor");
    // SAFETY: plain queries.
    unsafe {
        // Our own window, not whatever FindWindow would match (a running Vitrobar has one).
        let mut class = [0u16; 64];
        let n = GetClassNameW(flyout.hwnd(), &mut class) as usize;
        assert_eq!(
            String::from_utf16_lossy(&class[..n]),
            FLYOUT_CLASS.to_string().unwrap()
        );
        assert!(IsWindowVisible(flyout.hwnd()).as_bool());
    }

    let before = live_thumbnails();
    let strip =
        ThumbnailStrip::show(&flyout, &painter, Rgba(255, 255, 255, 255), &windows).unwrap();
    assert_eq!(live_thumbnails(), before + 2);
    let thumbs = strip.thumbnails();
    assert_eq!(thumbs.len(), 2);
    for (k, (h, r)) in thumbs.iter().enumerate() {
        assert_eq!(*h, windows[k].0);
        // Aspect kept (1.6) inside the 200 x 120 box: 192 x 120.
        assert_eq!((r.2 - r.0, r.3 - r.1), (192, 120), "{r:?}");
        let centre = ((r.0 + r.2) / 2, (r.1 + r.3) / 2);
        assert_eq!(strip.hit(centre), Some(*h));
    }
    // The title row belongs to its thumbnail; the padding and the gap belong to none.
    assert_eq!(strip.hit((20, 145)), Some(a.0));
    assert_eq!(strip.hit((2, 2)), None);
    assert_eq!(strip.hit((214, 60)), None);
    // The close button sits at the right end of the title row (x 190..210, y 136..156).
    assert_eq!(strip.hit_close((200, 146)), Some(a.0));
    assert_eq!(strip.hit_close((408, 146)), Some(b.0));
    assert_eq!(strip.hit_close((20, 146)), None);
    assert_eq!(strip.hit_close((200, 60)), None);

    drop(strip);
    assert_eq!(
        live_thumbnails(),
        before,
        "dropping the strip unregisters them"
    );
    let h = flyout.hwnd();
    drop(flyout);
    // SAFETY: plain query.
    unsafe { assert!(!IsWindow(Some(h)).as_bool()) };
}
