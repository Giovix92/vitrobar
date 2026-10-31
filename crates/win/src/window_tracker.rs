//! Window lifecycle events: shell hook messages plus out-of-context WinEvent hooks.
//!
//! The shell hook messages arrive at the notify window's procedure (owned by the app): the app
//! forwards them to [`translate_shell_hook`] and forwards `WM_TIMER` to [`WindowTracker::on_timer`].
//! WinEvent callbacks are delivered on the thread that called [`WindowTracker::start`], which
//! must therefore be the thread that owns the notify window and pumps its messages.
//!
//! Events from the WinEvent hooks go into a process-owned queue; `notify` gets a payload-free
//! `WM_VITRO_WINDOW_EVENT` wake-up and its procedure calls [`drain_window_events`]. The
//! message's parameters are ignored, so another process posting it can do no harm.

use std::collections::HashMap;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock};

use vitro_core::windows_model::Hwnd;

use crate::event_queue::WakeQueue;
use windows::core::{w, Error, Result, BOOL};
use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, E_FAIL, HWND, LPARAM, WPARAM};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{
    DeregisterShellHookWindow, EnumWindows, KillTimer, PostMessageW, RegisterShellHookWindow,
    RegisterWindowMessageW, SetTimer, CHILDID_SELF, EVENT_OBJECT_CLOAKED,
    EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_NAMECHANGE, EVENT_OBJECT_UNCLOAKED,
    EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND, EVENT_SYSTEM_MINIMIZESTART,
    HSHELL_MONITORCHANGED, HSHELL_REDRAW, HSHELL_WINDOWACTIVATED, HSHELL_WINDOWCREATED,
    HSHELL_WINDOWDESTROYED, OBJID_WINDOW, WINEVENT_OUTOFCONTEXT, WM_APP,
};

/// Wake-up only (`wParam` / `lParam` ignored): window events are waiting in
/// [`drain_window_events`].
pub const WM_VITRO_WINDOW_EVENT: u32 = WM_APP + 1;

/// One-shot timer (on `notify`) driving LOCATIONCHANGE coalescing.
pub const TIMER_LOCATION: usize = 0x5601;
/// One-shot timer (on `notify`) that fires after 50 ms without cloak/uncloak events.
pub const TIMER_DESKTOP: usize = 0x5602;

const LOCATION_COALESCE_MS: u32 = 100;
const DESKTOP_QUIET_MS: u32 = 50;

/// `HSHELL_FLASH` = `HSHELL_REDRAW | HSHELL_HIGHBIT`.
const HSHELL_FLASH: u32 = 0x8006;
/// `HSHELL_RUDEAPPACTIVATED` = `HSHELL_WINDOWACTIVATED | HSHELL_HIGHBIT`.
const HSHELL_RUDEAPPACTIVATED: u32 = 0x8004;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowEvent {
    Created(Hwnd),
    Destroyed(Hwnd),
    Changed(Hwnd),
    Activated(Option<Hwnd>),
    /// `EVENT_SYSTEM_FOREGROUND`: also fires for shell windows (the stock flyouts) that the
    /// shell hook never reports. Separate from `Activated` so its consumers do no extra work.
    Foreground(Hwnd),
    Flash(Hwnd),
    FullscreenChanged,
    DesktopSwitched,
}

/// The notify window of the running tracker (0 = none). Doubles as the "one per process" guard.
static NOTIFY: AtomicIsize = AtomicIsize::new(0);

/// Events waiting for the UI thread.
static EVENTS: WakeQueue<WindowEvent> = WakeQueue::new();

/// Per-window LOCATIONCHANGE cooldown: `true` = more events arrived while cooling down.
static COOLING: LazyLock<Mutex<HashMap<isize, bool>>> = LazyLock::new(Default::default);

fn notify_hwnd() -> Option<HWND> {
    match NOTIFY.load(Ordering::Acquire) {
        0 => None,
        n => Some(HWND(n as *mut _)),
    }
}

fn post(ev: WindowEvent) {
    if let Some(notify) = notify_hwnd() {
        queue_window_event(notify, ev);
    }
}

/// Queues `ev` as the tracker does and wakes `notify` with a payload-free
/// `WM_VITRO_WINDOW_EVENT` (one per burst). In-process only: the data never travels in the
/// message. Public for tests that need a real event without real windows.
pub fn queue_window_event(notify: HWND, ev: WindowEvent) {
    if EVENTS.push(ev) {
        // SAFETY: a payload-free post.
        if unsafe { PostMessageW(Some(notify), WM_VITRO_WINDOW_EVENT, WPARAM(0), LPARAM(0)) }
            .is_err()
        {
            EVENTS.wake_failed();
        }
    }
}

/// Takes the window events queued by the tracker, in order. Call when
/// `WM_VITRO_WINDOW_EVENT` arrives, whoever sent it: its parameters carry nothing.
pub fn drain_window_events() -> Vec<WindowEvent> {
    EVENTS.drain()
}

/// The registered message id of `"SHELLHOOK"`.
pub fn shell_hook_message_id() -> u32 {
    static ID: OnceLock<u32> = OnceLock::new();
    // SAFETY: plain registration call with a static string.
    *ID.get_or_init(|| unsafe { RegisterWindowMessageW(w!("SHELLHOOK")) })
}

/// Maps a shell hook message (`wParam` = code, `lParam` = hwnd) to an event.
pub fn translate_shell_hook(wparam: WPARAM, lparam: LPARAM) -> Option<WindowEvent> {
    let code = wparam.0 as u32;
    let h = lparam.0 as usize as u64;
    match code {
        HSHELL_WINDOWCREATED => Some(WindowEvent::Created(h)),
        HSHELL_WINDOWDESTROYED => Some(WindowEvent::Destroyed(h)),
        HSHELL_WINDOWACTIVATED => Some(WindowEvent::Activated((h != 0).then_some(h))),
        HSHELL_RUDEAPPACTIVATED => Some(WindowEvent::FullscreenChanged),
        HSHELL_REDRAW | HSHELL_MONITORCHANGED => Some(WindowEvent::Changed(h)),
        HSHELL_FLASH => Some(WindowEvent::Flash(h)),
        _ => None,
    }
}

fn set_timer(id: usize, ms: u32) {
    if let Some(notify) = notify_hwnd() {
        // SAFETY: WinEvent callbacks run on the thread that owns `notify`, which SetTimer needs.
        unsafe { SetTimer(Some(notify), id, ms, None) };
    }
}

fn kill_timer(id: usize) {
    if let Some(notify) = notify_hwnd() {
        // SAFETY: plain call; failure (timer not set) is irrelevant.
        unsafe {
            let _ = KillTimer(Some(notify), id);
        }
    }
}

fn changed_coalesced(hwnd: isize) {
    let mut cooling = COOLING.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(dirty) = cooling.get_mut(&hwnd) {
        *dirty = true;
        return;
    }
    let was_empty = cooling.is_empty();
    cooling.insert(hwnd, false);
    drop(cooling);
    post(WindowEvent::Changed(hwnd as usize as u64));
    if was_empty {
        set_timer(TIMER_LOCATION, LOCATION_COALESCE_MS);
    }
}

fn on_location_timer() {
    let mut flush = Vec::new();
    let keep_going = {
        let mut cooling = COOLING.lock().unwrap_or_else(|e| e.into_inner());
        cooling.retain(|&h, dirty| {
            if *dirty {
                *dirty = false;
                flush.push(h);
                true
            } else {
                false
            }
        });
        !cooling.is_empty()
    };
    for h in flush {
        post(WindowEvent::Changed(h as usize as u64));
    }
    if keep_going {
        set_timer(TIMER_LOCATION, LOCATION_COALESCE_MS);
    } else {
        kill_timer(TIMER_LOCATION);
    }
}

unsafe extern "system" fn win_event_proc(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    if hwnd.0.is_null() || id_object != OBJID_WINDOW.0 || id_child != CHILDID_SELF as i32 {
        return;
    }
    let id = hwnd.0 as usize as u64;
    match event {
        EVENT_OBJECT_LOCATIONCHANGE => changed_coalesced(hwnd.0 as isize),
        EVENT_SYSTEM_FOREGROUND => post(WindowEvent::Foreground(id)),
        EVENT_OBJECT_CLOAKED | EVENT_OBJECT_UNCLOAKED => {
            post(WindowEvent::Changed(id));
            // Restarts the countdown: DesktopSwitched fires once the burst has been quiet.
            set_timer(TIMER_DESKTOP, DESKTOP_QUIET_MS);
        }
        _ => post(WindowEvent::Changed(id)),
    }
}

pub struct WindowTracker {
    notify: HWND,
    hooks: Vec<HWINEVENTHOOK>,
}

impl WindowTracker {
    /// Starts tracking. Only one tracker may exist per process. Call from the thread that owns
    /// `notify` and runs its message loop.
    pub fn start(notify: HWND) -> Result<Self> {
        if NOTIFY
            .compare_exchange(0, notify.0 as isize, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(Error::from(ERROR_ALREADY_EXISTS.to_hresult()));
        }
        EVENTS.clear();
        let _ = shell_hook_message_id();
        let mut tracker = Self {
            notify,
            hooks: Vec::new(),
        };
        // From here on `Drop` undoes whatever succeeded, including on early return.
        // SAFETY: plain registration with a window owned by the caller.
        if !unsafe { RegisterShellHookWindow(notify) }.as_bool() {
            return Err(Error::from(E_FAIL)); // `tracker` drops and releases the guard
        }
        let ranges = [
            (EVENT_OBJECT_NAMECHANGE, EVENT_OBJECT_NAMECHANGE),
            (EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED),
            (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
            (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
            (EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE),
        ];
        for (lo, hi) in ranges {
            // SAFETY: the callback is a plain function and keeps no pointers.
            let hook = unsafe {
                SetWinEventHook(
                    lo,
                    hi,
                    None,
                    Some(win_event_proc),
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT,
                )
            };
            if hook.0.is_null() {
                return Err(Error::from(E_FAIL)); // `tracker` drops and cleans up
            }
            tracker.hooks.push(hook);
        }
        Ok(tracker)
    }

    /// All current top-level windows (unfiltered; use `read_window_info` + `is_taskbar_window`).
    pub fn enumerate(&self) -> Vec<HWND> {
        unsafe extern "system" fn collect(h: HWND, data: LPARAM) -> BOOL {
            // SAFETY: `data` is the `&mut Vec<HWND>` passed below, alive during the call.
            unsafe { &mut *(data.0 as *mut Vec<HWND>) }.push(h);
            true.into()
        }
        let mut out: Vec<HWND> = Vec::new();
        // SAFETY: the callback only touches `out`, which outlives the synchronous enumeration.
        unsafe {
            let _ = EnumWindows(Some(collect), LPARAM(&mut out as *mut Vec<HWND> as isize));
        }
        out
    }

    /// Call from the notify window's `WM_TIMER` handler with the timer id (`wParam`).
    /// Ids other than [`TIMER_LOCATION`] and [`TIMER_DESKTOP`] are ignored.
    pub fn on_timer(&self, id: usize) {
        match id {
            TIMER_LOCATION => on_location_timer(),
            TIMER_DESKTOP => {
                kill_timer(TIMER_DESKTOP);
                post(WindowEvent::DesktopSwitched);
            }
            _ => {}
        }
    }
}

impl Drop for WindowTracker {
    fn drop(&mut self) {
        // SAFETY: unhooking handles we created and deregistering our own notify window.
        unsafe {
            for h in self.hooks.drain(..) {
                let _ = UnhookWinEvent(h);
            }
            let _ = DeregisterShellHookWindow(self.notify);
        }
        kill_timer(TIMER_LOCATION);
        kill_timer(TIMER_DESKTOP);
        COOLING.lock().unwrap_or_else(|e| e.into_inner()).clear();
        NOTIFY.store(0, Ordering::Release);
        EVENTS.clear();
    }
}
