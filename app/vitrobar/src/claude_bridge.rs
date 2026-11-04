//! `vitrobar --claude-statusline` (ruling P1): Claude Code's status-line command while the
//! Claude limits widget is on. Claude Code pipes its status JSON in; the bridge saves the
//! rate-limit windows for the bar and then hands the same JSON to the user's previous
//! status-line command, printing what it prints.
//!
//! It must never get in Claude Code's way: no window, no COM, a capped read, at most 1 s for
//! the previous command, nothing but status text on stdout, and always exit code 0.

use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::process::{Command, ExitCode, Stdio};
use std::sync::mpsc::channel;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use vitro_core::widgets::parse_statusline;
use vitro_win::claude_usage::{data_path, save_at};
use windows::Win32::Foundation::{
    CloseHandle, SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT, HANDLE_FLAGS,
};
use windows::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject, TerminateJobObject,
    JobObjectExtendedLimitInformation, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};

const MAX_INPUT: u64 = 1 << 20;
/// The whole bridge run, the previous status-line command included, ends within this.
const BUDGET: Duration = Duration::from_millis(950);
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub fn run() -> ExitCode {
    let deadline = Instant::now() + BUDGET;
    let mut input = Vec::new();
    let _ = std::io::stdin().take(MAX_INPUT).read_to_end(&mut input);
    let json = String::from_utf8_lossy(&input);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    if let (Some(u), Some(path)) = (parse_statusline(&json, now), data_path()) {
        if let Err(e) = save_at(&path, &u) {
            eprintln!("vitrobar: could not save the Claude limits: {e}");
        }
    }
    let prev = crate::app::settings_path()
        .map(|p| vitro_core::settings::load(&p))
        .and_then(|s| s.claude_prev_statusline)
        .filter(|c| !c.trim().is_empty());
    let out = match prev {
        Some(cmd) => run_previous(&cmd, &input, deadline),
        None => minimal_line(&json).into_bytes(),
    };
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(&out);
    let _ = stdout.flush();
    ExitCode::SUCCESS
}

/// The model's name when Claude Code sent one; else nothing.
fn minimal_line(json: &str) -> String {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| {
            v.get("model")?
                .get("display_name")?
                .as_str()
                .map(|s| format!("{s}\n"))
        })
        .unwrap_or_default()
}

/// Our own standard handles stay ours: a command the previous status line leaves running must
/// not hold Claude Code's pipe open after we exit.
fn keep_std_handles() {
    for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: plain queries and flag changes on this process's own handles.
        unsafe {
            if let Ok(h) = GetStdHandle(which) {
                if !h.is_invalid() && !h.0.is_null() {
                    let _ = SetHandleInformation(h, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0));
                }
            }
        }
    }
}

/// A job that ends every process in it when it is closed (the previous command's tree).
struct Job(HANDLE);

impl Job {
    fn new() -> Option<Self> {
        // SAFETY: an anonymous job; the limit struct is a local passed with its size.
        unsafe {
            let job = CreateJobObjectW(None, None).ok()?;
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let set = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if set.is_err() {
                let _ = CloseHandle(job);
                return None;
            }
            Some(Self(job))
        }
    }

    fn add(&self, pid: u32) {
        // SAFETY: a process we just started; its handle is closed right after.
        unsafe {
            if let Ok(p) = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, false, pid) {
                let _ = AssignProcessToJobObject(self.0, p);
                let _ = CloseHandle(p);
            }
        }
    }

    fn end(&self) {
        // SAFETY: our job handle.
        unsafe {
            let _ = TerminateJobObject(self.0, 1);
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: our job handle; closing it ends what is still in it.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Runs `cmd` through `cmd /C` with `input` on its stdin; its stdout if it came before
/// `deadline` (else nothing, and it and whatever it started are ended).
fn run_previous(cmd: &str, input: &[u8], deadline: Instant) -> Vec<u8> {
    keep_std_handles();
    let job = Job::new();
    let child = Command::new("cmd")
        .args(["/D", "/S", "/C"])
        // `/S`: cmd strips these outer quotes and runs the rest exactly as the user wrote it.
        .raw_arg(format!("\"{cmd}\""))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
    let Ok(mut child) = child else {
        return Vec::new();
    };
    if let Some(j) = &job {
        j.add(child.id());
    }
    if let Some(mut stdin) = child.stdin.take() {
        let input = input.to_vec();
        // A command that never reads its input must not block us.
        std::thread::spawn(move || {
            let _ = stdin.write_all(&input);
        });
    }
    let (tx, rx) = channel();
    if let Some(mut stdout) = child.stdout.take() {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = stdout.read_to_end(&mut buf);
            let _ = tx.send(buf);
        });
    }
    let left = deadline.saturating_duration_since(Instant::now());
    let out = rx.recv_timeout(left).unwrap_or_default();
    // The output is complete; give the process the rest of the budget to exit, then end it.
    while Instant::now() < deadline {
        if let Ok(Some(_)) = child.try_wait() {
            return out;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    match &job {
        Some(j) => j.end(),
        None => {
            let _ = child.kill();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_minimal_line_is_the_model_name() {
        assert_eq!(minimal_line(r#"{"model":{"display_name":"Opus"}}"#), "Opus\n");
        assert_eq!(minimal_line(r#"{"model":{}}"#), "");
        assert_eq!(minimal_line("garbage"), "");
    }
}
