# Stage 3 — decisions and follow-ups

Carried out of the stage-3 execution ledger (plan `2026-10-31-stage3-panels-widgets.md`,
branch `stage3-panels`). Input for stage 3b and later.

## Rulings made while planning
- **P1 Claude usage source.** Anthropic's Claude Code docs (updated 2026-02-19) say OAuth tokens from Free/Pro/Max accounts may not be used "in any other product, tool, or service". Vitrobar therefore never reads Claude Code's credentials and makes no network call for the Claude widget. It uses Claude Code's own status line: `vitrobar.exe --claude-statusline` is the `statusLine.command` while the widget is on; it saves `rate_limits.five_hour` / `seven_day` (`used_percentage`, `resets_at` as epoch seconds, milliseconds or RFC 3339) to `%LOCALAPPDATA%\Vitrobar\claude-usage.json` and runs the user's previous status line with the same input. The values update while Claude Code is in use; the tile shows "as of HH:MM".
- **P2 Notifications list** needs package identity (sparse package) and code signing: deferred to stage 3b with signing. The calendar panel has a "Notifications" button that opens the stock notification centre.
- **P3 Bluetooth connect/disconnect** has no general Windows API: offered for audio devices through `BluetoothSetServiceState` on their A2DP sink and Hands-Free services; other devices show their state and hand off to `ms-settings:bluetooth`.
- **P4 Lazy services.** A system service (WLAN handle, audio endpoint, WMI, radios, media manager, the Claude folder watch) exists while its panel is open or its widget is on, and is dropped when the panel closes or the widget is turned off.

## Controller rulings
- S3-1: `PanelView` exposes each `Custom` row's rect and a `ContainerVisual` slot; controllers draw their own sprites there (month grid, artwork, progress bars).
- S3-2: `quick_model` defines plain input structs; the app converts the win snapshots (core stays free of `windows`).
- S3-3: no public airplane-mode API: the tile would toggle every radio; it is hidden (airplane `None`) in this build.
- S3-4: the input-language indicator's click posts `WM_INPUTLANGCHANGEREQUEST(INPUTLANGCHANGE_FORWARD)` to the foreground window (no picker list).
- S3-5: Wi-Fi never overwrites or deletes a profile Vitrobar did not create; a network whose name already has a profile is connected as known, and a failure hands off to Settings.
- S3-6: closing the panel mid-connect never deletes the profile being connected; a manual profile left behind by a later failure is accepted.
- Task 4: the internal display has an empty name and shows `Key::BuiltInDisplay`.
- Task 7: airplane hidden (S3-3); battery saver opens `ms-settings:batterysaver`.
- Task 8: the executor switched from subagent-driven to inline after the usage-limit stop (Tasks 8–12 got no per-task review; the final whole-branch review covers them). The calendar grid tests came with the WIP commit, so their RED was not watched in the resumed session.
- Task 9: grapheme clusters via `unicode-segmentation`; the media service decodes artwork on its worker with `Windows.Graphics.Imaging` (WIC) to a cover-cropped square, only while the detail panel is open (no encoded bytes in the snapshot); `can_play_pause` hides the play button like prev/next; transport buttons are glyph-only (the panel kit centres a glyph-only button); the pill's geometry constants live in `vitro_core::widgets` and the bar visual reads them.
- Task 10: `serde_json` in core too (parsing is pure); `wrap_statusline` returns `Wrap::{Wrapped(prev), AlreadyOurs}`; a window whose reset time has passed reads 0 %; "resets now" / "si azzera ora", days as "N d H h" / "N g H h"; the bridge has a 950 ms budget for its whole run, runs the previous command in a kill-on-close job and makes its own standard handles non-inheritable (an orphaned grandchild otherwise held Claude Code's stdout open for seconds); the Claude pill always shows while on ("Claude: no data" first); a failed restore on disable keeps the stored command for a later retry.
- Task 11: the `Shell_NotifyIconGetRect` query is parsed as ManagedShell's `TrayService` reads it (magic, corner 1/2, size, padding, 32-bit hWnd, uID, GUID) and answered only for icons the bar draws, else Explorer answers; the IME indicator has its own layout slot left of the Quick button; it is re-read on start-up, foreground changes, "intl" setting changes and after its own click — a hotkey switch inside another app shows at the next foreground change; short names ENG ITA DEU FRA ESP POR NLD POL РУС JPN KOR CHN, else the first three letters of the locale name; an empty catalog read is retried once after 3 s.

## Parked (minor, from the per-task reviews of Tasks 1–7)
- Panel kit: a commit-phase `?` after the state swap; a reused row's `SetOffset` early return skips cleanup; a restyle failure leaves no rows (each needs a Composition setter to fail).
- Widget pills are not reachable with the keyboard focus mode; their hover is the plate only.
- Audio: `CoInitializeEx` unbalanced on one path; the endpoint is activated again per call; a rename shows on the next change only.
- Brightness: an `IUnsecuredApartment` failure is not logged; a WMI event in the middle of a slider drag can briefly overwrite the pending value.
- Quick panel: Backspace / Delete do not zero the removed password characters; "Connecting" stays when Windows reconnects without leaving the old network (until the failure event).
- Unrelated flake: `crates/win/tests/icons.rs::notepad_icon_by_path` failed once in a full run and passed alone and on rerun.

## Not verified here (see `docs/release-checklist.md`)
Everything live: the quick panel, calendar, both widgets, tray rects, the IME indicator, the overflow Esc hand-back, and the stage-2.5 items listed in the checklist. The ignored stage-1 stock-taskbar tests were not run (they change the real taskbar), only their wait was raised from 2 s to 10 s.

## Measured numbers
Release build, 2026-11-04, one 2560×1080 monitor at 96 DPI, 10 test apps open (`scripts/perf-check.ps1`), all `"pass": true`:
- Idle (`-MeasureStartup`): memory 15.91 MB (bar 14.02 + watchdog 1.78), CPU 0 ms in 30 s, start-up 231 ms, exe 2.09 MB.
- Start panel (`-StartPanel`): open in 12 ms, 16.87 MB open, 16.86 MB 3 s after closing.
- Both widgets on (`-Widgets`): memory 16.07 MB (bar 14.15 + watchdog 1.79), CPU 15.6 ms in 30 s (one WARP redraw tick, within the 50 ms budget).
