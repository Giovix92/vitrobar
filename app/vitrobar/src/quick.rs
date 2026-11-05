//! The quick panel controller: Vitrobar's own glass Quick Settings above the bar's quick
//! button (Wi-Fi, Bluetooth, battery saver, power mode, brightness, volume, sound output).
//!
//! The system services are opened when the panel opens and dropped when it closes (P4).
//! Every service posts the payload-free [`WM_APP_QUICK`] to the notify window when something
//! changed; the app then calls [`QuickPanel::on_wake`], which drains all of them and redraws
//! only the rows that changed. Two one-shot timers cover what nothing reports: a Wi-Fi scan
//! that never completes and a Bluetooth connect the watcher never confirms. No polling.
//!
//! The Wi-Fi password lives in a [`TextInput`] only while its row is open; it is never logged,
//! is cleared right after the connect call, on a page change and when the panel closes.

use std::time::{Duration, Instant};

use vitro_core::i18n::{tr, Key, Lang};
use vitro_core::menu_model::NavKey;
use vitro_core::panel_kit::{focus_step, Part, Row, PAD_DIP, SLIDER_GLYPH_W_DIP};
use vitro_core::quick_model::{
    self, action, next_mode, nudge, Action, AudioInputs, BtDev, BtInputs, DisplayInput, Notice,
    PowerInputs, QuickInputs, QuickPage, QuickUi, WifiInputs, WifiNet, CUSTOM_PASSWORD,
    SLIDER_VOLUME,
};
use vitro_core::settings::{toggle_tile, Position, QuickTile};
use vitro_core::style::{MaterialStyle, Rgba};
use vitro_core::text_input::TextInput;
use vitro_render::compositor::Comp;
use vitro_render::flyout::PxRect;
use vitro_render::panel_view::PanelView;
use vitro_render::surfaces::{FontKind, Painter};
use vitro_render::{Vector2, Vector3};
use vitro_win::audio::{self, Audio, AudioSnapshot};
use vitro_win::bluetooth::{self, Bluetooth, BtError, BtSnapshot, PENDING_MAX};
use vitro_win::brightness::{Brightness, BrightnessKind, Display};
use vitro_win::power_status::{self, PowerMode, PowerRegistration, PowerSnapshot};
use vitro_win::system_theme;
use vitro_win::wifi::{self, NetKind, Wifi, WifiError, WifiSnapshot};
use windows::core::Result;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, VK_BACK, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME, VK_LEFT,
    VK_RETURN, VK_RIGHT, VK_SPACE, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    KillTimer, SetTimer, MSG, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
};
use windows::UI::Color;
use windows::UI::Composition::ContainerVisual;

use crate::bar_window::{mouse_pos, track_leave};
use crate::notify::{TIMER_QUICK_BT, TIMER_QUICK_SCAN, WM_APP_QUICK};

/// One mouse-wheel notch.
const WHEEL_DELTA: f32 = 120.0;
/// Wheel scroll of the list, DIPs per notch.
const WHEEL_SCROLL_DIP: f32 = 48.0;
/// Slack after a deadline, so the one-shot timer fires once it has passed.
const TIMER_SLACK_MS: u64 = 250;
/// The password box's text size, DIPs.
const PW_FONT_DIP: f32 = 14.0;
/// Characters the masked password keeps in the box (WPA passphrases have at most 63).
const PW_MASK: char = '\u{2022}';
/// `WM_MOUSELEAVE` (CommCtrl.h), as in app.rs.
const WM_MOUSELEAVE: u32 = 0x02A3;
/// `MK_LBUTTON` in a mouse message's `wParam`.
const MK_LBUTTON: usize = 0x0001;

/// What the app must do after an input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    None,
    Close,
    /// Close the panel and open this Windows Settings page.
    Open(String),
    /// The user changed which tiles show: save them (the panel already shows them).
    SaveTiles(Vec<QuickTile>),
    /// Close the panel and lock the PC.
    Lock,
}

/// What the panel draws with (owned by the app).
pub struct Gfx<'a> {
    pub comp: &'a Comp,
    pub painter: &'a Painter,
}

/// A slider being dragged and its live value (incoming changes of that value are ignored
/// until the button goes up: Core Audio echoes every set back as a volume change).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Drag {
    slider: u32,
    value: f32,
}

/// The services of an open panel. Dropped with it (P4).
#[derive(Default)]
struct Services {
    audio: Option<Audio>,
    brightness: Option<Brightness>,
    wifi: Option<Wifi>,
    bt: Option<Bluetooth>,
    _power: Option<PowerRegistration>,
}

pub struct QuickPanel {
    // Field order = drop order: the password visuals, then the view (window), then services.
    pw_slot: Option<ContainerVisual>,
    view: PanelView,
    services: Services,
    /// The foreground window when the panel opened: a panel that could not take the
    /// foreground closes when it moves away from this one (app.rs).
    pub fg_at_open: usize,
    /// The panel took the foreground (it then hears keys and its deactivation).
    pub activated: bool,
    notify: HWND,
    lang: Lang,
    /// The theme's text colour and the accent, for the password box.
    text: Rgba,
    accent: Rgba,
    page: QuickPage,
    // The snapshots the shown rows were built from: item indices refer to them.
    wifi: Option<WifiSnapshot>,
    bt: Option<BtSnapshot>,
    audio: Option<AudioSnapshot>,
    can_switch: bool,
    displays: Vec<Display>,
    power: PowerSnapshot,
    /// The tiles the user picked (settings), edited live on the Edit page.
    tiles: Vec<QuickTile>,
    dark_mode: Option<bool>,
    ui: QuickUi,
    password: TextInput,
    /// First masked character shown in the password box (scrolled so the caret stays inside).
    pw_first: usize,
    drag: Option<Drag>,
    /// A brightness value just set, shown until the worker reports it (see [`settle`]).
    hold: Option<(Drag, u8)>,
    /// The network connected when the pending attempt started, and whether the connection
    /// has changed since (see [`attempt_over`]).
    connect_from: Option<String>,
    connect_moved: bool,
    hot: Option<Part>,
    pressed: Option<Part>,
    /// A press on the volume glyph (mute), which is not a panel part.
    mute_pressed: bool,
    focus: Option<Part>,
    tracking: bool,
    /// The Wi-Fi scan deadline the one-shot timer is armed for.
    scan_timer: Option<Instant>,
    /// The Bluetooth pending timer is armed.
    bt_timer: bool,
    rows: Vec<Row>,
}

// ---- snapshot conversion (ruling S3-2) ----------------------------------------------------------

fn wifi_inputs(s: &WifiSnapshot) -> WifiInputs {
    WifiInputs {
        radio_on: s.radio_on,
        connected: s.connected.clone(),
        networks: s
            .networks
            .iter()
            .map(|n| WifiNet {
                ssid: n.ssid.clone(),
                bars: wifi::signal_bars(n.signal),
                secured: n.secured,
            })
            .collect(),
        scanning: s.scanning,
    }
}

fn bt_inputs(s: &BtSnapshot) -> BtInputs {
    BtInputs {
        radio_on: s.radio_on,
        devices: s
            .devices
            .iter()
            .map(|d| BtDev {
                name: d.name.clone(),
                connected: d.connected,
                audio: d.audio,
                battery: d.battery,
                pending: d.pending,
            })
            .collect(),
    }
}

/// `None` without a default playback device (nothing to set the volume of).
fn audio_inputs(s: &AudioSnapshot, can_switch: bool) -> Option<AudioInputs> {
    let default_id = s.default_id.as_deref()?;
    Some(AudioInputs {
        volume: s.volume,
        muted: s.muted,
        devices: s.devices.iter().map(|d| d.name.clone()).collect(),
        default: s.devices.iter().position(|d| d.id == default_id),
        can_switch,
    })
}

/// The built-in panel's name is left empty: the model labels it "Built-in display".
fn display_inputs(ds: &[Display]) -> Vec<DisplayInput> {
    ds.iter()
        .map(|d| DisplayInput {
            name: match d.kind {
                BrightnessKind::Internal => String::new(),
                BrightnessKind::Ddc => d.name.clone(),
            },
            level: d.level,
        })
        .collect()
}

fn core_mode(m: PowerMode) -> quick_model::PowerMode {
    match m {
        PowerMode::BestEfficiency => quick_model::PowerMode::BestEfficiency,
        PowerMode::Balanced => quick_model::PowerMode::Balanced,
        PowerMode::BestPerformance => quick_model::PowerMode::BestPerformance,
    }
}

fn win_mode(m: quick_model::PowerMode) -> PowerMode {
    match m {
        quick_model::PowerMode::BestEfficiency => PowerMode::BestEfficiency,
        quick_model::PowerMode::Balanced => PowerMode::Balanced,
        quick_model::PowerMode::BestPerformance => PowerMode::BestPerformance,
    }
}

fn power_inputs(s: &PowerSnapshot, lang: Lang) -> PowerInputs {
    PowerInputs {
        has_battery: s.has_battery,
        saver: s.saver,
        mode: s.mode.map(core_mode),
        battery_label: power_status::battery_label(s, lang),
    }
}

fn wifi_notice(e: WifiError) -> Notice {
    match e {
        WifiError::HandOff => Notice::WifiHandOff,
        WifiError::Denied => Notice::Denied,
        WifiError::BadPassword => Notice::BadPassword,
        WifiError::Failed => Notice::Failed,
    }
}

fn bt_notice(e: BtError) -> Notice {
    match e {
        BtError::HandOff => Notice::BtHandOff,
        BtError::Denied => Notice::Denied,
        BtError::Failed => Notice::Failed,
    }
}

/// The slider value shown: the dragged one while a drag is on.
fn shown_value(drag: Option<Drag>, slider: u32, actual: f32) -> f32 {
    match drag {
        Some(d) if d.slider == slider => d.value,
        _ => actual,
    }
}

/// Brightness wake-ups a just-set value is held against before the service value shows again.
const HOLD_ROUNDS: u8 = 2;
/// Levels this close count as the one that was set (DDC rounds to whole steps).
const HOLD_EPSILON: f32 = 0.01;

/// The held brightness value after a wake-up that read `actual`: released once the service
/// reports it, or after [`HOLD_ROUNDS`] wake-ups that don't (hardware keys win then).
fn settle(hold: Option<(Drag, u8)>, actual: Option<f32>) -> Option<(Drag, u8)> {
    let (d, rounds) = hold?;
    match actual {
        Some(a) if (a - d.value).abs() > HOLD_EPSILON && rounds > 1 => Some((d, rounds - 1)),
        _ => None,
    }
}

/// Whether the connect attempt to `target` is over: connected to it, the radio went off, or
/// the connection left the network it had at the start (`from`) and landed on another one
/// (`moved` remembers the leaving across calls). A failure arrives as its own change.
fn attempt_over(
    target: &str,
    from: Option<&str>,
    moved: &mut bool,
    now: Option<&str>,
    radio_off: bool,
) -> bool {
    if radio_off || now == Some(target) {
        return true;
    }
    if now != from {
        *moved = true;
    }
    *moved && now.is_some()
}

/// The service snapshots the rows are built from.
struct Snaps<'a> {
    wifi: Option<&'a WifiSnapshot>,
    bt: Option<&'a BtSnapshot>,
    audio: Option<&'a AudioSnapshot>,
    can_switch: bool,
    displays: &'a [Display],
    power: &'a PowerSnapshot,
    tiles: &'a [QuickTile],
    dark_mode: Option<bool>,
}

fn inputs_of(s: Snaps, ui: &QuickUi, drag: Option<Drag>, lang: Lang) -> QuickInputs {
    let mut audio = s.audio.and_then(|a| audio_inputs(a, s.can_switch));
    if let Some(a) = audio.as_mut() {
        a.volume = shown_value(drag, SLIDER_VOLUME, a.volume);
    }
    let mut displays = display_inputs(s.displays);
    for (k, d) in displays.iter_mut().enumerate() {
        d.level = shown_value(drag, quick_model::BRIGHTNESS_BASE + k as u32, d.level);
    }
    QuickInputs {
        wifi: s.wifi.map(wifi_inputs),
        bt: s.bt.map(bt_inputs),
        audio,
        displays,
        power: power_inputs(s.power, lang),
        // Ruling S3-3, shipped hidden: no set of controllable radios is detected yet.
        airplane: None,
        ui: ui.clone(),
        tiles: Some(s.tiles.to_vec()),
        dark_mode: s.dark_mode,
    }
}

fn wipe(mut s: String) {
    // SAFETY: zero bytes are valid UTF-8; the string is dropped right after.
    unsafe { s.as_bytes_mut().fill(0) };
}

fn ui_color(c: Rgba) -> Color {
    Color {
        A: c.3,
        R: c.0,
        G: c.1,
        B: c.2,
    }
}

impl QuickPanel {
    /// Opens the services and the panel above `anchor_px` (below it for a top bar).
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        g: &Gfx,
        style: &MaterialStyle,
        dpi: u32,
        anchor_px: PxRect,
        position: Position,
        accent: Rgba,
        lang: Lang,
        notify: HWND,
        tiles: Vec<QuickTile>,
    ) -> Result<Self> {
        let report = |what: &str, e: &windows::core::Error| {
            eprintln!("vitrobar: quick panel: no {what}: {e}");
        };
        let services = Services {
            audio: Audio::open(notify, WM_APP_QUICK)
                .inspect_err(|e| report("audio", e))
                .ok(),
            brightness: Brightness::open(notify, WM_APP_QUICK)
                .inspect_err(|e| report("brightness", e))
                .ok(),
            // No adapter is the usual failure: not worth a line.
            wifi: Wifi::open(notify, WM_APP_QUICK).ok(),
            bt: Bluetooth::open(notify, WM_APP_QUICK)
                .inspect_err(|e| report("Bluetooth", e))
                .ok(),
            _power: power_status::register(notify)
                .inspect_err(|e| report("power notifications", e))
                .ok(),
        };
        // The first frame already shows what the services report.
        let wifi = services.wifi.as_ref().map(Wifi::snapshot);
        let bt = services.bt.as_ref().map(Bluetooth::snapshot);
        let audio = services.audio.as_ref().map(Audio::snapshot);
        let can_switch = services.audio.as_ref().is_some_and(Audio::can_switch);
        let displays = services
            .brightness
            .as_ref()
            .map(Brightness::displays)
            .unwrap_or_default();
        let power = power_status::snapshot();
        let dark_mode = system_theme::dark_mode();
        let ui = QuickUi::default();
        let rows = quick_model::rows(
            QuickPage::Main,
            &inputs_of(
                Snaps {
                    wifi: wifi.as_ref(),
                    bt: bt.as_ref(),
                    audio: audio.as_ref(),
                    can_switch,
                    displays: &displays,
                    power: &power,
                    tiles: &tiles,
                    dark_mode,
                },
                &ui,
                None,
                lang,
            ),
            lang,
        );
        let view = PanelView::show(
            g.comp, g.painter, style, dpi, anchor_px, position, accent, &rows,
        )?;
        let mut q = Self {
            pw_slot: None,
            view,
            services,
            fg_at_open: 0,
            activated: false,
            notify,
            lang,
            text: style.text,
            accent,
            page: QuickPage::Main,
            wifi,
            bt,
            audio,
            can_switch,
            displays,
            power,
            tiles,
            dark_mode,
            ui,
            password: TextInput::new(),
            pw_first: 0,
            drag: None,
            hold: None,
            connect_from: None,
            connect_moved: false,
            hot: None,
            pressed: None,
            mute_pressed: false,
            focus: None,
            tracking: false,
            scan_timer: None,
            bt_timer: false,
            rows,
        };
        q.render(g)?;
        Ok(q)
    }

    pub fn hwnd(&self) -> HWND {
        self.view.hwnd()
    }

    pub fn rect_px(&self) -> PxRect {
        self.view.rect_px()
    }

    /// Takes the foreground; false if Windows refused.
    pub fn activate(&mut self) -> bool {
        self.activated |= self.view.activate();
        self.activated
    }

    fn read_wifi(&mut self) {
        self.wifi = self.services.wifi.as_ref().map(Wifi::snapshot);
        let Some(target) = self.ui.connecting.clone() else {
            return;
        };
        let now = self.wifi.as_ref().and_then(|w| w.connected.as_deref());
        let radio_off = self.wifi.as_ref().is_none_or(|w| w.radio_on == Some(false));
        if attempt_over(
            &target,
            self.connect_from.as_deref(),
            &mut self.connect_moved,
            now,
            radio_off,
        ) {
            self.ui.connecting = None;
        }
    }

    fn read_bt(&mut self) {
        self.bt = self.services.bt.as_ref().map(Bluetooth::snapshot);
    }

    fn read_audio(&mut self) {
        self.audio = self.services.audio.as_ref().map(Audio::snapshot);
        self.can_switch = self.services.audio.as_ref().is_some_and(Audio::can_switch);
    }

    fn read_brightness(&mut self) {
        self.displays = self
            .services
            .brightness
            .as_ref()
            .map(Brightness::displays)
            .unwrap_or_default();
        if let Some((d, _)) = self.hold {
            let actual = d
                .slider
                .checked_sub(quick_model::BRIGHTNESS_BASE)
                .and_then(|k| self.displays.get(k as usize))
                .map(|x| x.level);
            self.hold = settle(self.hold, actual);
        }
    }

    fn inputs(&self) -> QuickInputs {
        inputs_of(
            Snaps {
                wifi: self.wifi.as_ref(),
                bt: self.bt.as_ref(),
                audio: self.audio.as_ref(),
                can_switch: self.can_switch,
                displays: &self.displays,
                power: &self.power,
                tiles: &self.tiles,
                dark_mode: self.dark_mode,
            },
            &self.ui,
            self.drag.or(self.hold.map(|(d, _)| d)),
            self.lang,
        )
    }

    /// Builds the rows of the current page and shows them (only changed rows redraw).
    fn render(&mut self, g: &Gfx) -> Result<()> {
        let rows = quick_model::rows(self.page, &self.inputs(), self.lang);
        if rows != self.rows {
            self.view.update(g.comp, g.painter, &rows)?;
            self.rows = rows;
            // Parts that went away lose their hover / focus.
            let has = |p: Option<Part>| {
                p.filter(|p| self.view.layout().parts.iter().any(|(q, _)| q == p))
            };
            let (hot, focus, pressed) = (has(self.hot), has(self.focus), has(self.pressed));
            self.hot = hot;
            self.focus = focus;
            self.pressed = pressed;
            self.view.set_hot(hot)?;
            self.view.set_focus(focus)?;
            self.view.set_pressed(pressed)?;
        }
        self.draw_password(g)?;
        self.arm_timers();
        Ok(())
    }

    fn render_or_close(&mut self, g: &Gfx) -> Outcome {
        match self.render(g) {
            Ok(()) => Outcome::None,
            Err(e) => {
                eprintln!("vitrobar: the quick panel could not be drawn: {e}");
                Outcome::Close
            }
        }
    }

    /// The password box inside its Custom slot: plate, masked text (or the placeholder) and
    /// a non-blinking caret (no timers).
    fn draw_password(&mut self, g: &Gfx) -> Result<()> {
        let slot = self
            .ui
            .password_for
            .as_ref()
            .and_then(|_| self.view.custom_slot(CUSTOM_PASSWORD));
        if let Some(old) = self.pw_slot.take() {
            old.Children()?.RemoveAll()?;
        }
        let (Some(slot), Some(r)) = (slot, self.view.custom_rect_px(CUSTOM_PASSWORD)) else {
            return Ok(());
        };
        let comp: &windows::UI::Composition::Compositor = g.comp;
        let s = self.view.scale();
        let (w, h) = ((r.2 - r.0).max(1) as f32, (r.3 - r.1).max(1) as f32);
        let text = self.text;
        let kids = slot.Children()?;
        kids.RemoveAll()?;

        let geom = comp.CreateRoundedRectangleGeometry()?;
        geom.SetSize(Vector2::new(w, h))?;
        geom.SetCornerRadius(Vector2::new(6.0 * s, 6.0 * s))?;
        let shape = comp.CreateSpriteShapeWithGeometry(&geom)?;
        shape.SetFillBrush(
            &comp.CreateColorBrushWithColor(ui_color(Rgba(text.0, text.1, text.2, 18)))?,
        )?;
        shape.SetStrokeBrush(&comp.CreateColorBrushWithColor(ui_color(self.accent))?)?;
        shape.SetStrokeThickness((1.5 * s).max(1.0))?;
        let plate = comp.CreateShapeVisual()?;
        plate.SetSize(Vector2::new(w, h))?;
        plate.Shapes()?.Append(&shape)?;
        kids.InsertAtTop(&plate)?;

        let pad = (10.0 * s).round();
        let box_w = (w - 2.0 * pad).max(1.0);
        let size = PW_FONT_DIP * s;
        let n = self.password.len();
        let caret = self.password.caret().min(n);
        let masked = |from: usize, to: usize| -> String {
            std::iter::repeat_n(PW_MASK, to.saturating_sub(from)).collect()
        };
        let width = |from: usize| {
            g.painter
                .caret_x(&masked(from, n), FontKind::Text, size, caret - from)
                .unwrap_or(0.0)
        };
        let mut first = self.pw_first.min(caret);
        while first < caret && width(first) > box_w - 2.0 {
            first += 1;
        }
        while first > 0 && width(first - 1) <= box_w - 2.0 {
            first -= 1;
        }
        self.pw_first = first;
        let (shown, color) = if n == 0 {
            (
                tr(self.lang, Key::Password).to_string(),
                Rgba(text.0, text.1, text.2, 150),
            )
        } else {
            (masked(first, n), text)
        };
        let surface =
            g.painter
                .text_left(&shown, FontKind::Text, size, color, box_w as u32, h as u32)?;
        let sprite = comp.CreateSpriteVisual()?;
        sprite.SetBrush(&comp.CreateSurfaceBrushWithSurface(&surface)?)?;
        sprite.SetSize(Vector2::new(box_w.floor(), h))?;
        sprite.SetOffset(Vector3::new(pad, 0.0, 0.0))?;
        kids.InsertAtTop(&sprite)?;

        let x = if n == 0 { 0.0 } else { width(first) };
        let ch = (18.0 * s).round();
        let caret_v = comp.CreateSpriteVisual()?;
        caret_v.SetBrush(&comp.CreateColorBrushWithColor(ui_color(text))?)?;
        caret_v.SetSize(Vector2::new((1.5 * s).max(1.0), ch))?;
        caret_v.SetOffset(Vector3::new(
            (pad + x.round()).min(w - pad),
            ((h - ch) / 2.0).round(),
            0.0,
        ))?;
        kids.InsertAtTop(&caret_v)?;
        self.pw_slot = Some(slot);
        Ok(())
    }

    // ---- input -------------------------------------------------------------------------------

    /// Whether panel client `pt` (px) is on the volume slider's glyph, which toggles mute
    /// (the glyph column is not a panel part).
    fn on_mute_glyph(&self, pt: (i32, i32)) -> bool {
        let l = self.view.layout();
        let s = self.view.scale();
        let (x, y) = (pt.0 as f32 / s, pt.1 as f32 / s);
        if y < 0.0 || y >= l.size.1 {
            return false;
        }
        let y = y + self.view.scroll();
        l.parts.iter().any(|(p, r)| {
            *p == Part::Slider(SLIDER_VOLUME)
                && (PAD_DIP..PAD_DIP + SLIDER_GLYPH_W_DIP).contains(&x)
                && (r.y..r.bottom()).contains(&y)
        })
    }

    /// A mouse message on the panel window (client px in `lParam`).
    pub fn on_mouse(&mut self, msg: &MSG, g: &Gfx) -> Outcome {
        let pt = mouse_pos(msg.lParam);
        match msg.message {
            WM_MOUSEMOVE => {
                if !self.tracking {
                    track_leave(self.hwnd());
                    self.tracking = true;
                }
                if let Some(d) = self.drag {
                    if msg.wParam.0 & MK_LBUTTON == 0 {
                        // The button went up where we could not hear it (capture lost):
                        // the drag ends at the last value.
                        // SAFETY: releases our own capture, if we still have it.
                        unsafe {
                            let _ = ReleaseCapture();
                        }
                        self.pressed = None;
                        return self.set_slider(d.slider, d.value, false, g);
                    }
                    return match self.view.slider_value(d.slider, pt.0) {
                        Some(v) if v != d.value => self.set_slider(d.slider, v, true, g),
                        _ => Outcome::None,
                    };
                }
                let hot = self.view.hit_px(pt);
                self.hot = hot;
                ok(self.view.set_hot(hot))
            }
            WM_MOUSELEAVE => {
                self.tracking = false;
                self.hot = None;
                ok(self.view.set_hot(None))
            }
            WM_LBUTTONDOWN => {
                if !self.activated {
                    // A click on our own window may take the foreground (carry 12).
                    self.activate();
                }
                let part = self.view.hit_px(pt);
                self.mute_pressed = part.is_none() && self.on_mute_glyph(pt);
                self.pressed = part;
                if let Some(Part::Slider(id)) = part {
                    // SAFETY: our own window keeps the mouse while the button is down.
                    unsafe { SetCapture(self.hwnd()) };
                    let v = self
                        .view
                        .slider_value(id, pt.0)
                        .unwrap_or_else(|| self.slider_now(id));
                    return self.set_slider(id, v, true, g);
                }
                ok(self.view.set_pressed(part))
            }
            WM_LBUTTONUP => {
                if let Some(d) = self.drag {
                    // SAFETY: releases our own capture.
                    unsafe {
                        let _ = ReleaseCapture();
                    }
                    self.pressed = None;
                    let v = self.view.slider_value(d.slider, pt.0).unwrap_or(d.value);
                    // The final value is applied; the shown value follows the service again.
                    return self.set_slider(d.slider, v, false, g);
                }
                let pressed = self.pressed.take();
                let mute = std::mem::take(&mut self.mute_pressed);
                if let Outcome::Close = ok(self.view.set_pressed(None)) {
                    return Outcome::Close;
                }
                let part = self.view.hit_px(pt);
                if mute && part.is_none() && self.on_mute_glyph(pt) {
                    return self.toggle_mute(g);
                }
                match pressed {
                    Some(p) if Some(p) == part => self.activate_part(p, g),
                    _ => Outcome::None,
                }
            }
            _ => Outcome::None,
        }
    }

    /// A key on the panel (`WM_KEYDOWN`; characters follow as [`QuickPanel::on_char`]).
    pub fn on_key(&mut self, vk: u16, ctrl: bool, shift: bool, g: &Gfx) -> Outcome {
        let pw = self.ui.password_for.is_some();
        let k = windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY(vk);
        match k {
            VK_ESCAPE if pw => {
                self.close_password();
                self.render_or_close(g)
            }
            VK_ESCAPE if self.page != QuickPage::Main => self.go(QuickPage::Main, true, g),
            VK_ESCAPE => Outcome::Close,
            VK_TAB => self.move_focus(if shift { NavKey::Up } else { NavKey::Down }),
            VK_UP => self.move_focus(NavKey::Up),
            VK_DOWN => self.move_focus(NavKey::Down),
            VK_HOME | VK_END if pw => {
                if k == VK_HOME {
                    self.password.home();
                } else {
                    self.password.end();
                }
                self.render_or_close(g)
            }
            VK_HOME => self.move_focus(NavKey::Home),
            VK_END => self.move_focus(NavKey::End),
            VK_LEFT | VK_RIGHT => {
                let left = k == VK_LEFT;
                if pw {
                    match (left, ctrl) {
                        (true, false) => self.password.left(),
                        (true, true) => self.password.home(),
                        (false, false) => self.password.right(),
                        (false, true) => self.password.end(),
                    }
                    self.render_or_close(g)
                } else if let Some(Part::Slider(id)) = self.focus {
                    let v = nudge(self.slider_now(id), if left { -1.0 } else { 1.0 });
                    self.set_slider(id, v, false, g)
                } else {
                    self.move_focus(if left { NavKey::Up } else { NavKey::Down })
                }
            }
            VK_BACK if pw => {
                if ctrl {
                    self.password.backspace_word();
                } else {
                    self.password.backspace();
                }
                self.render_or_close(g)
            }
            VK_DELETE if pw => {
                self.password.delete();
                self.render_or_close(g)
            }
            VK_RETURN if pw && self.focus != Some(Part::Button(quick_model::BTN_CANCEL)) => {
                self.connect_password(g)
            }
            // While the password row is open, Space is a character.
            VK_RETURN | VK_SPACE if !(pw && k == VK_SPACE) => match self.focus {
                Some(p) => self.activate_part(p, g),
                None => Outcome::None,
            },
            _ => Outcome::None,
        }
    }

    /// A typed character (`WM_CHAR`): only the open password box takes text.
    pub fn on_char(&mut self, c: char, g: &Gfx) -> Outcome {
        if self.ui.password_for.is_none() || c.is_control() {
            return Outcome::None;
        }
        let mut buf = [0u8; 4];
        self.password.insert(c.encode_utf8(&mut buf));
        buf.fill(0);
        self.render_or_close(g)
    }

    /// Ctrl+V into the open password box. The text is wiped afterwards.
    pub fn paste(&mut self, text: String, g: &Gfx) -> Outcome {
        if self.ui.password_for.is_none() {
            wipe(text);
            return Outcome::None;
        }
        // A password copied with its line break still joins.
        self.password.insert(text.trim_end_matches(['\r', '\n']));
        wipe(text);
        self.render_or_close(g)
    }

    /// `WM_MOUSEWHEEL` (`screen_pt` in screen px): over a slider ±2 % per notch, elsewhere the
    /// list scrolls.
    pub fn on_wheel(&mut self, delta: i16, screen_pt: (i32, i32), g: &Gfx) -> Outcome {
        if self.drag.is_some() {
            return Outcome::None;
        }
        let r = self.rect_px();
        let pt = (screen_pt.0 - r.0, screen_pt.1 - r.1);
        let notches = f32::from(delta) / WHEEL_DELTA;
        match self.view.hit_px(pt) {
            Some(Part::Slider(id)) => {
                let v = nudge(self.slider_now(id), notches);
                self.set_slider(id, v, false, g)
            }
            _ => ok(self.view.scroll_by(-notches * WHEEL_SCROLL_DIP)),
        }
    }

    fn move_focus(&mut self, key: NavKey) -> Outcome {
        let f = focus_step(self.view.layout(), self.focus, key);
        self.focus = f;
        if let Some(p) = f {
            if let Outcome::Close = ok(self.scroll_into_view(p)) {
                return Outcome::Close;
            }
        }
        ok(self.view.set_focus(f))
    }

    fn scroll_into_view(&mut self, p: Part) -> Result<()> {
        let l = self.view.layout();
        let Some((_, r)) = l.parts.iter().find(|(q, _)| *q == p) else {
            return Ok(());
        };
        let (top, h) = (self.view.scroll(), l.size.1);
        let dy = if r.y - PAD_DIP < top {
            r.y - PAD_DIP - top
        } else if r.bottom() + PAD_DIP > top + h {
            r.bottom() + PAD_DIP - top - h
        } else {
            0.0
        };
        self.view.scroll_by(dy)
    }

    // ---- actions -----------------------------------------------------------------------------

    fn activate_part(&mut self, p: Part, g: &Gfx) -> Outcome {
        match action(self.page, p) {
            Some(a) => self.run(a, g),
            None => Outcome::None,
        }
    }

    fn run(&mut self, a: Action, g: &Gfx) -> Outcome {
        match a {
            Action::ToggleWifi => self.toggle_wifi(),
            Action::ToggleBt => self.toggle_bt(),
            Action::ToggleRadio => match self.page {
                QuickPage::Wifi => self.toggle_wifi(),
                QuickPage::Bluetooth => self.toggle_bt(),
                _ => Outcome::None,
            },
            // Never shown (airplane = None, ruling S3-3).
            Action::ToggleAirplane => Outcome::None,
            Action::BatterySaver => {
                // Windows has no public switch for saver on mains power: its page opens.
                if self.power.on_ac {
                    return Outcome::Open("ms-settings:batterysaver".into());
                }
                if !power_status::set_saver(!self.power.saver) {
                    eprintln!("vitrobar: Windows did not change battery saver");
                    return Outcome::Open("ms-settings:batterysaver".into());
                }
                // The status change also arrives as a power notification; show it now.
                self.power = power_status::snapshot();
                self.render_or_close(g)
            }
            Action::ToggleMute => self.toggle_mute(g),
            Action::ToggleDarkMode => {
                if let Some(on) = self.dark_mode {
                    if system_theme::set_dark_mode(!on) {
                        self.dark_mode = Some(!on);
                    }
                }
                self.render_or_close(g)
            }
            Action::Lock => Outcome::Lock,
            Action::ToggleTile(t) => {
                self.tiles = toggle_tile(&self.tiles, t);
                match self.render_or_close(g) {
                    Outcome::None => Outcome::SaveTiles(self.tiles.clone()),
                    o => o,
                }
            }
            Action::ResetTiles => {
                self.tiles = QuickTile::DEFAULT.to_vec();
                match self.render_or_close(g) {
                    Outcome::None => Outcome::SaveTiles(self.tiles.clone()),
                    o => o,
                }
            }
            Action::CyclePowerMode => {
                if let Some(m) = self.power.mode {
                    if !power_status::set_mode(win_mode(next_mode(core_mode(m)))) {
                        eprintln!("vitrobar: Windows did not change the power mode");
                    }
                }
                self.power = power_status::snapshot();
                self.render_or_close(g)
            }
            Action::Open(p) => self.go(p, false, g),
            Action::Back => self.go(QuickPage::Main, false, g),
            // Enter on the focused volume slider.
            Action::Volume => self.toggle_mute(g),
            Action::Brightness(_) => Outcome::None,
            Action::Network(i) => self.pick_network(i, g),
            Action::Device(i) => self.pick_device(i, g),
            Action::Output(i) => self.pick_output(i, g),
            Action::Connect => self.connect_password(g),
            Action::CancelPassword => {
                self.close_password();
                self.render_or_close(g)
            }
            Action::Settings(uri) => Outcome::Open(uri.to_string()),
        }
    }

    /// The result arrives as a wake-up (`Radio` or `Error`).
    fn toggle_wifi(&mut self) -> Outcome {
        let on = self.wifi.as_ref().and_then(|w| w.radio_on);
        if let (Some(w), Some(on)) = (&self.services.wifi, on) {
            w.set_radio(!on);
        }
        Outcome::None
    }

    fn toggle_bt(&mut self) -> Outcome {
        let on = self.bt.as_ref().and_then(|b| b.radio_on);
        if let (Some(b), Some(on)) = (&self.services.bt, on) {
            b.set_radio(!on);
        }
        Outcome::None
    }

    fn toggle_mute(&mut self, g: &Gfx) -> Outcome {
        let (Some(a), Some(s)) = (&self.services.audio, self.audio.as_mut()) else {
            return Outcome::None;
        };
        s.muted = !s.muted;
        a.set_muted(s.muted);
        self.render_or_close(g)
    }

    /// The service value of slider `id` (not the dragged one).
    fn slider_now(&self, id: u32) -> f32 {
        if id == SLIDER_VOLUME {
            return self.audio.as_ref().map_or(0.0, |a| a.volume);
        }
        id.checked_sub(quick_model::BRIGHTNESS_BASE)
            .and_then(|k| self.displays.get(k as usize))
            .map_or(0.0, |d| d.level)
    }

    /// Applies a slider value at once (audio) or through the brightness worker, which keeps
    /// one hardware write in flight and coalesces the rest. `dragging` keeps the shown value
    /// on the pointer until the button goes up.
    fn set_slider(&mut self, id: u32, v: f32, dragging: bool, g: &Gfx) -> Outcome {
        let v = audio::clamp_volume(v);
        if id == SLIDER_VOLUME {
            if let (Some(a), Some(s)) = (&self.services.audio, self.audio.as_mut()) {
                a.set_volume(v);
                s.volume = v;
            }
        } else if let Some(k) = id.checked_sub(quick_model::BRIGHTNESS_BASE) {
            if let (Some(b), Some(d)) =
                (&self.services.brightness, self.displays.get_mut(k as usize))
            {
                b.set_level(d.monitor, v);
                d.level = v;
            }
        }
        let d = Drag {
            slider: id,
            value: v,
        };
        self.drag = dragging.then_some(d);
        // A brightness level set for good is shown until the worker reports it (a WMI
        // event read mid-write would otherwise make the thumb jump back for a moment).
        self.hold = (!dragging && id != SLIDER_VOLUME).then_some((d, HOLD_ROUNDS));
        self.render_or_close(g)
    }

    fn close_password(&mut self) {
        self.password.wipe();
        self.pw_first = 0;
        self.ui.password_for = None;
    }

    fn go(&mut self, page: QuickPage, keyboard: bool, g: &Gfx) -> Outcome {
        self.close_password();
        self.ui.notice = None;
        self.page = page;
        self.hot = None;
        self.focus = None;
        if let Outcome::Close = ok(self.view.scroll_by(-self.view.scroll())) {
            return Outcome::Close;
        }
        match page {
            QuickPage::Wifi => {
                if let Some(w) = &self.services.wifi {
                    // The list is read from Windows' cache at once; the scan refreshes it.
                    w.scan();
                }
                self.read_wifi();
            }
            QuickPage::Bluetooth => self.read_bt(),
            QuickPage::Output => self.read_audio(),
            QuickPage::Main | QuickPage::Edit => {}
        }
        if let Outcome::Close = self.render_or_close(g) {
            return Outcome::Close;
        }
        if keyboard {
            return self.move_focus(NavKey::Down);
        }
        Outcome::None
    }

    fn pick_network(&mut self, i: usize, g: &Gfx) -> Outcome {
        let Some(snap) = &self.wifi else {
            return Outcome::None;
        };
        let Some(n) = snap.networks.get(i).cloned() else {
            return Outcome::None;
        };
        if snap.connected.as_deref() == Some(n.ssid.as_str()) {
            return Outcome::None;
        }
        self.ui.notice = None;
        match n.kind {
            NetKind::Enterprise if !n.known => Outcome::Open("ms-settings:network-wifi".into()),
            NetKind::Psk if !n.known => {
                let same = self.ui.password_for.as_deref() == Some(n.ssid.as_str());
                self.close_password();
                if !same {
                    self.ui.password_for = Some(n.ssid);
                }
                self.render_or_close(g)
            }
            _ => self.start_connect(n.ssid, None, g),
        }
    }

    fn start_connect(&mut self, ssid: String, password: Option<String>, g: &Gfx) -> Outcome {
        let r = match &self.services.wifi {
            Some(w) => w.connect(&ssid, password.as_deref()),
            None => Err(WifiError::Failed),
        };
        if let Some(p) = password {
            wipe(p);
        }
        // Never kept past the connect call, whatever the outcome.
        self.password.wipe();
        self.pw_first = 0;
        match r {
            Ok(()) => {
                self.ui.password_for = None;
                self.ui.connecting = Some(ssid);
                self.connect_from = self.wifi.as_ref().and_then(|w| w.connected.clone());
                self.connect_moved = false;
            }
            Err(WifiError::HandOff) => {
                self.close_password();
                return Outcome::Open("ms-settings:network-wifi".into());
            }
            Err(e) => self.ui.notice = Some(wifi_notice(e)),
        }
        self.read_wifi();
        self.render_or_close(g)
    }

    fn connect_password(&mut self, g: &Gfx) -> Outcome {
        let Some(ssid) = self.ui.password_for.clone() else {
            return Outcome::None;
        };
        let pw = self.password.text();
        self.start_connect(ssid, Some(pw), g)
    }

    fn pick_device(&mut self, i: usize, g: &Gfx) -> Outcome {
        let Some(d) = self.bt.as_ref().and_then(|b| b.devices.get(i)).cloned() else {
            return Outcome::None;
        };
        if d.pending {
            return Outcome::None;
        }
        let Some(b) = &self.services.bt else {
            return Outcome::None;
        };
        match b.set_connected(&d.id, !d.connected) {
            Ok(()) => self.ui.notice = None,
            Err(BtError::HandOff) => return Outcome::Open("ms-settings:bluetooth".into()),
            Err(e) => self.ui.notice = Some(bt_notice(e)),
        }
        self.read_bt();
        self.render_or_close(g)
    }

    fn pick_output(&mut self, i: usize, g: &Gfx) -> Outcome {
        let Some(s) = &self.audio else {
            return Outcome::None;
        };
        let Some(d) = s.devices.get(i) else {
            return Outcome::None;
        };
        if s.default_id.as_deref() == Some(d.id.as_str()) {
            return Outcome::None;
        }
        let done = self
            .services
            .audio
            .as_ref()
            .is_some_and(|a| a.set_default(&d.id));
        self.ui.notice = (!done).then_some(Notice::Failed);
        self.read_audio();
        self.render_or_close(g)
    }

    // ---- service events ----------------------------------------------------------------------

    /// [`WM_APP_QUICK`] arrived: drains every service (a pending wake-up blocks the next one)
    /// and shows what changed.
    pub fn on_wake(&mut self, g: &Gfx) -> Outcome {
        let s = &self.services;
        let audio = s
            .audio
            .as_ref()
            .map(Audio::drain_changes)
            .unwrap_or_default();
        let bright = s
            .brightness
            .as_ref()
            .map(Brightness::drain_changes)
            .unwrap_or_default();
        let wifi = s.wifi.as_ref().map(Wifi::drain_changes).unwrap_or_default();
        let bt =
            s.bt.as_ref()
                .map(Bluetooth::drain_changes)
                .unwrap_or_default();
        if audio.is_empty() && bright.is_empty() && wifi.is_empty() && bt.is_empty() {
            // Forged or already handled.
            return Outcome::None;
        }
        if !audio.is_empty() {
            self.read_audio();
        }
        if !bright.is_empty() {
            self.read_brightness();
        }
        for c in &wifi {
            match c {
                wifi::Change::ConnectFailed(ssid) => {
                    if ssid.is_empty() || self.ui.connecting.as_deref() == Some(ssid.as_str()) {
                        self.ui.connecting = None;
                    }
                    self.page_notice(QuickPage::Wifi, Notice::ConnectFailed);
                }
                wifi::Change::Error(e) => {
                    if *e == WifiError::HandOff {
                        self.ui.connecting = None;
                    }
                    self.page_notice(QuickPage::Wifi, wifi_notice(*e));
                }
                wifi::Change::Networks | wifi::Change::Radio => {}
            }
        }
        if !wifi.is_empty() {
            self.read_wifi();
        }
        for c in &bt {
            if let bluetooth::Change::Error(e) = c {
                self.page_notice(QuickPage::Bluetooth, bt_notice(*e));
            }
        }
        if !bt.is_empty() {
            self.read_bt();
        }
        self.render_or_close(g)
    }

    /// A notice is only shown on the page it belongs to.
    fn page_notice(&mut self, page: QuickPage, n: Notice) {
        if self.page == page {
            self.ui.notice = Some(n);
        }
    }

    /// `WM_POWERBROADCAST` (battery, power source or saver changed).
    pub fn on_power(&mut self, g: &Gfx) -> Outcome {
        self.power = power_status::snapshot();
        self.render_or_close(g)
    }

    /// One of the panel's one-shot timers fired; false if `id` is not one of them.
    pub fn on_timer(&mut self, id: usize, g: &Gfx) -> Option<Outcome> {
        match id {
            TIMER_QUICK_SCAN => {
                self.kill(TIMER_QUICK_SCAN);
                self.scan_timer = None;
                self.read_wifi();
            }
            TIMER_QUICK_BT => {
                self.kill(TIMER_QUICK_BT);
                self.bt_timer = false;
                self.read_bt();
            }
            _ => return None,
        }
        Some(self.render_or_close(g))
    }

    /// One one-shot timer for a pending Wi-Fi scan's deadline and one while a Bluetooth
    /// connect is pending; never a repeating timer.
    fn arm_timers(&mut self) {
        let deadline = self.services.wifi.as_ref().and_then(Wifi::scan_deadline);
        if deadline != self.scan_timer {
            match deadline {
                Some(d) => {
                    let ms = d.saturating_duration_since(Instant::now()).as_millis() as u64
                        + TIMER_SLACK_MS;
                    self.set_timer(TIMER_QUICK_SCAN, ms);
                }
                None => self.kill(TIMER_QUICK_SCAN),
            }
            self.scan_timer = deadline;
        }
        let pending = self
            .bt
            .as_ref()
            .is_some_and(|b| b.devices.iter().any(|d| d.pending));
        if pending != self.bt_timer {
            if pending {
                let ms = (PENDING_MAX + Duration::from_millis(TIMER_SLACK_MS)).as_millis();
                self.set_timer(TIMER_QUICK_BT, ms as u64);
            } else {
                self.kill(TIMER_QUICK_BT);
            }
            self.bt_timer = pending;
        }
    }

    fn set_timer(&self, id: usize, ms: u64) {
        // SAFETY: one-shot timer on the app's notify window (killed when it fires).
        unsafe {
            SetTimer(
                Some(self.notify),
                id,
                ms.min(u64::from(u32::MAX)) as u32,
                None,
            )
        };
    }

    fn kill(&self, id: usize) {
        // SAFETY: timer on the app's notify window.
        unsafe {
            let _ = KillTimer(Some(self.notify), id);
        }
    }

    /// The bar was laid out anew (the pill grew or shrank): the panel follows its button.
    pub fn reanchor(&mut self, comp: &Comp, anchor_px: PxRect) -> Result<()> {
        self.view.reanchor(comp, anchor_px)
    }

    /// Theme, material or accent changed while open.
    pub fn restyle(&mut self, g: &Gfx, style: &MaterialStyle, accent: Rgba) -> Result<()> {
        self.text = style.text;
        self.accent = accent;
        self.pw_slot = None;
        self.view
            .restyle(g.comp, g.painter, style, accent, &self.rows)?;
        self.draw_password(g)
    }
}

impl Drop for QuickPanel {
    fn drop(&mut self) {
        self.kill(TIMER_QUICK_SCAN);
        self.kill(TIMER_QUICK_BT);
        if self.drag.is_some() {
            // SAFETY: releases our own capture.
            unsafe {
                let _ = ReleaseCapture();
            }
        }
        self.password.wipe();
    }
}

fn ok(r: Result<()>) -> Outcome {
    match r {
        Ok(()) => Outcome::None,
        Err(e) => {
            eprintln!("vitrobar: the quick panel could not be drawn: {e}");
            Outcome::Close
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vitro_win::audio::OutputDevice;
    use vitro_win::bluetooth::BtDevice;
    use vitro_win::wifi::Network;

    #[test]
    fn the_built_in_display_has_no_name_and_ddc_keeps_its_own() {
        let ds = vec![
            Display {
                monitor: 0,
                name: "ignored".into(),
                level: 0.4,
                kind: BrightnessKind::Internal,
            },
            Display {
                monitor: 77,
                name: "DELL U2720Q".into(),
                level: 0.9,
                kind: BrightnessKind::Ddc,
            },
        ];
        let d = display_inputs(&ds);
        assert_eq!(d[0].name, "");
        assert_eq!(d[1].name, "DELL U2720Q");
        assert_eq!(d[1].level, 0.9);
    }

    #[test]
    fn audio_without_a_default_device_is_hidden() {
        let s = AudioSnapshot {
            volume: 0.0,
            muted: false,
            devices: Vec::new(),
            default_id: None,
        };
        assert_eq!(audio_inputs(&s, true), None);
        let s = AudioSnapshot {
            volume: 0.3,
            muted: true,
            devices: vec![
                OutputDevice {
                    id: "a".into(),
                    name: "Speakers".into(),
                },
                OutputDevice {
                    id: "b".into(),
                    name: "Headset".into(),
                },
            ],
            default_id: Some("b".into()),
        };
        let a = audio_inputs(&s, false).unwrap();
        assert_eq!(a.default, Some(1));
        assert_eq!(a.devices, vec!["Speakers", "Headset"]);
        assert!(a.muted && !a.can_switch);
    }

    #[test]
    fn wifi_and_bluetooth_snapshots_convert() {
        let w = WifiSnapshot {
            radio_on: Some(true),
            connected: Some("Casa".into()),
            networks: vec![Network {
                ssid: "Casa".into(),
                signal: 60,
                secured: true,
                kind: NetKind::Psk,
                known: true,
            }],
            scanning: true,
        };
        let i = wifi_inputs(&w);
        assert_eq!(i.networks[0].bars, 3);
        assert!(i.scanning && i.networks[0].secured);
        let b = BtSnapshot {
            radio_on: Some(false),
            devices: vec![BtDevice {
                id: "x".into(),
                name: "Cuffie".into(),
                connected: true,
                audio: true,
                battery: Some(50),
                pending: true,
            }],
        };
        let i = bt_inputs(&b);
        assert_eq!(i.radio_on, Some(false));
        assert!(i.devices[0].pending && i.devices[0].audio);
    }

    #[test]
    fn a_drag_holds_its_value_against_echoes() {
        let d = Some(Drag {
            slider: SLIDER_VOLUME,
            value: 0.8,
        });
        // The service echoes an older value while the pointer moves on: the pointer wins.
        assert_eq!(shown_value(d, SLIDER_VOLUME, 0.6), 0.8);
        // Other sliders and no drag show the service value.
        assert_eq!(shown_value(d, quick_model::BRIGHTNESS_BASE, 0.6), 0.6);
        assert_eq!(shown_value(None, SLIDER_VOLUME, 0.6), 0.6);
    }

    #[test]
    fn a_set_brightness_is_held_until_reported() {
        let d = Drag {
            slider: quick_model::BRIGHTNESS_BASE,
            value: 0.8,
        };
        let h = Some((d, HOLD_ROUNDS));
        // A stale level mid-write: still held, one round fewer.
        assert_eq!(settle(h, Some(0.3)), Some((d, HOLD_ROUNDS - 1)));
        // The worker reports the value: released.
        assert_eq!(settle(h, Some(0.805)), None);
        // Out of rounds (hardware keys moved it): the service wins.
        assert_eq!(settle(Some((d, 1)), Some(0.3)), None);
        // The display went away.
        assert_eq!(settle(h, None), None);
        assert_eq!(settle(None, Some(0.3)), None);
    }

    #[test]
    fn a_connect_attempt_ends_on_the_target_elsewhere_or_radio_off() {
        let mut moved = false;
        // Still on the old network: pending.
        assert!(!attempt_over("B", Some("A"), &mut moved, Some("A"), false));
        // Dropped the old one on the way: pending.
        assert!(!attempt_over("B", Some("A"), &mut moved, None, false));
        // Landed on another network (Windows fell back): over.
        assert!(attempt_over("B", Some("A"), &mut moved, Some("C"), false));
        // Back on the old one after leaving it: over too.
        let mut moved = false;
        assert!(!attempt_over("B", Some("A"), &mut moved, None, false));
        assert!(attempt_over("B", Some("A"), &mut moved, Some("A"), false));
        // Connected to the target, or the radio went off.
        let mut moved = false;
        assert!(attempt_over("B", None, &mut moved, Some("B"), false));
        assert!(attempt_over("B", None, &mut moved, None, true));
        // From nothing straight to another network.
        let mut moved = false;
        assert!(attempt_over("B", None, &mut moved, Some("C"), false));
    }

    #[test]
    fn power_modes_round_trip() {
        for m in [
            PowerMode::BestEfficiency,
            PowerMode::Balanced,
            PowerMode::BestPerformance,
        ] {
            assert_eq!(win_mode(core_mode(m)), m);
        }
        let p = power_inputs(
            &PowerSnapshot {
                has_battery: true,
                percent: Some(42),
                charging: false,
                saver: true,
                on_ac: false,
                mode: None,
            },
            Lang::En,
        );
        assert_eq!(p.battery_label.as_deref(), Some("42%"));
        assert!(p.saver && p.mode.is_none());
    }
}
