//! App state and the event-driven message loop (nothing redraws unless something happened).
//!
//! Timers: the minute clock tick, the tracker's coalescing timers and a one-shot GPU
//! recreation retry. Everything else is driven by window, shell and input messages.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use vitro_core::clock::ms_until_next_minute;
use vitro_core::layout::{compute_layout, metrics, BarLayout, LayoutInput, Rect};
use vitro_core::settings::{self, Settings, ThemeChoice};
use vitro_core::style::{hover_style, material_style, HoverStyle, MaterialStyle, Rgba, Theme};
use vitro_core::windows_model::{
    build_groups, click_action, AppGroup, ClickAction, GroupKey, Hwnd,
};
use vitro_render::bar_visual::{BarVisual, ItemState, VisualEnv};
use vitro_render::compositor::{init_compositor, Comp};
use vitro_render::device::{create_gpu_device, GpuDevice};
use vitro_render::surfaces::{FontKind, Painter};
use vitro_win::desktops::Desktops;
use vitro_win::launch::{
    activate_window, launch_pin, minimize_window, open_stock_notifications,
    open_stock_quick_settings, open_stock_start,
};
use vitro_win::monitors::{list_monitors, MonitorInfo};
use vitro_win::system_theme::{read_system_theme, SystemTheme};
use vitro_win::time_format::clock_now;
use vitro_win::window_tracker::{
    shell_hook_message_id, take_window_event, translate_shell_hook, WindowEvent, WindowTracker,
    WM_VITRO_WINDOW_EVENT,
};
use windows::core::{Error, Result, BOOL};
use windows::Win32::Foundation::{E_FAIL, HWND, LPARAM, WAIT_FAILED, WAIT_OBJECT_0, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Console::SetConsoleCtrlHandler;
use windows::Win32::System::Threading::{GetCurrentThreadId, INFINITE};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetForegroundWindow, KillTimer, MsgWaitForMultipleObjectsEx, PeekMessageW,
    PostThreadMessageW, SetTimer, TranslateMessage, MSG, MWMO_INPUTAVAILABLE, PM_REMOVE,
    QS_ALLINPUT, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_QUIT, WM_TIMER,
};

use crate::bar_window::{create_bar_window, hit_test, mouse_pos, BarWindow, Hit, PxRect};
use crate::icon_loader::{IconLoader, IconSrc, Lookup, MAX_ICON_PX};
use crate::notify::{
    NotifyWindow, TIMER_CLOCK, TIMER_GPU_RETRY, WM_APP_CLOCK, WM_APP_ICON, WM_APP_THEME,
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
    // Field order = drop order: bars, then the tracker (before its notify window), then the
    // notify window (drains queued events), then the GPU objects, the compositor last.
    bars: Vec<Bar>,
    tracker: WindowTracker,
    icons: IconLoader,
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
    gpu_lost: bool,
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
        let mut app = Self {
            bars: Vec::new(),
            tracker,
            icons,
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
            gpu_lost: false,
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
        unsafe { SetTimer(Some(self.notify.hwnd), TIMER_CLOCK, wait, None) };
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
                        icon = Some(img.clone());
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
        if self.gpu_lost && !self.recover_gpu() {
            return Ok(());
        }
        let layout = self.compute_layout();
        while self.bars.len() > layout.surfaces.len() {
            self.bars.pop();
        }
        for i in self.bars.len()..layout.surfaces.len() {
            let window = create_bar_window(&self.comp, to_px(layout.surfaces[i], self.scale))?;
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
        let mut draw_failed = None;
        for (i, bar) in self.bars.iter_mut().enumerate() {
            bar.window.set_rect(to_px(layout.surfaces[i], self.scale))?;
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
                    "{{\"hwnd\":{},\"rect\":{}}}",
                    b.window.hwnd.0 as usize,
                    json_rect(b.window.rect_px())
                )
            })
            .collect();
        let dump = format!(
            "{{\"bars\":[{}],\"groups\":{},\"shown\":{},\"start\":{},\"quick\":{},\"clock\":{},\"clock_text\":{},\"more\":{},\"apps\":[{}]}}",
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
                dirty.visuals = true;
            }
            other => self.tracker.on_timer(other),
        }
    }

    fn bar_index(&self, hwnd: HWND) -> Option<usize> {
        self.bars.iter().position(|b| b.window.hwnd == hwnd)
    }

    /// Mouse input on a bar window; returns true if the message was handled.
    fn on_mouse(&mut self, msg: &MSG, dirty: &mut Dirty) -> bool {
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
                    dirty.visuals = true;
                }
            }
            WM_MOUSELEAVE => {
                if self.tracking == Some(msg.hwnd) {
                    self.tracking = None;
                }
                if self.hover.is_some() || self.pressed.is_some() {
                    self.hover = None;
                    self.pressed = None;
                    dirty.visuals = true;
                }
            }
            WM_LBUTTONDOWN => {
                self.pressed = hit;
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
        true
    }

    fn click(&mut self, hit: Hit) {
        match hit {
            Hit::Start => open_stock_start(),
            Hit::Quick => open_stock_quick_settings(),
            Hit::Clock => open_stock_notifications(),
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
                            if let Err(e) = launch_pin(pin) {
                                eprintln!("vitrobar: launching {:?} failed: {e}", pin.name);
                            }
                        }
                    }
                    ClickAction::Activate(h) => {
                        if !activate_window(hwnd_of(h)) {
                            eprintln!("vitrobar: SetForegroundWindow refused for {h}");
                        }
                    }
                    ClickAction::Minimize(h) => minimize_window(hwnd_of(h)),
                    // Thumbnails are Task 10.
                    ClickAction::ShowThumbnails => {}
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
        if redraw || self.gpu_lost {
            self.refresh()?;
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
    let shell_hook = shell_hook_message_id();
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
            let to_notify = msg.hwnd == app.notify.hwnd;
            match msg.message {
                WM_QUIT => break 'event,
                m if to_notify && m == WM_VITRO_WINDOW_EVENT => {
                    // SAFETY: posted by the tracker to the notify window; taken once here.
                    let ev = unsafe { take_window_event(msg.lParam) };
                    app.on_window_event(ev, &mut dirty);
                }
                m if to_notify && m == shell_hook => {
                    if let Some(ev) = translate_shell_hook(msg.wParam, msg.lParam) {
                        app.on_window_event(ev, &mut dirty);
                    }
                }
                WM_TIMER if to_notify => app.on_timer(msg.wParam.0, &mut dirty),
                WM_APP_THEME if to_notify => dirty.theme = true,
                WM_APP_CLOCK if to_notify => dirty.clock = true,
                WM_APP_ICON if to_notify => dirty.icons = true,
                WM_MOUSEMOVE | WM_MOUSELEAVE | WM_LBUTTONDOWN | WM_LBUTTONUP
                    if app.on_mouse(&msg, &mut dirty) => {}
                _ => unsafe {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                },
            }
        }
        app.flush(dirty)?;
    }
    drop(app);
    Ok(())
}
