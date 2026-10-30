//! The tray host against real `Shell_NotifyIconW` calls from a window this test creates.
//!
//! It takes over the `Shell_TrayWnd` name for the whole desktop while it runs (every app's
//! tray calls pass through it and on to Explorer), so it is ignored by default:
//!
//! `cargo test -p vitro-win --test tray -- --ignored --test-threads=1`
//!
//! Clicks are forwarded only to the test's own window. When the host is dropped, its window
//! is destroyed and `TaskbarCreated` is broadcast, so every app re-registers with Explorer.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use vitro_core::tray_wire::{TrayMessage, TrayOp};
use vitro_win::stock_taskbar::{autohide_now, explorer_tray_window};
use vitro_win::tray_host::{
    take_tray_message, tray_identity_key, TrayClick, TrayHost, NIN_SELECT, WM_VITRO_TRAY,
};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_SETVERSION,
    NOTIFYICONDATAW, NOTIFYICONDATAW_0, NOTIFYICON_VERSION_4,
};
use windows::Win32::UI::WindowsAndMessaging::*;

const CALLBACK: u32 = WM_APP + 0x55;

/// Callback messages the owner window received: (wParam, lParam).
static RECEIVED: Mutex<Vec<(usize, isize)>> = Mutex::new(Vec::new());

unsafe extern "system" fn owner_proc(h: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if m == CALLBACK {
        RECEIVED.lock().unwrap().push((wp.0, lp.0));
        return LRESULT(0);
    }
    // SAFETY: default handling.
    unsafe { DefWindowProcW(h, m, wp, lp) }
}

fn register() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(owner_proc),
            hInstance: GetModuleHandleW(None).unwrap().into(),
            lpszClassName: w!("VitroTrayTestOwner"),
            ..Default::default()
        };
        assert_ne!(RegisterClassW(&wc), 0);
    });
}

/// A hidden top-level window of ours (the icon's owner, and the "notify" receiver).
struct Owned(HWND);
impl Owned {
    fn new() -> Self {
        register();
        // SAFETY: our class; never shown.
        Self(unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                w!("VitroTrayTestOwner"),
                w!("vitro tray test"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(GetModuleHandleW(None).unwrap().into()),
                None,
            )
            .unwrap()
        })
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: our own window.
        unsafe {
            let _ = DestroyWindow(self.0);
        }
    }
}

fn nid(owner: HWND, uid: u32, tip: &str) -> NOTIFYICONDATAW {
    let mut n = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: owner,
        uID: uid,
        uFlags: NIF_MESSAGE | NIF_TIP | NIF_ICON,
        uCallbackMessage: CALLBACK,
        // SAFETY: a shared system icon.
        hIcon: unsafe { LoadIconW(None, IDI_APPLICATION) }.unwrap(),
        ..Default::default()
    };
    for (i, u) in tip.encode_utf16().take(127).enumerate() {
        n.szTip[i] = u;
    }
    n
}

/// Pumps this thread's queue until a `WM_VITRO_TRAY` on `to` satisfies `want`.
fn wait_tray(to: HWND, want: impl Fn(&TrayMessage) -> bool) -> Option<TrayMessage> {
    let end = Instant::now() + Duration::from_secs(1);
    let mut msg = MSG::default();
    while Instant::now() < end {
        // SAFETY: standard pump of our own thread; tray messages are reclaimed once.
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_VITRO_TRAY && msg.hwnd == to {
                    let m = take_tray_message(msg.lParam);
                    if want(&m) {
                        return Some(m);
                    }
                } else {
                    DispatchMessageW(&msg);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    None
}

fn pump_for(ms: u64) {
    let end = Instant::now() + Duration::from_millis(ms);
    let mut msg = MSG::default();
    while Instant::now() < end {
        // SAFETY: standard pump; any stray tray message is reclaimed.
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_VITRO_TRAY {
                    let _ = take_tray_message(msg.lParam);
                } else {
                    DispatchMessageW(&msg);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Windows of class Shell_TrayWnd that belong to this process.
fn own_tray_windows() -> usize {
    let mut n = 0;
    let mut after = None;
    // SAFETY: plain queries.
    unsafe {
        let me = GetCurrentProcessId();
        while let Ok(h) = FindWindowExW(None, after, w!("Shell_TrayWnd"), PCWSTR::null()) {
            if h.0.is_null() {
                break;
            }
            let mut pid = 0;
            GetWindowThreadProcessId(h, Some(&mut pid));
            n += usize::from(pid == me);
            after = Some(h);
        }
    }
    n
}

#[test]
#[ignore = "takes over the Shell_TrayWnd name for the whole desktop"]
fn notify_icon_calls_reach_the_host_and_explorer_keeps_its_own() {
    let explorer = explorer_tray_window().expect("Explorer's taskbar is running");
    let autohide = autohide_now();
    let notify = Owned::new();
    let owner = Owned::new();
    let host = TrayHost::start(notify.0).unwrap();

    // FindWindow now finds ours; Explorer's is still found by the skipping lookup.
    // SAFETY: plain queries.
    let first = unsafe { FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()) }.unwrap();
    assert_eq!(first, host.hwnd());
    assert_eq!(explorer_tray_window(), Some(explorer));
    // Appbar calls are forwarded and answered by Explorer.
    assert_eq!(autohide_now(), autohide);

    let mut n = nid(owner.0, 7, "vitro-test");
    // SAFETY: `n` is a valid, sized NOTIFYICONDATAW for the call.
    assert!(unsafe { Shell_NotifyIconW(NIM_ADD, &n) }.as_bool());
    let added = wait_tray(notify.0, |m| m.op == TrayOp::Add).expect("Add within 1 s");
    assert_eq!(added.tip.as_deref(), Some("vitro-test"));
    assert_eq!(added.id.uid, 7);
    let e = host.entries();
    assert_eq!(e.len().min(1), 1);
    let entry = e
        .iter()
        .find(|e| e.icon.tip == "vitro-test")
        .expect("in the model")
        .clone();
    assert!(entry.image.is_some(), "the icon was copied");
    let exe = std::env::current_exe()
        .unwrap()
        .to_string_lossy()
        .to_lowercase();
    assert_eq!(tray_identity_key(&entry.icon), format!("{exe}|7"));

    // Version 4: clicks carry the cursor and (event, uid).
    n.Anonymous = NOTIFYICONDATAW_0 {
        uVersion: NOTIFYICON_VERSION_4,
    };
    // SAFETY: as above.
    assert!(unsafe { Shell_NotifyIconW(NIM_SETVERSION, &n) }.as_bool());
    wait_tray(notify.0, |m| m.op == TrayOp::SetVersion).expect("SetVersion");
    let icon = host
        .entries()
        .into_iter()
        .find(|e| e.icon.tip == "vitro-test")
        .unwrap()
        .icon;
    assert_eq!(icon.version, 4);
    RECEIVED.lock().unwrap().clear();
    TrayHost::forward_click(&icon, TrayClick::Left, (100, 200));
    pump_for(100);
    let got = RECEIVED.lock().unwrap().clone();
    let wp = (100 | (200 << 16)) as usize;
    assert_eq!(
        got,
        vec![
            (wp, (WM_LBUTTONDOWN | (7 << 16)) as isize),
            (wp, (WM_LBUTTONUP | (7 << 16)) as isize),
            (wp, (NIN_SELECT | (7 << 16)) as isize),
        ]
    );

    // SAFETY: as above.
    assert!(unsafe { Shell_NotifyIconW(NIM_DELETE, &n) }.as_bool());
    wait_tray(notify.0, |m| m.op == TrayOp::Delete).expect("Delete within 1 s");
    assert!(!host.entries().iter().any(|e| e.icon.tip == "vitro-test"));

    // An owner that dies without deleting its icon is swept.
    {
        let doomed = Owned::new();
        let d = nid(doomed.0, 9, "vitro-doomed");
        // SAFETY: as above.
        assert!(unsafe { Shell_NotifyIconW(NIM_ADD, &d) }.as_bool());
        wait_tray(notify.0, |m| m.op == TrayOp::Add && m.id.uid == 9).expect("second Add");
        drop(doomed);
        assert!(host.sweep() >= 1);
        assert!(!host.entries().iter().any(|e| e.icon.tip == "vitro-doomed"));
        // Explorer got the Add too: take it away there (our model no longer has it, so the
        // answer is FALSE, but the call is still forwarded).
        // SAFETY: as above.
        let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &d) };
    }

    drop(host);
    assert_eq!(own_tray_windows(), 0, "our Shell_TrayWnd is gone");
    assert_eq!(explorer_tray_window(), Some(explorer));
    // SAFETY: plain query.
    let first = unsafe { FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()) }.unwrap();
    assert_eq!(first, explorer, "FindWindow finds Explorer's again");
    assert_eq!(autohide_now(), autohide);
}
