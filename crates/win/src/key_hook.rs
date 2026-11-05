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
//! If the UI thread leaves an output undrained for more than
//! [`vitro_core::key_hook::UI_STALL_MS`] (hung or busy), [`HookState`] decides as if disabled
//! (stock Windows behaviour, in-flight pairs finished) until [`KeyHook::drain`] runs again.
//!
//! Nothing about the keys is stored, logged or sent anywhere; the only statistics kept are a
//! call counter and the longest time spent in the procedure (no key codes).
//!
//! While (and only while) focus mode is on, the same thread also runs a low-level mouse hook
//! (`WH_MOUSE_LL`, ruling S6): any button going down anywhere ends focus mode in the hook state
//! at once (so the very next key passes) and queues `ExitFocus`. It never swallows a mouse
//! event, keeps no coordinates, and is removed as soon as focus mode ends. Focus-mode changes
//! (from the UI thread or from the hooks themselves) post [`WM_SYNC_MOUSE`] to the hook
//! thread, which installs or removes the mouse hook to match.
//!
//! If the process dies, Windows removes the hooks with it. Dropping [`KeyHook`] posts
//! `WM_QUIT` to the hook thread, which unhooks and exits (waited for at most one second; if it
//! does not exit in time, `RUNNING` stays set and a later [`KeyHook::start`] in this process
//! is refused: the old thread still decides correctly until it gets to the quit message).

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicU64, Ordering};
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
    UnhookWindowsHookEx, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG, PM_NOREMOVE,
    WH_KEYBOARD_LL, WH_MOUSE_LL, WM_APP, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_MBUTTONDOWN,
    WM_QUIT, WM_RBUTTONDOWN, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_XBUTTONDOWN,
};

use crate::event_queue::WakeQueue;

/// Payload-free wake-up: hook outputs are waiting in [`KeyHook::drain`] (ruling S2).
pub const WM_VITRO_KEY: u32 = WM_APP + 0x24;

/// Thread message to the hook thread: install or remove the mouse hook to match focus mode.
/// Posted only to our own thread (thread messages are never seen by any window).
pub const WM_SYNC_MOUSE: u32 = WM_APP + 0x01;

/// How long [`KeyHook`]'s drop waits for the hook thread.
const JOIN_TIMEOUT: Duration = Duration::from_secs(1);

/// `Some` while a hook thread runs (one per process).
static STATE: Mutex<Option<HookState>> = Mutex::new(None);
static RUNNING: AtomicBool = AtomicBool::new(false);
static INSTALLED: AtomicBool = AtomicBool::new(false);
static MOUSE_INSTALLED: AtomicBool = AtomicBool::new(false);
static HOOK_THREAD: AtomicU32 = AtomicU32::new(0);
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
    /// The focus-mode mouse hook is installed.
    pub mouse_hook: bool,
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
        forwarded: forwarding_to_another_pc(),
    }
}

/// Mouse Without Borders (the Microsoft Garage app and the PowerToys module share the code)
/// parks this PC's cursor while it drives another one: at the centre column of the primary
/// screen, at y = 4 ("hide mouse at screen edge") or at its centre row. Its window titles
/// are stable across both builds.
const MWB_WINDOWS: [&str; 2] = [
    "Mouse without Borders Helper",
    "Mouse without Borders Application",
];

/// `(x, y)` is where Mouse Without Borders parks the cursor on a `w` x `h` primary screen
/// (a pixel or two of slack for odd sizes and scaled, DPI-unaware coordinates).
pub fn is_mwb_park_point(x: i32, y: i32, w: i32, h: i32) -> bool {
    (x - w / 2).abs() <= 2 && ((0..=8).contains(&y) || (y - h / 2).abs() <= 2)
}

/// The keys typed now go to another computer through Mouse Without Borders. Cheap enough
/// for the hook: two metrics and the cursor position, and the window lookup (no messages
/// are sent) only while the cursor sits on the park point.
fn forwarding_to_another_pc() -> bool {
    use windows::core::HSTRING;
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowW, GetCursorPos, GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN,
    };
    let mut p = POINT::default();
    // SAFETY: plain queries; FindWindowW compares titles without messaging the windows.
    unsafe {
        if GetCursorPos(&mut p).is_err() {
            return false;
        }
        let (w, h) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
        if !is_mwb_park_point(p.x, p.y, w, h) {
            return false;
        }
        MWB_WINDOWS
            .iter()
            .any(|t| FindWindowW(None, &HSTRING::from(*t)).is_ok_and(|w| !w.is_invalid()))
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

/// Asks the hook thread to install / remove the mouse hook to match focus mode.
fn post_sync() {
    let tid = HOOK_THREAD.load(Ordering::Acquire);
    if tid != 0 {
        // SAFETY: posting to our own thread never blocks.
        let _ = unsafe { PostThreadMessageW(tid, WM_SYNC_MOUSE, WPARAM(0), LPARAM(0)) };
    }
}

fn queue_output(out: HookOutput) {
    if OUTPUTS.push(out) {
        wake_ui();
    }
}

/// Posts the payload-free wake-up to the notify window.
fn wake_ui() {
    let notify = HWND(NOTIFY.load(Ordering::Acquire) as *mut _);
    // SAFETY: posting never blocks on the receiver.
    if unsafe { PostMessageW(Some(notify), WM_VITRO_KEY, WPARAM(0), LPARAM(0)) }.is_err() {
        OUTPUTS.wake_failed();
    }
}

/// Decides on one physical event at `time` (ms); `true` = swallow.
fn decide(ev: KeyEv, time: u32) -> bool {
    let held = matches!(ev, KeyEv::Down(_)).then(physical_held);
    let (decision, wake, focus_changed) = {
        let mut guard = state();
        let Some(st) = guard.as_mut() else {
            return false;
        };
        if let Some(h) = held {
            st.sync_held(h);
        }
        let before = st.focus_mode();
        let (d, o) = st.on_event_at(ev, time);
        // Queued under the state lock, as [`KeyHook::drain`] drains under it: the state's
        // "oldest undrained output" time (the hung-UI fallback) always matches the queue.
        let wake = o.is_some_and(|o| OUTPUTS.push(o));
        (d, wake, before != st.focus_mode())
    };
    if focus_changed {
        post_sync();
    }
    if decision == HookDecision::PassAndMask {
        // Before the UI thread is woken: the Start panel it opens asks for the foreground
        // right after, and the mask key must already be the latest input by then.
        inject_mask_key();
    }
    if wake {
        wake_ui();
    }
    decision == HookDecision::Swallow
}

fn record_call(t0: Instant) {
    CALLS.fetch_add(1, Ordering::Relaxed);
    MAX_NS.fetch_max(t0.elapsed().as_nanos() as u64, Ordering::Relaxed);
}

/// The focus-mode mouse hook: a button down ends focus mode. Never swallows; reads nothing
/// but the message id.
unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let t0 = Instant::now();
    if code == HC_ACTION as i32
        && matches!(
            wparam.0 as u32,
            WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN
        )
    {
        let out = state().as_mut().and_then(HookState::on_mouse_down);
        if let Some(out) = out {
            queue_output(out);
            post_sync();
        }
    }
    record_call(t0);
    // SAFETY: passes the event on unchanged.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let t0 = Instant::now();
    let mut swallow = false;
    if code == HC_ACTION as i32 && lparam.0 != 0 {
        // SAFETY: for HC_ACTION, lParam points at the event's KBDLLHOOKSTRUCT.
        let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        if (kb.flags.0 & LLKHF_INJECTED.0) == 0 {
            if let Some(ev) = key_event(wparam.0 as u32, kb.vkCode) {
                swallow = decide(ev, kb.time);
            }
        }
    }
    record_call(t0);
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
        post_sync();
    }

    pub fn set_focus_mode(&self, on: bool) {
        if let Some(st) = state().as_mut() {
            st.set_focus_mode(on);
        }
        post_sync();
    }

    /// The session or the input desktop switched: key-ups may have been lost.
    pub fn forget_swallowed(&self) {
        if let Some(st) = state().as_mut() {
            st.forget_swallowed();
        }
    }

    /// What the hook asked for since the last drain, in order. Draining also tells the
    /// hook the UI thread is responsive (it ends the hung-UI fallback, see
    /// [`vitro_core::key_hook::UI_STALL_MS`]).
    pub fn drain(&self) -> Vec<HookOutput> {
        let mut guard = state();
        let out = OUTPUTS.drain();
        if let Some(st) = guard.as_mut() {
            st.ui_drained();
        }
        out
    }

    pub fn stats(&self) -> HookStats {
        let (enabled, focus_mode) = state()
            .as_ref()
            .map_or((false, false), |s| (s.enabled(), s.focus_mode()));
        HookStats {
            installed: INSTALLED.load(Ordering::Acquire),
            enabled,
            focus_mode,
            mouse_hook: MOUSE_INSTALLED.load(Ordering::Acquire),
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
    HOOK_THREAD.store(0, Ordering::Release);
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
    let tid = unsafe { GetCurrentThreadId() };
    HOOK_THREAD.store(tid, Ordering::Release);
    let _ = ready.send(Ok(tid));
    let mut mouse: Option<HHOOK> = None;
    // Nothing else happens on this thread: the loop only lets the hook callbacks run and
    // keeps the mouse hook in step with focus mode.
    // SAFETY: standard message retrieval; there are no windows to dispatch to.
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {
        if msg.message == WM_SYNC_MOUSE && msg.hwnd.is_invalid() {
            sync_mouse_hook(&mut mouse);
        }
    }
    // SAFETY: our own hooks, each removed once.
    unsafe {
        if let Some(m) = mouse.take() {
            let _ = UnhookWindowsHookEx(m);
        }
        let _ = UnhookWindowsHookEx(hook);
    }
    MOUSE_INSTALLED.store(false, Ordering::Release);
    INSTALLED.store(false, Ordering::Release);
    cleanup();
    let _ = done.send(());
}

/// Installs the mouse hook while focus mode is on, removes it otherwise (hook thread only).
fn sync_mouse_hook(mouse: &mut Option<HHOOK>) {
    let want = state().as_ref().is_some_and(HookState::focus_mode);
    match (want, mouse.is_some()) {
        (true, false) => {
            // SAFETY: a low-level hook whose callbacks run on this thread's loop.
            let hook = unsafe {
                GetModuleHandleW(None).and_then(|module| {
                    SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), Some(module.into()), 0)
                })
            };
            // Without it a click simply does not end focus mode (keys still do).
            *mouse = hook.ok();
        }
        (false, true) => {
            if let Some(m) = mouse.take() {
                // SAFETY: our own hook, removed once.
                unsafe {
                    let _ = UnhookWindowsHookEx(m);
                }
            }
        }
        _ => {}
    }
    MOUSE_INSTALLED.store(mouse.is_some(), Ordering::Release);
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
            // process exit removes the hook in any case. RUNNING stays set (the thread
            // clears it as its last step), so a new start in this process is refused.
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
    fn mwb_park_points() {
        // Hide-mouse-at-edge parks at (centre, 4); otherwise at the screen's centre.
        assert!(is_mwb_park_point(960, 4, 1920, 1080));
        assert!(is_mwb_park_point(960, 6, 1920, 1080), "4 at 150 % scale");
        assert!(is_mwb_park_point(960, 540, 1920, 1080));
        assert!(is_mwb_park_point(1280, 720, 2560, 1440));
        assert!(!is_mwb_park_point(960, 300, 1920, 1080));
        assert!(!is_mwb_park_point(100, 4, 1920, 1080));
        assert!(
            !is_mwb_park_point(960, 1070, 1920, 1080),
            "the bottom bar, centred"
        );
    }

    #[test]
    fn no_hook_no_state() {
        // Without a running hook the setters and the decision are no-ops that pass.
        if !hook_thread_running() {
            assert!(!decide(KeyEv::Down(0x5B), 0));
            assert!(!decide(KeyEv::Up(0x5B), 0), "no mask, nothing swallowed");
        }
    }
}
