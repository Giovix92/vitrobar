//! Vitrobar: a glass taskbar for Windows 11.

mod app;
mod bar_window;
mod icon_loader;
mod launcher;
mod menus;
mod notify;
mod shell;
mod window_set;

use std::process::ExitCode;
use std::time::{Duration, Instant};

const USAGE: &str = "usage: vitrobar [--bar] [--debug-state] | --restore";

/// `--restore` (the RunOnce safety net, run at sign-in): Explorer may still be starting, so
/// wait a little for its taskbar before putting it back.
const RESTORE_WAIT: Duration = Duration::from_secs(30);

fn restore_mode() -> ExitCode {
    let Some(path) = shell::record_path() else {
        eprintln!("vitrobar: LOCALAPPDATA is not set");
        return ExitCode::FAILURE;
    };
    if path.exists() {
        let end = Instant::now() + RESTORE_WAIT;
        while !vitro_win::stock_taskbar::stock_taskbar_present() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(250));
        }
    }
    let restored = vitro_win::stock_taskbar::restore_stock_taskbar(&path);
    if vitro_win::stock_taskbar::stock_taskbar_present() {
        // A Vitrobar that ended without cleaning up may have held the tray icons: ask every
        // app to register them with Explorer again.
        vitro_win::tray_host::broadcast_taskbar_created();
    }
    match restored {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vitrobar: could not restore the stock taskbar: {e}");
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    let mut opts = app::Options::default();
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            // No arguments behaves like `--bar` for now.
            "--bar" => {}
            "--debug-state" => opts.debug_state = true,
            "--restore" => return restore_mode(),
            other => {
                eprintln!("vitrobar: unknown argument {other:?}\n{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    match app::run(opts) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // `run` already restored the stock taskbar on its way out; make sure.
            shell::restore_now();
            eprintln!("vitrobar: {e}");
            ExitCode::FAILURE
        }
    }
}
