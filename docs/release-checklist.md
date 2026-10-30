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
