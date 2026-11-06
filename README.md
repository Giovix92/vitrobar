<p align="center"><img src="assets/logo.png" width="128" height="128" alt="Vitrobar logo"></p>

# Vitrobar

Vitrobar is a free, open-source replacement for the Windows 11 taskbar, with a glass look and a small resource footprint. It is written in Rust and licensed under GPL-3.0.

Status: pre-alpha. Website: <https://giovix92.com/vitrobar/>. Downloads (x64 and ARM64): [Releases](https://github.com/Giovix92/vitrobar/releases).

## Features

- A glass taskbar for Windows 11 (x64 and ARM64) that replaces the stock one: pinned and running apps with window dots, a thumbnail strip on hover, tray icons with an overflow, a clock, and right-click menus (pin, unpin, new window, close all windows).
- Bar shape, material, icon size, top or bottom position, theme and accent colour; one bar per monitor.
- A Start panel of its own: pinned apps, recent files, an "All apps" list, search over apps, Settings pages and files (from the Windows Search index), a calculator in the search box, the account and a power menu. A full-screen app grid is available as an alternative style.
- The Windows key opens Vitrobar's Start; Win+1...9 act on the bar's apps and Win+T / Win+B move a focus ring over the apps and the tray. Other Windows shortcuts stay as they are.
- A settings window for the look, behaviour and language (English and Italian), applied live.
- A quick panel of its own on the Quick button: volume and output device, brightness, Wi-Fi, Bluetooth, battery and power mode.
- A calendar panel on the clock, with a button to the Windows notification centre; an input-language indicator when more than one keyboard layout is installed.
- Optional widgets, off by default: the music playing now (play / pause / skip, from Windows media sessions) and your Claude 5-hour and weekly limits (from Claude Code's own status line, no account and no network).
- A watchdog gives the stock taskbar back if the bar crashes or exits.

## Build and test

```
cargo build --release
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release --target aarch64-pc-windows-msvc
pwsh scripts/perf-check.ps1 -Exe target/release/vitrobar.exe -MeasureStartup
pwsh scripts/perf-check.ps1 -Exe target/release/vitrobar.exe -StartPanel
```

The manual checks to run before a release are in `docs/release-checklist.md`.

## Artwork

The logo is original artwork made for this project: a dusk lake on a rounded tile, where the sun sets behind a frosted glass bar holding three app tiles. Its vector sources are `assets/logo.svg` (48 px and up) and `assets/logo-small.svg` (a simplified drawing for 16 to 32 px), drawn from plain shapes and gradients, with no third-party images, fonts or logos. `app/vitrobar/vitrobar.ico` (16, 20, 24, 32, 40, 48, 64, 128 and 256 px), `assets/logo.png`, the website's `site/logo.svg`, `site/favicon.ico` and `site/apple-touch-icon.png` are rendered from them. Like the rest of the project, the logo is licensed under GPL-3.0.
