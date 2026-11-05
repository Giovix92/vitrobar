//! The quick panel's content (Wi-Fi, Bluetooth, airplane, battery, power mode, brightness,
//! volume) as [`panel_kit`](crate::panel_kit) rows, from plain snapshots of the system
//! services. Pure: the app converts the `vitro-win` snapshots into these structs (ruling
//! S3-2), builds the rows here and maps a clicked [`Part`] back with [`action`].

use crate::i18n::{tr, Key, Lang};
use crate::panel_kit::{Button, Part, Row, Tile};
use crate::settings::QuickTile;
use crate::style::Rgba;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum QuickPage {
    #[default]
    Main,
    Wifi,
    Bluetooth,
    Output,
    /// Which tiles the main page shows.
    Edit,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct WifiNet {
    pub ssid: String,
    /// 1..=4.
    pub bars: u8,
    pub secured: bool,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct WifiInputs {
    /// `None`: no radio object to ask (the switch is hidden).
    pub radio_on: Option<bool>,
    pub connected: Option<String>,
    /// In the order shown.
    pub networks: Vec<WifiNet>,
    pub scanning: bool,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct BtDev {
    pub name: String,
    pub connected: bool,
    pub audio: bool,
    pub battery: Option<u8>,
    pub pending: bool,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct BtInputs {
    /// `None`: no Bluetooth radio (the tile hides).
    pub radio_on: Option<bool>,
    pub devices: Vec<BtDev>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct AudioInputs {
    /// 0..=1.
    pub volume: f32,
    pub muted: bool,
    /// Output device names, in the order shown.
    pub devices: Vec<String>,
    /// Index of the default device in `devices`.
    pub default: Option<usize>,
    /// The default device can be changed (the Output page is offered).
    pub can_switch: bool,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct DisplayInput {
    /// Empty for the built-in panel.
    pub name: String,
    /// 0..=1.
    pub level: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerMode {
    BestEfficiency,
    Balanced,
    BestPerformance,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct PowerInputs {
    pub has_battery: bool,
    pub saver: bool,
    /// `None`: the power-mode tile hides.
    pub mode: Option<PowerMode>,
    /// "80%" / "80% · charging"; `None` hides the battery button.
    pub battery_label: Option<String>,
}

/// A message the panel shows after something failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notice {
    /// The network needs Windows' own Wi-Fi settings.
    WifiHandOff,
    /// The device can only be connected from Windows' Bluetooth settings.
    BtHandOff,
    /// Windows refused the radio change.
    Denied,
    /// The passphrase is not 8..=63 characters.
    BadPassword,
    /// The connection attempt failed.
    ConnectFailed,
    Failed,
}

/// What the panel itself is doing (not read from a service).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct QuickUi {
    /// The secured network whose password row is open.
    pub password_for: Option<String>,
    /// The network a connect was started for.
    pub connecting: Option<String>,
    pub notice: Option<Notice>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct QuickInputs {
    /// `None`: no Wi-Fi adapter (the tile hides).
    pub wifi: Option<WifiInputs>,
    pub bt: Option<BtInputs>,
    /// `None`: no playback device (the volume slider hides).
    pub audio: Option<AudioInputs>,
    pub displays: Vec<DisplayInput>,
    pub power: PowerInputs,
    /// `None`: airplane mode cannot be controlled (the tile hides).
    pub airplane: Option<bool>,
    pub ui: QuickUi,
    /// The tiles the user picked, in order; `None`: [`QuickTile::DEFAULT`].
    pub tiles: Option<Vec<QuickTile>>,
    /// Windows' dark mode; `None` hides its tile (unreadable).
    pub dark_mode: Option<bool>,
}

// ---- ids --------------------------------------------------------------------------------------

pub const TILE_WIFI: u32 = 1;
pub const TILE_BT: u32 = 2;
pub const TILE_AIRPLANE: u32 = 3;
pub const TILE_SAVER: u32 = 4;
pub const TILE_POWER: u32 = 5;
pub const TILE_MUTE: u32 = 6;
pub const TILE_DARK: u32 = 7;
pub const TILE_SNIP: u32 = 8;
pub const TILE_PROJECT: u32 = 9;
pub const TILE_LOCK: u32 = 10;
pub const SLIDER_VOLUME: u32 = 20;
/// The volume slider's trailing chevron (the Output page).
pub const OUTPUT_CHEVRON: u32 = 21;
pub const BTN_BATTERY: u32 = 30;
pub const BTN_ALL_SETTINGS: u32 = 31;
pub const BTN_MORE: u32 = 32;
pub const BTN_BACK: u32 = 33;
pub const BTN_CONNECT: u32 = 34;
pub const BTN_CANCEL: u32 = 35;
/// The main page's pencil: opens the tile editor.
pub const BTN_EDIT: u32 = 36;
pub const BTN_DONE: u32 = 37;
pub const BTN_RESET: u32 = 38;
/// A sub-page's radio switch row.
pub const ITEM_RADIO: u32 = 40;
/// The Wi-Fi password box (a Custom row the app draws into).
pub const CUSTOM_PASSWORD: u32 = 50;
pub const PASSWORD_H_DIP: f32 = 36.0;
/// Brightness slider of display `i` is `BRIGHTNESS_BASE + i`.
pub const BRIGHTNESS_BASE: u32 = 100;
/// List item `i` of a sub-page (network, device, output) is `ITEM_BASE + i`.
pub const ITEM_BASE: u32 = 1000;

/// One keyboard or wheel step of a slider.
pub const SLIDER_STEP: f32 = 0.02;

// ---- glyphs (Segoe Fluent Icons) --------------------------------------------------------------

const G_WIFI: char = '\u{E701}';
const G_WIFI_BARS: [char; 4] = ['\u{E872}', '\u{E873}', '\u{E874}', '\u{E701}'];
const G_BT: char = '\u{E702}';
const G_SAVER: char = '\u{EBBF}';
const G_POWER: char = '\u{E945}';
const G_BRIGHTNESS: char = '\u{E706}';
const G_MUTED: char = '\u{E74F}';
const G_VOLUME: [char; 4] = ['\u{E992}', '\u{E993}', '\u{E994}', '\u{E995}'];
const G_CHEVRON: char = '\u{E76C}';
const G_SETTINGS: char = '\u{E713}';
const G_BATTERY: char = '\u{EBA7}';
const G_BACK: char = '\u{E72B}';
const G_HEADPHONE: char = '\u{E7F6}';
const G_SPEAKER: char = '\u{E7F5}';
const G_MOON: char = '\u{E708}';
const G_SNIP: char = '\u{E8C6}';
const G_PROJECT: char = '\u{E7F4}';
const G_LOCK: char = '\u{E72E}';
const G_EDIT: char = '\u{E70F}';
const G_DONE: char = '\u{E73E}';
const G_RESET: char = '\u{E72C}';

/// The power tile's colour per mode: green for efficiency, blue for balanced, orange for
/// performance. White text reads on all three in both themes.
pub fn mode_tint(m: PowerMode) -> Rgba {
    match m {
        PowerMode::BestEfficiency => Rgba(0x1E, 0x8E, 0x4E, 255),
        PowerMode::Balanced => Rgba(0x2D, 0x6F, 0xD6, 255),
        PowerMode::BestPerformance => Rgba(0xD6, 0x5A, 0x16, 255),
    }
}

/// What a click (or Enter) on a part means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    ToggleWifi,
    ToggleBt,
    ToggleAirplane,
    /// Battery saver on or off (on mains power Windows has no public switch: the app opens
    /// its battery settings instead).
    BatterySaver,
    CyclePowerMode,
    ToggleMute,
    ToggleDarkMode,
    Lock,
    /// Shows or hides a tile (the editor).
    ToggleTile(QuickTile),
    ResetTiles,
    Open(QuickPage),
    Back,
    /// The Wi-Fi or Bluetooth radio switch on that page.
    ToggleRadio,
    Volume,
    Brightness(usize),
    Network(usize),
    Device(usize),
    Output(usize),
    Connect,
    CancelPassword,
    /// Opens a Windows Settings page (and closes the panel).
    Settings(&'static str),
}

pub fn next_mode(m: PowerMode) -> PowerMode {
    match m {
        PowerMode::BestEfficiency => PowerMode::Balanced,
        PowerMode::Balanced => PowerMode::BestPerformance,
        PowerMode::BestPerformance => PowerMode::BestEfficiency,
    }
}

/// `v` moved by `steps` slider steps, clamped to 0..=1.
pub fn nudge(v: f32, steps: f32) -> f32 {
    let v = if v.is_finite() { v } else { 0.0 };
    (v + steps * SLIDER_STEP).clamp(0.0, 1.0)
}

pub fn volume_glyph(volume: f32, muted: bool) -> char {
    if muted {
        return G_MUTED;
    }
    let v = if volume.is_finite() { volume } else { 0.0 };
    match v {
        v if v <= 0.0 => G_VOLUME[0],
        v if v < 1.0 / 3.0 => G_VOLUME[1],
        v if v < 2.0 / 3.0 => G_VOLUME[2],
        _ => G_VOLUME[3],
    }
}

/// The meaning of `part` on `page`.
pub fn action(page: QuickPage, part: Part) -> Option<Action> {
    Some(match part {
        Part::Tile(TILE_WIFI) => Action::ToggleWifi,
        Part::TileMore(TILE_WIFI) => Action::Open(QuickPage::Wifi),
        Part::Tile(TILE_BT) => Action::ToggleBt,
        Part::TileMore(TILE_BT) => Action::Open(QuickPage::Bluetooth),
        Part::Tile(TILE_AIRPLANE) => Action::ToggleAirplane,
        Part::Tile(TILE_SAVER) => Action::BatterySaver,
        Part::Tile(TILE_POWER) => Action::CyclePowerMode,
        Part::Tile(TILE_MUTE) => Action::ToggleMute,
        Part::Tile(TILE_DARK) => Action::ToggleDarkMode,
        Part::Tile(TILE_SNIP) => Action::Settings("ms-screenclip:"),
        Part::Tile(TILE_PROJECT) => Action::Settings("ms-settings-displays-topology:"),
        Part::Tile(TILE_LOCK) => Action::Lock,
        Part::Button(BTN_EDIT) => Action::Open(QuickPage::Edit),
        Part::Button(BTN_DONE) => Action::Back,
        Part::Button(BTN_RESET) => Action::ResetTiles,
        Part::Slider(SLIDER_VOLUME) => Action::Volume,
        Part::SliderTrailing(OUTPUT_CHEVRON) => Action::Open(QuickPage::Output),
        Part::Slider(id) if (BRIGHTNESS_BASE..ITEM_BASE).contains(&id) => {
            Action::Brightness((id - BRIGHTNESS_BASE) as usize)
        }
        Part::Button(BTN_BATTERY) => Action::Settings("ms-settings:batterysaver"),
        Part::Button(BTN_ALL_SETTINGS) => Action::Settings("ms-settings:"),
        Part::Button(BTN_MORE) => match page {
            QuickPage::Wifi => Action::Settings("ms-settings:network-wifi"),
            QuickPage::Bluetooth => Action::Settings("ms-settings:bluetooth"),
            QuickPage::Output => Action::Settings("ms-settings:sound"),
            QuickPage::Main | QuickPage::Edit => return None,
        },
        Part::Button(BTN_BACK) => Action::Back,
        Part::Button(BTN_CONNECT) => Action::Connect,
        Part::Button(BTN_CANCEL) => Action::CancelPassword,
        Part::Item(ITEM_RADIO) => Action::ToggleRadio,
        Part::Item(id) if id >= ITEM_BASE => {
            let i = (id - ITEM_BASE) as usize;
            match page {
                QuickPage::Wifi => Action::Network(i),
                QuickPage::Bluetooth => Action::Device(i),
                QuickPage::Output => Action::Output(i),
                QuickPage::Edit => Action::ToggleTile(*QuickTile::ALL.get(i)?),
                QuickPage::Main => return None,
            }
        }
        _ => return None,
    })
}

pub fn rows(page: QuickPage, i: &QuickInputs, lang: Lang) -> Vec<Row> {
    match page {
        QuickPage::Main => main_rows(i, lang),
        QuickPage::Wifi => wifi_rows(i, lang),
        QuickPage::Bluetooth => bt_rows(i, lang),
        QuickPage::Output => output_rows(i, lang),
        QuickPage::Edit => edit_rows(i, lang),
    }
}

/// The tiles shown, in order.
pub fn shown_tiles(i: &QuickInputs) -> &[QuickTile] {
    i.tiles.as_deref().unwrap_or(&QuickTile::DEFAULT)
}

fn tile_glyph(t: QuickTile, i: &QuickInputs) -> char {
    match t {
        QuickTile::Wifi => G_WIFI,
        QuickTile::Bluetooth => G_BT,
        QuickTile::EnergySaver => G_SAVER,
        QuickTile::PowerMode => G_POWER,
        QuickTile::Mute => i
            .audio
            .as_ref()
            .map_or(G_MUTED, |a| volume_glyph(a.volume, true)),
        QuickTile::DarkMode => G_MOON,
        QuickTile::Snip => G_SNIP,
        QuickTile::Project => G_PROJECT,
        QuickTile::Lock => G_LOCK,
    }
}

fn tile_name(t: QuickTile, lang: Lang) -> &'static str {
    tr(
        lang,
        match t {
            QuickTile::Wifi => Key::WiFi,
            QuickTile::Bluetooth => Key::QuickBluetooth,
            QuickTile::EnergySaver => Key::BatterySaver,
            QuickTile::PowerMode => Key::PowerModeLabel,
            QuickTile::Mute => Key::Mute,
            QuickTile::DarkMode => Key::DarkMode,
            QuickTile::Snip => Key::Snip,
            QuickTile::Project => Key::Project,
            QuickTile::Lock => Key::Lock,
        },
    )
}

/// The tile `t` as the main page shows it; `None` when this PC cannot show it (no adapter,
/// no battery, a state Windows will not tell).
fn tile_of(t: QuickTile, i: &QuickInputs, lang: Lang) -> Option<Tile> {
    let plain = |id: u32, on: bool| Tile {
        id,
        glyph: tile_glyph(t, i),
        label: tile_name(t, lang).to_string(),
        on,
        more: false,
        tint: None,
    };
    Some(match t {
        QuickTile::Wifi => {
            let w = i.wifi.as_ref()?;
            Tile {
                label: w
                    .connected
                    .clone()
                    .filter(|_| w.radio_on != Some(false))
                    .unwrap_or_else(|| tr(lang, Key::WiFi).to_string()),
                more: true,
                ..plain(TILE_WIFI, w.radio_on == Some(true))
            }
        }
        QuickTile::Bluetooth => {
            let b = bt_present(i)?;
            Tile {
                more: true,
                ..plain(TILE_BT, b.radio_on == Some(true))
            }
        }
        QuickTile::EnergySaver => {
            if !i.power.has_battery {
                return None;
            }
            plain(TILE_SAVER, i.power.saver)
        }
        QuickTile::PowerMode => {
            let m = i.power.mode?;
            Tile {
                label: mode_label(m, lang).to_string(),
                tint: Some(mode_tint(m)),
                ..plain(TILE_POWER, false)
            }
        }
        QuickTile::Mute => {
            let a = i.audio.as_ref()?;
            Tile {
                glyph: volume_glyph(a.volume, a.muted),
                ..plain(TILE_MUTE, a.muted)
            }
        }
        QuickTile::DarkMode => plain(TILE_DARK, i.dark_mode?),
        QuickTile::Snip => plain(TILE_SNIP, false),
        QuickTile::Project => plain(TILE_PROJECT, false),
        QuickTile::Lock => plain(TILE_LOCK, false),
    })
}

/// The editor: every tile, the shown ones first (in their order), each with a check.
fn edit_rows(i: &QuickInputs, lang: Lang) -> Vec<Row> {
    let shown = shown_tiles(i);
    let mut rows = vec![
        Row::Header {
            text: tr(lang, Key::QuickEditTitle).to_string(),
            trailing: None,
            actions: Vec::new(),
        },
        Row::Text(tr(lang, Key::QuickEditHint).to_string()),
    ];
    let hidden = QuickTile::ALL.iter().filter(|t| !shown.contains(t));
    for &t in shown.iter().chain(hidden) {
        let Some(k) = QuickTile::ALL.iter().position(|x| *x == t) else {
            continue;
        };
        let on = shown.contains(&t);
        rows.push(Row::Item {
            id: ITEM_BASE + k as u32,
            glyph: Some(tile_glyph(t, i)),
            text: tile_name(t, lang).to_string(),
            detail: None,
            checked: on,
            busy: false,
        });
    }
    rows.push(Row::Separator);
    rows.push(Row::Buttons(vec![
        button(BTN_RESET, Some(G_RESET), tr(lang, Key::QuickReset)),
        button(BTN_DONE, Some(G_DONE), tr(lang, Key::Done)),
    ]));
    rows
}

fn mode_label(m: PowerMode, lang: Lang) -> &'static str {
    tr(
        lang,
        match m {
            PowerMode::BestEfficiency => Key::PowerEfficiency,
            PowerMode::Balanced => Key::PowerBalanced,
            PowerMode::BestPerformance => Key::PowerPerformance,
        },
    )
}

fn notice_text(n: Notice, lang: Lang) -> &'static str {
    tr(
        lang,
        match n {
            Notice::WifiHandOff => Key::NoticeWifiHandOff,
            Notice::BtHandOff => Key::NoticeBtHandOff,
            Notice::Denied => Key::NoticeDenied,
            Notice::BadPassword => Key::NoticeBadPassword,
            Notice::ConnectFailed => Key::NoticeConnectFailed,
            Notice::Failed => Key::NoticeFailed,
        },
    )
}

fn button(id: u32, glyph: Option<char>, text: &str) -> Button {
    Button {
        id,
        glyph,
        text: text.to_string(),
    }
}

/// A sub-page's footer: Windows' own settings for the area, then the way back (one button
/// per row: the labels are too long to share one).
fn footer(rows: &mut Vec<Row>, more: Key, lang: Lang) {
    rows.push(Row::Separator);
    rows.push(Row::Buttons(vec![button(
        BTN_MORE,
        Some(G_SETTINGS),
        tr(lang, more),
    )]));
    rows.push(Row::Buttons(vec![button(
        BTN_BACK,
        Some(G_BACK),
        tr(lang, Key::Back),
    )]));
}

fn notice(rows: &mut Vec<Row>, i: &QuickInputs, lang: Lang) {
    if let Some(n) = i.ui.notice {
        rows.push(Row::Text(notice_text(n, lang).to_string()));
    }
}

fn radio_item(on: bool, text: &str, glyph: char, lang: Lang) -> Row {
    Row::Item {
        id: ITEM_RADIO,
        glyph: Some(glyph),
        text: text.to_string(),
        detail: Some(tr(lang, if on { Key::On } else { Key::Off }).to_string()),
        checked: on,
        busy: false,
    }
}

fn bt_present(i: &QuickInputs) -> Option<&BtInputs> {
    i.bt.as_ref().filter(|b| b.radio_on.is_some())
}

fn main_rows(i: &QuickInputs, lang: Lang) -> Vec<Row> {
    let mut rows = Vec::new();
    let tiles: Vec<Tile> = shown_tiles(i)
        .iter()
        .filter_map(|&t| tile_of(t, i, lang))
        .collect();
    if !tiles.is_empty() {
        rows.push(Row::Tiles(tiles));
    }
    let named = i.displays.len() > 1;
    for (k, d) in i.displays.iter().enumerate() {
        if named {
            let name = if d.name.is_empty() {
                tr(lang, Key::BuiltInDisplay).to_string()
            } else {
                d.name.clone()
            };
            rows.push(Row::Text(name));
        }
        rows.push(Row::Slider {
            id: BRIGHTNESS_BASE + k as u32,
            glyph: G_BRIGHTNESS,
            value: d.level,
            trailing: None,
        });
    }
    if let Some(a) = &i.audio {
        rows.push(Row::Slider {
            id: SLIDER_VOLUME,
            glyph: volume_glyph(a.volume, a.muted),
            value: a.volume,
            trailing: a.can_switch.then_some((OUTPUT_CHEVRON, G_CHEVRON)),
        });
    }
    rows.push(Row::Separator);
    let mut buttons = Vec::new();
    if let Some(b) = i
        .power
        .battery_label
        .as_deref()
        .filter(|_| i.power.has_battery)
    {
        buttons.push(button(BTN_BATTERY, Some(G_BATTERY), b));
    }
    buttons.push(button(
        BTN_ALL_SETTINGS,
        Some(G_SETTINGS),
        tr(lang, Key::AllSettings),
    ));
    buttons.push(button(BTN_EDIT, Some(G_EDIT), tr(lang, Key::QuickEdit)));
    rows.push(Row::Buttons(buttons));
    rows
}

fn wifi_rows(i: &QuickInputs, lang: Lang) -> Vec<Row> {
    let mut rows = Vec::new();
    let w = i.wifi.as_ref();
    // A connect in progress shows on its network's row.
    rows.push(Row::Header {
        text: tr(lang, Key::WiFi).to_string(),
        trailing: w
            .is_some_and(|w| w.scanning && w.radio_on != Some(false))
            .then(|| tr(lang, Key::Scanning).to_string()),
        actions: Vec::new(),
    });
    let Some(w) = w else {
        notice(&mut rows, i, lang);
        footer(&mut rows, Key::MoreWifiSettings, lang);
        return rows;
    };
    if let Some(on) = w.radio_on {
        rows.push(radio_item(on, tr(lang, Key::WiFi), G_WIFI, lang));
    }
    notice(&mut rows, i, lang);
    if w.radio_on != Some(false) {
        if w.networks.is_empty() && !w.scanning {
            rows.push(Row::Text(tr(lang, Key::NoNetworks).to_string()));
        }
        for (k, n) in w.networks.iter().enumerate() {
            let connected = w.connected.as_deref() == Some(n.ssid.as_str());
            let connecting = i.ui.connecting.as_deref() == Some(n.ssid.as_str());
            let kind = tr(
                lang,
                if n.secured {
                    Key::SecuredNetwork
                } else {
                    Key::OpenNetwork
                },
            );
            let detail = if connecting {
                tr(lang, Key::Connecting).to_string()
            } else if connected {
                format!("{} · {kind}", tr(lang, Key::WifiConnected))
            } else {
                kind.to_string()
            };
            let bars = usize::from(n.bars.clamp(1, 4)) - 1;
            rows.push(Row::Item {
                id: ITEM_BASE + k as u32,
                glyph: Some(G_WIFI_BARS[bars]),
                text: n.ssid.clone(),
                detail: Some(detail),
                checked: connected,
                busy: connecting,
            });
            if i.ui.password_for.as_deref() == Some(n.ssid.as_str()) {
                rows.push(Row::Custom {
                    id: CUSTOM_PASSWORD,
                    h: PASSWORD_H_DIP,
                });
                rows.push(Row::Buttons(vec![
                    button(BTN_CONNECT, None, tr(lang, Key::Connect)),
                    button(BTN_CANCEL, None, tr(lang, Key::Cancel)),
                ]));
            }
        }
    }
    footer(&mut rows, Key::MoreWifiSettings, lang);
    rows
}

fn bt_rows(i: &QuickInputs, lang: Lang) -> Vec<Row> {
    let mut rows = vec![Row::Header {
        text: tr(lang, Key::QuickBluetooth).to_string(),
        trailing: None,
        actions: Vec::new(),
    }];
    let b = bt_present(i);
    if let Some(b) = b {
        let on = b.radio_on == Some(true);
        rows.push(radio_item(on, tr(lang, Key::QuickBluetooth), G_BT, lang));
    }
    notice(&mut rows, i, lang);
    if let Some(b) = b.filter(|b| b.radio_on == Some(true)) {
        if b.devices.is_empty() {
            rows.push(Row::Text(tr(lang, Key::NoDevices).to_string()));
        }
        for (k, d) in b.devices.iter().enumerate() {
            let state = tr(
                lang,
                if d.connected {
                    Key::Connected
                } else {
                    Key::NotConnected
                },
            );
            let detail = match d.battery {
                Some(p) => format!("{state} · {p}%"),
                None => state.to_string(),
            };
            rows.push(Row::Item {
                id: ITEM_BASE + k as u32,
                glyph: Some(if d.audio { G_HEADPHONE } else { G_BT }),
                text: d.name.clone(),
                detail: Some(detail),
                checked: d.connected,
                busy: d.pending,
            });
        }
    }
    footer(&mut rows, Key::MoreBluetoothSettings, lang);
    rows
}

fn output_rows(i: &QuickInputs, lang: Lang) -> Vec<Row> {
    let mut rows = vec![Row::Header {
        text: tr(lang, Key::SoundOutput).to_string(),
        trailing: None,
        actions: Vec::new(),
    }];
    notice(&mut rows, i, lang);
    let devices = i.audio.as_ref().map_or(&[][..], |a| &a.devices[..]);
    if devices.is_empty() {
        rows.push(Row::Text(tr(lang, Key::NoOutputs).to_string()));
    }
    let default = i.audio.as_ref().and_then(|a| a.default);
    for (k, name) in devices.iter().enumerate() {
        rows.push(Row::Item {
            id: ITEM_BASE + k as u32,
            glyph: Some(G_SPEAKER),
            text: name.clone(),
            detail: None,
            checked: default == Some(k),
            busy: false,
        });
    }
    footer(&mut rows, Key::MoreSoundSettings, lang);
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiles(rows: &[Row]) -> Vec<Tile> {
        rows.iter()
            .find_map(|r| match r {
                Row::Tiles(t) => Some(t.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }

    fn tile_ids(rows: &[Row]) -> Vec<u32> {
        tiles(rows).iter().map(|t| t.id).collect()
    }

    fn sliders(rows: &[Row]) -> Vec<u32> {
        rows.iter()
            .filter_map(|r| match r {
                Row::Slider { id, .. } => Some(*id),
                _ => None,
            })
            .collect()
    }

    fn buttons(rows: &[Row]) -> Vec<Button> {
        rows.iter()
            .flat_map(|r| match r {
                Row::Buttons(b) => b.clone(),
                _ => Vec::new(),
            })
            .collect()
    }

    fn texts(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .filter_map(|r| match r {
                Row::Text(t) => Some(t.clone()),
                _ => None,
            })
            .collect()
    }

    /// (id, text, checked) of every item.
    fn items(rows: &[Row]) -> Vec<(u32, String, bool)> {
        rows.iter()
            .filter_map(|r| match r {
                Row::Item {
                    id, text, checked, ..
                } => Some((*id, text.clone(), *checked)),
                _ => None,
            })
            .collect()
    }

    fn wifi() -> WifiInputs {
        WifiInputs {
            radio_on: Some(true),
            connected: Some("Casa".into()),
            networks: vec![
                WifiNet {
                    ssid: "Casa".into(),
                    bars: 4,
                    secured: true,
                },
                WifiNet {
                    ssid: "Bar".into(),
                    bars: 2,
                    secured: false,
                },
                WifiNet {
                    ssid: "Vicino".into(),
                    bars: 1,
                    secured: true,
                },
            ],
            scanning: false,
        }
    }

    fn laptop() -> QuickInputs {
        QuickInputs {
            wifi: Some(WifiInputs {
                connected: None,
                ..wifi()
            }),
            bt: Some(BtInputs {
                radio_on: Some(false),
                devices: Vec::new(),
            }),
            audio: Some(AudioInputs {
                volume: 0.5,
                muted: false,
                devices: vec!["Speakers".into(), "Headphones".into()],
                default: Some(0),
                can_switch: true,
            }),
            displays: vec![DisplayInput {
                name: String::new(),
                level: 0.7,
            }],
            power: PowerInputs {
                has_battery: true,
                saver: false,
                mode: Some(PowerMode::Balanced),
                battery_label: Some("80%".into()),
            },
            airplane: Some(false),
            ui: QuickUi::default(),
            tiles: None,
            dark_mode: Some(false),
        }
    }

    fn desktop() -> QuickInputs {
        QuickInputs {
            power: PowerInputs {
                has_battery: false,
                saver: false,
                mode: Some(PowerMode::Balanced),
                battery_label: None,
            },
            ..laptop()
        }
    }

    #[test]
    fn laptop_main_page_has_everything_in_order() {
        let r = rows(QuickPage::Main, &laptop(), Lang::En);
        assert_eq!(
            tile_ids(&r),
            vec![TILE_WIFI, TILE_BT, TILE_SAVER, TILE_POWER, TILE_MUTE, TILE_DARK]
        );
        let t = tiles(&r);
        assert!(t[0].more && t[1].more && !t[2].more);
        // The power tile wears its mode's colour; the others do not.
        assert_eq!(t[3].tint, Some(mode_tint(PowerMode::Balanced)));
        assert!(t.iter().filter(|t| t.tint.is_some()).count() == 1);
        assert_eq!(sliders(&r), vec![BRIGHTNESS_BASE, SLIDER_VOLUME]);
        assert!(r.contains(&Row::Separator));
        let b = buttons(&r);
        assert_eq!(
            b.iter().map(|b| b.id).collect::<Vec<_>>(),
            vec![BTN_BATTERY, BTN_ALL_SETTINGS, BTN_EDIT]
        );
        assert_eq!(b[0].text, "80%");
        assert_eq!(b[1].text, "All settings");
        // The volume slider has the Output chevron.
        assert!(r.iter().any(|x| matches!(
            x,
            Row::Slider {
                id: SLIDER_VOLUME,
                trailing: Some((OUTPUT_CHEVRON, _)),
                ..
            }
        )));
        // One display: no name above its slider.
        assert!(texts(&r).is_empty());
    }

    #[test]
    fn no_battery_no_saver_tile_and_no_battery_label() {
        let r = rows(QuickPage::Main, &desktop(), Lang::En);
        assert!(!tile_ids(&r).contains(&TILE_SAVER));
        assert_eq!(
            buttons(&r).iter().map(|b| b.id).collect::<Vec<_>>(),
            vec![BTN_ALL_SETTINGS, BTN_EDIT]
        );
    }

    #[test]
    fn no_wifi_adapter_no_wifi_tile() {
        let i = QuickInputs {
            wifi: None,
            ..laptop()
        };
        assert!(!tile_ids(&rows(QuickPage::Main, &i, Lang::En)).contains(&TILE_WIFI));
    }

    #[test]
    fn no_bluetooth_radio_no_bluetooth_tile() {
        for bt in [
            None,
            Some(BtInputs {
                radio_on: None,
                devices: Vec::new(),
            }),
        ] {
            let i = QuickInputs { bt, ..laptop() };
            assert!(!tile_ids(&rows(QuickPage::Main, &i, Lang::En)).contains(&TILE_BT));
        }
    }

    #[test]
    fn the_picked_tiles_show_in_their_order() {
        let i = QuickInputs {
            tiles: Some(vec![QuickTile::Lock, QuickTile::PowerMode, QuickTile::Wifi]),
            ..laptop()
        };
        assert_eq!(
            tile_ids(&rows(QuickPage::Main, &i, Lang::En)),
            vec![TILE_LOCK, TILE_POWER, TILE_WIFI]
        );
        let none = QuickInputs {
            tiles: Some(Vec::new()),
            ..laptop()
        };
        assert!(tiles(&rows(QuickPage::Main, &none, Lang::En)).is_empty());
    }

    #[test]
    fn power_modes_have_three_distinct_colours() {
        let c = [
            mode_tint(PowerMode::BestEfficiency),
            mode_tint(PowerMode::Balanced),
            mode_tint(PowerMode::BestPerformance),
        ];
        assert!(c[0] != c[1] && c[1] != c[2] && c[0] != c[2]);
    }

    #[test]
    fn the_editor_lists_shown_tiles_first_and_toggles_them() {
        let i = QuickInputs {
            tiles: Some(vec![QuickTile::Mute, QuickTile::Wifi]),
            ..laptop()
        };
        let r = rows(QuickPage::Edit, &i, Lang::En);
        let it = items(&r);
        assert_eq!(it.len(), QuickTile::ALL.len());
        assert_eq!(it[0].1, "Mute");
        assert_eq!(it[1].1, "Wi-Fi");
        assert!(it[0].2 && it[1].2 && !it[2].2);
        // Every item maps back to its own tile.
        for (id, _, _) in &it {
            let a = action(QuickPage::Edit, Part::Item(*id));
            let k = (*id - ITEM_BASE) as usize;
            assert_eq!(a, Some(Action::ToggleTile(QuickTile::ALL[k])));
        }
        assert_eq!(
            action(QuickPage::Edit, Part::Button(BTN_DONE)),
            Some(Action::Back)
        );
        assert_eq!(
            action(QuickPage::Main, Part::Button(BTN_EDIT)),
            Some(Action::Open(QuickPage::Edit))
        );
    }

    #[test]
    fn mute_and_dark_mode_tiles_follow_the_system() {
        let mut i = laptop();
        i.dark_mode = None;
        assert!(!tile_ids(&rows(QuickPage::Main, &i, Lang::En)).contains(&TILE_DARK));
        i.dark_mode = Some(true);
        i.audio.as_mut().unwrap().muted = true;
        let t = tiles(&rows(QuickPage::Main, &i, Lang::En));
        let on = |id| t.iter().find(|t| t.id == id).unwrap().on;
        assert!(on(TILE_DARK) && on(TILE_MUTE));
    }

    #[test]
    fn no_power_mode_hides_its_tile() {
        let mut i = laptop();
        i.power.mode = None;
        assert!(!tile_ids(&rows(QuickPage::Main, &i, Lang::En)).contains(&TILE_POWER));
    }

    #[test]
    fn nothing_at_all_still_has_the_settings_button() {
        let i = QuickInputs::default();
        let r = rows(QuickPage::Main, &i, Lang::En);
        assert!(tiles(&r).is_empty());
        assert!(!r.iter().any(|x| matches!(x, Row::Tiles(_))));
        assert!(sliders(&r).is_empty());
        assert_eq!(
            buttons(&r).iter().map(|b| b.id).collect::<Vec<_>>(),
            vec![BTN_ALL_SETTINGS, BTN_EDIT]
        );
    }

    #[test]
    fn brightness_sliders_follow_the_displays() {
        let mut i = laptop();
        i.displays.clear();
        let r = rows(QuickPage::Main, &i, Lang::En);
        assert_eq!(sliders(&r), vec![SLIDER_VOLUME]);

        i.displays = vec![
            DisplayInput {
                name: String::new(),
                level: 0.3,
            },
            DisplayInput {
                name: "DELL U2720Q".into(),
                level: 0.9,
            },
        ];
        let r = rows(QuickPage::Main, &i, Lang::It);
        assert_eq!(
            sliders(&r),
            vec![BRIGHTNESS_BASE, BRIGHTNESS_BASE + 1, SLIDER_VOLUME]
        );
        assert_eq!(texts(&r), vec!["Schermo integrato", "DELL U2720Q"]);
        let level = r.iter().find_map(|x| match x {
            Row::Slider { id, value, .. } if *id == BRIGHTNESS_BASE + 1 => Some(*value),
            _ => None,
        });
        assert_eq!(level, Some(0.9));
    }

    #[test]
    fn no_audio_no_volume_and_no_switch_no_chevron() {
        let i = QuickInputs {
            audio: None,
            ..laptop()
        };
        assert!(!sliders(&rows(QuickPage::Main, &i, Lang::En)).contains(&SLIDER_VOLUME));
        let mut i = laptop();
        i.audio.as_mut().unwrap().can_switch = false;
        let r = rows(QuickPage::Main, &i, Lang::En);
        assert!(r.iter().any(|x| matches!(
            x,
            Row::Slider {
                id: SLIDER_VOLUME,
                trailing: None,
                ..
            }
        )));
    }

    #[test]
    fn muted_volume_shows_the_mute_glyph() {
        let mut i = laptop();
        i.audio.as_mut().unwrap().muted = true;
        let r = rows(QuickPage::Main, &i, Lang::En);
        let g = r.iter().find_map(|x| match x {
            Row::Slider {
                id: SLIDER_VOLUME,
                glyph,
                ..
            } => Some(*glyph),
            _ => None,
        });
        assert_eq!(g, Some(volume_glyph(0.5, true)));
        assert_ne!(volume_glyph(0.5, true), volume_glyph(0.5, false));
    }

    #[test]
    fn tiles_reflect_the_state() {
        let mut i = laptop();
        i.power.saver = true;
        let t = tiles(&rows(QuickPage::Main, &i, Lang::En));
        let on = |id| t.iter().find(|t| t.id == id).unwrap().on;
        assert!(on(TILE_WIFI) && !on(TILE_BT) && on(TILE_SAVER));
        // Connected: the Wi-Fi tile names the network.
        let i = QuickInputs {
            wifi: Some(wifi()),
            ..laptop()
        };
        let t = tiles(&rows(QuickPage::Main, &i, Lang::En));
        assert_eq!(t[0].label, "Casa");
    }

    #[test]
    fn italian_labels_for_every_tile() {
        let mut i = laptop();
        let t = tiles(&rows(QuickPage::Main, &i, Lang::It));
        let labels: Vec<&str> = t.iter().map(|t| t.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "Wi-Fi",
                "Bluetooth",
                "Risparmio energia",
                "Bilanciato",
                "Muto",
                "Tema scuro"
            ]
        );
        i.power.mode = Some(PowerMode::BestPerformance);
        assert_eq!(
            tiles(&rows(QuickPage::Main, &i, Lang::It))[3].label,
            "Prestazioni"
        );
        i.power.mode = Some(PowerMode::BestEfficiency);
        assert_eq!(
            tiles(&rows(QuickPage::Main, &i, Lang::It))[3].label,
            "Efficienza"
        );
        let b = buttons(&rows(QuickPage::Main, &i, Lang::It));
        assert_eq!(b[b.len() - 2].text, "Tutte le impostazioni");
        assert_eq!(b.last().unwrap().text, "Modifica");
    }

    #[test]
    fn wifi_page_lists_networks_in_order_with_the_connected_one_checked() {
        let i = QuickInputs {
            wifi: Some(wifi()),
            ..laptop()
        };
        let r = rows(QuickPage::Wifi, &i, Lang::En);
        assert!(matches!(&r[0], Row::Header { text, .. } if text == "Wi-Fi"));
        let it = items(&r);
        assert_eq!(it[0], (ITEM_RADIO, "Wi-Fi".into(), true));
        assert_eq!(
            it[1..].to_vec(),
            vec![
                (ITEM_BASE, "Casa".to_string(), true),
                (ITEM_BASE + 1, "Bar".to_string(), false),
                (ITEM_BASE + 2, "Vicino".to_string(), false),
            ]
        );
        let b = buttons(&r);
        assert_eq!(
            b.iter()
                .map(|b| (b.id, b.text.as_str()))
                .collect::<Vec<_>>(),
            vec![(BTN_MORE, "More Wi-Fi settings"), (BTN_BACK, "Back")]
        );
        assert_eq!(
            action(QuickPage::Wifi, Part::Button(BTN_MORE)),
            Some(Action::Settings("ms-settings:network-wifi"))
        );
        assert_eq!(
            action(QuickPage::Wifi, Part::Item(ITEM_BASE + 2)),
            Some(Action::Network(2))
        );
    }

    #[test]
    fn italian_network_and_device_states_agree_in_gender() {
        let mut i = QuickInputs {
            wifi: Some(wifi()),
            ..laptop()
        };
        i.bt = Some(BtInputs {
            radio_on: Some(true),
            devices: vec![BtDev {
                name: "Cuffie".into(),
                connected: true,
                ..BtDev::default()
            }],
        });
        let detail = |r: &[Row]| {
            r.iter().find_map(|x| match x {
                Row::Item { id, detail, .. } if *id == ITEM_BASE => detail.clone(),
                _ => None,
            })
        };
        assert_eq!(
            detail(&rows(QuickPage::Wifi, &i, Lang::It)).as_deref(),
            Some("Connessa · Protetta")
        );
        assert_eq!(
            detail(&rows(QuickPage::Bluetooth, &i, Lang::It)).as_deref(),
            Some("Connesso")
        );
    }

    #[test]
    fn wifi_page_scanning_connecting_and_radio_off() {
        let mut w = wifi();
        w.scanning = true;
        let mut i = QuickInputs {
            wifi: Some(w),
            ..laptop()
        };
        i.ui.connecting = Some("Bar".into());
        let r = rows(QuickPage::Wifi, &i, Lang::It);
        assert!(
            matches!(&r[0], Row::Header { trailing: Some(t), .. } if t == "Ricerca…"),
            "{:?}",
            r[0]
        );
        let busy = r.iter().find_map(|x| match x {
            Row::Item {
                id, busy, detail, ..
            } if *id == ITEM_BASE + 1 => Some((*busy, detail.clone())),
            _ => None,
        });
        assert_eq!(busy, Some((true, Some("Connessione…".to_string()))));

        let mut w = wifi();
        w.radio_on = Some(false);
        let i = QuickInputs {
            wifi: Some(w),
            ..laptop()
        };
        let it = items(&rows(QuickPage::Wifi, &i, Lang::En));
        assert_eq!(it, vec![(ITEM_RADIO, "Wi-Fi".into(), false)]);
    }

    #[test]
    fn password_row_opens_under_its_network() {
        let mut i = QuickInputs {
            wifi: Some(wifi()),
            ..laptop()
        };
        i.ui.password_for = Some("Vicino".into());
        i.ui.notice = Some(Notice::BadPassword);
        let r = rows(QuickPage::Wifi, &i, Lang::It);
        let at = r
            .iter()
            .position(|x| matches!(x, Row::Item { id, .. } if *id == ITEM_BASE + 2))
            .unwrap();
        assert_eq!(
            r[at + 1],
            Row::Custom {
                id: CUSTOM_PASSWORD,
                h: PASSWORD_H_DIP
            }
        );
        match &r[at + 2] {
            Row::Buttons(b) => assert_eq!(
                b.iter()
                    .map(|b| (b.id, b.text.as_str()))
                    .collect::<Vec<_>>(),
                vec![(BTN_CONNECT, "Connetti"), (BTN_CANCEL, "Annulla")]
            ),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            texts(&r),
            vec!["La password deve avere da 8 a 63 caratteri"]
        );
    }

    #[test]
    fn bluetooth_page_lists_devices() {
        let mut i = laptop();
        i.bt = Some(BtInputs {
            radio_on: Some(true),
            devices: vec![
                BtDev {
                    name: "Cuffie".into(),
                    connected: true,
                    audio: true,
                    battery: Some(80),
                    pending: false,
                },
                BtDev {
                    name: "Mouse".into(),
                    connected: false,
                    audio: false,
                    battery: None,
                    pending: true,
                },
            ],
        });
        let r = rows(QuickPage::Bluetooth, &i, Lang::En);
        assert!(matches!(&r[0], Row::Header { text, .. } if text == "Bluetooth"));
        assert_eq!(
            items(&r),
            vec![
                (ITEM_RADIO, "Bluetooth".into(), true),
                (ITEM_BASE, "Cuffie".into(), true),
                (ITEM_BASE + 1, "Mouse".into(), false),
            ]
        );
        let detail = r.iter().find_map(|x| match x {
            Row::Item { id, detail, .. } if *id == ITEM_BASE => detail.clone(),
            _ => None,
        });
        assert_eq!(detail.as_deref(), Some("Connected · 80%"));
        assert!(r.iter().any(|x| matches!(
            x,
            Row::Item { id, busy: true, .. } if *id == ITEM_BASE + 1
        )));
        assert_eq!(
            buttons(&r)
                .iter()
                .map(|b| b.text.as_str())
                .collect::<Vec<_>>(),
            vec!["More Bluetooth settings", "Back"]
        );
        assert_eq!(
            action(QuickPage::Bluetooth, Part::Button(BTN_MORE)),
            Some(Action::Settings("ms-settings:bluetooth"))
        );
    }

    #[test]
    fn output_page_checks_the_default() {
        let mut i = laptop();
        i.audio.as_mut().unwrap().default = Some(1);
        let r = rows(QuickPage::Output, &i, Lang::En);
        assert_eq!(
            items(&r),
            vec![
                (ITEM_BASE, "Speakers".into(), false),
                (ITEM_BASE + 1, "Headphones".into(), true),
            ]
        );
        assert_eq!(
            action(QuickPage::Output, Part::Item(ITEM_BASE + 1)),
            Some(Action::Output(1))
        );
        assert!(buttons(&r).iter().any(|b| b.id == BTN_BACK));
    }

    #[test]
    fn a_missing_service_leaves_its_page_with_just_the_way_back() {
        let i = QuickInputs::default();
        for p in [QuickPage::Wifi, QuickPage::Bluetooth, QuickPage::Output] {
            let r = rows(p, &i, Lang::En);
            assert!(buttons(&r).iter().any(|b| b.id == BTN_BACK), "{p:?}");
        }
    }

    #[test]
    fn actions_map_main_parts() {
        let m = QuickPage::Main;
        assert_eq!(action(m, Part::Tile(TILE_WIFI)), Some(Action::ToggleWifi));
        assert_eq!(
            action(m, Part::TileMore(TILE_BT)),
            Some(Action::Open(QuickPage::Bluetooth))
        );
        assert_eq!(
            action(m, Part::SliderTrailing(OUTPUT_CHEVRON)),
            Some(Action::Open(QuickPage::Output))
        );
        assert_eq!(
            action(m, Part::Slider(BRIGHTNESS_BASE + 1)),
            Some(Action::Brightness(1))
        );
        assert_eq!(
            action(m, Part::Button(BTN_ALL_SETTINGS)),
            Some(Action::Settings("ms-settings:"))
        );
        assert_eq!(action(m, Part::Item(ITEM_BASE)), None);
        assert_eq!(action(m, Part::Custom(CUSTOM_PASSWORD)), None);
    }

    #[test]
    fn helpers() {
        assert_eq!(
            next_mode(PowerMode::BestPerformance),
            PowerMode::BestEfficiency
        );
        assert_eq!(next_mode(PowerMode::Balanced), PowerMode::BestPerformance);
        assert!((nudge(0.5, 1.0) - 0.52).abs() < 1e-6);
        assert_eq!(nudge(0.99, 1.0), 1.0);
        assert_eq!(nudge(0.01, -1.0), 0.0);
        assert_eq!(nudge(f32::NAN, 1.0), 0.02);
        assert_eq!(volume_glyph(0.0, false), G_VOLUME[0]);
        assert_eq!(volume_glyph(1.0, false), G_VOLUME[3]);
    }
}
