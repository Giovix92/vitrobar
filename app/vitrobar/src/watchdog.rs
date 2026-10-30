//! The safety net around the bar: a tiny watchdog process that restarts a crashed bar and,
//! above all, gives the stock taskbar back the moment the bar dies.
//!
//! Process model:
//! - `vitrobar.exe` (no arguments) is the watchdog. It holds `Local\Vitrobar.Watchdog` (a
//!   second launch finds it taken and exits 0), spawns `vitrobar.exe --bar --parent <pid>`
//!   and waits on the bar's process handle. Exit code 0 (the user chose Exit, the session
//!   ends, or another bar already runs) ends the watchdog too. Anything else is a crash: the
//!   stock taskbar is restored at once from here (it only takes Explorer's windows, the
//!   registry and the record file, so it works across processes), then [`RestartPolicy`]
//!   decides between a restart and giving up.
//! - `--bar` holds `Local\Vitrobar.Bar` and watches the watchdog's process handle
//!   ([`ParentWatch`]): if the watchdog dies, the bar starts `vitrobar.exe --adopt <bar pid>`,
//!   a new watchdog that waits on the existing bar instead of spawning one.
//!
//! The watchdog has no window, no COM, no GPU and reads no settings.

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

use vitro_core::restart::{Decision, RestartPolicy};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
use windows::Win32::System::Console::SetConsoleCtrlHandler;
use windows::Win32::System::Threading::{
    CreateMutexW, GetCurrentProcessId, GetExitCodeProcess, OpenMutexW, OpenProcess,
    WaitForSingleObject, INFINITE, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    SYNCHRONIZATION_SYNCHRONIZE,
};
use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_SHUTTINGDOWN};

pub const WATCHDOG_MUTEX: PCWSTR = w!(r"Local\Vitrobar.Watchdog");
pub const BAR_MUTEX: PCWSTR = w!(r"Local\Vitrobar.Bar");

/// After a crash: the stock taskbar is back at once, the bar a moment later. The pause also
/// lets "End process tree" finish killing the watchdog before it restarts anything.
const RESPAWN_DELAY: Duration = Duration::from_millis(500);
/// The bar waits this long after its watchdog died before starting a new one, so that "End
/// process tree" (watchdog first, bar right after) leaves nothing running.
pub const ADOPT_DELAY: Duration = Duration::from_millis(1000);

/// A named mutex held for the life of the process (its existence is the lock).
pub struct NamedMutex(HANDLE);

impl NamedMutex {
    /// `None` when another process already holds it (or it cannot be created).
    pub fn acquire(name: PCWSTR) -> Option<Self> {
        // SAFETY: plain call with a static name; GetLastError is read right after it.
        unsafe {
            let h = CreateMutexW(None, false, name).ok()?;
            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = CloseHandle(h);
                return None;
            }
            Some(Self(h))
        }
    }
}

impl Drop for NamedMutex {
    fn drop(&mut self) {
        // SAFETY: the handle is owned and closed once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Some live process holds the named mutex.
pub fn mutex_exists(name: PCWSTR) -> bool {
    // SAFETY: plain call; the handle is closed at once.
    unsafe {
        match OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, name) {
            Ok(h) => {
                let _ = CloseHandle(h);
                true
            }
            Err(_) => false,
        }
    }
}

fn own_pid() -> u32 {
    // SAFETY: plain query.
    unsafe { GetCurrentProcessId() }
}

fn exe() -> std::io::Result<std::path::PathBuf> {
    std::env::current_exe()
}

/// Opens a process for waiting on it and reading its exit code.
fn open_process(pid: u32) -> Option<OwnedHandle> {
    // SAFETY: plain call; the returned handle is owned by the OwnedHandle.
    unsafe {
        OpenProcess(
            PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
            false,
            pid,
        )
        .ok()
        .map(|h| OwnedHandle::from_raw_handle(h.0))
    }
}

fn raw(h: &OwnedHandle) -> HANDLE {
    HANDLE(h.as_raw_handle())
}

/// Waits for the process to end and returns its exit code.
fn wait_exit(h: &OwnedHandle) -> u32 {
    let mut code = 1u32;
    // SAFETY: the handle is valid while `h` lives.
    unsafe {
        WaitForSingleObject(raw(h), INFINITE);
        if GetExitCodeProcess(raw(h), &mut code).is_err() {
            code = 1;
        }
    }
    code
}

fn spawn_bar(extra: &[String]) -> std::io::Result<OwnedHandle> {
    let child = Command::new(exe()?)
        .arg("--bar")
        .arg("--parent")
        .arg(own_pid().to_string())
        .args(extra)
        .spawn()?;
    Ok(OwnedHandle::from(child))
}

/// Starts a watchdog that adopts this (bar) process.
pub fn spawn_adopter(extra: &[String]) -> std::io::Result<OwnedHandle> {
    let child = Command::new(exe()?)
        .arg("--adopt")
        .arg(own_pid().to_string())
        .args(extra)
        .spawn()?;
    Ok(OwnedHandle::from(child))
}

/// The bar died without cleaning up: give the stock taskbar back from here.
fn restore_after_crash() {
    use vitro_win::stock_taskbar::{
        default_record_path, restore_stock_taskbar_within, show_stock_taskbar,
        stock_taskbar_present, ShellLock, SHELL_LOCK_TIMEOUT_MS,
    };
    // Serialized with every other take-over and restore; on a timeout restore anyway.
    let _shell = ShellLock::acquire(SHELL_LOCK_TIMEOUT_MS);
    if mutex_exists(BAR_MUTEX) {
        // Another live bar owns the shell (the one that died never got that far).
        return;
    }
    // Show first and let Explorer handle it before the auto-hide change: that change keeps
    // Explorer busy for most of a second, and a posted show would wait behind it.
    show_stock_taskbar();
    if stock_taskbar_present() {
        wait_until_shown();
    }
    if let Some(p) = default_record_path() {
        if let Err(e) = restore_stock_taskbar_within(&p, 0) {
            eprintln!("vitrobar-watchdog: could not restore the stock taskbar: {e}");
        }
    }
    // Whether or not a record was left (another restore may have consumed it).
    show_stock_taskbar();
    if stock_taskbar_present() {
        // The broadcast below makes every app re-register its tray icons with *sent*
        // messages, which Explorer handles before the posted show: let the show land first.
        wait_until_shown();
        // The dead bar held the tray icons: ask every app to register them with Explorer.
        vitro_win::tray_host::broadcast_taskbar_created();
    }
}

/// After a deliberate exit: a record still on disk means the bar's own restore did not
/// finish. Restore (idempotent) unless another live bar owns the shell.
fn restore_leftover() {
    use vitro_win::stock_taskbar::{
        default_record_path, restore_stock_taskbar_within, ShellLock, SHELL_LOCK_TIMEOUT_MS,
    };
    let Some(p) = default_record_path() else {
        return;
    };
    if !p.exists() {
        return;
    }
    let _shell = ShellLock::acquire(SHELL_LOCK_TIMEOUT_MS);
    if mutex_exists(BAR_MUTEX) {
        return;
    }
    if let Err(e) = restore_stock_taskbar_within(&p, 0) {
        eprintln!("vitrobar-watchdog: could not restore the stock taskbar: {e}");
    }
}

/// Waits (bounded, only after a crash) for Explorer to show its taskbar, posting the show
/// again now and then in case a hide from the dead bar landed after it. Deliberately a
/// bounded busy-wait (20 ms sleeps, 1.5 s at most): Explorer's window visibility has no
/// event to wait on, and this runs only right after a crash, never while idle.
fn wait_until_shown() {
    const STEP: Duration = Duration::from_millis(20);
    const REPOST_EVERY: u32 = 10;
    const MAX: Duration = Duration::from_millis(1500);
    let start = Instant::now();
    let mut n = 0u32;
    while !vitro_win::stock_taskbar::stock_taskbar_visible() && start.elapsed() < MAX {
        std::thread::sleep(STEP);
        n += 1;
        if n.is_multiple_of(REPOST_EVERY) {
            vitro_win::stock_taskbar::show_stock_taskbar();
        }
    }
}

fn session_ending() -> bool {
    // SAFETY: plain query.
    unsafe { GetSystemMetrics(SM_SHUTTINGDOWN) != 0 }
}

/// In a console (debug builds): Ctrl+C goes to the bar, which quits with code 0; the
/// watchdog ignores it and follows.
fn ignore_ctrl_c() {
    // SAFETY: plain call; a null handler with TRUE only sets the ignore flag.
    unsafe {
        let _ = SetConsoleCtrlHandler(None, true);
    }
}

fn watch(mut bar: OwnedHandle, extra: &[String]) -> ExitCode {
    let start = Instant::now();
    let mut policy = RestartPolicy::new();
    loop {
        let code = wait_exit(&bar);
        if code == 0 {
            // The bar ended on purpose and restored the shell itself; if its restore did not
            // finish (the record is still there), finish it. Never a restart.
            restore_leftover();
            return ExitCode::SUCCESS;
        }
        eprintln!("vitrobar-watchdog: the bar ended with code {code:#x}");
        restore_after_crash();
        let now_ms = start.elapsed().as_millis() as u64;
        if policy.on_crash(now_ms) == Decision::GiveUp {
            eprintln!("vitrobar-watchdog: too many crashes, the stock taskbar stays");
            return ExitCode::FAILURE;
        }
        std::thread::sleep(RESPAWN_DELAY);
        if session_ending() {
            return ExitCode::SUCCESS;
        }
        bar = match spawn_bar(extra) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("vitrobar-watchdog: could not start the bar: {e}");
                return ExitCode::FAILURE;
            }
        };
    }
}

/// `vitrobar.exe` with no mode argument.
pub fn run(extra: &[String]) -> ExitCode {
    let Some(_lock) = NamedMutex::acquire(WATCHDOG_MUTEX) else {
        // Vitrobar already runs: a second launch does nothing.
        return ExitCode::SUCCESS;
    };
    ignore_ctrl_c();
    match spawn_bar(extra) {
        Ok(bar) => watch(bar, extra),
        Err(e) => {
            eprintln!("vitrobar-watchdog: could not start the bar: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `vitrobar.exe --adopt <bar pid>`: the bar's watchdog died; watch the bar as it is.
pub fn adopt(bar_pid: u32, extra: &[String]) -> ExitCode {
    let Some(_lock) = NamedMutex::acquire(WATCHDOG_MUTEX) else {
        return ExitCode::SUCCESS;
    };
    ignore_ctrl_c();
    if !mutex_exists(BAR_MUTEX) {
        // No bar runs (it ended before we got here): nothing to watch.
        return ExitCode::SUCCESS;
    }
    match open_process(bar_pid) {
        Some(bar) => watch(bar, extra),
        None => ExitCode::SUCCESS,
    }
}

/// The bar's side: watches the watchdog's process handle and starts an adopting watchdog
/// when it dies (a short while later, see [`ADOPT_DELAY`]). No polling: the handle sits in
/// the bar's message wait, and the delay is that wait's timeout.
pub struct ParentWatch {
    handle: Option<OwnedHandle>,
    adopt_at: Option<Instant>,
    policy: RestartPolicy,
    start: Instant,
    extra: Vec<String>,
    /// Failed adopter launches in a row (each retry waits twice as long).
    spawn_failures: u32,
    spawn: fn(&[String]) -> std::io::Result<OwnedHandle>,
}

/// Adopter launches tried after a failure before the bar runs without a watchdog.
const MAX_SPAWN_RETRIES: u32 = 4;

impl ParentWatch {
    /// `parent` is the watchdog's pid from `--parent`; `None` (a bare `--bar` run) watches
    /// nothing.
    pub fn new(parent: Option<u32>, extra: Vec<String>) -> Self {
        let mut w = Self {
            handle: None,
            adopt_at: None,
            policy: RestartPolicy::new(),
            start: Instant::now(),
            extra,
            spawn_failures: 0,
            spawn: spawn_adopter,
        };
        if let Some(pid) = parent {
            match open_process(pid) {
                Some(h) => w.handle = Some(h),
                // Already gone: adopt right away.
                None => w.adopt_at = Some(Instant::now()),
            }
        }
        w
    }

    /// The handle to put in the message wait, if any.
    pub fn handle(&self) -> Option<HANDLE> {
        self.handle.as_ref().map(raw)
    }

    /// The wait timeout: until a pending adoption is due, else infinite.
    pub fn timeout_ms(&self) -> u32 {
        match self.adopt_at {
            Some(at) => at
                .saturating_duration_since(Instant::now())
                .as_millis()
                .min(u128::from(INFINITE - 1)) as u32,
            None => INFINITE,
        }
    }

    /// The watchdog's handle was signalled: it died.
    pub fn on_parent_exit(&mut self) {
        self.handle = None;
        let now_ms = self.start.elapsed().as_millis() as u64;
        if self.policy.on_crash(now_ms) == Decision::GiveUp {
            eprintln!("vitrobar: watchdogs keep dying; running without one");
            return;
        }
        self.adopt_at = Some(Instant::now() + ADOPT_DELAY);
    }

    /// Call after every wake-up: starts the adopting watchdog when it is due.
    pub fn tick(&mut self) {
        if self.adopt_at.is_some_and(|at| Instant::now() >= at) {
            self.adopt_at = None;
            match (self.spawn)(&self.extra) {
                Ok(h) => {
                    self.handle = Some(h);
                    self.spawn_failures = 0;
                }
                Err(e) => {
                    eprintln!("vitrobar: could not start a new watchdog: {e}");
                    if self.spawn_failures < MAX_SPAWN_RETRIES {
                        // One more try later, as the wait's timeout (1 s, 2 s, 4 s, 8 s).
                        self.adopt_at =
                            Some(Instant::now() + ADOPT_DELAY * (1u32 << self.spawn_failures));
                        self.spawn_failures += 1;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_mutex_is_exclusive_and_released() {
        let name =
            windows::core::HSTRING::from(format!(r"Local\Vitrobar.Test.{}", std::process::id()));
        let n = PCWSTR(name.as_ptr());
        assert!(!mutex_exists(n));
        let a = NamedMutex::acquire(n).expect("first acquire");
        assert!(mutex_exists(n));
        assert!(NamedMutex::acquire(n).is_none(), "second acquire must fail");
        drop(a);
        assert!(!mutex_exists(n));
        assert!(NamedMutex::acquire(n).is_some());
    }

    #[test]
    fn no_parent_waits_forever() {
        let w = ParentWatch::new(None, Vec::new());
        assert!(w.handle().is_none());
        assert_eq!(w.timeout_ms(), INFINITE);
    }

    #[test]
    fn a_gone_parent_adopts_at_once() {
        // Pid 0 is the idle process: it cannot be opened.
        let w = ParentWatch::new(Some(0), Vec::new());
        assert!(w.handle().is_none());
        assert_eq!(w.timeout_ms(), 0);
    }

    #[test]
    fn failed_adopter_launch_retries_with_backoff_then_stops() {
        fn fail(_: &[String]) -> std::io::Result<OwnedHandle> {
            Err(std::io::Error::other("no"))
        }
        let mut w = ParentWatch::new(Some(0), Vec::new());
        w.spawn = fail;
        let mut waits = Vec::new();
        for _ in 0..=MAX_SPAWN_RETRIES {
            // Due now.
            w.adopt_at = w.adopt_at.map(|_| Instant::now());
            w.tick();
            waits.push(w.timeout_ms());
        }
        let last = waits.pop().unwrap();
        assert_eq!(last, INFINITE, "gives up after {MAX_SPAWN_RETRIES} retries");
        for (i, t) in waits.iter().enumerate() {
            let want = ADOPT_DELAY.as_millis() as u32 * (1 << i);
            assert!(
                *t <= want && *t + 100 >= want,
                "retry {i}: {t} ms, want {want}"
            );
        }
    }

    #[test]
    fn parent_exit_waits_before_adopting() {
        let mut w = ParentWatch::new(None, Vec::new());
        w.on_parent_exit();
        let t = w.timeout_ms();
        assert!(t > 0 && t <= ADOPT_DELAY.as_millis() as u32, "{t}");
    }
}
