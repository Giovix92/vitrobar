# Stage 3 — Quick panel, calendar, music and Claude widgets Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the stock Quick Settings and clock flyouts with Vitrobar's own glass panels (opening above the bar, so they follow the pill), and ship the Music and Claude-usage widgets in the first release.

**Architecture:** A small pure "panel kit" in `vitro-core` (rows: tiles, sliders, list rows, headers, buttons; layout + hit test + keyboard focus) drawn by one `vitro-render` view on the existing glass `Flyout`. Each system area (audio, brightness, power, Wi-Fi, Bluetooth, media, Claude usage) is one `vitro-win` module with a pure snapshot type and event-driven change notification (no polling). Services are created when their panel/widget needs them and released when it closes, so idle memory stays put.

**Tech Stack:** Rust, `windows` 0.62.2 (Core Audio, WLAN API, WMI, dxva2 DDC/CI, Windows.Devices.Radios / Enumeration, Windows.Media.Control), existing Composition/WARP drawing.

**Spec:** `docs/superpowers/specs/2026-10-30-vitrobar-design.md` (§5.4 tray and quick panels, §5.7 widgets, §3 budgets).

**User decisions behind this plan (2026-10-31):** the quick panel must open where the pill is ("the wifi bluetooth etc. panel is still shown to the right"); Music and Claude limits must be in the first release (pulled forward from stage 5); "go ahead without asking".

## Rulings made while planning (recorded in the stage-3 followups at the end)

- **P1 Claude usage source.** Anthropic's Claude Code docs (updated 2026-02-19) say OAuth tokens from Free/Pro/Max accounts may not be used "in any other product, tool, or service". The spec says: if reading the token doesn't fit the terms, drop that approach. So Vitrobar **never reads Claude Code's credentials and makes no network call** for this widget. Instead it uses Claude Code's own **status line**: Claude Code pipes a JSON object to the configured `statusLine.command`, and since v2.1.80 that JSON carries `rate_limits.five_hour` / `rate_limits.seven_day` with `used_percentage` (0–100) and `resets_at` (epoch seconds; some versions give an ISO string — accept both). `vitrobar.exe --claude-statusline` is that command: it saves the two windows to a small file Vitrobar watches, then runs the user's previous status-line command (if any) with the same input and prints its output. The values update while Claude Code is in use; the tile shows "as of HH:MM".
- **P2 Notifications list** needs package identity (sparse package) and code signing, which don't exist yet. Deferred to a stage 3b plan together with signing. Until then the calendar panel has a "Notifications" button that opens the stock notification centre (existing `open_stock_notifications()`).
- **P3 Bluetooth connect/disconnect** has no general Windows API. Connect/disconnect is offered for audio devices via `BluetoothSetServiceState` on their audio services (the call path tools like ToothTray use); other devices show their state and hand off to `ms-settings:bluetooth`.
- **P4 Lazy services.** A system service (WLAN handle, audio endpoint, WMI, radios, media manager) is created when its panel opens or its widget is enabled, and dropped when the panel closes / the widget is disabled. Enabled widgets keep only their own service.

## Global Constraints

- All earlier Global Constraints apply (`2026-10-30-stage1-bar.md`, `2026-10-30-stage2-start.md`, `2026-10-31-stage2-5-polish.md`): RAM < 40 MB total idle with panels closed (widgets **off**), < 60 MB with a panel open, back under 40 MB after closing; with **both widgets on** idle must still be < 40 MB; idle CPU 0 %; no polling (WLAN/audio/power/radio/media/file-change notifications only; the Claude file is watched with `ReadDirectoryChangesW`); animations 150–250 ms and "Animation effects off" respected; WARP drawing; no injection; en + it strings via `vitro_core::i18n`; undocumented interfaces only read-only or through their normal call path, guarded so the feature hides itself on failure.
- Widgets are **off by default**; a disabled widget loads nothing and makes no calls (spec §5.7).
- No network use anywhere in this plan.
- Panels open above the bar element they belong to (below it with the bar at the top), clamped to the monitor's work area — never at a fixed screen corner.
- **Desktop safety (binding for every live test):** only touch windows/processes the test started; never toggle the user's Wi-Fi, Bluetooth or airplane mode, never change their volume, default audio device, brightness or power mode in a test — tests use fake services or read-only calls; never connect/disconnect a device; never Notepad; `VITROBAR_SETTINGS` → temp file with `first-run-done = true` and `start-with-windows = false`; never write `~/.claude/settings.json` in a test (use a temp path via `VITROBAR_CLAUDE_SETTINGS`); screenshots only of Vitrobar's own windows; the user may have Vitrobar running — use the debug profile, never kill a process you didn't start, and skip live take-over checks while their instance runs.

## Review Focus

1. **A service fails or is missing** (no Wi-Fi adapter, Bluetooth radio off/absent, desktop PC with no battery, external monitor without DDC/CI, WMI brightness absent, no media session, Claude Code never run) → that tile/row hides or says so; the panel still opens; nothing panics (all tasks; tests with fake services).
2. **Panel opened on the secondary monitor / pill mode / bar at the top** → it opens above (below) the button it belongs to on that monitor, inside the work area (Task 2).
3. **Rapid changes while a panel is open** (volume keys held, Wi-Fi scan results arriving, a track change every second) → the panel updates without flicker or growing memory; closing it releases the service (Tasks 3–9).
4. **Claude status-line bridge given odd input** — empty stdin, non-JSON, missing `rate_limits`, one window only, `resets_at` as number or ISO string, a previous status-line command that fails or hangs → the bridge never blocks Claude Code for more than 1 s, never loses the user's previous status-line output, never writes a corrupt file (Task 10).
5. **Enabling/disabling the Claude widget twice, or with an existing `statusLine`** → the user's original `statusLine` is restored exactly on disable; Vitrobar never overwrites one it didn't wrap (Task 10).

---

## File structure

```
crates/core/src/
  panel_kit.rs       NEW  panel rows, layout, hit test, keyboard focus (pure)
  quick_model.rs     NEW  quick-panel content from service snapshots (pure)
  calendar.rs        NEW  month grid (pure)
  widgets.rs         NEW  widget pill text/width, Claude usage parse + format (pure)
  layout.rs          MOD  widget slots in the bar
  settings.rs        MOD  widget_music, widget_claude, claude_prev_statusline
  settings_ui.rs     MOD  Widgets section
  i18n.rs            MOD  new keys (en + it)
crates/win/src/
  audio.rs           NEW  volume, mute, output devices (Core Audio + IPolicyConfig)
  brightness.rs      NEW  WMI (internal) + DDC/CI (external)
  power_status.rs    NEW  battery, power mode overlay
  wifi.rs            NEW  WLAN API: radio, scan, connect (PSK), notifications
  bluetooth.rs       NEW  radio on/off, paired devices, audio connect/disconnect
  media.rs           NEW  GSMTC session snapshot + commands + events
  claude_usage.rs    NEW  bridge file read/watch; statusline wrap/unwrap in Claude settings
  tray_host.rs       MOD  Shell_NotifyIconGetRect
crates/render/src/
  panel_view.rs      NEW  draws a panel_kit layout on a Flyout
  bar_visual.rs      MOD  widget pills
app/vitrobar/src/
  quick.rs           NEW  quick panel controller
  calendar_panel.rs  NEW  calendar controller
  widgets.rs         NEW  music + Claude widget controllers
  main.rs            MOD  --claude-statusline mode
  app.rs, bar_window.rs MOD  wiring, Hit::Widget
```

---

### Task 1: Panel kit (core) and panel view (render)

**Files:** Create `crates/core/src/panel_kit.rs`, `crates/render/src/panel_view.rs`; modify both `lib.rs`.

**Interfaces:**
- `vitro_core::panel_kit`:
  - `pub enum Row { Header { text: String, trailing: Option<String> }, Tiles(Vec<Tile>), Slider { id: u32, glyph: char, value: f32 /*0..=1*/, trailing: Option<(u32, char)> }, Item { id: u32, glyph: Option<char>, text: String, detail: Option<String>, checked: bool, busy: bool }, Buttons(Vec<Button>), Text(String), Separator, Custom { id: u32, h: f32 } }`
  - `pub struct Tile { pub id: u32, pub glyph: char, pub label: String, pub on: bool, pub more: bool }` (`more` = a chevron half that opens a sub-page, like stock Quick Settings)
  - `pub struct Button { pub id: u32, pub glyph: Option<char>, pub text: String }`
  - Constants: `WIDTH_DIP = 360.0`, `PAD_DIP = 12.0`, `GAP_DIP = 8.0`, `TILE_H_DIP = 64.0`, `TILES_PER_ROW = 3`, `SLIDER_H_DIP = 40.0`, `ITEM_H_DIP = 44.0`, `HEADER_H_DIP = 36.0`, `BUTTONS_H_DIP = 40.0`, `TEXT_H_DIP = 20.0`, `SEP_H_DIP = 9.0`, `MAX_H_DIP = 640.0` (content beyond scrolls).
  - `pub struct PanelLayout { pub size: (f32, f32), pub parts: Vec<(Part, Rect)>, pub content_h: f32 }`, `pub enum Part { Tile(u32), TileMore(u32), Slider(u32), SliderTrailing(u32), Item(u32), Button(u32), Custom(u32) }`
  - `pub fn layout(rows: &[Row]) -> PanelLayout`; `pub fn hit(l: &PanelLayout, pt_dip: (f32, f32), scroll: f32) -> Option<Part>`; `pub fn slider_value(l: &PanelLayout, id: u32, x_dip: f32) -> Option<f32>` (clamped 0..=1); `pub fn focus_step(l: &PanelLayout, cur: Option<Part>, key: NavKey) -> Option<Part>` (reuse `menu_model::NavKey`; Tab/Up/Down move in reading order, wrapping).
- `vitro_render::panel_view::PanelView`: `show(comp, painter, style, dpi, anchor_px, position, rows) -> Result<Self>` (uses `Flyout::show` anchored like the window preview), `update(&mut self, rows) -> Result<()>` (redraws only rows whose content changed; a size change calls `Flyout::reshape`), `set_hot(Option<Part>)`, `set_pressed`, `set_focus`, `scroll_by(dy)`, `hit_px`, `hwnd`, `rect_px`. Look: tiles = rounded 8 DIP plates, accent fill when `on`; sliders = 4 DIP track + 16 DIP thumb in accent; glyphs Segoe Fluent Icons; text via the existing `Painter`.

- [ ] **Step 1: Failing tests** (`panel_kit.rs`):
```rust
#[test] fn tiles_wrap_three_per_row() {
    let r = vec![Row::Tiles((0..4).map(|i| tile(i)).collect())];
    let l = layout(&r);
    assert_eq!(l.size.1, 12.0 + 64.0 + 8.0 + 64.0 + 12.0);
    assert_eq!(rect_of(&l, Part::Tile(3)).y, 12.0 + 64.0 + 8.0);
    assert_eq!(rect_of(&l, Part::Tile(0)).w, (360.0 - 24.0 - 16.0) / 3.0);
}
#[test] fn more_half_is_its_own_part() { /* Tile with more=true: right 40 % hits TileMore, left hits Tile */ }
#[test] fn slider_maps_x_to_value_and_clamps() { /* value at track start 0.0, end 1.0, beyond → clamped */ }
#[test] fn tall_content_scrolls_and_hit_follows_scroll() { /* 30 items → size.1 == 640, content_h > 640; hit with scroll 100 returns the item 100 DIP lower */ }
#[test] fn focus_walks_in_reading_order_and_wraps() {}
#[test] fn empty_panel_has_padding_only() { assert_eq!(layout(&[]).size, (360.0, 24.0)); }
```
- [ ] **Step 2:** `cargo test -p vitro-core panel_kit` → FAIL. **Step 3:** implement → PASS.
- [ ] **Step 4:** Implement `PanelView`; a render test in `crates/render/tests/` that builds a view off-screen (as the existing flyout tests do) with one row of each kind and asserts `hit_px` of a tile centre returns `Part::Tile`.
- [ ] **Step 5:** `cargo test --workspace` → PASS. **Commit** `feat(panels): panel kit and glass panel view`

---

### Task 2: Bar widget slots and panel anchoring

**Files:** Modify `crates/core/src/layout.rs`, `crates/core/src/settings.rs`, `crates/core/src/settings_ui.rs`, `crates/core/src/i18n.rs`, `app/vitrobar/src/bar_window.rs` (`Hit`), `crates/render/src/bar_visual.rs`.

**Interfaces:**
- `LayoutInput` gains `pub widgets: [f32; 2]` (DIP widths of the Music and Claude pills; 0.0 = hidden). `BarLayout` gains `pub widgets: [Option<Rect>; 2]`. Placement: on the tray-area bar only, immediately left of the tray area (Pill: inside the pill; Islands: at the start of the right island; Full: left of the tray), separated by `GAP`. Non-primary bars never show widgets. Index 0 = Music, 1 = Claude.
- `Hit::Widget(usize)`.
- `Settings`: `widget_music: bool` (false), `widget_claude: bool` (false), `claude_prev_statusline: Option<String>` (None) — all with the tolerant per-field load of stage 2.
- `settings_ui`: new `Section::Widgets` (between Behaviour and Language) with `ControlId::WidgetMusic` / `ControlId::WidgetClaude` toggles and a `Text` line under Claude: `Key::ClaudeWidgetHint` ("Shows your Claude 5-hour and weekly limits from Claude Code's status line. No account, no network.").
- `pub fn anchor_for(layout: &BarLayout, target: AnchorTarget) -> Rect` in `layout.rs` with `pub enum AnchorTarget { Quick, Clock, Widget(usize) }` — the DIP rect panels anchor to (on Pill/Islands the rect of that element; on Full too). Panels call `Flyout::show(anchor_px, position, …)`.
- i18n keys (en / it): `Widgets` "Widgets"/"Widget", `MusicWidget` "Music"/"Musica", `ClaudeWidget` "Claude limits"/"Limiti di Claude", `ClaudeWidgetHint` (above / "Mostra i limiti di 5 ore e settimanali di Claude dalla riga di stato di Claude Code. Nessun account, nessuna rete.").

- [ ] **Step 1: Failing tests** in `layout.rs`:
```rust
#[test] fn widgets_sit_left_of_the_tray_in_the_pill() {
    let mut i = input(MON, Shape::Pill, Position::Bottom, 5, true); i.widgets = [120.0, 90.0];
    let l = compute_layout(&i);
    let (m, c) = (l.widgets[0].unwrap(), l.widgets[1].unwrap());
    assert!(m.right() <= c.x && c.right() <= l.overflow_chevron.x);
    assert!(l.surfaces[0].x <= m.x && c.right() <= l.surfaces[0].right());
}
#[test] fn widgets_hidden_on_secondary_bars_and_when_zero() { /* tray_area false → [None, None]; width 0 → None */ }
#[test] fn pill_grows_by_the_widgets() { /* surface width with widgets == without + 120 + 90 + 2*GAP */ }
#[test] fn anchors_follow_the_element() { /* anchor_for(Quick) == l.quick, Clock == l.clock, Widget(1) == widgets[1] */ }
```
plus settings round-trip/tolerant-load tests for the three fields and a `settings_ui` test that toggling `WidgetMusic` sets `widget_music`.
- [ ] **Step 2:** FAIL → **Step 3:** implement (pill rendering: a widget pill is drawn by `bar_visual` as a rounded inner plate with a glyph + one line of text supplied by the app; hover uses the current hover effect's plate) → PASS.
- [ ] **Step 4:** `cargo test --workspace` → PASS. **Commit** `feat(bar): widget slots, settings and panel anchors`

---

### Task 3: Audio — volume, mute, output device

**Files:** Create `crates/win/src/audio.rs`.

**Interfaces:**
- `pub struct AudioSnapshot { pub volume: f32, pub muted: bool, pub devices: Vec<OutputDevice>, pub default_id: Option<String> }`, `pub struct OutputDevice { pub id: String, pub name: String }`.
- `pub struct Audio` — `pub fn open(wake: WakeQueue) -> Result<Self>` (IMMDeviceEnumerator; registers `IMMNotificationClient` and `IAudioEndpointVolumeCallback` on the default render endpoint; callbacks only push a wake, re-registering the volume callback when the default device changes); `snapshot(&self) -> AudioSnapshot`; `set_volume(f32)`, `set_muted(bool)`, `set_default(&str)` (IPolicyConfig `SetDefaultEndpoint` for eConsole and eMultimedia; guarded: if the interface can't be created, `devices` is still listed but `can_switch()` returns false and the UI hides switching).
- Use the WakeQueue/wake-message pattern of stage 1 (`event_queue.rs`).

- [ ] **Step 1:** Unit tests for the pure parts (volume clamping, snapshot of a device list sorted by name with default first) using a small trait `AudioBackend` with a fake implementation; the COM implementation is exercised by one `#[ignore]`d read-only test (`snapshot()` on the real machine returns a volume in 0..=1).
- [ ] **Step 2–4:** FAIL → implement → PASS (`cargo test -p vitro-win audio`).
- [ ] **Step 5: Commit** `feat(win): audio volume and output devices`

---

### Task 4: Brightness and power

**Files:** Create `crates/win/src/brightness.rs`, `crates/win/src/power_status.rs`.

**Interfaces:**
- `brightness.rs`: `pub struct Display { pub monitor: u64, pub name: String, pub level: f32, pub kind: BrightnessKind }`, `pub enum BrightnessKind { Internal, Ddc }`; `pub fn displays() -> Vec<Display>` (WMI `root\wmi` `WmiMonitorBrightness` for internal; dxva2 `GetPhysicalMonitorsFromHMONITOR` + `GetMonitorBrightness` for externals — DDC/CI calls are slow (≈50 ms each): run `displays()` and every `set_level` on a worker thread, results come back as a wake); `pub fn set_level(monitor: u64, level: f32)` (WMI `WmiSetBrightness`, or `SetMonitorBrightness`); brightness changes from the hardware keys arrive via WMI event `WmiMonitorBrightnessEvent` while the panel is open.
- `power_status.rs`: `pub struct PowerSnapshot { pub has_battery: bool, pub percent: Option<u8>, pub charging: bool, pub saver: bool, pub mode: Option<PowerMode> }`, `pub enum PowerMode { BestEfficiency, Balanced, BestPerformance }`; `pub fn snapshot() -> PowerSnapshot` (GetSystemPowerStatus; mode via `PowerGetEffectiveOverlayScheme`, the three documented overlay GUIDs — guarded, `None` if unavailable); `pub fn set_mode(PowerMode)` (`PowerSetActiveOverlayScheme`); change notifications: `RegisterPowerSettingNotification` for `GUID_BATTERY_PERCENTAGE_REMAINING`, `GUID_ACDC_POWER_SOURCE`, `GUID_POWER_SAVING_STATUS` on the notify window (arrives as `WM_POWERBROADCAST` → wake).

- [ ] **Step 1:** Pure tests: overlay GUID ↔ `PowerMode` mapping both ways and unknown GUID → `None`; DDC range normalisation (`min..max` → 0..=1 and back, max == min → 1.0); battery text helper `battery_label(&PowerSnapshot) -> Option<String>` ("99%", "99% · charging" in en/it via i18n keys `Charging` "charging"/"in carica").
- [ ] **Step 2–4:** FAIL → implement → PASS. One `#[ignore]`d read-only test per module (`displays()` returns without panic; `snapshot()` percent in 0..=100 or None).
- [ ] **Step 5: Commit** `feat(win): brightness and power status`

---

### Task 5: Wi-Fi

**Files:** Create `crates/win/src/wifi.rs`.

**Interfaces:**
- `pub struct WifiSnapshot { pub radio_on: Option<bool>, pub connected: Option<String>, pub networks: Vec<Network>, pub scanning: bool }`, `pub struct Network { pub ssid: String, pub signal: u8 /*0..100*/, pub secured: bool, pub kind: NetKind, pub known: bool }`, `pub enum NetKind { Open, Psk, Enterprise }`.
- `pub struct Wifi` — `open(wake) -> Result<Self>` (`WlanOpenHandle` v2, first interface; `WlanRegisterNotification` ACM+MSM → wake); `snapshot()`; `scan()`; `set_radio(bool)` (Windows.Devices.Radios `Radio.SetStateAsync` for the Wi-Fi radio; `RadioAccessStatus` denied → error shown); `connect(ssid, password: Option<&str>) -> Result<()>` (known network → `WlanConnect` with its profile; new PSK network → build a WPA2/WPA3-personal profile XML, `WlanSetProfile` with user scope, then connect; `Enterprise` → returns `Err(HandOff)` and the UI opens `ms-settings:network-wifi`); `disconnect()`. Captive portals are left to Windows (it opens the browser itself).
- `pub fn profile_xml(ssid: &str, password: &str, auth: Auth) -> String` — pure; XML-escapes the SSID and key; SSID also as hex (`<hex>`), as Windows writes it.

- [ ] **Step 1: Failing tests:** `profile_xml` escapes `& < > " '` and emits the hex SSID (`"Café"` → `436166C3A9`); networks are de-duplicated by SSID keeping the strongest, sorted connected-first then by signal; hidden (empty SSID) networks are dropped; signal → 4-bar glyph mapping (`0..25 → 1 bar`… boundary values pinned).
- [ ] **Step 2–4:** FAIL → implement → PASS. One `#[ignore]`d read-only test: `snapshot()` returns without panic (no scan, no connect, no radio change).
- [ ] **Step 5: Commit** `feat(win): Wi-Fi list and connect`

---

### Task 6: Bluetooth

**Files:** Create `crates/win/src/bluetooth.rs`.

**Interfaces:**
- `pub struct BtSnapshot { pub radio_on: Option<bool>, pub devices: Vec<BtDevice> }`, `pub struct BtDevice { pub id: String, pub name: String, pub connected: bool, pub audio: bool, pub battery: Option<u8> }`.
- `pub struct Bluetooth` — `open(wake)` (Radios for the Bluetooth radio + `StateChanged`; a `DeviceWatcher` on `BluetoothDevice::GetDeviceSelectorFromPairingState(true)` requesting `System.Devices.Aep.IsConnected` → wake on add/update/remove); `snapshot()`; `set_radio(bool)`; `set_connected(id, bool) -> Result<()>` — only for `audio` devices, via `BluetoothSetServiceState` enabling/disabling the A2DP sink (`0000110B-…`) and Hands-Free (`0000111E-…`) services (ruling P3); others return `Err(HandOff)` → `ms-settings:bluetooth`.
- Battery: `System.Devices.Aep.Bluetooth.Le.BatteryLevel` / the `{104EA319-…} 2` property when present; else `None`.

- [ ] **Step 1: Failing tests** (pure): device list sorting (connected first, then name, case-insensitive); audio class detection from the Bluetooth class-of-device major class 0x04; service-GUID constants.
- [ ] **Step 2–4:** FAIL → implement → PASS. `#[ignore]`d read-only snapshot test.
- [ ] **Step 5: Commit** `feat(win): Bluetooth radio and paired devices`

---

### Task 7: Quick panel

**Files:** Create `crates/core/src/quick_model.rs`, `app/vitrobar/src/quick.rs`; modify `app/vitrobar/src/app.rs` (`Hit::Quick` → own panel instead of `Job::QuickSettings`; remove the flyout-toggle use for Quick; keep `open_stock_quick_settings` as the fallback if the panel fails to open), `crates/core/src/i18n.rs`.

**Interfaces:**
- `pub enum QuickPage { Main, Wifi, Bluetooth, Output }`; `pub struct QuickInputs { pub wifi: Option<WifiSnapshot-like pure struct>, pub bt: Option<…>, pub audio: Option<…>, pub displays: Vec<…>, pub power: PowerSnapshot-like, pub airplane: Option<bool> }` — mirror the win snapshot types as plain structs in core (core has no `windows` dependency); `pub fn rows(page: QuickPage, i: &QuickInputs, lang: Lang) -> Vec<Row>`.
- Main page, in order: Tiles (Wi-Fi [more], Bluetooth [more], Airplane mode, Battery saver when a battery exists, Power mode cycle); Slider brightness per display with a DDC/internal name when > 1 display (hidden when none); Slider volume with mute glyph and trailing `Output` chevron; Separator; Buttons row: battery label (left, hidden without battery), "All settings" gear → `ms-settings:`.
- Wi-Fi page: Header "Wi-Fi" + radio toggle trailing; one Item per network (connected one checked, lock glyph when secured, signal glyph); selecting a secured unknown network expands a `Custom` password row (the app draws a single-line text box using stage 2's `text_input` model and the Start panel's text drawing) with Connect/Cancel buttons; footer button "More Wi-Fi settings" → `ms-settings:network-wifi`. Bluetooth page: same pattern with devices and "More Bluetooth settings" → `ms-settings:bluetooth`. Output page: devices with the default checked.
- Panel behaviour: opens anchored to `anchor_for(Quick)`; second click on the Quick button or Esc closes (Esc on a sub-page goes back to Main); click outside closes (Flyout deactivation); keyboard: Tab/arrows via `focus_step`, Enter/Space activates, Left/Right on a focused slider ±2 %; mouse wheel over a slider ±2 %; drag on a slider sets live (audio applied immediately; brightness throttled to one call in flight).
- Services opened when the panel opens, dropped when it closes (P4); wake handling rebuilds rows and calls `PanelView::update`.

- [ ] **Step 1: Failing tests** (`quick_model.rs`): main page with no battery has no battery saver tile and no battery label; with no Wi-Fi adapter the Wi-Fi tile is absent; airplane `None` hides the tile; brightness sliders absent with no displays and one per display with names when 2; Wi-Fi page lists networks in snapshot order with the connected one `checked`; output page checks the default; Italian labels for every tile.
- [ ] **Step 2–4:** FAIL → implement model → PASS; then the controller and wiring.
- [ ] **Step 5:** `cargo test --workspace` → PASS. Live check only if the user's instance is not running: open the panel through the debug hook (`Vitrobar.Debug.OpenQuick`, honoured only with `--debug-state`, mirroring `Vitrobar.Debug.OpenStart`), assert a visible `VitrobarFlyout` whose rect is horizontally within the quick button's span ± panel width/2 and above the bar, then close it — never click tiles. Otherwise skip and say so.
- [ ] **Step 6: Commit** `feat(quick): Vitrobar's own quick panel`

---

### Task 8: Calendar panel

**Files:** Create `crates/core/src/calendar.rs`, `app/vitrobar/src/calendar_panel.rs`; modify `app.rs` (`Hit::Clock` → own panel; stock notification centre stays reachable from the panel, ruling P2).

**Interfaces:**
- `pub struct Month { pub year: i32, pub month: u8 }`; `pub fn grid(m: Month, first_weekday: Weekday) -> [[Option<u8>; 7]; 6]`; `pub fn month_title(m: Month, lang: Lang) -> String` ("ottobre 2026" / "October 2026"); `pub fn weekday_initials(first: Weekday, lang: Lang) -> [String; 7]`; `Month::next/prev`. First weekday from the user's locale (`LOCALE_IFIRSTDAYOFWEEK`) in the app.
- Panel rows: Header (long date of today, e.g. "sabato 31 ottobre"), `Custom` month grid (drawn by the controller: 7×6 cells 40 DIP, today in accent circle, prev/next chevrons in the header trailing), Buttons: "Notifications" → `open_stock_notifications()` and closes the panel; "Date & time settings" → `ms-settings:dateandtime`.
- Same open/close behaviour as the quick panel (second click on the clock closes; Esc; click outside); wheel over the grid changes month; Home key returns to today.

- [ ] **Step 1: Failing tests:** grid for October 2026 with Monday first: first row `[None, None, None, Some(1), Some(2), Some(3), Some(4)]`, last day 31 in row 4 col 5 (Saturday); February 2028 (leap) has 29; Sunday-first shifts by one; titles/initials in en and it.
- [ ] **Step 2–4:** FAIL → implement → PASS; controller + wiring.
- [ ] **Step 5: Commit** `feat(calendar): calendar panel on the clock`

---

### Task 9: Music widget

**Files:** Create `crates/win/src/media.rs`, `crates/core/src/widgets.rs` (music part), `app/vitrobar/src/widgets.rs` (music part).

**Interfaces:**
- `media.rs`: `pub struct MediaSnapshot { pub app: String, pub title: String, pub artist: String, pub playing: bool, pub can_prev: bool, pub can_next: bool, pub thumbnail: Option<Vec<u8>> /*encoded image*/ }`; `pub struct Media` — `open(wake)` (`GlobalSystemMediaTransportControlsSessionManager::RequestAsync`, `CurrentSessionChanged`, per-session `MediaPropertiesChanged` / `PlaybackInfoChanged` → wake); `snapshot() -> Option<MediaSnapshot>`; `play_pause()`, `next()`, `prev()`. Thumbnail read only while the detail panel is open (decoded by WIC to a 64 DIP square).
- `widgets.rs` (core): `pub fn music_pill_text(s: Option<&MusicView>, max_chars: usize) -> String` ("Title — Artist", ellipsised; `None` → the widget hides, width 0); `pub const MUSIC_PILL_MAX_DIP: f32 = 180.0`; pill = play/pause glyph button + text; clicking the glyph toggles play/pause, clicking the text opens the detail panel.
- Detail panel rows: `Custom` artwork + title/artist block, Buttons (prev, play/pause, next; disabled ones hidden), Text (source app name).
- Widget on → `Media` opened at start-up; off → dropped (P4).

- [ ] **Step 1: Failing tests:** pill text ellipsis at `max_chars` keeps whole grapheme clusters (emoji title); empty artist → title only; no session → width 0 so `layout` hides the slot.
- [ ] **Step 2–4:** FAIL → implement → PASS. `#[ignore]`d read-only `Media::open` + `snapshot()` test (never sends play/pause).
- [ ] **Step 5:** Measure idle memory with the widget on (perf-check with a temp settings file `widget-music = true`) — record it; must keep the total < 40 MB.
- [ ] **Step 6: Commit** `feat(widgets): music`

---

### Task 10: Claude limits widget

**Files:** Create `crates/win/src/claude_usage.rs`; extend `crates/core/src/widgets.rs`, `app/vitrobar/src/widgets.rs`; modify `app/vitrobar/src/main.rs` (`--claude-statusline`), `settings_ui` handling of `WidgetClaude`.

**Interfaces:**
- core `widgets.rs`: `pub struct ClaudeUsage { pub five_hour: Option<Window>, pub seven_day: Option<Window>, pub at: i64 /*epoch s when saved*/ }`, `pub struct Window { pub used: f32 /*0..100*/, pub resets_at: Option<i64> }`; `pub fn parse_statusline(json: &str, now: i64) -> Option<ClaudeUsage>` (reads `rate_limits.five_hour.used_percentage`, `.resets_at` as integer seconds, integer milliseconds (> 10^12 → /1000) or RFC 3339 string; returns `None` when `rate_limits` is absent or both windows missing; tolerant of extra fields); `pub fn claude_pill_text(u: Option<&ClaudeUsage>, now: i64, lang) -> String` ("5h 42% · 7d 18%"; one window → only that; data older than 6 h → append " · " + `Key::Stale` "old"/"vecchio"; `None` → `Key::ClaudeNoData` "Claude: no data"/"Claude: nessun dato"); `pub fn resets_in(resets_at, now, lang) -> String` ("resets in 2 h 10 min" / "si azzera tra 2 h 10 min").
- `claude_usage.rs`:
  - Data file: `%LOCALAPPDATA%\Vitrobar\claude-usage.json` (`{"five_hour":{…},"seven_day":{…},"at":…}`), written atomically (temp + `MoveFileExW` replace).
  - `pub fn watch(wake) -> Result<Watcher>` — `ReadDirectoryChangesW` on the folder (overlapped, on a worker) → wake; `pub fn read() -> Option<ClaudeUsage>`.
  - `pub fn claude_settings_path() -> PathBuf` — `VITROBAR_CLAUDE_SETTINGS` if set, else `%USERPROFILE%\.claude\settings.json`.
  - `pub fn wrap_statusline(exe: &Path) -> Result<Option<String>>` — reads the Claude settings JSON (missing file → `{}`), takes the existing `statusLine.command` (if any) as the previous command, sets `statusLine` to `{"type":"command","command":"\"<exe>\" --claude-statusline"}` (keeping any other keys of the object, e.g. `padding`), writes it back preserving every other key and formatting with 2-space indentation; returns the previous command so the app stores it in `settings.claude_prev_statusline`. If the current command already is our bridge, it does nothing and returns the stored previous one (idempotent). Use `serde_json` with `preserve_order` (add the dependency to vitro-win only).
  - `pub fn unwrap_statusline(prev: Option<&str>) -> Result<()>` — only if the current command is our bridge: restore `prev` (or remove `statusLine` if there was none). If the user changed it meanwhile, leave it alone.
- `main.rs` `--claude-statusline` mode (no window, no COM, exits fast): read stdin fully (cap 1 MiB); `parse_statusline` → write the data file if `Some`; then, if `settings.claude_prev_statusline` is set, run it through `cmd /C` with the same stdin, wait at most 1 s, print its stdout; otherwise print a minimal line (model name from the JSON `model.display_name` if present, else nothing). Never prints errors to stdout; never exits non-zero.
- Settings UI: turning `WidgetClaude` on calls `wrap_statusline` and stores the previous command; off calls `unwrap_statusline`. Failure → toggle stays off and the hint shows `Key::ClaudeSetupFailed` ("Couldn't set up Claude Code's status line."/"Impossibile configurare la riga di stato di Claude Code.").
- Pill: Claude glyph + `claude_pill_text`; detail panel: two rows with a progress bar each (`Custom`), `resets_in` text, "as of HH:MM", and a Text line "Updated while you use Claude Code." / "Si aggiorna mentre usi Claude Code.".

- [ ] **Step 1: Failing tests** (core): parse a full example (both windows, epoch seconds); `resets_at` as ms and as `"2026-10-31T18:00:00Z"`; only `seven_day`; no `rate_limits` → None; garbage → None; empty → None; pill texts for both/one/stale/none in en and it; `resets_in` rounding (59 s → "1 min", 0/negative → "now"/"ora").
- [ ] **Step 2: Failing tests** (win, temp files only via `VITROBAR_CLAUDE_SETTINGS`): wrap with no file creates `{"statusLine":{...}}`; wrap with an existing command returns it and keeps other top-level keys and `statusLine.padding`; wrapping twice is idempotent; unwrap restores exactly the previous command; unwrap after the user replaced our command leaves theirs; malformed JSON → `Err`, file untouched.
- [ ] **Step 3: Failing test** (app, integration via `std::process::Command` on the built debug exe with `VITROBAR_SETTINGS` and `LOCALAPPDATA` pointed at temp dirs): piping a sample JSON writes the data file and exits 0 within 1 s; with a previous command `echo hi` the output contains `hi`; with a previous command that sleeps 5 s the bridge returns within ~1.2 s.
- [ ] **Step 4:** Implement → all PASS. **Step 5: Commit** `feat(widgets): Claude limits from Claude Code's status line`

---

### Task 11: Tray and stage-2 leftovers

**Files:** `crates/win/src/tray_host.rs`, `app/vitrobar/src/app.rs`, `app/vitrobar/src/start.rs` / catalog code, stage-1 ignored tests, IME.

- [ ] **Shell_NotifyIconGetRect:** answer the `dwData == 3` tray message with the icon's screen rect (promoted icon → its bar slot; overflowed → the chevron rect) so apps' own flyouts open next to Vitrobar. Test: pure helper `icon_rect_for(identity, layout, promoted) -> Option<PxRect>`.
- [ ] **IME indicator:** a tray-area glyph showing the current input language's short name (e.g. "ITA", "ENG") when the user has more than one keyboard layout (`GetKeyboardLayoutList` count > 1); updates on `WM_INPUTLANGCHANGE` / `EVENT_SYSTEM_FOREGROUND` (already hooked) via `GetKeyboardLayout(foreground thread)`; click opens the stock switcher with Win+Space semantics (`open input switcher` → `ms-settings:regionlanguage` fallback). Pure test: layout id → short name (`0x0410` → "ITA", `0x0409` → "ENG", unknown → first 3 letters of the locale name, uppercased).
- [ ] **Stage-2 parked:** retry the app catalog read once when it comes back empty right after sign-in (so the "Loading apps…" row stays until real data); boost-before-prefetch priority race; overflow flyout gives the previous foreground back on Esc; lengthen the wait in the stage-1 ignored stock-taskbar tests (`restore_twice_is_safe`, `show_works_without_a_record`) so they pass in a full ignored run.
- [ ] `cargo test --workspace` → PASS. **Commit** `fix: tray rects, IME indicator, stage-2 leftovers`

---

### Task 12: Docs, checks, perf

- [ ] `docs/superpowers/plans/2026-10-31-stage3-followups.md`: rulings P1–P4 (with the Anthropic docs quote and date for P1), deferred items, measured numbers.
- [ ] `docs/release-checklist.md` (keep its line endings): a "Quick panel" section (opens above the Quick button in each shape and on both monitors and with the bar at the top; Wi-Fi list/connect to a known network/password network; Bluetooth on/off and audio device connect; volume slider + output switch; brightness on laptop panel and on an external DDC monitor; power mode; Esc/second click/click outside close; sub-page Esc goes back), "Calendar" (today, month navigation, Notifications button), "Widgets" (enable Music: pill appears with the playing track, play/pause from the pill, detail panel; enable Claude limits with and without an existing Claude Code status line: the pill fills after the next Claude Code reply, disabling restores the previous status line exactly).
- [ ] README feature list: quick panel, calendar, widgets (one line each).
- [ ] Perf (only when the user's instance is not running — otherwise record as pending): `scripts/perf-check.ps1`, `-StartPanel`, and a run with both widgets on; all `"pass":true`.
- [ ] **Commit** `docs: stage 3 followups, checks and perf`
