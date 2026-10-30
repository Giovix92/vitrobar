# Stage 1 — decisions and follow-ups

Carried out of the stage-1 execution ledger (workspace deleted after merge-readiness). Input for the stage 2+ plans.

## Owner decisions
- USER DECISION D1 (2026-10-30): draw bar content with WARP; hardware min-power device only while liquid glass is on (stage 4). Spec §3 updated by controller.

## Controller rulings
- | 5 internal | Sweep 450 ms vs Global "animations 150–250 ms" | conflict | Ruling R1 |
- | 6↔16 | bar window class for FindWindow | class name not pinned | Ruling R2 |
- | 7 internal | hung_window test uses a window in the test process; GetWindowTextW sends WM_GETTEXT to same-process windows → would block | conflict | Ruling R3 |
- Ruling R1: Sweep keeps 450 ms for the light streak only; all motion (lift/scale) stays ≤ 250 ms — the streak is a decorative highlight, not movement — cost if wrong: change one constant.
- Ruling R2: bar window class name is `VitrobarBar` (Task 6 defines `pub const BAR_CLASS: &str = "VitrobarBar"` in app/vitrobar/src/bar_window.rs; Task 16 polls it) — cost if wrong: rename a constant.
- Ruling R3: read_window_info reads titles with `InternalGetWindowText` (never sends messages) instead of GetWindowTextW — satisfies Review Focus #3 for same- and cross-process hung windows — cost if wrong: none expected, API is stable since XP.
- Ruling R4: work on branch `stage1-bar` in the main checkout instead of a separate worktree — fresh repo with only docs, nothing to isolate from — cost if wrong: none.
- Ruling R5: Task 0 Step 1 (toolchain install via winget) is environment setup done by the controller, since it may need a UAC prompt only the user can approve — cost if wrong: none.
- Ruling R7: the 40 MB RAM budget means private bytes (PrivateMemorySize64 = commit) summed over Vitrobar processes, as Task 16 already measures — working set includes shared driver/system DLL pages Vitrobar doesn't own — cost if wrong: perf check passes while Task Manager "Memory" column (private WS) shows similar numbers anyway; low.
- Ruling R8: RAM is ~40 MB private bytes already (26 MB GPU driver). Continue building; Task 16 gets an explicit memory-reduction investigation (lazy/shared device, D2D resource sizes, trimming) and, if 40 MB stays unreachable, the controller brings measured data to the user to adjust the budget — cost if wrong: later rework of device usage.
- Ruling R9: the flaky vitro-win tests get a dedicated debugging fix (systematic-debugging) right after Task 10's review, before Task 11 — flaky tests would make every later review's evidence worthless — cost if wrong: one extra dispatch.
- Ruling R10: tray host forwards WM_COPYDATA, WM_COMMAND, 0x0400–0xBFFF, and registered ≥0xC000 except TaskbarCreated/SHELLHOOK; WM_SETTINGCHANGE only for "TraySettings"; never other system msgs — doubled broadcasts are worse than a rare missed private message — cost if wrong: some Explorer-private feature misbehaves; revisit with ManagedShell parity.
- Ruling R11: dispatch a dedicated regression fix F2 (systematic-debugging) for the appbar reservation right after Task 13's review — it breaks a core spec behaviour and Task 14 builds on the AppBar — cost if wrong: none.
- Ruling R12: startup budget miss is a technical ordering problem — show the bars first, then take over the stock taskbar (ABM_SETSTATE) off the critical path; dispatch after Task 16 review — cost if wrong: brief overlap of stock taskbar and bar at start-up (~0.5 s).
- Final review: Ready with fixes (I1 forged-pointer free via WM_APP lParam; I2 --restore disarms RunOnce while bar runs; I3 inbox not re-checked after flush). One fix wave dispatched (final-findings.md). Ruling R13: VITROBAR_FAKE_MONITOR only honoured with an explicit debug flag — checklist needs it — cost if wrong: none.

## Parked at the final review
- Parked — I3 WM_NULL path doesn't truly yield; a self-refilling inbox could spin — Ruling: park for stage 2 (measured cpu 0, no growth); fix by falling back to a blocking wait after N wake-later rounds — cost if wrong: CPU spin in a rare feedback case.
- Parked — batch draining weakens WinEvent vs shell-hook ordering — Ruling: harmless today (Created/Changed equivalent) — cost if wrong: transient mis-grouping.
- Parked — WakeQueue::wake_failed race needs single producer — Ruling: single producer per queue today; add doc note later — cost if wrong: one lost wake-up.
- Parked — corrupt settings file adds a .bad copy every launch — Ruling: fix with stage-2 settings window — cost if wrong: disk clutter.
- Parked — --restore re-arm without re-checking bar_running under lock — Ruling: safe (RunOnce restores next sign-in) — cost if wrong: restore delayed to next sign-in.

## Deferred minors (by task)
- Task 0: minor (deferred): no cargo cache in CI (Swatinem/rust-cache).
- Task 0: minor (deferred): verify windows-latest has ARM64 MSVC libs once a GitHub remote exists.
- Task 1: minor (deferred): save() doesn't create parent dir — carry to Task 13 (app must create %APPDATA%\Vitrobar or save must create_dir_all).
- Task 1: minor (deferred): .bad copy result ignored / overwrites earlier .bad; corrupt test doesn't assert original untouched; one bad value resets all settings.
- Task 2: minor (deferred): monitors < ~340 DIP → islands/full overlap; k==0 → no `more` (apps unreachable); portrait test asserts only no-overlap; RED was retroactive stub.
- Task 3: minor (deferred): pin_matches allocates lowercase per compare; click_action unwrap_or(0) on impossible unpinned-empty group; empty pin → Path(""); missing same-aumid-different-path merge test.
- Task 4: minor (deferred): guid-matched Modify doesn't update stored hwnd/uid (affects remove_owner) — carry to Task 12; one-sided guid falls back to (hwnd,uid); no guid dup/delete/remove_owner tests.
- Task 5: minor (deferred): BgraImage::new/is_valid extra API (unchecked w*h*4); HoverStyle single duration_ms → renderer (Task 10) must cap Sweep motion at 220/250 ms per R1; Smoked border/highlight inferred.
- Task 6: minor (deferred → carry to Task 9 which edits app.rs): WAIT_FAILED in MsgWaitForMultipleObjectsEx would busy-spin; recreate() failure never retried; RoInitialize unbalanced; GetMonitorInfoW provenance (pass MONITORINFOEXW ptr); register_class caches only first result; system_theme test asserts nothing real; adapter compared by description not LUID; CTRL_CLOSE race; solid styles draw border (defensible).
- Task 7: minor (deferred → Task 9): RUDEAPPACTIVATED yields only FullscreenChanged (app must re-query foreground); boxed events leak if notify destroyed first; Desktops::new does CoInitializeEx STA on caller thread; start() loses GetLastError (E_FAIL); coalescing bound ~100-200 ms; no unit tests for translate_shell_hook / debounce. Manual check: SHGetPropertyStoreForWindow against a suspended foreign process (add to release checklist).
- Task 8: minor (deferred → Task 9): load_icon/GetImage is synchronous and can stall (busy windows 100 ms each, network paths) — Task 9 must load icons off the render path or with negative backoff; cache cap is entries not bytes.
- Task 9: minor (deferred): backdrop tree rebuilt on every pill resize (resize instead); update() runs on all bars; bars.pop()→PostQuitMessage latent (Task 14); set_flashing unbounded; no client_to_dip test at 125/150 %; no WM_DPICHANGED/WM_DISPLAYCHANGE (Task 14).
- Task 9: minor (deferred): clock re-arm fallback only on next wake; inbox not re-checked before blocking; AUMID launch branch untested.
- Task 10: minor (deferred): DwmUpdateThumbnailProperties result discarded; animate() dispatch on strings with silent arm + easing alloc per animation; hover/strip state machine untested; hover_margin uses metrics().icon not drawn px size.
- Fix-F1: minor (deferred): a pinned foreground window can seed a wrong current-desktop id until next invalidate; extra GetWindowDesktopId COM call per query.
- Task 11: minor (deferred): --restore polls 250 ms ×30 s (one-shot, comment it); unreachable CTRL_LOGOFF comment; Task View may count as fullscreen (check manually).
- Task 12: minor (deferred → stage 3): answer Shell_NotifyIconGetRect (dwData 3) with our own icon rect so app flyouts open near Vitrobar; overflow should restore previous foreground on Esc; v4 lParam sign-extension vs MAKELPARAM; redundant checks in forward().
- Task 13: minor (deferred): no-AUMID window matches AUMID pin by exe; pin from app menu named after exe stem; first-run blob scan O(bytes×names) on UI thread — measure vs 300 ms (Task 16); i18n test only counts items; SAFETY comment alignment.
- Task 14: minor (deferred): resync() moves bars to old rect then refresh moves to new (double SetWindowPos, no reassert_topmost); VITROBAR_FAKE_MONITOR debug hook ships in release — gate behind debug flag/cfg; monitors.rs doc overclaims id stability; reconcile_monitors untested beyond plan; env-mutating test; hot-plugged Shell_SecondaryTrayWnd created >1 s later relies on WM_SETTINGCHANGE.
- Task 15: minor (deferred): restore_now off-UI-thread can stall a UI-thread take-over up to 10 s (bounded, safe); broadcast_taskbar_created under lock (check it's non-blocking); aborted re-take-over keeps `active`; ShellLock should be !Send; unquoted Run value with spaces misread; `--bar --bar` message wording; repair_autostart may repoint to a dev build.
- Task 16: minor (deferred): CPU measure includes live-desktop reactions / per-PID deltas; ea877ff commit message contradicted by f78019c (history, not pushed); CI perf job unverified until a remote exists.
- Task 16: minor (deferred): Explorer "full path in title" breaks folder-title match (use Contains); child-scan PID-reuse risk (compare start times); checklist says ~200 ms startup (actual 82–101); first frame without tray area (pop).

## Carried items
- Task 11: carry → Task 15: restore only shows taskbar when a record exists (multi-instance / RunOnce-at-sign-in race leaves Shell_TrayWnd hidden) — guard drop must always set_tray_visible(true) in Hidden mode; --restore must hand off/refuse when VitrobarNotify exists; unreadable record overwritten with current (own) autohide state — treat InvalidData as unknown.
- Task 11: carry → Task 14: WM_DISPLAYCHANGE re-claims strip with start-up monitor rect; hot-plug: arm TIMER_REASSERT on WM_DISPLAYCHANGE for new Shell_SecondaryTrayWnd.
