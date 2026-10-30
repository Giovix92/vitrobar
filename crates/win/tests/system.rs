//! Smoke tests against the real system (any Windows 11 machine has a monitor).

use vitro_win::monitors::list_monitors;
use vitro_win::system_theme::read_system_theme;

#[test]
fn exactly_one_primary_monitor_listed_first() {
    let mons = list_monitors();
    assert!(!mons.is_empty());
    assert!(mons[0].primary);
    assert_eq!(mons.iter().filter(|m| m.primary).count(), 1);
    for m in &mons {
        let (l, t, r, b) = m.rect_px;
        assert!(r > l && b > t, "{m:?}");
        assert!(m.dpi >= 96, "{m:?}");
    }
}

#[test]
fn system_theme_reads() {
    let t = read_system_theme();
    assert!(t.accent <= 0xFF_FFFF);
}

mod fullscreen {
    use vitro_win::appbar::{covers_as_fullscreen, foreground_info, Foreground};
    use vitro_win::monitors::list_monitors;
    use windows::core::w;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, WINDOW_EX_STYLE, WINDOW_STYLE, WS_MAXIMIZE,
        WS_OVERLAPPEDWINDOW, WS_POPUP,
    };

    /// A never-shown window of the built-in STATIC class (nothing appears on screen).
    fn hidden(style: WINDOW_STYLE, r: (i32, i32, i32, i32)) -> HWND {
        // SAFETY: built-in class, static strings; destroyed by the caller.
        unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!("vitro-fullscreen-test"),
                style,
                r.0,
                r.1,
                r.2 - r.0,
                r.3 - r.1,
                None,
                None,
                None,
                None,
            )
        }
        .unwrap()
    }

    fn as_foreign(f: Foreground) -> Foreground {
        // Our own windows never count; pretend it belongs to another app to test the rest.
        Foreground { own: false, ..f }
    }

    #[test]
    fn real_windows_read_as_fullscreen_or_not() {
        let mon = list_monitors()[0].rect_px;
        let full = hidden(WS_POPUP, mon);
        let maxed = hidden(WS_OVERLAPPEDWINDOW | WS_MAXIMIZE, mon);
        let small = hidden(WS_POPUP, (mon.0, mon.1, mon.0 + 300, mon.1 + 200));
        let f = foreground_info(full).unwrap();
        assert!(f.own && !f.maximized_with_caption, "{f:?}");
        assert!(!covers_as_fullscreen(&f, mon), "own windows never count");
        assert!(covers_as_fullscreen(&as_foreign(f), mon));
        let m = foreground_info(maxed).unwrap();
        assert!(m.maximized_with_caption, "{m:?}");
        assert!(!covers_as_fullscreen(&as_foreign(m), mon));
        let s = foreground_info(small).unwrap();
        assert!(!covers_as_fullscreen(&as_foreign(s), mon));
        // SAFETY: destroying the windows created above.
        unsafe {
            for h in [full, maxed, small] {
                let _ = DestroyWindow(h);
            }
        }
    }
}
