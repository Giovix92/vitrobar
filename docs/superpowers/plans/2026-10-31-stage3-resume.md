# Vitrobar — where to resume (saved 2026-10-31, ~16:00)

Work stopped because the Claude 5-hour usage limit ran out in the middle of stage 3, Task 8.
Everything is committed on branch **`stage3-panels`** (not merged, not pushed). `main` = stages 1, 2, 2.5.

## How to resume (tell Claude)
> "Resume Vitrobar stage 3 from docs/superpowers/plans/2026-10-31-stage3-resume.md, subagent-driven, without asking."

Claude should: check out `stage3-panels`; recreate the SDD workspace for
`docs/superpowers/plans/2026-10-31-stage3-panels-widgets.md` and paste the ledger below into its
`progress.md` (the `.superpowers/` folder is git-ignored but still on disk — if it survived, use it as is);
then continue at Task 8.

## State of stage 3 (plan: `2026-10-31-stage3-panels-widgets.md`)

| Task | What | State |
|---|---|---|
| 1 | Panel kit + glass panel view | done, reviewed (751dd8b, e71ef7c) |
| 2 | Widget slots on the bar, Widgets settings, panel anchors | done, reviewed (bbc57a8, 801133e) |
| 3 | Audio: volume, mute, output device | done, reviewed (f4630d0) |
| 4 | Brightness (WMI + DDC/CI), battery, power mode | done, reviewed (34205d6, 8a43bed) |
| 5 | Wi-Fi list / connect | done, reviewed (5bf3162, 1200ac7, 9434ee6) |
| 6 | Bluetooth (classic + LE), audio connect | done, reviewed (b552f2f, 076dd19) |
| — | Fix: stock notification centre showed on the bar as an app | done (4640ca9) |
| 7 | **Quick panel** above the Quick button | done, reviewed (6746062, a53c44b, 75dafeb) |
| 8 | **Calendar panel** on the clock | **IN PROGRESS** — WIP commit 9022aed, does **not** build |
| 9 | Music widget | to do |
| 10 | Claude limits widget (status-line bridge, ruling P1) | to do |
| 11 | Tray rects, IME indicator, stage-2 leftovers | to do |
| 12 | Docs, checklist, perf | to do |
| — | Final whole-branch review (opus), fix wave, merge into `main` | to do |
| — | Ask the user to create the GitHub repo (public `Giovix92/vitrobar`, GPL-3.0) and push = first release | to do (needs the user's yes) |

### Task 8 — what the WIP commit has and what's missing
Has: `crates/core/src/calendar.rs` (month grid), `app/vitrobar/src/calendar_panel.rs` (controller),
panel_kit/panel_view changes (clickable header actions for the month chevrons), i18n strings,
`time_format.rs` additions, `flyout_toggle.rs` deleted (clock no longer opens the stock flyout),
app.rs wiring started.
Build errors left (`cargo build -p vitrobar`):
- `crate::calendar_panel` not declared (`mod calendar_panel;` in app/vitrobar/src/main.rs)
- `notify::TIMER_CALENDAR_MIDNIGHT` missing (one-shot midnight timer id in notify.rs)
- `clock_anchor()` missing in app.rs (mirror `quick_anchor`, using `AnchorTarget::Clock`, any monitor's bar)
- `Inbox::debug_open_calendar` missing (debug hook `Vitrobar.Debug.OpenCalendar`, only with --debug-state)
Then: run `cargo test --workspace`, finish against `task-8` of the plan, task review, fix rounds.
If the WIP looks wrong, `git reset --hard 75dafeb` (on this branch only) and redo Task 8 from the plan.

## Try builds
- `target/try2/release/vitrobar.exe` = stage 3 up to Task 7 (quick panel, shell-panel fix).
- The user's running copy may be `target/try/release` (stage 2.5) — never kill it; build into a new
  `--target-dir` (e.g. `target/try3`) for new try copies.

## Things only the user can check (add to docs/release-checklist.md in Task 12)
- Quick panel: opens above the Quick button in pill / islands / full, both monitors, bar at top.
- Wi-Fi connect (known network, new password network, wrong password → message), Bluetooth headphones
  connect/disconnect, LE mouse listed, volume + output switch, brightness slider follows the laptop keys,
  power mode, battery label.
- From stage 2.5 (never verified live): Start fade/hand-back, glass menus (Esc, Alt, Win key, click outside),
  preview ✕ and middle-click, Quick/clock second click.
- Perf re-run (`scripts/perf-check.ps1`, `-StartPanel`, and with both widgets on) — needs the user's
  instance closed.

## Ledger (copy of `.superpowers/sdd/2026-10-31-stage3-panels-widgets/progress.md`)
    # SDD ledger — plan: docs/superpowers/plans/2026-10-31-stage3-panels-widgets.md
    Branch stage3-panels from main aff560b; plan commit 3650485. Spec: docs/superpowers/specs/2026-10-30-vitrobar-design.md
    
    ## Pre-flight scan
    | Pair / task | Shared | Finding |
    |---|---|---|
    | T1 → T7,T8,T9,T10 | panel_kit Row/Part, PanelView | consumers use Row::Custom for grid/artwork/progress/password; T1 must give Custom a draw hook (controller draws into a sub-visual at the Custom rect). Ruling S3-1. |
    | T2 → T7–T10 | anchor_for, Hit::Widget, settings fields | consistent names. clean |
    | T3–T6 → T7 | win snapshots vs core mirror structs | T7 defines core mirror structs; T3–T6 return win types; app converts. Ruling S3-2. |
    | T9,T10 → T2 | widget widths | app computes pill widths from text; layout takes widths. clean |
    | T7 airplane | no public airplane API | Ruling S3-3. |
    | T10 self | serde_json preserve_order new dep in vitro-win | allowed (no runtime dep beyond crate). clean |
    | T11 self | IME click "Win+Space semantics" vague | Ruling S3-4. |
    | T12 self | docs | clean |
    
    Ruling S3-1: PanelView exposes the Custom row's rect (px) and a ContainerVisual slot per Custom id; controllers attach their own sprites there — why: keeps panel_kit generic — cost: small API addition.
    Ruling S3-2: core quick_model defines its own plain input structs; the app converts win snapshots — why: core has no windows dep — cost: a little duplication.
    Ruling S3-3: airplane tile shown only if Windows.Devices.Radios reports every radio controllable; toggling sets all radios off/on (approximation); else hidden (airplane None) — why: no public airplane API — cost: approximation vs stock airplane mode.
    Ruling S3-4: IME glyph click opens nothing new: it cycles the layout for the foreground window via WM_INPUTLANGCHANGEREQUEST(INPUTLANGCHANGE_FORWARD) posted to the foreground window — why: simple, documented — cost: no picker list.
    Task 1: dispatched (BASE 3650485)
    Task 1: implemented 751dd8b
    Task 1: review Needs fixes (hard-coded accent; no restyle; fractional-DPI offsets; partial apply). fix round 1 dispatched
    Task 1: fix round 1 e71ef7c; re-review all ADDRESSED. Parked minors: apply commit-phase `?` after state swap; reused-row SetOffset early return skips cleanup; restyle failure leaves no rows (all need a Composition setter to fail).
    Task 1: complete (751dd8b, e71ef7c). API notes for later tasks: PanelView::show(comp, anchor_px, position, dpi, style, painter?, accent, rows) — check signature; update(comp, painter, rows); restyle(...); custom_slot(id)/custom_rect_px(id) (scroll 0); NavKey has no Tab (map Tab→Down, Shift+Tab→Up).
    Task 2: dispatched (BASE e71ef7c)
    Task 2: implemented bbc57a8
    Task 2: review Approved; controller shortened hint (801133e). Parked: anchor_for widget on secondary bar falls back to zero-width quick (callers use primary layout); widget pills not keyboard-reachable; pill hover plate only.
    Task 2: complete (bbc57a8, 801133e). API: set_widgets([Option<WidgetPill>;2], hovered: Option<usize>) on bar visual; LayoutInput.widgets widths; anchor_for(&layout, AnchorTarget).
    Task 3: dispatched (BASE 801133e)
    Task 3: implemented f4630d0 (open(notify, wake_msg) signature; IPolicyConfig vtable 10 methods before SetDefaultEndpoint — controller confirms matches the known declaration)
    Task 3: review Approved. Parked minors: CoInitializeEx unbalanced; endpoint re-activated per call; multi-producer doc; slider drags echo back as Change::Volume (Task 7 must tolerate — carry); rename on next change only.
    Task 3: complete (f4630d0). API: Audio::open(notify: HWND, wake_msg: u32), drain_changes(), snapshot(), set_volume, set_muted, set_default, can_switch.
    Task 4: dispatched (BASE f4630d0)
    Task 4: implemented 34205d6
    Task 4: review approved-with-fixes (hard-coded Built-in display; WMI sink needs IUnsecuredApartment; overlay setter GUID by value for ARM64; OnceLock; refresh vs optimistic). fix round 1 dispatched. Ruling: internal display name empty + Key::BuiltInDisplay.
    Task 4: fix round 1 8a43bed; re-review all ADDRESSED. Parked: unsecured-apartment failure not logged; WMI event mid-write may briefly overwrite pending (check in Task 7 wiring). Live: hardware brightness keys → slider follows (checklist).
    Task 4: complete (34205d6, 8a43bed). Carry to Task 7: map BrightnessKind::Internal → tr(Key::BuiltInDisplay); Audio slider echo.
    Task 5: dispatched (BASE 8a43bed)
    Task 5: implemented 5bf3162
    Task 5: review Needs fixes (overwrite=true can clobber/delete user profiles; scan expiry has no wake). Ruling S3-5: never overwrite/delete profiles not created by Vitrobar; existing-name → connect as known, fail → HandOff. fix round 1 dispatched
    Task 5: fix round 1 1200ac7, re-review ADDRESSED. Ruling S3-6: Wifi::Drop never deletes a pending created profile (panel close mid-connect must not abort); leaked manual profile after later failure accepted. fix round 2 dispatched (A drop, B stale created, C short payload, D single outcome)
    Task 5: fix round 2 9434ee6 (implementer staged but didn't commit; controller verified diff A–D and committed). 
    Task 5: complete (5bf3162, 1200ac7, 9434ee6). API: Wifi::open(notify, wake_msg), drain_changes() -> Vec<Change> (Networks, ConnectFailed, Error(WifiError)), snapshot(), scan(), scan_deadline(), set_radio, connect(ssid, Option<&str>), disconnect. Carry to Task 7: arm one-shot timer from scan_deadline().
    Task 6: dispatched (BASE 9434ee6)
    Task 6: implemented b552f2f
    Task 6: review Approved; controller asked small fix round (resolve retry, BLE watcher, pending flag)
    Task 6: fix round 1 076dd19 (resolve retry, BLE watcher, pending w/ 20 s max). Carry to Task 7: pending expiry is lazy — arm a one-shot timer while any device is pending.
    Task 6: complete (b552f2f, 076dd19)
    Controller fix (user screenshot 15:10): stock notification centre listed on bar as an app → shell-surface exe filter in is_taskbar_window (4640ca9).
    Task 7: dispatched (BASE 4640ca9)
    Task 7: implemented 6746062, a53c44b (airplane hidden; battery saver → ms-settings; header trailing not clickable)
    Task 7: review Needs fixes (no foreground hand-back; drag after lost capture; paste CRLF; zero-fill; re-place on relayout; brightness jump; connecting stuck; it gender, ctrl-arrows). fix round 1 dispatched
    Task 7: fix round 1 75dafeb; re-review all ADDRESSED. Parked: backspace/delete chars not zeroed; connecting stays if Windows reconnects without leaving old network (until failure event).
    Task 7: complete (6746062, a53c44b, 75dafeb). Try build: target/try2/release/vitrobar.exe. API: PanelView::reanchor; hand_back_foreground(hwnd, fg_at_open) shared; header trailing NOT clickable (Task 8 needs it for month chevrons → extend kit).
    Task 8: dispatched (BASE 75dafeb)
    Task 8: interrupted by usage limit; partial work saved as WIP commit 9022aed (does not build). Resume: finish Task 8 from the WIP (or redo from 75dafeb), then review.
