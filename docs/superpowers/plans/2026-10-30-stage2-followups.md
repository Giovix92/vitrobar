# Stage 2 — decisions and follow-ups

Carried out of the stage-2 execution ledger. Input for stage 3+.

## Controller rulings
- Ruling S1: Task 2 adds the 19 settings-page keys itself using Task 3's listed strings — keeps T2 compilable/TDD-able — cost if wrong: none.
- Ruling S2: WM_APP+0x24 = key hook wake, WM_APP+0x25 = file-search wake; FileSearch takes the id as a parameter — avoids collision — cost if wrong: renumber a constant.
- Ruling S3: branch stage2-start in the main checkout (as stage 1, R4) — cost if wrong: none.
- Ruling S4: Tasks 1 and 2 (both small pure-core modules, T2 consumes T1) dispatched to one implementer and reviewed as one unit — saves a context per task — cost if wrong: a larger review diff.
- Ruling S5: panel_layout must adapt rows (pins 1..=3 rows, recent 0..=4) to the placed panel height — finding conflicted with my fixed-offset clarification; spec invariant (everything inside the panel) wins — cost if wrong: fewer pins visible on short screens.
- Ruling S6: focus mode exits on any mouse button-down system-wide via a WH_MOUSE_LL hook installed only while focus mode is on (pass-through, no coordinates kept) — keeps the bar NOACTIVATE and matches "any mouse click exits" — cost if wrong: a second short-lived hook; alternative would be making the bar take the foreground.
- Ruling S7: the WakeLater counter resets only on Wait (my earlier note said Wait/Again, which made the fallback dead code because run() resets spins after WakeLater) — cost if wrong: an occasional extra blocking wait when the inbox refills persistently.

## Parked at the final review
- Parked — I2 priority race (boost before PrefetchApps lowers priority) — Ruling: stage 3 (small window; open still completes) — cost if wrong: one slow first open.
- Parked — empty early catalog read sets apps_at → no Loading row for 60 s — Ruling: stage 3 fix (retry once on empty read) — cost if wrong: right after sign-in, search may find no apps for up to 60 s.
- Parked — M2 toggle-off waits ≤1 s for the hook thread — Ruling: acceptable (bound only, thread exits in ms) — cost if wrong: one 1 s UI pause on that click.
- Parked — Enter blocked for Math while apps load — Ruling: acceptable per finding wording — cost if wrong: minor UX.
- Parked — stage-1 ignored stock_taskbar tests fail after the long concurrency test (Explorer slow) — Ruling: environmental; lengthen the wait in stage 3 — cost if wrong: masked real regression in hiding (live perf shell_ok passes).

## Deferred minors and carries (by task)
- Tasks 1+2: carry → Task 6: format_result prints 10^300 as 301 chars — clip in the panel or switch to scientific notation beyond ~1e15; subsequence matches have no score floor — apply a minimum score for settings/files (e.g. ≥ 300 for files/settings when query < 3 chars).
- Tasks 1+2: minor (deferred): "wifi" keyword; \r\n paste → two spaces, U+2028/2029/zero-width not filtered; weak search tests (Sound index, kind order, caps, ties, files, accented query, 1/0 no Math); per-keystroke folding alloc; no unary plus.
- Task 3: minor (deferred): pin_rows max(1) can cross footer below MIN_PANEL_H (unreachable); recent_title zero-size sentinel when 0 rows (renderer must skip); .bad-* filename collision within same ms unverified.
- Task 4: carry → Task 6: catalog on a worker with cache; launch catalog entries via shell_open("shell:AppsFolder\<aumid>") (launch_pin fails for desktop-app parsing names); respect Start_TrackDocs=0 (show "No recent files"); filter file-search noise.
- Task 4: carry → Task 6: Sleep may fail without SeShutdownPrivilege — handle the error (no crash); dry-run env switch for live power-menu runs (VITROBAR_POWER_DRY_RUN with --debug-state); recent_files off the UI thread (network paths can block); clipboard: allocate before EmptyClipboard (fix in clipboard.rs), owner must be non-null.
- Task 4: minor (deferred): worst-case (never-matching) file query unmeasured — possible crawl fallback; Drop detaches worker after 2 s; shutdown privilege not restored on ExitWindowsEx failure; ignored clipboard test leaves Win+V history entry; small cleanups.
- Task 5: minor (deferred): panel test race if foreground granted; Painter !Sync doc; move_to failure leaves stale state (build glass first); drop-order comment; duplicated class literal; ellipsis untested.
- Task 6: minor (deferred): IME WM_IME_CHAR may be sent to panel wndproc (stage 3 IME); clipboard on UI thread (5×10 ms retry); FileSearch drop can delay exit ≤2 s; tests not red-first.
- Task 6: minor (deferred): key_of(App) uses new catalog indices after refresh (selection may move); fit_results doc comment wrong (dropped apps unreachable); start_pins_initialized set even when bar had no pins at first open; scroll shared AllApps/Search reset unverified; caret-fit O(n²).
- Task 7: minor (deferred, MUST triage in final review): animate_open sets opacity 0 before StartAnimation — on failure panel stays invisible but focused, swallowing input (reset to 1 on error); research() reveal_selected snaps wheel scroll when catalog arrives; redundant StartState::new in open_grid; whitespace query = all apps; no hit/scroll tests with n_top>0. Pending user: fade on glass, first-frame flash, contrast, second monitor.
- Task 8: minor (deferred): mouse events counted in calls/max_us stats (doc); activation_ends_focus ignores any foreground change away from our windows; foreground_ours initial false; Win+click opens Vitrobar Start (stock unverified).
- Task 9: minor (deferred, triage in final): narrow work area (<720 DIP) → close ✕ hit-test mismatch + clipped controls (store width_dip); deferred DPI resize during drag; icon provenance note missing; silent save failure in GUI build; hard-coded hint index in DWM test; flaky vitro-win icons cache test (notepad icon) seen twice now.

## Incidents
- Task 9: implementer DONE_WITH_CONCERNS (02dc278). Changes apply 7–10 ms; mem open ≤16 MB. StockTaskbar switch applies at next start. Placeholder .ico added. INCIDENT: a screenshot caught another app's window on the user's screen — deleted immediately (disclose to user).

## Stage 2.5 (polish after the first real run, plan 2026-10-31-stage2-5-polish.md)
- Rulings: R1 Start ignores a deactivation only when the new foreground is a glass menu; R2 the menu loop consumes only its own deactivation entry; R3/R4 the fading Start panel is not click-through — a click on it drops it at once (that click is lost); R5 the menu loop ends on any foreground change (EVENT_SYSTEM_FOREGROUND WinEvent hook) and when the key hook has output pending; R6 only a WS_EX_TOPMOST window of another process can be claimed as the stock Quick Settings / notification flyout.
- New synthetic input: Esc (SendInput) to close the stock Quick Settings / notification flyout on a second click, only after re-checking it is the recorded foreground window.
- Accessibility: the glass menus lose the native menu's screen-reader support — revisit with UI Automation.
- Watch item: "two apps appeared after opening the stock Start" (user report 2026-10-31, not reproducible). The only Vitrobar path that opens the stock Start is open_stock_start() (Ctrl+Esc via SendInput) when the Start panel fails. If it recurs: which apps, and how Start was opened.
- Parked minors: a key pressed during a menu opened while a stale key flag was pending won't dismiss it (hook stall fallback covers); preview ✕ hot state starts at rest after a rebuild; a ✕ sprite is built for a window whose thumbnail failed (harmless); the app's save prompt may appear behind other windows (as with the stock taskbar); mid-open Start close relies on compositor getters after StopAnimation (visual check on the checklist).
- Unverified live (user's instance was running): Start fade/foreground hand-back, glass menu focus and dismissal, flyout toggle (is the stock flyout WS_EX_TOPMOST?), preview ✕ and middle-click. Perf re-run pending for the same reason.
