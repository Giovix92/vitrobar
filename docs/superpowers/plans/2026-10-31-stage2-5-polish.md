# Stage 2.5 — Polish fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the four problems the user hit in the first real run of stages 1–2: no Start animation, plain Windows right-click menus, Quick Settings / clock buttons that can't close their panel, and no way to close a window from the hover preview.

**Architecture:** Pure models (menu layout + keyboard navigation, flyout toggle state) go in `vitro-core` with unit tests; drawing goes in `vitro-render`; wiring in the app. The glass menu keeps `menus.rs`'s synchronous public functions (they still return `Option<MenuChoice>` / `Option<PowerAction>`) by running its own nested message loop, so callers change only by passing a drawing context.

**Tech Stack:** Rust, `windows` 0.62.2, Windows.UI.Composition + Direct2D/DirectWrite on WARP (existing `Painter`, `Flyout`, `Panel`).

**Spec:** `docs/superpowers/specs/2026-10-30-vitrobar-design.md`

**User report (2026-10-31), the source of this plan:** "no animation when opening/closing start menu"; "dialogs are in basic windows theme, shall we theme those too?" (they are the right-click menus); "if I open the settings quickpanel … I can't close it unless I click in another part of the screen; same goes for the clock dialog"; "when I hover an app and the preview is shown I can't close it from there". Not in this plan: pill-mode positioning of the stock flyouts (fixed by stage 3's own panels — Windows always places its flyouts at the right edge); "two apps spawn when opening the stock Start" (not reproducible any more — see Task 5).

## Global Constraints

- All stage-1 and stage-2 Global Constraints still apply (see `2026-10-30-stage1-bar.md` and `2026-10-30-stage2-start.md`): RAM < 40 MB idle with panels closed, < 60 MB with a panel open; idle CPU 0 %; no polling; animations 150–250 ms and "Animation effects off" (`self.sys.animations == false`) means no animation at all; no injection into other processes; en + it strings via `vitro_core::i18n`; WARP drawing (D1).
- **Desktop safety (binding for every live test):** only touch windows/processes the test started; never send WM_CLOSE / SC_CLOSE to a window the test did not create; never Notepad; `VITROBAR_SETTINGS` → temp file with `first-run-done = true` and `start-with-windows = false`; never execute a power action; screenshots only of Vitrobar's own windows; always verify the stock taskbar state at the end.
- A Vitrobar instance the user started may be running (`target/release/vitrobar.exe`). Build and test with the debug profile; never kill a process you did not start.
- Composition animations only (the compositor runs them; no timers driving frames). A one-shot timer is allowed to destroy a window after its close animation.

## Review Focus

1. **Start opened again while its close animation is still running** (double-tap Win, or click Start twice fast) → the fading panel is dropped at once and a fresh one opens; never two panels, never an invisible panel holding the keyboard (Task 1).
2. **Right-click menu opened from the Start panel** → Start stays open while the menu is up and after it closes (the menu taking the foreground must not count as "clicked elsewhere") (Task 2).
3. **Glass menu dismissal** — Esc, click outside, Alt+Tab, Win key, a second right-click on the bar → the menu closes and returns `None`; a `WM_QUIT` posted during the menu is re-posted after it (Task 2).
4. **Quick Settings open, user clicks the clock button (or the reverse)** → switches to the other panel, not a close; clicking the same button after the user dismissed the panel some other way → opens it again (Task 3).
5. **Closing a window from the preview whose app shows a "save changes?" prompt** → the prompt appears and the preview just updates; Vitrobar never forces a close (Task 4).

---

## File structure

```
crates/core/src/
  menu_model.rs      NEW  glass menu items, layout, hit test, keyboard navigation (pure)
  flyout_toggle.rs   NEW  which stock flyout is open, from launches + foreground changes (pure)
crates/render/src/
  menu_view.rs       NEW  draws a menu model into a Flyout (glass, hover row, check marks)
  panel.rs           MOD  open/close motions
  flyout.rs          MOD  close button per thumbnail
app/vitrobar/src/
  menus.rs           MOD  glass menus with a nested modal loop (same return types)
  app.rs             MOD  Start close animation, toggle wiring, preview close, menu context
```

---

### Task 1: Start panel open and close animation

**Files:**
- Modify: `crates/render/src/panel.rs` (`animate_open` at ~284, `show_static`)
- Modify: `app/vitrobar/src/start.rs` (`animate_open` wrapper at ~1283)
- Modify: `app/vitrobar/src/app.rs` (`open_start` ~2878–2990, `close_start` ~2997, `toggle_start` ~2865, `GRID_OPEN_MS` at 161, the timer dispatch)

**Interfaces:**
- Produces in `vitro_render::panel`:
  - `pub enum Motion { Grow, Rise { from: Position } }` — `Grow` is today's fade + 98 %→100 % scale (grid); `Rise` fades in while sliding `RISE_DIP = 12.0` DIP from the bar's side (`Position::Bottom` → starts 12 DIP lower; `Top` → 12 DIP higher; `Position` has only these two variants).
  - `pub fn animate_open(&self, ms: u32, motion: Motion) -> Result<()>` (replaces the one-argument version; keep the `or_restore` safety).
  - `pub fn animate_close(&self, ms: u32, motion: Motion) -> Result<()>` — the reverse (opacity 1→0, slide back / scale to 98 %), and immediately makes the window stop taking input: `WS_EX_TRANSPARENT | WS_EX_NOACTIVATE` added and the window un-foregrounded is not needed — it just must not receive clicks.
- Constants in `app.rs`: `START_OPEN_MS: u32 = 160`, `START_CLOSE_MS: u32 = 110`, keep `GRID_OPEN_MS = 180`, add `GRID_CLOSE_MS: u32 = 130`.

- [ ] **Step 1: Write the failing tests** (in `panel.rs` tests, pure helper):

```rust
#[test]
fn rise_starts_on_the_bar_side() {
    assert_eq!(rise_offset(Position::Bottom, 2.0), (0.0, 24.0));
    assert_eq!(rise_offset(Position::Top, 2.0), (0.0, -24.0));
}
```
`fn rise_offset(from: Position, scale: f32) -> (f32, f32)` returns the start offset in px.

- [ ] **Step 2:** `cargo test -p vitro-render rise_starts` → FAIL (not defined).
- [ ] **Step 3: Implement** `rise_offset`, `Motion`, two-argument `animate_open`, `animate_close`. `Rise` animates the root's `Offset` (Vector3 key frames from the base offset + `rise_offset` to the base offset) and `Opacity`, same cubic-bezier ease as today (0.1,0.9)/(0.2,1.0) for open; close uses (0.4,0)/(1,1).
- [ ] **Step 4: Wire it in `app.rs`:**
  - Panel mode open: when `self.sys.animations`, call `sp.animate_open(START_OPEN_MS, Motion::Rise { from: self.settings.position })`; grid keeps `Motion::Grow` with `GRID_OPEN_MS`. On error → `show_static()` (as today).
  - Closing: `close_start` keeps all its current bookkeeping (icons, file query, rev, surrogate, dump) but, when animations are on and the close was not caused by a monitor/DPI/Explorer change, moves the panel into a new field `start_closing: Option<StartPanel>` after `animate_close(...)` and arms a one-shot timer `TIMER_START_CLOSED` for the close duration + 30 ms; the timer drops it. If `animate_close` fails, drop at once.
  - Any `open_start` first drops `start_closing` (Review Focus 1). `toggle_start` must treat a closing panel as closed. Shutdown/Explorer-restart paths drop `start_closing` with no animation.
  - Input or deactivation messages for the closing panel's hwnd are ignored (it is no longer `self.start`).
- [ ] **Step 5:** `cargo test --workspace` → all pass. Live check (desktop-safety rules, temp settings): open/close Start 5× quickly by the debug hook used in stage 2 tests (`--debug-state` dumps); assert after each burst exactly one or zero panel windows of class `VitrobarPanel` exist and, 500 ms after the last close, zero.
- [ ] **Step 6: Commit** `feat(start): open and close animations`

---

### Task 2: Glass right-click menus

**Files:**
- Create: `crates/core/src/menu_model.rs` (+ `pub mod` in `lib.rs`)
- Create: `crates/render/src/menu_view.rs` (+ `pub mod`)
- Modify: `app/vitrobar/src/menus.rs` (replace the `Menu`/`TrackPopupMenuEx` internals; keep `MenuChoice`, the id constants, `choice_for`, `power_for` and their tests)
- Modify: `app/vitrobar/src/app.rs` (the four callers at ~3141, ~3174, ~3394, ~3403; Start deactivation at ~3838)

**Interfaces:**
- `vitro_core::menu_model`:
  - `pub enum Entry { Command { id: u32, text: String, checked: bool }, Separator }`
  - `pub struct MenuLayout { pub size: (f32, f32), pub rows: Vec<Rect> }` (DIP; one rect per entry, separators included)
  - `pub fn layout(entries: &[Entry], text_w: &dyn Fn(&str) -> f32) -> MenuLayout` — constants `PAD_DIP = 4.0`, `ROW_H_DIP = 32.0`, `SEP_H_DIP = 9.0`, `TEXT_LEFT_DIP = 36.0` (room for the check mark), `TEXT_RIGHT_DIP = 16.0`, `MIN_W_DIP = 180.0`, `MAX_W_DIP = 360.0`.
  - `pub fn hit(l: &MenuLayout, entries: &[Entry], pt_dip: (f32, f32)) -> Option<usize>` — command rows only; separators and padding → `None`.
  - `pub enum NavKey { Up, Down, Home, End }` and `pub fn step(entries: &[Entry], cur: Option<usize>, key: NavKey) -> Option<usize>` — skips separators, wraps, `None` start: Down → first command, Up → last.
  - `pub fn place(at: (i32, i32), size_px: (i32, i32), work: (i32, i32, i32, i32)) -> (i32, i32)` — opens up-left of the point when the point is in the lower half / right half of `work`, else down-right; then clamps inside `work`.
- `vitro_render::menu_view::MenuView` — `pub fn show(comp, painter, style: &Style, dpi: u32, at_px: (i32,i32), entries: &[Entry]) -> Result<MenuView>`, `pub fn hwnd(&self) -> HWND`, `pub fn set_hot(&mut self, i: Option<usize>) -> Result<()>` (redraws only the hot-row highlight: a rounded 6 DIP-radius plate in the style's hover colour), `pub fn hit_px(&self, client_px) -> Option<usize>`. Built on `Flyout::show` (same glass and radius as the window preview); text with the existing `Painter`, check mark = Segoe Fluent Icons `\u{E73E}`.
- `menus.rs`: `pub struct MenuCtx<'a> { pub comp: &'a Compositor, pub painter: &'a Painter, pub style: &'a Style, pub dpi: u32 }` (use the app's real types). Each public function gains `ctx: &MenuCtx` as its first parameter; return types unchanged.

- [ ] **Step 1: Write the failing core tests** in `menu_model.rs`:

```rust
fn e() -> Vec<Entry> { vec![cmd(1,"Settings…"), Entry::Separator, cmd(5,"Start with Windows"), cmd(6,"Exit")] }

#[test] fn rows_stack_with_padding() {
    let l = layout(&e(), &|_| 100.0);
    assert_eq!(l.rows[0].y, 4.0); assert_eq!(l.rows[1].h, 9.0); assert_eq!(l.rows[2].y, 4.0 + 32.0 + 9.0);
    assert_eq!(l.size, (180.0, 4.0 + 32.0 * 3.0 + 9.0 + 4.0));
}
#[test] fn width_follows_the_longest_text_within_bounds() {
    assert_eq!(layout(&e(), &|_| 200.0).size.0, 36.0 + 200.0 + 16.0);
    assert_eq!(layout(&e(), &|_| 900.0).size.0, 360.0);
}
#[test] fn separators_are_never_hit_or_selected() {
    let (en, l) = (e(), layout(&e(), &|_| 100.0));
    assert_eq!(hit(&l, &en, (50.0, 4.0 + 32.0 + 4.0)), None);
    assert_eq!(step(&en, Some(0), NavKey::Down), Some(2));
    assert_eq!(step(&en, Some(2), NavKey::Up), Some(0));
    assert_eq!(step(&en, Some(3), NavKey::Down), Some(0));
    assert_eq!(step(&en, None, NavKey::Up), Some(3));
    assert_eq!(step(&[], None, NavKey::Down), None);
}
#[test] fn opens_away_from_the_bar_and_stays_on_screen() {
    let work = (0, 0, 1920, 1040);
    assert_eq!(place((100, 1030), (200, 150), work), (100, 880));
    assert_eq!(place((1900, 1030), (200, 150), work), (1700, 880));
    assert_eq!(place((100, 10), (200, 150), work), (100, 10));
}
```

- [ ] **Step 2:** `cargo test -p vitro-core menu_model` → FAIL.
- [ ] **Step 3: Implement `menu_model.rs`.** Run the tests → PASS.
- [ ] **Step 4: Implement `MenuView`** (render). One surface for all row texts drawn once; the hot plate is a separate sprite moved/resized on `set_hot` (no text redraw on hover).
- [ ] **Step 5: Replace the internals of `menus.rs`.** A private `fn run(owner: HWND, ctx: &MenuCtx, at: (i32,i32), entries: &[Entry]) -> u32` (0 = dismissed):
  - shows the `MenuView`, `activate()`s it, then runs a nested `GetMessageW` loop until done. For messages to the menu hwnd: mouse move → `set_hot`; left/right button up on a command row → return its id; key down: Up/Down/Home/End → `step`, Enter/Space → hot id, Esc → 0. Everything else: `TranslateMessage` + `DispatchMessageW` (the notify window keeps recording its inbox, as with the native menu).
  - Ends with 0 on: the menu's deactivation (`take_deactivated()` contains its hwnd — click outside, Alt+Tab, Win key), any button-down outside it, `WM_QUIT` (re-post it with `PostQuitMessage` after the loop).
  - Before returning, if `owner` is a Vitrobar panel (the Start panel), give it the foreground back (`SetForegroundWindow(owner)`).
  - The four public functions build `Vec<Entry>` from the same ids and `tr(lang, …)` strings they use today, in the same order; `build_bar_menu` becomes `fn bar_entries(s, lang) -> Vec<Entry>` and the existing test asserts `bar_entries(..).len() == 4` (3 commands + 1 separator) in both languages.
- [ ] **Step 6: Start must survive its own menu (Review Focus 2).** In `app.rs`, a Start-panel deactivation is ignored when the window taking the foreground belongs to our process (`GetWindowThreadProcessId(GetForegroundWindow())` == our pid) or is the panel itself. Settings window opening from the bar menu still works (it closes Start today only if that's existing behaviour — keep it).
- [ ] **Step 7: Update the callers** to pass `MenuCtx` built from `self.comp`, `self.painter`, `self.style` and the monitor's dpi at the click point.
- [ ] **Step 8:** `cargo test --workspace` → PASS. Live check (desktop safety; temp settings; `--debug-state`): open the bar menu with a synthetic right-click on Vitrobar's own bar window only; press Esc via `PostMessage(WM_KEYDOWN, VK_ESCAPE)` to the menu hwnd; assert no `VitrobarFlyout` window remains and the bar still responds. Screenshot only the menu's own window rect, to look at it.
- [ ] **Step 9: Commit** `feat(menus): glass right-click menus`

---

### Task 3: Quick Settings and clock buttons toggle their panel

**Files:**
- Create: `crates/core/src/flyout_toggle.rs` (+ `pub mod`)
- Modify: `app/vitrobar/src/app.rs` (`click` at ~2577–2579; the foreground-change handler that already exists for focus/grouping)
- Modify: `crates/win/src/launch.rs` (add `pub fn dismiss_foreground_flyout()`)

**Interfaces:**
- `vitro_core::flyout_toggle`:
  - `#[derive(Clone, Copy, PartialEq, Eq, Debug)] pub enum StockFlyout { QuickSettings, Notifications }`
  - `pub struct FlyoutToggle` with `pub fn new() -> Self`;
  - `pub fn launched(&mut self, which: StockFlyout, now_ms: u64)` — remembers a pending launch;
  - `pub fn foreground_changed(&mut self, fg: u64, now_ms: u64)` — the first foreground change within `CLAIM_MS = 1500` after a launch claims `fg` as that flyout's window; any later change to a different window clears the open state;
  - `pub fn on_click(&mut self, which: StockFlyout, fg: u64, now_ms: u64) -> ToggleAction` where `pub enum ToggleAction { Open, Close }` — `Close` only when the same flyout is recorded open and its window `fg` is still the foreground; otherwise `Open`. After `Close` the state is cleared.
- `launch.rs`: `pub fn dismiss_foreground_flyout()` sends Esc (`SendInput` VK_ESCAPE down/up) — allowed synthetic input; add it to the Global Constraints list in the stage-2 followups doc. Callers check the foreground is the recorded flyout window first.

- [ ] **Step 1: Write the failing tests** in `flyout_toggle.rs`:

```rust
use StockFlyout::*;
#[test] fn second_click_closes() {
    let mut t = FlyoutToggle::new();
    assert_eq!(t.on_click(QuickSettings, 1, 0), ToggleAction::Open);
    t.launched(QuickSettings, 0); t.foreground_changed(77, 200);
    assert_eq!(t.on_click(QuickSettings, 77, 3000), ToggleAction::Close);
    assert_eq!(t.on_click(QuickSettings, 1, 3100), ToggleAction::Open);
}
#[test] fn other_button_switches() {
    let mut t = FlyoutToggle::new(); t.launched(QuickSettings, 0); t.foreground_changed(77, 200);
    assert_eq!(t.on_click(Notifications, 77, 900), ToggleAction::Open);
}
#[test] fn dismissed_elsewhere_reopens() {
    let mut t = FlyoutToggle::new(); t.launched(Notifications, 0); t.foreground_changed(77, 200);
    t.foreground_changed(5, 4000);
    assert_eq!(t.on_click(Notifications, 5, 4100), ToggleAction::Open);
}
#[test] fn late_foreground_is_not_claimed() {
    let mut t = FlyoutToggle::new(); t.launched(QuickSettings, 0); t.foreground_changed(77, 2000);
    assert_eq!(t.on_click(QuickSettings, 77, 2100), ToggleAction::Open);
}
```

- [ ] **Step 2:** `cargo test -p vitro-core flyout_toggle` → FAIL. **Step 3:** implement → PASS.
- [ ] **Step 4: Wire it.** `Hit::Quick` / `Hit::Clock`: ask `on_click(which, foreground() as u64, now)`; `Open` → `self.launcher.launch(...)` then `launched(...)`; `Close` → `dismiss_foreground_flyout()`. Feed `foreground_changed` from the tracker's `WindowEvent::Activated` (`crates/win/src/window_tracker.rs:115`, handled in `app.rs` ~1647). The shell hook may not report the stock flyout's window: if a live check shows no `Activated` for it, add `EVENT_SYSTEM_FOREGROUND` to the tracker's `SetWinEventHook` ranges (~230) mapped to the same `WindowEvent::Activated`. Time from a monotonic `Instant` base kept in the app.
- [ ] **Step 5:** `cargo test --workspace` → PASS. Manual check goes to `docs/release-checklist.md` (new line under "Bar"): "Click the Quick Settings button twice → opens then closes; same for the clock; Quick Settings then clock → switches."
- [ ] **Step 6: Commit** `fix(bar): Quick Settings and clock buttons close their panel on a second click`

---

### Task 4: Close a window from the hover preview

**Files:**
- Modify: `crates/render/src/flyout.rs` (`strip_layout`, `ThumbnailStrip`)
- Modify: `app/vitrobar/src/app.rs` (`on_flyout_mouse` ~2254, `build_strip`/`sync_strip`)
- Modify: `crates/win/src/launch.rs` (next to `activate_window` ~124 / `minimize_window` ~134): add `pub fn request_close(h: HWND)`

**Interfaces:**
- `StripLayout` gains `pub closes: Vec<Rect>` — one `CLOSE_DIP = 20.0` square per window, right-aligned in its title row and vertically centred on it; the title rect's width shrinks by `CLOSE_DIP + 4.0` so text never runs under it. (DWM thumbnails draw above composition content, so the button lives in the title row, never on the thumbnail.)
- `ThumbnailStrip`: `pub fn hit_close(&self, pt_px) -> Option<HWND>`, `pub fn set_close_hot(&mut self, h: Option<HWND>) -> Result<()>` (hot = round 4 DIP-radius plate in a soft red `#C42B1C` at 90 %, glyph white; not hot = glyph in `text` colour at 70 %); the ✕ glyph is Segoe Fluent Icons `\u{E8BB}` at 10 DIP, drawn in the same one-time title surface.
- `request_close(h)`: `PostMessageW(h, WM_SYSCOMMAND, SC_CLOSE, 0)` — exactly what the stock taskbar sends; never `WM_CLOSE` forcing, never `EndTask` (Review Focus 5).

- [ ] **Step 1: Failing test** added to `strip_sizes_follow_the_brief`'s module:

```rust
#[test] fn close_buttons_sit_at_the_right_of_each_title() {
    let l = strip_layout(&[1.6, 1.6]);
    assert_eq!(l.closes.len(), 2);
    assert_eq!(l.closes[0].right(), l.boxes[0].right());
    assert_eq!(l.closes[0].w, 20.0);
    assert!(l.titles[0].right() <= l.closes[0].x - 4.0);
    assert_eq!(l.closes[1].y + l.closes[1].h / 2.0, l.titles[1].y + l.titles[1].h / 2.0);
}
```
Update the existing `l.size.1` assertion only if the title row height must grow to fit 20 DIP (if it does, title row = 20 DIP and document it).

- [ ] **Step 2:** FAIL → **Step 3:** implement layout + drawing + hit → PASS.
- [ ] **Step 4: Wire it in `on_flyout_mouse`:** mouse move → `set_close_hot(hit_close(pt))`; left button up on the same close button it went down on → `request_close(h)` (do not activate, do not close the strip); middle button up (`WM_MBUTTONUP`, with `WM_MBUTTONDOWN` pairing like the left button) on a thumbnail slot → `request_close(h)`. The strip already rebuilds when the group's windows change (`sync_strip`); when the last window goes, it closes.
- [ ] **Step 5:** `cargo test --workspace` → PASS. Live check: the test creates its own two top-level windows (a minimal Win32 window class in the test, own process), hovers its group only via the debug hooks, clicks the ✕ of one by posting mouse messages to the flyout hwnd at `closes[0]` px; assert that window received `SC_CLOSE` and the other still exists; destroy the remaining test window.
- [ ] **Step 6: Commit** `feat(preview): close button and middle-click close per window`

---

### Task 5: Docs, followups and perf

**Files:**
- Modify: `docs/superpowers/plans/2026-10-30-stage2-followups.md` (append a "Stage 2.5" section)
- Modify: `docs/release-checklist.md` (LF line endings only)
- Run: `scripts/perf-check.ps1` and `scripts/perf-check.ps1 -StartPanel`

- [ ] **Step 1:** Followups: record "two apps appeared after opening the stock Start (user report 2026-10-31, not reproducible)": the only Vitrobar-side path that opens stock Start is `open_stock_start()` (Ctrl+Esc via SendInput) on a Start-panel failure; watch item — if it recurs, capture which apps and how Start was opened. Record the new synthetic input (Esc in Task 3) and that glass menus lose the native menu's screen-reader support (to revisit with UI Automation later).
- [ ] **Step 2:** Checklist: add the Task 3 line, plus "Right-click the bar, an app, a Start pin and the power button → glass menus; Esc and clicking outside close them; Start stays open behind its own menus" and "Hover an app with two windows → ✕ closes one; middle-click closes one; an unsaved document asks before closing".
- [ ] **Step 3:** Perf: both runs report `"pass":true`; record the numbers in the followups.
- [ ] **Step 4: Commit** `docs: stage 2.5 followups and checks`
