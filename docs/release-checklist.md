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

- Start opens the Windows Start menu, the quick-settings button opens Quick Settings, the
  clock opens the notification centre and calendar.
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

Automated where safe:

- `cargo test -p vitro-win --test tray -- --ignored --test-threads=1`: a test window's
  `Shell_NotifyIconW` Add / SetVersion / Delete reach the host within 1 s, clicks arrive with
  the version-4 packing, a dead owner is swept, `FindWindow` finds ours while it runs and
  Explorer's afterwards, and appbar calls still get Explorer's answer.
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
