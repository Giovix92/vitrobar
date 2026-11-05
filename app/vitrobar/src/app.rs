//! App state and the event-driven message loop (nothing redraws unless something happened).
//!
//! Timers: the minute clock tick, the tracker's coalescing timers, a one-shot GPU
//! recreation retry, the one-shot hover timers of the thumbnail strip, a one-shot that
//! hides the stock taskbar again after Explorer restarted and the one-shot tray tooltip
//! delay. Everything else is driven by window, shell, tray and input messages.
//!
//! The tray host ([`TrayHost`]) collects every app's notification-area icons. Icons listed
//! in `Settings.tray_promoted` sit in the bar; the rest live in the overflow, a glass grid
//! opened by the chevron. Clicks are handed to the icon's owner as the stock taskbar does.
//!
//! The stock taskbar is taken over right after the bar first appears (ruling R12: Explorer
//! applies auto-hide synchronously in ~0.5 s, which must not delay the bar) and given back
//! on every way out ([`StockGuard`]). The appbars and the tray host only start after the
//! take-over, in the same order as before.
//!
//! Every monitor has its own bar ([`MonitorBar`]): its windows, layout, scale (the monitor's
//! DPI / 96), app groups (pins, then the windows on that monitor and the current virtual
//! desktop) and appbar (the main bar window reserves the bar's strip of that monitor). A bar
//! hides while a full-screen app covers its monitor. Only the primary monitor's bar has the
//! tray area (chevron, tray icons, quick settings); the others show Start, their apps and
//! the clock. Display, DPI, work-area and session changes re-read the monitors; bars are
//! created, destroyed or laid out again only for monitors that actually changed
//! ([`App::reconcile_monitors`]).
//!
//! The Windows key ([`Keys`], `keys.rs`): a low-level keyboard hook on its own thread
//! reports a lone Windows-key tap (Start), Win+1…9 (the bar's apps) and Win+T / Win+B (the
//! bar's keyboard focus mode); every other shortcut stays with Windows. The actions apply
//! to the bar of the monitor showing the foreground window.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use vitro_core::clock::ms_until_next_minute;
use vitro_core::i18n::{lang_from_tag, tr, Key, Lang};
use vitro_core::image::BgraImage;
use vitro_core::key_hook::HookOutput;
use vitro_core::layout::{
    anchor_for, compute_layout, diff_monitors, metrics, AnchorTarget, BarLayout, LayoutInput, Rect,
};
use vitro_core::search::ResultKind;
use vitro_core::settings::{self, Pin, Position, Settings, StartStyle, ThemeChoice};
use vitro_core::settings_ui::ControlId;
use vitro_core::start_layout::{panel_layout, place_panel};
use vitro_core::style::{hover_style, material_style, HoverStyle, MaterialStyle, Rgba, Theme};
use vitro_core::tray_model::{split_promoted, TrayIcon};
use vitro_core::widgets::{on_pill_glyph, MusicAction};
use vitro_core::windows_model::{
    build_groups, click_action, AppGroup, ClickAction, GroupKey, Hwnd,
};
use vitro_render::bar_visual::{
    hover_timing, BarFocus, BarVisual, ChevronState, ItemState, TrayItemState, VisualEnv,
    TRAY_ICON_DIP,
};
use vitro_render::compositor::{init_compositor, Comp};
use vitro_render::device::{create_gpu_device, GpuDevice};
use vitro_render::flyout::{live_thumbnails, take_deactivated, Flyout, ThumbnailStrip};
use vitro_render::panel::{
    take_panel_clicks, take_panel_close_requests, take_panel_deactivated, take_panel_dpi_changes,
    Motion,
};
use vitro_render::settings_view::{SettingsLook, WINDOW_H_DIP, WINDOW_W_DIP};
use vitro_render::surfaces::{FontKind, Painter};
use vitro_render::tray_flyout::{grid_layout, Tooltip, TrayGrid};
use vitro_win::appbar::{is_fullscreen_on_rect, reserved_rect, AppBar};
use vitro_win::catalog::{bar_pin, file_location};
use vitro_win::desktops::Desktops;
use vitro_win::key_hook::inject_mask_key;
use vitro_win::launch::{
    activate_window, close_window, minimize_window, open_stock_start, request_close,
};
use vitro_win::monitors::{
    list_monitors, parse_monitor_spec, window_monitor, work_area_px, MonitorInfo,
};
use vitro_win::pins_import::{import_promoted_tray, import_windows_pins};
use vitro_win::system_theme::{read_system_theme, user_ui_language_tag, SystemTheme};
use vitro_win::time_format::clock_now;
use vitro_win::tray_host::{tray_identity_key, TrayClick, TrayEntry, TrayHost};
use vitro_win::window_tracker::{WindowEvent, WindowTracker};
use windows::core::{Error, Result, BOOL};
use windows::Win32::Foundation::{
    E_FAIL, HANDLE, HWND, LPARAM, WAIT_FAILED, WAIT_OBJECT_0, WPARAM,
};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Console::{
    SetConsoleCtrlHandler, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
};
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetDoubleClickTime, GetKeyState, VK_CONTROL, VK_ESCAPE, VK_F4, VK_MENU, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetCursorPos, GetForegroundWindow, GetMessagePos, GetWindowThreadProcessId,
    IsIconic, IsWindow, IsWindowVisible, KillTimer, MsgWaitForMultipleObjectsEx, PeekMessageW,
    PostMessageW, PostQuitMessage, PostThreadMessageW, SetForegroundWindow, SetTimer,
    TranslateMessage, MSG, MWMO_INPUTAVAILABLE, PM_REMOVE, QS_ALLINPUT, WM_CHAR, WM_KEYDOWN,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_NULL, WM_QUIT, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN,
};

use crate::bar_window::{
    create_bar_window, hit_test, mouse_pos, track_leave, BarWindow, Hit, MarginMode, PxRect,
};
use crate::calendar_panel::{CalendarPanel, Outcome as CalOutcome};
use crate::icon_loader::{IconLoader, IconSrc, Lookup, TrayImage, MAX_ICON_PX};
use crate::keys::{
    activation_ends_focus, first_in, launch_action, open_start_fresh, step, Counts, Focus, Keys,
    Step, Zone,
};
use crate::launcher::{Job, Launcher};
use crate::menus::{app_menu, bar_menu, power_menu, start_item_menu, MenuChoice, MenuCtx};
use crate::notify::{
    inbox_pending, take_inbox, Inbox, NotifyWindow, TIMER_CALENDAR_MIDNIGHT, TIMER_CLAUDE_MINUTE,
    TIMER_CLAUDE_STALE, TIMER_CLOCK, TIMER_GPU_RETRY, TIMER_HOVER_HIDE, TIMER_HOVER_SHOW,
    TIMER_IME, TIMER_MARGIN_COLLAPSE, TIMER_QUICK_BT, TIMER_QUICK_SCAN, TIMER_REASSERT,
    TIMER_START_CLOSED, TIMER_START_PREFETCH, TIMER_TRAY_TIP, WM_APP_TAKE_OVER,
};
use crate::quick::{Gfx, Outcome as QuickOutcome, QuickPanel};
use crate::settings_window::{apply_choice, Effect, SettingsAction, SettingsInput, SettingsWindow};
use crate::shell::{self, StockGuard};
use crate::start::{
    launch_job, same_app, Look, MenuItem, StartAction, StartData, StartInput, StartPanel,
};
use crate::widgets::{Claude, ClaudePanel, Music, MusicPanel, Outcome as MusicOutcome};
use crate::window_set::WindowSet;
use vitro_core::ime::ime_short_name;
use vitro_core::settings_ui::set_claude_hint;
use vitro_win::claude_usage::{self, Wrap};
use vitro_win::tray_host::{set_icon_rects, IconRects};

/// Command-line options for the bar process.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Print one `vitrobar-state {json}` line on stderr whenever the bar's content changes.
    pub debug_state: bool,
    /// The watchdog's pid (`--parent`); watched so a new watchdog can adopt the bar.
    pub parent: Option<u32>,
    /// Arguments handed on to a watchdog this bar starts (`--debug-state`, `--console`).
    pub pass_on: Vec<String>,
}

/// `WM_MOUSELEAVE` (CommCtrl.h); defined here to avoid pulling in the Controls feature.
const WM_MOUSELEAVE: u32 = 0x02A3;

const CLOCK_FONT_DIP: f32 = 12.0;
const CLOCK_PAD_DIP: f32 = 4.0;
const CLOCK_MIN_DIP: f32 = 64.0;
/// Padding each side of the input-language indicator's text.
const IME_PAD_DIP: f32 = 6.0;
/// After a click on the indicator, the new layout is read this much later.
const IME_REREAD_MS: u32 = 250;
const GPU_RETRY_MS: u32 = 2000;
/// Hovering an app opens its thumbnail strip after this long.
const HOVER_SHOW_MS: u32 = 400;
/// Leaving both the icon and the strip closes the strip after this long.
const HOVER_HIDE_MS: u32 = 300;
/// Highest point of the springy hover curve, cubic-bezier(0.3, 1.6, 0.5, 1): about 1.11 of
/// the distance to the target.
const SPRING_PEAK: f32 = 1.12;
/// After `TaskbarCreated`, hide the stock taskbar once more this long later (Explorer can
/// show it after its broadcast).
const REASSERT_MS: u32 = 1000;
/// Delay from the takeover to the background read of the Start app catalog.
const START_PREFETCH_MS: u32 = 3000;
/// Resting on a tray icon shows its tooltip after this long.
const TRAY_TIP_MS: u32 = 500;

/// Debug hook for live tests of the multi-monitor paths: names a file that, while it exists
/// and holds `left,top,right,bottom,dpi`, adds one made-up monitor to the real ones. It is
/// read on every display check (post `WM_DISPLAYCHANGE` to the notify window after editing
/// it). Honoured only together with `--debug-state` (ruling R13): an environment variable
/// alone never changes a normal run. Unset in normal use.
const FAKE_MONITOR_ENV: &str = "VITROBAR_FAKE_MONITOR";

/// The Start panel's rise and fade-out, and the full-screen grid's grow and shrink.
const START_OPEN_MS: u32 = 160;
const START_CLOSE_MS: u32 = 110;
const GRID_OPEN_MS: u32 = 180;
const GRID_CLOSE_MS: u32 = 130;
/// How long past its close animation a fading panel lives before the timer destroys it.
const START_CLOSED_SLACK_MS: u32 = 30;

/// Debug hook for live tests of the Start panel's power menu: `1` makes every power action a
/// dry run (recorded, never carried out). Honoured only together with `--debug-state`.
const POWER_DRY_RUN_ENV: &str = "VITROBAR_POWER_DRY_RUN";

/// The monitors bars belong on: the real ones (primary first), plus the debug hook's when
/// `debug_state` (`--debug-state`) is on.
fn current_monitors(debug_state: bool) -> Vec<MonitorInfo> {
    let mut list = list_monitors();
    let hook = std::env::var_os(FAKE_MONITOR_ENV).filter(|p| debug_state && !p.is_empty());
    if let Some(path) = hook {
        if let Some(m) = std::fs::read_to_string(path)
            .ok()
            .and_then(|s| parse_monitor_spec(&s))
        {
            list.push(m);
        }
    }
    list
}

/// What changes between the bars of `old` and `new` (both in bar order: the first one
/// carries the tray area).
#[derive(Debug, Default, PartialEq, Eq)]
struct ReconcilePlan {
    /// Monitors that get a new bar.
    added: Vec<u64>,
    /// Monitors whose bar goes away.
    removed: Vec<u64>,
    /// Monitors kept whose rectangle, DPI, primary flag or tray area changed: laid out anew.
    changed: Vec<u64>,
}

impl ReconcilePlan {
    fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }
}

fn plan_reconcile(old: &[MonitorInfo], new: &[MonitorInfo]) -> ReconcilePlan {
    let ids = |l: &[MonitorInfo]| l.iter().map(|m| m.id).collect::<Vec<_>>();
    let (added, removed) = diff_monitors(&ids(old), &ids(new));
    let changed = new
        .iter()
        .enumerate()
        .filter_map(|(k, m)| {
            let (j, was) = old.iter().enumerate().find(|(_, o)| o.id == m.id)?;
            (was != m || (j == 0) != (k == 0)).then_some(m.id)
        })
        .collect();
    ReconcilePlan {
        added,
        removed,
        changed,
    }
}

fn empty_layout() -> BarLayout {
    compute_layout(&LayoutInput {
        monitor: Rect {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
        },
        shape: Default::default(),
        position: Default::default(),
        icon_size: Default::default(),
        app_count: 0,
        tray_count: 0,
        clock_w: CLOCK_MIN_DIP,
        reserve_floating: false,
        tray_area: false,
        widgets: [0.0; 2],
        ime: 0.0,
    })
}

/// A tray icon's identity in the model: (owner window, uid, guid).
type TrayId = (u32, u32, Option<[u8; 16]>);

fn tray_id(i: &TrayIcon) -> TrayId {
    (i.id.hwnd, i.id.uid, i.id.guid)
}

/// Screen px of the cursor when the message just retrieved was posted (not where the
/// cursor is by the time it is handled).
fn message_pos() -> (i32, i32) {
    // SAFETY: plain query about the last message this thread retrieved.
    let v = unsafe { GetMessagePos() };
    (
        i32::from((v & 0xFFFF) as u16 as i16),
        i32::from((v >> 16) as u16 as i16),
    )
}

static UI_THREAD: AtomicU32 = AtomicU32::new(0);

unsafe extern "system" fn on_console_ctrl(ctrl_type: u32) -> BOOL {
    // Ctrl+C, Ctrl+Break, console close: ask the UI thread to quit cleanly.
    let tid = UI_THREAD.load(Ordering::SeqCst);
    // SAFETY: posting a message to a thread id carries no pointers.
    let posted = unsafe { PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0)).is_ok() };
    if [CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT].contains(&ctrl_type) {
        // The process ends soon after this returns: give the stock taskbar back from here.
        shell::restore_now();
    }
    posted.into()
}

/// Overrides the settings file (tests and trial runs that must not touch the real one).
const SETTINGS_ENV: &str = "VITROBAR_SETTINGS";

/// `%APPDATA%\Vitrobar\settings.toml`, or the file named by `VITROBAR_SETTINGS`.
pub(crate) fn settings_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(SETTINGS_ENV).filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("Vitrobar").join("settings.toml"))
}

/// First run: start from what the stock taskbar shows (lists already set are kept).
fn first_run_import(s: &mut Settings) {
    if s.pins.is_empty() {
        s.pins = import_windows_pins();
    }
    if s.tray_promoted.is_empty() {
        s.tray_promoted = import_promoted_tray();
    }
    s.first_run_done = true;
}

/// The settings' language, else the Windows display language.
fn ui_lang(s: &Settings) -> Lang {
    match s.language.as_deref().filter(|l| !l.is_empty()) {
        Some(tag) => lang_from_tag(tag),
        None => user_ui_language_tag().map_or(Lang::En, |t| lang_from_tag(&t)),
    }
}

/// A pin for an app group that is not pinned (from its key and its first window).
fn pin_for_group(g: &AppGroup, infos: &[vitro_core::windows_model::WindowInfo]) -> Option<Pin> {
    let first = g
        .windows
        .first()
        .and_then(|h| infos.iter().find(|w| w.hwnd == *h));
    let aumid = match &g.key {
        GroupKey::Aumid(a) => Some(a.clone()),
        _ => first.and_then(|w| w.aumid.clone()),
    };
    let path = first.and_then(|w| w.exe_path.clone()).or(match &g.key {
        GroupKey::Path(p) if !p.is_empty() => Some(p.clone()),
        _ => None,
    });
    if aumid.is_none() && path.is_none() {
        return None;
    }
    let name = path
        .as_deref()
        .and_then(|p| std::path::Path::new(p).file_stem())
        .map(|n| n.to_string_lossy().into_owned())
        .or_else(|| aumid.clone())
        .unwrap_or_default();
    Some(Pin {
        aumid,
        path,
        name,
        link: None,
    })
}

fn style_for(settings: &Settings, sys: &SystemTheme) -> MaterialStyle {
    let theme = match settings.theme {
        ThemeChoice::System => sys.theme,
        ThemeChoice::Light => Theme::Light,
        ThemeChoice::Dark => Theme::Dark,
    };
    material_style(settings.material, theme, sys.transparency)
}

fn rgb(c: u32) -> Rgba {
    Rgba((c >> 16) as u8, (c >> 8) as u8, c as u8, 255)
}

fn to_px(r: Rect, scale: f32) -> PxRect {
    let p = |v: f32| (v * scale).round() as i32;
    (p(r.x), p(r.y), p(r.right()), p(r.bottom()))
}

/// The quick button's rectangle with the full height of the bar surface it sits on, in
/// screen px; `None` when the bar shows no quick button.
fn quick_anchor(l: &BarLayout, scale: f32) -> Option<PxRect> {
    surface_anchor(l, AnchorTarget::Quick, scale)
}

/// The clock's rectangle with the full height of its bar surface, in screen px; `None` when
/// the bar shows no clock.
fn clock_anchor(l: &BarLayout, scale: f32) -> Option<PxRect> {
    surface_anchor(l, AnchorTarget::Clock, scale)
}

fn surface_anchor(l: &BarLayout, target: AnchorTarget, scale: f32) -> Option<PxRect> {
    let r = anchor_for(l, target);
    if r.w <= 0.0 {
        return None;
    }
    let mid = r.x + r.w / 2.0;
    let s = l.surfaces.iter().find(|s| mid >= s.x && mid < s.right())?;
    Some(to_px(
        Rect {
            x: r.x,
            y: s.y,
            w: r.w,
            h: s.h,
        },
        scale,
    ))
}

fn hwnd_of(h: Hwnd) -> HWND {
    HWND(h as usize as *mut _)
}

fn foreground() -> Option<Hwnd> {
    // SAFETY: plain query.
    let h = unsafe { GetForegroundWindow() };
    (!h.0.is_null()).then_some(h.0 as usize as Hwnd)
}

/// Whether `h` can be handed the foreground: a live, visible, uncloaked window.
fn can_take_foreground(h: HWND) -> bool {
    // SAFETY: plain queries; the attribute value is a local whose size is passed alongside.
    unsafe {
        if h.0.is_null()
            || !IsWindow(Some(h)).as_bool()
            || !IsWindowVisible(h).as_bool()
            // A minimized window would take the keys without showing them.
            || IsIconic(h).as_bool()
        {
            return false;
        }
        let mut cloaked = 0u32;
        let ok = DwmGetWindowAttribute(
            h,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut _,
            size_of::<u32>() as u32,
        );
        ok.is_err() || cloaked == 0
    }
}

/// `h` belongs to this process (a bar, the Start panel, a flyout). Read while it exists.
fn is_own_window(h: Option<Hwnd>) -> bool {
    let Some(h) = h else { return false };
    let mut pid = 0u32;
    // SAFETY: plain queries; a stale handle yields thread 0 and leaves `pid` at 0.
    unsafe {
        GetWindowThreadProcessId(hwnd_of(h), Some(&mut pid));
        pid != 0 && pid == GetCurrentProcessId()
    }
}

fn icon_sources(g: &AppGroup) -> Vec<IconSrc> {
    let mut v = match &g.key {
        GroupKey::Aumid(a) => vec![IconSrc::Aumid(a.clone())],
        GroupKey::Path(p) if !p.is_empty() => vec![IconSrc::Path(p.clone())],
        _ => Vec::new(),
    };
    if let Some(&h) = g.windows.first() {
        v.push(IconSrc::Window(h));
    }
    v
}

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn group_key_text(k: &GroupKey) -> String {
    match k {
        GroupKey::Aumid(a) => format!("aumid:{a}"),
        GroupKey::Path(p) => format!("path:{p}"),
        GroupKey::Window(h) => format!("hwnd:{h}"),
    }
}

fn json_rect(r: PxRect) -> String {
    format!("[{},{},{},{}]", r.0, r.1, r.2, r.3)
}

/// The open thumbnail strip.
struct Strip {
    // Field order = drop order: the thumbnails are unregistered before their window goes.
    thumbs: ThumbnailStrip,
    flyout: Flyout,
    /// The monitor whose bar it opened from.
    monitor: u64,
    group: GroupKey,
    windows: Vec<Hwnd>,
    anchor: PxRect,
}

/// The open tray overflow.
struct Overflow {
    // Field order = drop order: the grid's visuals go before their window.
    grid: TrayGrid,
    flyout: Flyout,
    /// The icon in each slot.
    ids: Vec<TrayId>,
    /// The cursor is over the flyout (tracked for WM_MOUSELEAVE).
    over: bool,
    /// Slot under a button press, and whether it is the right button.
    pressed: Option<(usize, bool)>,
    /// The foreground window when it opened: Esc gives the foreground back to it.
    fg_at_open: usize,
}

/// A tray icon under the cursor (bar or overflow): the tooltip's subject.
#[derive(Clone, Copy, PartialEq, Eq)]
struct TipTarget {
    id: TrayId,
    /// Screen px the tooltip sits next to.
    anchor: PxRect,
}

/// One bar surface on screen.
struct Bar {
    // Field order = drop order: the visual tree goes before its window.
    visual: BarVisual,
    window: BarWindow,
    shown: bool,
}

/// One monitor's bar.
struct MonitorBar {
    // Field order = drop order: the appbar is unregistered before its window goes; each
    // bar's visual tree goes before its window. Dropping the bar windows never ends the app
    // (`BarWindow`'s drop marks the destruction as ours).
    appbar: Option<AppBar>,
    bars: Vec<Bar>,
    info: MonitorInfo,
    groups: Vec<AppGroup>,
    layout: BarLayout,
    /// A full-screen app covers this monitor: the bar windows are hidden.
    fullscreen: bool,
    /// Carries the tray area (the first bar, the primary monitor's).
    tray: bool,
}

impl MonitorBar {
    fn new(info: MonitorInfo, tray: bool) -> Self {
        Self {
            appbar: None,
            bars: Vec::new(),
            info,
            groups: Vec::new(),
            layout: empty_layout(),
            fullscreen: false,
            tray,
        }
    }

    fn id(&self) -> u64 {
        self.info.id
    }

    fn scale(&self) -> f32 {
        self.info.dpi as f32 / 96.0
    }

    fn monitor_dip(&self) -> Rect {
        let (l, t, r, b) = self.info.rect_px;
        let s = self.scale();
        Rect {
            x: l as f32 / s,
            y: t as f32 / s,
            w: (r - l) as f32 / s,
            h: (b - t) as f32 / s,
        }
    }

    fn drop_appbar(&mut self) {
        if let Some(ab) = self.appbar.take() {
            shell::remove_appbar_hwnd(ab.hwnd());
        }
    }
}

impl Drop for MonitorBar {
    fn drop(&mut self) {
        self.drop_appbar();
    }
}

/// Changes collected while draining the message queue, applied once afterwards.
#[derive(Default)]
struct Dirty {
    windows: BTreeSet<Hwnd>,
    regroup: bool,
    theme: bool,
    clock: bool,
    icons: bool,
    /// Every bar must be redrawn (settings, menus, GPU recovery).
    visuals: bool,
    /// Only these monitors' bars must be redrawn (hover, presses, the tray).
    bars: BTreeSet<u64>,
    /// Check whether a full-screen app covers a bar's monitor.
    fullscreen: bool,
    /// Re-read the monitors and reconcile the bars with them.
    monitors: bool,
    /// The display configuration changed or the session came back: put the bar windows
    /// back in place and let every appbar claim its strip again.
    display: bool,
    /// Explorer may be showing one of its taskbars again: hide it.
    stock: bool,
}

struct App {
    // Field order = drop order: the tray host first (its Shell_TrayWnd goes and every app is
    // asked to register its icons with Explorer again; later shell calls go straight to
    // Explorer), the flyouts, the monitors' bars (each appbar unregistered before its window
    // goes), then the tracker (before its notify window), then the notify window (drains
    // queued events), then the GPU objects, the compositor, and last the stock taskbar guard
    // (the shell is restored after the appbars are gone). The keyboard hook goes before all
    // of it: nothing reaches a half-dropped app.
    keys: Keys,
    tray: Option<TrayHost>,
    tooltip: Option<Tooltip>,
    overflow: Option<Overflow>,
    strip: Option<Strip>,
    /// The open Start panel (its visuals and window go before the compositor and GPU).
    start: Option<StartPanel>,
    /// The Start panel that is fading out (no longer `start`: it takes no input). Dropped by
    /// `TIMER_START_CLOSED`, by the next `open_start` or by any close that must be immediate.
    start_closing: Option<StartPanel>,
    /// When `TIMER_START_CLOSED` may drop `start_closing`.
    start_closing_until: Instant,
    /// What the Start panel shows, cached across opens, and its worker threads.
    start_data: StartData,
    /// The open quick panel (its window goes before the compositor and GPU; the system
    /// services it opened go with it, P4).
    quick: Option<QuickPanel>,
    /// The open calendar panel (same rules as `quick`, no services).
    calendar: Option<CalendarPanel>,
    /// The music widget while it is enabled (it owns the media service, P4).
    music: Option<Music>,
    /// The music widget's open detail panel (same rules as `quick`).
    music_panel: Option<MusicPanel>,
    /// The Claude limits widget while it is enabled (it owns the folder watch, P4).
    claude: Option<Claude>,
    claude_panel: Option<ClaudePanel>,
    /// Turning the Claude widget on failed: the settings say so under its toggle.
    claude_failed: bool,
    /// The input-language indicator's text (`None` with a single keyboard layout) and its
    /// DIP width.
    ime_text: Option<String>,
    ime_w: f32,
    /// The open settings window (its visuals and window go before the compositor and GPU).
    settings_win: Option<SettingsWindow>,
    /// One per monitor, in [`current_monitors`] order: the first carries the tray area.
    monitors: Vec<MonitorBar>,
    tracker: WindowTracker,
    icons: IconLoader,
    launcher: Launcher,
    notify: NotifyWindow,
    desktops: Desktops,
    painter: Painter,
    gpu: GpuDevice,
    comp: Comp,
    stock: StockGuard,

    opts: Options,
    settings: Settings,
    /// Where settings changes are saved (`None`: nowhere, `APPDATA` is not set).
    settings_path: Option<PathBuf>,
    /// The monitor whose bar the cursor was last on: `hover`, `pressed`, `rpressed`,
    /// `tracking` and `show_pending` refer to its bar (app indices into its groups).
    cur: u64,
    sys: SystemTheme,
    style: MaterialStyle,
    hover_style: HoverStyle,
    windows: WindowSet,
    foreground: Option<Hwnd>,
    /// `foreground` is one of our own windows (read when it became the foreground).
    foreground_ours: bool,
    clock_text: String,
    clock_w: f32,
    hover: Option<Hit>,
    pressed: Option<Hit>,
    /// Bar window currently tracked for WM_MOUSELEAVE.
    tracking: Option<HWND>,
    /// App whose strip opens when TIMER_HOVER_SHOW fires.
    show_pending: Option<usize>,
    /// TIMER_HOVER_HIDE is armed.
    hide_pending: bool,
    /// TIMER_MARGIN_COLLAPSE is armed (some bar margin is lingering).
    collapse_pending: bool,
    /// The cursor is over the flyout (tracked for WM_MOUSELEAVE).
    over_flyout: bool,
    /// Thumbnail under the button press in the flyout.
    flyout_pressed: Option<HWND>,
    /// The close button / the thumbnail a middle press went down on.
    flyout_close_pressed: Option<HWND>,
    flyout_mid_pressed: Option<HWND>,
    /// A click on the hovered app: no strip opens until the cursor moves to another item.
    show_suppressed: bool,
    gpu_lost: bool,
    /// The stock taskbar was taken over (or that was tried): the appbars may reserve their
    /// strips and the tray host runs. False until the first frame is on screen (R12).
    shell_ready: bool,
    /// The minute timer could not be armed; retried on the next flush.
    clock_unarmed: bool,
    /// The tray icons as last read from the host (dead owners swept).
    tray_entries: Vec<TrayEntry>,
    /// Identity keys (`tray_identity_key`), cached per icon.
    tray_keys: HashMap<TrayId, String>,
    /// Indices into `tray_entries`: promoted (in the bar) and the overflow.
    tray_bar: Vec<usize>,
    tray_over: Vec<usize>,
    /// The tray icon under the cursor; TIMER_TRAY_TIP is armed for it while no tooltip shows.
    tip_target: Option<TipTarget>,
    /// A right-button press on a bar window, and the item under it (`Some(None)`: pressed on
    /// the empty bar). `None` when no right press started on the bar, so a right-up arriving
    /// from a drag that began elsewhere does nothing.
    rpressed: Option<Option<Hit>>,
    /// Screen px of the mouse message being handled (forwarded with clicks and hovers).
    msg_pt: (i32, i32),
    /// Last left click on a tray icon, to tell a double click.
    last_left: Option<(TrayId, Instant)>,
    last_dump: String,
    /// The high half of a character typed into the Start panel as two `WM_CHAR`s.
    surrogate: Option<u16>,
    /// Time stamp (`MSG::time`, the `GetTickCount` clock) of the input being handled: the
    /// Start panel's open time is measured from it.
    input_time: u32,
}

impl App {
    /// `stock` is dropped (and the shell restored) if creating the app fails.
    fn new(
        opts: Options,
        settings: Settings,
        settings_path: Option<PathBuf>,
        stock: StockGuard,
    ) -> Result<Self> {
        let comp = init_compositor()?;
        let gpu = create_gpu_device(&comp)?;
        let painter = Painter::new(&gpu)?;
        // Carry 11: digits, shaping and line breaking follow the Windows display language.
        painter.set_locale(&user_ui_language_tag().unwrap_or_else(|| "en-US".into()));
        let monitors: Vec<MonitorBar> = current_monitors(opts.debug_state)
            .into_iter()
            .enumerate()
            .map(|(k, m)| MonitorBar::new(m, k == 0))
            .collect();
        let cur = monitors
            .first()
            .ok_or_else(|| Error::new(E_FAIL, "no monitor"))?
            .id();
        let sys = read_system_theme();
        let notify = NotifyWindow::create()?;
        // Carry 10: after the UI thread's own CoInitializeEx.
        let desktops = Desktops::new()?;
        // Carry 6: on the thread that owns `notify` and pumps its messages.
        let tracker = WindowTracker::start(notify.hwnd)?;
        let icons =
            IconLoader::start(notify.hwnd).map_err(|e| Error::new(E_FAIL, e.to_string()))?;
        let launcher = Launcher::start(None).map_err(|e| Error::new(E_FAIL, e.to_string()))?;
        let mut app = Self {
            // Both started with the take-over, after the first frame (`finish_start`).
            keys: Keys::default(),
            tray: None,
            tooltip: None,
            overflow: None,
            strip: None,
            start: None,
            start_closing: None,
            start_closing_until: Instant::now(),
            start_data: StartData::new(),
            quick: None,
            calendar: None,
            music: None,
            music_panel: None,
            claude: None,
            claude_panel: None,
            claude_failed: false,
            ime_text: None,
            ime_w: 0.0,
            settings_win: None,
            monitors,
            tracker,
            icons,
            launcher,
            notify,
            desktops,
            painter,
            gpu,
            comp,
            stock,
            opts,
            style: style_for(&settings, &sys),
            hover_style: hover_style(settings.hover),
            settings,
            settings_path,
            cur,
            sys,
            windows: WindowSet::new(),
            foreground: foreground(),
            foreground_ours: false,
            clock_text: String::new(),
            clock_w: CLOCK_MIN_DIP,
            hover: None,
            pressed: None,
            tracking: None,
            show_pending: None,
            hide_pending: false,
            collapse_pending: false,
            over_flyout: false,
            flyout_pressed: None,
            flyout_close_pressed: None,
            flyout_mid_pressed: None,
            show_suppressed: false,
            gpu_lost: false,
            shell_ready: false,
            clock_unarmed: false,
            tray_entries: Vec::new(),
            tray_keys: HashMap::new(),
            tray_bar: Vec::new(),
            tray_over: Vec::new(),
            tip_target: None,
            rpressed: None,
            msg_pt: (0, 0),
            last_left: None,
            last_dump: String::new(),
            surrogate: None,
            input_time: 0,
        };
        app.tick_clock();
        app.read_ime();
        app.sync_music();
        app.sync_claude();
        // Show the bars before the (slower) first window enumeration.
        app.refresh()?;
        let all = app.tracker.enumerate();
        app.windows.load(&all, &app.desktops);
        app.regroup();
        for k in 0..app.monitors.len() {
            let on = is_fullscreen_on_rect(app.monitors[k].info.rect_px);
            app.set_fullscreen(k, on);
        }
        app.refresh()?;
        // The bars are on screen: the take-over follows as the next message, after the
        // compositor has committed this frame.
        // SAFETY: plain call; posting carries no pointer.
        unsafe {
            PostMessageW(
                Some(app.notify.hwnd),
                WM_APP_TAKE_OVER,
                WPARAM(0),
                LPARAM(0),
            )?;
        }
        Ok(app)
    }

    /// The second half of the start-up (R12): take the stock taskbar over, then reserve the
    /// bars' strips and start the tray host, in the order they had before the bar showed
    /// first. Runs once.
    fn finish_start(&mut self, dirty: &mut Dirty) {
        if self.shell_ready {
            return;
        }
        // Record + RunOnce first, then auto-hide and hide (all inside `take_over_now`).
        self.stock.take_over_now();
        self.shell_ready = true;
        // The bar works without it (no tray icons).
        self.tray = TrayHost::start(self.notify.hwnd)
            .inspect_err(|e| eprintln!("vitrobar: the tray host could not start: {e}"))
            .ok();
        // Every app registers its tray icons again, now with us (and, forwarded, Explorer).
        if let Some(t) = &self.tray {
            t.announce();
        }
        // Explorer applies the auto-hide change of the takeover asynchronously and can show
        // a secondary taskbar again meanwhile: hide once more a little later.
        self.arm_reassert();
        // The Windows key is ours from here on (the hook is installed only while the setting
        // is on).
        self.keys
            .start(self.notify.hwnd, self.settings.win_key_opens_start);
        // "Win, type, Enter" right after sign-in needs the catalog (a cold read takes many
        // seconds): read it once in the background, after the startup work has settled.
        // SAFETY: one-shot timer on our own window (killed when it fires).
        unsafe {
            SetTimer(
                Some(self.notify.hwnd),
                TIMER_START_PREFETCH,
                START_PREFETCH_MS,
                None,
            )
        };
        // Registers the appbars and draws the tray area.
        dirty.visuals = true;
    }

    /// Index of the bar of monitor `id`.
    fn mon_index(&self, id: u64) -> Option<usize> {
        self.monitors.iter().position(|m| m.id() == id)
    }

    /// The bar the cursor was last on.
    fn cur_bar(&self) -> Option<&MonitorBar> {
        self.monitors.iter().find(|m| m.id() == self.cur)
    }

    /// The groups of the bar the cursor was last on (what `Hit::App` indices refer to).
    fn cur_groups(&self) -> &[AppGroup] {
        self.cur_bar().map_or(&[], |m| &m.groups)
    }

    /// The bar with the tray area.
    fn tray_monitor(&self) -> Option<&MonitorBar> {
        self.monitors.iter().find(|m| m.tray)
    }

    fn compute_layout(&self, k: usize) -> BarLayout {
        let mb = &self.monitors[k];
        compute_layout(&LayoutInput {
            monitor: mb.monitor_dip(),
            shape: self.settings.shape,
            position: self.settings.position,
            icon_size: self.settings.icon_size,
            app_count: mb.groups.len(),
            tray_count: if mb.tray { self.tray_bar.len() } else { 0 },
            clock_w: self.clock_w,
            reserve_floating: self.settings.reserve_space_floating,
            tray_area: mb.tray,
            widgets: [
                self.music.as_ref().map_or(0.0, |m| m.width),
                self.claude.as_ref().map_or(0.0, |c| c.width),
            ],
            ime: self.ime_w,
        })
    }

    fn env(&self, k: usize) -> VisualEnv {
        VisualEnv {
            dpi: self.monitors[k].info.dpi,
            accent: rgb(self.settings.accent.unwrap_or(self.sys.accent)),
            animations: self.sys.animations,
            position: self.settings.position,
        }
    }

    /// Extra px beyond the bar's outer edge, (above, below), that a magnified icon can reach
    /// (spring overshoot included) at `scale`. Only Magnify grows icons past the edge.
    fn hover_margin(&self, scale: f32) -> (i32, i32) {
        let h = &self.hover_style;
        if h.neighbour_scale <= 1.0 {
            return (0, 0);
        }
        let m = metrics(self.settings.icon_size);
        let peak = 1.0 + (h.scale - 1.0) * SPRING_PEAK;
        // Icons sit centred in the bar, 1 DIP above centre; Magnify anchors them at the edge.
        let (near_edge, grown) = match self.settings.position {
            Position::Bottom => (m.bar_h / 2.0 + m.icon / 2.0 - 1.0, m.icon * peak),
            Position::Top => (
                m.bar_h - (m.bar_h / 2.0 - m.icon / 2.0 - 1.0),
                m.icon * peak,
            ),
        };
        let over = grown - near_edge;
        if over <= 0.0 {
            return (0, 0);
        }
        let px = ((over + 1.0) * scale).ceil() as i32;
        match self.settings.position {
            Position::Bottom => (px, 0),
            Position::Top => (0, px),
        }
    }

    /// Rebuilds every bar's groups from the window set (pins on every bar, windows on the
    /// bar of their monitor); returns the monitors whose groups changed. A window that moved
    /// to another monitor changes exactly the two bars involved.
    fn regroup(&mut self) -> Vec<u64> {
        let infos = self.windows.infos();
        let mut changed = Vec::new();
        for mb in &mut self.monitors {
            let groups = build_groups(&self.settings.pins, &infos, mb.id(), self.foreground);
            if groups != mb.groups {
                mb.groups = groups;
                changed.push(mb.id());
            }
        }
        if changed.contains(&self.cur) {
            // Hover/press indices may now point at a different app.
            if matches!(self.hover, Some(Hit::App(_))) {
                self.hover = None;
            }
            if matches!(self.pressed, Some(Hit::App(_))) {
                self.pressed = None;
            }
            if matches!(self.rpressed, Some(Some(Hit::App(_)))) {
                self.rpressed = None;
            }
        }
        changed
    }

    /// Re-reads the clock, re-measures its width and re-arms the one-shot minute timer.
    /// Returns true if the text changed.
    fn tick_clock(&mut self) -> bool {
        let (text, into_minute) = clock_now();
        // A few ms late rather than early, so the new minute is already there.
        let wait = ms_until_next_minute(into_minute) + 15;
        // SAFETY: timer on our own window; re-setting the id replaces the previous one.
        let id = unsafe { SetTimer(Some(self.notify.hwnd), TIMER_CLOCK, wait, None) };
        // On failure (out of timers / quota) the next flush tries again; any message wakes it.
        self.clock_unarmed = id == 0;
        if text == self.clock_text {
            return false;
        }
        let w = self
            .painter
            .measure(&text, FontKind::Text, CLOCK_FONT_DIP)
            .map(|(w, _)| w)
            .unwrap_or(CLOCK_MIN_DIP);
        self.clock_w = (w.ceil() + 2.0 * CLOCK_PAD_DIP).max(CLOCK_MIN_DIP);
        self.clock_text = text;
        true
    }

    /// The first `shown` app items of bar `k` (hover and press only on the cursor's bar).
    fn items(&mut self, k: usize, shown: usize) -> Vec<ItemState> {
        let mb = &self.monitors[k];
        let icon_px = ((metrics(self.settings.icon_size).icon * mb.scale()).round() as u32)
            .clamp(1, MAX_ICON_PX);
        let (hover, pressed) = if mb.id() == self.cur {
            (self.hover, self.pressed)
        } else {
            (None, None)
        };
        let mut items = Vec::with_capacity(shown);
        for (i, g) in mb.groups.iter().take(shown).enumerate() {
            let mut icon = None;
            for src in icon_sources(g) {
                match self.icons.get(&src, icon_px) {
                    Lookup::Ready(img) => {
                        icon = Some(Arc::clone(img));
                        break;
                    }
                    // Wait for this source before trying the next one.
                    Lookup::Pending => break,
                    Lookup::Failed => continue,
                }
            }
            items.push(ItemState {
                icon,
                dots: g.windows.len().min(3) as u8,
                active: g.active,
                attention: g.attention,
                hovered: hover == Some(Hit::App(i)),
                pressed: pressed == Some(Hit::App(i)),
            });
        }
        items
    }

    /// Whether some bar (or the tray) shows icons of `src`.
    fn bar_shows(&self, src: &IconSrc) -> bool {
        matches!(src, IconSrc::Tray(_))
            || self
                .monitors
                .iter()
                .any(|m| m.groups.iter().any(|g| icon_sources(g).contains(src)))
    }

    /// Keeps the icon cache bounded to what some bar (or the tray, or the open Start panel)
    /// still shows.
    fn trim_icons(&mut self) {
        let monitors = &self.monitors;
        let start = self.start.as_ref().map(|s| &s.icon_srcs);
        self.icons.trim(|src| {
            matches!(src, IconSrc::Tray(_))
                || start.is_some_and(|s| s.contains(src))
                || monitors
                    .iter()
                    .any(|m| m.groups.iter().any(|g| icon_sources(g).contains(src)))
        });
    }

    /// The tray bar's scale (1.0 if there is none).
    fn tray_scale(&self) -> f32 {
        self.tray_monitor().map_or(1.0, |m| m.scale())
    }

    /// DPI of the tray bar's monitor (for the overflow and the tooltip).
    fn tray_dpi(&self) -> u32 {
        self.tray_monitor().map_or(96, |m| m.info.dpi)
    }

    /// What a right-click menu opened at `pt` (screen px) is drawn with.
    fn menu_ctx(&self, pt: (i32, i32)) -> MenuCtx<'_> {
        let dpi = self
            .monitors
            .iter()
            .find(|m| {
                let r = m.info.rect_px;
                pt.0 >= r.0 && pt.0 < r.2 && pt.1 >= r.1 && pt.1 < r.3
            })
            .map_or_else(|| self.tray_dpi(), |m| m.info.dpi);
        MenuCtx {
            comp: &self.comp,
            painter: &self.painter,
            style: &self.style,
            dpi,
        }
    }

    /// Tray icons are drawn at this many px.
    fn tray_px(&self) -> u32 {
        ((TRAY_ICON_DIP * self.tray_scale()).round() as u32).clamp(1, MAX_ICON_PX)
    }

    /// Re-reads the tray from the host, first dropping icons whose owner died (so this runs
    /// whenever the tray is redrawn or the overflow opens), and splits it into the bar and
    /// the overflow.
    fn reload_tray(&mut self) {
        let Some(host) = &self.tray else { return };
        host.sweep();
        let entries = host.entries();
        let mut keys = Vec::with_capacity(entries.len());
        for e in &entries {
            let key = self
                .tray_keys
                .entry(tray_id(&e.icon))
                .or_insert_with(|| tray_identity_key(&e.icon));
            keys.push(key.clone());
        }
        self.tray_keys
            .retain(|id, _| entries.iter().any(|e| tray_id(&e.icon) == *id));
        let icons: Vec<TrayIcon> = entries.iter().map(|e| e.icon.clone()).collect();
        let (bar, over) = split_promoted(&icons, &keys, &self.settings.tray_promoted);
        // Images of icons that changed or went away are not needed any more.
        let live: Vec<u32> = entries
            .iter()
            .filter_map(|e| e.image.as_ref().map(|i| i.handle()))
            .collect();
        self.icons
            .forget(|src| matches!(src, IconSrc::Tray(t) if !live.contains(&t.0.handle())));
        self.tray_entries = entries;
        self.tray_bar = bar;
        self.tray_over = over;
        if matches!(self.hover, Some(Hit::Tray(i)) if i >= self.tray_bar.len()) {
            self.hover = None;
        }
        if matches!(self.pressed, Some(Hit::Tray(_))) {
            self.pressed = None;
        }
        if self.tray_over.is_empty() && matches!(self.hover, Some(Hit::Chevron)) {
            self.hover = None;
        }
    }

    /// Reads the foreground window's input language for the indicator (hidden with a single
    /// keyboard layout); true if the text or width changed.
    fn read_ime(&mut self) -> bool {
        let text = (vitro_win::ime::layout_count() > 1).then(|| {
            let (langid, name) = vitro_win::ime::foreground_language()
                .unwrap_or_else(|| vitro_win::ime::thread_language(0));
            ime_short_name(langid, &name)
        });
        let w = match &text {
            Some(t) => {
                let tw = self
                    .painter
                    .measure(t, FontKind::Text, CLOCK_FONT_DIP)
                    .map(|(w, _)| w.ceil())
                    .unwrap_or(24.0);
                (tw + 2.0 * IME_PAD_DIP).round()
            }
            None => 0.0,
        };
        let changed = text != self.ime_text || w != self.ime_w;
        self.ime_text = text;
        self.ime_w = w;
        changed
    }

    /// Tells the tray host where bar `k` (the tray bar) draws each icon, for apps that ask
    /// with `Shell_NotifyIconGetRect`.
    fn publish_icon_rects(&self, k: usize) {
        let mb = &self.monitors[k];
        let scale = mb.scale();
        let promoted = self
            .tray_bar
            .iter()
            .zip(&mb.layout.tray)
            .filter_map(|(&i, r)| Some((self.tray_entries.get(i)?.icon.id, to_px(*r, scale))))
            .collect();
        let overflowed = self
            .tray_over
            .iter()
            .filter_map(|&i| Some(self.tray_entries.get(i)?.icon.id))
            .collect();
        let chev = mb.layout.overflow_chevron;
        let chevron = (!self.tray_over.is_empty() && chev.w > 0.0).then(|| to_px(chev, scale));
        set_icon_rects(IconRects {
            promoted,
            overflowed,
            chevron,
        });
    }

    /// The loaded image of tray entry `idx` (requests it if needed).
    fn tray_image(&mut self, idx: usize) -> Option<Arc<BgraImage>> {
        let px = self.tray_px();
        let img = self.tray_entries.get(idx)?.image.clone()?;
        match self.icons.get(&IconSrc::Tray(TrayImage(img)), px) {
            Lookup::Ready(i) => Some(Arc::clone(i)),
            _ => None,
        }
    }

    fn tray_items(&mut self) -> Vec<TrayItemState> {
        let bar = self.tray_bar.clone();
        let items = bar
            .iter()
            .enumerate()
            .map(|(i, &idx)| TrayItemState {
                icon: self.tray_image(idx),
                hovered: self.hover == Some(Hit::Tray(i)),
                pressed: self.pressed == Some(Hit::Tray(i))
                    || self.rpressed == Some(Some(Hit::Tray(i))),
            })
            .collect();
        // Overflow icons load ahead, so the overflow opens with its images.
        for idx in self.tray_over.clone() {
            let _ = self.tray_image(idx);
        }
        items
    }

    fn chevron_state(&self) -> ChevronState {
        ChevronState {
            shown: !self.tray_over.is_empty(),
            hovered: self.hover == Some(Hit::Chevron),
            pressed: self.pressed == Some(Hit::Chevron),
            open: self.overflow.is_some(),
        }
    }

    /// Lays out and redraws every bar (only what changed in each). Windows are created and
    /// moved as needed.
    fn refresh(&mut self) -> Result<()> {
        self.refresh_once(None, true)
    }

    /// Lays out and redraws the bars of the monitors in `only` (`None`: all of them).
    /// `retry`: after a lost device is recovered mid-draw, draw once more (only once, and
    /// every bar, since all surfaces were lost).
    fn refresh_once(&mut self, only: Option<&BTreeSet<u64>>, retry: bool) -> Result<()> {
        if self.gpu_lost {
            // Recovery is driven by the removal event and the one-shot retry timer only.
            return Ok(());
        }
        let wanted = |id: u64| only.is_none_or(|s| s.contains(&id));
        let tray_touched = self.tray_monitor().is_some_and(|m| wanted(m.id()));
        if tray_touched {
            self.reload_tray();
        }
        let mut draw_failed = None;
        let mut raised = false;
        for k in 0..self.monitors.len() {
            if wanted(self.monitors[k].id()) {
                raised |= self.refresh_monitor(k, &mut draw_failed)?;
                if draw_failed.is_some() {
                    break;
                }
            }
        }
        self.trim_icons();
        if raised {
            // Bar windows just went (back) to the top of the topmost band: keep our
            // Shell_TrayWnd above Explorer's there.
            if let Some(t) = &self.tray {
                t.reassert_topmost();
            }
        }
        if tray_touched {
            self.sync_overflow();
            self.sync_tooltip();
        }
        if let Some(e) = draw_failed {
            // Carry 2: a failed draw is usually a lost device; recover (or retry later).
            if self.gpu.is_lost() {
                self.gpu_lost = true;
                if self.recover_gpu() && retry {
                    return self.refresh_once(None, false);
                }
            } else {
                eprintln!("vitrobar: drawing failed: {e}");
            }
        }
        if self.opts.debug_state {
            self.dump_state();
        }
        Ok(())
    }

    /// Lays out and redraws bar `k`; true if a window was moved or shown (raised to the top
    /// of the topmost band). A draw error lands in `draw_failed`; window errors return.
    fn refresh_monitor(&mut self, k: usize, draw_failed: &mut Option<Error>) -> Result<bool> {
        let layout = self.compute_layout(k);
        let scale = self.monitors[k].scale();
        let margin = self.hover_margin(scale);
        let items = self.items(k, layout.apps.len());
        let (tray_items, chevron) = if self.monitors[k].tray {
            (self.tray_items(), self.chevron_state())
        } else {
            (Vec::new(), ChevronState::default())
        };
        let env = self.env(k);
        let focus = self
            .keys
            .focus()
            .filter(|f| f.monitor == self.monitors[k].id())
            .map(|f| f.item);
        let pills = [
            self.music.as_ref().and_then(|m| m.pill.clone()),
            self.claude.as_ref().and_then(|c| c.pill.clone()),
        ];
        let ime_hover = self.hover == Some(Hit::Ime) && self.monitors[k].id() == self.cur;
        let ime_text = self.ime_text.clone();
        let pill_hover = match self.hover {
            Some(Hit::Widget(w)) if self.monitors[k].id() == self.cur => Some(w),
            _ => None,
        };
        let mb = &mut self.monitors[k];
        let mut raised = layout.surfaces != mb.layout.surfaces;
        while mb.bars.len() > layout.surfaces.len() {
            mb.bars.pop();
        }
        for i in mb.bars.len()..layout.surfaces.len() {
            let window = create_bar_window(&self.comp, to_px(layout.surfaces[i], scale), margin)?;
            let visual = BarVisual::new(&self.comp, &self.gpu, i)?;
            window.set_content(visual.root())?;
            mb.bars.push(Bar {
                visual,
                window,
                shown: false,
            });
        }
        for (i, bar) in mb.bars.iter_mut().enumerate() {
            bar.window
                .set_rect(to_px(layout.surfaces[i], scale), margin)?;
            bar.visual.set_env(env);
            bar.visual.set_tray(tray_items.clone(), chevron);
            bar.visual.set_focus(focus);
            bar.visual.set_widgets(pills.clone(), pill_hover);
            bar.visual.set_ime(ime_text.clone(), ime_hover);
            if let Err(e) = bar.visual.update(
                &layout,
                &items,
                &self.clock_text,
                &self.style,
                &self.hover_style,
            ) {
                *draw_failed = Some(e);
                break;
            }
            if !bar.shown && !mb.fullscreen {
                bar.window.show();
                bar.shown = true;
                raised = true;
            }
        }
        let tray = mb.tray;
        mb.layout = layout;
        self.sync_appbar(k);
        if tray {
            // The pill grows and shrinks as apps come and go: the panel follows its button.
            self.quick_reanchor();
            self.publish_icon_rects(k);
        }
        self.calendar_reanchor(k);
        if tray {
            self.music_reanchor();
            self.claude_reanchor();
        }
        Ok(raised)
    }

    /// Keeps bar `k`'s main window registered as the appbar reserving its strip of its
    /// monitor.
    fn sync_appbar(&mut self, k: usize) {
        if !self.shell_ready {
            // Reserved only once the stock taskbar no longer reserves its own strip.
            return;
        }
        let edge = self.settings.position;
        let mb = &mut self.monitors[k];
        let Some(bar) = mb.bars.first() else {
            mb.drop_appbar();
            return;
        };
        let hwnd = bar.window.hwnd;
        let reserved = (mb.layout.reserved * mb.scale()).round() as i32;
        let mon = mb.info.rect_px;
        match &mut mb.appbar {
            // A new edge registers afresh, so the shell drops the old strip entirely.
            Some(ab) if ab.hwnd() == hwnd && ab.edge() == edge => ab.update(edge, mon, reserved),
            _ => {
                mb.drop_appbar();
                match AppBar::register(hwnd, edge, mon, reserved) {
                    Ok(ab) => {
                        shell::add_appbar_hwnd(hwnd);
                        mb.appbar = Some(ab);
                    }
                    Err(e) => eprintln!("vitrobar: could not reserve the bar's space: {e}"),
                }
            }
        }
    }

    /// Explorer restarted: hide its new taskbar, register the appbars with the new shell and
    /// reconnect to its virtual desktop manager.
    fn on_taskbar_created(&mut self, dirty: &mut Dirty) {
        self.close_start_now();
        self.close_quick_now();
        self.close_calendar_now();
        self.close_music_panel_now();
        self.close_claude_panel_now();
        self.stock.on_taskbar_created();
        // The old manager is a proxy into the Explorer that just ended: every query on it
        // would fail open (every window "on the current desktop") until the next restart.
        match self.desktops.recreate() {
            Ok(()) => dirty.regroup |= self.windows.refresh_desktops(&self.desktops),
            Err(e) => eprintln!("vitrobar: could not reconnect to the virtual desktops: {e}"),
        }
        for mb in &mut self.monitors {
            if let Some(ab) = &mut mb.appbar {
                if let Err(e) = ab.reregister() {
                    eprintln!("vitrobar: could not reserve the bar's space again: {e}");
                }
            }
        }
        self.arm_reassert();
    }

    /// One-shot: hide the stock taskbars once more a little later (Explorer can show its
    /// taskbar after `TaskbarCreated`, and creates a new monitor's `Shell_SecondaryTrayWnd`
    /// some time after `WM_DISPLAYCHANGE`). Event-driven, never a poll.
    fn arm_reassert(&self) {
        // SAFETY: one-shot timer on our own window (killed when it fires); re-setting the id
        // restarts it.
        unsafe { SetTimer(Some(self.notify.hwnd), TIMER_REASSERT, REASSERT_MS, None) };
    }

    /// Hides bar `k` (at once, no animation) while a full-screen app covers its monitor, and
    /// lets `refresh` show it again afterwards. True if the state changed.
    fn set_fullscreen(&mut self, k: usize, on: bool) -> bool {
        let Some(mb) = self.monitors.get(k) else {
            return false;
        };
        if on == mb.fullscreen {
            return false;
        }
        let (id, tray) = (mb.id(), mb.tray);
        if on {
            // Whatever is open from this bar goes with it.
            if self.strip.as_ref().is_some_and(|s| s.monitor == id) {
                self.close_strip();
            }
            if tray {
                self.close_overflow();
                self.close_quick();
                self.close_music_panel();
                self.close_claude_panel();
                self.set_tip_target(None);
            }
            if self.calendar.as_ref().is_some_and(|c| c.monitor == id) {
                self.close_calendar();
            }
            if self.start.as_ref().is_some_and(|s| s.monitor == id) {
                self.close_start_now();
            }
            if self.cur == id {
                self.cancel_show();
                self.hover = None;
                self.pressed = None;
                self.rpressed = None;
                self.tracking = None;
            }
        }
        let mb = &mut self.monitors[k];
        mb.fullscreen = on;
        if on {
            for b in &mut mb.bars {
                b.window.hide();
                b.shown = false;
            }
            self.sync_expanded();
        }
        if self.opts.debug_state {
            self.dump_state();
        }
        true
    }

    /// Brings the bars in line with `monitors` (in [`current_monitors`] order): a bar for
    /// each new monitor, none for the ones that went away, a new layout for those whose
    /// rectangle, DPI or tray area changed. Nothing happens when nothing changed, so the
    /// work-area broadcasts our own appbars cause end here. An empty list (a transient state
    /// while displays reconfigure) is ignored. True if anything changed; the caller redraws.
    fn reconcile_monitors(&mut self, monitors: Vec<MonitorInfo>) -> bool {
        if monitors.is_empty() {
            return false;
        }
        let old: Vec<MonitorInfo> = self.monitors.iter().map(|m| m.info).collect();
        let plan = plan_reconcile(&old, &monitors);
        if plan.is_empty() {
            return false;
        }
        // The strip, the overflow and the tooltip are placed for the old geometry.
        self.reset_interaction();
        let mut old_bars = std::mem::take(&mut self.monitors);
        for (k, info) in monitors.into_iter().enumerate() {
            let mb = match old_bars.iter().position(|m| m.id() == info.id) {
                Some(p) => {
                    let mut mb = old_bars.remove(p);
                    mb.info = info;
                    mb.tray = k == 0;
                    mb
                }
                None => MonitorBar::new(info, k == 0),
            };
            self.monitors.push(mb);
        }
        // The bars of removed monitors: appbars unregistered, windows destroyed. Never ends
        // the app (carry 1: `BarWindow`'s drop marks the destruction as ours).
        drop(old_bars);
        if self.mon_index(self.cur).is_none() {
            self.cur = self.monitors[0].id();
        }
        if self.opts.debug_state {
            eprintln!(
                "vitrobar-monitors added={:?} removed={:?} changed={:?}",
                plan.added, plan.removed, plan.changed
            );
        }
        true
    }

    /// Recreates the GPU device; on failure arms a one-shot retry. True when usable again.
    fn recover_gpu(&mut self) -> bool {
        match self.gpu.recreate() {
            Ok(()) => {
                self.gpu_lost = false;
                for bar in self.monitors.iter_mut().flat_map(|m| &mut m.bars) {
                    bar.visual.invalidate();
                }
                true
            }
            Err(e) => {
                eprintln!("vitrobar: GPU device recreation failed: {e}");
                self.gpu_lost = true;
                // SAFETY: one-shot timer on our own window (killed when it fires).
                unsafe { SetTimer(Some(self.notify.hwnd), TIMER_GPU_RETRY, GPU_RETRY_MS, None) };
                false
            }
        }
    }

    /// One monitor's bar for the state dump.
    fn dump_monitor(&self, mb: &MonitorBar) -> String {
        let s = mb.scale();
        let keys: Vec<String> = mb
            .groups
            .iter()
            .map(|g| {
                let hwnds: Vec<String> = g.windows.iter().map(|h| h.to_string()).collect();
                format!(
                    "{{\"key\":{},\"windows\":[{}]}}",
                    json_str(&group_key_text(&g.key)),
                    hwnds.join(",")
                )
            })
            .collect();
        let bars: Vec<String> = mb
            .bars
            .iter()
            .map(|b| {
                format!(
                    "{{\"hwnd\":{},\"rect\":{},\"shown\":{}}}",
                    b.window.hwnd.0 as usize,
                    json_rect(b.window.rect_px()),
                    b.shown
                )
            })
            .collect();
        let appbar = match &mb.appbar {
            Some(_) => json_rect(reserved_rect(
                self.settings.position,
                mb.info.rect_px,
                (mb.layout.reserved * s).round() as i32,
            )),
            None => "null".to_string(),
        };
        let l = &mb.layout;
        format!(
            "{{\"id\":{},\"rect\":{},\"dpi\":{},\"primary\":{},\"tray\":{},\"fullscreen\":{},\"appbar\":{},\"bars\":[{}],\"start\":{},\"quick\":{},\"chevron\":{},\"clock\":{},\"tray_slots\":{},\"shown\":{},\"groups\":[{}]}}",
            mb.id(),
            json_rect(mb.info.rect_px),
            mb.info.dpi,
            mb.info.primary,
            mb.tray,
            mb.fullscreen,
            appbar,
            bars.join(","),
            json_rect(to_px(l.start, s)),
            json_rect(to_px(l.quick, s)),
            json_rect(to_px(l.overflow_chevron, s)),
            json_rect(to_px(l.clock, s)),
            l.tray.len(),
            l.apps.len(),
            keys.join(","),
        )
    }

    fn dump_state(&mut self) {
        let Some(mb) = self.monitors.first() else {
            return;
        };
        let monitors: Vec<String> = self.monitors.iter().map(|m| self.dump_monitor(m)).collect();
        let l = &mb.layout;
        let s = mb.scale();
        let apps: Vec<String> = mb
            .groups
            .iter()
            .zip(&l.apps)
            .map(|(g, r)| {
                let key = group_key_text(&g.key);
                let titles: Vec<String> = g
                    .windows
                    .iter()
                    .map(|h| json_str(self.windows.title(*h).unwrap_or("")))
                    .collect();
                let hwnds: Vec<String> = g.windows.iter().map(|h| h.to_string()).collect();
                format!(
                    "{{\"key\":{},\"windows\":[{}],\"titles\":[{}],\"dots\":{},\"active\":{},\"attention\":{},\"rect\":{}}}",
                    json_str(&key),
                    hwnds.join(","),
                    titles.join(","),
                    g.windows.len().min(3),
                    g.active,
                    g.attention,
                    json_rect(to_px(*r, s)),
                )
            })
            .collect();
        let bars: Vec<String> = mb
            .bars
            .iter()
            .map(|b| {
                format!(
                    "{{\"hwnd\":{},\"rect\":{},\"expanded\":{}}}",
                    b.window.hwnd.0 as usize,
                    json_rect(b.window.rect_px()),
                    b.window.expanded()
                )
            })
            .collect();
        let strip = match &self.strip {
            None => "null".to_string(),
            Some(st) => {
                let (fl, ft, _, _) = st.flyout.rect_px();
                let thumbs: Vec<String> = st
                    .thumbs
                    .thumbnails()
                    .into_iter()
                    .map(|(h, r)| {
                        format!(
                            "[{},{}]",
                            h.0 as usize,
                            json_rect((fl + r.0, ft + r.1, fl + r.2, ft + r.3))
                        )
                    })
                    .collect();
                format!(
                    "{{\"hwnd\":{},\"rect\":{},\"thumbs\":[{}]}}",
                    st.flyout.hwnd().0 as usize,
                    json_rect(st.flyout.rect_px()),
                    thumbs.join(",")
                )
            }
        };
        let hover = match self.hover {
            Some(Hit::App(i)) if self.cur == mb.id() => i.to_string(),
            _ => "null".to_string(),
        };
        let appbar = match &mb.appbar {
            Some(_) => json_rect(reserved_rect(
                self.settings.position,
                mb.info.rect_px,
                (l.reserved * s).round() as i32,
            )),
            None => "null".to_string(),
        };
        let tray: Vec<String> = self
            .tray_entries
            .iter()
            .enumerate()
            .map(|(k, e)| {
                let bar = self.tray_bar.iter().position(|&b| b == k);
                format!(
                    "{{\"key\":{},\"tip\":{},\"hwnd\":{},\"uid\":{},\"version\":{},\"hidden\":{},\"image\":{},\"rect\":{}}}",
                    json_str(self.tray_keys.get(&tray_id(&e.icon)).map_or("", |k| k)),
                    json_str(&e.icon.tip),
                    e.icon.id.hwnd,
                    e.icon.id.uid,
                    e.icon.version,
                    e.icon.hidden,
                    e.image.is_some(),
                    bar.and_then(|b| l.tray.get(b))
                        .map_or("null".to_string(), |r| json_rect(to_px(*r, s))),
                )
            })
            .collect();
        let overflow = match &self.overflow {
            None => "null".to_string(),
            Some(o) => {
                let (fl, ft, _, _) = o.flyout.rect_px();
                let slots: Vec<String> = o
                    .ids
                    .iter()
                    .enumerate()
                    .filter_map(|(i, id)| {
                        let r = o.grid.slot_px(i)?;
                        let tip = self.tray_entry(*id).map_or("", |e| e.icon.tip.as_str());
                        Some(format!(
                            "{{\"tip\":{},\"rect\":{}}}",
                            json_str(tip),
                            json_rect((fl + r.0, ft + r.1, fl + r.2, ft + r.3))
                        ))
                    })
                    .collect();
                format!(
                    "{{\"hwnd\":{},\"rect\":{},\"slots\":[{}]}}",
                    o.flyout.hwnd().0 as usize,
                    json_rect(o.flyout.rect_px()),
                    slots.join(",")
                )
            }
        };
        let tooltip = match &self.tooltip {
            None => "null".to_string(),
            Some(t) => format!(
                "{{\"text\":{},\"rect\":{}}}",
                json_str(t.tip()),
                json_rect(t.rect_px())
            ),
        };
        let tray_host = self
            .tray
            .as_ref()
            .map_or("null".to_string(), |t| (t.hwnd().0 as usize).to_string());
        let st = &self.settings;
        let settings = format!(
            "{{\"shape\":\"{:?}\",\"material\":\"{:?}\",\"hover\":\"{:?}\",\"icon_size\":\"{:?}\",\"position\":\"{:?}\",\"start_with_windows\":{},\"theme\":\"{:?}\",\"accent\":{},\"start_style\":\"{:?}\",\"win_key\":{},\"stock_taskbar\":\"{:?}\",\"language\":{}}}",
            st.shape,
            st.material,
            st.hover,
            st.icon_size,
            st.position,
            st.start_with_windows,
            st.theme,
            st.accent.map_or("null".to_string(), |a| a.to_string()),
            st.start_style,
            st.win_key_opens_start,
            st.stock_taskbar,
            st.language.as_deref().map_or("null".to_string(), json_str),
        );
        let settings_window = self.dump_settings_window();
        let pins: Vec<String> = st.pins.iter().map(|p| json_str(&p.name)).collect();
        let start = self.dump_start();
        let keys = self.keys.dump();
        let dump = format!(
            "{{\"keys\":{},\"start\":{},\"settings_window\":{},\"settings\":{},\"pins\":[{}],\"tray_host\":{},\"chevron\":{},\"tray\":[{}],\"overflow\":{},\"tooltip\":{},\"fullscreen\":{},\"stock_taken\":{},\"appbar\":{},\"hover\":{},\"strip\":{},\"live_thumbnails\":{},\"bars\":[{}],\"groups\":{},\"shown\":{},\"start_button\":{},\"quick\":{},\"clock\":{},\"clock_text\":{},\"more\":{},\"apps\":[{}],\"cur\":{},\"monitors\":[{}]}}",
            keys,
            start,
            settings_window,
            settings,
            pins.join(","),
            tray_host,
            json_rect(to_px(l.overflow_chevron, s)),
            tray.join(","),
            overflow,
            tooltip,
            mb.fullscreen,
            self.stock.active(),
            appbar,
            hover,
            strip,
            live_thumbnails(),
            bars.join(","),
            mb.groups.len(),
            l.apps.len(),
            json_rect(to_px(l.start, s)),
            json_rect(to_px(l.quick, s)),
            json_rect(to_px(l.clock, s)),
            json_str(&self.clock_text),
            l.more.is_some(),
            apps.join(","),
            self.cur,
            monitors.join(","),
        );
        if dump != self.last_dump {
            eprintln!("vitrobar-state {dump}");
            self.last_dump = dump;
        }
    }

    fn on_window_event(&mut self, ev: WindowEvent, dirty: &mut Dirty) {
        match ev {
            WindowEvent::Changed(h) if self.stock.watches(h) => {
                // Explorer moved one of its (hidden) taskbars: it is showing it again.
                dirty.stock = true;
            }
            WindowEvent::Created(h) | WindowEvent::Changed(h) => {
                // Changed also covers a window moving to another monitor (LOCATIONCHANGE,
                // HSHELL_MONITORCHANGED): the re-read finds its new monitor and the regroup
                // touches only the two bars involved.
                dirty.windows.insert(h);
                // The foreground window moved or resized: it may have gone full screen.
                dirty.fullscreen |= self.foreground == Some(h);
            }
            WindowEvent::Destroyed(h) => {
                dirty.windows.remove(&h);
                dirty.regroup |= self.windows.remove(h);
                dirty.fullscreen |= self.monitors.iter().any(|m| m.fullscreen);
            }
            WindowEvent::Activated(h) => {
                // Carry 7: activation ends a window's attention request.
                let fg = h.or_else(foreground);
                if let Some(h) = fg {
                    dirty.regroup |= self.windows.set_flashing(h, false);
                }
                let changed = self.foreground != fg;
                dirty.regroup |= changed;
                // Another window in front ends the keyboard focus mode, unless the change
                // only leaves one of our own windows (the Start panel or the overflow that
                // entering focus mode just closed).
                if activation_ends_focus(changed, self.foreground_ours) {
                    self.leave_focus(dirty);
                }
                self.foreground = fg;
                self.foreground_ours = is_own_window(fg);
                dirty.fullscreen = true;
                self.start_foreground_changed(fg);
                self.quick_foreground_changed(fg);
                self.calendar_foreground_changed(fg);
                // Each window has its own input language.
                if self.read_ime() {
                    dirty.visuals = true;
                }
                self.music_foreground_changed(fg);
                self.claude_foreground_changed(fg);
                // A foreground change is when the shell reorders the topmost band: keep our
                // Shell_TrayWnd above Explorer's (async, never waits).
                if let Some(t) = &self.tray {
                    t.reassert_topmost();
                }
            }
            WindowEvent::Foreground(_) => {}
            WindowEvent::Flash(h) => {
                // Carry 7: read_window_info never reports flashing; the event does.
                if self.foreground != Some(h) {
                    dirty.regroup |= self.windows.set_flashing(h, true);
                    if !self.windows.contains(h) {
                        dirty.windows.insert(h);
                    }
                }
            }
            WindowEvent::FullscreenChanged => {
                // Carry 8: HSHELL_RUDEAPPACTIVATED is also an activation.
                let fg = foreground();
                dirty.regroup |= self.foreground != fg;
                self.foreground = fg;
                self.foreground_ours = is_own_window(fg);
                dirty.fullscreen = true;
            }
            WindowEvent::DesktopSwitched => {
                // A key-up may have been lost in the switch.
                self.keys.forget_swallowed();
                // Every bar shows only the current desktop's windows; pins stay.
                let changed = self.windows.refresh_desktops(&self.desktops);
                dirty.regroup |= changed;
                if self.opts.debug_state {
                    eprintln!("vitrobar-desktop-switched changed={changed}");
                }
            }
        }
    }

    fn on_timer(&mut self, id: usize, dirty: &mut Dirty) {
        match id {
            TIMER_CLOCK => dirty.clock = true,
            TIMER_GPU_RETRY => {
                // SAFETY: one-shot: stop the periodic timer.
                unsafe {
                    let _ = KillTimer(Some(self.notify.hwnd), TIMER_GPU_RETRY);
                }
                if self.gpu_lost && self.recover_gpu() {
                    dirty.visuals = true;
                }
            }
            TIMER_HOVER_SHOW => {
                self.kill_timer(TIMER_HOVER_SHOW);
                if let Some(i) = self.show_pending.take() {
                    if self.hover == Some(Hit::App(i)) && self.strip.is_none() {
                        self.open_strip(i);
                    }
                }
            }
            TIMER_REASSERT => {
                self.kill_timer(TIMER_REASSERT);
                self.stock.reassert();
                if let Some(t) = &self.tray {
                    t.reassert_topmost();
                }
            }
            TIMER_TRAY_TIP => {
                self.kill_timer(TIMER_TRAY_TIP);
                self.show_tooltip();
            }
            TIMER_START_CLOSED => {
                // A timer of an earlier panel, queued before it was re-armed, is not this
                // panel's: only the deadline of the panel now fading counts.
                if Instant::now() >= self.start_closing_until {
                    self.kill_timer(TIMER_START_CLOSED);
                    self.drop_start_closing();
                }
            }
            TIMER_START_PREFETCH => {
                self.kill_timer(TIMER_START_PREFETCH);
                self.start_data.prefetch(self.notify.hwnd);
            }
            TIMER_MARGIN_COLLAPSE => {
                self.kill_timer(TIMER_MARGIN_COLLAPSE);
                self.collapse_pending = false;
                for b in self.all_bars_mut() {
                    b.window.finish_linger();
                }
            }
            TIMER_HOVER_HIDE => {
                self.kill_timer(TIMER_HOVER_HIDE);
                self.hide_pending = false;
                if self.strip.is_some() && !self.over_flyout && self.hovered_strip_app().is_none() {
                    self.close_strip();
                }
            }
            TIMER_QUICK_SCAN | TIMER_QUICK_BT => self.quick_timer(id),
            TIMER_CALENDAR_MIDNIGHT => self.calendar_timer(id),
            TIMER_CLAUDE_STALE | TIMER_CLAUDE_MINUTE => self.claude_timer(id, dirty),
            TIMER_IME => {
                self.kill_timer(id);
                if self.read_ime() {
                    dirty.visuals = true;
                }
            }
            other => self.tracker.on_timer(other),
        }
    }

    /// Every bar window of every monitor.
    fn all_bars_mut(&mut self) -> impl Iterator<Item = &mut Bar> {
        self.monitors.iter_mut().flat_map(|m| m.bars.iter_mut())
    }

    /// (monitor index, surface index) of bar window `hwnd`.
    fn bar_index(&self, hwnd: HWND) -> Option<(usize, usize)> {
        self.monitors.iter().enumerate().find_map(|(k, m)| {
            m.bars
                .iter()
                .position(|b| b.window.hwnd == hwnd)
                .map(|i| (k, i))
        })
    }

    /// The cursor came onto the bar of monitor `id`: the hover and presses on the bar it was
    /// on before end (that bar is redrawn), and app indices now refer to the new bar.
    fn switch_cur(&mut self, id: u64, dirty: &mut Dirty) {
        if id == self.cur {
            return;
        }
        dirty.bars.insert(self.cur);
        dirty.bars.insert(id);
        self.cancel_show();
        self.hover = None;
        self.pressed = None;
        self.rpressed = None;
        self.show_suppressed = false;
        self.set_tip_target(None);
        self.cur = id;
    }

    /// Mouse input on a bar window or a flyout; returns true if the message was handled.
    fn on_mouse(&mut self, msg: &MSG, dirty: &mut Dirty) -> bool {
        if matches!(msg.message, WM_LBUTTONDOWN | WM_RBUTTONDOWN) {
            // Any click ends the keyboard focus mode.
            self.leave_focus(dirty);
        }
        let had_overflow = self.overflow.is_some();
        let handled = self.on_mouse_inner(msg, dirty);
        if had_overflow != self.overflow.is_some() {
            if let Some(m) = self.tray_monitor() {
                dirty.bars.insert(m.id());
            }
        }
        handled
    }

    fn on_mouse_inner(&mut self, msg: &MSG, dirty: &mut Dirty) -> bool {
        self.msg_pt = message_pos();
        self.input_time = msg.time;
        if self.start.as_ref().is_some_and(|s| s.hwnd() == msg.hwnd) {
            return self.on_start_mouse(msg, dirty);
        }
        if self.quick.as_ref().is_some_and(|q| q.hwnd() == msg.hwnd) {
            self.on_quick_mouse(msg);
            return true;
        }
        if self.calendar.as_ref().is_some_and(|c| c.hwnd() == msg.hwnd) {
            self.calendar_event(|c, g| c.on_mouse(msg, g));
            return true;
        }
        if self
            .music_panel
            .as_ref()
            .is_some_and(|p| p.hwnd() == msg.hwnd)
        {
            self.music_event(|p, _| p.on_mouse(msg));
            return true;
        }
        if self
            .claude_panel
            .as_ref()
            .is_some_and(|p| p.hwnd() == msg.hwnd)
        {
            self.claude_event(|p, _| p.on_mouse(msg));
            return true;
        }
        if self.is_settings(msg.hwnd) {
            return self.on_settings_mouse(msg, dirty);
        }
        if self
            .strip
            .as_ref()
            .is_some_and(|s| s.flyout.hwnd() == msg.hwnd)
        {
            return self.on_flyout_mouse(msg);
        }
        if self
            .overflow
            .as_ref()
            .is_some_and(|o| o.flyout.hwnd() == msg.hwnd)
        {
            return self.on_overflow_mouse(msg);
        }
        let Some((k, i)) = self.bar_index(msg.hwnd) else {
            return false;
        };
        let id = self.monitors[k].id();
        if id != self.cur {
            if msg.message == WM_MOUSELEAVE {
                // The late leave of a bar the cursor has already left for another monitor's.
                return true;
            }
            self.switch_cur(id, dirty);
        }
        let mb = &self.monitors[k];
        let (cx, cy) = mouse_pos(msg.lParam);
        let (x, y) = mb.bars[i].window.client_to_dip(cx, cy, mb.scale());
        let mut hit = hit_test(&mb.layout, x, y);
        if hit == Some(Hit::Chevron) && (self.tray_over.is_empty() || !mb.tray) {
            // No overflow, no chevron.
            hit = None;
        }
        match msg.message {
            WM_MOUSEMOVE => {
                if self.tracking != Some(msg.hwnd) {
                    self.monitors[k].bars[i].window.track_leave();
                    self.tracking = Some(msg.hwnd);
                }
                if self.hover != hit {
                    self.hover = hit;
                    self.show_suppressed = false;
                    dirty.bars.insert(self.cur);
                    let target = match hit {
                        Some(Hit::Tray(k)) => self.bar_tray_target(k),
                        _ => None,
                    };
                    self.set_tip_target(target);
                }
            }
            WM_MOUSELEAVE => {
                // With several bar windows (Islands) the move into the next window can arrive
                // before the leave of the previous one: only the tracked window's leave counts.
                if self.tracking == Some(msg.hwnd) {
                    self.tracking = None;
                    if self.hover.is_some() || self.pressed.is_some() || self.rpressed.is_some() {
                        self.hover = None;
                        self.pressed = None;
                        self.rpressed = None;
                        dirty.bars.insert(self.cur);
                    }
                    if self.overflow.as_ref().is_none_or(|o| !o.over) {
                        self.set_tip_target(None);
                    }
                }
            }
            WM_LBUTTONDOWN => {
                self.pressed = hit;
                // A click decides what happens; the hover timer does not open the strip later.
                self.show_suppressed = true;
                self.cancel_show();
                self.hide_tooltip();
                // The bar never takes the foreground, so a press on it does not deactivate
                // the overflow: close it here (the chevron toggles it on release).
                if hit != Some(Hit::Chevron) {
                    self.close_overflow();
                }
                dirty.bars.insert(self.cur);
            }
            WM_LBUTTONUP => {
                let pressed = self.pressed.take();
                dirty.bars.insert(self.cur);
                if pressed.is_some() && pressed == hit {
                    if let Some(h) = hit {
                        self.click(h, dirty);
                    }
                }
            }
            WM_RBUTTONDOWN => {
                self.rpressed = Some(hit);
                self.hide_tooltip();
                dirty.bars.insert(self.cur);
            }
            WM_RBUTTONUP => {
                let pressed = self.rpressed.take();
                dirty.bars.insert(self.cur);
                match hit {
                    Some(Hit::Tray(k)) => {
                        if pressed == Some(hit) {
                            self.close_overflow();
                            self.tray_click(self.tray_bar.get(k).copied(), TrayClick::Right);
                        }
                    }
                    Some(Hit::App(i)) => {
                        if pressed == Some(hit) {
                            self.context_menu(Some(i), dirty);
                        }
                    }
                    // Anywhere else on the bar: the bar's own menu, when the right press started
                    // on the bar at the same place (the empty bar counts only if pressed there).
                    _ if pressed == Some(hit) => self.context_menu(None, dirty),
                    _ => {}
                }
            }
            _ => return false,
        }
        self.sync_expanded();
        self.update_strip();
        true
    }

    /// Screen px of promoted tray slot `k`, spanning the bar's height (a tooltip anchor).
    fn bar_tray_target(&self, k: usize) -> Option<TipTarget> {
        let mb = self.tray_monitor()?;
        let idx = *self.tray_bar.get(k)?;
        let r = mb.layout.tray.get(k)?;
        let s = mb
            .layout
            .surfaces
            .iter()
            .find(|s| r.x + r.w / 2.0 >= s.x && r.x + r.w / 2.0 < s.right())?;
        Some(TipTarget {
            id: tray_id(&self.tray_entries.get(idx)?.icon),
            anchor: to_px(
                Rect {
                    x: r.x,
                    y: s.y,
                    w: r.w,
                    h: s.h,
                },
                mb.scale(),
            ),
        })
    }

    fn tray_entry(&self, id: TrayId) -> Option<&TrayEntry> {
        self.tray_entries.iter().find(|e| tray_id(&e.icon) == id)
    }

    /// The cursor moved onto another tray icon (or off the tray): the tooltip follows after
    /// the delay, and the icon's owner hears the hover (as with the stock taskbar).
    fn set_tip_target(&mut self, target: Option<TipTarget>) {
        if target.map(|t| t.id) == self.tip_target.map(|t| t.id) {
            self.tip_target = target;
            return;
        }
        self.hide_tooltip();
        self.tip_target = target;
        let Some(t) = target else { return };
        if let Some(e) = self.tray_entry(t.id) {
            TrayHost::forward_click(&e.icon, TrayClick::Hover, self.msg_pt);
        }
        // SAFETY: one-shot timer on our own window (killed when it fires); re-setting the id
        // restarts it.
        unsafe {
            SetTimer(Some(self.notify.hwnd), TIMER_TRAY_TIP, TRAY_TIP_MS, None);
        }
    }

    fn hide_tooltip(&mut self) {
        self.kill_timer(TIMER_TRAY_TIP);
        if self.tooltip.take().is_some() && self.opts.debug_state {
            self.dump_state();
        }
    }

    fn show_tooltip(&mut self) {
        let Some(t) = self.tip_target else { return };
        let Some(tip) = self.tray_entry(t.id).map(|e| e.icon.tip.clone()) else {
            return;
        };
        self.tooltip = None;
        if tip.trim().is_empty() {
            return;
        }
        match Tooltip::show(
            &self.comp,
            &self.painter,
            t.anchor,
            self.settings.position,
            self.tray_dpi(),
            &self.style,
            &tip,
        ) {
            Ok(tt) => self.tooltip = Some(tt),
            Err(e) => eprintln!("vitrobar: tray tooltip failed: {e}"),
        }
        if self.opts.debug_state {
            self.dump_state();
        }
    }

    /// The shown tooltip follows its icon's tip text, and goes with the icon.
    fn sync_tooltip(&mut self) {
        let Some(t) = self.tip_target else { return };
        match self.tray_entry(t.id).map(|e| e.icon.tip.clone()) {
            None => self.set_tip_target(None),
            Some(tip) => {
                if self.tooltip.as_ref().is_some_and(|tt| tt.tip() != tip) {
                    self.show_tooltip();
                }
            }
        }
    }

    /// Hands a click on tray entry `idx` to its owner.
    fn tray_click(&mut self, idx: Option<usize>, kind: TrayClick) {
        let Some(e) = idx.and_then(|i| self.tray_entries.get(i)) else {
            return;
        };
        let icon = e.icon.clone();
        let kind = if kind == TrayClick::Left {
            // The second click of a double click (our windows get no WM_LBUTTONDBLCLK).
            let id = tray_id(&icon);
            // SAFETY: plain query.
            let dbl = Duration::from_millis(u64::from(unsafe { GetDoubleClickTime() }));
            match self.last_left {
                Some((prev, at)) if prev == id && at.elapsed() <= dbl => {
                    self.last_left = None;
                    TrayClick::Double
                }
                _ => {
                    self.last_left = Some((id, Instant::now()));
                    TrayClick::Left
                }
            }
        } else {
            kind
        };
        self.hide_tooltip();
        if self.opts.debug_state {
            eprintln!(
                "vitrobar-tray-click {} {} {kind:?}",
                icon.id.hwnd, icon.id.uid
            );
        }
        TrayHost::forward_click(&icon, kind, self.msg_pt);
    }

    /// Screen px of the chevron's slot, spanning the bar's height.
    fn chevron_anchor(&self) -> Option<PxRect> {
        let mb = self.tray_monitor()?;
        let r = mb.layout.overflow_chevron;
        let s = mb
            .layout
            .surfaces
            .iter()
            .find(|s| r.x + r.w / 2.0 >= s.x && r.x + r.w / 2.0 < s.right())?;
        Some(to_px(
            Rect {
                x: r.x,
                y: s.y,
                w: r.w,
                h: s.h,
            },
            mb.scale(),
        ))
    }

    fn overflow_images(&mut self) -> (Vec<TrayId>, Vec<Option<Arc<BgraImage>>>) {
        let over = self.tray_over.clone();
        let ids = over
            .iter()
            .map(|&i| tray_id(&self.tray_entries[i].icon))
            .collect();
        let images = over.iter().map(|&i| self.tray_image(i)).collect();
        (ids, images)
    }

    /// Opens the overflow above the chevron and makes it the foreground window, so a click
    /// anywhere else, Esc or switching apps closes it.
    fn open_overflow(&mut self) {
        self.close_strip();
        self.cancel_show();
        self.set_tip_target(None);
        // An app that died since the last redraw must not show up.
        self.reload_tray();
        if self.tray_over.is_empty() {
            return;
        }
        let Some(anchor) = self.chevron_anchor() else {
            return;
        };
        let slot = metrics(self.settings.icon_size).slot;
        let (ids, images) = self.overflow_images();
        let (size, _) = grid_layout(ids.len(), slot);
        let fg_at_open = foreground().unwrap_or(0) as usize;
        let opened = Flyout::show(
            &self.comp,
            anchor,
            self.settings.position,
            size,
            self.tray_dpi(),
            &self.style,
        )
        .and_then(|flyout| {
            let mut grid = TrayGrid::new(&flyout)?;
            grid.set(&self.painter, slot, &images, self.style.text)?;
            Ok(Overflow {
                grid,
                flyout,
                ids,
                over: false,
                pressed: None,
                fg_at_open,
            })
        });
        match opened {
            Ok(o) => {
                if !o.flyout.activate() {
                    eprintln!("vitrobar: the tray overflow could not take the foreground");
                }
                self.overflow = Some(o);
            }
            Err(e) => eprintln!("vitrobar: tray overflow failed: {e}"),
        }
        if self.opts.debug_state {
            self.dump_state();
        }
    }

    /// True if it was open.
    fn close_overflow(&mut self) -> bool {
        let Some(o) = self.overflow.take() else {
            return false;
        };
        if o.over {
            self.set_tip_target(None);
        }
        drop(o);
        if self.opts.debug_state {
            self.dump_state();
        }
        true
    }

    /// After the tray changed: the open overflow follows (or closes when nothing is left).
    fn sync_overflow(&mut self) {
        if self.overflow.is_none() {
            return;
        }
        if self.tray_over.is_empty() {
            self.close_overflow();
            return;
        }
        let slot = metrics(self.settings.icon_size).slot;
        let (ids, images) = self.overflow_images();
        let anchor = self.chevron_anchor();
        let Some(o) = self.overflow.as_mut() else {
            return;
        };
        let mut result = Ok(());
        if o.ids.len() != ids.len() {
            if let Some(anchor) = anchor {
                let (size, _) = grid_layout(ids.len(), slot);
                result = o.flyout.reshape(
                    &self.comp,
                    anchor,
                    self.settings.position,
                    size,
                    &self.style,
                );
            }
            o.pressed = None;
        }
        o.ids = ids;
        let result =
            result.and_then(|()| o.grid.set(&self.painter, slot, &images, self.style.text));
        if let Err(e) = result {
            eprintln!("vitrobar: tray overflow update failed: {e}");
            self.close_overflow();
        }
    }

    /// Mouse input on the overflow: hover highlight and tooltip, clicks to the icon owners.
    fn on_overflow_mouse(&mut self, msg: &MSG) -> bool {
        let pt = mouse_pos(msg.lParam);
        let text = self.style.text;
        let Some(o) = self.overflow.as_mut() else {
            return false;
        };
        let slot = o.grid.hit(pt);
        let mut target = None;
        let mut click = None;
        match msg.message {
            WM_MOUSEMOVE => {
                if !o.over {
                    track_leave(msg.hwnd);
                    o.over = true;
                }
                let pressed = o.pressed.is_some_and(|(p, _)| Some(p) == slot);
                let _ = o.grid.set_hover(slot, pressed, text);
                target = slot.and_then(|k| {
                    let (fl, ft, _, _) = o.flyout.rect_px();
                    let r = o.grid.slot_px(k)?;
                    Some(TipTarget {
                        id: *o.ids.get(k)?,
                        anchor: (fl + r.0, ft + r.1, fl + r.2, ft + r.3),
                    })
                });
            }
            WM_MOUSELEAVE => {
                o.over = false;
                o.pressed = None;
                let _ = o.grid.set_hover(None, false, text);
            }
            WM_LBUTTONDOWN | WM_RBUTTONDOWN => {
                o.pressed = slot.map(|k| (k, msg.message == WM_RBUTTONDOWN));
                let _ = o.grid.set_hover(slot, true, text);
            }
            WM_LBUTTONUP | WM_RBUTTONUP => {
                let right = msg.message == WM_RBUTTONUP;
                if let (Some(k), Some((p, pr))) = (slot, o.pressed.take()) {
                    if p == k && pr == right {
                        click = o.ids.get(k).copied().map(|id| (id, right));
                    }
                }
                let _ = o.grid.set_hover(slot, false, text);
            }
            _ => return false,
        }
        match msg.message {
            WM_MOUSEMOVE => self.set_tip_target(target),
            WM_MOUSELEAVE => self.set_tip_target(None),
            WM_LBUTTONDOWN | WM_RBUTTONDOWN => self.hide_tooltip(),
            _ => {}
        }
        if let Some((id, right)) = click {
            let idx = self
                .tray_entries
                .iter()
                .position(|e| tray_id(&e.icon) == id);
            let kind = if right {
                TrayClick::Right
            } else {
                TrayClick::Left
            };
            self.tray_click(idx, kind);
        }
        true
    }

    /// Keys reach us only while the overflow or the Start panel is the foreground window:
    /// Esc closes the overflow; the Start panel gets every key.
    fn on_key(&mut self, msg: &MSG, dirty: &mut Dirty) -> bool {
        if self.quick.as_ref().is_some_and(|q| q.hwnd() == msg.hwnd) {
            self.on_quick_key(msg);
            return true;
        }
        if self.calendar.as_ref().is_some_and(|c| c.hwnd() == msg.hwnd) {
            self.on_calendar_key(msg);
            return true;
        }
        if self
            .music_panel
            .as_ref()
            .is_some_and(|p| p.hwnd() == msg.hwnd)
        {
            let vk = msg.wParam.0 as u16;
            // SAFETY: plain query of this thread's keyboard state.
            let shift = unsafe { GetKeyState(i32::from(VK_SHIFT.0)) < 0 };
            self.music_event(|p, _| p.on_key(vk, shift));
            return true;
        }
        if self
            .claude_panel
            .as_ref()
            .is_some_and(|p| p.hwnd() == msg.hwnd)
        {
            let vk = msg.wParam.0 as u16;
            self.claude_event(|p, _| p.on_key(vk));
            return true;
        }
        if self.start.as_ref().is_some_and(|s| s.hwnd() == msg.hwnd) {
            self.on_start_key(msg, dirty);
            return true;
        }
        if self.is_settings(msg.hwnd) {
            self.on_settings_key(msg, dirty);
            return true;
        }
        let ours = self
            .overflow
            .as_ref()
            .is_some_and(|o| o.flyout.hwnd() == msg.hwnd);
        if !ours {
            return false;
        }
        if msg.wParam.0 == usize::from(VK_ESCAPE.0) {
            // Back to the window the user was in (while the flyout still has the foreground).
            if let Some(o) = &self.overflow {
                let _ = self.hand_back_foreground(o.flyout.hwnd(), o.fg_at_open);
            }
            self.close_overflow();
            dirty.visuals = true;
        }
        true
    }

    /// A middle press or release on the thumbnail strip (it closes the window under it).
    fn on_strip_middle(&mut self, msg: &MSG) -> bool {
        self.strip
            .as_ref()
            .is_some_and(|s| s.flyout.hwnd() == msg.hwnd)
            && self.on_flyout_mouse(msg)
    }

    fn reset_flyout_press(&mut self) {
        self.flyout_pressed = None;
        self.flyout_close_pressed = None;
        self.flyout_mid_pressed = None;
    }

    /// Mouse input on the flyout: hovering keeps the strip open, a click on a thumbnail
    /// switches to that window, the close button or a middle click closes it.
    fn on_flyout_mouse(&mut self, msg: &MSG) -> bool {
        let pt = mouse_pos(msg.lParam);
        let hit = self.strip.as_ref().and_then(|s| s.thumbs.hit(pt));
        let close = self.strip.as_ref().and_then(|s| s.thumbs.hit_close(pt));
        match msg.message {
            WM_MOUSEMOVE => {
                if !self.over_flyout {
                    track_leave(msg.hwnd);
                    self.over_flyout = true;
                }
                if let Some(s) = self.strip.as_mut() {
                    let _ = s.thumbs.set_close_hot(close);
                }
            }
            WM_MOUSELEAVE => {
                self.over_flyout = false;
                self.reset_flyout_press();
                if let Some(s) = self.strip.as_mut() {
                    let _ = s.thumbs.set_close_hot(None);
                }
            }
            WM_LBUTTONDOWN => {
                self.flyout_close_pressed = close;
                self.flyout_pressed = hit.filter(|_| close.is_none());
            }
            WM_MBUTTONDOWN => self.flyout_mid_pressed = hit,
            WM_MBUTTONUP => {
                let pressed = self.flyout_mid_pressed.take();
                if let Some(h) = hit.filter(|_| pressed == hit) {
                    request_close(h);
                }
            }
            WM_LBUTTONUP => {
                let pressed_close = self.flyout_close_pressed.take();
                if close.is_some() || pressed_close.is_some() {
                    // The strip stays up: the closed window drops out of it by itself.
                    self.flyout_pressed = None;
                    if let Some(h) = close.filter(|_| pressed_close == close) {
                        request_close(h);
                    }
                    return true;
                }
                let pressed = self.flyout_pressed.take();
                if let Some(h) = hit.filter(|_| pressed == hit) {
                    if self.opts.debug_state {
                        eprintln!("vitrobar-thumb-click {}", h.0 as usize);
                    }
                    if !activate_window(h) {
                        eprintln!("vitrobar: SetForegroundWindow refused for {:?}", h.0);
                    }
                    self.close_strip();
                    return true;
                }
            }
            _ => return false,
        }
        self.update_strip();
        true
    }

    /// Only the bar window under the cursor, while it is over an app, takes input in (and
    /// shows content in) its margin. When the hover ends the margin keeps drawing but stops
    /// taking input for the length of the shrink-back (one-shot timer), then collapses.
    fn sync_expanded(&mut self) {
        let on_app = matches!(self.hover, Some(Hit::App(_)));
        let tracking = self.tracking;
        let mut lingering = false;
        let mut began = false;
        let animations = self.sys.animations;
        for b in self.monitors.iter_mut().flat_map(|m| m.bars.iter_mut()) {
            if on_app && tracking == Some(b.window.hwnd) {
                b.window.set_expanded(true);
            } else {
                if b.window.mode() == MarginMode::Active {
                    if animations {
                        b.window.begin_linger();
                        began = true;
                    } else {
                        b.window.set_expanded(false);
                    }
                }
                lingering |= b.window.mode() == MarginMode::Lingering;
            }
        }
        if began {
            self.arm_collapse();
        } else if lingering {
            // Already counting down.
        } else if self.collapse_pending {
            self.collapse_pending = false;
            self.kill_timer(TIMER_MARGIN_COLLAPSE);
        }
    }

    /// (Re)starts the one-shot that ends the margin shrink-back.
    fn arm_collapse(&mut self) {
        // SAFETY: one-shot timer on our own window (killed when it fires); re-setting the id
        // restarts it.
        let id = unsafe {
            SetTimer(
                Some(self.notify.hwnd),
                TIMER_MARGIN_COLLAPSE,
                hover_timing(&self.hover_style).0.max(1),
                None,
            )
        };
        self.collapse_pending = id != 0;
        if id == 0 {
            // No timer available: collapse now rather than leave the margin lingering.
            for b in self.all_bars_mut() {
                b.window.finish_linger();
            }
        }
    }

    /// The hovered app, if it has windows (and so a strip).
    fn hovered_strip_app(&self) -> Option<usize> {
        match self.hover {
            Some(Hit::App(i))
                if self
                    .cur_groups()
                    .get(i)
                    .is_some_and(|g| !g.windows.is_empty()) =>
            {
                Some(i)
            }
            _ => None,
        }
    }

    /// Arms or cancels the hover timers after the cursor moved.
    fn update_strip(&mut self) {
        let hovered = self.hovered_strip_app();
        if let Some(s) = &self.strip {
            let own = s.monitor == self.cur
                && hovered.is_some_and(|i| self.cur_groups()[i].key == s.group);
            self.cancel_show();
            if self.over_flyout || own {
                self.cancel_hide();
            } else if let Some(i) = hovered {
                // Another app while the strip is open: swap at once.
                self.open_strip(i);
            } else {
                self.arm_hide();
            }
        } else {
            match hovered {
                Some(i) if !self.show_suppressed => {
                    if self.show_pending != Some(i) {
                        self.arm_show(i);
                    }
                }
                _ => self.cancel_show(),
            }
        }
    }

    fn kill_timer(&self, id: usize) {
        // SAFETY: timer on our own window.
        unsafe {
            let _ = KillTimer(Some(self.notify.hwnd), id);
        }
    }

    fn arm_show(&mut self, i: usize) {
        // SAFETY: one-shot timer on our own window (killed when it fires); re-setting the id
        // restarts it.
        let id = unsafe {
            SetTimer(
                Some(self.notify.hwnd),
                TIMER_HOVER_SHOW,
                HOVER_SHOW_MS,
                None,
            )
        };
        self.show_pending = (id != 0).then_some(i);
    }

    fn cancel_show(&mut self) {
        if self.show_pending.take().is_some() {
            self.kill_timer(TIMER_HOVER_SHOW);
        }
    }

    fn arm_hide(&mut self) {
        if self.hide_pending {
            return;
        }
        // SAFETY: one-shot timer on our own window (killed when it fires).
        let id = unsafe {
            SetTimer(
                Some(self.notify.hwnd),
                TIMER_HOVER_HIDE,
                HOVER_HIDE_MS,
                None,
            )
        };
        self.hide_pending = id != 0;
        if id == 0 {
            // No timer available: close now rather than leave the strip open.
            self.close_strip();
        }
    }

    fn cancel_hide(&mut self) {
        if self.hide_pending {
            self.hide_pending = false;
            self.kill_timer(TIMER_HOVER_HIDE);
        }
    }

    /// Screen px of app `i`'s slot on bar `k`, spanning the full height of its surface.
    fn anchor_px(&self, k: usize, i: usize) -> Option<PxRect> {
        let mb = self.monitors.get(k)?;
        let r = mb.layout.apps.get(i)?;
        let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
        let s = mb
            .layout
            .surfaces
            .iter()
            .find(|s| cx >= s.x && cx < s.right() && cy >= s.y && cy < s.bottom())?;
        Some(to_px(
            Rect {
                x: r.x,
                y: s.y,
                w: r.w,
                h: s.h,
            },
            mb.scale(),
        ))
    }

    /// Opens the strip of app `i` of the cursor's bar now (replacing any other strip).
    fn open_strip(&mut self, i: usize) {
        self.cancel_show();
        self.cancel_hide();
        if let Some(k) = self.mon_index(self.cur) {
            self.build_strip(k, i);
        }
    }

    /// (Re)builds the strip of app `i` of bar `k` unless it already shows exactly that; the
    /// hover timers are left alone.
    fn build_strip(&mut self, k: usize, i: usize) {
        let Some(mb) = self.monitors.get(k) else {
            return;
        };
        let (monitor, dpi) = (mb.id(), mb.info.dpi);
        let Some(g) = mb.groups.get(i) else { return };
        if g.windows.is_empty() {
            self.close_strip();
            return;
        }
        let Some(anchor) = self.anchor_px(k, i) else {
            self.close_strip();
            return;
        };
        if self.strip.as_ref().is_some_and(|s| {
            s.monitor == monitor && s.group == g.key && s.windows == g.windows && s.anchor == anchor
        }) {
            return;
        }
        let (key, hwnds) = (g.key.clone(), g.windows.clone());
        let windows: Vec<(HWND, String)> = hwnds
            .iter()
            .map(|&h| (hwnd_of(h), self.windows.title(h).unwrap_or("").to_string()))
            .collect();
        let raw: Vec<HWND> = windows.iter().map(|(h, _)| *h).collect();
        let size = ThumbnailStrip::size_dip(&raw);
        let opened = Flyout::show(
            &self.comp,
            anchor,
            self.settings.position,
            size,
            dpi,
            &self.style,
        )
        .and_then(|flyout| {
            let thumbs = ThumbnailStrip::show(&flyout, &self.painter, self.style.text, &windows)?;
            Ok(Strip {
                thumbs,
                flyout,
                monitor,
                group: key,
                windows: hwnds,
                anchor,
            })
        });
        match opened {
            Ok(strip) => {
                // The new strip is up before the old one goes (no flicker on a swap).
                let old = self.strip.replace(strip);
                drop(old);
                self.over_flyout = false;
                self.reset_flyout_press();
            }
            Err(e) => eprintln!("vitrobar: thumbnail strip failed: {e}"),
        }
        if self.opts.debug_state {
            self.dump_state();
        }
    }

    fn close_strip(&mut self) {
        self.cancel_hide();
        if self.strip.take().is_some() {
            self.over_flyout = false;
            self.reset_flyout_press();
            if self.opts.debug_state {
                self.dump_state();
            }
        }
    }

    /// After the groups or the layout changed: follow the strip's group, or close it.
    fn sync_strip(&mut self) {
        let Some(s) = &self.strip else { return };
        let found = self.mon_index(s.monitor).and_then(|k| {
            let mb = &self.monitors[k];
            let i = mb.groups.iter().position(|g| g.key == s.group)?;
            (i < mb.layout.apps.len()).then_some((k, i))
        });
        match found {
            Some((k, i)) => self.build_strip(k, i),
            None => self.close_strip(),
        }
    }

    fn click(&mut self, hit: Hit, dirty: &mut Dirty) {
        if !matches!(hit, Hit::App(_)) {
            self.close_strip();
        }
        if hit != Hit::Quick {
            self.close_quick();
        }
        if hit != Hit::Clock {
            self.close_calendar();
        }
        if hit != Hit::Widget(0) {
            self.close_music_panel();
        }
        if hit != Hit::Widget(1) {
            self.close_claude_panel();
        }
        if hit != Hit::Chevron {
            self.close_overflow();
        }
        match hit {
            Hit::Chevron => {
                if !self.close_overflow() {
                    self.open_overflow();
                }
            }
            Hit::Tray(k) => {
                let idx = self.tray_bar.get(k).copied();
                self.tray_click(idx, TrayClick::Left);
            }
            Hit::Start => {
                if let Some(k) = self.mon_index(self.cur) {
                    self.toggle_start(k, dirty);
                }
            }
            // Vitrobar's own panel; a second click closes it.
            Hit::Quick => self.toggle_quick(dirty),
            Hit::Clock => self.toggle_calendar(dirty),
            Hit::App(i) => {
                let Some(g) = self.cur_groups().get(i) else {
                    return;
                };
                // The bar never takes focus, so the foreground window is the user's app.
                let action = click_action(g, foreground());
                if self.opts.debug_state {
                    eprintln!("vitrobar-click {i} {action:?}");
                }
                match action {
                    ClickAction::Launch(p) => {
                        if let Some(pin) = self.settings.pins.get(p) {
                            self.launcher.launch(Job::Pin(pin.clone()));
                        }
                    }
                    ClickAction::Activate(h) => {
                        if !activate_window(hwnd_of(h)) {
                            eprintln!("vitrobar: SetForegroundWindow refused for {h}");
                        }
                    }
                    ClickAction::Minimize(h) => minimize_window(hwnd_of(h)),
                    ClickAction::ShowThumbnails => {}
                }
                if action == ClickAction::ShowThumbnails {
                    self.open_strip(i);
                } else {
                    self.close_strip();
                }
            }
            // The overflow menu and show-desktop arrive in later tasks.
            // The music pill: its glyph plays / pauses, its text opens the detail panel.
            Hit::Widget(0) => {
                if self.on_music_glyph() {
                    if let Some(m) = &self.music {
                        m.act(MusicAction::PlayPause);
                    }
                } else {
                    self.toggle_music_panel(dirty);
                }
            }
            Hit::Widget(1) => self.toggle_claude_panel(dirty),
            Hit::Widget(_) => {}
            Hit::Ime => {
                vitro_win::ime::next_layout_for_foreground();
                // The switch is asynchronous: read it again a moment later.
                // SAFETY: timer on our own window; setting the id again replaces it.
                unsafe {
                    SetTimer(Some(self.notify.hwnd), TIMER_IME, IME_REREAD_MS, None);
                }
            }
            Hit::More | Hit::ShowDesktop => {}
        }
    }

    /// Ends every transient interaction (strip, overflow, tooltip, hover, presses, margin
    /// expansion): before a modal menu and before the bar is laid out anew.
    fn reset_interaction(&mut self) {
        // Every caller but `open_start` redraws all bars (which drops the ring).
        self.keys.set_focus(None);
        self.close_start();
        self.close_quick();
        self.close_calendar();
        self.close_music_panel();
        self.close_claude_panel();
        self.close_strip();
        self.close_overflow();
        self.set_tip_target(None);
        self.cancel_show();
        self.hover = None;
        self.pressed = None;
        self.rpressed = None;
        // The modal menu loop swallows WM_MOUSELEAVE: start tracking afresh on the next move.
        self.tracking = None;
        self.show_suppressed = false;
        self.kill_timer(TIMER_MARGIN_COLLAPSE);
        self.collapse_pending = false;
        for b in self.all_bars_mut() {
            b.window.set_expanded(false);
        }
    }

    // -----------------------------------------------------------------------------------
    // The Windows key (hook and focus-mode navigation in `keys.rs`; here the actions)
    // -----------------------------------------------------------------------------------

    /// The bar Windows-key actions apply to: the monitor showing the foreground window,
    /// else the primary monitor's.
    fn key_monitor(&self) -> usize {
        // SAFETY: plain query.
        let fg = unsafe { GetForegroundWindow() };
        window_monitor(fg)
            .and_then(|id| self.mon_index(id))
            .or_else(|| self.monitors.iter().position(|m| m.tray))
            .unwrap_or(0)
    }

    fn on_hook_output(&mut self, out: HookOutput, dirty: &mut Dirty) {
        // Never logged: what the user typed stays between the hook and this match.
        match out {
            HookOutput::OpenStart(at) => {
                self.leave_focus(dirty);
                // SAFETY: plain query.
                if !open_start_fresh(at, unsafe { GetTickCount() }) {
                    // The UI thread was too long busy: a late Start would surprise.
                    return;
                }
                let k = self.key_monitor();
                // The open time is measured from the Windows-key up.
                self.input_time = at;
                self.toggle_start(k, dirty);
                if let Some(sp) = self.start.as_mut().filter(|s| !s.activated) {
                    // The mask key the hook injected is normally the latest input, which
                    // permits the foreground change. If Windows still refused: one more
                    // mask key, one more try (bounded); else the panel takes the foreground
                    // on its first click (carry 12).
                    inject_mask_key();
                    if !sp.activate() {
                        eprintln!("vitrobar: the Start panel could not take the foreground");
                    }
                }
            }
            HookOutput::LaunchNth(n) => {
                self.leave_focus(dirty);
                let k = self.key_monitor();
                let i = usize::from(n.max(1)) - 1;
                if i < self.focus_counts(k).apps {
                    self.close_start();
                    self.launch_app(k, i);
                }
            }
            HookOutput::FocusApps => self.enter_focus(Zone::Apps, dirty),
            HookOutput::FocusTray => self.enter_focus(Zone::Tray, dirty),
            HookOutput::FocusKey(vk) => self.focus_key(vk, dirty),
            HookOutput::ExitFocus => self.leave_focus(dirty),
        }
    }

    /// `Settings.win_key_opens_start` changed: off removes the hook and the focus ring (if
    /// any); on installs the hook.
    fn set_win_key(&mut self, on: bool, dirty: &mut Dirty) {
        if let Some(m) = self.keys.set_enabled(on) {
            dirty.bars.insert(m);
        }
    }

    /// What focus mode can land on in bar `k`.
    fn focus_counts(&self, k: usize) -> Counts {
        let mb = &self.monitors[k];
        Counts {
            apps: mb.layout.apps.len().min(mb.groups.len()),
            tray: if mb.tray {
                self.tray_bar.len().min(mb.layout.tray.len())
            } else {
                0
            },
            chevron: mb.tray && !self.tray_over.is_empty(),
        }
    }

    /// Win+T / Win+B: the focus ring goes on the first app / tray icon of the bar of the
    /// foreground window's monitor (the tray: of the primary bar, which has the tray area).
    fn enter_focus(&mut self, zone: Zone, dirty: &mut Dirty) {
        self.leave_focus(dirty);
        self.close_start();
        self.close_strip();
        if self.close_overflow() {
            dirty.visuals = true;
        }
        let mut k = self.key_monitor();
        if zone == Zone::Tray && !self.monitors[k].tray {
            k = self.monitors.iter().position(|m| m.tray).unwrap_or(k);
        }
        let item = if self.monitors[k].fullscreen {
            None
        } else {
            first_in(zone, self.focus_counts(k))
        };
        if let Some(item) = item {
            let monitor = self.monitors[k].id();
            self.keys.set_focus(Some(Focus {
                monitor,
                zone,
                item,
            }));
            dirty.bars.insert(monitor);
        }
        // Nothing to focus: `leave_focus` above already told the hook (which entered focus
        // mode by itself) to leave it.
    }

    /// Ends the keyboard focus mode (also tells the hook, which may have entered it alone).
    fn leave_focus(&mut self, dirty: &mut Dirty) {
        if let Some(f) = self.keys.focus() {
            dirty.bars.insert(f.monitor);
        }
        self.keys.set_focus(None);
    }

    fn focus_key(&mut self, vk: u32, dirty: &mut Dirty) {
        let Some(f) = self.keys.focus() else {
            // The hook entered focus mode, the app never did (nothing to focus).
            self.keys.set_focus(None);
            return;
        };
        let Some(k) = self.mon_index(f.monitor) else {
            self.leave_focus(dirty);
            return;
        };
        match step(f.zone, f.item, vk, self.focus_counts(k)) {
            Step::Move(zone, item) => {
                self.keys.set_focus(Some(Focus { zone, item, ..f }));
                dirty.bars.insert(f.monitor);
            }
            Step::Click(item) => {
                self.leave_focus(dirty);
                self.focus_click(k, item, false, dirty);
            }
            Step::Menu(item) => {
                self.leave_focus(dirty);
                self.focus_click(k, item, true, dirty);
            }
            Step::Stay => {}
            Step::Exit => self.leave_focus(dirty),
        }
    }

    /// Screen px of the centre of `r` (DIP) on bar `k`.
    fn centre_px(&self, k: usize, r: Rect) -> (i32, i32) {
        let s = self.monitors[k].scale();
        (
            ((r.x + r.w / 2.0) * s).round() as i32,
            ((r.y + r.h / 2.0) * s).round() as i32,
        )
    }

    /// Enter / Space (`menu` false) or the Apps key (`menu` true) on a focused item: what a
    /// left / right click on it does.
    fn focus_click(&mut self, k: usize, item: BarFocus, menu: bool, dirty: &mut Dirty) {
        let layout = &self.monitors[k].layout;
        match item {
            BarFocus::App(i) => {
                let Some(r) = layout.apps.get(i).copied() else {
                    return;
                };
                if menu {
                    let id = self.monitors[k].id();
                    self.switch_cur(id, dirty);
                    self.msg_pt = self.centre_px(k, r);
                    self.context_menu(Some(i), dirty);
                } else {
                    self.launch_app(k, i);
                }
            }
            BarFocus::Tray(i) => {
                let Some(r) = layout.tray.get(i).copied() else {
                    return;
                };
                self.msg_pt = self.centre_px(k, r);
                let idx = self.tray_bar.get(i).copied();
                let kind = if menu {
                    TrayClick::Right
                } else {
                    TrayClick::Left
                };
                self.tray_click(idx, kind);
            }
            BarFocus::Chevron => {
                if !self.close_overflow() {
                    self.open_overflow();
                }
                dirty.bars.insert(self.monitors[k].id());
            }
        }
    }

    /// Win+n / Enter on app `i` of bar `k`: launch, activate (the next window of a group
    /// with several) or minimize.
    fn launch_app(&mut self, k: usize, i: usize) {
        let Some(g) = self.monitors[k].groups.get(i) else {
            return;
        };
        match launch_action(g, foreground()) {
            ClickAction::Launch(p) => {
                if let Some(pin) = self.settings.pins.get(p) {
                    self.launcher.launch(Job::Pin(pin.clone()));
                }
            }
            ClickAction::Activate(h) => {
                // As for the Start panel: one more mask key and one more try, bounded.
                if !activate_window(hwnd_of(h)) {
                    inject_mask_key();
                    if !activate_window(hwnd_of(h)) {
                        eprintln!("vitrobar: SetForegroundWindow refused for {h}");
                    }
                }
            }
            ClickAction::Minimize(h) => minimize_window(hwnd_of(h)),
            ClickAction::ShowThumbnails => {}
        }
        self.close_strip();
    }

    // -----------------------------------------------------------------------------------
    // Start panel (controller in `start.rs`; here only the wiring)
    // -----------------------------------------------------------------------------------

    /// The Start button (and, later, the Windows key): opens the Start panel on monitor `k`,
    /// or closes it when it is already open there (open elsewhere, it moves to `k`).
    /// `StartStyle::FullScreen` opens the full-screen app grid instead of the panel.
    fn toggle_start(&mut self, k: usize, dirty: &mut Dirty) {
        let Some(id) = self.monitors.get(k).map(MonitorBar::id) else {
            return;
        };
        if let Some(open) = self.start.as_ref().map(|s| s.monitor) {
            if open == id {
                self.close_start();
                return;
            }
            // Moving to another monitor: no fade for a panel that is about to reappear.
            self.close_start_now();
        }
        self.open_start(k, dirty);
    }

    fn open_start(&mut self, k: usize, dirty: &mut Dirty) {
        let began = Instant::now();
        // A panel still fading out goes at once: never two panels, never an invisible one.
        self.drop_start_closing();
        if let Some(f) = self.keys.focus() {
            dirty.bars.insert(f.monitor);
        }
        // Strip, overflow, tooltip and the keyboard focus mode close; the hover ends.
        self.reset_interaction();
        dirty.bars.insert(self.monitors[k].id());
        if !self.settings.start_pins_initialized {
            // First open: Start begins with the bar's pins (a file from before the flag that
            // already lists Start pins keeps them). Unpinning everything later stays empty.
            if self.settings.start_pins.is_empty() {
                self.settings.start_pins = self.settings.pins.clone();
            }
            self.settings.start_pins_initialized = true;
            self.save_settings();
        }
        let mb = &self.monitors[k];
        let s = mb.scale();
        let (l, t, r, b) = work_area_px(mb.info.rect_px);
        let work = Rect {
            x: l as f32 / s,
            y: t as f32 / s,
            w: (r - l) as f32 / s,
            h: (b - t) as f32 / s,
        };
        let full_screen = self.settings.start_style == StartStyle::FullScreen;
        let rect = if full_screen {
            work
        } else {
            place_panel(mb.layout.start, self.settings.position, work)
        };
        let layout = panel_layout(rect);
        let (id, dpi) = (mb.id(), mb.info.dpi);
        let fg_before = foreground().unwrap_or(0) as usize;
        // Cached data draws the first frame; the workers refresh it in the background.
        self.start_data.refresh(self.notify.hwnd);
        self.start_data.drain();
        let lang = ui_lang(&self.settings);
        let before_show = began.elapsed();
        let opened = if full_screen {
            // The monitor's work area exactly (the bar's strip stays visible).
            StartPanel::open_grid(
                &self.comp,
                (l, t, r, b),
                dpi,
                &self.style,
                id,
                tr(lang, Key::Start),
            )
        } else {
            StartPanel::open(
                &self.comp,
                &layout,
                to_px(rect, s),
                dpi,
                &self.style,
                id,
                tr(lang, Key::Start),
            )
        };
        let shown = began.elapsed();
        let mut drawn = shown;
        match opened {
            Ok(mut sp) => {
                sp.fg_at_open = fg_before;
                // Query revisions continue after the last panel's.
                let state = crate::start::StartState::with_rev(self.start_data.last_rev + 1);
                sp.state = if full_screen {
                    state.into_grid()
                } else {
                    state
                };
                if full_screen {
                    // Every app is listed from the first frame.
                    sp.research(&self.start_data, lang, true);
                }
                if self.sys.animations {
                    let (ms, motion) = if full_screen {
                        (GRID_OPEN_MS, Motion::Grow)
                    } else {
                        (
                            START_OPEN_MS,
                            Motion::Rise {
                                from: self.settings.position,
                            },
                        )
                    };
                    if let Err(e) = sp.animate_open(ms, motion) {
                        eprintln!("vitrobar: the Start panel could not animate in: {e}");
                        // Never activate an invisible panel: it would hold the keyboard.
                        sp.show_static();
                    }
                }
                self.start = Some(sp);
                self.start_draw();
                drawn = began.elapsed();
                if let Some(sp) = self.start.as_mut() {
                    // Carry 12: Windows may refuse the foreground; the panel then takes it
                    // on its first click.
                    if !sp.activate() {
                        eprintln!("vitrobar: the Start panel could not take the foreground");
                    }
                }
            }
            Err(e) => {
                eprintln!("vitrobar: the Start panel failed ({e}); opening the stock Start");
                open_stock_start();
            }
        }
        if self.opts.debug_state {
            // SAFETY: plain query.
            let now = unsafe { GetTickCount() };
            eprintln!(
                "vitrobar-start-open ms={} build_ms={} (prepare {} show {} draw {} activate {}) activated={}",
                now.wrapping_sub(self.input_time),
                began.elapsed().as_millis(),
                before_show.as_millis(),
                (shown - before_show).as_millis(),
                (drawn - shown).as_millis(),
                (began.elapsed() - drawn).as_millis(),
                self.start.as_ref().is_some_and(|s| s.activated)
            );
            self.dump_state();
        }
    }

    /// Closes the Start panel: it fades out (with "Animation effects" on) and its window
    /// goes a moment later; its icons leave the cache (unless a bar shows them) at once.
    /// True if it was open.
    fn close_start(&mut self) -> bool {
        self.close_start_as(true)
    }

    /// A closing panel (Start, the quick panel) must not keep the keyboard: if `panel` is the
    /// foreground window, hands the foreground back to `fg_at_open`, the window that had it
    /// before the panel opened, so keys typed right after Esc / the Win key / the button
    /// reach it (animated close or not). False when it cannot (that window is gone, hidden,
    /// cloaked, Explorer's hidden taskbar, or Windows refuses): the caller then drops the
    /// panel at once, so Windows picks the next window.
    fn hand_back_foreground(&self, panel: HWND, fg_at_open: usize) -> bool {
        // SAFETY: plain queries and a foreground request for a window we only test.
        unsafe {
            if GetForegroundWindow() != panel {
                return true;
            }
            let prev = HWND(fg_at_open as *mut _);
            fg_at_open != 0
                && !self.stock.watches(fg_at_open as u64)
                && can_take_foreground(prev)
                && SetForegroundWindow(prev).as_bool()
                && GetForegroundWindow() != panel
        }
    }

    /// [`Self::close_start`] without the animation, for when the panel cannot be kept
    /// (monitor, theme or device change, Explorer restart): the window goes now, along with
    /// any panel still fading.
    fn close_start_now(&mut self) -> bool {
        self.drop_start_closing();
        self.close_start_as(false)
    }

    fn close_start_as(&mut self, animate: bool) -> bool {
        let Some(sp) = self.start.take() else {
            return false;
        };
        // Only one panel fades at a time.
        self.drop_start_closing();
        let srcs = sp.icon_srcs.clone();
        // The next panel's query revisions continue after this one's.
        self.start_data.last_rev = self.start_data.last_rev.max(sp.state.query_rev);
        let handed_back = !animate || self.hand_back_foreground(sp.hwnd(), sp.fg_at_open);
        if animate && self.sys.animations && handed_back {
            let (ms, motion) = if sp.state.grid {
                (GRID_CLOSE_MS, Motion::Grow)
            } else {
                (
                    START_CLOSE_MS,
                    Motion::Rise {
                        from: self.settings.position,
                    },
                )
            };
            match sp.animate_close(ms, motion) {
                Ok(()) => {
                    self.start_closing = Some(sp);
                    self.start_closing_until = Instant::now()
                        + Duration::from_millis(u64::from(ms + START_CLOSED_SLACK_MS));
                    // SAFETY: one-shot timer on our own window (killed when it fires).
                    unsafe {
                        SetTimer(
                            Some(self.notify.hwnd),
                            TIMER_START_CLOSED,
                            ms + START_CLOSED_SLACK_MS,
                            None,
                        )
                    };
                }
                // Dropped at once, as when animations are off.
                Err(e) => eprintln!("vitrobar: the Start panel could not fade out: {e}"),
            }
        } else {
            drop(sp);
        }
        let monitors = &self.monitors;
        self.icons.forget(|src| {
            srcs.contains(src)
                && !monitors
                    .iter()
                    .any(|m| m.groups.iter().any(|g| icon_sources(g).contains(src)))
        });
        if let Some(fs) = &self.start_data.files {
            // Stops a search still running (revision 0 is never current).
            fs.query(0, String::new());
        }
        self.surrogate = None;
        if self.opts.debug_state {
            self.dump_state();
        }
        true
    }

    /// Destroys the Start panel that is fading out, if any.
    fn drop_start_closing(&mut self) {
        if self.start_closing.take().is_some() {
            self.kill_timer(TIMER_START_CLOSED);
        }
    }

    fn start_look(&self) -> (Rgba, Lang) {
        (
            rgb(self.settings.accent.unwrap_or(self.sys.accent)),
            ui_lang(&self.settings),
        )
    }

    /// Redraws the open Start panel (only what changed is drawn again).
    fn start_render(&mut self) {
        if self.start_draw() && self.opts.debug_state {
            self.dump_state();
        }
    }

    /// [`Self::start_render`] without the debug state dump (the open path dumps once, after
    /// measuring). False when the panel is not open or could not be drawn (it is then closed).
    fn start_draw(&mut self) -> bool {
        let (accent, lang) = self.start_look();
        let Some(sp) = self.start.as_mut() else {
            return false;
        };
        let look = Look {
            style: &self.style,
            accent,
            lang,
        };
        let drawn = sp.render(
            &self.painter,
            &mut self.icons,
            &self.start_data,
            &self.settings.start_pins,
            &look,
        );
        if let Err(e) = drawn {
            eprintln!("vitrobar: the Start panel could not be drawn: {e}");
            self.close_start_now();
            return false;
        }
        true
    }

    /// The catalog, recent files or account name arrived.
    /// The Start data changed; `keep` is the selection's identity from before the change.
    fn start_refresh(&mut self, apps_changed: bool, keep: Option<String>) {
        let lang = ui_lang(&self.settings);
        let Some(sp) = self.start.as_mut() else {
            return;
        };
        if apps_changed {
            sp.research_kept(&self.start_data, lang, keep);
        }
        self.start_render();
    }

    /// File-search results arrived (always drained, so the next wake-up is posted).
    fn on_file_results(&mut self) {
        let Some((rev, hits)) = self.start_data.files.as_ref().and_then(|f| f.drain()) else {
            return;
        };
        let lang = ui_lang(&self.settings);
        let Some(sp) = self.start.as_mut() else {
            return;
        };
        if sp.files_arrived(rev, hits, &self.start_data, lang) {
            self.start_render();
        }
    }

    /// Feeds `input` to the panel's state machine and carries out what it asks for.
    fn start_input(&mut self, input: StartInput, dirty: &mut Dirty) {
        let lang = ui_lang(&self.settings);
        let Some(sp) = self.start.as_mut() else {
            return;
        };
        let action = sp.handle(input, &self.start_data, &self.settings.start_pins, lang);
        if let Some((rev, text)) = sp.research(&self.start_data, lang, false) {
            if let Some(fs) = &self.start_data.files {
                fs.query(rev, text);
            }
        }
        self.start_action(action, dirty);
    }

    fn start_action(&mut self, action: StartAction, dirty: &mut Dirty) {
        let debug = self.opts.debug_state;
        match action {
            StartAction::None => {}
            StartAction::Redraw => self.start_render(),
            StartAction::Close => {
                self.close_start();
            }
            StartAction::Launch(p) => {
                self.close_start();
                if debug {
                    eprintln!("vitrobar-start-launch {}", json_str(&p.name));
                }
                if let Some(job) = launch_job(&p) {
                    self.launcher.launch(job);
                }
            }
            StartAction::Open(target) => {
                self.close_start();
                if debug {
                    eprintln!("vitrobar-start-open-item {}", json_str(&target));
                }
                self.launcher.launch(Job::Open(target));
            }
            StartAction::Power(a) => {
                self.close_start();
                if debug {
                    eprintln!("vitrobar-power {a:?}");
                }
                self.launcher.launch(Job::Power(a));
            }
            StartAction::PowerMenu(pt) => {
                let Some(sp) = &self.start else { return };
                let (l, t, _, _) = sp.rect_px();
                let lang = ui_lang(&self.settings);
                let at = (l + pt.0, t + pt.1);
                let choice = power_menu(&self.menu_ctx(at), sp.hwnd(), at, lang);
                if let Some(a) = choice {
                    self.start_action(StartAction::Power(a), dirty);
                }
            }
            StartAction::CopyMath(text) => {
                if let Err(e) = vitro_win::clipboard::set_text(self.notify.hwnd, &text) {
                    eprintln!("vitrobar: could not copy the result: {e}");
                } else if debug {
                    eprintln!("vitrobar-start-copied {}", text.chars().count());
                }
                self.close_start();
            }
            StartAction::Menu(pt, item) => self.start_item_menu(pt, item, dirty),
        }
    }

    /// Right-click on a Start pin or an All-apps entry: pin / unpin (Start and bar), open
    /// the file location. Changes are saved at once.
    fn start_item_menu(&mut self, pt: (i32, i32), item: MenuItem, dirty: &mut Dirty) {
        let Some(sp) = &self.start else { return };
        let pin = match item {
            MenuItem::Pin(i) => self.settings.start_pins.get(i).cloned(),
            MenuItem::App(a) => self.start_data.apps.get(a).cloned(),
        };
        let Some(pin) = pin else { return };
        let bar = bar_pin(&pin);
        let on_bar = |p: &Pin| same_app(p, &pin) || same_app(p, &bar);
        let in_start = self.settings.start_pins.iter().any(|p| same_app(p, &pin));
        let in_bar = self.settings.pins.iter().any(on_bar);
        let location = file_location(&pin);
        let (l, t, _, _) = sp.rect_px();
        let lang = ui_lang(&self.settings);
        let at = (l + pt.0, t + pt.1);
        let choice = start_item_menu(
            &self.menu_ctx(at),
            sp.hwnd(),
            at,
            in_start,
            in_bar,
            location.is_some(),
            lang,
        );
        if self.opts.debug_state {
            eprintln!("vitrobar-menu start {choice:?}");
        }
        match choice {
            Some(MenuChoice::PinToStart) if !in_start => {
                self.settings.start_pins.push(pin);
                self.save_settings();
            }
            Some(MenuChoice::UnpinFromStart) => {
                self.settings.start_pins.retain(|p| !same_app(p, &pin));
                self.save_settings();
            }
            Some(MenuChoice::Pin) if !in_bar => {
                self.settings.pins.push(bar);
                self.pins_changed(dirty);
            }
            Some(MenuChoice::Unpin) => {
                self.settings.pins.retain(|p| !on_bar(p));
                self.pins_changed(dirty);
            }
            Some(MenuChoice::OpenFileLocation) => {
                if let Some(path) = location {
                    self.close_start();
                    self.launcher.launch(Job::Reveal(path));
                }
                return;
            }
            _ => return,
        }
        if let Some(sp) = self.start.as_mut() {
            sp.state.hover = None;
        }
        self.start_render();
    }

    /// Mouse input on the Start panel (client px).
    fn on_start_mouse(&mut self, msg: &MSG, dirty: &mut Dirty) -> bool {
        let pt = mouse_pos(msg.lParam);
        let Some(sp) = self.start.as_mut() else {
            return false;
        };
        let input = match msg.message {
            WM_MOUSEMOVE => {
                if !sp.tracking {
                    track_leave(msg.hwnd);
                    sp.tracking = true;
                }
                StartInput::Move(pt)
            }
            WM_MOUSELEAVE => {
                sp.tracking = false;
                StartInput::Leave
            }
            WM_LBUTTONUP => StartInput::Click(pt),
            WM_RBUTTONUP => StartInput::RightClick(pt),
            WM_LBUTTONDOWN | WM_RBUTTONDOWN => return true,
            _ => return false,
        };
        self.start_input(input, dirty);
        true
    }

    /// A key on the Start panel. Ctrl+V pastes the clipboard's text.
    fn on_start_key(&mut self, msg: &MSG, dirty: &mut Dirty) {
        self.input_time = msg.time;
        let vk = msg.wParam.0 as u32;
        // SAFETY: plain queries of this thread's keyboard state.
        let (ctrl, alt) = unsafe {
            (
                GetKeyState(i32::from(VK_CONTROL.0)) < 0,
                GetKeyState(i32::from(VK_MENU.0)) < 0,
            )
        };
        // AltGr (Ctrl+Alt) types characters: it is not Ctrl.
        let ctrl = ctrl && !alt;
        if ctrl && vk == u32::from(b'V') {
            let text = self
                .start
                .as_ref()
                .and_then(|s| vitro_win::clipboard::get_text(s.hwnd(), 4096));
            if let Some(text) = text {
                self.start_input(StartInput::Paste(text), dirty);
            }
            return;
        }
        self.start_input(StartInput::Key(vk, ctrl), dirty);
    }

    /// `WM_CHAR`, `WM_MOUSEWHEEL` and `WM_SYSKEYDOWN` on the Start panel; false for anything
    /// else (it is dispatched as usual).
    fn on_start_msg(&mut self, msg: &MSG, dirty: &mut Dirty) -> bool {
        if !self.start.as_ref().is_some_and(|s| s.hwnd() == msg.hwnd) {
            return false;
        }
        match msg.message {
            WM_CHAR => {
                let u = msg.wParam.0 as u16;
                let c = match u {
                    0xD800..=0xDBFF => {
                        self.surrogate = Some(u);
                        return true;
                    }
                    0xDC00..=0xDFFF => {
                        let Some(hi) = self.surrogate.take() else {
                            return true;
                        };
                        char::decode_utf16([hi, u]).next().and_then(|r| r.ok())
                    }
                    _ => {
                        // A lone high surrogate is dropped by anything but its low half.
                        self.surrogate = None;
                        char::from_u32(u32::from(u))
                    }
                };
                // Control characters (Ctrl+letters, Backspace, Esc, DEL) come as keys.
                if let Some(c) = c.filter(|c| *c >= ' ' && *c != '\u{7F}') {
                    self.start_input(StartInput::Char(c), dirty);
                }
                true
            }
            WM_MOUSEWHEEL => {
                let delta = ((msg.wParam.0 >> 16) & 0xFFFF) as u16 as i16;
                self.start_input(StartInput::Wheel(i32::from(delta)), dirty);
                true
            }
            WM_SYSKEYDOWN if msg.wParam.0 == usize::from(VK_F4.0) => {
                // Alt+F4 closes the panel (and not the window behind our back).
                self.close_start();
                true
            }
            _ => false,
        }
    }

    // -----------------------------------------------------------------------------------
    // The calendar panel (controller in `calendar_panel.rs`)
    // -----------------------------------------------------------------------------------
    /// A second click on the clock of the monitor the panel is on closes it; a click on
    /// another monitor's clock moves it there.
    fn toggle_calendar(&mut self, dirty: &mut Dirty) {
        let here = self.cur;
        let was = self.calendar.as_ref().map(|c| c.monitor);
        self.close_calendar();
        if was != Some(here) {
            self.open_calendar(here, dirty);
        }
    }

    /// Opens the calendar above the clock of the bar of monitor `id` (below it with the bar
    /// at the top) and gives it the foreground. If it cannot open, the stock notification
    /// centre does.
    fn open_calendar(&mut self, id: u64, dirty: &mut Dirty) {
        if let Some(f) = self.keys.focus() {
            dirty.bars.insert(f.monitor);
        }
        // Start, quick, strip, overflow, tooltip and the keyboard focus mode close.
        self.reset_interaction();
        let Some(mb) = self.monitors.iter().find(|m| m.id() == id) else {
            return;
        };
        dirty.bars.insert(id);
        let anchor = clock_anchor(&mb.layout, mb.scale());
        let (accent, lang) = self.start_look();
        let fg = foreground().unwrap_or(0) as usize;
        let opened = match anchor {
            Some(anchor) => CalendarPanel::open(
                &Gfx {
                    comp: &self.comp,
                    painter: &self.painter,
                },
                &self.style,
                mb.info.dpi,
                anchor,
                self.settings.position,
                accent,
                lang,
                self.notify.hwnd,
                id,
            ),
            None => Err(Error::new(E_FAIL, "no clock on the bar")),
        };
        match opened {
            Ok(mut c) => {
                c.fg_at_open = fg;
                if !c.activate() {
                    eprintln!("vitrobar: the calendar panel could not take the foreground");
                }
                if self.opts.debug_state {
                    eprintln!(
                        "vitrobar-calendar-open rect={:?} anchor={:?} activated={}",
                        c.rect_px(),
                        anchor,
                        c.activated
                    );
                }
                self.calendar = Some(c);
            }
            Err(e) => {
                eprintln!("vitrobar: the calendar panel failed ({e}); opening the stock one");
                self.launcher.launch(Job::Notifications);
            }
        }
    }

    /// True if it was open. Its timer goes with it. The foreground goes back to the window
    /// that had it before the panel opened (same rules as Start and the quick panel).
    fn close_calendar(&mut self) -> bool {
        self.close_calendar_as(true)
    }

    /// [`Self::close_calendar`] for when the panel cannot be kept (Explorer restart, GPU
    /// loss, monitor change): no foreground hand-back.
    fn close_calendar_now(&mut self) -> bool {
        self.close_calendar_as(false)
    }

    fn close_calendar_as(&mut self, hand_back: bool) -> bool {
        let Some(c) = self.calendar.take() else {
            return false;
        };
        if hand_back {
            let _ = self.hand_back_foreground(c.hwnd(), c.fg_at_open);
        }
        drop(c);
        if self.opts.debug_state {
            eprintln!("vitrobar-calendar-closed");
        }
        true
    }

    /// Runs `f` on the open calendar and acts on what it asks for.
    fn calendar_event(&mut self, f: impl FnOnce(&mut CalendarPanel, &Gfx) -> CalOutcome) {
        let Some(c) = self.calendar.as_mut() else {
            return;
        };
        let g = Gfx {
            comp: &self.comp,
            painter: &self.painter,
        };
        match f(c, &g) {
            CalOutcome::None => {}
            CalOutcome::Close => {
                self.close_calendar();
            }
            CalOutcome::Notifications => {
                self.close_calendar();
                self.launcher.launch(Job::Notifications);
            }
            CalOutcome::Open(uri) => {
                self.close_calendar();
                self.launcher.launch(Job::Open(uri));
            }
        }
    }

    fn on_calendar_key(&mut self, msg: &MSG) {
        let vk = msg.wParam.0 as u16;
        // SAFETY: plain query of this thread's keyboard state.
        let shift = unsafe { GetKeyState(i32::from(VK_SHIFT.0)) < 0 };
        self.calendar_event(|c, g| c.on_key(vk, shift, g));
    }

    /// `WM_MOUSEWHEEL` and Alt+F4 on the calendar; false for anything else.
    fn on_calendar_msg(&mut self, msg: &MSG) -> bool {
        if !self.calendar.as_ref().is_some_and(|c| c.hwnd() == msg.hwnd) {
            return false;
        }
        match msg.message {
            WM_MOUSEWHEEL => {
                let delta = ((msg.wParam.0 >> 16) & 0xFFFF) as u16 as i16;
                let pt = mouse_pos(msg.lParam);
                self.calendar_event(|c, g| c.on_wheel(delta, pt, g));
                true
            }
            WM_SYSKEYDOWN if msg.wParam.0 == usize::from(VK_F4.0) => {
                self.close_calendar();
                true
            }
            _ => false,
        }
    }

    fn calendar_timer(&mut self, id: usize) {
        if self.calendar.is_none() {
            // Queued before the panel closed.
            self.kill_timer(id);
            return;
        }
        self.calendar_event(|c, g| c.on_timer(id, g).unwrap_or(CalOutcome::None));
    }

    /// Like [`Self::quick_foreground_changed`].
    fn calendar_foreground_changed(&mut self, fg: Option<Hwnd>) {
        let Some(c) = self.calendar.as_mut() else {
            return;
        };
        if c.activated {
            return;
        }
        let fg = fg.unwrap_or(0) as usize;
        if fg == c.hwnd().0 as usize {
            c.activated = true;
        } else if fg != c.fg_at_open {
            self.close_calendar();
        }
    }

    /// Moves the open calendar to where the clock of bar `k` is now (when it is that bar's).
    fn calendar_reanchor(&mut self, k: usize) {
        let mb = &self.monitors[k];
        if !self.calendar.as_ref().is_some_and(|c| c.monitor == mb.id()) {
            return;
        }
        let Some(anchor) = clock_anchor(&mb.layout, mb.scale()) else {
            self.close_calendar();
            return;
        };
        if let Some(c) = self.calendar.as_mut() {
            if let Err(e) = c.reanchor(&self.comp, anchor) {
                eprintln!("vitrobar: the calendar panel could not follow the clock: {e}");
                self.close_calendar();
            }
        }
    }

    fn calendar_restyle(&mut self) {
        let (accent, _) = self.start_look();
        let Some(c) = self.calendar.as_mut() else {
            return;
        };
        let g = Gfx {
            comp: &self.comp,
            painter: &self.painter,
        };
        if let Err(e) = c.restyle(&g, &self.style, accent) {
            eprintln!("vitrobar: the calendar panel could not be restyled: {e}");
            self.close_calendar();
        }
    }

    // -----------------------------------------------------------------------------------
    // The music widget (controller in `widgets.rs`)
    // -----------------------------------------------------------------------------------

    /// Opens the media service when the widget is on and drops it when it is off (P4).
    fn sync_music(&mut self) {
        match (self.settings.widget_music, self.music.is_some()) {
            (true, false) => match Music::open(self.notify.hwnd) {
                Ok(mut m) => {
                    m.remeasure(&self.painter);
                    self.music = Some(m);
                }
                Err(e) => eprintln!("vitrobar: the music widget could not start: {e}"),
            },
            (false, true) => {
                self.close_music_panel();
                self.music = None;
            }
            _ => {}
        }
    }

    /// [`WM_APP_MEDIA`](crate::notify::WM_APP_MEDIA) arrived: the pill and the open panel
    /// follow the session.
    fn on_media(&mut self, dirty: &mut Dirty) {
        let Some(m) = self.music.as_mut() else {
            return;
        };
        let wake = m.on_wake(&self.painter);
        if wake.pill {
            // The pill appears, leaves or changes width: every bar is laid out anew.
            dirty.visuals = true;
        }
        let view = m.view.clone();
        if wake.session {
            match view {
                None => {
                    // The session ended: nothing left to show.
                    self.close_music_panel();
                }
                Some(v) => {
                    self.music_event(|p, g| p.set_data(g, v));
                    self.music_request_art();
                }
            }
        }
        if wake.thumbnail {
            let art = self.music.as_ref().and_then(|m| m.take_thumbnail());
            self.music_event(|p, g| p.set_art(g, art));
        }
    }

    /// The press on the music pill was on its glyph (play / pause) rather than its text.
    fn on_music_glyph(&self) -> bool {
        let Some(mb) = self.tray_monitor() else {
            return false;
        };
        let Some(r) = mb.layout.widgets[0] else {
            return false;
        };
        on_pill_glyph(self.msg_pt.0 as f32 / mb.scale() - r.x)
    }

    fn toggle_music_panel(&mut self, dirty: &mut Dirty) {
        if !self.close_music_panel() {
            self.open_music_panel(dirty);
        }
    }

    /// Opens the detail panel above the music pill (below it with the bar at the top) and
    /// gives it the foreground; asks for the artwork.
    fn open_music_panel(&mut self, dirty: &mut Dirty) {
        if let Some(f) = self.keys.focus() {
            dirty.bars.insert(f.monitor);
        }
        self.reset_interaction();
        let Some(view) = self.music.as_ref().and_then(|m| m.view.clone()) else {
            return;
        };
        let Some(mb) = self.tray_monitor() else {
            return;
        };
        if mb.layout.widgets[0].is_none() {
            return;
        }
        let Some(anchor) = surface_anchor(&mb.layout, AnchorTarget::Widget(0), mb.scale()) else {
            return;
        };
        let (id, dpi) = (mb.id(), mb.info.dpi);
        dirty.bars.insert(id);
        let (accent, lang) = self.start_look();
        let fg = foreground().unwrap_or(0) as usize;
        let opened = MusicPanel::open(
            &Gfx {
                comp: &self.comp,
                painter: &self.painter,
            },
            &self.style,
            dpi,
            anchor,
            self.settings.position,
            accent,
            lang,
            view,
        );
        match opened {
            Ok(mut p) => {
                p.fg_at_open = fg;
                if !p.activate() {
                    eprintln!("vitrobar: the music panel could not take the foreground");
                }
                if self.opts.debug_state {
                    eprintln!(
                        "vitrobar-music-open rect={:?} anchor={:?} activated={}",
                        p.rect_px(),
                        anchor,
                        p.activated
                    );
                }
                self.music_panel = Some(p);
                self.music_request_art();
            }
            Err(e) => eprintln!("vitrobar: the music panel failed: {e}"),
        }
    }

    /// Asks the media service for the open panel's artwork when its track is new.
    fn music_request_art(&mut self) {
        let Some(p) = self.music_panel.as_mut() else {
            return;
        };
        if p.take_art_wanted() {
            if let Some(m) = &self.music {
                m.request_thumbnail(p.art_px());
            }
        }
    }

    /// True if it was open. The foreground goes back to the window that had it before.
    fn close_music_panel(&mut self) -> bool {
        self.close_music_panel_as(true)
    }

    /// [`Self::close_music_panel`] without the foreground hand-back (Explorer restart, GPU
    /// loss, monitor change).
    fn close_music_panel_now(&mut self) -> bool {
        self.close_music_panel_as(false)
    }

    fn close_music_panel_as(&mut self, hand_back: bool) -> bool {
        let Some(p) = self.music_panel.take() else {
            return false;
        };
        if hand_back {
            let _ = self.hand_back_foreground(p.hwnd(), p.fg_at_open);
        }
        drop(p);
        // A late artwork is not kept.
        if let Some(m) = &self.music {
            let _ = m.take_thumbnail();
        }
        if self.opts.debug_state {
            eprintln!("vitrobar-music-closed");
        }
        true
    }

    /// Runs `f` on the open music panel and acts on what it asks for.
    fn music_event(&mut self, f: impl FnOnce(&mut MusicPanel, &Gfx) -> MusicOutcome) {
        let Some(p) = self.music_panel.as_mut() else {
            return;
        };
        let g = Gfx {
            comp: &self.comp,
            painter: &self.painter,
        };
        match f(p, &g) {
            MusicOutcome::None => {}
            MusicOutcome::Close => {
                self.close_music_panel();
            }
            MusicOutcome::Act(a) => {
                if let Some(m) = &self.music {
                    m.act(a);
                }
            }
        }
    }

    /// Alt+F4 on the music panel; false for anything else.
    fn on_music_msg(&mut self, msg: &MSG) -> bool {
        if !self
            .music_panel
            .as_ref()
            .is_some_and(|p| p.hwnd() == msg.hwnd)
        {
            return false;
        }
        if msg.message == WM_SYSKEYDOWN && msg.wParam.0 == usize::from(VK_F4.0) {
            self.close_music_panel();
            return true;
        }
        false
    }

    /// Like [`Self::quick_foreground_changed`].
    fn music_foreground_changed(&mut self, fg: Option<Hwnd>) {
        let Some(p) = self.music_panel.as_mut() else {
            return;
        };
        if p.activated {
            return;
        }
        let fg = fg.unwrap_or(0) as usize;
        if fg == p.hwnd().0 as usize {
            p.activated = true;
        } else if fg != p.fg_at_open {
            self.close_music_panel();
        }
    }

    /// Moves the open music panel to where its pill is now, or closes it with the pill.
    fn music_reanchor(&mut self) {
        if self.music_panel.is_none() {
            return;
        }
        let anchor = self.tray_monitor().and_then(|mb| {
            mb.layout.widgets[0]?;
            surface_anchor(&mb.layout, AnchorTarget::Widget(0), mb.scale())
        });
        let Some(anchor) = anchor else {
            self.close_music_panel();
            return;
        };
        if let Some(p) = self.music_panel.as_mut() {
            if let Err(e) = p.reanchor(&self.comp, anchor) {
                eprintln!("vitrobar: the music panel could not follow its pill: {e}");
                self.close_music_panel();
            }
        }
    }

    fn music_restyle(&mut self) {
        let (accent, _) = self.start_look();
        let Some(p) = self.music_panel.as_mut() else {
            return;
        };
        let g = Gfx {
            comp: &self.comp,
            painter: &self.painter,
        };
        if let Err(e) = p.restyle(&g, &self.style, accent) {
            eprintln!("vitrobar: the music panel could not be restyled: {e}");
            self.close_music_panel();
        }
    }

    // -----------------------------------------------------------------------------------
    // The Claude limits widget (controller in `widgets.rs`, bridge in `claude_bridge.rs`)
    // -----------------------------------------------------------------------------------

    /// The widget was turned on or off in the settings: wrap Claude Code's status line in the
    /// bridge (storing the user's command) or put it back. A failed wrap leaves the widget
    /// off and says so under the toggle.
    fn claude_toggled(&mut self) {
        if self.settings.widget_claude {
            let wrapped = std::env::current_exe()
                .map_err(|e| e.to_string())
                .and_then(|exe| claude_usage::wrap_statusline(&exe).map_err(|e| e.to_string()));
            match wrapped {
                Ok(Wrap::Wrapped(prev)) => {
                    self.settings.claude_prev_statusline = prev;
                    self.claude_failed = false;
                }
                Ok(Wrap::AlreadyOurs) => self.claude_failed = false,
                Err(e) => {
                    eprintln!("vitrobar: could not set up Claude Code's status line: {e}");
                    self.settings.widget_claude = false;
                    self.claude_failed = true;
                }
            }
        } else {
            self.claude_failed = false;
            match claude_usage::unwrap_statusline(self.settings.claude_prev_statusline.as_deref()) {
                Ok(()) => self.settings.claude_prev_statusline = None,
                // The stored command stays, so turning the widget off again can still restore it.
                Err(e) => eprintln!("vitrobar: could not restore Claude Code's status line: {e}"),
            }
        }
        self.save_settings();
        self.sync_claude();
    }

    /// Opens the folder watch when the widget is on and drops it when it is off (P4).
    fn sync_claude(&mut self) {
        match (self.settings.widget_claude, self.claude.is_some()) {
            (true, false) => match Claude::open(self.notify.hwnd) {
                Ok(mut c) => {
                    c.remeasure(&self.painter, ui_lang(&self.settings));
                    self.claude = Some(c);
                    self.arm_claude_stale();
                }
                Err(e) => eprintln!("vitrobar: the Claude widget could not start: {e}"),
            },
            (false, true) => {
                self.close_claude_panel();
                self.claude = None;
                self.kill_timer(TIMER_CLAUDE_STALE);
            }
            _ => {}
        }
    }

    /// One one-shot timer for the next time the pill changes by itself (a reset, or the data
    /// turning "old").
    fn arm_claude_stale(&mut self) {
        match self.claude.as_ref().and_then(|c| c.ms_until_change()) {
            // SAFETY: timer on our own window; setting the id again replaces it.
            Some(ms) => unsafe {
                SetTimer(Some(self.notify.hwnd), TIMER_CLAUDE_STALE, ms, None);
            },
            None => self.kill_timer(TIMER_CLAUDE_STALE),
        }
    }

    /// [`WM_APP_CLAUDE`](crate::notify::WM_APP_CLAUDE): the bridge saved new data.
    fn on_claude_data(&mut self, dirty: &mut Dirty) {
        let lang = ui_lang(&self.settings);
        let Some(c) = self.claude.as_mut() else {
            return;
        };
        if c.reload(&self.painter, lang) {
            dirty.visuals = true;
        }
        let usage = c.usage;
        self.arm_claude_stale();
        self.claude_event(|p, g| p.refresh(g, usage));
    }

    fn claude_timer(&mut self, id: usize, dirty: &mut Dirty) {
        self.kill_timer(id);
        let lang = ui_lang(&self.settings);
        let Some(c) = self.claude.as_mut() else {
            return;
        };
        if id == TIMER_CLAUDE_STALE {
            if c.remeasure(&self.painter, lang) {
                dirty.visuals = true;
            }
            self.arm_claude_stale();
            return;
        }
        // The minute timer: "resets in" moves on while the panel is open.
        let usage = c.usage;
        if self.claude_panel.is_some() {
            self.claude_event(|p, g| p.refresh(g, usage));
            self.arm_claude_minute();
        }
    }

    fn arm_claude_minute(&mut self) {
        // SAFETY: timer on our own window; setting the id again replaces it.
        unsafe {
            SetTimer(Some(self.notify.hwnd), TIMER_CLAUDE_MINUTE, 60_000, None);
        }
    }

    fn toggle_claude_panel(&mut self, dirty: &mut Dirty) {
        if !self.close_claude_panel() {
            self.open_claude_panel(dirty);
        }
    }

    fn open_claude_panel(&mut self, dirty: &mut Dirty) {
        if let Some(f) = self.keys.focus() {
            dirty.bars.insert(f.monitor);
        }
        self.reset_interaction();
        let Some(usage) = self.claude.as_ref().map(|c| c.usage) else {
            return;
        };
        let Some(mb) = self.tray_monitor() else {
            return;
        };
        if mb.layout.widgets[1].is_none() {
            return;
        }
        let Some(anchor) = surface_anchor(&mb.layout, AnchorTarget::Widget(1), mb.scale()) else {
            return;
        };
        let (id, dpi) = (mb.id(), mb.info.dpi);
        dirty.bars.insert(id);
        let (accent, lang) = self.start_look();
        let fg = foreground().unwrap_or(0) as usize;
        let opened = ClaudePanel::open(
            &Gfx {
                comp: &self.comp,
                painter: &self.painter,
            },
            &self.style,
            dpi,
            anchor,
            self.settings.position,
            accent,
            lang,
            usage,
        );
        match opened {
            Ok(mut p) => {
                p.fg_at_open = fg;
                if !p.activate() {
                    eprintln!("vitrobar: the Claude panel could not take the foreground");
                }
                if self.opts.debug_state {
                    eprintln!(
                        "vitrobar-claude-open rect={:?} anchor={:?} activated={}",
                        p.rect_px(),
                        anchor,
                        p.activated
                    );
                }
                self.claude_panel = Some(p);
                self.arm_claude_minute();
            }
            Err(e) => eprintln!("vitrobar: the Claude panel failed: {e}"),
        }
    }

    fn close_claude_panel(&mut self) -> bool {
        self.close_claude_panel_as(true)
    }

    fn close_claude_panel_now(&mut self) -> bool {
        self.close_claude_panel_as(false)
    }

    fn close_claude_panel_as(&mut self, hand_back: bool) -> bool {
        let Some(p) = self.claude_panel.take() else {
            return false;
        };
        if hand_back {
            let _ = self.hand_back_foreground(p.hwnd(), p.fg_at_open);
        }
        drop(p);
        self.kill_timer(TIMER_CLAUDE_MINUTE);
        if self.opts.debug_state {
            eprintln!("vitrobar-claude-closed");
        }
        true
    }

    fn claude_event(&mut self, f: impl FnOnce(&mut ClaudePanel, &Gfx) -> MusicOutcome) {
        let Some(p) = self.claude_panel.as_mut() else {
            return;
        };
        let g = Gfx {
            comp: &self.comp,
            painter: &self.painter,
        };
        if f(p, &g) == MusicOutcome::Close {
            self.close_claude_panel();
        }
    }

    /// Alt+F4 on the Claude panel; false for anything else.
    fn on_claude_msg(&mut self, msg: &MSG) -> bool {
        if !self
            .claude_panel
            .as_ref()
            .is_some_and(|p| p.hwnd() == msg.hwnd)
        {
            return false;
        }
        if msg.message == WM_SYSKEYDOWN && msg.wParam.0 == usize::from(VK_F4.0) {
            self.close_claude_panel();
            return true;
        }
        false
    }

    /// Like [`Self::quick_foreground_changed`].
    fn claude_foreground_changed(&mut self, fg: Option<Hwnd>) {
        let Some(p) = self.claude_panel.as_mut() else {
            return;
        };
        if p.activated {
            return;
        }
        let fg = fg.unwrap_or(0) as usize;
        if fg == p.hwnd().0 as usize {
            p.activated = true;
        } else if fg != p.fg_at_open {
            self.close_claude_panel();
        }
    }

    fn claude_reanchor(&mut self) {
        if self.claude_panel.is_none() {
            return;
        }
        let anchor = self.tray_monitor().and_then(|mb| {
            mb.layout.widgets[1]?;
            surface_anchor(&mb.layout, AnchorTarget::Widget(1), mb.scale())
        });
        let Some(anchor) = anchor else {
            self.close_claude_panel();
            return;
        };
        if let Some(p) = self.claude_panel.as_mut() {
            if let Err(e) = p.reanchor(&self.comp, anchor) {
                eprintln!("vitrobar: the Claude panel could not follow its pill: {e}");
                self.close_claude_panel();
            }
        }
    }

    fn claude_restyle(&mut self) {
        let (accent, _) = self.start_look();
        let Some(p) = self.claude_panel.as_mut() else {
            return;
        };
        let g = Gfx {
            comp: &self.comp,
            painter: &self.painter,
        };
        if let Err(e) = p.restyle(&g, &self.style, accent) {
            eprintln!("vitrobar: the Claude panel could not be restyled: {e}");
            self.close_claude_panel();
        }
    }

    // -----------------------------------------------------------------------------------
    // The quick panel (controller in `quick.rs`)
    // -----------------------------------------------------------------------------------

    fn toggle_quick(&mut self, dirty: &mut Dirty) {
        if !self.close_quick() {
            self.open_quick(dirty);
        }
    }

    /// Opens the quick panel above the tray bar's quick button (below it with the bar at the
    /// top) and gives it the foreground. If it cannot open, the stock Quick Settings do.
    fn open_quick(&mut self, dirty: &mut Dirty) {
        if let Some(f) = self.keys.focus() {
            dirty.bars.insert(f.monitor);
        }
        // Start, strip, overflow, tooltip and the keyboard focus mode close.
        self.reset_interaction();
        let Some(mb) = self.monitors.iter().find(|m| m.tray) else {
            return;
        };
        dirty.bars.insert(mb.id());
        let anchor = quick_anchor(&mb.layout, mb.scale());
        let (accent, lang) = self.start_look();
        let fg = foreground().unwrap_or(0) as usize;
        let opened = match anchor {
            Some(anchor) => QuickPanel::open(
                &Gfx {
                    comp: &self.comp,
                    painter: &self.painter,
                },
                &self.style,
                mb.info.dpi,
                anchor,
                self.settings.position,
                accent,
                lang,
                self.notify.hwnd,
                self.settings.quick_tiles.clone(),
            ),
            None => Err(Error::new(E_FAIL, "no quick button on the bar")),
        };
        match opened {
            Ok(mut q) => {
                q.fg_at_open = fg;
                if !q.activate() {
                    eprintln!("vitrobar: the quick panel could not take the foreground");
                }
                if self.opts.debug_state {
                    eprintln!(
                        "vitrobar-quick-open rect={:?} anchor={:?} activated={}",
                        q.rect_px(),
                        anchor,
                        q.activated
                    );
                }
                self.quick = Some(q);
            }
            Err(e) => {
                eprintln!("vitrobar: the quick panel failed ({e}); opening the stock one");
                self.launcher.launch(Job::QuickSettings);
            }
        }
    }

    /// True if it was open. Its services and timers go with it. The foreground goes back to
    /// the window that had it before the panel opened (same rules as Start).
    fn close_quick(&mut self) -> bool {
        self.close_quick_as(true)
    }

    /// [`Self::close_quick`] for when the panel cannot be kept (Explorer restart, GPU loss,
    /// monitor change): no foreground hand-back, Windows picks the next window.
    fn close_quick_now(&mut self) -> bool {
        self.close_quick_as(false)
    }

    fn close_quick_as(&mut self, hand_back: bool) -> bool {
        let Some(q) = self.quick.take() else {
            return false;
        };
        if hand_back {
            let _ = self.hand_back_foreground(q.hwnd(), q.fg_at_open);
        }
        drop(q);
        if self.opts.debug_state {
            eprintln!("vitrobar-quick-closed");
        }
        true
    }

    fn quick_outcome(&mut self, o: QuickOutcome) {
        match o {
            QuickOutcome::None => {}
            QuickOutcome::Close => {
                self.close_quick();
            }
            QuickOutcome::Open(uri) => {
                self.close_quick();
                self.launcher.launch(Job::Open(uri));
            }
            QuickOutcome::SaveTiles(tiles) => {
                self.settings.quick_tiles = tiles;
                self.save_settings();
                self.settings_sync();
            }
            QuickOutcome::Lock => {
                self.close_quick();
                if let Err(e) = vitro_win::power::perform(vitro_win::power::PowerAction::Lock) {
                    eprintln!("vitrobar: could not lock: {e}");
                }
            }
        }
    }

    /// Runs `f` on the open quick panel and acts on what it asks for.
    fn quick_event(&mut self, f: impl FnOnce(&mut QuickPanel, &Gfx) -> QuickOutcome) {
        let Some(q) = self.quick.as_mut() else {
            return;
        };
        let g = Gfx {
            comp: &self.comp,
            painter: &self.painter,
        };
        let o = f(q, &g);
        self.quick_outcome(o);
    }

    fn on_quick_mouse(&mut self, msg: &MSG) {
        self.quick_event(|q, g| q.on_mouse(msg, g));
    }

    fn on_quick_key(&mut self, msg: &MSG) {
        let vk = msg.wParam.0 as u16;
        // SAFETY: plain queries of this thread's keyboard state.
        let (ctrl, alt, shift) = unsafe {
            (
                GetKeyState(i32::from(VK_CONTROL.0)) < 0,
                GetKeyState(i32::from(VK_MENU.0)) < 0,
                GetKeyState(i32::from(VK_SHIFT.0)) < 0,
            )
        };
        // AltGr (Ctrl+Alt) types characters: it is not Ctrl.
        let ctrl = ctrl && !alt;
        if ctrl && vk == u16::from(b'V') {
            let text = self
                .quick
                .as_ref()
                .and_then(|q| vitro_win::clipboard::get_text(q.hwnd(), 256));
            if let Some(text) = text {
                self.quick_event(|q, g| q.paste(text, g));
            }
            return;
        }
        self.quick_event(|q, g| q.on_key(vk, ctrl, shift, g));
    }

    /// `WM_CHAR`, `WM_MOUSEWHEEL` and Alt+F4 on the quick panel; false for anything else.
    fn on_quick_msg(&mut self, msg: &MSG) -> bool {
        if !self.quick.as_ref().is_some_and(|q| q.hwnd() == msg.hwnd) {
            return false;
        }
        match msg.message {
            WM_CHAR => {
                // A password is ASCII: surrogate halves are dropped.
                if let Some(c) = char::from_u32(msg.wParam.0 as u32 & 0xFFFF) {
                    self.quick_event(|q, g| q.on_char(c, g));
                }
                true
            }
            WM_MOUSEWHEEL => {
                let delta = ((msg.wParam.0 >> 16) & 0xFFFF) as u16 as i16;
                let pt = mouse_pos(msg.lParam);
                self.quick_event(|q, g| q.on_wheel(delta, pt, g));
                true
            }
            WM_SYSKEYDOWN if msg.wParam.0 == usize::from(VK_F4.0) => {
                self.close_quick();
                true
            }
            _ => false,
        }
    }

    fn quick_timer(&mut self, id: usize) {
        if self.quick.is_none() {
            // Queued before the panel closed.
            self.kill_timer(id);
            return;
        }
        self.quick_event(|q, g| q.on_timer(id, g).unwrap_or(QuickOutcome::None));
    }

    /// Like [`Self::start_foreground_changed`]: a panel that never got the foreground closes
    /// once the foreground moves away from the window that had it at open.
    fn quick_foreground_changed(&mut self, fg: Option<Hwnd>) {
        let Some(q) = self.quick.as_mut() else {
            return;
        };
        if q.activated {
            return;
        }
        let fg = fg.unwrap_or(0) as usize;
        if fg == q.hwnd().0 as usize {
            q.activated = true;
        } else if fg != q.fg_at_open {
            self.close_quick();
        }
    }

    /// Moves the open quick panel to where the tray bar's quick button is now.
    fn quick_reanchor(&mut self) {
        if self.quick.is_none() {
            return;
        }
        let anchor = self
            .tray_monitor()
            .and_then(|mb| quick_anchor(&mb.layout, mb.scale()));
        let Some(anchor) = anchor else {
            // The button went away.
            self.close_quick();
            return;
        };
        if let Some(q) = self.quick.as_mut() {
            if let Err(e) = q.reanchor(&self.comp, anchor) {
                eprintln!("vitrobar: the quick panel could not follow its button: {e}");
                self.close_quick();
            }
        }
    }

    fn quick_restyle(&mut self) {
        let (accent, _) = self.start_look();
        let Some(q) = self.quick.as_mut() else {
            return;
        };
        let g = Gfx {
            comp: &self.comp,
            painter: &self.painter,
        };
        if let Err(e) = q.restyle(&g, &self.style, accent) {
            eprintln!("vitrobar: the quick panel could not be restyled: {e}");
            self.close_quick();
        }
    }

    /// The foreground window changed. When the panel never got the foreground (activation
    /// refused), it cannot hear its deactivation: it closes as soon as the foreground moves
    /// away from the window that had it at open (fallback for carry 12). Getting the
    /// foreground itself (a click on it) makes it a normal activated panel.
    fn start_foreground_changed(&mut self, fg: Option<Hwnd>) {
        let Some(sp) = self.start.as_mut() else {
            return;
        };
        if sp.activated {
            return;
        }
        let fg = fg.unwrap_or(0) as usize;
        if fg == sp.hwnd().0 as usize {
            sp.activated = true;
        } else if fg != sp.fg_at_open {
            self.close_start();
        }
    }

    /// The `"start"` object of the state dump. Never the query text: only its length.
    fn dump_start(&self) -> String {
        let dry = vitro_win::power::last_dry_run()
            .map_or("null".to_string(), |a| json_str(&format!("{a:?}")));
        let Some(sp) = &self.start else {
            return format!(
                "{{\"open\":false,\"catalog\":{},\"power_dry_run\":{}}}",
                self.start_data.apps.len(),
                dry
            );
        };
        let st = &sp.state;
        let (l, t, _, _) = sp.rect_px();
        let screen = |r: PxRect| json_rect((l + r.0, t + r.1, l + r.2, t + r.3));
        let top = match sp.results.first() {
            Some(ResultKind::Math(_)) => "\"math\"",
            Some(ResultKind::App(_)) => "\"app\"",
            Some(ResultKind::Setting(_)) => "\"setting\"",
            Some(ResultKind::File(_)) => "\"file\"",
            Some(ResultKind::Loading) => "\"loading\"",
            None => "null",
        };
        format!(
            "{{\"open\":true,\"grid\":{},\"cols\":{},\"mode\":\"{}\",\"query\":{},\"results\":{},\"top\":{},\"search\":{},\"all_apps\":{},\"list\":{},\"power\":{},\"selected\":{},\"files\":{},\"scroll\":{},\"hwnd\":{},\"rect\":{},\"monitor\":{},\"activated\":{},\"catalog\":{},\"recent\":{},\"recent_enabled\":{},\"power_dry_run\":{}}}",
            st.grid,
            sp.geom.grid.as_ref().map_or(0, |g| g.cols),
            st.mode.name(),
            st.input.text().chars().count(),
            sp.results.len(),
            top,
            screen(sp.geom.search),
            screen(sp.geom.all_apps),
            screen(sp.geom.list),
            screen(sp.geom.power),
            st.selected,
            sp.files.len(),
            st.scroll.round() as i32,
            sp.hwnd().0 as usize,
            json_rect(sp.rect_px()),
            sp.monitor,
            sp.activated,
            self.start_data.apps.len(),
            self.start_data.recent.len(),
            self.start_data.recent_enabled,
            dry
        )
    }

    /// Right-click: the menu of app `app`, or the bar's own menu. Modal.
    fn context_menu(&mut self, app: Option<usize>, dirty: &mut Dirty) {
        self.reset_interaction();
        // The menu runs its own loop: a panel still fading would sit over Start's area
        // (hit-testable) for as long as the menu is up.
        self.drop_start_closing();
        dirty.visuals = true;
        let lang = ui_lang(&self.settings);
        let (owner, pt) = (self.notify.hwnd, self.msg_pt);
        match app {
            Some(i) => {
                let Some(g) = self.cur_groups().get(i).cloned() else {
                    return;
                };
                let ctx = self.menu_ctx(pt);
                let choice = app_menu(
                    &ctx,
                    owner,
                    pt,
                    g.pin.is_some(),
                    !g.windows.is_empty(),
                    lang,
                );
                if self.opts.debug_state {
                    eprintln!("vitrobar-menu app {i} {choice:?}");
                }
                if let Some(c) = choice {
                    self.apply_app_choice(&g, c, dirty);
                }
            }
            None => {
                let choice = bar_menu(&self.menu_ctx(pt), owner, pt, &self.settings, lang);
                if self.opts.debug_state {
                    eprintln!("vitrobar-menu bar {choice:?}");
                }
                if let Some(c) = choice {
                    self.apply_bar_choice(c, dirty);
                }
            }
        }
    }

    fn apply_app_choice(&mut self, g: &AppGroup, choice: MenuChoice, dirty: &mut Dirty) {
        match choice {
            MenuChoice::Pin => {
                if g.pin.is_some() {
                    return;
                }
                let Some(pin) = pin_for_group(g, &self.windows.infos()) else {
                    eprintln!("vitrobar: this app cannot be pinned (no AUMID or executable)");
                    return;
                };
                self.settings.pins.push(pin);
                self.pins_changed(dirty);
            }
            MenuChoice::Unpin => {
                let Some(p) = g.pin.filter(|&p| p < self.settings.pins.len()) else {
                    return;
                };
                self.settings.pins.remove(p);
                self.pins_changed(dirty);
            }
            MenuChoice::NewWindow => {
                let pin = g
                    .pin
                    .and_then(|p| self.settings.pins.get(p).cloned())
                    .or_else(|| pin_for_group(g, &self.windows.infos()));
                if let Some(pin) = pin {
                    self.launcher.launch(Job::Pin(pin));
                }
            }
            MenuChoice::CloseAll => {
                // Only this group's windows, and only those still alive.
                for &h in &g.windows {
                    let hwnd = hwnd_of(h);
                    // SAFETY: plain query.
                    if self.windows.contains(h) && unsafe { IsWindow(Some(hwnd)) }.as_bool() {
                        close_window(hwnd);
                    }
                }
            }
            _ => {}
        }
    }

    fn pins_changed(&mut self, dirty: &mut Dirty) {
        self.save_settings();
        // Group indices (and so hover and press) refer to the old pins; pins are on every bar.
        self.regroup();
        dirty.visuals = true;
    }

    fn apply_bar_choice(&mut self, choice: MenuChoice, dirty: &mut Dirty) {
        match choice {
            MenuChoice::OpenSettings => self.open_settings(),
            MenuChoice::ToggleStartWithWindows => {
                let on = usize::from(!self.settings.start_with_windows);
                self.apply_setting(ControlId::StartWithWindows, on, dirty);
            }
            MenuChoice::Exit => {
                // The normal way out: the loop ends and the stock taskbar comes back.
                // SAFETY: plain call.
                unsafe { PostQuitMessage(0) };
            }
            _ => {}
        }
    }

    // -----------------------------------------------------------------------------------
    // The settings window (`settings_window.rs`)
    // -----------------------------------------------------------------------------------

    /// Opens the settings window centred on the monitor under the cursor; if it is open
    /// already, brings it to the front.
    fn open_settings(&mut self) {
        if let Some(sw) = &self.settings_win {
            if !sw.focus() {
                eprintln!("vitrobar: the settings window could not take the foreground");
            }
            return;
        }
        let mut pt = windows::Win32::Foundation::POINT::default();
        // SAFETY: out-pointer to a local.
        let cursor = if unsafe { GetCursorPos(&mut pt) }.is_ok() {
            (pt.x, pt.y)
        } else {
            self.msg_pt
        };
        let inside = |m: &MonitorBar| {
            let (l, t, r, b) = m.info.rect_px;
            cursor.0 >= l && cursor.0 < r && cursor.1 >= t && cursor.1 < b
        };
        let Some(mb) = self
            .monitors
            .iter()
            .find(|m| inside(m))
            .or_else(|| self.tray_monitor())
            .or(self.monitors.first())
        else {
            return;
        };
        let s = mb.scale();
        let dpi = mb.info.dpi;
        let (l, t, r, b) = work_area_px(mb.info.rect_px);
        let w = ((WINDOW_W_DIP * s).round() as i32).min(r - l);
        let h = ((WINDOW_H_DIP * s).round() as i32).min(b - t);
        let (x, y) = (l + (r - l - w) / 2, t + (b - t - h) / 2);
        let lang = ui_lang(&self.settings);
        match SettingsWindow::open(
            &self.comp,
            (x, y, x + w, y + h),
            dpi,
            &self.style,
            tr(lang, Key::VitrobarSettings),
            &self.settings,
        ) {
            Ok(sw) => {
                self.settings_win = Some(sw);
                self.settings_render();
                if let Some(sw) = &self.settings_win {
                    if !sw.focus() {
                        eprintln!("vitrobar: the settings window could not take the foreground");
                    }
                }
            }
            Err(e) => eprintln!("vitrobar: the settings window could not open: {e}"),
        }
        if self.opts.debug_state {
            self.dump_state();
        }
    }

    /// Closes the settings window: its visuals, surfaces and window go.
    fn close_settings(&mut self) {
        if self.settings_win.take().is_some() && self.opts.debug_state {
            self.dump_state();
        }
    }

    /// The note under the stock-taskbar row when its setting waits for the next start.
    fn settings_hint(&self) -> Option<(usize, &'static str)> {
        let sw = self.settings_win.as_ref()?;
        if self.settings.stock_taskbar == self.stock.mode() {
            return None;
        }
        let i = sw
            .ctl
            .model
            .controls
            .iter()
            .position(|c| c.id == ControlId::StockTaskbar)?;
        Some((i, tr(ui_lang(&self.settings), Key::AppliesNextStart)))
    }

    /// Redraws the open settings window (only what changed is drawn again).
    fn settings_render(&mut self) {
        let hint = self.settings_hint();
        let lang = ui_lang(&self.settings);
        let accent = rgb(self.settings.accent.unwrap_or(self.sys.accent));
        let Some(sw) = self.settings_win.as_mut() else {
            return;
        };
        let look = SettingsLook {
            style: &self.style,
            accent,
            system_accent: rgb(self.sys.accent),
            lang,
            title: tr(lang, Key::VitrobarSettings),
            version: env!("CARGO_PKG_VERSION"),
            hint,
            page: sw.ctl.page,
        };
        if let Err(e) = sw.render(&self.painter, &look) {
            eprintln!("vitrobar: the settings window could not be drawn: {e}");
            self.close_settings();
        }
    }

    /// The settings changed (here or from the bar menu): the open window shows them.
    fn settings_sync(&mut self) {
        let Some(sw) = self.settings_win.as_mut() else {
            return;
        };
        sw.ctl.rebuild(&self.settings);
        set_claude_hint(&mut sw.ctl.model, self.claude_failed);
        self.settings_render();
    }

    /// Redraws the window's glass for the current style (material, theme).
    fn settings_restyle(&mut self) {
        let Some(sw) = self.settings_win.as_mut() else {
            return;
        };
        let (rect, dpi) = (sw.rect_px(), sw.dpi);
        if let Err(e) = sw.reshape(&self.comp, rect, dpi, &self.style, &self.settings) {
            eprintln!("vitrobar: the settings window could not be restyled: {e}");
        }
    }

    /// Sets control `id` to `choice` and carries the change out at once, as the bar menu
    /// did: the bars follow (the live preview), the hook, autostart; then saves.
    fn apply_setting(&mut self, id: ControlId, choice: usize, dirty: &mut Dirty) {
        let Some(effect) = apply_choice(&mut self.settings, id, choice) else {
            self.settings_render();
            return;
        };
        self.save_settings();
        if self.opts.debug_state {
            eprintln!("vitrobar-setting {id:?} {choice} {effect:?}");
        }
        match effect {
            Effect::Relayout => {
                if id == ControlId::WidgetMusic {
                    self.sync_music();
                }
                if id == ControlId::WidgetClaude {
                    self.claude_toggled();
                }
                self.relayout(dirty);
                if matches!(id, ControlId::Material | ControlId::Theme) {
                    self.settings_restyle();
                }
            }
            Effect::WinKey(on) => self.set_win_key(on, dirty),
            Effect::Autostart(on) => apply_autostart(on),
            Effect::Language => {
                // The Claude pill's words change language.
                let lang = ui_lang(&self.settings);
                if let Some(c) = self.claude.as_mut() {
                    if c.remeasure(&self.painter, lang) {
                        dirty.visuals = true;
                    }
                }
                let title = tr(ui_lang(&self.settings), Key::VitrobarSettings);
                if let Some(sw) = &self.settings_win {
                    sw.set_title(title);
                }
            }
            // The stock taskbar's mode is fixed while it is taken over: the row says so.
            Effect::NextStart | Effect::Saved => {}
        }
        self.settings_sync();
        if self.opts.debug_state {
            self.dump_state();
        }
    }

    fn settings_input(&mut self, input: SettingsInput, dirty: &mut Dirty) {
        let Some(sw) = self.settings_win.as_mut() else {
            return;
        };
        match sw.ctl.handle(input) {
            SettingsAction::None => {}
            SettingsAction::Redraw => {
                self.settings_render();
                if self.opts.debug_state {
                    self.dump_state();
                }
            }
            SettingsAction::Close => self.close_settings(),
            SettingsAction::Rebuild => self.settings_sync(),
            SettingsAction::Apply(id, choice) => self.apply_setting(id, choice, dirty),
            SettingsAction::OpenLink(url) => {
                if self.opts.debug_state {
                    eprintln!("vitrobar-settings-link {}", json_str(url));
                }
                self.launcher.launch(Job::Open(url.to_string()));
                self.settings_render();
            }
        }
    }

    fn is_settings(&self, hwnd: HWND) -> bool {
        self.settings_win.as_ref().is_some_and(|s| s.hwnd() == hwnd)
    }

    /// Mouse input on the settings window (client px).
    fn on_settings_mouse(&mut self, msg: &MSG, dirty: &mut Dirty) -> bool {
        let pt = mouse_pos(msg.lParam);
        let Some(sw) = self.settings_win.as_mut() else {
            return false;
        };
        let input = match msg.message {
            WM_MOUSEMOVE => {
                if !sw.tracking {
                    track_leave(msg.hwnd);
                    sw.tracking = true;
                }
                SettingsInput::Move(pt)
            }
            WM_MOUSELEAVE => {
                sw.tracking = false;
                SettingsInput::Leave
            }
            WM_LBUTTONDOWN => SettingsInput::Down(pt),
            WM_LBUTTONUP => SettingsInput::Up(pt),
            WM_RBUTTONDOWN | WM_RBUTTONUP => return true,
            _ => return false,
        };
        self.settings_input(input, dirty);
        true
    }

    /// A key on the settings window.
    fn on_settings_key(&mut self, msg: &MSG, dirty: &mut Dirty) {
        // SAFETY: plain query of this thread's keyboard state.
        let shift = unsafe { GetKeyState(i32::from(VK_SHIFT.0)) } < 0;
        // SAFETY: as above.
        let ctrl = unsafe { GetKeyState(i32::from(VK_CONTROL.0)) } < 0;
        let vk = msg.wParam.0 as u32;
        self.settings_input(SettingsInput::Key { vk, shift, ctrl }, dirty);
    }

    /// `WM_MOUSEWHEEL` and Alt+F4 on the settings window; false for anything else.
    fn on_settings_msg(&mut self, msg: &MSG, dirty: &mut Dirty) -> bool {
        if !self.is_settings(msg.hwnd) {
            return false;
        }
        match msg.message {
            WM_MOUSEWHEEL => {
                let delta = ((msg.wParam.0 >> 16) & 0xFFFF) as u16 as i16;
                self.settings_input(SettingsInput::Wheel(i32::from(delta)), dirty);
                true
            }
            WM_SYSKEYDOWN if msg.wParam.0 == usize::from(VK_F4.0) => {
                self.close_settings();
                true
            }
            _ => false,
        }
    }

    /// The window moved to a monitor of another DPI: resize it as Windows suggests.
    fn settings_dpi_changed(&mut self, dpi: u32, rect: PxRect) {
        let Some(sw) = self.settings_win.as_mut() else {
            return;
        };
        if let Err(e) = sw.reshape(&self.comp, rect, dpi, &self.style, &self.settings) {
            eprintln!("vitrobar: the settings window could not follow the DPI: {e}");
        }
        self.settings_render();
    }

    /// The `"settings_window"` object of the state dump.
    fn dump_settings_window(&self) -> String {
        let Some(sw) = &self.settings_win else {
            return "null".to_string();
        };
        let c = &sw.ctl;
        format!(
            "{{\"hwnd\":{},\"rect\":{},\"dpi\":{},\"focus\":\"{:?}\",\"ring\":{},\"scroll\":{},\"hover\":{}}}",
            sw.hwnd().0 as usize,
            json_rect(sw.rect_px()),
            sw.dpi,
            c.model.controls.get(c.model.focus).map(|x| x.id).unwrap_or(ControlId::Shape),
            c.keyboard,
            c.scroll,
            json_str(&format!("{:?}", c.hover)),
        )
    }

    /// Settings that change how the bar looks took effect: new styles, then the next flush
    /// lays out, resizes / creates / destroys the bar windows and moves the appbar.
    fn relayout(&mut self, dirty: &mut Dirty) {
        self.reset_interaction();
        self.style = style_for(&self.settings, &self.sys);
        self.hover_style = hover_style(self.settings.hover);
        dirty.visuals = true;
    }

    fn save_settings(&self) {
        let Some(p) = &self.settings_path else { return };
        if let Err(e) = settings::save(p, &self.settings) {
            eprintln!("vitrobar: could not save the settings: {e}");
        }
    }

    fn on_device_removed(&mut self, dirty: &mut Dirty) {
        // Its surfaces belonged to the lost device.
        self.close_start_now();
        self.close_quick_now();
        self.close_calendar_now();
        self.close_music_panel_now();
        self.close_claude_panel_now();
        self.close_settings();
        // GPU device removed (TDR, driver update): recreate, then redraw everything.
        self.gpu_lost = true;
        if self.recover_gpu() {
            dirty.visuals = true;
        }
    }

    /// Folds what the notify window recorded into `dirty`.
    fn absorb(&mut self, inbox: Inbox, dirty: &mut Dirty) {
        for ev in inbox.events {
            self.on_window_event(ev, dirty);
        }
        for id in inbox.timers {
            self.on_timer(id, dirty);
        }
        if inbox.keys {
            // Always drained: a pending wake-up blocks the next one.
            for out in self.keys.drain() {
                self.on_hook_output(out, dirty);
            }
        }
        if inbox.debug_open_start && self.opts.debug_state {
            // Debug hook (perf check): toggles Start on the primary monitor's bar.
            self.toggle_start(0, dirty);
        }
        if inbox.debug_open_quick && self.opts.debug_state {
            // Debug hook (live checks): toggles the quick panel.
            self.toggle_quick(dirty);
        }
        if inbox.debug_open_calendar && self.opts.debug_state {
            // Debug hook (live checks): toggles the calendar on the current bar's clock.
            self.toggle_calendar(dirty);
        }
        if inbox.media {
            self.on_media(dirty);
        }
        if inbox.claude {
            self.on_claude_data(dirty);
        }
        if inbox.clock && self.read_ime() {
            // Regional settings changed: keyboard layouts may have been added or removed.
            dirty.visuals = true;
        }
        if inbox.clock {
            // The system clock or time zone changed: today may have moved.
            self.calendar_event(|c, g| c.refresh_today(g));
        }
        if inbox.quick {
            self.quick_event(|q, g| q.on_wake(g));
        }
        if inbox.power {
            self.quick_event(|q, g| q.on_power(g));
        }
        dirty.theme |= inbox.theme;
        dirty.clock |= inbox.clock;
        dirty.icons |= inbox.icons;
        dirty.fullscreen |= inbox.fullscreen;
        // The model is shared with the host; the messages only say it changed. Only the
        // tray bar shows it.
        if !inbox.tray.is_empty() {
            if let Some(m) = self.tray_monitor() {
                dirty.bars.insert(m.id());
            }
        }
        // Monitors may have come, gone, moved or changed DPI. Our own appbars cause the
        // work-area broadcasts too: re-reading the monitors is all those do unless something
        // really changed.
        dirty.monitors |= inbox.display || inbox.dpi || inbox.workarea || inbox.session;
        dirty.display |= inbox.display || inbox.session;
        if inbox.session {
            // Back from the lock screen or another session: key-ups may have been lost.
            self.keys.forget_swallowed();
        }
        if inbox.start_data {
            // Always drained (a pending wake-up blocks the next one), shown only while open.
            let was_loading = self.start_data.apps_loading();
            // Before the drain: a new catalog renumbers the apps the selection points at.
            let keep = self
                .start
                .as_ref()
                .and_then(|sp| sp.selection_key(&self.start_data));
            let (apps_changed, any) = self.start_data.drain();
            if any {
                // The end of the catalog read also takes the "Loading apps…" row away.
                let changed = apps_changed || was_loading != self.start_data.apps_loading();
                self.start_refresh(changed, keep);
            }
        }
        if inbox.files {
            self.on_file_results();
        }
        if take_panel_deactivated()
            .iter()
            .any(|h| self.start.as_ref().is_some_and(|s| s.hwnd() == *h))
        {
            // A click elsewhere or a switch to another app.
            self.close_start();
        }
        // A click on the fading panel: it goes now (that click is lost; accepted).
        let clicked = take_panel_clicks();
        if self
            .start_closing
            .as_ref()
            .is_some_and(|s| clicked.contains(&s.hwnd()))
        {
            self.drop_start_closing();
        }
        for h in take_panel_close_requests() {
            // Alt+F4 through the system menu, or "Close window" on a bar's app menu.
            if self.is_settings(h) {
                self.close_settings();
            } else if self.start.as_ref().is_some_and(|s| s.hwnd() == h) {
                self.close_start();
            }
        }
        for (h, dpi, rect) in take_panel_dpi_changes() {
            if self.is_settings(h) {
                self.settings_dpi_changed(dpi, rect);
            }
        }
        let deactivated = take_deactivated();
        if self
            .overflow
            .as_ref()
            .is_some_and(|o| deactivated.contains(&o.flyout.hwnd()))
        {
            self.close_overflow();
            dirty.visuals = true;
        }
        if self
            .quick
            .as_ref()
            .is_some_and(|q| deactivated.contains(&q.hwnd()))
        {
            // A click elsewhere or a switch to another app.
            self.close_quick();
        }
        if self
            .calendar
            .as_ref()
            .is_some_and(|c| deactivated.contains(&c.hwnd()))
        {
            self.close_calendar();
        }
        if self
            .music_panel
            .as_ref()
            .is_some_and(|p| deactivated.contains(&p.hwnd()))
        {
            self.close_music_panel();
        }
        if self
            .claude_panel
            .as_ref()
            .is_some_and(|p| deactivated.contains(&p.hwnd()))
        {
            self.close_claude_panel();
        }
        if inbox.fullscreen {
            // Explorer may have raised its own Shell_TrayWnd for the full-screen change: go
            // back above it. (Restarts, settings and display changes reach the tray host's
            // own window, which handles them.)
            if let Some(t) = &self.tray {
                t.reassert_topmost();
            }
        }
        if inbox.take_over {
            self.finish_start(dirty);
        }
        if inbox.taskbar_created {
            self.on_taskbar_created(dirty);
        } else if inbox.shell || inbox.display {
            self.stock.reassert();
        }
        if inbox.display {
            // Carry 6: a new monitor's Shell_SecondaryTrayWnd may only appear after this.
            self.arm_reassert();
        }
        if inbox.appbar_pos {
            // Display changes claim the strips in `flush`, after the monitors were re-read.
            let mut moved = 0;
            for mb in &self.monitors {
                if let Some(ab) = &mb.appbar {
                    moved += usize::from(ab.on_pos_changed());
                }
            }
            if self.opts.debug_state {
                eprintln!("vitrobar-appbar-pos reclaimed={moved}");
            }
        }
    }

    /// Applies everything collected from one drain of the queue.
    fn flush(&mut self, mut d: Dirty) -> Result<()> {
        if d.monitors && self.reconcile_monitors(current_monitors(self.opts.debug_state)) {
            // Its anchor moved or went away.
            self.close_quick_now();
            self.close_calendar_now();
            self.close_music_panel_now();
            self.close_claude_panel_now();
            // New bars, new geometry: everything is laid out and drawn again, and every
            // window's monitor is read again (windows on a removed monitor were moved).
            d.visuals = true;
            d.fullscreen = true;
            d.windows.extend(self.windows.hwnds());
        }
        if d.display {
            // Carry 5: the system may have moved our windows while monitors changed.
            for b in self.all_bars_mut() {
                b.window.resync();
            }
        }
        if !d.windows.is_empty() {
            for h in std::mem::take(&mut d.windows) {
                d.regroup |= self.windows.refresh(h, &self.desktops);
            }
        }
        let mut redraw = d.visuals;
        let mut some = std::mem::take(&mut d.bars);
        if d.fullscreen {
            for k in 0..self.monitors.len() {
                let on = is_fullscreen_on_rect(self.monitors[k].info.rect_px);
                if self.set_fullscreen(k, on) {
                    some.insert(self.monitors[k].id());
                }
            }
        }
        if d.regroup {
            some.extend(self.regroup());
        }
        if d.theme {
            let sys = read_system_theme();
            let style = style_for(&self.settings, &sys);
            if sys != self.sys || style != self.style {
                self.sys = sys;
                self.style = style;
                redraw = true;
                // Its glass was made for the old style.
                self.close_start_now();
                self.quick_restyle();
                self.calendar_restyle();
                self.music_restyle();
                self.claude_restyle();
                self.settings_restyle();
                self.settings_render();
            }
        }
        if d.clock {
            redraw |= self.tick_clock();
        }
        if d.icons {
            let arrived = self.icons.drain();
            if !arrived.is_empty() {
                let for_panel = |src: &IconSrc| {
                    self.start
                        .as_ref()
                        .is_some_and(|s| s.icon_srcs.contains(src))
                };
                let panel = arrived.iter().any(for_panel);
                // Icons only the Start panel shows redraw the panel, not every bar.
                redraw |= arrived
                    .iter()
                    .any(|src| !for_panel(src) || self.bar_shows(src));
                if panel {
                    self.start_render();
                }
            }
        }
        if redraw {
            self.refresh()?;
        } else if !some.is_empty() {
            // Carry 3: only the bars whose content changed.
            self.refresh_once(Some(&some), true)?;
        }
        if redraw || !some.is_empty() {
            self.sync_strip();
        }
        if d.display {
            // Carry 5: every appbar claims its strip of its monitor again, with the monitor
            // rectangle just read (changed rectangles were already set by the layout).
            for mb in &self.monitors {
                if let Some(ab) = &mb.appbar {
                    ab.set_pos();
                }
            }
        }
        if d.stock {
            self.stock.reassert();
        }
        if self.clock_unarmed {
            self.tick_clock();
        }
        Ok(())
    }
}

struct ComGuard;
impl Drop for ComGuard {
    fn drop(&mut self) {
        // SAFETY: balances the CoInitializeEx in `run`.
        unsafe { CoUninitialize() };
    }
}

/// Writes (or removes) the `Run\Vitrobar` value to match the setting.
/// Used by the "Start with Windows" switch (window and menu) and a first-run yes.
fn apply_autostart(enabled: bool) {
    if let Err(e) = vitro_win::autostart::set_autostart(enabled) {
        eprintln!("vitrobar: could not update \"Start with Windows\": {e}");
    }
}

/// Immediate passes allowed in a row before the loop waits again.
const MAX_IMMEDIATE_PASSES: u32 = 8;

/// Consecutive [`Next::WakeLater`] rounds (each with the inbox still non-empty) after which
/// the loop falls back to an ordinary blocking wait.
const MAX_WAKE_ROUNDS: u32 = 4;

/// What the event loop does after a flush.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Next {
    /// The inbox is empty: block until the next message or handle.
    Wait,
    /// The inbox is not empty: run another pass without blocking.
    Again,
    /// Still not empty after [`MAX_IMMEDIATE_PASSES`]: post `WM_NULL` to ourselves and wait
    /// (the wait ends at once), so a pathological loop cannot spin without yielding.
    WakeLater,
}

/// `wakes` counts the wake-later rounds in a row; a persistent refill (a producer that keeps
/// the inbox non-empty) ends in a blocking [`Next::Wait`] after [`MAX_WAKE_ROUNDS`] of them,
/// which resets the counter. Only a `Wait` resets it: `run` zeroes `spins` after each
/// `WakeLater`, so `Again` rounds sit between the wake-later rounds of a persistent refill.
fn after_flush(pending: bool, spins: u32, wakes: &mut u32) -> Next {
    if !pending {
        *wakes = 0;
        Next::Wait
    } else if spins < MAX_IMMEDIATE_PASSES {
        Next::Again
    } else if *wakes >= MAX_WAKE_ROUNDS {
        *wakes = 0;
        Next::Wait
    } else {
        *wakes += 1;
        Next::WakeLater
    }
}

pub fn run(opts: Options) -> Result<()> {
    // SAFETY: plain apartment initialisation of the UI thread; balanced by ComGuard.
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()? };
    let _com = ComGuard;
    // SAFETY: plain calls; the handler only posts a message.
    unsafe {
        UI_THREAD.store(GetCurrentThreadId(), Ordering::SeqCst);
        // Fails harmlessly in a build without a console.
        let _ = SetConsoleCtrlHandler(Some(on_console_ctrl), true);
    }
    let mut parent = crate::watchdog::ParentWatch::new(opts.parent, opts.pass_on.clone());
    if opts.debug_state && std::env::var(POWER_DRY_RUN_ENV).is_ok_and(|v| v == "1") {
        // Carry 8: live tests of the power menu record the action instead of doing it.
        vitro_win::power::set_dry_run(true);
        eprintln!("vitrobar: power actions are a dry run");
    }

    let path = settings_path();
    let (mut settings, status) = path
        .as_deref()
        .map(settings::load_with_status)
        .unwrap_or((Settings::default(), settings::LoadStatus::Missing));
    let corrupt = status == settings::LoadStatus::Corrupt;
    if corrupt {
        // Not a first run (`first_run_done` is set): no import, and the `Run` value stays
        // exactly as it is. The menu shows what it actually is.
        settings.start_with_windows = vitro_win::autostart::autostart_value().is_some();
        eprintln!("vitrobar: the settings file could not be read; using defaults (copy kept)");
    }
    let first_run = !settings.first_run_done;
    if first_run {
        // Before the stock taskbar is taken over: the import only reads.
        first_run_import(&mut settings);
        // "Start with Windows" is opt-in: nothing goes into HKCU\...\Run without a yes.
        // Not asked in debug-state runs (scripts and live tests drive those).
        settings.start_with_windows = !opts.debug_state
            && vitro_win::autostart::ask_start_with_windows(tr(
                ui_lang(&settings),
                Key::AutostartQuestion,
            ));
        if opts.debug_state {
            eprintln!(
                "vitrobar-import {} pins, {} promoted tray icons",
                settings.pins.len(),
                settings.tray_promoted.len()
            );
        }
        if let Some(p) = &path {
            if let Err(e) = settings::save(p, &settings) {
                eprintln!("vitrobar: could not save the settings: {e}");
            }
        }
    }
    if first_run {
        if settings.start_with_windows {
            apply_autostart(true);
        }
    } else if settings.start_with_windows && !corrupt {
        // Only follows a Vitrobar that moved: never registers the exe started now by itself.
        if let Err(e) = vitro_win::autostart::repair_autostart() {
            eprintln!("vitrobar: could not check \"Start with Windows\": {e}");
        }
    }
    // Takes nothing over yet (that follows the first frame), but from here on the shell is
    // restored when `app` (or a failing `App::new`) drops it.
    let stock = StockGuard::new(settings.stock_taskbar);
    #[cfg(debug_assertions)]
    if std::env::var_os("VITROBAR_TEST_PANIC").is_some() {
        // Test hook (debug builds only): proves the panic hook restores the shell.
        panic!("VITROBAR_TEST_PANIC");
    }
    let mut app = App::new(opts, settings, path, stock)?;
    let mut msg = MSG::default();
    // Immediate passes in a row because the inbox still held something after a flush.
    let mut spins = 0u32;
    // Wake-later rounds in a row (see `after_flush`).
    let mut wakes = 0u32;
    'event: loop {
        let removed = app.gpu.removed_event();
        let mut handles = [removed, HANDLE::default()];
        let mut count = 1;
        if let Some(h) = parent.handle() {
            handles[1] = h;
            count = 2;
        }
        // An inbox left non-empty by the last pass must not wait for the next message: poll
        // the handles without blocking instead.
        let timeout = if spins > 0 { 0 } else { parent.timeout_ms() };
        // SAFETY: the handles stay valid while `app.gpu` and `parent` live.
        let wait = unsafe {
            MsgWaitForMultipleObjectsEx(
                Some(&handles[..count]),
                timeout,
                QS_ALLINPUT,
                MWMO_INPUTAVAILABLE,
            )
        };
        if wait == WAIT_FAILED {
            // Carry 1: never spin on a failing wait.
            return Err(Error::from_thread());
        }
        let mut dirty = Dirty::default();
        if wait == WAIT_OBJECT_0 {
            app.on_device_removed(&mut dirty);
        } else if count == 2 && wait.0 == WAIT_OBJECT_0.0 + 1 {
            parent.on_parent_exit();
        }
        // Cheap: starts an adopting watchdog only once one is due.
        parent.tick();
        // SAFETY: standard message pump of this thread.
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            // Everything for the notify window goes through its window procedure, which records
            // it in the inbox (the same path a nested modal loop would take).
            match msg.message {
                WM_QUIT => break 'event,
                WM_MOUSEMOVE | WM_MOUSELEAVE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN
                | WM_RBUTTONUP
                    if app.on_mouse(&msg, &mut dirty) => {}
                WM_MBUTTONDOWN | WM_MBUTTONUP if app.on_strip_middle(&msg) => {}
                // Then translated, so the Start panel's characters follow as WM_CHAR.
                WM_KEYDOWN if app.on_key(&msg, &mut dirty) => unsafe {
                    let _ = TranslateMessage(&msg);
                },
                WM_CHAR | WM_SYSKEYDOWN | WM_MOUSEWHEEL if app.on_quick_msg(&msg) => {}
                WM_SYSKEYDOWN | WM_MOUSEWHEEL if app.on_calendar_msg(&msg) => {}
                WM_SYSKEYDOWN if app.on_music_msg(&msg) => {}
                WM_SYSKEYDOWN if app.on_claude_msg(&msg) => {}
                WM_CHAR | WM_SYSKEYDOWN | WM_MOUSEWHEEL if app.on_start_msg(&msg, &mut dirty) => {}
                WM_SYSKEYDOWN | WM_MOUSEWHEEL if app.on_settings_msg(&msg, &mut dirty) => {}
                _ => unsafe {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                },
            }
        }
        app.absorb(take_inbox(), &mut dirty);
        app.flush(dirty)?;
        // Nested message loops inside absorb / flush (the COM modal loop of the virtual
        // desktop calls, sent SHAppBarMessage calls) dispatch to the notify window, so the
        // inbox may hold events again that no queued message will announce.
        match after_flush(inbox_pending(), spins, &mut wakes) {
            Next::Wait => spins = 0,
            Next::Again => spins += 1,
            Next::WakeLater => {
                // Bounded: give the rest of the system a turn, then come back through the
                // ordinary wait, which this message ends at once.
                spins = 0;
                // SAFETY: a payload-free post to our own window.
                unsafe {
                    let _ = PostMessageW(Some(app.notify.hwnd), WM_NULL, WPARAM(0), LPARAM(0));
                }
            }
        }
    }
    drop(app);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mon(id: u64, x: i32, dpi: u32, primary: bool) -> MonitorInfo {
        MonitorInfo {
            id,
            rect_px: (x, 0, x + 1920, 1080),
            dpi,
            primary,
        }
    }

    #[test]
    fn a_non_empty_inbox_runs_again_then_yields() {
        let mut wakes = 0;
        assert_eq!(after_flush(false, 0, &mut wakes), Next::Wait);
        assert_eq!(
            after_flush(false, MAX_IMMEDIATE_PASSES, &mut wakes),
            Next::Wait
        );
        let mut spins = 0;
        let mut passes = 0;
        // An inbox that never empties: bounded immediate passes, then a posted wake-up.
        loop {
            match after_flush(true, spins, &mut wakes) {
                Next::Again => {
                    spins += 1;
                    passes += 1;
                }
                Next::WakeLater => break,
                Next::Wait => unreachable!(),
            }
        }
        assert_eq!(passes, MAX_IMMEDIATE_PASSES);
    }

    /// One loop turn exactly as `run` applies it to `after_flush`'s answer.
    fn turn(pending: bool, spins: &mut u32, wakes: &mut u32) -> Next {
        let n = after_flush(pending, *spins, wakes);
        match n {
            Next::Wait | Next::WakeLater => *spins = 0,
            Next::Again => *spins += 1,
        }
        n
    }

    #[test]
    fn persistent_refill_falls_back_to_blocking_wait() {
        let (mut spins, mut wakes) = (0, 0);
        let mut wake_later = 0;
        // A producer that keeps the inbox non-empty: eventually a blocking wait.
        let mut waited = false;
        for _ in 0..1000 {
            match turn(true, &mut spins, &mut wakes) {
                Next::WakeLater => wake_later += 1,
                Next::Wait => {
                    waited = true;
                    break;
                }
                Next::Again => {}
            }
        }
        assert!(waited, "never fell back to a blocking wait");
        assert_eq!(wake_later, MAX_WAKE_ROUNDS);
        assert_eq!(wakes, 0);
        // And the cycle starts over.
        let mut again = 0;
        while turn(true, &mut spins, &mut wakes) == Next::Again {
            again += 1;
        }
        assert_eq!(again, MAX_IMMEDIATE_PASSES);
    }

    #[test]
    fn an_inbox_that_empties_resets_to_normal_behaviour() {
        let (mut spins, mut wakes) = (0, 0);
        loop {
            if turn(true, &mut spins, &mut wakes) == Next::WakeLater {
                break;
            }
        }
        assert_eq!(wakes, 1);
        assert_eq!(turn(false, &mut spins, &mut wakes), Next::Wait);
        assert_eq!((spins, wakes), (0, 0));
        assert_eq!(turn(true, &mut spins, &mut wakes), Next::Again);
    }

    #[test]
    fn same_monitors_need_nothing() {
        let a = [mon(1, 0, 120, true), mon(2, 1920, 96, false)];
        assert!(plan_reconcile(&a, &a).is_empty());
    }

    #[test]
    fn plug_and_unplug() {
        let one = [mon(1, 0, 120, true)];
        let two = [mon(1, 0, 120, true), mon(2, 1920, 96, false)];
        let plug = plan_reconcile(&one, &two);
        assert_eq!(plug.added, vec![2]);
        assert!(plug.removed.is_empty() && plug.changed.is_empty());
        let unplug = plan_reconcile(&two, &one);
        assert_eq!(unplug.removed, vec![2]);
        assert!(unplug.added.is_empty() && unplug.changed.is_empty());
    }

    #[test]
    fn dpi_or_rect_change_touches_only_that_monitor() {
        let old = [mon(1, 0, 120, true), mon(2, 1920, 96, false)];
        let dpi = [mon(1, 0, 120, true), mon(2, 1920, 144, false)];
        assert_eq!(
            plan_reconcile(&old, &dpi),
            ReconcilePlan {
                changed: vec![2],
                ..Default::default()
            }
        );
        let moved = [mon(1, 0, 120, true), mon(2, -1920, 96, false)];
        assert_eq!(plan_reconcile(&old, &moved).changed, vec![2]);
    }

    #[test]
    fn a_new_primary_moves_the_tray_area() {
        let old = [mon(1, 0, 120, true), mon(2, 1920, 96, false)];
        let new = [mon(2, 1920, 96, true), mon(1, 0, 120, false)];
        let p = plan_reconcile(&old, &new);
        assert!(p.added.is_empty() && p.removed.is_empty());
        assert_eq!(p.changed, vec![2, 1]);
    }

    #[test]
    fn primary_unplugged_promotes_the_other() {
        let old = [mon(1, 0, 120, true), mon(2, 1920, 96, false)];
        let new = [mon(2, 1920, 96, true)];
        let p = plan_reconcile(&old, &new);
        assert_eq!(p.removed, vec![1]);
        assert_eq!(p.changed, vec![2]);
    }

    #[test]
    fn fake_monitor_comes_from_the_hook_file() {
        let dir = std::env::temp_dir().join(format!("vitrobar-fake-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("mon.txt");
        std::fs::write(&f, "1920,0,3840,1080,144").unwrap();
        // No other test reads this variable.
        std::env::set_var(FAKE_MONITOR_ENV, &f);
        let with = current_monitors(true);
        // R13: without --debug-state the variable is ignored.
        let not_debug = current_monitors(false);
        std::fs::remove_file(&f).unwrap();
        let without = current_monitors(true);
        std::env::remove_var(FAKE_MONITOR_ENV);
        let _ = std::fs::remove_dir(&dir);
        assert_eq!(with.len(), without.len() + 1);
        assert_eq!(not_debug.len(), without.len());
        let fake = with.last().unwrap();
        assert_eq!((fake.dpi, fake.primary), (144, false));
        assert!(with[0].primary, "a real primary stays first: the tray bar");
    }
}
