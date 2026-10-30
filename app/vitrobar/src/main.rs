//! Vitrobar: a glass taskbar for Windows 11.

mod app;
mod bar_window;

use std::process::ExitCode;

const USAGE: &str = "usage: vitrobar [--bar]";

fn main() -> ExitCode {
    let arg = std::env::args().nth(1);
    match arg.as_deref() {
        // No arguments behaves like `--bar` for now.
        None | Some("--bar") => match app::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("vitrobar: {e}");
                ExitCode::FAILURE
            }
        },
        Some(other) => {
            eprintln!("vitrobar: unknown argument {other:?}\n{USAGE}");
            ExitCode::from(2)
        }
    }
}
