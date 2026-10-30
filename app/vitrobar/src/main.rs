//! Vitrobar: a glass taskbar for Windows 11.

mod app;
mod bar_window;
mod icon_loader;
mod launcher;
mod notify;
mod window_set;

use std::process::ExitCode;

const USAGE: &str = "usage: vitrobar [--bar] [--debug-state]";

fn main() -> ExitCode {
    let mut opts = app::Options::default();
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            // No arguments behaves like `--bar` for now.
            "--bar" => {}
            "--debug-state" => opts.debug_state = true,
            other => {
                eprintln!("vitrobar: unknown argument {other:?}\n{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    match app::run(opts) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vitrobar: {e}");
            ExitCode::FAILURE
        }
    }
}
