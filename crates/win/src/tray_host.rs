//! The tray host: Vitrobar's own `Shell_TrayWnd`, which receives the `Shell_NotifyIcon`
//! traffic of every app, so their notification-area icons can be drawn in the bar.
//!
//! How it works (the approach of ManagedShell's `TrayService`):
//! - A window of class `Shell_TrayWnd` (window classes are per process, so registering the
//!   name here does not touch Explorer) is kept at the top of the topmost band, 0 x 0 and
//!   never shown. `Shell_NotifyIcon` and `SHAppBarMessage` find their target with
//!   `FindWindow("Shell_TrayWnd")`, which returns the first match in z-order: ours.
//! - Nothing is taken away from Explorer. Every `WM_COPYDATA` (tray icons, appbar calls,
//!   icon-rect queries) and every message addressed to the taskbar (`WM_COMMAND`,
//!   `WM_USER`..`WM_APP` range) is forwarded synchronously to Explorer's real `Shell_TrayWnd`,
//!   so Explorer keeps exactly the state it would have without Vitrobar. Appbar and other
//!   non-tray copies get Explorer's answer; tray copies get ours (from [`TrayModel::apply`]).
//! - When the host goes away, its window is destroyed first and `TaskbarCreated` is then
//!   posted to every top-level window, so every app registers its icons again with whatever
//!   `Shell_TrayWnd` is left: Explorer's.
//!
//! The window lives on its own thread. That thread never waits on the UI thread: it posts
//! [`WM_VITRO_TRAY`] (a boxed [`TrayMessage`]) and shares the model behind a mutex that is
//! never held across a call out of the process.
//!
//! Icon handles in the payload belong to the sender, which may destroy them as soon as
//! `Shell_NotifyIcon` returns, so each one is copied (`CopyIcon`) while the sender waits; the
//! model then holds the copy, owned here ([`OwnedIcon`]) and destroyed when nothing uses it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread::JoinHandle;

use vitro_core::tray_model::{tray_key, TrayIcon, TrayModel};
use vitro_core::tray_wire::{parse_tray_copydata, TrayMessage, TrayOp, TRAY_COPYDATA_ID};
use windows::core::{w, Error, Result, BOOL, PCWSTR};
use windows::Win32::Foundation::{E_FAIL, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, ChangeWindowMessageFilterEx, CopyIcon, CreateWindowExW,
    DefWindowProcW, DestroyIcon, DestroyWindow, DispatchMessageW, EnumWindows, GetMessageW,
    GetWindow, GetWindowThreadProcessId, InSendMessageEx, IsWindow, PostMessageW, PostQuitMessage,
    RegisterClassW, RegisterWindowMessageW, SendMessageTimeoutW, SetWindowPos, TranslateMessage,
    GW_OWNER, HICON, HWND_BOTTOM, HWND_TOPMOST, MSG, MSGFLT_ALLOW, SMTO_ABORTIFHUNG, SMTO_NORMAL,
    SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, WM_APP, WM_COMMAND, WM_CONTEXTMENU,
    WM_COPYDATA, WM_DESTROY, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
    WM_RBUTTONDOWN, WM_RBUTTONUP, WM_USER, WNDCLASSW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::stock_taskbar::{explorer_tray_window, taskbar_created_msg};
use crate::window_props::exe_of_window;

/// Posted to the notify window with a `Box<TrayMessage>` in `lParam` (see
/// [`take_tray_message`]) after each tray message that the model accepted.
pub const WM_VITRO_TRAY: u32 = WM_APP + 2;

/// `NIN_SELECT` (`WM_USER + 0`): a version-4 icon was selected with the mouse.
pub const NIN_SELECT: u32 = WM_USER;

/// How long a forwarded message may wait for Explorer.
const FORWARD_TIMEOUT_MS: u32 = 2000;
/// How long the shutdown waits for the host thread to destroy its window.
const SHUTDOWN_TIMEOUT_MS: u32 = 2000;

/// The class name every `Shell_NotifyIcon` caller looks for.
const TRAY_CLASS: PCWSTR = w!("Shell_TrayWnd");

/// What the user did to an icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayClick {
    Left,
    Right,
    Double,
    Hover,
}

/// An icon handle copied from a sender; destroyed when the last reference goes.
#[derive(Debug)]
pub struct OwnedIcon(HICON);

// SAFETY: an HICON is a process-wide USER handle with no thread affinity; this type only
// draws from it (CopyIcon / DrawIconEx) and destroys it once, in Drop.
unsafe impl Send for OwnedIcon {}
// SAFETY: as above; shared references only read the handle value.
unsafe impl Sync for OwnedIcon {}

impl OwnedIcon {
    /// The 32-bit handle value (USER handles are 32-bit significant), as the model stores it.
    pub fn handle(&self) -> u32 {
        self.0 .0 as usize as u32
    }
}

impl Drop for OwnedIcon {
    fn drop(&mut self) {
        // SAFETY: the icon came from CopyIcon and is destroyed exactly once.
        unsafe {
            let _ = DestroyIcon(self.0);
        }
    }
}

fn copy_icon(v: u32) -> Option<OwnedIcon> {
    if v == 0 {
        return None;
    }
    // A 32-bit handle, sign-extended (as the 64-bit system does).
    let h = HICON(v as i32 as isize as *mut c_void);
    // SAFETY: CopyIcon validates the handle and fails on anything that is not an icon; the
    // copy is ours.
    unsafe { CopyIcon(h) }.ok().map(OwnedIcon)
}

/// One icon as the app sees it: the model's record plus the image to draw.
#[derive(Clone, Debug)]
pub struct TrayEntry {
    pub icon: TrayIcon,
    /// Our copy of the icon's image (`None` when it has none or it could not be copied).
    pub image: Option<Arc<OwnedIcon>>,
}

#[derive(Default)]
struct Shared {
    model: TrayModel,
    /// Icon copies by handle value; only those the model references are kept.
    owned: HashMap<u32, Arc<OwnedIcon>>,
}

impl Shared {
    fn prune(&mut self) {
        let model = &self.model;
        self.owned
            .retain(|h, _| model.icons().iter().any(|i| i.hicon == *h));
    }
}

fn lock(s: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    s.lock().unwrap_or_else(|e| e.into_inner())
}

/// The host's window (0 = none), for [`shutdown_global`] from any thread.
static TRAY_HWND: AtomicIsize = AtomicIsize::new(0);
/// Explorer's `Shell_TrayWnd` as last seen by the host (to tell an Explorer restart).
static LAST_EXPLORER: AtomicIsize = AtomicIsize::new(0);

/// The message that asks the host thread to destroy its window; `wParam` must be
/// [`quit_cookie`] so no other process can close it.
fn quit_msg() -> u32 {
    static ID: OnceLock<u32> = OnceLock::new();
    // SAFETY: plain registration call with a static string.
    *ID.get_or_init(|| unsafe { RegisterWindowMessageW(w!("VitrobarTrayHostQuit")) })
}

fn quit_cookie() -> usize {
    static COOKIE: OnceLock<usize> = OnceLock::new();
    *COOKIE.get_or_init(|| {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as usize)
            .unwrap_or(0);
        let a = &COOKIE as *const _ as usize;
        (t ^ a.rotate_left(17)) | 1
    })
}

struct HostCtx {
    notify: isize,
    shared: Arc<Mutex<Shared>>,
}

thread_local! {
    /// Set on the host thread only.
    static HOST: RefCell<Option<HostCtx>> = const { RefCell::new(None) };
}

fn own_pid() -> u32 {
    // SAFETY: plain query.
    unsafe { GetCurrentProcessId() }
}

fn pid_of(h: HWND) -> u32 {
    let mut pid = 0u32;
    // SAFETY: out-pointer to a local; the call does not message the window.
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    pid
}

/// Sends (or posts, if it was posted to us) one message on to Explorer's `Shell_TrayWnd`
/// and returns Explorer's answer (0 when Explorer is missing, hung or slow).
///
/// # Safety
/// `lparam` must be valid for `msg` as received by the host window (for `WM_COPYDATA`, a
/// `COPYDATASTRUCT` that the system keeps valid while the message is being handled).
unsafe fn forward(own: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let Some(target) = explorer_tray_window() else {
        return LRESULT(0);
    };
    if target == own || pid_of(target) == own_pid() {
        return LRESULT(0);
    }
    // SAFETY: plain query about the message being handled. Anything but ISMEX_NOSEND (0) was
    // sent (SendMessage, SendNotifyMessage, SendMessageCallback).
    let sent = unsafe { InSendMessageEx(None) } != 0 || msg == WM_COPYDATA;
    if sent {
        let mut result = 0usize;
        // SAFETY: the arguments are the ones we received, valid for the duration of this
        // call (the system marshals WM_COPYDATA again for Explorer); `result` is a local.
        // ABORTIFHUNG plus the timeout bound the wait; without SMTO_BLOCK, messages sent to
        // this thread meanwhile are still handled (no deadlock with a sender that Explorer
        // messages back).
        let ok = unsafe {
            SendMessageTimeoutW(
                target,
                msg,
                wparam,
                lparam,
                SMTO_ABORTIFHUNG | SMTO_NORMAL,
                FORWARD_TIMEOUT_MS,
                Some(&mut result),
            )
        };
        if ok.0 == 0 {
            return LRESULT(0);
        }
        LRESULT(result as isize)
    } else {
        // SAFETY: posting carries the values on unchanged, as they came to us.
        unsafe {
            let _ = PostMessageW(Some(target), msg, wparam, lparam);
        }
        LRESULT(0)
    }
}

/// Applies one tray payload to the model; returns the answer for the sender.
fn handle_tray(data: &[u8]) -> bool {
    let Ok(mut m) = parse_tray_copydata(data) else {
        return false;
    };
    let copy = m.hicon.and_then(copy_icon);
    if let Some(h) = m.hicon {
        m.hicon = match &copy {
            Some(c) => Some(c.handle()),
            // An explicit "no icon", or an Add whose icon is unusable: drawn as a placeholder.
            None if h == 0 || m.op == TrayOp::Add => Some(0),
            // A Modify with an unusable handle keeps the previous image.
            None => None,
        };
    }
    let Some((notify, shared)) = HOST.with(|h| {
        h.borrow()
            .as_ref()
            .map(|c| (c.notify, Arc::clone(&c.shared)))
    }) else {
        return false;
    };
    let changed = {
        let mut s = lock(&shared);
        if let Some(c) = copy {
            let c = Arc::new(c);
            s.owned.insert(c.handle(), c);
        }
        let changed = s.model.apply(m.clone());
        s.prune();
        changed
    };
    // A null window would make PostMessage post to this thread's own queue (and leak the box).
    if changed && notify != 0 {
        let raw = Box::into_raw(Box::new(m));
        // SAFETY: the box is handed over to the message; if posting fails it is reclaimed
        // here, otherwise the receiver reclaims it with `take_tray_message` exactly once.
        unsafe {
            if PostMessageW(
                Some(HWND(notify as *mut c_void)),
                WM_VITRO_TRAY,
                WPARAM(0),
                LPARAM(raw as isize),
            )
            .is_err()
            {
                drop(Box::from_raw(raw));
            }
        }
    }
    changed
}

/// Reclaims the [`TrayMessage`] of a [`WM_VITRO_TRAY`].
///
/// # Safety
/// `lparam` must come from a `WM_VITRO_TRAY` posted by the host, each reclaimed once.
pub unsafe fn take_tray_message(lparam: LPARAM) -> TrayMessage {
    // SAFETY: per the contract, a Box<TrayMessage> leaked by `handle_tray`.
    *unsafe { Box::from_raw(lparam.0 as *mut TrayMessage) }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_COPYDATA => {
            let cds = lparam.0 as *const COPYDATASTRUCT;
            if cds.is_null() {
                return LRESULT(0);
            }
            // SAFETY: for WM_COPYDATA the system passes a COPYDATASTRUCT and `cbData` bytes at
            // `lpData`, both valid (read-only) until this message returns. Only `cbData`
            // bytes are read, and nothing is kept past the call.
            let (id, data) = unsafe {
                let cd = &*cds;
                let data: &[u8] = if cd.lpData.is_null() || cd.cbData == 0 {
                    &[]
                } else {
                    std::slice::from_raw_parts(cd.lpData as *const u8, cd.cbData as usize)
                };
                (cd.dwData, data)
            };
            if id == TRAY_COPYDATA_ID {
                // Copy the icon and apply first, while the sender waits and its HICON lives.
                let ours = handle_tray(data);
                // SAFETY: the message as we received it.
                unsafe { forward(hwnd, msg, wparam, lparam) };
                LRESULT(ours as isize)
            } else {
                // Appbar calls (0), icon-rect queries (3) and anything newer: Explorer answers.
                // SAFETY: the message as we received it.
                unsafe { forward(hwnd, msg, wparam, lparam) }
            }
        }
        WM_DESTROY => {
            let _ =
                TRAY_HWND.compare_exchange(hwnd.0 as isize, 0, Ordering::SeqCst, Ordering::SeqCst);
            HOST.with(|h| h.borrow_mut().take());
            // SAFETY: plain call; ends the host thread's loop.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        m if m == quit_msg() && wparam.0 == quit_cookie() => {
            // SAFETY: destroying our own window on its own thread.
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(1)
        }
        m if m == taskbar_created_msg() => {
            // Our own broadcasts never reach this process, so this comes from Explorer (or
            // another shell). Only a new Explorer taskbar window means a restart: then go
            // back on top and ask every app to register its icons again (with us).
            let now = explorer_tray_window().map_or(0, |h| h.0 as isize);
            if LAST_EXPLORER.swap(now, Ordering::SeqCst) != now && now != 0 {
                raise(hwnd);
                broadcast_taskbar_created();
            }
            LRESULT(0)
        }
        // Messages meant for the taskbar itself. System messages (window lifetime, painting,
        // broadcasts such as WM_SETTINGCHANGE) are never forwarded: Explorer gets broadcasts
        // directly, and a second copy of registered (>= 0xC000) messages, nearly all
        // broadcasts, could make it act twice.
        WM_COMMAND | 0x0400..=0xBFFF => {
            // SAFETY: the message as we received it.
            unsafe { forward(hwnd, msg, wparam, lparam) }
        }
        // SAFETY: default handling with our own arguments.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn register_class() -> Result<()> {
    static RESULT: OnceLock<Result<()>> = OnceLock::new();
    RESULT
        .get_or_init(|| unsafe {
            // SAFETY: plain class registration with a static window procedure. The class
            // name is registered in this process only.
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: GetModuleHandleW(None)?.into(),
                lpszClassName: TRAY_CLASS,
                ..Default::default()
            };
            if RegisterClassW(&wc) == 0 {
                return Err(Error::from_thread());
            }
            Ok(())
        })
        .clone()
}

/// Puts `hwnd` (our tray window) back at the top of the topmost band, without waiting.
fn raise(hwnd: HWND) {
    // SAFETY: plain call on our own window; ASYNC so the caller never waits for its thread.
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS,
        );
    }
}

unsafe extern "system" fn post_taskbar_created(hwnd: HWND, data: LPARAM) -> BOOL {
    // SAFETY: plain queries; posting carries no pointers. `data` holds our process id.
    unsafe {
        let unowned = GetWindow(hwnd, GW_OWNER).is_err();
        if unowned && pid_of(hwnd) != data.0 as u32 {
            let _ = PostMessageW(Some(hwnd), taskbar_created_msg(), WPARAM(0), LPARAM(0));
        }
    }
    true.into()
}

/// Posts `TaskbarCreated` to every unowned top-level window of other processes (what
/// `HWND_BROADCAST` reaches), so apps register their tray icons again with the topmost
/// `Shell_TrayWnd`. Windows of this process are skipped, so the broadcast never loops back.
pub fn broadcast_taskbar_created() {
    // SAFETY: the callback only reads its lParam value and posts plain messages.
    unsafe {
        let _ = EnumWindows(Some(post_taskbar_created), LPARAM(own_pid() as isize));
    }
}

/// Destroys the host's window (on its thread) and then broadcasts `TaskbarCreated`, so every
/// app registers its icons with Explorer again. Safe from any thread, any number of times
/// (only the first call does anything). Used by the restore path.
pub fn shutdown_global() {
    let h = TRAY_HWND.swap(0, Ordering::SeqCst);
    if h != 0 {
        destroy_and_announce(HWND(h as *mut c_void));
    }
}

fn destroy_and_announce(hwnd: HWND) {
    // SAFETY: plain calls; the timeout bounds the wait for the host thread.
    unsafe {
        if IsWindow(Some(hwnd)).as_bool() {
            let _ = SendMessageTimeoutW(
                hwnd,
                quit_msg(),
                WPARAM(quit_cookie()),
                LPARAM(0),
                SMTO_ABORTIFHUNG | SMTO_NORMAL,
                SHUTDOWN_TIMEOUT_MS,
                None,
            );
        }
        if IsWindow(Some(hwnd)).as_bool() {
            // The host thread is stuck: at least stop being the first Shell_TrayWnd.
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_BOTTOM),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS,
            );
        }
    }
    broadcast_taskbar_created();
}

/// Vitrobar's `Shell_TrayWnd` and the icons it collected. One per process.
pub struct TrayHost {
    hwnd: HWND,
    shared: Arc<Mutex<Shared>>,
    thread: Option<JoinHandle<()>>,
}

impl TrayHost {
    /// Creates our `Shell_TrayWnd` on its own thread. Each tray message the model accepts is
    /// posted to `notify` as [`WM_VITRO_TRAY`]. Call [`TrayHost::announce`] next to collect
    /// the icons that already exist.
    pub fn start(notify: HWND) -> Result<Self> {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (tx, rx) = channel::<Result<isize>>();
        let notify = notify.0 as isize;
        let thread_shared = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("vitrobar-tray".into())
            .spawn(move || {
                let created = (|| -> Result<HWND> {
                    register_class()?;
                    // SAFETY: the class is registered; static strings. 0 x 0 and never shown.
                    let hwnd = unsafe {
                        CreateWindowExW(
                            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                            TRAY_CLASS,
                            PCWSTR::null(),
                            WS_POPUP,
                            0,
                            0,
                            0,
                            0,
                            None,
                            None,
                            Some(GetModuleHandleW(None)?.into()),
                            None,
                        )?
                    };
                    Ok(hwnd)
                })();
                let hwnd = match created {
                    Ok(h) => h,
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        return;
                    }
                };
                HOST.with(|h| {
                    *h.borrow_mut() = Some(HostCtx {
                        notify,
                        shared: thread_shared,
                    })
                });
                // SAFETY: plain calls on our own window. Lower-integrity senders (sandboxed
                // apps) may reach the taskbar with these, as they may reach Explorer's.
                unsafe {
                    let _ = ChangeWindowMessageFilterEx(hwnd, WM_COPYDATA, MSGFLT_ALLOW, None);
                    let _ = ChangeWindowMessageFilterEx(
                        hwnd,
                        taskbar_created_msg(),
                        MSGFLT_ALLOW,
                        None,
                    );
                }
                LAST_EXPLORER.store(
                    explorer_tray_window().map_or(0, |h| h.0 as isize),
                    Ordering::SeqCst,
                );
                raise(hwnd);
                TRAY_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
                let _ = tx.send(Ok(hwnd.0 as isize));
                let mut msg = MSG::default();
                // SAFETY: standard message loop of this thread; ends at WM_QUIT (or an error).
                unsafe {
                    while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
                HOST.with(|h| h.borrow_mut().take());
            })
            .map_err(|e| Error::new(E_FAIL, e.to_string()))?;
        let hwnd = rx
            .recv()
            .map_err(|_| Error::new(E_FAIL, "the tray host thread ended"))??;
        Ok(Self {
            hwnd: HWND(hwnd as *mut c_void),
            shared,
            thread: Some(thread),
        })
    }

    /// Our `Shell_TrayWnd`.
    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Asks every app to register its tray icons again (they come to us now).
    pub fn announce(&self) {
        broadcast_taskbar_created();
    }

    /// Goes back to the top of the topmost band, so `FindWindow` keeps finding us before
    /// Explorer's window. Never waits.
    pub fn reassert_topmost(&self) {
        raise(self.hwnd);
    }

    /// The icons, in the order they were added.
    pub fn entries(&self) -> Vec<TrayEntry> {
        let s = lock(&self.shared);
        s.model
            .icons()
            .iter()
            .map(|i| TrayEntry {
                icon: i.clone(),
                image: s.owned.get(&i.hicon).cloned(),
            })
            .collect()
    }

    /// Drops the icons whose owner window is gone (the app died without removing them);
    /// returns how many.
    pub fn sweep(&self) -> usize {
        let mut s = lock(&self.shared);
        let dead: Vec<u32> = s
            .model
            .icons()
            .iter()
            .map(|i| i.id.hwnd)
            .filter(|&h| {
                // SAFETY: plain query; any value is acceptable.
                !unsafe { IsWindow(Some(HWND(h as i32 as isize as *mut c_void))) }.as_bool()
            })
            .collect();
        let mut n = 0;
        for h in dead {
            n += s.model.remove_owner(h);
        }
        if n > 0 {
            s.prune();
        }
        n
    }

    /// Delivers a click (or hover) to the icon's owner as the stock taskbar does.
    /// `cursor_px` is the cursor in screen pixels.
    pub fn forward_click(icon: &TrayIcon, kind: TrayClick, cursor_px: (i32, i32)) {
        let owner = HWND(icon.id.hwnd as i32 as isize as *mut c_void);
        let posts = click_messages(icon, kind, cursor_px);
        if posts.is_empty() {
            return;
        }
        if kind != TrayClick::Hover {
            // The owner usually shows a window or a menu: let it come to the front. We may,
            // because we just received the click.
            let pid = pid_of(owner);
            if pid != 0 {
                // SAFETY: plain call.
                let _ = unsafe { AllowSetForegroundWindow(pid) };
            }
        }
        for (wparam, lparam) in posts {
            // SAFETY: posting plain values (no pointers) to the icon's owner.
            unsafe {
                let _ = PostMessageW(
                    Some(owner),
                    icon.callback_msg,
                    WPARAM(wparam),
                    LPARAM(lparam),
                );
            }
        }
    }
}

impl Drop for TrayHost {
    fn drop(&mut self) {
        // Unless the restore path already did it: destroy, then broadcast.
        if TRAY_HWND
            .compare_exchange(self.hwnd.0 as isize, 0, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            destroy_and_announce(self.hwnd);
        }
        // SAFETY: plain query.
        let gone = !unsafe { IsWindow(Some(self.hwnd)) }.as_bool();
        if let Some(t) = self.thread.take() {
            if gone {
                let _ = t.join();
            }
        }
    }
}

fn makelong(lo: u32, hi: u32) -> u32 {
    (lo & 0xFFFF) | ((hi & 0xFFFF) << 16)
}

/// `(wParam, lParam)` of each callback message, in order, for a click on `icon`: what the
/// stock taskbar sends. Version 4 icons get the cursor in `wParam` and `(event, uid)` in
/// `lParam`, plus `NIN_SELECT` / `WM_CONTEXTMENU`; older ones get `uid` and the event.
pub fn click_messages(
    icon: &TrayIcon,
    kind: TrayClick,
    cursor_px: (i32, i32),
) -> Vec<(usize, isize)> {
    if icon.callback_msg == 0 {
        return Vec::new();
    }
    let v4 = icon.version >= 4;
    let events: &[u32] = match (kind, v4) {
        (TrayClick::Left, true) => &[WM_LBUTTONDOWN, WM_LBUTTONUP, NIN_SELECT],
        (TrayClick::Left, false) => &[WM_LBUTTONDOWN, WM_LBUTTONUP],
        (TrayClick::Right, true) => &[WM_RBUTTONDOWN, WM_RBUTTONUP, WM_CONTEXTMENU],
        (TrayClick::Right, false) => &[WM_RBUTTONDOWN, WM_RBUTTONUP],
        (TrayClick::Double, _) => &[WM_LBUTTONDBLCLK, WM_LBUTTONUP],
        (TrayClick::Hover, _) => &[WM_MOUSEMOVE],
    };
    let (x, y) = cursor_px;
    events
        .iter()
        .map(|&ev| {
            if v4 {
                (
                    makelong(x as u32, y as u32) as usize,
                    makelong(ev, icon.id.uid) as i32 as isize,
                )
            } else {
                (icon.id.uid as usize, ev as isize)
            }
        })
        .collect()
}

/// The icon's stable identity (what `Settings.tray_promoted` lists): its GUID as
/// `{xxxxxxxx-...}` lowercase hex, else `"<exe path lowercase>|<uid>"` with the exe of the
/// process owning the icon's window.
pub fn tray_identity_key(icon: &TrayIcon) -> String {
    let exe = match icon.id.guid {
        Some(_) => String::new(),
        None => {
            exe_of_window(HWND(icon.id.hwnd as i32 as isize as *mut c_void)).unwrap_or_default()
        }
    };
    tray_key(icon.id.guid.as_ref(), &exe, icon.id.uid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vitro_core::tray_wire::TrayIconId;

    fn icon(version: u32, callback: u32) -> TrayIcon {
        TrayIcon {
            id: TrayIconId {
                hwnd: 0x1234,
                uid: 7,
                guid: None,
            },
            callback_msg: callback,
            hicon: 0,
            tip: String::new(),
            hidden: false,
            version,
        }
    }

    #[test]
    fn version_4_packs_cursor_and_event_with_uid() {
        let m = click_messages(&icon(4, 0x8001), TrayClick::Left, (100, 200));
        let wp = (100 | (200 << 16)) as usize;
        assert_eq!(
            m,
            vec![
                (wp, (WM_LBUTTONDOWN | (7 << 16)) as isize),
                (wp, (WM_LBUTTONUP | (7 << 16)) as isize),
                (wp, (NIN_SELECT | (7 << 16)) as isize),
            ]
        );
        let r = click_messages(&icon(4, 0x8001), TrayClick::Right, (1, 2));
        assert_eq!(r.last().unwrap().1, (WM_CONTEXTMENU | (7 << 16)) as isize);
        // Negative coordinates (monitor left of the primary) keep their low 16 bits.
        let n = click_messages(&icon(4, 1), TrayClick::Hover, (-1, 5));
        assert_eq!(n[0].0, (0xFFFF | (5 << 16)) as usize);
    }

    #[test]
    fn older_versions_get_uid_and_event() {
        let m = click_messages(&icon(0, 0x8001), TrayClick::Right, (100, 200));
        assert_eq!(
            m,
            vec![(7, WM_RBUTTONDOWN as isize), (7, WM_RBUTTONUP as isize)]
        );
        let d = click_messages(&icon(3, 0x8001), TrayClick::Double, (0, 0));
        assert_eq!(d[0], (7, WM_LBUTTONDBLCLK as isize));
    }

    #[test]
    fn no_callback_no_messages() {
        assert!(click_messages(&icon(4, 0), TrayClick::Left, (0, 0)).is_empty());
    }

    #[test]
    fn guid_key_needs_no_process() {
        let mut i = icon(4, 1);
        i.id.guid = Some([0u8; 16]);
        assert_eq!(
            tray_identity_key(&i),
            "{00000000-0000-0000-0000-000000000000}"
        );
    }

    #[test]
    fn owned_icon_copies_and_reports_handle() {
        use windows::Win32::UI::WindowsAndMessaging::{LoadIconW, IDI_APPLICATION};
        // SAFETY: a shared system icon; we only copy it.
        let sys = unsafe { LoadIconW(None, IDI_APPLICATION) }.unwrap();
        let c = copy_icon(sys.0 as usize as u32).expect("system icon copies");
        assert_ne!(c.handle(), 0);
        assert!(copy_icon(0).is_none());
        assert!(copy_icon(0x7FFF_FFF0).is_none(), "not an icon");
    }
}
