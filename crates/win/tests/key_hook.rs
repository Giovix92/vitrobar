//! The real low-level keyboard hook, installed for a few seconds.
//!
//! It is system-wide (every key on the desktop passes through it while it runs) and the test
//! injects keys, so it is ignored by default:
//!
//! `cargo test -p vitro-win --test key_hook -- --ignored --test-threads=1`
//!
//! The only key injected is the unassigned mask key (VK 0xE8). Injected events are let
//! through untouched by design, so they exercise only the hook procedure's fast path; the
//! real Windows-key behaviour is checked by hand (release checklist, "Keyboard"). A guard
//! always unhooks, even when an assertion fails, and checks that no Windows key is left down.
//! The focus-mode mouse hook is installed and removed (no mouse input is injected: it would
//! reach whatever window is under the cursor).

use std::time::{Duration, Instant};

use vitro_win::key_hook::{hook_thread_running, inject_mask_key, KeyHook};
use windows::core::w;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
    VIRTUAL_KEY, VK_LWIN, VK_RWIN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE,
};

fn win_down(vk: VIRTUAL_KEY) -> bool {
    // SAFETY: plain query.
    unsafe { GetAsyncKeyState(vk.0 as i32) as u16 & 0x8000 != 0 }
}

fn any_win_down() -> bool {
    win_down(VK_LWIN) || win_down(VK_RWIN)
}

/// Releases a Windows key that reads as down (never expected; reported if it happens).
fn release_stuck_win() -> bool {
    let mut released = false;
    for vk in [VK_LWIN, VK_RWIN] {
        if win_down(vk) {
            let up = INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: vk,
                        dwFlags: KEYEVENTF_KEYUP,
                        ..Default::default()
                    },
                },
            };
            // SAFETY: one input, described by the slice.
            unsafe { SendInput(&[up], size_of::<INPUT>() as i32) };
            released = true;
        }
    }
    released
}

/// Always unhooks (also while a failed assertion unwinds) and releases a stuck Windows key.
struct Guard {
    hook: Option<KeyHook>,
    window: HWND,
}

impl Drop for Guard {
    fn drop(&mut self) {
        drop(self.hook.take());
        if release_stuck_win() {
            eprintln!("key_hook test: a Windows key read as down after the run: released");
        }
        // SAFETY: our own message-only window.
        unsafe {
            let _ = DestroyWindow(self.window);
        }
    }
}

fn notify_window() -> HWND {
    // SAFETY: a message-only STATIC window owned by this test.
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            w!("vitrobar key hook test"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            None,
            None,
        )
        .expect("test window")
    }
}

#[test]
#[ignore = "installs a system-wide keyboard hook and injects keys"]
fn hook_answers_while_the_ui_thread_is_blocked_and_unhooks_on_drop() {
    assert!(
        !any_win_down(),
        "a Windows key is down before the test: not starting"
    );
    let window = notify_window();
    let mut guard = Guard {
        hook: Some(KeyHook::start(window, true).expect("hook installs")),
        window,
    };
    let hook = guard.hook.as_ref().unwrap();
    assert!(hook_thread_running());
    assert!(hook.stats().installed && hook.stats().enabled);
    assert!(
        KeyHook::start(window, true).is_err(),
        "one hook per process"
    );

    // This thread plays the UI thread and blocks for 2 s (no message pumping at all) while
    // another thread types the mask key: the hook, on its own thread, keeps answering.
    const PAIRS: u64 = 100;
    let injector = std::thread::spawn(|| {
        let mut slowest = Duration::ZERO;
        for _ in 0..PAIRS {
            let t = Instant::now();
            inject_mask_key();
            slowest = slowest.max(t.elapsed());
            std::thread::sleep(Duration::from_millis(15));
        }
        slowest
    });
    std::thread::sleep(Duration::from_secs(2));
    let slowest_send = injector.join().unwrap();
    // The last events may still be in flight.
    let deadline = Instant::now() + Duration::from_secs(2);
    while hook.stats().calls < 2 * PAIRS && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let stats = hook.stats();
    println!(
        "hook calls {} (>= {} injected), slowest call {} us, slowest SendInput {:?}",
        stats.calls,
        2 * PAIRS,
        stats.max_us,
        slowest_send
    );
    assert!(stats.calls >= 2 * PAIRS, "{stats:?}");
    assert!(stats.max_us < 5_000, "{stats:?}");
    assert!(
        hook.drain().is_empty(),
        "injected keys are never decided on"
    );
    // Focus mode brings the mouse hook (on the hook thread), leaving it removes it.
    let wait_for = |want: bool| {
        let deadline = Instant::now() + Duration::from_secs(2);
        while hook.stats().mouse_hook != want && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        hook.stats().mouse_hook == want
    };
    assert!(!hook.stats().mouse_hook, "no mouse hook outside focus mode");
    hook.set_focus_mode(true);
    assert!(hook.stats().focus_mode);
    assert!(wait_for(true), "mouse hook installed in focus mode");
    hook.set_focus_mode(false);
    assert!(wait_for(false), "mouse hook removed when focus mode ends");
    hook.set_focus_mode(true);
    assert!(wait_for(true));
    // Disabling ends focus mode, and with it the mouse hook.
    hook.set_enabled(false);
    assert!(!hook.stats().enabled && !hook.stats().focus_mode);
    assert!(wait_for(false));
    hook.set_focus_mode(true);
    assert!(!hook.stats().focus_mode, "no focus mode while disabled");
    hook.set_enabled(true);
    hook.set_focus_mode(true);
    assert!(wait_for(true));
    hook.forget_swallowed();
    // Dropped below while the mouse hook is in: the thread removes both.

    // Dropping unhooks and ends the thread (bounded wait inside the drop).
    drop(guard.hook.take());
    assert!(!hook_thread_running(), "the hook thread has exited");
    assert!(!any_win_down(), "no Windows key left down");

    // Installs again (as after a watchdog restart), and goes again.
    guard.hook = Some(KeyHook::start(window, false).expect("hook installs again"));
    assert!(guard.hook.as_ref().unwrap().stats().installed);
    assert!(
        !guard.hook.as_ref().unwrap().stats().mouse_hook,
        "the old mouse hook went with its thread"
    );
    drop(guard.hook.take());
    assert!(!hook_thread_running());
    drop(guard);
    assert!(!any_win_down());
}
