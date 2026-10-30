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
//! The stock taskbar is taken over before the bar appears and given back on every way out
//! ([`StockGuard`]); the main bar window is an appbar that reserves the bar's strip; the bar
//! hides while a full-screen app covers its monitor.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use vitro_core::clock::ms_until_next_minute;
use vitro_core::i18n::{lang_from_tag, Lang};
use vitro_core::image::BgraImage;
use vitro_core::layout::{compute_layout, metrics, BarLayout, LayoutInput, Rect};
use vitro_core::settings::{self, Pin, Position, Settings, ThemeChoice};
use vitro_core::style::{hover_style, material_style, HoverStyle, MaterialStyle, Rgba, Theme};
use vitro_core::tray_model::{split_promoted, TrayIcon};
use vitro_core::windows_model::{
    build_groups, click_action, AppGroup, ClickAction, GroupKey, Hwnd,
};
use vitro_render::bar_visual::{
    hover_timing, BarVisual, ChevronState, ItemState, TrayItemState, VisualEnv, TRAY_ICON_DIP,
};
use vitro_render::compositor::{init_compositor, Comp};
use vitro_render::device::{create_gpu_device, GpuDevice};
use vitro_render::flyout::{live_thumbnails, take_deactivated, Flyout, ThumbnailStrip};
use vitro_render::surfaces::{FontKind, Painter};
use vitro_render::tray_flyout::{grid_layout, Tooltip, TrayGrid};
use vitro_win::appbar::{is_fullscreen_on, reserved_rect, AppBar};
use vitro_win::desktops::Desktops;
use vitro_win::launch::{activate_window, close_window, minimize_window, open_stock_start};
use vitro_win::monitors::{list_monitors, MonitorInfo};
use vitro_win::pins_import::{import_promoted_tray, import_windows_pins};
use vitro_win::system_theme::{read_system_theme, user_ui_language_tag, SystemTheme};
use vitro_win::time_format::clock_now;
use vitro_win::tray_host::{tray_identity_key, TrayClick, TrayEntry, TrayHost};
use vitro_win::window_tracker::{WindowEvent, WindowTracker};
use windows::core::{Error, Result, BOOL};
use windows::Win32::Foundation::{E_FAIL, HWND, LPARAM, WAIT_FAILED, WAIT_OBJECT_0, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Console::{
    SetConsoleCtrlHandler, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
};
use windows::Win32::System::Threading::{GetCurrentThreadId, INFINITE};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetDoubleClickTime, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetForegroundWindow, GetMessagePos, IsWindow, KillTimer,
    MsgWaitForMultipleObjectsEx, PeekMessageW, PostQuitMessage, PostThreadMessageW, SetTimer,
    TranslateMessage, MSG, MWMO_INPUTAVAILABLE, PM_REMOVE, QS_ALLINPUT, WM_KEYDOWN, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEMOVE, WM_QUIT, WM_RBUTTONDOWN, WM_RBUTTONUP,
};

use crate::bar_window::{
    create_bar_window, hit_test, mouse_pos, track_leave, BarWindow, Hit, MarginMode, PxRect,
};
use crate::icon_loader::{IconLoader, IconSrc, Lookup, TrayImage, MAX_ICON_PX};
use crate::launcher::{Job, Launcher};
use crate::menus::{app_menu, bar_menu, MenuChoice};
use crate::notify::{
    take_inbox, Inbox, NotifyWindow, TIMER_CLOCK, TIMER_GPU_RETRY, TIMER_HOVER_HIDE,
    TIMER_HOVER_SHOW, TIMER_MARGIN_COLLAPSE, TIMER_REASSERT, TIMER_TRAY_TIP,
};
use crate::shell::{self, StockGuard};
use crate::window_set::WindowSet;

/// Command-line options for the bar process.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// Print one `vitrobar-state {json}` line on stderr whenever the bar's content changes.
    pub debug_state: bool,
}

/// `WM_MOUSELEAVE` (CommCtrl.h); defined here to avoid pulling in the Controls feature.
const WM_MOUSELEAVE: u32 = 0x02A3;

const CLOCK_FONT_DIP: f32 = 12.0;
const CLOCK_PAD_DIP: f32 = 4.0;
const CLOCK_MIN_DIP: f32 = 64.0;
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
/// Resting on a tray icon shows its tooltip after this long.
const TRAY_TIP_MS: u32 = 500;

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
fn settings_path() -> Option<PathBuf> {
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

fn hwnd_of(h: Hwnd) -> HWND {
    HWND(h as usize as *mut _)
}

fn foreground() -> Option<Hwnd> {
    // SAFETY: plain query.
    let h = unsafe { GetForegroundWindow() };
    (!h.0.is_null()).then_some(h.0 as usize as Hwnd)
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

fn json_rect(r: PxRect) -> String {
    format!("[{},{},{},{}]", r.0, r.1, r.2, r.3)
}

/// The open thumbnail strip.
struct Strip {
    // Field order = drop order: the thumbnails are unregistered before their window goes.
    thumbs: ThumbnailStrip,
    flyout: Flyout,
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

/// Changes collected while draining the message queue, applied once afterwards.
#[derive(Default)]
struct Dirty {
    windows: BTreeSet<Hwnd>,
    regroup: bool,
    theme: bool,
    clock: bool,
    icons: bool,
    visuals: bool,
    /// Check whether a full-screen app covers the bar's monitor.
    fullscreen: bool,
}

struct App {
    // Field order = drop order: the tray host first (its Shell_TrayWnd goes and every app is
    // asked to register its icons with Explorer again; later shell calls go straight to
    // Explorer), the appbar (unregistered before its window goes), the flyouts, the bars,
    // then the tracker (before its notify window), then the notify window (drains queued
    // events), then the GPU objects, the compositor, and last the stock taskbar guard (the
    // shell is restored after the appbar is gone).
    tray: Option<TrayHost>,
    appbar: Option<AppBar>,
    tooltip: Option<Tooltip>,
    overflow: Option<Overflow>,
    strip: Option<Strip>,
    bars: Vec<Bar>,
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
    monitor: MonitorInfo,
    scale: f32,
    sys: SystemTheme,
    style: MaterialStyle,
    hover_style: HoverStyle,
    windows: WindowSet,
    foreground: Option<Hwnd>,
    groups: Vec<AppGroup>,
    clock_text: String,
    clock_w: f32,
    layout: BarLayout,
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
    /// A click on the hovered app: no strip opens until the cursor moves to another item.
    show_suppressed: bool,
    gpu_lost: bool,
    /// A full-screen app covers the bar's monitor: the bar windows are hidden.
    fullscreen: bool,
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
    /// Bar item under a right-button press.
    rpressed: Option<Hit>,
    /// Screen px of the mouse message being handled (forwarded with clicks and hovers).
    msg_pt: (i32, i32),
    /// Last left click on a tray icon, to tell a double click.
    last_left: Option<(TrayId, Instant)>,
    last_dump: String,
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
        // Primary monitor only in this stage (multi-monitor is Task 14).
        let monitor = list_monitors()
            .into_iter()
            .find(|m| m.primary)
            .ok_or_else(|| Error::new(E_FAIL, "no primary monitor"))?;
        let scale = monitor.dpi as f32 / 96.0;
        let sys = read_system_theme();
        let notify = NotifyWindow::create()?;
        // Carry 10: after the UI thread's own CoInitializeEx.
        let desktops = Desktops::new()?;
        // Carry 6: on the thread that owns `notify` and pumps its messages.
        let tracker = WindowTracker::start(notify.hwnd)?;
        let icons =
            IconLoader::start(notify.hwnd).map_err(|e| Error::new(E_FAIL, e.to_string()))?;
        let launcher = Launcher::start(None).map_err(|e| Error::new(E_FAIL, e.to_string()))?;
        // The bar works without it (no tray icons).
        let tray = TrayHost::start(notify.hwnd)
            .inspect_err(|e| eprintln!("vitrobar: the tray host could not start: {e}"))
            .ok();
        let mut app = Self {
            tray,
            appbar: None,
            tooltip: None,
            overflow: None,
            strip: None,
            bars: Vec::new(),
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
            monitor,
            scale,
            sys,
            windows: WindowSet::new(),
            foreground: foreground(),
            groups: Vec::new(),
            clock_text: String::new(),
            clock_w: CLOCK_MIN_DIP,
            layout: compute_layout(&LayoutInput {
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
            }),
            hover: None,
            pressed: None,
            tracking: None,
            show_pending: None,
            hide_pending: false,
            collapse_pending: false,
            over_flyout: false,
            flyout_pressed: None,
            show_suppressed: false,
            gpu_lost: false,
            fullscreen: false,
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
        };
        app.tick_clock();
        // Show the bar before the (slower) first window enumeration.
        app.refresh()?;
        let all = app.tracker.enumerate();
        app.windows.load(&all, &app.desktops);
        app.regroup();
        app.set_fullscreen(is_fullscreen_on(app.monitor.id));
        app.refresh()?;
        // Every app registers its tray icons again, now with us (and, forwarded, Explorer).
        if let Some(t) = &app.tray {
            t.announce();
        }
        Ok(app)
    }

    fn monitor_dip(&self) -> Rect {
        let (l, t, r, b) = self.monitor.rect_px;
        Rect {
            x: l as f32 / self.scale,
            y: t as f32 / self.scale,
            w: (r - l) as f32 / self.scale,
            h: (b - t) as f32 / self.scale,
        }
    }

    fn compute_layout(&self) -> BarLayout {
        compute_layout(&LayoutInput {
            monitor: self.monitor_dip(),
            shape: self.settings.shape,
            position: self.settings.position,
            icon_size: self.settings.icon_size,
            app_count: self.groups.len(),
            tray_count: self.tray_bar.len(),
            clock_w: self.clock_w,
            reserve_floating: self.settings.reserve_space_floating,
        })
    }

    fn env(&self) -> VisualEnv {
        VisualEnv {
            dpi: self.monitor.dpi,
            accent: rgb(self.settings.accent.unwrap_or(self.sys.accent)),
            animations: self.sys.animations,
            position: self.settings.position,
        }
    }

    /// Extra px beyond the bar's outer edge, (above, below), that a magnified icon can reach
    /// (spring overshoot included). Only Magnify grows icons past the edge.
    fn hover_margin(&self) -> (i32, i32) {
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
        let px = ((over + 1.0) * self.scale).ceil() as i32;
        match self.settings.position {
            Position::Bottom => (px, 0),
            Position::Top => (0, px),
        }
    }

    /// Rebuilds the groups from the window set; true if they changed.
    fn regroup(&mut self) -> bool {
        let groups = build_groups(
            &self.settings.pins,
            &self.windows.infos(),
            self.monitor.id,
            self.foreground,
        );
        if groups == self.groups {
            return false;
        }
        self.groups = groups;
        // Hover/press indices may now point at a different app.
        if matches!(self.hover, Some(Hit::App(_))) {
            self.hover = None;
        }
        if matches!(self.pressed, Some(Hit::App(_))) {
            self.pressed = None;
        }
        true
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

    fn items(&mut self, shown: usize) -> Vec<ItemState> {
        let icon_px = ((metrics(self.settings.icon_size).icon * self.scale).round() as u32)
            .clamp(1, MAX_ICON_PX);
        let mut items = Vec::with_capacity(shown);
        for (i, g) in self.groups.iter().take(shown).enumerate() {
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
                hovered: self.hover == Some(Hit::App(i)),
                pressed: self.pressed == Some(Hit::App(i)),
            });
        }
        let groups = &self.groups;
        self.icons
            .trim(|src| groups.iter().any(|g| icon_sources(g).contains(src)));
        items
    }

    /// Tray icons are drawn at this many px.
    fn tray_px(&self) -> u32 {
        ((TRAY_ICON_DIP * self.scale).round() as u32).clamp(1, MAX_ICON_PX)
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
                pressed: self.pressed == Some(Hit::Tray(i)) || self.rpressed == Some(Hit::Tray(i)),
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

    /// Lays out and redraws (only what changed). Windows are created/moved as needed.
    fn refresh(&mut self) -> Result<()> {
        self.refresh_once(true)
    }

    /// `retry`: after a lost device is recovered mid-draw, draw once more (only once).
    fn refresh_once(&mut self, retry: bool) -> Result<()> {
        if self.gpu_lost {
            // Recovery is driven by the removal event and the one-shot retry timer only.
            return Ok(());
        }
        self.reload_tray();
        let layout = self.compute_layout();
        let moved = layout.surfaces != self.layout.surfaces;
        while self.bars.len() > layout.surfaces.len() {
            self.bars.pop();
        }
        for i in self.bars.len()..layout.surfaces.len() {
            let window = create_bar_window(
                &self.comp,
                to_px(layout.surfaces[i], self.scale),
                self.hover_margin(),
            )?;
            let visual = BarVisual::new(&self.comp, &self.gpu, i)?;
            window.set_content(visual.root())?;
            self.bars.push(Bar {
                visual,
                window,
                shown: false,
            });
        }
        let items = self.items(layout.apps.len());
        let tray_items = self.tray_items();
        let chevron = self.chevron_state();
        let env = self.env();
        let margin = self.hover_margin();
        let mut draw_failed = None;
        let mut raised = moved;
        for (i, bar) in self.bars.iter_mut().enumerate() {
            bar.window
                .set_rect(to_px(layout.surfaces[i], self.scale), margin)?;
            bar.visual.set_env(env);
            bar.visual.set_tray(tray_items.clone(), chevron);
            if let Err(e) = bar.visual.update(
                &layout,
                &items,
                &self.clock_text,
                &self.style,
                &self.hover_style,
            ) {
                draw_failed = Some(e);
                break;
            }
            if !bar.shown && !self.fullscreen {
                bar.window.show();
                bar.shown = true;
                raised = true;
            }
        }
        self.layout = layout;
        if raised {
            // The bar windows just went (back) to the top of the topmost band: keep our
            // Shell_TrayWnd above Explorer's there.
            if let Some(t) = &self.tray {
                t.reassert_topmost();
            }
        }
        self.sync_appbar();
        self.sync_overflow();
        self.sync_tooltip();
        if let Some(e) = draw_failed {
            // Carry 2: a failed draw is usually a lost device; recover (or retry later).
            if self.gpu.is_lost() {
                self.gpu_lost = true;
                if self.recover_gpu() && retry {
                    return self.refresh_once(false);
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

    /// Keeps the main bar window registered as the appbar reserving the bar's strip.
    fn sync_appbar(&mut self) {
        let Some(bar) = self.bars.first() else {
            self.drop_appbar();
            return;
        };
        let hwnd = bar.window.hwnd;
        let reserved = (self.layout.reserved * self.scale).round() as i32;
        let (edge, mon) = (self.settings.position, self.monitor.rect_px);
        match &mut self.appbar {
            // A new edge registers afresh, so the shell drops the old strip entirely.
            Some(ab) if ab.hwnd() == hwnd && ab.edge() == edge => ab.update(edge, mon, reserved),
            _ => {
                self.drop_appbar();
                match AppBar::register(hwnd, edge, mon, reserved) {
                    Ok(ab) => {
                        shell::set_appbar_hwnd(Some(hwnd));
                        self.appbar = Some(ab);
                    }
                    Err(e) => eprintln!("vitrobar: could not reserve the bar's space: {e}"),
                }
            }
        }
    }

    fn drop_appbar(&mut self) {
        if self.appbar.take().is_some() {
            shell::set_appbar_hwnd(None);
        }
    }

    /// Explorer restarted: hide its new taskbar and register the appbar with the new shell.
    fn on_taskbar_created(&mut self) {
        self.stock.on_taskbar_created();
        if let Some(ab) = &mut self.appbar {
            if let Err(e) = ab.reregister() {
                eprintln!("vitrobar: could not reserve the bar's space again: {e}");
            }
        }
        // SAFETY: one-shot timer on our own window (killed when it fires).
        unsafe { SetTimer(Some(self.notify.hwnd), TIMER_REASSERT, REASSERT_MS, None) };
    }

    /// Hides the bar (at once, no animation) while a full-screen app covers its monitor, and
    /// lets `refresh` show it again afterwards. True if the state changed.
    fn set_fullscreen(&mut self, on: bool) -> bool {
        if on == self.fullscreen {
            return false;
        }
        self.fullscreen = on;
        if on {
            self.close_strip();
            self.close_overflow();
            self.set_tip_target(None);
            self.cancel_show();
            self.hover = None;
            self.pressed = None;
            self.tracking = None;
            for b in &mut self.bars {
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

    /// Recreates the GPU device; on failure arms a one-shot retry. True when usable again.
    fn recover_gpu(&mut self) -> bool {
        match self.gpu.recreate() {
            Ok(()) => {
                self.gpu_lost = false;
                for bar in &mut self.bars {
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

    fn dump_state(&mut self) {
        let l = &self.layout;
        let s = self.scale;
        let apps: Vec<String> = self
            .groups
            .iter()
            .zip(&l.apps)
            .map(|(g, r)| {
                let key = match &g.key {
                    GroupKey::Aumid(a) => format!("aumid:{a}"),
                    GroupKey::Path(p) => format!("path:{p}"),
                    GroupKey::Window(h) => format!("hwnd:{h}"),
                };
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
        let bars: Vec<String> = self
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
            Some(Hit::App(i)) => i.to_string(),
            _ => "null".to_string(),
        };
        let appbar = match &self.appbar {
            Some(_) => json_rect(reserved_rect(
                self.settings.position,
                self.monitor.rect_px,
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
            "{{\"shape\":\"{:?}\",\"material\":\"{:?}\",\"hover\":\"{:?}\",\"icon_size\":\"{:?}\",\"position\":\"{:?}\",\"start_with_windows\":{}}}",
            st.shape, st.material, st.hover, st.icon_size, st.position, st.start_with_windows
        );
        let pins: Vec<String> = st.pins.iter().map(|p| json_str(&p.name)).collect();
        let dump = format!(
            "{{\"settings\":{},\"pins\":[{}],\"tray_host\":{},\"chevron\":{},\"tray\":[{}],\"overflow\":{},\"tooltip\":{},\"fullscreen\":{},\"stock_taken\":{},\"appbar\":{},\"hover\":{},\"strip\":{},\"live_thumbnails\":{},\"bars\":[{}],\"groups\":{},\"shown\":{},\"start\":{},\"quick\":{},\"clock\":{},\"clock_text\":{},\"more\":{},\"apps\":[{}]}}",
            settings,
            pins.join(","),
            tray_host,
            json_rect(to_px(l.overflow_chevron, s)),
            tray.join(","),
            overflow,
            tooltip,
            self.fullscreen,
            self.stock.active(),
            appbar,
            hover,
            strip,
            live_thumbnails(),
            bars.join(","),
            self.groups.len(),
            l.apps.len(),
            json_rect(to_px(l.start, s)),
            json_rect(to_px(l.quick, s)),
            json_rect(to_px(l.clock, s)),
            json_str(&self.clock_text),
            l.more.is_some(),
            apps.join(","),
        );
        if dump != self.last_dump {
            eprintln!("vitrobar-state {dump}");
            self.last_dump = dump;
        }
    }

    fn on_window_event(&mut self, ev: WindowEvent, dirty: &mut Dirty) {
        match ev {
            WindowEvent::Created(h) | WindowEvent::Changed(h) => {
                dirty.windows.insert(h);
                // The foreground window moved or resized: it may have gone full screen.
                dirty.fullscreen |= self.foreground == Some(h);
            }
            WindowEvent::Destroyed(h) => {
                dirty.windows.remove(&h);
                dirty.regroup |= self.windows.remove(h);
                dirty.fullscreen |= self.fullscreen;
            }
            WindowEvent::Activated(h) => {
                // Carry 7: activation ends a window's attention request.
                let fg = h.or_else(foreground);
                if let Some(h) = fg {
                    dirty.regroup |= self.windows.set_flashing(h, false);
                }
                dirty.regroup |= self.foreground != fg;
                self.foreground = fg;
                dirty.fullscreen = true;
                // A foreground change is when the shell reorders the topmost band: keep our
                // Shell_TrayWnd above Explorer's (async, never waits).
                if let Some(t) = &self.tray {
                    t.reassert_topmost();
                }
            }
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
                dirty.fullscreen = true;
            }
            WindowEvent::DesktopSwitched => {
                dirty.regroup |= self.windows.refresh_desktops(&self.desktops);
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
            TIMER_MARGIN_COLLAPSE => {
                self.kill_timer(TIMER_MARGIN_COLLAPSE);
                self.collapse_pending = false;
                for b in &mut self.bars {
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
            other => self.tracker.on_timer(other),
        }
    }

    fn bar_index(&self, hwnd: HWND) -> Option<usize> {
        self.bars.iter().position(|b| b.window.hwnd == hwnd)
    }

    /// Mouse input on a bar window or a flyout; returns true if the message was handled.
    fn on_mouse(&mut self, msg: &MSG, dirty: &mut Dirty) -> bool {
        self.msg_pt = message_pos();
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
        let Some(i) = self.bar_index(msg.hwnd) else {
            return false;
        };
        let (cx, cy) = mouse_pos(msg.lParam);
        let (x, y) = self.bars[i].window.client_to_dip(cx, cy, self.scale);
        let mut hit = hit_test(&self.layout, x, y);
        if hit == Some(Hit::Chevron) && self.tray_over.is_empty() {
            // No overflow, no chevron.
            hit = None;
        }
        match msg.message {
            WM_MOUSEMOVE => {
                if self.tracking != Some(msg.hwnd) {
                    self.bars[i].window.track_leave();
                    self.tracking = Some(msg.hwnd);
                }
                if self.hover != hit {
                    self.hover = hit;
                    self.show_suppressed = false;
                    dirty.visuals = true;
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
                        dirty.visuals = true;
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
                dirty.visuals = true;
            }
            WM_LBUTTONUP => {
                let pressed = self.pressed.take();
                dirty.visuals = true;
                if pressed.is_some() && pressed == hit {
                    if let Some(h) = hit {
                        self.click(h);
                    }
                }
            }
            WM_RBUTTONDOWN => {
                self.rpressed = hit;
                self.hide_tooltip();
                dirty.visuals = true;
            }
            WM_RBUTTONUP => {
                let pressed = self.rpressed.take();
                dirty.visuals = true;
                match hit {
                    Some(Hit::Tray(k)) => {
                        if pressed == hit {
                            self.close_overflow();
                            self.tray_click(self.tray_bar.get(k).copied(), TrayClick::Right);
                        }
                    }
                    Some(Hit::App(i)) => {
                        if pressed == hit {
                            self.context_menu(Some(i), dirty);
                        }
                    }
                    // Anywhere else on the bar: the bar's own menu (pressed there too).
                    _ if pressed == hit => self.context_menu(None, dirty),
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
        let idx = *self.tray_bar.get(k)?;
        let r = self.layout.tray.get(k)?;
        let s = self
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
                self.scale,
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
            self.monitor.dpi,
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
        let r = self.layout.overflow_chevron;
        let s = self
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
            self.scale,
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
        let opened = Flyout::show(
            &self.comp,
            anchor,
            self.settings.position,
            size,
            self.monitor.dpi,
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

    /// Keys reach us only while the overflow is the foreground window: Esc closes it.
    fn on_key(&mut self, msg: &MSG, dirty: &mut Dirty) -> bool {
        let ours = self
            .overflow
            .as_ref()
            .is_some_and(|o| o.flyout.hwnd() == msg.hwnd);
        if !ours {
            return false;
        }
        if msg.wParam.0 == usize::from(VK_ESCAPE.0) {
            self.close_overflow();
            dirty.visuals = true;
        }
        true
    }

    /// Mouse input on the flyout: hovering keeps the strip open, a click on a thumbnail
    /// switches to that window.
    fn on_flyout_mouse(&mut self, msg: &MSG) -> bool {
        let pt = mouse_pos(msg.lParam);
        let hit = self.strip.as_ref().and_then(|s| s.thumbs.hit(pt));
        match msg.message {
            WM_MOUSEMOVE => {
                if !self.over_flyout {
                    track_leave(msg.hwnd);
                    self.over_flyout = true;
                }
            }
            WM_MOUSELEAVE => {
                self.over_flyout = false;
                self.flyout_pressed = None;
            }
            WM_LBUTTONDOWN => self.flyout_pressed = hit,
            WM_LBUTTONUP => {
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
        for b in &mut self.bars {
            if on_app && tracking == Some(b.window.hwnd) {
                b.window.set_expanded(true);
            } else {
                if b.window.mode() == MarginMode::Active {
                    if self.sys.animations {
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
            for b in &mut self.bars {
                b.window.finish_linger();
            }
        }
    }

    /// The hovered app, if it has windows (and so a strip).
    fn hovered_strip_app(&self) -> Option<usize> {
        match self.hover {
            Some(Hit::App(i)) if self.groups.get(i).is_some_and(|g| !g.windows.is_empty()) => {
                Some(i)
            }
            _ => None,
        }
    }

    /// Arms or cancels the hover timers after the cursor moved.
    fn update_strip(&mut self) {
        let hovered = self.hovered_strip_app();
        if let Some(s) = &self.strip {
            let own = hovered.is_some_and(|i| self.groups[i].key == s.group);
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

    /// Screen px of app `i`'s slot, spanning the full height of its bar surface.
    fn anchor_px(&self, i: usize) -> Option<PxRect> {
        let r = self.layout.apps.get(i)?;
        let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
        let s = self
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
            self.scale,
        ))
    }

    /// Opens the strip of app `i` now (replacing any other strip).
    fn open_strip(&mut self, i: usize) {
        self.cancel_show();
        self.cancel_hide();
        self.build_strip(i);
    }

    /// (Re)builds the strip of app `i` unless it already shows exactly that; the hover
    /// timers are left alone.
    fn build_strip(&mut self, i: usize) {
        let Some(g) = self.groups.get(i) else { return };
        if g.windows.is_empty() {
            self.close_strip();
            return;
        }
        let Some(anchor) = self.anchor_px(i) else {
            self.close_strip();
            return;
        };
        if self
            .strip
            .as_ref()
            .is_some_and(|s| s.group == g.key && s.windows == g.windows && s.anchor == anchor)
        {
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
            self.monitor.dpi,
            &self.style,
        )
        .and_then(|flyout| {
            let thumbs = ThumbnailStrip::show(&flyout, &self.painter, self.style.text, &windows)?;
            Ok(Strip {
                thumbs,
                flyout,
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
                self.flyout_pressed = None;
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
            self.flyout_pressed = None;
            if self.opts.debug_state {
                self.dump_state();
            }
        }
    }

    /// After the groups or the layout changed: follow the strip's group, or close it.
    fn sync_strip(&mut self) {
        let Some(s) = &self.strip else { return };
        match self.groups.iter().position(|g| g.key == s.group) {
            Some(i) if i < self.layout.apps.len() => self.build_strip(i),
            _ => self.close_strip(),
        }
    }

    fn click(&mut self, hit: Hit) {
        if !matches!(hit, Hit::App(_)) {
            self.close_strip();
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
            Hit::Start => open_stock_start(),
            Hit::Quick => self.launcher.launch(Job::QuickSettings),
            Hit::Clock => self.launcher.launch(Job::Notifications),
            Hit::App(i) => {
                let Some(g) = self.groups.get(i) else { return };
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
            Hit::More | Hit::ShowDesktop => {}
        }
    }

    /// Ends every transient interaction (strip, overflow, tooltip, hover, presses, margin
    /// expansion): before a modal menu and before the bar is laid out anew.
    fn reset_interaction(&mut self) {
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
        for b in &mut self.bars {
            b.window.set_expanded(false);
        }
    }

    /// Right-click: the menu of app `app`, or the bar's own menu. Modal.
    fn context_menu(&mut self, app: Option<usize>, dirty: &mut Dirty) {
        self.reset_interaction();
        dirty.visuals = true;
        let lang = ui_lang(&self.settings);
        let (owner, pt) = (self.notify.hwnd, self.msg_pt);
        match app {
            Some(i) => {
                let Some(g) = self.groups.get(i).cloned() else {
                    return;
                };
                let choice = app_menu(owner, pt, g.pin.is_some(), !g.windows.is_empty(), lang);
                if self.opts.debug_state {
                    eprintln!("vitrobar-menu app {i} {choice:?}");
                }
                if let Some(c) = choice {
                    self.apply_app_choice(&g, c, dirty);
                }
            }
            None => {
                let choice = bar_menu(owner, pt, &self.settings, lang);
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
        // Group indices (and so hover and press) refer to the old pins.
        self.regroup();
        dirty.visuals = true;
    }

    fn apply_bar_choice(&mut self, choice: MenuChoice, dirty: &mut Dirty) {
        let s = &mut self.settings;
        let changed = match choice {
            MenuChoice::SetShape(v) => std::mem::replace(&mut s.shape, v) != v,
            MenuChoice::SetMaterial(v) => std::mem::replace(&mut s.material, v) != v,
            MenuChoice::SetHover(v) => std::mem::replace(&mut s.hover, v) != v,
            MenuChoice::SetIconSize(v) => std::mem::replace(&mut s.icon_size, v) != v,
            MenuChoice::SetPosition(v) => std::mem::replace(&mut s.position, v) != v,
            MenuChoice::ToggleStartWithWindows => {
                // The Run key itself is written by a later stage; the setting is kept now.
                s.start_with_windows = !s.start_with_windows;
                self.save_settings();
                return;
            }
            MenuChoice::Exit => {
                // The normal way out: the loop ends and the stock taskbar comes back.
                // SAFETY: plain call.
                unsafe { PostQuitMessage(0) };
                return;
            }
            _ => false,
        };
        if changed {
            self.save_settings();
            self.relayout(dirty);
        }
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
        dirty.theme |= inbox.theme;
        dirty.clock |= inbox.clock;
        dirty.icons |= inbox.icons;
        dirty.fullscreen |= inbox.fullscreen;
        // The model is shared with the host; the messages only say it changed.
        dirty.visuals |= !inbox.tray.is_empty();
        let deactivated = take_deactivated();
        if self
            .overflow
            .as_ref()
            .is_some_and(|o| deactivated.contains(&o.flyout.hwnd()))
        {
            self.close_overflow();
            dirty.visuals = true;
        }
        if inbox.fullscreen {
            // Explorer may have raised its own Shell_TrayWnd for the full-screen change: go
            // back above it. (Restarts, settings and display changes reach the tray host's
            // own window, which handles them.)
            if let Some(t) = &self.tray {
                t.reassert_topmost();
            }
        }
        if inbox.taskbar_created {
            self.on_taskbar_created();
        } else if inbox.shell || inbox.display {
            self.stock.reassert();
        }
        if inbox.appbar_pos || inbox.display {
            if let Some(ab) = &self.appbar {
                ab.set_pos();
            }
        }
    }

    /// Applies everything collected from one drain of the queue.
    fn flush(&mut self, mut d: Dirty) -> Result<()> {
        if !d.windows.is_empty() {
            for h in std::mem::take(&mut d.windows) {
                d.regroup |= self.windows.refresh(h, &self.desktops);
            }
        }
        let mut redraw = d.visuals;
        if d.fullscreen {
            redraw |= self.set_fullscreen(is_fullscreen_on(self.monitor.id));
        }
        if d.regroup {
            redraw |= self.regroup();
        }
        if d.theme {
            let sys = read_system_theme();
            let style = style_for(&self.settings, &sys);
            if sys != self.sys || style != self.style {
                self.sys = sys;
                self.style = style;
                redraw = true;
            }
        }
        if d.clock {
            redraw |= self.tick_clock();
        }
        if d.icons {
            redraw |= self.icons.drain();
        }
        if redraw {
            self.refresh()?;
            self.sync_strip();
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

pub fn run(opts: Options) -> Result<()> {
    // SAFETY: plain apartment initialisation of the UI thread; balanced by ComGuard.
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()? };
    let _com = ComGuard;
    // SAFETY: plain calls; the handler only posts a message.
    unsafe {
        UI_THREAD.store(GetCurrentThreadId(), Ordering::SeqCst);
        SetConsoleCtrlHandler(Some(on_console_ctrl), true)?;
    }

    let path = settings_path();
    let mut settings = path.as_deref().map(settings::load).unwrap_or_default();
    if !settings.first_run_done {
        // Before the stock taskbar is taken over: the import only reads.
        first_run_import(&mut settings);
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
    // Before the bar shows; restored when `app` (or a failing `App::new`) drops it.
    let stock = StockGuard::take_over(settings.stock_taskbar);
    let mut app = App::new(opts, settings, path, stock)?;
    let mut msg = MSG::default();
    'event: loop {
        let removed = app.gpu.removed_event();
        // SAFETY: the handle stays valid while `app.gpu` lives.
        let wait = unsafe {
            MsgWaitForMultipleObjectsEx(
                Some(&[removed]),
                INFINITE,
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
        }
        // SAFETY: standard message pump; boxed window events are reclaimed exactly once.
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            // Everything for the notify window goes through its window procedure, which records
            // it in the inbox (the same path a nested modal loop would take).
            match msg.message {
                WM_QUIT => break 'event,
                WM_MOUSEMOVE | WM_MOUSELEAVE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN
                | WM_RBUTTONUP
                    if app.on_mouse(&msg, &mut dirty) => {}
                WM_KEYDOWN if app.on_key(&msg, &mut dirty) => {}
                _ => unsafe {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                },
            }
        }
        app.absorb(take_inbox(), &mut dirty);
        app.flush(dirty)?;
    }
    drop(app);
    Ok(())
}
