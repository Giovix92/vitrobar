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
