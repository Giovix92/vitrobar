//! The bar's right-click menus: glass menus ([`MenuView`], the same material as the other
//! flyouts) shown modally by a nested message loop, returning the chosen command.
//!
//! The loop dispatches everything that is not the menu's own input, so the notify window
//! keeps recording its inbox as it did under the native menu. The menu ends on a choice,
//! Esc, a click anywhere outside it, losing the activation (Alt+Tab, the Win key) or
//! `WM_QUIT` (which is posted again afterwards).

use std::cell::Cell;

use vitro_core::i18n::{tr, Key, Lang};
use vitro_core::menu_model::{step, Entry, NavKey};
use vitro_core::settings::Settings;
use vitro_core::style::MaterialStyle;
use vitro_render::compositor::Comp;
use vitro_render::flyout::take_deactivated_of;
use vitro_render::menu_view::MenuView;
use vitro_render::panel::{note_deactivated, PANEL_CLASS};
use vitro_render::surfaces::Painter;
use vitro_win::power::PowerAction;

use crate::notify::keys_pending;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_DOWN, VK_END,
    VK_ESCAPE, VK_HOME, VK_RETURN, VK_SPACE, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetClassNameW, GetForegroundWindow, GetMessageW, PostMessageW,
    PostQuitMessage, SetForegroundWindow, TranslateMessage, EVENT_SYSTEM_FOREGROUND, MSG,
    WINEVENT_OUTOFCONTEXT, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MOUSEMOVE,
    WM_NCLBUTTONDOWN, WM_NCRBUTTONDOWN, WM_NULL, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSCHAR,
    WM_SYSCOMMAND, WM_SYSKEYDOWN,
};

/// `WM_MOUSELEAVE` (it lives in the Controls module, which nothing else here needs).
const WM_MOUSELEAVE: u32 = 0x02A3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuChoice {
    Pin,
    Unpin,
    NewWindow,
    CloseAll,
    /// Opens the settings window (every look-and-behaviour setting lives there).
    OpenSettings,
    ToggleStartWithWindows,
    Exit,
    PinToStart,
    UnpinFromStart,
    OpenFileLocation,
}

/// The Start panel's power menu, in order.
const POWER: &[(PowerAction, Key)] = &[
    (PowerAction::Lock, Key::Lock),
    (PowerAction::Sleep, Key::Sleep),
    (PowerAction::Restart, Key::Restart),
    (PowerAction::ShutDown, Key::ShutDown),
    (PowerAction::SignOut, Key::SignOut),
    (PowerAction::SwitchUser, Key::SwitchUser),
];

// Command ids: 0 is "nothing chosen"; submenu entries are base + index.
const ID_PIN: u32 = 1;
const ID_UNPIN: u32 = 2;
const ID_NEW_WINDOW: u32 = 3;
const ID_CLOSE_ALL: u32 = 4;
const ID_START_WITH_WINDOWS: u32 = 5;
const ID_EXIT: u32 = 6;
const ID_PIN_TO_START: u32 = 7;
const ID_UNPIN_FROM_START: u32 = 8;
const ID_OPEN_LOCATION: u32 = 9;
const ID_SETTINGS: u32 = 10;
const ID_POWER: u32 = 0x600;

fn pick<T: Copy>(list: &[(T, Key)], base: u32, id: u32) -> Option<T> {
    let i = id.checked_sub(base)? as usize;
    list.get(i).map(|(v, _)| *v)
}

/// The choice behind a command id returned by the menu.
fn choice_for(id: u32) -> Option<MenuChoice> {
    Some(match id {
        ID_PIN => MenuChoice::Pin,
        ID_UNPIN => MenuChoice::Unpin,
        ID_NEW_WINDOW => MenuChoice::NewWindow,
        ID_CLOSE_ALL => MenuChoice::CloseAll,
        ID_START_WITH_WINDOWS => MenuChoice::ToggleStartWithWindows,
        ID_EXIT => MenuChoice::Exit,
        ID_PIN_TO_START => MenuChoice::PinToStart,
        ID_UNPIN_FROM_START => MenuChoice::UnpinFromStart,
        ID_OPEN_LOCATION => MenuChoice::OpenFileLocation,
        ID_SETTINGS => MenuChoice::OpenSettings,
        _ => return None,
    })
}

/// What the menus draw with: the app's compositor, painter and bar material, and the DPI of
/// the monitor the menu opens on.
pub struct MenuCtx<'a> {
    pub comp: &'a Comp,
    pub painter: &'a Painter,
    pub style: &'a MaterialStyle,
    pub dpi: u32,
}

fn cmd(id: u32, text: &str) -> Entry {
    Entry::Command {
        id,
        text: text.to_string(),
        checked: false,
    }
}

/// Mouse position of a client-area mouse message (the signed 16-bit halves of `lparam`).
fn client_pt(l: LPARAM) -> (i32, i32) {
    (
        i32::from(l.0 as u16 as i16),
        i32::from((l.0 >> 16) as u16 as i16),
    )
}

/// Whether `h` is one of our panels (Start, settings).
fn is_panel(h: HWND) -> bool {
    let mut buf = [0u16; 32];
    // SAFETY: plain query into a local buffer; a stale handle gives 0.
    let n = unsafe { GetClassNameW(h, &mut buf) }.max(0) as usize;
    String::from_utf16_lossy(&buf[..n]) == PANEL_CLASS
}

/// The command id at row `i` of `view`.
fn command_id(view: &MenuView, i: Option<usize>) -> Option<u32> {
    match view.entries().get(i?) {
        Some(Entry::Command { id, .. }) => Some(*id),
        _ => None,
    }
}

thread_local! {
    /// The open menu's window and whether the foreground moved away from it (set by
    /// [`on_foreground`]).
    static FG: Cell<(isize, bool)> = const { Cell::new((0, false)) };
}

unsafe extern "system" fn on_foreground(
    _: HWINEVENTHOOK,
    _: u32,
    hwnd: HWND,
    _: i32,
    _: i32,
    _: u32,
    _: u32,
) {
    FG.with(|f| {
        let (me, _) = f.get();
        if hwnd.0 as isize != me {
            f.set((me, true));
            // GetMessage does not return for a WinEvent callback: wake the menu's loop.
            // SAFETY: plain post to our own window.
            unsafe {
                let _ = PostMessageW(Some(HWND(me as *mut _)), WM_NULL, WPARAM(0), LPARAM(0));
            }
        }
    });
}

/// Watches the foreground for the menu's lifetime (a menu that could not take it would
/// otherwise never hear that the user went elsewhere); unhooked on drop.
struct ForegroundWatch(HWINEVENTHOOK);

impl ForegroundWatch {
    fn new(me: HWND) -> Self {
        FG.with(|f| f.set((me.0 as isize, false)));
        // SAFETY: an out-of-context hook with a static callback; it is delivered through
        // this thread's message loop and removed in Drop.
        Self(unsafe {
            SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_FOREGROUND,
                None,
                Some(on_foreground),
                0,
                0,
                WINEVENT_OUTOFCONTEXT,
            )
        })
    }

    /// The foreground moved to another window since the watch began.
    fn moved(&self) -> bool {
        FG.with(|f| f.get().1)
    }
}

impl Drop for ForegroundWatch {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: unhooks the hook this value created.
            unsafe {
                let _ = UnhookWinEvent(self.0);
            }
        }
        FG.with(|f| f.set((0, false)));
    }
}

/// Shows `entries` at `at` (screen px) and runs the menu until it ends: the chosen command's
/// id, or 0 when it was dismissed. `owner` is the window the menu belongs to; a panel (Start)
/// gets the foreground back afterwards.
fn run(owner: HWND, ctx: &MenuCtx, at: (i32, i32), entries: &[Entry]) -> u32 {
    let mut view = match MenuView::show(ctx.comp, ctx.painter, ctx.style, ctx.dpi, at, entries) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("vitrobar: the menu failed: {e}");
            return 0;
        }
    };
    let me = view.hwnd();
    let _ = take_deactivated_of(me);
    // Without the foreground nothing would tell us about a click elsewhere: take the mouse
    // instead, so that click reaches the menu.
    let captured = !view.activate();
    if captured {
        // SAFETY: plain call on our own window.
        unsafe {
            let _ = SetCapture(me);
        }
    }
    let watch = ForegroundWatch::new(me);
    // A key left pending before the menu opened is not a key pressed during it.
    let stale_keys = keys_pending();
    let (mut tracking, mut mouse_hot) = (false, false);
    let mut quit = None;
    let mut msg = MSG::default();
    let picked = loop {
        // SAFETY: the standard message loop on this thread; `msg` is a local.
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) }.0;
        if got == 0 {
            quit = Some(msg.wParam.0 as i32);
            break 0;
        }
        if got < 0 {
            break 0;
        }
        if msg.hwnd == me {
            match msg.message {
                WM_MOUSEMOVE => {
                    if !tracking {
                        let mut t = TRACKMOUSEEVENT {
                            cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                            dwFlags: TME_LEAVE,
                            hwndTrack: me,
                            dwHoverTime: 0,
                        };
                        // SAFETY: `t` is a sized local.
                        tracking = unsafe { TrackMouseEvent(&mut t) }.is_ok();
                    }
                    mouse_hot = true;
                    let hot = view.hit_px(client_pt(msg.lParam));
                    let _ = view.set_hot(hot);
                }
                WM_MOUSELEAVE => {
                    tracking = false;
                    if mouse_hot {
                        mouse_hot = false;
                        let _ = view.set_hot(None);
                    }
                }
                // Alt (and so Alt+F4) dismisses; the system never handles these on the menu.
                WM_SYSKEYDOWN => break 0,
                WM_SYSCHAR | WM_SYSCOMMAND => {}
                WM_LBUTTONUP | WM_RBUTTONUP => {
                    if let Some(id) = command_id(&view, view.hit_px(client_pt(msg.lParam))) {
                        break id;
                    }
                }
                WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
                    if !view.contains_px(client_pt(msg.lParam)) {
                        break 0;
                    }
                }
                WM_KEYDOWN => {
                    let key = msg.wParam.0 as u16;
                    let nav = [
                        (VK_UP.0, NavKey::Up),
                        (VK_DOWN.0, NavKey::Down),
                        (VK_HOME.0, NavKey::Home),
                        (VK_END.0, NavKey::End),
                    ]
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, n)| *n);
                    if let Some(n) = nav {
                        mouse_hot = false;
                        let next = step(view.entries(), view.hot(), n);
                        let _ = view.set_hot(next);
                    } else if key == VK_ESCAPE.0 {
                        break 0;
                    } else if key == VK_RETURN.0 || key == VK_SPACE.0 {
                        if let Some(id) = command_id(&view, view.hot()) {
                            break id;
                        }
                    }
                }
                _ => {
                    // SAFETY: dispatching a message we just received.
                    unsafe {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
            }
        } else {
            // SAFETY: dispatching a message we just received.
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            // A click on another window of ours (a second right-click on the bar).
            if matches!(
                msg.message,
                WM_LBUTTONDOWN
                    | WM_RBUTTONDOWN
                    | WM_MBUTTONDOWN
                    | WM_NCLBUTTONDOWN
                    | WM_NCRBUTTONDOWN
            ) {
                break 0;
            }
        }
        // A click elsewhere, Alt+Tab, the Win key. Only our own entry is taken: the overflow
        // flyout's deactivation stays queued for the app.
        if take_deactivated_of(me) || watch.moved() {
            break 0;
        }
        // The Win key: the hook swallowed it and left it for the app, which handles it
        // once the menu is gone (the hook gives up on a stalled UI after 500 ms).
        if !stale_keys && keys_pending() {
            break 0;
        }
    };
    drop(watch);
    // SAFETY: plain calls on windows of this process.
    unsafe {
        if captured {
            let _ = ReleaseCapture();
        }
        if is_panel(owner) {
            let fg = GetForegroundWindow();
            if fg == me || fg.is_invalid() || fg == owner {
                // Dismissed from inside: the panel gets back what the menu took.
                let _ = SetForegroundWindow(owner);
            } else {
                // Another app took the foreground: the panel was left for it (and was told
                // nothing while the menu was up).
                note_deactivated(owner);
            }
        }
    }
    drop(view);
    let _ = take_deactivated_of(me);
    if let Some(code) = quit {
        // SAFETY: posts again the quit this loop swallowed.
        unsafe { PostQuitMessage(code) };
    }
    picked
}

/// The menu of an app on the bar.
pub fn app_menu(
    ctx: &MenuCtx,
    owner: HWND,
    pt_px: (i32, i32),
    pinned: bool,
    has_windows: bool,
    lang: Lang,
) -> Option<MenuChoice> {
    let mut e = vec![cmd(ID_NEW_WINDOW, tr(lang, Key::NewWindow))];
    if pinned {
        e.push(cmd(ID_UNPIN, tr(lang, Key::Unpin)));
    } else {
        e.push(cmd(ID_PIN, tr(lang, Key::Pin)));
    }
    if has_windows {
        e.push(Entry::Separator);
        e.push(cmd(ID_CLOSE_ALL, tr(lang, Key::CloseAllWindows)));
    }
    choice_for(run(owner, ctx, pt_px, &e))
}

/// The menu of an app in the Start panel (a pin or an All-apps entry). `owner` is the panel,
/// which gets the foreground back afterwards.
pub fn start_item_menu(
    ctx: &MenuCtx,
    owner: HWND,
    pt_px: (i32, i32),
    in_start: bool,
    in_bar: bool,
    has_location: bool,
    lang: Lang,
) -> Option<MenuChoice> {
    let mut e = Vec::new();
    if in_start {
        e.push(cmd(ID_UNPIN_FROM_START, tr(lang, Key::UnpinFromStart)));
    } else {
        e.push(cmd(ID_PIN_TO_START, tr(lang, Key::PinToStart)));
    }
    if in_bar {
        e.push(cmd(ID_UNPIN, tr(lang, Key::Unpin)));
    } else {
        e.push(cmd(ID_PIN, tr(lang, Key::Pin)));
    }
    if has_location {
        e.push(Entry::Separator);
        e.push(cmd(ID_OPEN_LOCATION, tr(lang, Key::OpenFileLocation)));
    }
    choice_for(run(owner, ctx, pt_px, &e))
}

/// The power action behind a command id of the power menu.
fn power_for(id: u32) -> Option<PowerAction> {
    pick(POWER, ID_POWER, id).filter(|_| id & !0xFF == ID_POWER)
}

/// The Start panel's power menu (Lock, Sleep, Restart, Shut down, Sign out, Switch user).
pub fn power_menu(
    ctx: &MenuCtx,
    owner: HWND,
    pt_px: (i32, i32),
    lang: Lang,
) -> Option<PowerAction> {
    let mut e = Vec::new();
    for (i, (_, key)) in POWER.iter().enumerate() {
        if i == 2 {
            e.push(Entry::Separator);
        }
        e.push(cmd(ID_POWER + i as u32, tr(lang, *key)));
    }
    power_for(run(owner, ctx, pt_px, &e))
}

/// The bar's own menu: the settings window, "Start with Windows" (checked when on) and Exit.
pub fn bar_menu(
    ctx: &MenuCtx,
    owner: HWND,
    pt_px: (i32, i32),
    s: &Settings,
    lang: Lang,
) -> Option<MenuChoice> {
    choice_for(run(owner, ctx, pt_px, &bar_entries(s, lang)))
}

fn bar_entries(s: &Settings, lang: Lang) -> Vec<Entry> {
    vec![
        cmd(ID_SETTINGS, tr(lang, Key::SettingsEllipsis)),
        Entry::Separator,
        Entry::Command {
            id: ID_START_WITH_WINDOWS,
            text: tr(lang, Key::StartWithWindows).to_string(),
            checked: s.start_with_windows,
        },
        cmd(ID_EXIT, tr(lang, Key::ExitVitrobar)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_id_maps_back() {
        assert_eq!(choice_for(0), None, "dismissed");
        assert_eq!(choice_for(ID_PIN), Some(MenuChoice::Pin));
        assert_eq!(choice_for(ID_UNPIN), Some(MenuChoice::Unpin));
        assert_eq!(choice_for(ID_NEW_WINDOW), Some(MenuChoice::NewWindow));
        assert_eq!(choice_for(ID_CLOSE_ALL), Some(MenuChoice::CloseAll));
        assert_eq!(
            choice_for(ID_START_WITH_WINDOWS),
            Some(MenuChoice::ToggleStartWithWindows)
        );
        assert_eq!(choice_for(ID_EXIT), Some(MenuChoice::Exit));
        assert_eq!(choice_for(ID_PIN_TO_START), Some(MenuChoice::PinToStart));
        assert_eq!(
            choice_for(ID_UNPIN_FROM_START),
            Some(MenuChoice::UnpinFromStart)
        );
        assert_eq!(
            choice_for(ID_OPEN_LOCATION),
            Some(MenuChoice::OpenFileLocation)
        );
        for (i, (a, _)) in POWER.iter().enumerate() {
            assert_eq!(power_for(ID_POWER + i as u32), Some(*a));
            assert_eq!(choice_for(ID_POWER + i as u32), None);
        }
        assert_eq!(power_for(ID_POWER + 6), None);
        assert_eq!(power_for(0), None);
        assert_eq!(power_for(ID_EXIT), None);
        assert_eq!(choice_for(ID_SETTINGS), Some(MenuChoice::OpenSettings));
        assert_eq!(choice_for(0x100), None);
        assert_eq!(choice_for(0x900), None);
    }

    #[test]
    fn menus_build_in_both_languages() {
        // Building (not showing) exercises every item: 3 commands and a separator.
        for lang in [Lang::En, Lang::It] {
            let s = Settings::default();
            assert_eq!(bar_entries(&s, lang).len(), 4);
        }
    }
}
