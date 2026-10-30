//! Window tracking tests against real windows created in the test process.

use std::sync::Once;
use std::time::{Duration, Instant};

use vitro_core::windows_model::is_taskbar_window;
use vitro_win::desktops::Desktops;
use vitro_win::window_props::read_window_info;
use vitro_win::window_tracker::{
    shell_hook_message_id, take_window_event, translate_shell_hook, WindowEvent, WindowTracker,
    WM_VITRO_WINDOW_EVENT,
};
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

unsafe extern "system" fn wndproc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(h, m, w, l) }
}

fn register_class() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        let hi = GetModuleHandleW(None).unwrap();
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hi.into(),
            lpszClassName: w!("VitroTestWindow"),
            ..Default::default()
        };
        assert_ne!(RegisterClassW(&wc), 0);
    });
}

fn make_window(style: WINDOW_STYLE, ex: WINDOW_EX_STYLE, owner: Option<HWND>) -> HWND {
    register_class();
    unsafe {
        let hi = GetModuleHandleW(None).unwrap();
        CreateWindowExW(
            ex,
            w!("VitroTestWindow"),
            w!("Vitro test window"),
            style,
            100,
            100,
            300,
            200,
            owner,
            None,
            Some(hi.into()),
            None,
        )
        .unwrap()
    }
}

fn hwnd_u64(h: HWND) -> u64 {
    h.0 as usize as u64
}

#[test]
fn normal_window_is_taskbar_window() {
    let h = make_window(WS_OVERLAPPEDWINDOW | WS_VISIBLE, WINDOW_EX_STYLE(0), None);
    let i = read_window_info(h, 1, &Desktops::new().unwrap()).unwrap();
    assert!(is_taskbar_window(&i), "{i:?}");
    assert_eq!(i.hwnd, hwnd_u64(h));
    assert_eq!(i.seq, 1);
    assert_eq!(i.title, "Vitro test window");
    assert!(i.exe_path.is_some());
    assert!(i.on_current_desktop);
    assert!(!i.minimized);
    let mons = vitro_win::monitors::list_monitors();
    assert!(mons.iter().any(|m| m.id == i.monitor), "{i:?} vs {mons:?}");
    unsafe { DestroyWindow(h).unwrap() };
}

#[test]
fn tool_window_is_not() {
    let h = make_window(WS_POPUP | WS_VISIBLE, WS_EX_TOOLWINDOW, None);
    let i = read_window_info(h, 1, &Desktops::new().unwrap()).unwrap();
    assert!(i.tool_window);
    assert!(!is_taskbar_window(&i));
    unsafe { DestroyWindow(h).unwrap() };
}

#[test]
fn owned_window_is_not() {
    let owner = make_window(WS_OVERLAPPEDWINDOW | WS_VISIBLE, WINDOW_EX_STYLE(0), None);
    let h = make_window(WS_POPUP | WS_VISIBLE, WINDOW_EX_STYLE(0), Some(owner));
    let i = read_window_info(h, 1, &Desktops::new().unwrap()).unwrap();
    assert!(i.has_owner);
    assert!(!is_taskbar_window(&i));
    unsafe {
        DestroyWindow(h).unwrap();
        DestroyWindow(owner).unwrap();
    }
}

#[test]
fn hung_window_does_not_block() {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let h = make_window(WS_OVERLAPPEDWINDOW | WS_VISIBLE, WINDOW_EX_STYLE(0), None);
        tx.send(hwnd_u64(h)).unwrap();
        // Never pumps messages: any SendMessage to this window would block.
        std::thread::sleep(Duration::from_secs(5));
    });
    let h = HWND(rx.recv().unwrap() as usize as *mut _);
    let desktops = Desktops::new().unwrap();
    let t = Instant::now();
    let i = read_window_info(h, 1, &desktops);
    let dt = t.elapsed();
    assert!(dt < Duration::from_millis(300), "took {dt:?}");
    assert_eq!(i.unwrap().title, "Vitro test window");
}

#[test]
fn tracker_reports_create_and_destroy() {
    register_class();
    let notify = make_window(WS_POPUP, WINDOW_EX_STYLE(0), None); // hidden, never on the taskbar
    let tracker = WindowTracker::start(notify).unwrap();
    assert!(
        WindowTracker::start(notify).is_err(),
        "second tracker must fail"
    );
    assert!(!tracker.enumerate().is_empty());

    let target = make_window(WS_OVERLAPPEDWINDOW | WS_VISIBLE, WINDOW_EX_STYLE(0), None);
    unsafe {
        let _ = ShowWindow(target, SW_SHOWNORMAL);
    }
    let mut destroyed_at: Option<Instant> = None;
    let mut events = Vec::new();
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(1500) {
        if destroyed_at.is_none() && start.elapsed() > Duration::from_millis(500) {
            unsafe { DestroyWindow(target).unwrap() };
            destroyed_at = Some(Instant::now());
        }
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == shell_hook_message_id() {
                    if let Some(ev) = translate_shell_hook(msg.wParam, msg.lParam) {
                        events.push(ev);
                    }
                } else if msg.message == WM_VITRO_WINDOW_EVENT {
                    events.push(take_window_event(msg.lParam));
                } else if msg.message == WM_TIMER {
                    tracker.on_timer(msg.wParam.0);
                } else {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let id = hwnd_u64(target);
    let c = events.iter().position(|e| *e == WindowEvent::Created(id));
    let d = events.iter().position(|e| *e == WindowEvent::Destroyed(id));
    assert!(c.is_some(), "no Created in {events:?}");
    assert!(d.is_some(), "no Destroyed in {events:?}");
    assert!(c < d);
    drop(tracker);
    unsafe { DestroyWindow(notify).unwrap() };
}

/// Regression: the shell assigns a new window to a virtual desktop asynchronously
/// (30-60 ms), and during that gap `IsWindowOnCurrentVirtualDesktop` says `false`. Reading
/// a window right after creation must still say "current desktop".
#[test]
fn fresh_window_counts_as_on_current_desktop() {
    let d = Desktops::new().unwrap();
    // Like the app, learn the current desktop from an already settled window first.
    let settled = make_window(WS_OVERLAPPEDWINDOW | WS_VISIBLE, WINDOW_EX_STYLE(0), None);
    let t = Instant::now();
    while !d.is_on_current(settled) && t.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(5));
    }
    for _ in 0..40 {
        let h = make_window(WS_OVERLAPPEDWINDOW | WS_VISIBLE, WINDOW_EX_STYLE(0), None);
        assert!(d.is_on_current(h));
        unsafe { DestroyWindow(h).unwrap() };
    }
    unsafe { DestroyWindow(settled).unwrap() };
}
