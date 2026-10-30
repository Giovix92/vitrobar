# Vitrobar Stage 2 — Start menu, Windows key, settings window — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the stock Start menu with Vitrobar's own glass Start panel (panel that turns into search when you type, optional full-screen app grid), make the Windows key and Win+1…9 / Win+T / Win+B drive Vitrobar, and replace the right-click settings submenus with a glass settings window whose changes preview live on the real bar.

**Architecture:** Pure logic (text editing, quick math, search ranking, Start/settings geometry, keyboard-hook decisions) goes in `vitro-core` with TDD. Windows data sources (all-apps catalog, recent files, Windows Search queries, power actions, account name, clipboard) go in `vitro-win`, each blocking source on a worker thread that reports through the existing `WakeQueue` + payload-free wake-up pattern. The Start panel and settings window are new **activatable** glass windows built in `vitro-render` from the same Composition + `Painter` primitives as the bar. A low-level keyboard hook runs on its **own dedicated thread** so a busy UI thread can never lag the user's typing.

**Tech Stack:** as stage 1 (Rust stable MSVC, `windows` 0.62.2, Windows.UI.Composition, Direct2D/DirectWrite on WARP).

**Spec:** `docs/superpowers/specs/2026-10-30-vitrobar-design.md` (§5.3 Start menu, §5.5 keyboard, §4 settings window, §3 budgets). Stage-1 code map: `.superpowers/stage2-interface-map.md` (read the sections a task names). Stage-1 follow-ups: `docs/superpowers/plans/2026-10-30-stage1-followups.md`.

## Global Constraints

- All stage-1 Global Constraints still apply (see `docs/superpowers/plans/2026-10-30-stage1-bar.md`): Windows 11 x64+ARM64; RAM < 40 MB total when idle with panels closed (< 60 MB transient with a panel open, back under 40 MB after closing); idle CPU 0 %; no polling; animations 150–250 ms and "Animation effects off" respected; never modify Windows permanently; never inject into other processes; no telemetry; asInvoker; PerMonitorV2; en + it strings via `vitro_core::i18n`; content drawn with WARP (user decision D1).
- The keyboard hook never stores, logs or transmits key data beyond the single decision it is making.
- Synthetic input is limited to: the stock-Start fallback (Ctrl+Esc, existing) and the mask key `VK 0xE8` (unassigned) described in Task 8. Every injected event is ignored by our own hook (`LLKHF_INJECTED`).
- **Desktop safety (binding for every live test, as in stage 1):** only touch windows/processes the test started; synthetic input only into Vitrobar windows or test windows after a `WindowFromPoint`/foreground check; never Notepad; use `VITROBAR_SETTINGS` pointing at a temp file pre-filled with `first-run-done = true` and `start-with-windows = false`; **never execute a power action** (lock/sleep/restart/shut down/sign out/switch user) — tests use the dry-run hook; never install a keyboard hook that swallows keys in an automated test except the `#[ignore]`d hook test of Task 8, which uninstalls itself in a drop guard; always verify the shell state at the end.
- Start panel opens (Win key up → panel visible) in < 150 ms; it adds no idle cost while closed.

## Review Focus

1. **The Vitrobar UI thread is busy or hung** → the user's typing in other apps must not lag; the hook lives on its own thread and decides from atomics only (Task 8).
2. **Windows-key combinations** (Win+E, Win+R, Win+D, Win+Shift+S, Win+Tab, Win+arrows, Win+L) → behave exactly as stock; only a lone Win tap opens Vitrobar Start; the Win key never stays logically pressed (Task 8).
3. **Odd search input** — emoji, 1 000 characters, only spaces, pasted multi-line text, `1/0`, `2^9999`, `((`, Italian decimal comma → no panic, sensible results, the panel stays responsive (Tasks 1, 2, 6).
4. **Panel open during a monitor/DPI change, position switch to Top, Explorer restart or fullscreen app** → the panel closes cleanly or re-places itself; no orphan window, no stuck keyboard focus (Tasks 5, 6).
5. **Corrupt or old settings file** with an unknown/invalid value → only that field falls back to its default; nothing else is lost (Task 3).

---

## File structure

```
crates/core/src/
  text_input.rs     single-line edit model (caret, insert, delete, word ops, paste sanitising)
  calc.rs           quick-math evaluator
  search.rs         ranking of apps / settings pages / files + settings-page table
  start_layout.rs   Start panel + full-screen grid geometry
  settings_ui.rs    settings window control model (sections, controls, focus, values ↔ Settings)
  key_hook.rs       pure hook state machine (Win tap vs combo, Win+n, bar-focus mode)
  settings.rs       (modify) new fields, per-field tolerant load
  i18n.rs           (modify) new keys
crates/win/src/
  catalog.rs        all-apps enumeration (shell:AppsFolder) + recent files
  file_search.rs    Windows Search query worker
  power.rs          lock / sleep / restart / shut down / sign out / switch user (+ dry run)
  account.rs        display name
  clipboard.rs      set text
  launch.rs         (modify) pub shell_open
  key_hook.rs       LL keyboard hook thread + mask key
crates/render/src/
  panel.rs          activatable glass window placed at an explicit rect (Start panel, settings window)
  surfaces.rs       (modify) left-aligned text, caret position, wrapped text
  start_view.rs     draws the Start panel / grid / results from StartLayout + data
  settings_view.rs  draws the settings window from settings_ui model
app/vitrobar/src/
  start.rs          Start panel controller (open/close, input, search, launch)
  settings_window.rs settings window controller
  keys.rs           glue: hook events → app actions, bar focus mode
  app.rs, notify.rs, menus.rs, launcher.rs (modify)
```

---

### Task 1: Text input model and quick math (core)

**Files:** Create `crates/core/src/text_input.rs`, `crates/core/src/calc.rs` (unit tests inline).

**Interfaces — Produces:**
```rust
// text_input.rs — char indices are grapheme-agnostic UTF-16-free: indices into `chars()` (Vec<char>)
pub struct TextInput { /* text: Vec<char>, caret: usize */ }
pub const MAX_INPUT_CHARS: usize = 256;
impl TextInput {
    pub fn new() -> Self; pub fn text(&self) -> String; pub fn caret(&self) -> usize; pub fn is_empty(&self) -> bool;
    pub fn insert(&mut self, s: &str);         // sanitises: newlines/tabs → space, control chars dropped, truncates at MAX_INPUT_CHARS
    pub fn backspace(&mut self); pub fn delete(&mut self);
    pub fn backspace_word(&mut self);          // Ctrl+Backspace
    pub fn left(&mut self); pub fn right(&mut self); pub fn word_left(&mut self); pub fn word_right(&mut self);
    pub fn home(&mut self); pub fn end(&mut self); pub fn clear(&mut self);
}
// calc.rs
pub fn looks_like_math(s: &str) -> bool;       // contains a digit and only [0-9 .,+-*/x×÷^%() ] after trim; at least one operator
pub fn eval(s: &str) -> Option<f64>;           // + - * / ^ %, unary minus, parentheses, '×' and 'x' = *, '÷' = /, ',' or '.' decimal
pub fn format_result(v: f64, lang: Lang) -> String; // up to 10 significant digits, trailing zeros trimmed, it → ',' decimal; NaN/inf → None upstream
```
Rules: `eval` returns `None` for division by zero, NaN, infinities, unbalanced parentheses, empty input, more than 64 tokens or nesting deeper than 32. `%` is modulo between two operands (not percent). `^` right-associative.

- [ ] **Step 1: Failing tests**
```rust
#[test] fn insert_and_caret() { let mut t = TextInput::new(); t.insert("ciao"); t.left(); t.insert("X"); assert_eq!((t.text().as_str(), t.caret()), ("ciaXo", 4)); }
#[test] fn newlines_become_spaces_and_controls_drop() { let mut t = TextInput::new(); t.insert("a\nb\tc\u{7}"); assert_eq!(t.text(), "a b c"); }
#[test] fn length_capped() { let mut t = TextInput::new(); t.insert(&"x".repeat(1000)); assert_eq!(t.text().chars().count(), MAX_INPUT_CHARS); }
#[test] fn emoji_is_one_char_step() { let mut t = TextInput::new(); t.insert("a😀"); t.backspace(); assert_eq!(t.text(), "a"); }
#[test] fn word_ops() { let mut t = TextInput::new(); t.insert("open file now"); t.backspace_word(); assert_eq!(t.text(), "open file "); t.home(); t.word_right(); assert_eq!(t.caret(), 5); }
#[test] fn math_basics() { assert_eq!(eval("2+2*3"), Some(8.0)); assert_eq!(eval("(1+2)^2"), Some(9.0)); assert_eq!(eval("2^3^2"), Some(512.0)); assert_eq!(eval("-3*-3"), Some(9.0)); assert_eq!(eval("7 % 3"), Some(1.0)); assert_eq!(eval("1,5×2"), Some(3.0)); }
#[test] fn math_rejects() { for s in ["1/0", "2^9999", "((", "", "abc", "1++", &"(".repeat(40)] { assert_eq!(eval(s), None, "{s}"); } }
#[test] fn looks_like_math_needs_operator() { assert!(looks_like_math("12*4")); assert!(!looks_like_math("2024")); assert!(!looks_like_math("chrome")); }
#[test] fn formats_per_language() { assert_eq!(format_result(2.5, Lang::It), "2,5"); assert_eq!(format_result(1.0/3.0, Lang::En), "0.3333333333"); assert_eq!(format_result(8.0, Lang::En), "8"); }
```
- [ ] **Step 2: Run** `cargo test -p vitro-core text_input calc` → FAIL. **Step 3: Implement** (recursive-descent parser, no `unsafe`, no new deps). **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** `feat(core): text input model and quick math`

---

### Task 2: Search ranking and Settings pages (core)

**Files:** Create `crates/core/src/search.rs`.

**Interfaces — Consumes:** `Pin` (settings.rs), `Lang`, `tr`, `Key` (i18n.rs), `calc::{looks_like_math, eval}`.
**Produces:**
```rust
pub struct FileHit { pub name: String, pub path: String }
pub struct SettingsPage { pub key: Key, pub uri: &'static str, pub keywords: &'static [&'static str] }
pub const SETTINGS_PAGES: &[SettingsPage];
pub enum ResultKind { Math(f64), App(usize /*index into apps*/), Setting(usize /*index into SETTINGS_PAGES*/), File(usize /*index into files*/) }
pub struct SearchResults { pub items: Vec<ResultKind> }   // display order; items[0] is what Enter runs
pub fn score(query: &str, candidate: &str) -> Option<u32>;  // higher is better; None = no match
pub fn search(query: &str, apps: &[Pin], files: &[FileHit], lang: Lang) -> SearchResults;
```
Ranking: normalise both sides to lowercase with accents folded (à→a, è/é→e, ì→i, ò→o, ù→u, ç→c). `score`: exact = 1000; whole-name prefix = 800; prefix of any word (split on space, '-', '_', '.') = 600 − word index; acronym of word initials (e.g. "vsc" → "Visual Studio Code") = 500; substring = 300; in-order subsequence = 100 − gaps (min 1); else None. Ties: shorter candidate first, then alphabetical. Settings pages match on their translated title (current `lang`) and their English keywords. Order of `items`: Math (if `looks_like_math` and `eval` is Some) first; then up to 6 apps; then up to 3 settings pages; then up to 6 files. Empty / whitespace-only query → empty results.

`SETTINGS_PAGES` (key → uri, keywords): Display `ms-settings:display` ["screen","resolution","scale","monitor"]; Sound `ms-settings:sound` ["audio","volume","speaker","microphone"]; Bluetooth `ms-settings:bluetooth` ["devices","pair"]; WiFi `ms-settings:network-wifi` ["wireless","network","internet"]; Personalization `ms-settings:personalization` ["theme","wallpaper","background","colors"]; Apps `ms-settings:appsfeatures` ["uninstall","programs","installed"]; WindowsUpdate `ms-settings:windowsupdate` ["update","upgrade"]; Power `ms-settings:powersleep` ["battery","sleep","energy"]; Mouse `ms-settings:mousetouchpad` ["touchpad","cursor","pointer"]; Keyboard `ms-settings:typing` ["typing","input"]; Notifications `ms-settings:notifications` ["alerts","focus"]; DefaultApps `ms-settings:defaultapps` ["browser","open with"]; Storage `ms-settings:storagesense` ["disk","space","cleanup"]; Privacy `ms-settings:privacy` ["permissions","camera","location"]; DateTime `ms-settings:dateandtime` ["clock","time zone"]; Language `ms-settings:regionlanguage` ["region","locale"]; Accounts `ms-settings:yourinfo` ["user","profile","sign-in"]; Printers `ms-settings:printers` ["scanner","print"]; NightLight `ms-settings:nightlight` ["blue light","warm"]. (Keys added in Task 3.)

- [ ] **Step 1: Failing tests**
```rust
#[test] fn prefix_beats_substring() { assert!(score("chr", "Google Chrome").unwrap() < score("chr", "Chrome").unwrap()); }
#[test] fn acronym() { assert!(score("vsc", "Visual Studio Code").is_some()); }
#[test] fn accents_folded() { assert!(score("impostazioni", "Impostazióni").is_some()); assert!(score("e", "è").is_some()); }
#[test] fn math_first() { let r = search("2+2", &[pin("2+2 Game")], &[], Lang::En); assert_eq!(r.items[0], ResultKind::Math(4.0)); }
#[test] fn caps_per_kind() { let apps: Vec<Pin> = (0..20).map(|i| pin(&format!("App {i}"))).collect(); let r = search("app", &apps, &[], Lang::En);
    assert_eq!(r.items.iter().filter(|k| matches!(k, ResultKind::App(_))).count(), 6); }
#[test] fn settings_by_italian_title_and_english_keyword() { let it = search("suono", &[], &[], Lang::It); let en = search("volume", &[], &[], Lang::It);
    assert!(matches!(it.items[0], ResultKind::Setting(_))); assert!(matches!(en.items[0], ResultKind::Setting(_))); }
#[test] fn blank_query_is_empty() { assert!(search("   ", &[pin("a")], &[], Lang::En).items.is_empty()); }
#[test] fn long_query_no_panic() { let q = "a".repeat(256); let _ = search(&q, &[pin("a")], &[], Lang::En); }
```
- [ ] **Step 2–4:** run → FAIL, implement, run → PASS (`cargo test -p vitro-core search`).
- [ ] **Step 5: Commit** `feat(core): search ranking and settings pages`

---

### Task 3: Settings fields, tolerant load, i18n keys, Start/settings geometry (core)

**Files:** Modify `crates/core/src/settings.rs`, `crates/core/src/i18n.rs`; create `crates/core/src/start_layout.rs`, `crates/core/src/settings_ui.rs`.

**Interfaces — Produces:**
```rust
// settings.rs — new enums + fields (serde default keeps old files loadable)
setting_enum! StartStyle { Panel, FullScreen }          // default Panel
// Settings gains:
pub start_style: StartStyle,
pub win_key_opens_start: bool,      // default true
pub start_pins: Vec<Pin>,           // default empty → filled from `pins` on first open (see Task 6)
// Tolerant load: a value that fails to parse for ONE field resets only that field (log nothing); unknown keys ignored.
pub fn load_with_status(path: &Path) -> (Settings, LoadStatus)   // unchanged signature; LoadStatus::Corrupt only when the file is not TOML at all
// The .bad copy is written at most once per distinct corrupt content (skip if an identical .bad-* exists) — fixes the stage-1 parked clutter item.

// start_layout.rs
pub const PANEL_W_DIP: f32 = 640.0; pub const PANEL_H_DIP: f32 = 720.0; pub const PIN_COLS: usize = 6; pub const PIN_ROWS: usize = 3;
pub const RECENT_ROWS: usize = 4; pub const RESULT_ROW_H_DIP: f32 = 44.0; pub const GRID_CELL_DIP: f32 = 96.0;
pub struct StartLayout { pub panel: Rect, pub search: Rect, pub pins_title: Rect, pub all_apps_button: Rect, pub pins: Vec<Rect>,
    pub recent_title: Rect, pub recent: Vec<Rect>, pub footer: Rect, pub account: Rect, pub power: Rect, pub results: Vec<Rect>, pub list_view: Rect }
pub fn panel_layout(panel: Rect) -> StartLayout;                       // panel = placed rect in DIP
pub struct GridLayout { pub search: Rect, pub cells: Vec<Rect>, pub cols: usize, pub rows_visible: usize }
pub fn grid_layout(work_area: Rect, app_count: usize) -> GridLayout;   // full-screen mode, cells GRID_CELL_DIP, centred, search 480×40 at top
pub fn place_panel(anchor: Rect, position: Position, work_area: Rect) -> Rect; // centred on anchor, 12 DIP gap, clamped inside work area; shrinks to fit (min 400×480)

// settings_ui.rs
pub enum Section { Appearance, Behaviour, Language, About }
pub enum ControlKind { Choice { options: Vec<Key>, selected: usize }, Toggle(bool), Swatches { colors: Vec<Option<u32>>, selected: usize }, Link(&'static str), Text(Key) }
pub enum ControlId { Shape, Material, Hover, IconSize, Position, Theme, Accent, StartStyle, WinKey, StartWithWindows, ReserveFloating, StockTaskbar, Language, AboutVersion, AboutLicense, AboutSource }
pub struct Control { pub id: ControlId, pub section: Section, pub label: Key, pub kind: ControlKind, pub rect: Rect }
pub struct SettingsModel { pub controls: Vec<Control>, pub focus: usize }
pub fn build_model(s: &Settings, width_dip: f32) -> SettingsModel;          // vertical stack, 24 DIP padding, rows 48 DIP, section headers 36 DIP
pub fn apply(s: &mut Settings, id: ControlId, choice: usize) -> bool;        // returns true if changed; Toggle uses 0/1
pub fn focus_next(m: &mut SettingsModel); pub fn focus_prev(m: &mut SettingsModel); // skip Text/Link? Links focusable, Text not
pub const ACCENT_SWATCHES: &[Option<u32>] = &[None /*system*/, Some(0x0078D4), Some(0x0E7C86), Some(0x7A5AF8), Some(0xC239B3), Some(0xE81123), Some(0xF7630C), Some(0x10893E), Some(0x5D5A58)];
```
Controls and options (order fixed): Appearance — Shape {Pill, Islands, Full}; Material {Frosted, Smoked, SolidBlack, SolidWhite} (no Liquid until stage 4); Hover {Plate, Lift, Magnify, Sweep}; IconSize {Small, Medium, Large}; Position {Bottom, Top}; Theme {System, Light, Dark}; Accent swatches. Behaviour — StartStyle {Panel, FullScreen}; WinKey toggle; StartWithWindows toggle; ReserveFloating toggle; StockTaskbar {Hidden, AutoHide}. Language {System, English, Italiano}. About — version text, GPL-3.0 link (`https://www.gnu.org/licenses/gpl-3.0.html`), source link (`https://github.com/vitrobar/vitrobar`, placeholder until the repo exists — update at push).

New i18n keys (en / it) — add to `Key`, `ALL_KEYS`, `tr`:
Start "Start"/"Start"; SearchPlaceholder "Type to search"/"Digita per cercare"; Pinned "Pinned"/"Aggiunte"; AllApps "All apps"/"Tutte le app"; Back "Back"/"Indietro"; Recent "Recent"/"Recenti"; NoRecent "No recent files"/"Nessun file recente"; Apps "Apps"/"App"; SettingsPages "Settings"/"Impostazioni"; Files "Files"/"File"; Calculator "Calculator"/"Calcolatrice"; CopyResult "Press Enter to copy"/"Premi Invio per copiare"; NoResults "No results"/"Nessun risultato"; Lock "Lock"/"Blocca"; Sleep "Sleep"/"Sospendi"; Restart "Restart"/"Riavvia"; ShutDown "Shut down"/"Arresta il sistema"; SignOut "Sign out"/"Disconnetti"; SwitchUser "Switch user"/"Cambia utente"; PinToStart "Pin to Start"/"Aggiungi a Start"; UnpinFromStart "Unpin from Start"/"Rimuovi da Start"; OpenFileLocation "Open file location"/"Apri percorso file"; VitrobarSettings "Vitrobar settings"/"Impostazioni di Vitrobar"; SettingsEllipsis "Settings…"/"Impostazioni…"; Appearance "Appearance"/"Aspetto"; Behaviour "Behaviour"/"Comportamento"; Language "Language"/"Lingua"; About "About"/"Informazioni"; Theme "Theme"/"Tema"; ThemeSystem "System"/"Sistema"; ThemeLight "Light"/"Chiaro"; ThemeDark "Dark"/"Scuro"; Accent "Accent colour"/"Colore principale"; StartStyleKey "Start menu"/"Menu Start"; StartPanel "Panel"/"Pannello"; StartFullScreen "Full screen"/"Schermo intero"; WinKeyOpensStart "Windows key opens Vitrobar Start"/"Il tasto Windows apre lo Start di Vitrobar"; ReserveSpace "Keep maximized windows above the bar"/"Le finestre ingrandite restano sopra la barra"; StockTaskbarKey "Windows taskbar while Vitrobar runs"/"Barra di Windows mentre Vitrobar è attivo"; StockHidden "Hidden"/"Nascosta"; StockAutoHide "Auto-hide"/"Nascondi automaticamente"; LanguageSystem "Same as Windows"/"Come Windows"; Version "Version"/"Versione"; License "License: GPL-3.0"/"Licenza: GPL-3.0"; SourceCode "Source code"/"Codice sorgente"; and one key per SETTINGS_PAGES entry: Display "Display"/"Schermo"; Sound "Sound"/"Suono"; Bluetooth "Bluetooth & devices"/"Bluetooth e dispositivi"; WiFi "Wi-Fi"/"Wi-Fi"; Personalization "Personalization"/"Personalizzazione"; AppsSettings "Installed apps"/"App installate"; WindowsUpdate "Windows Update"/"Windows Update"; Power "Power & battery"/"Alimentazione e batteria"; Mouse "Mouse"/"Mouse"; Keyboard "Typing"/"Digitazione"; Notifications "Notifications"/"Notifiche"; DefaultApps "Default apps"/"App predefinite"; Storage "Storage"/"Archiviazione"; Privacy "Privacy & security"/"Privacy e sicurezza"; DateTime "Date & time"/"Data e ora"; Region "Language & region"/"Lingua e area geografica"; Accounts "Your info"/"Le tue informazioni"; Printers "Printers & scanners"/"Stampanti e scanner"; NightLight "Night light"/"Luce notturna".

- [ ] **Step 1: Failing tests**
```rust
#[test] fn old_file_without_new_fields_loads() { /* write stage-1 style toml → start_style Panel, win_key_opens_start true, start_pins empty */ }
#[test] fn one_bad_value_resets_only_that_field() { /* "shape = \"hexagon\"\nposition = \"top\"" → shape Pill (default), position Top, status Loaded */ }
#[test] fn identical_corruption_writes_one_bad_copy() { /* load a non-TOML file twice → exactly one .bad-* file */ }
#[test] fn panel_layout_fits_and_has_18_pins() { let l = panel_layout(Rect{x:0.,y:0.,w:PANEL_W_DIP,h:PANEL_H_DIP}); assert_eq!(l.pins.len(), PIN_COLS*PIN_ROWS); assert_eq!(l.recent.len(), RECENT_ROWS); assert_all_inside(&l); }
#[test] fn place_panel_clamps_and_shrinks() { let r = place_panel(start_rect_at_left(), Position::Bottom, Rect{x:0.,y:0.,w:1000.,h:600.}); assert!(r.x >= 0.0 && r.bottom() <= 600.0 && r.h >= 480.0); }
#[test] fn place_panel_top_opens_below() { let a = Rect{x:900.,y:8.,w:40.,h:40.}; assert!(place_panel(a, Position::Top, wa()).y >= a.bottom()); }
#[test] fn grid_layout_no_overlap() { let g = grid_layout(Rect{x:0.,y:0.,w:1920.,h:1032.}, 120); assert_no_overlap(&g.cells); }
#[test] fn settings_model_reflects_values_and_apply_changes() { let mut s = Settings::default(); assert!(apply(&mut s, ControlId::Shape, 2)); assert_eq!(s.shape, Shape::Full); assert!(!apply(&mut s, ControlId::Shape, 2)); }
#[test] fn material_choices_exclude_liquid() { /* Material control options len 4 */ }
#[test] fn focus_cycles_and_skips_text() { /* focus_next from last wraps to first focusable */ }
#[test] fn every_key_translated() { /* existing test still covers ALL_KEYS incl. new ones; Italian differs from English for at least the keys whose strings differ above */ }
```
- [ ] **Step 2–4:** run → FAIL, implement (tolerant load: deserialize into `toml::Table` first, then each known field via `T::deserialize` with fallback to `Default`), run → PASS.
- [ ] **Step 5: Commit** `feat(core): stage-2 settings, tolerant load, start and settings geometry, strings`

---

### Task 4: Windows data sources (win)

**Files:** Create `crates/win/src/{catalog,file_search,power,account,clipboard}.rs`; modify `crates/win/src/launch.rs` (`pub fn shell_open`), `crates/win/src/pins_import.rs` (make `pin_from_link`, `start_menu_folders`, `links_under`, `packaged_app_name` `pub(crate)`), `crates/win/Cargo.toml` (features `Win32_System_Power`, `Win32_System_Shutdown`, `Win32_Security_Authentication_Identity`, `Win32_System_Memory`, `Win32_System_Ole` as needed). Tests `crates/win/tests/{catalog,power,clipboard}.rs`.

**Interfaces — Produces:**
```rust
// catalog.rs (blocking; COM STA on the calling thread)
pub fn list_all_apps() -> Vec<Pin>;            // shell:AppsFolder via IShellItem + IEnumShellItems: name = SIGDN_NORMALDISPLAY, aumid = SIGDN_PARSINGNAME;
                                               // skip uninstallers/"Uninstall*"/"*Readme*"/URLs; dedupe by aumid; sorted case-insensitive with accents folded (reuse search normalisation)
pub struct RecentFile { pub name: String, pub path: String }
pub fn recent_files(max: usize) -> Vec<RecentFile>; // %APPDATA%\Microsoft\Windows\Recent\*.lnk by mtime desc, resolve via read_link, keep existing files only (not folders)
// file_search.rs — worker thread "vitrobar-search", COM STA
pub struct FileSearch; impl FileSearch {
    pub fn start(notify: HWND, wake_msg: u32) -> Result<Self>;
    pub fn query(&self, generation: u64, text: String);   // debounced 120 ms on the worker; newer generation cancels older
    pub fn drain(&self) -> Option<(u64, Vec<FileHit>)>;   // latest finished result
}
// Implementation: enumerate `search-ms:query=<escaped>&crumb=location:%USERPROFILE%` as a shell folder (SHCreateItemFromParsingName → BindToHandler BHID_EnumItems), stop after 6 file items or 1 s; folders skipped. Uses the Windows Search index; never indexes itself.
// power.rs
pub enum PowerAction { Lock, Sleep, Restart, ShutDown, SignOut, SwitchUser }
pub fn perform(a: PowerAction) -> Result<()>;  // Lock: LockWorkStation; Sleep: SetSuspendState(false,false,false); Restart/ShutDown: ExitWindowsEx(EWX_REBOOT|EWX_POWEROFF, SHTDN_REASON_FLAG_PLANNED) after enabling SeShutdownPrivilege; SignOut: ExitWindowsEx(EWX_LOGOFF); SwitchUser: WTSDisconnectSession(WTS_CURRENT_SERVER_HANDLE, WTS_CURRENT_SESSION, false)
pub fn set_dry_run(on: bool);                  // when on, perform() records the action in a static and returns Ok without acting
pub fn last_dry_run() -> Option<PowerAction>;
// account.rs
pub fn display_name() -> String;               // GetUserNameExW(NameDisplay) → fallback GetUserNameW
// clipboard.rs
pub fn set_text(owner: HWND, text: &str) -> Result<()>;
```
- [ ] **Step 1: Failing tests** (`catalog.rs`): `all_apps_contains_settings_and_explorer` (aumids `windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft.windows.immersivecontrolpanel` and `Microsoft.Windows.Explorer` present, no duplicates, sorted); `recent_files_are_existing_files` (every path exists and is not a directory; `len <= max`). `power.rs`: `dry_run_records_and_does_nothing` (set_dry_run(true); perform(ShutDown) is Ok; last_dry_run == Some(ShutDown)) — **this is the only power test; never call perform without dry run in tests.** `clipboard.rs`: `set_text_roundtrip` (`#[ignore = "overwrites the user's clipboard"]`, saves and restores the previous clipboard text). `file_search`: unit test of the URI escaping helper `search_uri(text) -> String` (spaces, `&`, `%`, quotes, non-ASCII).
- [ ] **Step 2–4:** run → FAIL, implement, run → PASS (`cargo test -p vitro-win --test catalog --test power`, plus unit tests).
- [ ] **Step 5: Commit** `feat(win): app catalog, recent files, file search, power actions, account, clipboard`

---

### Task 5: Activatable glass panel and text drawing (render)

**Files:** Create `crates/render/src/panel.rs`; modify `crates/render/src/surfaces.rs`; tests `crates/render/tests/panel.rs`.

**Interfaces — Produces:**
```rust
pub const PANEL_CLASS: &str = "VitrobarPanel";
pub struct Panel; impl Panel {
    pub fn show(comp: &Comp, rect_px: PxRect, dpi: u32, style: &MaterialStyle, radius_dip: f32, title: &str, app_window: bool) -> Result<Self>;
    // WS_POPUP, ex TOOLWINDOW|TOPMOST|NOREDIRECTIONBITMAP (NOT NOACTIVATE; app_window=true uses WS_EX_APPWINDOW instead of TOOLWINDOW and no TOPMOST, for the settings window)
    pub fn hwnd(&self) -> HWND; pub fn rect_px(&self) -> PxRect; pub fn scale(&self) -> f32; pub fn root(&self) -> &ContainerVisual;
    pub fn activate(&self) -> bool;          // SetForegroundWindow; returns whether it is now foreground
    pub fn move_to(&mut self, comp: &Comp, rect_px: PxRect, dpi: u32, style: &MaterialStyle) -> Result<()>;
}
pub fn take_panel_deactivated() -> Vec<HWND>;   // like flyout::take_deactivated (WM_ACTIVATE WA_INACTIVE)
// surfaces.rs additions
impl Painter {
    pub fn text_left(&self, s: &str, kind: FontKind, size_px: f32, color: Rgba, w_px: u32, h_px: u32) -> Result<CompositionDrawingSurface>; // left-aligned, vertical centre, ellipsis
    pub fn caret_x(&self, s: &str, kind: FontKind, size_px: f32, char_index: usize) -> Result<f32>;  // IDWriteTextLayout::HitTestTextPosition, char index → UTF-16 index
    pub fn text_wrapped(&self, s: &str, kind: FontKind, size_px: f32, color: Rgba, w_px: u32, max_lines: u32) -> Result<(CompositionDrawingSurface, u32 /*h_px*/)>;
}
```
Text layouts use the user's locale (`user_ui_language_tag()`), not the hard-coded "en-us" (fix in the private `layout()`).

- [ ] **Step 1: Failing tests** (`tests/panel.rs`, real DWM, own windows only): `panel_shows_at_rect_and_activates` (rect matches; `activate()` true when called right after creating it from the test's foreground thread — if the environment denies foreground, assert the window exists and document); `deactivation_is_reported` (activate a second test window → `take_panel_deactivated()` contains the panel); `caret_x_monotonic` (x for indices 0..=n strictly increasing for "Vitrobar", and for "a😀b" index 2 > index 1); `text_left_draws` (surface size matches; no error for 1 000-char string).
- [ ] **Step 2–4:** run → FAIL, implement, run → PASS (`cargo test -p vitro-render --test panel`).
- [ ] **Step 5: Commit** `feat(render): activatable glass panel and text layout helpers`

---

### Task 6: Start panel — panel mode, search, All apps, launching (app + render)

**Files:** Create `crates/render/src/start_view.rs`, `app/vitrobar/src/start.rs`; modify `app/vitrobar/src/app.rs`, `notify.rs`, `launcher.rs`, `menus.rs`, `icon_loader.rs` keep-predicate; tests: unit tests in `start.rs` for the controller state machine; checklist section "Start menu" in `docs/release-checklist.md`.

**Interfaces — Consumes:** Tasks 1–5; `IconLoader::get(&IconSrc::Aumid|Path, px)`; `Launcher::launch(Job)`; `click_action`; `reset_interaction`.
**Produces:**
```rust
// launcher.rs
pub enum Job { Pin(Pin), QuickSettings, Notifications, Open(String) /*file path or ms-settings: uri via shell_open*/, Power(PowerAction) }
// app/start.rs
pub enum StartMode { Home, AllApps, Search }
pub enum StartInput { Char(char), Key(u32 /*VK*/ , bool /*ctrl*/), Paste(String), Wheel(i32), Move((i32,i32)), Click((i32,i32)), RightClick((i32,i32)) }
pub enum StartAction { None, Close, Launch(Pin), Open(String), Power(PowerAction), CopyMath(String), Menu((i32,i32), usize /*item*/), Redraw }
pub struct StartState { pub mode: StartMode, pub input: TextInput, pub selected: usize, pub scroll: f32, /* … */ }
impl StartState { pub fn handle(&mut self, ev: StartInput, ctx: &StartCtx) -> StartAction; }  // pure: StartCtx holds layout rects (px), item counts, results
// app.rs
fn toggle_start(&mut self, k: usize /*monitor index*/, dirty: &mut Dirty);   // Start button click and Win key
fn close_start(&mut self);                                                  // also called from reset_interaction
```
Behaviour (all from spec §5.3 + decisions below):
- Opens on the monitor whose Start button was clicked, or (Win key) the monitor with the foreground window. Placement `place_panel(layout.start, position, work_area)`; panel radius 12 DIP; current material/theme. Opening closes strip/overflow/tooltip. A second click / Win tap / Esc / deactivation / launching anything closes it. Opening must not take > 150 ms: draw from cached data first; refresh the app catalog and recent files on the launcher/search worker in the background (catalog cached in `App`, refreshed at most once per open and only if older than 60 s).
- **Home:** search box (placeholder text, non-blinking caret — no timers), "Pinned" title + "All apps ›" button, 6×3 grid of `start_pins` (first open with empty `start_pins` copies the bar `pins`, then saves), "Recent" with 4 recent files (icon + name + folder), footer with account initial in an accent circle + display name (click → `ms-settings:yourinfo`) and power button (click → native popup menu: Lock, Sleep, Restart, Shut down, Sign out, Switch user — via the existing `menus.rs` `Menu` helper; selection → `Job::Power`).
- **Typing** any printable char while Home or AllApps is shown switches to **Search**: results list (`search()`), rows `RESULT_ROW_H_DIP` with icon, title, kind subtitle (App / Settings / File path / Calculator + "Press Enter to copy"); ↑/↓ move selection, Enter runs selected (Math → clipboard), Esc clears the query (second Esc closes), Ctrl+V pastes (sanitised by `TextInput`), Ctrl+Backspace deletes a word. Files arrive asynchronously from `FileSearch` (generation-checked) and append without moving the selection if it is above them. Empty query returns to Home.
- **All apps:** alphabetical list of `list_all_apps()` with letter headers, mouse-wheel scrolling (container offset, no surface redraw per frame), "‹ Back".
- Right-click on a pinned or listed app → menu Pin to Start / Unpin from Start, Pin to bar / Unpin from bar, Open file location (for `link`/`path` pins). Changes save immediately.
- Text input routing: in the run loop, for messages whose `hwnd` is the Start panel, `WM_KEYDOWN` → `StartInput::Key` (then `TranslateMessage` so `WM_CHAR` follows), `WM_CHAR` (≥ 0x20, not 0x7F) → `Char`, `WM_MOUSEWHEEL` → `Wheel`.
- Icons: panel icons are requested at 32 px via the icon loader; the `trim` keep-predicate keeps them while the panel is open and drops them after it closes.
- After closing: all panel surfaces/visuals dropped; memory returns under 40 MB (measure in Step 4).
- `dump_state` gains `"start": {"open": bool, "mode": "...", "query": <len only, never the text>, "results": n}`.

- [ ] **Step 1: Failing unit tests** in `start.rs` for `StartState::handle`: typing switches Home→Search; Esc clears then closes; ↑/↓ clamp; Enter on Math → `CopyMath("4")`; Enter on empty results → None; Wheel in AllApps changes scroll and clamps to content; RightClick on a pin → `Menu`; Paste of "a\nb" inserts "a b"; a stale file-result generation is ignored (add a `StartState::files_arrived(gen, n)` helper and test it).
- [ ] **Step 2–3:** run → FAIL; implement `start.rs`, `start_view.rs`, app wiring, launcher jobs, menus.
- [ ] **Step 4: Live acceptance** (Desktop safety; power actions in **dry run** via a `VITROBAR_POWER_DRY_RUN=1` env var honoured only with `--debug-state`): click Start → panel appears above the Start button within 150 ms (measure: message timestamp → `dump_state` open); type "char" → Character Map first, Enter launches it (close it afterwards); type "2+2" → "4", Enter → clipboard (save/restore the user's clipboard around this step); type "volume" → Sound settings result (do NOT run it); All apps opens, wheel scrolls; power menu → Shut down → dry-run recorded, nothing happens; Esc closes; memory with panel open < 60 MB and back < 40 MB 3 s after closing; both monitors; Position Top opens below. Write the "Start menu" checklist section.
- [ ] **Step 5: Commit** `feat: Vitrobar Start panel with search`

---

### Task 7: Full-screen app grid mode

**Files:** Modify `crates/render/src/start_view.rs`, `app/vitrobar/src/start.rs`, `app.rs`.

**Interfaces — Consumes:** `grid_layout`, `StartStyle::FullScreen`, Task 6 controller.
Behaviour: when `settings.start_style == FullScreen`, Start opens a Panel covering the monitor's **work area** (radius 0) with a lighter tint (frosted at 0.12 alpha), search box at top centre, all apps in a grid (`GRID_CELL_DIP` cells: 48 px icon + 2-line wrapped name via `text_wrapped`), wheel/PageUp/PageDown scroll by rows, typing filters the grid in place (same ranking, apps only, plus the Math/Settings/Files rows above the grid), arrows move a focus ring, Enter launches, Esc closes. Opens with a 180 ms fade+scale(0.98→1) unless animations are off.

- [ ] **Step 1: Failing unit tests**: `StartState` in grid mode — arrows move by 1 / by `cols`, clamp at edges; typing filters to matching apps only; PageDown moves by `rows_visible * cols`.
- [ ] **Step 2–3:** FAIL → implement.
- [ ] **Step 4: Live acceptance**: switch setting to FullScreen (temp settings file) → Win/Start opens the grid on the right monitor's work area (taskbar strip still visible), filter "paint" → Paint, Enter launches (close it), Esc closes; memory open < 60 MB, back < 40 MB.
- [ ] **Step 5: Commit** `feat: full-screen app grid Start mode`

---

### Task 8: Keyboard hook — Windows key, Win+1…9, Win+T, Win+B

**Files:** Create `crates/core/src/key_hook.rs` (pure decisions), `crates/win/src/key_hook.rs` (hook thread), `app/vitrobar/src/keys.rs` (glue + bar focus mode); modify `app.rs`, `notify.rs` (new wake `WM_VITRO_KEY = WM_APP + 0x24`), `crates/render/src/bar_visual.rs` (focus ring on an item); tests: unit tests in core; `crates/win/tests/key_hook.rs` (`#[ignore]`).

**Interfaces — Produces:**
```rust
// core::key_hook — pure
pub enum KeyEv { Down(u32 /*vk*/), Up(u32) }
pub enum HookDecision { Pass, Swallow, PassAndMask /*inject VK 0xE8 down+up before letting this event through*/ }
pub enum HookOutput { OpenStart, LaunchNth(u8 /*1..=9*/), FocusApps, FocusTray, FocusKey(u32 /*vk while in focus mode*/), ExitFocus }
pub struct HookState { /* win_down, other_pressed, focus_mode, enabled */ }
impl HookState {
    pub fn new(enabled: bool) -> Self; pub fn set_enabled(&mut self, on: bool); pub fn set_focus_mode(&mut self, on: bool);
    pub fn on_event(&mut self, ev: KeyEv) -> (HookDecision, Option<HookOutput>);
}
// win::key_hook — dedicated thread "vitrobar-keys" with its own message loop; decides only via HookState behind a Mutex taken for microseconds; pushes HookOutput to a WakeQueue and posts WM_VITRO_KEY
pub struct KeyHook; impl KeyHook { pub fn start(notify: HWND, enabled: bool) -> Result<Self>; pub fn set_enabled(&self, on: bool); pub fn set_focus_mode(&self, on: bool); pub fn drain(&self) -> Vec<HookOutput>; }
// Drop: post WM_QUIT to the hook thread, UnhookWindowsHookEx, join (bounded 1 s).
```
Decision rules (pure, unit-tested):
- Ignore everything when `enabled == false` (Pass). Injected events never reach `on_event` (the hook proc filters `LLKHF_INJECTED` first).
- LWin/RWin Down → Pass, `win_down = true`, `other_pressed = false`.
- Any other key Down while `win_down` → `other_pressed = true`. If it is `'1'..='9'` → Swallow + `LaunchNth(n)` (and its Up is swallowed too). `'T'` → Swallow + `FocusApps`; `'B'` → Swallow + `FocusTray`. Every other key → Pass (Windows handles Win+E, Win+R, Win+L, …).
- LWin/RWin Up: if `!other_pressed` → `PassAndMask` + `OpenStart` (the mask key makes Windows treat it as a combo so the stock Start never opens and the Win key is released normally). If `other_pressed` → Pass.
- In focus mode: Left/Right/Up/Down/Home/End/Enter/Space/Tab/Escape/Apps(context-menu key) Down → Swallow + `FocusKey(vk)` (their Ups swallowed); any other key Down → Pass + `ExitFocus`.
Bar focus mode (keys.rs): `FocusApps` focuses the first app item on the bar of the monitor with the foreground window and shows a 2 DIP accent focus ring; Left/Right move, Home/End, Enter/Space = click, Apps key = app menu, Tab toggles between apps and tray, Esc or any other key or any mouse click exits. `FocusTray` focuses the first promoted tray icon (or the chevron). `LaunchNth(n)` = `click_action` on the n-th app group of that bar (Launch / Activate / Minimize / cycle: when the group has several windows, activate the next one instead of showing thumbnails). `OpenStart` = `toggle_start` on that monitor.
Foreground for the Start panel after a Win tap: `Panel::activate()`; the mask key event just injected counts as recent input, so SetForegroundWindow is permitted; if `activate()` still fails, retry once after injecting one more mask key (documented, bounded).
Setting `win_key_opens_start` off → hook stays installed but disabled: the Windows key and Win+1…9/T/B all behave exactly as stock.

- [ ] **Step 1: Failing tests** (core):
```rust
#[test] fn lone_win_tap_opens_start_with_mask() { let mut h = HookState::new(true); assert_eq!(h.on_event(Down(VK_LWIN)).0, Pass);
    assert_eq!(h.on_event(Up(VK_LWIN)), (PassAndMask, Some(OpenStart))); }
#[test] fn win_e_passes_untouched() { let mut h = HookState::new(true); h.on_event(Down(VK_LWIN)); assert_eq!(h.on_event(Down(0x45)), (Pass, None)); assert_eq!(h.on_event(Up(VK_LWIN)), (Pass, None)); }
#[test] fn win_digit_launches_and_swallows_both_edges() { /* Down('3') → (Swallow, LaunchNth(3)); Up('3') → (Swallow, None) */ }
#[test] fn win_t_and_b() { /* FocusApps / FocusTray */ }
#[test] fn disabled_is_fully_stock() { let mut h = HookState::new(false); h.on_event(Down(VK_LWIN)); assert_eq!(h.on_event(Up(VK_LWIN)), (Pass, None)); }
#[test] fn focus_mode_swallows_nav_and_exits_on_other() { /* Left → (Swallow, FocusKey(VK_LEFT)); 'A' → (Pass, ExitFocus) */ }
#[test] fn shift_win_s_passes() { /* Shift down, Win down, S down → Pass; Win up → Pass */ }
#[test] fn rwin_works_like_lwin() { }
```
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement** core, then the hook thread and glue.
- [ ] **Step 4: Hook integration test** `crates/win/tests/key_hook.rs`, `#[ignore = "installs a system-wide keyboard hook and injects keys"]`: start the hook on a test notify window, block the test's UI thread for 2 s, and assert through a debug counter (hook calls seen, max time spent in the hook proc) that the hook thread kept answering in < 5 ms while the UI thread was blocked — events injected by the test are filtered as injected by design, so they exercise the hook proc's fast path only. Then drop the hook and assert the thread has exited and `GetAsyncKeyState(VK_LWIN) & 0x8000 == 0`. Real Win-key behaviour is checked manually (checklist).
- [ ] **Step 5: Live acceptance (manual items written to the checklist section "Keyboard")**: Win tap opens Vitrobar Start, again closes it; Win+E opens Explorer; Win+R Run; Win+D desktop; Win+Shift+S snip; Win+L locks (user does this); Win+1 opens/activates the first app; Win+T focus ring + arrows + Enter; Win+B focuses tray; disable the setting → stock Start returns. Automated part: with the bar running, check `GetAsyncKeyState(VK_LWIN)` is not down after the session and the hook thread exists/exits with the bar.
- [ ] **Step 6: Commit** `feat: Windows key and Win+1…9/T/B handled by Vitrobar`

---

### Task 9: Glass settings window with live preview

**Files:** Create `crates/render/src/settings_view.rs`, `app/vitrobar/src/settings_window.rs`; modify `app.rs` (open/close, apply), `menus.rs` (bar menu becomes: "Settings…", separator, "Start with Windows" check, "Exit Vitrobar"; remove the settings submenus and their tests, keep `app_menu`), `docs/release-checklist.md` ("Pins and menus" → "Pins, menus and settings").

**Interfaces — Consumes:** `settings_ui::{build_model, apply, focus_next, focus_prev, ACCENT_SWATCHES}`, `Panel::show(.., app_window: true)`, `apply_bar_choice`/`relayout`/`save_settings` (existing), `KeyHook::set_enabled`, `apply_autostart`.
**Produces:** `fn open_settings(&mut self)`, `fn close_settings(&mut self)`, `MenuChoice::OpenSettings`.
Behaviour: a normal top-level window (title "Vitrobar settings"/"Impostazioni di Vitrobar", app icon, appears on our own bar), 720×640 DIP, centred on the monitor under the cursor, glass material, draggable by its top 40 DIP (WM_NCHITTEST → HTCAPTION), close button ✕ top-right (glyph E8BB), Esc closes, Alt+F4 closes, minimises normally. One page with section headers (Appearance, Behaviour, Language, About) scrolled by the mouse wheel. Controls: Choice = segmented pills; Toggle = switch; Swatches = 9 circles (first = "System"). **Every change applies immediately to the real bar (the live preview) and is saved at once** (no OK/Cancel — like Windows Settings). Keyboard: Tab/Shift+Tab move focus (ring), Left/Right change a Choice/Swatch, Space toggles, Enter activates links (`shell_open`). Language change re-builds the window in the new language. Opening it again while open focuses the existing window. Only one instance. Closing frees all its surfaces.

- [ ] **Step 1: Failing unit tests** (settings_window.rs controller, pure parts): clicking the "Full" segment → `apply(.., Shape, 2)` called and returns the relayout action; Left/Right on Material cycles within the 4 options; Space on WinKey toggles and requests `KeyHook::set_enabled(false)`; Esc → Close.
- [ ] **Step 2–3:** FAIL → implement.
- [ ] **Step 4: Live acceptance** (temp settings): right-click bar → Settings… opens the window; change shape/material/hover/size/position/theme/accent → bar updates within 100 ms each and the temp settings file reflects it; StartStyle → next Start open uses it; language → window + menus switch; Tab/arrows/Space work; Esc closes; reopen focuses existing; memory open < 60 MB, back < 40 MB.
- [ ] **Step 5: Commit** `feat: glass settings window with live preview`

---

### Task 10: Stage-1 parked items, perf and docs

**Files:** modify `app/vitrobar/src/app.rs` (`after_flush`), `crates/win/src/event_queue.rs` (doc), `scripts/perf-check.ps1`, `docs/release-checklist.md`, `README.md`.

- I3 parked item: after `MAX_IMMEDIATE_PASSES` wake-later rounds in a row (e.g. 4 consecutive `WakeLater` with a non-empty inbox each time), fall back to a normal blocking wait; unit test `persistent_refill_falls_back_to_blocking_wait` on `after_flush`.
- `WakeQueue` doc: "single producer per queue" (the key hook queue has exactly one producer, its thread).
- perf-check.ps1: new `-StartPanel` switch: opens the Start panel via a posted message to the notify window (debug-only message honoured with `--debug-state`), measures open time (< 150 ms) and memory open (< 60 MB) and 3 s after close (< 40 MB). The default run (panels closed) must still pass all stage-1 budgets.
- Checklist: sections "Start menu", "Keyboard", "Pins, menus and settings" complete; "Bar basics" updated (Start now opens Vitrobar Start).
- README: short feature list for stage 2.

- [ ] **Step 1: Failing test** for `after_flush` fallback. **Step 2–4:** FAIL → implement → PASS.
- [ ] **Step 5:** Run `pwsh scripts/perf-check.ps1 -Exe target/release/vitrobar.exe -MeasureStartup` and with `-StartPanel` → both `"pass": true`.
- [ ] **Step 6: Commit** `chore: stage-2 perf checks, docs and parked loop fix`

---

## After this plan

Stage 3: quick panels (Wi-Fi, Bluetooth, volume, brightness, battery), notifications (sparse package + SignPath application), calendar flyout, IME support for the Start search, Shell_NotifyIconGetRect answering. Stage-2 notes go in a new follow-ups file at the end of execution.
