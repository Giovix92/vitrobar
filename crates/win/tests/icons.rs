//! Icon loading and cache tests.

use std::sync::Once;
use std::time::{Duration, Instant};

use vitro_win::icons::{load_icon, IconCache, IconSource};
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

fn init_com() {
    // S_FALSE / RPC_E_CHANGED_MODE are fine: COM is available either way.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
}

unsafe extern "system" fn wndproc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(h, m, w, l) }
}

fn make_window() -> HWND {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        let hi = GetModuleHandleW(None).unwrap();
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hi.into(),
            lpszClassName: w!("VitroIconTestWindow"),
            ..Default::default()
        };
        assert_ne!(RegisterClassW(&wc), 0);
    });
    unsafe {
        let hi = GetModuleHandleW(None).unwrap();
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("VitroIconTestWindow"),
            w!("Vitro icon test"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            100,
            100,
            300,
            200,
            None,
            None,
            Some(hi.into()),
            None,
        )
        .unwrap()
    }
}

const NOTEPAD: &str = r"C:\Windows\System32\notepad.exe";

#[test]
fn notepad_icon_by_path() {
    init_com();
    let i = load_icon(IconSource::Path(NOTEPAD), 32).unwrap();
    assert_eq!((i.width, i.height), (32, 32));
    assert!(i.is_valid());
    assert!(i.pixels.chunks(4).any(|p| p[3] > 0));
}

#[test]
fn settings_icon_by_aumid() {
    init_com();
    assert!(load_icon(
        IconSource::Aumid(
            "windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft.windows.immersivecontrolpanel"
        ),
        48
    )
    .is_some());
}

#[test]
fn missing_path_is_none() {
    init_com();
    assert!(load_icon(IconSource::Path(r"C:\nope\x.exe"), 32).is_none());
}

#[test]
fn hung_window_icon_times_out() {
    init_com();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let h = make_window();
        tx.send(h.0 as usize).unwrap();
        // Never pumps messages: a plain SendMessage would block.
        std::thread::sleep(Duration::from_secs(5));
    });
    let h = HWND(rx.recv().unwrap() as *mut _);
    let t = Instant::now();
    let _ = load_icon(IconSource::Window(h), 32);
    let dt = t.elapsed();
    assert!(dt < Duration::from_millis(300), "took {dt:?}");
}

#[test]
fn tray_handle_icon_renders() {
    init_com();
    let hicon = unsafe { LoadIconW(None, IDI_APPLICATION).unwrap() };
    let i = load_icon(IconSource::TrayHandle(hicon.0 as usize as u32), 24).unwrap();
    assert_eq!((i.width, i.height), (24, 24));
    assert!(i.pixels.chunks(4).any(|p| p[3] > 0));
}

#[test]
fn cache_hits_and_skips_negatives() {
    init_com();
    let mut c = IconCache::new();
    assert!(c.get(IconSource::Path(r"C:\nope\x.exe"), 32).is_none());
    assert_eq!(c.len(), 0);
    assert!(c.get(IconSource::Path(NOTEPAD), 32).is_some());
    assert_eq!(c.len(), 1);
    assert!(c
        .get(IconSource::Path(&NOTEPAD.to_lowercase()), 32)
        .is_some());
    assert_eq!(c.len(), 1, "path keys are case-insensitive");
    assert!(c.get(IconSource::Path(NOTEPAD), 16).is_some());
    assert_eq!(c.len(), 2, "size is part of the key");
}
