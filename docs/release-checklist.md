# Vitrobar release checklist

Manual checks to run on a real Windows 11 machine before every release. Each section names
the task that introduced it; run all of them, in order, on both x64 and ARM64 when possible.

## Bar basics

Introduced by stage 1, Task 9 (live bar). Start from a clean desktop: no Notepad or File
Explorer windows open, no `settings.toml` (or one with an empty pin list).

1. Run `cargo run -p vitrobar -- --bar`. With the pin list empty, the bar shows the Start
   button, one icon per open app, the quick-settings button and the clock (time on the first
   line, short date on the second, in the regional format of Windows).
2. Open Notepad twice and File Explorer once. Notepad shows one icon with 2 dots and Explorer
   one icon with 1 dot, in the order they were opened (Notepad before Explorer). The dot of
   the app in front is wider (12 DIP) and in the accent colour.
3. Click the Explorer icon while Explorer is in front: it minimizes. Click it again: it
   restores and gets the keyboard focus. The bar itself never takes the focus.
4. Close both Notepad windows: the Notepad icon disappears and the bar shrinks back.
5. Change the PC clock (Settings > Time & language > Date & time, turn off "Set time
   automatically", change the minute): the clock updates at once. Then leave it running across
   a minute boundary: the new minute shows without a visible delay.
6. Leave the bar idle for 60 s with no panel open: Task Manager shows 0 % CPU for
   `vitrobar.exe` and no GPU engine use (Details tab, "GPU" column).

Also check while you are here:

- Start opens Vitrobar's Start panel (see "Start menu"), the quick-settings button opens
  Quick Settings, the clock opens the notification centre and calendar.
- An icon that takes long to load shows a soft rounded placeholder first; the bar never
  freezes while icons load.
- An app asking for attention (e.g. a chat app flashing) gets an accent plate that pulses
  twice and stays until the app is activated. With Settings > Accessibility > Visual effects >
  Animation effects off, the plate appears without pulsing.

## Hover and thumbnails

Introduced by stage 1, Task 10. The hover effect is read from `settings.toml` at start-up
(`hover = "plate" | "lift" | "magnify" | "sweep"` in `%APPDATA%\Vitrobar\settings.toml`) until
the settings menu of Task 13 can switch it live: edit the file and restart the bar.

1. Hover each app icon under each of the 4 hover settings and compare with the brainstorm
   mockup (`.superpowers/brainstorm/*/content/hover.html`):
   - **Plate**: a white glass tile (28 %) with a bright top edge fades in behind the icon in
     150 ms; pressing squeezes the icon to 90 %.
   - **Lift**: the icon pops up 5 DIP with a small springy overshoot (220 ms).
   - **Magnify**: the hovered icon grows to 155 % and its neighbours to 122 %, growing up from
     the bottom edge (down from the top edge with a top bar), 180 ms. The grown icon may stand
     out above the glass; the strip above the bar stays click-through when nothing is hovered.
   - **Sweep**: a light streak crosses the icon (shaped like the icon) in 450 ms while the
     icon lifts 2 DIP and grows to 106 % in 220 ms.
2. Open Notepad twice. Hover its icon: after 400 ms a glass strip appears 8 DIP above the bar
   with 2 live thumbnails (200 DIP wide at most 120 DIP high, title under each, cut with "…"
   when too long). Type in one Notepad: its thumbnail updates. Move onto the strip: it stays.
   Move away from both icon and strip: it closes 300 ms later. Hovering another app while the
   strip is open swaps it at once (an app with one window shows a strip of one). Clicking the
   Notepad icon opens the strip at once; clicking a thumbnail brings that window to the front
   and closes the strip.
3. Turn off Settings > Accessibility > Visual effects > Animation effects (the bar follows
   at once): every effect appears in its final state with no motion (Sweep shows no streak).
4. After the strip closes and 2 s pass: Task Manager shows 0 % CPU for `vitrobar.exe` and no
   GPU use.

## Stock taskbar

Introduced by stage 1, Task 11. Vitrobar switches the stock taskbar to auto-hide (Explorer
persists that) and, in the default `stock-taskbar = "hidden"` mode, hides it. The original
auto-hide setting goes to `%LOCALAPPDATA%\Vitrobar\restore.json` and a
`HKCU\Software\Microsoft\Windows\CurrentVersion\RunOnce\VitrobarRestore` value runs
`vitrobar.exe --restore` at the next sign-in if Vitrobar never got to put it back.

Before you start, note the stock taskbar's auto-hide setting (Settings > Personalization >
Taskbar > Taskbar behaviours > "Automatically hide the taskbar"). Try it once with the setting
off and once with it on.

1. Start Vitrobar: the stock taskbar disappears. Maximize a window: it stops above the bar.
   Repeat with `shape = "pill"`, `"islands"` and `"full"` in `settings.toml`, and with
   `position = "top"` (the window starts below the bar). With `reserve-space-floating = false`
   a maximized window reaches the screen edge behind a floating bar.
2. `Stop-Process -Name explorer -Force` (with no File Explorer windows open you care about, or
   with "Restore previous folder windows at logon" on). Within 2 s of the Explorer taskbar
   coming back, it is hidden again; the bar still works and maximized windows still stop above
   it.
3. Play a YouTube video full screen (or press F11 in a browser): the bar on that monitor
   disappears at once (no animation) and any thumbnail strip closes. Leave full screen: the bar
   comes back.
4. Exit Vitrobar (Ctrl+C in its console; later the menu's Exit): the stock taskbar is back,
   with exactly the auto-hide setting you noted, and `restore.json` and the RunOnce value are
   gone.
5. Safety net: start Vitrobar, end it with Task Manager (End task), then run
   `vitrobar.exe --restore` (or sign out and back in): the stock taskbar is back as before.
6. `stock-taskbar = "auto-hide"`: the stock taskbar stays, auto-hidden, under the bar; exit puts
   its setting back.

Automated where safe (run on a dev machine; they change the real shell state and restore it):

- `cargo test -p vitro-win --test stock_taskbar -- --ignored --test-threads=1`: the record is
  written before any change, a crashed run's record is kept, restore is idempotent and removes
  the record and the RunOnce value.
- Steps 1 and 4 and a simulated step 2 (the taskbar is re-shown, then `TaskbarCreated` /
  `WM_SETTINGCHANGE` reach the bar) can be scripted with test windows of your own: start
  `vitrobar.exe --debug-state`, read the `appbar` and `bars` rects from the `vitrobar-state`
  lines, compare with a maximized window's `DWMWA_EXTENDED_FRAME_BOUNDS`, close Vitrobar with
  `WM_CLOSE` on its `VitrobarNotify` window and check `ABM_GETSTATE`, the taskbar's visibility,
  `restore.json` and the RunOnce value. Step 3 needs a window that can take the foreground,
  which a script usually cannot: check it by hand.

## Tray

Introduced by stage 1, Task 12. Vitrobar runs its own (invisible, 0 x 0) `Shell_TrayWnd`, so
every app's `Shell_NotifyIcon` calls reach it first; each call (and every appbar call) is also
forwarded to Explorer's real taskbar, so Explorer's own tray never loses track. Icons whose
identity key is listed in `tray-promoted` (`settings.toml`) sit in the bar; all others are in
the overflow behind the chevron. Use a few real tray apps (Discord, Steam, OneDrive, ...).

1. Start Vitrobar: within 2 s the existing tray icons appear in the overflow (click the
   chevron). Hidden icons (apps that hid theirs) are not shown.
2. In the overflow and (promoted) in the bar: left click, right click (the app's menu opens in
   front, near the cursor) and double click behave as on the stock taskbar. Resting on an icon
   for half a second shows its tooltip; it goes when the cursor leaves.
3. The overflow closes on: the chevron again, Esc, a click anywhere else (another window, the
   desktop, the bar), switching apps.
4. `Stop-Process -Name explorer -Force` (see the Stock taskbar step 2 caution): within 2 s of
   Explorer coming back, the icons are back in Vitrobar.
5. End a tray app with `taskkill /F /IM <app>.exe`: its icon goes the next time the tray
   redraws (hover the bar) or the overflow opens.
6. Exit Vitrobar (and, separately, End task + `vitrobar.exe --restore`): the stock taskbar's
   tray shows every icon again, and no window of class `Shell_TrayWnd` belongs to a Vitrobar
   process.
7. While Vitrobar runs (its `Shell_TrayWnd` is the one `FindWindow` finds), the bar's strip is
   still reserved: a maximized window stops at the bar in Pill (reserve on), Islands and Full,
   at the bottom and at the top, and after every live change of shape or position. Appbar calls
   with a rectangle carry a shared block that only the process owning the found
   `Shell_TrayWnd` can open, so the host re-allocates it for Explorer (a regression here shows
   as the work area not following the bar).

Automated where safe:

- `cargo test -p vitro-win --test tray -- --ignored --test-threads=1`: a test window's
  `Shell_NotifyIconW` Add / SetVersion / Delete reach the host within 1 s, clicks arrive with
  the version-4 packing, a dead owner is swept, `FindWindow` finds ours while it runs and
  Explorer's afterwards, and appbar calls still get Explorer's answer.
- `cargo test -p vitro-win --test tray appbar -- --ignored --test-threads=1` (step 7): an
  appbar registered while the host runs takes exactly its reserved px off `SPI_GETWORKAREA`
  (bottom 80, then 120, after a re-registration, at the top 64) and gives them back on removal.
- Steps 1, 2, 3 and 5 with a tray app of your own (a PowerShell `NotifyIcon` that logs its
  clicks) driven against `vitrobar.exe --debug-state` (the `tray`, `overflow` and `tooltip`
  fields). Only ever click the test app's own icon: clicks are forwarded to real apps.

## Pins and menus

Introduced by stage 1, Task 13. On the first run (`first-run-done` false) Vitrobar copies the
stock taskbar's pinned apps (in its order) and its promoted tray icons into `settings.toml`;
afterwards the right-click menus change settings and pins, saved at once and applied without a
restart. `VITROBAR_SETTINGS=<file>` makes Vitrobar use another settings file: use it for trial
runs so the real `%APPDATA%\Vitrobar\settings.toml` stays as it is.

1. Delete `%APPDATA%\Vitrobar\settings.toml` and start Vitrobar: the bar shows the same pinned
   apps as the stock taskbar, in the same order (shortcuts in `User Pinned\TaskBar`, shortcuts
   pinned from the Start menu, and packaged apps such as Terminal or Teams). Tray icons shown
   outside the stock overflow are in the bar; the rest are in the overflow.
2. Right-click the bar outside the apps and tray icons: Shape, Material (Frosted, Smoked,
   Black, White), Hover effect, Icon size and Position each show a radio check on the current
   value. Pick each value in turn: the bar changes at once (Islands = three glass windows,
   Top = the bar and the reserved strip move to the top edge), and the choice is still there
   after Exit and a new start.
3. Right-click an app: New window, Pin or Unpin (whichever applies), and Close all windows
   when it has windows. Pin keeps it on the bar after its windows close; Unpin removes a pinned
   app without windows; New window starts another instance; Close all closes that app's
   windows and no others (try it with Character Map, not with an app holding unsaved work).
4. Start with Windows toggles its check (the Run entry itself comes with the installer stage).
5. Exit Vitrobar: the stock taskbar and its tray come back, as with any other clean exit.
6. With the Windows display language set to Italian the menus are in Italian; `language = "en"`
   in `settings.toml` overrides it.

Automated where safe: `cargo test -p vitro-win --test pins_import -- --nocapture` prints what
the import would read on this machine (read-only). The menus and their effects were driven
with keyboard input to Vitrobar's own menu, against `vitrobar.exe --debug-state` with
`VITROBAR_SETTINGS` pointing at a temporary file (`settings`, `pins`, `bars` and `appbar` in
the `vitrobar-state` lines; `vitrobar-menu` lines name each choice).

## Monitors and desktops

Introduced by stage 1, Task 14. Every monitor has its own bar with its own windows; pins are
on every bar. Only the primary monitor's bar has the tray area (overflow chevron, tray icons,
quick settings); the other bars show Start, their apps and the clock. Each bar reserves its
own strip of its own monitor. Best tried with two monitors at different scales.

1. Start Vitrobar with two monitors: a bar on each, each with only the windows on its
   monitor; the stock taskbar is gone from both (no sliver of an auto-hidden one at the
   bottom of either monitor, also 5 s later). Maximize a window on each monitor: it stops
   above that monitor's bar.
2. Drag a window from one monitor to the other: its icon moves from one bar to the other
   when it lands (within a fraction of a second).
3. Plug in another monitor: a bar appears on it within 1 s, with only its windows; the
   stock taskbar does not show up on it. Unplug it: its bar goes away and no Vitrobar window
   is left behind (`Get-Process vitrobar` is still running; Spy++ shows no orphan
   `VitrobarBar` window); the other bars do not flicker.
4. Set one monitor to 150 % (Settings > System > Display > Scale): its bar is crisp and sized
   for 150 % (72 px high for a 48 DIP bar); the other bar does not change. Set it back.
5. Win+Ctrl+D (new virtual desktop): every bar shows only that desktop's windows, the pins
   stay. Win+Ctrl+Left / Right: the bars follow. Close the extra desktop.
6. Change which monitor is the main display: the tray area moves to the new primary's bar.
7. Connect to the PC with Remote Desktop and disconnect again, and lock and unlock the PC:
   afterwards the bars are on the right monitors, at the right size, and maximized windows
   stop above them.
8. Full screen (F11 in a browser) on one monitor hides only that monitor's bar.

Automated where safe: run `vitrobar.exe --debug-state` with `VITROBAR_SETTINGS` pointing at a
temporary file; each `vitrobar-state` line has a `monitors` array (id, rect, DPI, tray area,
appbar strip, bar windows, groups with their windows) and `vitrobar-monitors` lines name each
reconcile. Steps 1 and 2 can be scripted with test windows of your own moved with
`SetWindowPos` (run the script Per-Monitor-V2 aware: a system-aware script sees monitors at
another scale in scaled coordinates). Steps 3 and 4 can be simulated on any machine: set
`VITROBAR_FAKE_MONITOR` to a file path and start Vitrobar with `--debug-state` (the variable
is ignored without that flag); writing `left,top,right,bottom,dpi`
into that file and posting `WM_DISPLAYCHANGE` to Vitrobar's `VitrobarNotify` window adds a
made-up monitor (deleting the file and posting again removes it; changing its DPI and posting
again re-lays it out). A virtual desktop switch can be simulated by posting `WM_TIMER` with id
`0x5602` to the same window (a `vitrobar-desktop-switched` line follows). Steps 5 to 8 need a
person.

## Start menu

Introduced by stage 2, Task 6. The Start button opens Vitrobar's own Start panel: a glass
panel (640 x 720 DIP, smaller on short screens) above the Start button (below it with the bar
at the top), with a search box, the pinned apps, recent files, the account and the power
button. For trial runs use `VITROBAR_SETTINGS=<file>` (with `first-run-done = true` and
`start-with-windows = false`). To try the power menu without powering anything off, start
with `--debug-state` and `VITROBAR_POWER_DRY_RUN=1`: every power action is then only recorded
(`vitrobar-power <action>` on stderr) and nothing happens.

1. Click Start: the panel appears at once (well under 150 ms; with `--debug-state` the line
   `vitrobar-start-open ms=<n>` gives the time from the click), takes the keyboard focus (the
   caret shows in the search box) and closes the tray overflow, a thumbnail strip or a
   tooltip that was open. Click Start again: it closes. Click on the desktop or switch apps
   (Alt+Tab): it closes. Esc closes it; Alt+F4 closes only the panel.
2. The first time, "Pinned" shows the bar's pins (up to 6 x 3); icons that are still loading
   show a soft placeholder. The footer shows the account's initial in an accent circle and
   the display name; clicking it opens Settings › Accounts › Your info.
3. "Recent" lists the last 4 files opened (icon, name, folder; click opens the file). With
   Settings › Personalization › Start › "Show recently opened items" off it says "No recent
   files" (and Vitrobar does not read the Recent folder).
4. Type `paint` (or any app name): the pinned grid is replaced at once by results (apps,
   Settings pages, files from the Windows Search index; never files under `node_modules`,
   `.git`, `AppData`, the Recycle Bin or Python environments). The first result is
   highlighted; ↑/↓ move the highlight, Enter runs it and the panel closes. Esc clears the
   text (back to the pinned grid), a second Esc closes. Ctrl+V pastes (line breaks become
   spaces), Ctrl+Backspace deletes a word, ←/→/Home/End move the caret. Emoji typed with
   Win+. arrive whole.
5. Type `2+2`: the first result is "= 4 · Calculator · Press Enter to copy"; Enter puts `4` on
   the clipboard. Huge or tiny results use scientific notation (`10^300` → `1e300`); with
   Italian Windows the decimal separator is a comma.
6. Type `volume` (Italian: `suono`): Settings › Sound is offered; Enter opens it.
7. Click "All apps ›": every installed app, alphabetically, under letter headers. The wheel
   scrolls smoothly (no flicker, no redraw of the whole list); "‹ Back" returns. Clicking an
   app starts it (desktop apps too, through `shell:AppsFolder`).
8. Right-click a pinned app or an app in All apps / the results: Pin to Start / Unpin from
   Start, Pin to bar / Unpin from bar, and Open file location for desktop programs. Each
   change is saved at once (check `settings.toml`) and the bar updates; the panel stays open.
9. Power button: the menu offers Lock, Sleep, Restart, Shut down, Sign out, Switch user. In a
   dry run choose Shut down → `vitrobar-power ShutDown` and nothing happens. On a test machine
   (not in a dry run) check each action once; Sleep on a machine without sleep support fails
   quietly (the panel closes, nothing else happens).
10. Bar at the top (bar menu › Position › Top): the panel opens below the Start button. With
    two monitors, each bar's Start opens the panel on its own monitor (with the work area of
    that monitor); clicking the other monitor's Start moves it there.
11. Memory (Task Manager › Details, private working set of all `vitrobar.exe`): with the
    panel open (All apps scrolled through) under 60 MB; 3 s after closing it under 40 MB.
    Opening and closing 20 times in a row does not make it grow.
12. Change the colour mode or accent colour while the panel is open: it closes; the next
    open uses the new colours.
13. Full-screen grid (`start-style = "full-screen"` in `settings.toml`, or Settings > Start menu):
    Start (button or Windows key) opens a square-cornered, lightly frosted (12 % tint) panel
    over the work area of that monitor, the bar's strip staying visible, with the search box
    at the top centre and every app in a grid (48 px icon, name on up to 2 lines), fading in
    and growing from 98 % in 180 ms (with Animation effects off it just appears). The wheel,
    PageUp/PageDown scroll by rows; arrows move a focus ring (Enter launches the focused
    app); typing `paint` leaves only matching apps, with Calculator/Settings/Files rows above
    them (up from the first grid row reaches those rows); Esc clears the text, a second Esc
    closes; so do deactivation, launching an app and a second click on Start. Memory with the
    grid scrolled through under 60 MB, 3 s after closing under 40 MB.

## Keyboard

Introduced by stage 2, Task 8. A low-level keyboard hook (its own thread, `vitrobar-keys`)
gives the Windows key to Vitrobar: a lone tap opens Vitrobar's Start, Win+1…9 act on the bar's
apps, Win+T / Win+B put a focus ring on the bar's apps / tray. Every other Windows shortcut
stays exactly stock. The hook never stores or logs keys (`--debug-state` shows only
`"keys":{"hook","enabled","focus","calls","max_us"}`). The actions apply to the bar of the
monitor showing the foreground window. These steps need a real keyboard: injected keys are
ignored by the hook by design.

1. Tap the Windows key (left, then right): Vitrobar's Start opens on the monitor of the
   foreground window and takes the keyboard (the caret blinks in the search box; type at
   once). Tap again: it closes. The stock Start never flashes. With the full-screen grid
   (`start-style = "full-screen"`) the grid opens instead.
2. Stock shortcuts, each exactly as without Vitrobar: Win+E (Explorer), Win+R (Run), Win+D
   (desktop, again to come back), Win+Shift+S (snipping), Win+I (Settings), Win+V (clipboard
   history), Win+. (emoji), Win+Tab, Win+←/→ (snap), Ctrl+Win+D / Ctrl+Win+←/→ (virtual
   desktops), Shift+Win+3 (new instance of the 3rd app, Windows' own). After each, the Windows
   key is not stuck: type a letter, it arrives as itself.
3. Win+L locks (user does this). After unlocking, press `3`: a plain 3 is typed (no app
   launches). Same after Ctrl+Alt+Del → Esc.
4. Win+1 with the first bar app not running: it starts. Win+1 again: it comes to the front;
   again while it is in front: it minimizes. An app with several windows: each Win+n brings
   the next of its windows to the front (no thumbnail strip). Win+9 with fewer than 9 apps:
   nothing happens. Holding Win+2 starts the app once, not repeatedly.
5. Win+T: an accent ring (2 DIP) goes around the first app of the bar. ←/→ move it (wrapping),
   Home/End jump, Enter or Space click the app, the menu key opens the app's menu (Shift and
   the other modifiers end the mode), Tab moves to the tray and back, Esc leaves. Typing any
   other key leaves the mode and the key reaches the app in front (e.g. `a` in an editor);
   clicking anywhere on the bar or switching apps leaves it too. While the ring shows, the
   arrows do not reach the app in front.
6. Win+B: the ring goes on the first promoted tray icon (or the overflow chevron when none is
   promoted). Enter on an icon is a left click on it, the menu key a right click; Enter on the
   chevron opens the overflow.
7. With two monitors: with a window in front on the second monitor, Win tap opens Start there,
   Win+1 acts on that bar's first app, Win+T rings that bar (Win+B always rings the primary
   bar, which has the tray).
8. Turn "Windows key opens Vitrobar Start" off (Settings window, Task 9; or
   `win-key-opens-start = false` in `settings.toml` before starting): the Windows key opens
   the stock Start again and Win+1…9 / Win+T / Win+B behave as Windows does; the hook stays
   installed (`"hook":true,"enabled":false`).
9. Quit Vitrobar (bar menu › Exit) while holding nothing: no key is stuck afterwards (Win+E
   still opens Explorer once, letters type normally). Kill `vitrobar.exe --bar` in Task
   Manager: Windows removes the hook with the process; the watchdog's new bar installs it
   again (`"hook":true` in its dump).
10. Busy bar: while the bar is busy (e.g. opening All apps the first time) typing in another
    app never lags; with `--debug-state`, `"max_us"` stays in single or low double digits.

## Safety net

Introduced by stage 1, Task 15. `vitrobar.exe` with no arguments is a small watchdog (no
window, under 2 MB private memory) that starts `vitrobar.exe --bar --parent <pid>`. If the
bar crashes, the watchdog gives the stock taskbar back at once and starts the bar again; the
fourth crash within a minute makes it give up, leaving the stock taskbar. If the watchdog
dies, the bar starts `vitrobar.exe --adopt <bar pid>` a second later. "Start with Windows"
is the `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\Vitrobar` value. Release builds
show no console window; add `--console` to see diagnostics in the console that started them.

1. Kill only the bar process:
   `Get-CimInstance Win32_Process -Filter "Name='vitrobar.exe'" | ? CommandLine -like '*--bar*' | % { Stop-Process -Id $_.ProcessId -Force }`
   → the stock taskbar appears at once (well under 1 s), Vitrobar comes back within 2 s and
   hides it again; tray icons return to Vitrobar.
2. Kill the bar 4 times within a minute → after the fourth the stock taskbar stays (both
   monitors, auto-hide back as it was) and no `vitrobar.exe` is left running.
3. Task Manager › Details › `vitrobar.exe` (the one without `--bar`) › End process tree →
   nothing starts again; the taskbar may stay hidden until the next sign-in. Sign out and in:
   the stock taskbar is normal, auto-hide as originally set (the RunOnce `--restore` ran),
   and `HKCU\...\RunOnce\VitrobarRestore` and `%LOCALAPPDATA%\Vitrobar\restore.json` are gone.
4. Launch `vitrobar.exe` twice → only one bar (the second launch exits at once).
5. Bar menu › "Start with Windows" off → the `Run\Vitrobar` value is removed; on again → it
   is `"<path to vitrobar.exe>"`. Sign in with it on: Vitrobar starts by itself.
6. Kill only the watchdog: about a second later a `vitrobar.exe --adopt <bar pid>` is
   running; killing the bar then still brings the stock taskbar back and the bar again.
7. With Vitrobar running, run `vitrobar.exe --restore` → it does nothing (the taskbar stays
   Vitrobar's; Vitrobar's own exit later restores it).

## Performance (manual, hybrid-GPU laptop)

Introduced by stage 1, Task 16. Run on a laptop with an integrated and a discrete GPU, on
battery and on mains, with a release build. `scripts/perf-check.ps1` opens its own test
windows (Character Map ×3, Paint ×3, File Explorer ×2 on temp folders, Calculator, one
plain test window), uses a throw-away settings file and closes only what it opened; quit
any running Vitrobar first (the script refuses to start next to one).

**Controlled desktop.** The memory gate measures the bar with everything that is open, so
run it on a quiet desktop: no apps open beyond the usual tray ones, and do not open or
close apps while it runs. Do not open Calculator yourself meanwhile either: the check takes
the first Calculator process that appears after its launch as its own (calc.exe only asks
Windows to start the packaged app). Explorer windows are recognised by their unique temp
folder name, Character Map and Paint by the process the check started.

1. `pwsh scripts/perf-check.ps1 -Exe target\release\vitrobar.exe -MeasureStartup` → one
   line of JSON with `"pass": true`:
   - `memory_mb` < 40: the largest sum of private bytes (`PrivateMemorySize64`, the
     commit charge) of all Vitrobar processes (watchdog + bar) over 30 s, bar idle, panels
     closed, 10 apps open. Expected since the WARP drawing device (Task 16): about 14–15 MB
     (bar ~12.5–13, watchdog ~1.7); a number near 40 means the content is drawn on a
     hardware device again;
   - `cpu_ms` ≤ 50: CPU time of all Vitrobar processes over those 30 s (with WARP a clock
     redraw in the window can show as one 15.6 ms tick);
   - `exe_mb` < 5;
   - `startup_ms` < 300: launch of `vitrobar.exe` (the watchdog) until a visible
     `VitrobarBar` window exists (polled every ~2 ms through `EnumWindows`). Expected about
     100 ms (measured 82–101 ms): the bar shows first and takes the stock taskbar over right after (ruling R12),
     so the stock taskbar may stay visible under the bar for about half a second;
   - `shell_ok` true: afterwards Explorer's `Shell_TrayWnd` is the only one and visible.
   `failures` names every budget missed, and every test window left open. Run it 3 times;
   all 3 must pass.
2. GPU idle: with the bar idle and no panel open, Task Manager › Details › "GPU" column shows
   0 for `vitrobar.exe` for 60 s. Repeat for each material (`material = "frosted"`,
   `"liquid"`, `"smoked"`, `"solid-black"`, `"solid-white"` in `settings.toml`, restart the
   bar after each change).
3. Discrete GPU: it stays in low power / off while Vitrobar runs (NVIDIA or AMD control
   panel, or `Get-Counter '\GPU Engine(*)\Utilization Percentage'` lists no engine with the
   discrete GPU's LUID for the `vitrobar.exe` process id). The Performance tab of Task
   Manager shows "GPU 1" (discrete) at 0 % with only Vitrobar running. Vitrobar's own
   drawing uses WARP (no GPU at all); the glass is composited by DWM.
