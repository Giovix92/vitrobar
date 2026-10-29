# Vitrobar — design

Date: 2026-10-30 · Status: draft for approval · License: GPL-3.0

## 1. Intent

Vitrobar is a free, open-source **replacement for the Windows 11 taskbar** whose reason to exist is *looks*: a polished glass bar, free and open source. It is built from scratch as its own public project, open to stars and contributions.

It must do everything people use the taskbar for every day — Start, pinned apps, open windows, tray, clock, quick settings, notifications — and do it while being **lighter than the taskbar it replaces**.

### Why, and what failure looks like

The owner's words for why someone would uninstall it after a day: *"if it's laggy or eats too much ram, or it hogs my gpu (or it eats my laptop battery cause it runs too heavy on the gpu / uses the external gpu)"*. Performance is therefore the primary anti-goal, ahead of feature count.

### Goals
- Looks polished; frosted glass by default, real liquid-glass refraction as an option.
- Complete taskbar replacement from launch (see §5): nothing the user reaches for is missing.
- Lightweight and fast, with measured budgets (§3) enforced in CI.
- Almost no-hassle install and configuration: works well with zero setup; every option discoverable in one settings window.

### Non-goals (v1)
- Windows 10 support (Windows 11 only at launch).
- Different bar configuration per virtual desktop (later; relies on undocumented, fragile APIs).
- ChatGPT and Cursor usage tiles (left out until a clean, non-cookie way exists).
- Web search in the Start menu; airplane-mode / night-light / hotspot toggles.
- A long tail of third-party service widgets (Stripe, Shopify, screen time, etc.).

### Anti-goals (failure even if it "works")
- Noticeable lag, idle CPU/GPU use, or high RAM.
- Waking a laptop's discrete/external GPU, or draining battery.
- Ever leaving the user without a working taskbar (crash, uninstall, update).
- Permanently modifying Windows.
- Telemetry or hidden network traffic.

## 2. Prior art and positioning

| Project | What it is | Relation |
|---|---|---|
| Seelen UI | AGPL shell (Tauri/web) with a dock that can be full-width or floating | Closest overlap; web-based, heavier. Vitrobar differentiates on weight + glass |
| glassbar (pikammmmm/custom-windows-taskbar) | MIT Rust/Tauri glass dock, v0.1, coexists with taskbar | Very early; not a replacement |
| RetroBar, Cairo Desktop | Open-source taskbar/shell replacements (retro look) | No visual overlap; **reference implementations** for tray hosting, AppBar, TaskbarList (via ManagedShell) |
| TranslucentTB, Windhawk Taskbar Styler | Restyle the stock taskbar | Not replacements |
| PowerToys Dock (in dev) | Extra bar alongside the taskbar | Not a replacement |

No existing open-source project does true refraction on Windows.

## 3. Hard constraints

### Technology (builder's choice was delegated; decided as)
- **Rust**, using the `windows` crate. Native Win32 windows, **Direct3D 11 + DirectComposition** for rendering, DWM for backdrops/thumbnails.
- **No webview, no .NET, no runtime dependency.** Single `.exe`.
- GPU device created on the **minimum-power adapter** (`DXGI_GPU_PREFERENCE_MINIMUM_POWER`); never the discrete GPU.

### Performance budgets (verified before every release, with 10 apps open)
| Metric | Budget |
|---|---|
| RAM (private working set) | < 40 MB |
| CPU when idle | 0% (event-driven; no polling loops, no timers except the clock tick) |
| GPU when idle | 0%, liquid glass included |
| Time to bar on screen | < 300 ms |
| Download | < 5 MB per architecture |

Rendering rules that follow from this:
- Redraw **only on change** (hover, window events, clock tick, setting change). No continuous frame loop.
- Animations 150–250 ms, capped to the display refresh rate, then back to idle. Respect Windows "Animation effects: off".
- Liquid glass refracts the **wallpaper**, loaded once and cached; refraction is recomputed only when the bar moves/resizes or the wallpaper changes. No live screen capture.
- On battery or in Windows battery saver, liquid glass falls back to frosted automatically (user can disable this).
- **Liquid glass with a window behind the bar** (owner's decision): by default, while any window overlaps the bar, the bar **crossfades smoothly to frosted glass** and back to liquid glass when it leaves. Opt-in setting **"Live refraction"** instead bends the real content behind the bar (live capture); the setting is labelled as costing GPU and battery, is exempt from the GPU-idle budget only while enabled, and still obeys the battery fallback.
- A perf check runs in CI on every build and fails it when budgets are exceeded. *(builder's call)* Hosted CI runners have no real GPU/DWM, so CI enforces RAM, idle CPU and binary size; GPU-idle, startup time and "discrete GPU never wakes" are checked on a real hybrid-GPU laptop as a mandatory step of the release checklist.
- *(builder's call)* The 40 MB budget is the **total of all Vitrobar processes** (watchdog included), measured with the bar idle and panels closed. With the Start menu or a panel open, a transient budget of < 60 MB applies; memory must return under 40 MB after closing.

### Platform
- Windows 11 (x64 and **ARM64** builds).
- Runs unelevated.

### How deep into Windows (owner's decision)
- Allowed: official APIs, plus **undocumented Windows interfaces and registry formats used read-only or through their normal call path** (e.g. `IPolicyConfig` for audio output, the taskbar pins blob for import, jump-list stores). Each is guarded by Windows-build checks and fails cleanly: the feature hides itself rather than misbehaving.
- **Not allowed: injecting code/DLLs into Explorer or other processes**, or patching system files.
- A launch feature that turns out to be impossible without injection (most likely: receiving apps' `ITaskbarList3` progress/badges/flashing while Explorer runs) ships reduced or without that part, and the gap is documented.

## 4. Look and feel (decided)

### Shapes — all three ship, chosen in settings
1. **Pill**: one centered floating capsule; width grows with content.
2. **Islands**: three floating capsules — Start (left), apps (center), tray+clock (right).
3. **Full width**: edge-to-edge glass bar.

Default shape is left to the builder (suggestion: pill, since it is the signature look).

### Materials — all ship, chosen in settings
- **Frosted glass — default.** Blur + saturation of what's behind (acrylic-like).
- **Liquid glass — premium option.** Clear glass that bends the wallpaper, with light-catching edges.
- Dark smoked glass, solid black, solid white.
- Light/dark follows Windows automatically; accent colour follows the Windows accent unless the user picks one.
- Windows "Transparency effects: off" → solid style; high-contrast mode respected.

Mockups of shapes, materials, Start menu and hover effects are in `.superpowers/brainstorm/*/content/` (browser approximations, not specs).

### Hover effects — all four ship, chosen in settings
- **Glass plate — default.** A brighter glass tile lights behind the hovered icon; icon squishes slightly on click.
- Springy lift (pop up with bounce; open-dot stretches to a line).
- macOS magnify (hovered icon grows, neighbours less; animates only while the mouse moves over the bar).
- Light sweep (a streak of light glides across the icon, tiny lift).

### Other defaults (builder's calls, stated to the owner, not objected to)
- Position bottom by default; drag to top edge snaps it there and panels open downward.
- Icon sizes small/medium/large, medium default.
- One glass **settings window** with live preview; no config file editing required, no restart needed; settings persisted as one plain file (easy backup).
- UI in **English and Italian** at launch, structured so contributors can add languages.

## 5. Features at launch

### 5.1 Bar and windows
- Start button, pinned apps, open windows, tray, clock.
- **One icon per app (grouped).** Dots under the icon show window count (up to 3).
- **Hover** shows a glass strip of **live thumbnails** (DWM thumbnails) with titles; click a thumbnail to focus.
- **Click**: 1 window → focus, or minimize if already focused; several → open the same thumbnail strip.
- Grouping by **AppUserModelID**, not executable (Chrome profiles, PWAs, Store apps stay distinct, matching Windows).
- Progress bars, overlay badges and attention flashing (apps' `ITaskbarList3` calls must reach Vitrobar), within the no-injection rule of §3.
- Right-click: jump lists (recent files, tasks), pin/unpin, new window, close.
- Drag & drop: reorder pinned apps, drop a file onto an icon to open it with that app, pin from the Start menu.
- Show-desktop strip at the far right.
- First run imports the user's current Windows taskbar pins.

### 5.2 Multi-monitor and virtual desktops
- A bar on **every monitor**; each shows only the windows on that monitor.
- Each **virtual desktop** shows only its own windows; pinned apps are the same everywhere (public `IVirtualDesktopManager` is sufficient).
- Per-monitor DPI; handles monitor hot-plug, DPI changes, sleep/resume, Explorer restarts, RDP.

### 5.3 Start menu (Vitrobar's own, same glass)
- **Default:** opens as a panel — search box on top, pinned-apps grid, recent files, account + power at the bottom. No ads, no "recommended". **Typing instantly replaces the grid with search results** (apps, files, Settings pages, quick math); Enter launches the top result.
- **Option:** full-screen app grid (Launchpad-style) over blurred wallpaper with search on top.
- The **Windows key** opens Vitrobar's Start menu.
- Power menu: lock, sleep, restart, shut down, sign out, switch user.
- File search uses the Windows Search index (no own indexer).

### 5.4 Tray and quick panels (Vitrobar's own glass panels)
- Vitrobar hosts tray icons itself; overflow chevron with drag in/out.
- Quick panel: **Wi-Fi, Bluetooth, volume (+ output device), brightness, battery/power mode**. Depth *(builder's call)*:
  - Wi-Fi: on/off + network list + connect with password; enterprise and captive-portal networks hand off to Windows Settings.
  - Bluetooth: on/off + list of paired devices with connect/disconnect; new pairing hands off to Windows Settings.
  - Brightness: built-in screens, plus external monitors via DDC/CI where supported.
- **Notification list** panel, with unread badge. Requires package identity → Vitrobar ships a **sparse package** registered by the installer (still one-click). Toast pop-ups keep working natively either way.
- Clock opens a calendar flyout. Input-language (IME) indicator in the tray.

### 5.5 Keyboard and accessibility
- Win key, Win+1…9, Win+T, Win+B.
- Full keyboard navigation; UI Automation support for screen readers.

### 5.6 Coexisting with the screen
- Full-width style reserves screen space (AppBar) so maximized windows stop above it; floating styles: reserve or overlap (setting). Auto-hide option.
- Hides on a monitor when a fullscreen app/game/video is active there.

### 5.7 Widgets — all **off by default**, enabled later by the user
A disabled widget loads nothing and makes no network calls. Widgets appear as small pills in the bar; click opens a bigger glass detail panel (builder's call).
- **Music:** current track, artist, play/pause/skip, via Windows media sessions (GSMTC). No account, no network.
- **Calendar:** next event + time until it starts, from a pasted private **iCal URL** (Google/Outlook/iCloud), refreshed every 15 min, read-only.
- **Claude usage:** one tile for the Pro/Max 5-hour and weekly limits (shared by Claude Code and claude.ai), read via Claude Code's existing local OAuth login — the same source Claude Code's `/usage` uses. No setup. **Known risk:** undocumented endpoint; may break and need updating. If Claude Code isn't installed/logged in, the tile says so. *(builder's call)* **Read-only:** Vitrobar never refreshes the token or writes Claude Code's credentials file; when the token has expired the tile says "open Claude Code to refresh". Before release, check that reading this token from a third-party app fits Anthropic's terms; if not, drop the tile.

## 6. Install, run, update, uninstall
- Distribution: signed installer on GitHub Releases, `winget install vitrobar`, portable `.zip`.
- **Code signing:** apply to SignPath Foundation (free for OSS); fallback Microsoft Trusted Signing (~€10/month). Needed for the sparse package and to avoid SmartScreen warnings.
- Starts with Windows by default (toggle).
- Daily update check against GitHub; one-click update offered, **never silent**.
- Hides the Windows taskbar while running (auto-hide or fully hidden); **never modifies Windows permanently**.
- **Safety net:** if Vitrobar crashes, the Windows taskbar is restored immediately and Vitrobar restarts itself.
- Uninstall restores everything exactly and leaves nothing behind.

## 7. Privacy
No account, no Vitrobar server, no telemetry. Network use only: update check (GitHub), and enabled widgets (each to its own service). Crash reports opt-in, sent only on explicit click.

## 8. Public project
- GPL-3.0. English README with demo GIF, CONTRIBUTING guide, CI builds for every change, GitHub Pages site, app icon/logo.

## 9. Build order (builder's call, each stage usable on its own)
1. The bar: shapes, frosted glass, pinned/open apps, tray, clock, hiding/restoring the Windows taskbar + safety net.
2. Start menu.
3. Quick panels + notifications (sparse package).
4. Liquid glass.
5. Widgets.
Release hardening (installer, signing, winget, a11y, perf CI) runs alongside, finished before the first public release.

*(builder's call)* **Version 1.0 — the public launch — ships after stage 5**, because the owner wants it complete from launch. Earlier stages may be published as GitHub pre-releases labelled "preview" for testers. Apply to SignPath at the start of stage 3 (the sparse package needs signing).

## 10. Left to the builder
- Default shape; exact sizes, radii, colours, easing curves.
- Frosted-glass implementation (system backdrop / accent blur on a shaped window vs. own blur), as long as it blurs what is actually behind the bar and meets the budgets.
- Refraction shader design.
- Tray-hosting technique (study RetroBar/ManagedShell).
- Settings file format, module structure, test strategy.
- Calendar flyout contents beyond a month view with today's events.

## 11. Known technical risks
- **Tray hosting and receiving `ITaskbarList3`** while Explorer still runs: the hardest part; mitigated by existing open-source implementations.
- **Windows key interception** needs a low-level keyboard hook.
- **UIPI:** an unelevated shell has limits interacting with elevated windows. *(builder's call)* The installed build uses a `uiAccess=true` manifest (signed, under Program Files) so the Win key and hotkeys work over elevated windows; the portable `.zip` cannot, and its README says so: while an elevated window is focused, the Win key falls back to the stock Start menu.
- **Claude usage endpoint** is undocumented.
- **SignPath approval** is not guaranteed.
