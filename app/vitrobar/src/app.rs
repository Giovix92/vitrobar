//! App state and the event-driven message loop (nothing redraws unless something happened).
//!
//! Timers: the minute clock tick, the tracker's coalescing timers, a one-shot GPU
//! recreation retry and the one-shot hover timers of the thumbnail strip. Everything else is
//! driven by window, shell and input messages.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use vitro_core::clock::ms_until_next_minute;
use vitro_core::layout::{compute_layout, metrics, BarLayout, LayoutInput, Rect};
use vitro_core::settings::{self, Position, Settings, ThemeChoice};
use vitro_core::style::{hover_style, material_style, HoverStyle, MaterialStyle, Rgba, Theme};
use vitro_core::windows_model::{
    build_groups, click_action, AppGroup, ClickAction, GroupKey, Hwnd,
};
use vitro_render::bar_visual::{BarVisual, ItemState, VisualEnv};
use vitro_render::compositor::{init_compositor, Comp};
use vitro_render::device::{create_gpu_device, GpuDevice};
use vitro_render::flyout::{live_thumbnails, Flyout, ThumbnailStrip};
use vitro_render::surfaces::{FontKind, Painter};
use vitro_win::desktops::Desktops;
use vitro_win::launch::{activate_window, minimize_window, open_stock_start};
use vitro_win::monitors::{list_monitors, MonitorInfo};
use vitro_win::system_theme::{read_system_theme, SystemTheme};
use vitro_win::time_format::clock_now;
use vitro_win::window_tracker::{WindowEvent, WindowTracker};
use windows::core::{Error, Result, BOOL};
use windows::Win32::Foundation::{E_FAIL, HWND, LPARAM, WAIT_FAILED, WAIT_OBJECT_0, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Console::SetConsoleCtrlHandler;
use windows::Win32::System::Threading::{GetCurrentThreadId, INFINITE};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetForegroundWindow, KillTimer, MsgWaitForMultipleObjectsEx, PeekMessageW,
    PostThreadMessageW, SetTimer, TranslateMessage, MSG, MWMO_INPUTAVAILABLE, PM_REMOVE,
    QS_ALLINPUT, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_QUIT,
};

use crate::bar_window::{
    create_bar_window, hit_test, mouse_pos, track_leave, BarWindow, Hit, PxRect,
};
use crate::icon_loader::{IconLoader, IconSrc, Lookup, MAX_ICON_PX};
use crate::launcher::{Job, Launcher};
use crate::notify::{
    take_inbox, Inbox, NotifyWindow, TIMER_CLOCK, TIMER_GPU_RETRY, TIMER_HOVER_HIDE,
    TIMER_HOVER_SHOW,
};
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

static UI_THREAD: AtomicU32 = AtomicU32::new(0);

unsafe extern "system" fn on_console_ctrl(_ctrl_type: u32) -> BOOL {
    // Ctrl+C, Ctrl+Break, console close: ask the UI thread to quit cleanly.
    let tid = UI_THREAD.load(Ordering::SeqCst);
    // SAFETY: posting a message to a thread id carries no pointers.
    unsafe {
        PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0))
            .is_ok()
            .into()
    }
}

fn settings_path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("Vitrobar").join("settings.toml"))
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
}

struct App {
    // Field order = drop order: the strip, the bars, then the tracker (before its notify
    // window), then the notify window (drains queued events), then the GPU objects, the
    // compositor last.
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

    opts: Options,
    settings: Settings,
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
    /// The cursor is over the flyout (tracked for WM_MOUSELEAVE).
    over_flyout: bool,
    /// Thumbnail under the button press in the flyout.
    flyout_pressed: Option<HWND>,
    /// A click on the hovered app: no strip opens until the cursor moves to another item.
    show_suppressed: bool,
    gpu_lost: bool,
    /// The minute timer could not be armed; retried on the next flush.
    clock_unarmed: bool,
    last_dump: String,
}

impl App {
    fn new(opts: Options) -> Result<Self> {
        let comp = init_compositor()?;
        let gpu = create_gpu_device(&comp)?;
        let painter = Painter::new(&gpu)?;
        // Read only: defaults when missing; the settings file is not written yet.
        let settings = settings_path()
            .map(|p| settings::load(&p))
            .unwrap_or_default();
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
        let mut app = Self {
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
            opts,
            style: style_for(&settings, &sys),
            hover_style: hover_style(settings.hover),
            settings,
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
            over_flyout: false,
            flyout_pressed: None,
            show_suppressed: false,
            gpu_lost: false,
            clock_unarmed: false,
            last_dump: String::new(),
        };
        app.tick_clock();
        // Show the bar before the (slower) first window enumeration.
        app.refresh()?;
        let all = app.tracker.enumerate();
        app.windows.load(&all, &app.desktops);
        app.regroup();
        app.refresh()?;
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
            tray_count: 0,
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
        let layout = self.compute_layout();
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
        let env = self.env();
        let margin = self.hover_margin();
        let mut draw_failed = None;
        for (i, bar) in self.bars.iter_mut().enumerate() {
            bar.window
                .set_rect(to_px(layout.surfaces[i], self.scale), margin)?;
            bar.visual.set_env(env);
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
            if !bar.shown {
                bar.window.show();
                bar.shown = true;
            }
        }
        self.layout = layout;
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
        let dump = format!(
            "{{\"hover\":{},\"strip\":{},\"live_thumbnails\":{},\"bars\":[{}],\"groups\":{},\"shown\":{},\"start\":{},\"quick\":{},\"clock\":{},\"clock_text\":{},\"more\":{},\"apps\":[{}]}}",
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
            }
            WindowEvent::Destroyed(h) => {
                dirty.windows.remove(&h);
                dirty.regroup |= self.windows.remove(h);
            }
            WindowEvent::Activated(h) => {
                // Carry 7: activation ends a window's attention request.
                let fg = h.or_else(foreground);
                if let Some(h) = fg {
                    dirty.regroup |= self.windows.set_flashing(h, false);
                }
                dirty.regroup |= self.foreground != fg;
                self.foreground = fg;
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

    /// Mouse input on a bar window or the flyout; returns true if the message was handled.
    fn on_mouse(&mut self, msg: &MSG, dirty: &mut Dirty) -> bool {
        if self
            .strip
            .as_ref()
            .is_some_and(|s| s.flyout.hwnd() == msg.hwnd)
        {
            return self.on_flyout_mouse(msg);
        }
        let Some(i) = self.bar_index(msg.hwnd) else {
            return false;
        };
        let (cx, cy) = mouse_pos(msg.lParam);
        let (x, y) = self.bars[i].window.client_to_dip(cx, cy, self.scale);
        let hit = hit_test(&self.layout, x, y);
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
                }
            }
            WM_MOUSELEAVE => {
                // With several bar windows (Islands) the move into the next window can arrive
                // before the leave of the previous one: only the tracked window's leave counts.
                if self.tracking == Some(msg.hwnd) {
                    self.tracking = None;
                    if self.hover.is_some() || self.pressed.is_some() {
                        self.hover = None;
                        self.pressed = None;
                        dirty.visuals = true;
                    }
                }
            }
            WM_LBUTTONDOWN => {
                self.pressed = hit;
                // A click decides what happens; the hover timer does not open the strip later.
                self.show_suppressed = true;
                self.cancel_show();
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
            _ => return false,
        }
        self.sync_expanded();
        self.update_strip();
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
    /// shows content in) its margin.
    fn sync_expanded(&mut self) {
        let on_app = matches!(self.hover, Some(Hit::App(_)));
        let tracking = self.tracking;
        for b in &mut self.bars {
            b.window
                .set_expanded(on_app && tracking == Some(b.window.hwnd));
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
        match hit {
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
    }

    /// Applies everything collected from one drain of the queue.
    fn flush(&mut self, mut d: Dirty) -> Result<()> {
        if !d.windows.is_empty() {
            for h in std::mem::take(&mut d.windows) {
                d.regroup |= self.windows.refresh(h, &self.desktops);
            }
        }
        let mut redraw = d.visuals;
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

    let mut app = App::new(opts)?;
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
                WM_MOUSEMOVE | WM_MOUSELEAVE | WM_LBUTTONDOWN | WM_LBUTTONUP
                    if app.on_mouse(&msg, &mut dirty) => {}
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
