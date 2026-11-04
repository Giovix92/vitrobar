//! Vitrobar: a glass taskbar for Windows 11.
//!
//! Release builds are GUI-subsystem programs (no console window, also not for the
//! watchdog); `--console` attaches to the console of the process that started Vitrobar to
//! show its diagnostics. Debug builds keep their console.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod bar_window;
mod calendar_panel;
mod icon_loader;
mod keys;
mod launcher;
mod menus;
mod notify;
mod quick;
mod settings_window;
mod shell;
mod start;
mod watchdog;
mod widgets;
mod window_set;

use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use vitro_win::stock_taskbar::{ShellLock, SHELL_LOCK_TIMEOUT_MS};

const USAGE: &str =
    "usage: vitrobar [--debug-state] [--console]   (the watchdog, which starts the bar)\n\
       vitrobar --bar [--parent <pid>] [--debug-state] [--console]\n\
       vitrobar --adopt <bar pid> [--debug-state] [--console]\n\
       vitrobar --restore";

/// `--restore` (the RunOnce safety net, run at sign-in): Explorer may still be starting, so
/// wait a little for its taskbar before putting it back.
const RESTORE_WAIT: Duration = Duration::from_secs(30);

fn restore_mode() -> ExitCode {
    let Some(path) = shell::record_path() else {
        eprintln!("vitrobar: LOCALAPPDATA is not set");
        return ExitCode::FAILURE;
    };
    restore_mode_at(&path)
}

/// A live bar owns the shell: leave its record alone, but put back the RunOnce value that
/// Windows deleted before starting this `--restore`, so the record keeps its safety net.
/// Called with [`ShellLock`] held.
fn leave_to_the_live_bar(path: &Path) -> ExitCode {
    if let Err(e) = vitro_win::stock_taskbar::rearm_runonce_if_record(path) {
        eprintln!("vitrobar: could not set the RunOnce safety net again: {e}");
    }
    ExitCode::SUCCESS
}

fn restore_mode_at(path: &Path) -> ExitCode {
    let bar_running = || watchdog::mutex_exists(watchdog::BAR_MUTEX);
    if bar_running() {
        // Restoring now would delete the live bar's record and let it hide the taskbar for
        // good. But Windows deleted the RunOnce value that started us before running it, so
        // the record would be left without its safety net: put the value back.
        let _shell = ShellLock::acquire(SHELL_LOCK_TIMEOUT_MS);
        return leave_to_the_live_bar(path);
    }
    if path.exists() {
        let end = Instant::now() + RESTORE_WAIT;
        while !vitro_win::stock_taskbar::stock_taskbar_present() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(250));
        }
    }
    // The check and the restore under one cross-process lock, which a bar's take-over also
    // holds: an autostarted bar starting right now either finishes its take-over first (and
    // is seen here) or starts after this restore is complete. On a timeout: restore anyway.
    let _shell = ShellLock::acquire(SHELL_LOCK_TIMEOUT_MS);
    if bar_running() {
        // An autostarted bar came up while we waited for Explorer (the lock is held).
        return leave_to_the_live_bar(path);
    }
    let restored = vitro_win::stock_taskbar::restore_stock_taskbar_within(path, 0);
    if vitro_win::stock_taskbar::stock_taskbar_present() {
        // A Vitrobar that ended without cleaning up may have hidden the taskbar and held the
        // tray icons: show it, and ask every app to register its icons with Explorer again.
        vitro_win::stock_taskbar::show_stock_taskbar();
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

fn bar_mode(opts: app::Options) -> ExitCode {
    let Some(_lock) = watchdog::NamedMutex::acquire(watchdog::BAR_MUTEX) else {
        // A bar already runs: nothing to do (exit 0, so a watchdog that started us ends).
        return ExitCode::SUCCESS;
    };
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_hook(info);
        // A panic may cross an `extern "system"` frame (and abort): restore before that.
        shell::restore_after_panic();
        std::process::exit(101);
    }));
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

#[cfg(not(debug_assertions))]
fn attach_console() {
    use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    // SAFETY: plain call; failing (no parent console) leaves the diagnostics silent.
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(debug_assertions)]
fn attach_console() {}

enum Mode {
    Watchdog,
    Bar,
    Adopt(u32),
    Restore,
}

fn parse_pid(v: Option<String>) -> Option<u32> {
    v.and_then(|s| s.parse().ok())
}

fn usage_error(msg: &str) -> ExitCode {
    eprintln!("vitrobar: {msg}\n{USAGE}");
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let mut mode = Mode::Watchdog;
    // Only one of --bar, --adopt, --restore.
    let mut mode_flag: Option<String> = None;
    let mut opts = app::Options::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if matches!(arg.as_str(), "--bar" | "--adopt" | "--restore") {
            if let Some(prev) = &mode_flag {
                return usage_error(&format!("{prev} and {arg} cannot be combined"));
            }
            mode_flag = Some(arg.clone());
        }
        match arg.as_str() {
            "--bar" => mode = Mode::Bar,
            "--restore" => mode = Mode::Restore,
            "--adopt" => match parse_pid(args.next()) {
                Some(pid) => mode = Mode::Adopt(pid),
                None => return usage_error("--adopt needs a process id"),
            },
            "--parent" => match parse_pid(args.next()) {
                Some(pid) => opts.parent = Some(pid),
                None => return usage_error("--parent needs a process id"),
            },
            "--debug-state" => {
                opts.debug_state = true;
                opts.pass_on.push(arg);
            }
            "--console" => {
                attach_console();
                opts.pass_on.push(arg);
            }
            other => return usage_error(&format!("unknown argument {other:?}")),
        }
    }
    if opts.parent.is_some() && !matches!(mode, Mode::Bar) {
        return usage_error("--parent goes only with --bar");
    }
    match mode {
        Mode::Watchdog => watchdog::run(&opts.pass_on),
        Mode::Adopt(pid) => watchdog::adopt(pid, &opts.pass_on),
        Mode::Restore => restore_mode(),
        Mode::Bar => bar_mode(opts),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vitro_win::stock_taskbar::{
        delete_runonce, rearm_runonce_if_record, runonce_exists, RestoreRecord,
    };

    /// Removes the RunOnce value and the temporary record however the test ends (the value
    /// points at the test binary, which cannot restore anything).
    struct Teardown(std::path::PathBuf);
    impl Drop for Teardown {
        fn drop(&mut self) {
            let _ = delete_runonce();
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Windows deleted `RunOnce\VitrobarRestore` before running `--restore`, which finds a
    /// live bar: the record stays and the RunOnce value is back.
    #[test]
    #[ignore = "writes HKCU RunOnce VitrobarRestore and holds the bar mutex"]
    fn restore_with_a_live_bar_rearms_runonce() {
        assert!(!runonce_exists(), "a VitrobarRestore value is already set");
        let dir = std::env::temp_dir().join(format!("vitrobar-rearm-{}", std::process::id()));
        let _t = Teardown(dir.clone());
        let path = dir.join("restore.json");
        RestoreRecord {
            original_autohide: false,
        }
        .write(&path)
        .unwrap();
        let bar = watchdog::NamedMutex::acquire(watchdog::BAR_MUTEX)
            .expect("a Vitrobar bar is running: stop it before this test");
        assert_eq!(restore_mode_at(&path), ExitCode::SUCCESS);
        assert!(path.exists(), "the live bar's record was left alone");
        assert!(runonce_exists(), "the RunOnce safety net is back");
        drop(bar);
        // Without a record there is nothing to re-arm.
        delete_runonce().unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(!rearm_runonce_if_record(&path).unwrap());
        assert!(!runonce_exists());
    }
}
