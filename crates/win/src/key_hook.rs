//! The low-level keyboard hook (`WH_KEYBOARD_LL`) on its own thread, "vitrobar-keys".
//!
//! That thread does nothing but run the message loop the hook needs, so the hook answers in
//! microseconds even while the UI thread is busy (Windows silently removes a hook that is too
//! slow, and every key on the desktop waits for it). The hook procedure:
//! 1. lets injected events through at once (`LLKHF_INJECTED`: our own mask key and Ctrl+Esc,
//!    and anything other software types);
//! 2. otherwise takes the decision from [`HookState`] behind a mutex held only for that;
//! 3. injects the mask key for [`HookDecision::PassAndMask`];
//! 4. queues the resulting [`HookOutput`] (if any) and posts the payload-free
//!    [`WM_VITRO_KEY`] to the notify window;
//! 5. returns 1 to swallow, or `CallNextHookEx` for everything it passes.
//!
//! Nothing about the keys is stored, logged or sent anywhere; the only statistics kept are a
//! call counter and the longest time spent in the procedure (no key codes).
//!
//! If the process dies, Windows removes the hook with it. Dropping [`KeyHook`] posts `WM_QUIT`
//! to the hook thread, which unhooks and exits (waited for at most one second).

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use vitro_core::key_hook::{Held, HookDecision, HookOutput, HookState, KeyEv, VK_MASK};
use windows::core::{Error, Result};
use windows::Win32::Foundation::{E_FAIL, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, PeekMessageW, PostMessageW, PostThreadMessageW, SetWindowsHookExW,
    UnhookWindowsHookEx, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG, PM_NOREMOVE,
    WH_KEYBOARD_LL, WM_APP, WM_KEYDOWN, WM_KEYUP, WM_QUIT, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use crate::event_queue::WakeQueue;

/// Payload-free wake-up: hook outputs are waiting in [`KeyHook::drain`] (ruling S2).
pub const WM_VITRO_KEY: u32 = WM_APP + 0x24;

/// How long [`KeyHook`]'s drop waits for the hook thread.
const JOIN_TIMEOUT: Duration = Duration::from_secs(1);

/// `Some` while a hook thread runs (one per process).
static STATE: Mutex<Option<HookState>> = Mutex::new(None);
static RUNNING: AtomicBool = AtomicBool::new(false);
static INSTALLED: AtomicBool = AtomicBool::new(false);
static NOTIFY: AtomicIsize = AtomicIsize::new(0);
/// Single producer: the hook thread.
static OUTPUTS: WakeQueue<HookOutput> = WakeQueue::new();
static CALLS: AtomicU64 = AtomicU64::new(0);
static MAX_NS: AtomicU64 = AtomicU64::new(0);

fn state() -> MutexGuard<'static, Option<HookState>> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// The hook's own counters (never key data).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HookStats {
    pub installed: bool,
    pub enabled: bool,
    pub focus_mode: bool,
    /// Hook procedure calls since start (injected ones included).
    pub calls: u64,
    /// The longest time one call took, in microseconds.
    pub max_us: u64,
}

/// The key event a low-level hook message describes (`None` for anything else).
pub fn key_event(msg: u32, vk: u32) -> Option<KeyEv> {
    match msg {
        WM_KEYDOWN | WM_SYSKEYDOWN => Some(KeyEv::Down(vk)),
        WM_KEYUP | WM_SYSKEYUP => Some(KeyEv::Up(vk)),
        _ => None,
    }
}

fn down(vk: VIRTUAL_KEY) -> bool {
    // SAFETY: plain query.
    unsafe { GetAsyncKeyState(vk.0 as i32) < 0 }
}

/// The physical Windows-key / modifier state. If the system will not tell us (another
/// desktop), everything reads as up: then Win+n passes to Windows, the stock behaviour.
fn physical_held() -> Held {
    Held {
        win: down(VK_LWIN) || down(VK_RWIN),
        modifier: down(VK_SHIFT) || down(VK_CONTROL) || down(VK_MENU),
    }
}

fn key_input(vk: u16, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                dwFlags: if up {
                    KEYEVENTF_KEYUP
                } else {
                    KEYBD_EVENT_FLAGS(0)
                },
                ..Default::default()
            },
        },
    }
}

/// Injects the mask key (down + up). Ours is injected, so this hook lets it through untouched.
pub fn inject_mask_key() {
    let inputs = [
        key_input(VK_MASK as u16, false),
        key_input(VK_MASK as u16, true),
    ];
    // SAFETY: the slice and the element size describe the array above.
    unsafe {
        SendInput(&inputs, size_of::<INPUT>() as i32);
    }
}

/// Decides on one physical event; `true` = swallow.
fn decide(ev: KeyEv) -> bool {
    let held = matches!(ev, KeyEv::Down(_)).then(physical_held);
    let (decision, output) = {
        let mut guard = state();
        let Some(st) = guard.as_mut() else {
            return false;
        };
        if let Some(h) = held {
            st.sync_held(h);
        }
        st.on_event(ev)
    };
    if decision == HookDecision::PassAndMask {
        // Before the UI thread is woken: the Start panel it opens asks for the foreground
        // right after, and the mask key must already be the latest input by then.
        inject_mask_key();
    }
    if let Some(out) = output {
        if OUTPUTS.push(out) {
            let notify = HWND(NOTIFY.load(Ordering::Acquire) as *mut _);
            // SAFETY: posting never blocks on the receiver.
            if unsafe { PostMessageW(Some(notify), WM_VITRO_KEY, WPARAM(0), LPARAM(0)) }.is_err() {
                OUTPUTS.wake_failed();
            }
        }
    }
    decision == HookDecision::Swallow
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let t0 = Instant::now();
    let mut swallow = false;
    if code == HC_ACTION as i32 && lparam.0 != 0 {
        // SAFETY: for HC_ACTION, lParam points at the event's KBDLLHOOKSTRUCT.
        let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        if (kb.flags.0 & LLKHF_INJECTED.0) == 0 {
            if let Some(ev) = key_event(wparam.0 as u32, kb.vkCode) {
                swallow = decide(ev);
            }
        }
    }
    CALLS.fetch_add(1, Ordering::Relaxed);
    MAX_NS.fetch_max(t0.elapsed().as_nanos() as u64, Ordering::Relaxed);
    if swallow {
        LRESULT(1)
    } else {
        // SAFETY: passes the event on unchanged.
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }
}

/// The installed hook; dropping it unhooks.
pub struct KeyHook {
    thread_id: u32,
    done: mpsc::Receiver<()>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl KeyHook {
    /// Starts the hook thread and installs the hook (one per process). Outputs wake `notify`
    /// with [`WM_VITRO_KEY`].
    pub fn start(notify: HWND, enabled: bool) -> Result<Self> {
        if RUNNING.swap(true, Ordering::AcqRel) {
            return Err(Error::new(E_FAIL, "the keyboard hook is already running"));
        }
        *state() = Some(HookState::new(enabled));
        NOTIFY.store(notify.0 as isize, Ordering::Release);
        OUTPUTS.clear();
        CALLS.store(0, Ordering::Relaxed);
        MAX_NS.store(0, Ordering::Relaxed);
        let (ready_tx, ready_rx) = mpsc::channel::<Result<u32>>();
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let spawned = std::thread::Builder::new()
            .name("vitrobar-keys".into())
            .spawn(move || hook_thread(ready_tx, done_tx));
        let thread = match spawned {
            Ok(t) => t,
            Err(e) => {
                cleanup();
                return Err(Error::new(E_FAIL, format!("no hook thread: {e}")));
            }
        };
        match ready_rx.recv() {
            Ok(Ok(thread_id)) => Ok(Self {
                thread_id,
                done: done_rx,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = thread.join();
                Err(Error::new(E_FAIL, "the hook thread ended early"))
            }
        }
    }

    pub fn set_enabled(&self, on: bool) {
        if let Some(st) = state().as_mut() {
            st.set_enabled(on);
        }
    }

    pub fn set_focus_mode(&self, on: bool) {
        if let Some(st) = state().as_mut() {
            st.set_focus_mode(on);
        }
    }

    /// What the hook asked for since the last drain, in order.
    pub fn drain(&self) -> Vec<HookOutput> {
        OUTPUTS.drain()
    }

    pub fn stats(&self) -> HookStats {
        let (enabled, focus_mode) = state()
            .as_ref()
            .map_or((false, false), |s| (s.enabled(), s.focus_mode()));
        HookStats {
            installed: INSTALLED.load(Ordering::Acquire),
            enabled,
            focus_mode,
            calls: CALLS.load(Ordering::Relaxed),
            max_us: MAX_NS.load(Ordering::Relaxed) / 1000,
        }
    }
}

/// A hook thread is running in this process (it clears this as the very last step).
pub fn hook_thread_running() -> bool {
    RUNNING.load(Ordering::Acquire)
}

fn cleanup() {
    *state() = None;
    OUTPUTS.clear();
    NOTIFY.store(0, Ordering::Release);
    RUNNING.store(false, Ordering::Release);
}

fn hook_thread(ready: mpsc::Sender<Result<u32>>, done: mpsc::Sender<()>) {
    let mut msg = MSG::default();
    // SAFETY: creates this thread's message queue (so WM_QUIT can be posted to it), then
    // installs the hook, whose callbacks run on this thread's message loop.
    let hook = unsafe {
        let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
        GetModuleHandleW(None).and_then(|module| {
            SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), Some(module.into()), 0)
        })
    };
    let hook = match hook {
        Ok(h) => h,
        Err(e) => {
            cleanup();
            let _ = ready.send(Err(e));
            return;
        }
    };
    INSTALLED.store(true, Ordering::Release);
    // SAFETY: plain call.
    let _ = ready.send(Ok(unsafe { GetCurrentThreadId() }));
    // Nothing else happens on this thread: the loop only lets the hook callbacks run.
    // SAFETY: standard message retrieval; there are no windows to dispatch to.
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {}
    // SAFETY: our own hook, removed once.
    unsafe {
        let _ = UnhookWindowsHookEx(hook);
    }
    INSTALLED.store(false, Ordering::Release);
    cleanup();
    let _ = done.send(());
}

impl Drop for KeyHook {
    fn drop(&mut self) {
        // SAFETY: posting to our own thread never blocks.
        let posted =
            unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) }.is_ok();
        if posted && self.done.recv_timeout(JOIN_TIMEOUT).is_ok() {
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        } else {
            // The thread keeps deciding correctly until it gets to the quit message; the
            // process exit removes the hook in any case.
            eprintln!("vitrobar: the keyboard hook thread did not stop within 1 s");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_messages_map_to_key_events() {
        assert_eq!(key_event(WM_KEYDOWN, 0x5B), Some(KeyEv::Down(0x5B)));
        assert_eq!(key_event(WM_SYSKEYDOWN, 0x41), Some(KeyEv::Down(0x41)));
        assert_eq!(key_event(WM_KEYUP, 0x5C), Some(KeyEv::Up(0x5C)));
        assert_eq!(key_event(WM_SYSKEYUP, 0x31), Some(KeyEv::Up(0x31)));
        assert_eq!(key_event(WM_APP, 0x31), None);
    }

    #[test]
    fn no_hook_no_state() {
        // Without a running hook the setters and the decision are no-ops that pass.
        if !hook_thread_running() {
            assert!(!decide(KeyEv::Down(0x5B)));
            assert!(!decide(KeyEv::Up(0x5B)), "no mask, nothing swallowed");
        }
    }
}
