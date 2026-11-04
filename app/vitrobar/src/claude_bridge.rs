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
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use vitro_core::widgets::parse_statusline;
use vitro_win::claude_usage::{data_dir, data_path, save_at, status_shell, StatusShell};
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
        Some(cmd) => previous_output(&cmd, &input, deadline),
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

/// The previous status line's output: this run's when it finished in time (and it is kept
/// for later); else the last complete output of the same command, or failing that whatever
/// it printed before the cut. A slow status line (a cold `npx`) so keeps showing.
fn previous_output(cmd: &str, input: &[u8], deadline: Instant) -> Vec<u8> {
    let (out, complete) = run_previous(cmd, input, deadline);
    let cache = data_dir().map(|d| d.join(PREV_OUT_FILE));
    if complete {
        if let Some(p) = &cache {
            save_prev_output(p, cmd, &out);
        }
        return out;
    }
    cache
        .and_then(|p| load_prev_output(&p, cmd))
        .unwrap_or(out)
}

/// The last complete output of the previous status line, with the command that printed it.
const PREV_OUT_FILE: &str = "claude-prev-out.json";

fn save_prev_output(path: &std::path::Path, cmd: &str, out: &[u8]) {
    let v = serde_json::json!({ "command": cmd, "output": String::from_utf8_lossy(out) });
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    let saved = std::fs::write(&tmp, v.to_string()).and_then(|()| std::fs::rename(&tmp, path));
    if saved.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// The cached output, only if `cmd` printed it (the user may have changed the command).
fn load_prev_output(path: &std::path::Path, cmd: &str) -> Option<Vec<u8>> {
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    if v.get("command")?.as_str()? != cmd {
        return None;
    }
    Some(v.get("output")?.as_str()?.as_bytes().to_vec())
}

/// `cmd` run the way Claude Code runs status lines: `bash -c` in Git Bash when it is
/// installed, else PowerShell.
fn shell_command(shell: &StatusShell, cmd: &str) -> Command {
    match shell {
        StatusShell::GitBash(bash) => {
            let mut c = Command::new(bash);
            c.args(["-c", cmd]);
            c
        }
        StatusShell::PowerShell => {
            let mut c = Command::new("powershell.exe");
            c.args(["-NoProfile", "-NonInteractive", "-Command", cmd]);
            c
        }
    }
}

/// Runs `cmd` in Claude Code's shell with `input` on its stdin; its stdout and whether that is
/// complete (it ended before `deadline`). Otherwise what it printed so far, and it and
/// whatever it started are ended.
fn run_previous(cmd: &str, input: &[u8], deadline: Instant) -> (Vec<u8>, bool) {
    keep_std_handles();
    let job = Job::new();
    let child = shell_command(&status_shell(), cmd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
    let Ok(mut child) = child else {
        return (Vec::new(), true);
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
    // The reader appends as output arrives, so a cut still has what came so far.
    let got = Arc::new(Mutex::new(Vec::new()));
    let (tx, rx) = channel();
    if let Some(mut stdout) = child.stdout.take() {
        let got = Arc::clone(&got);
        std::thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            while let Ok(n @ 1..) = stdout.read(&mut chunk) {
                got.lock().unwrap_or_else(|e| e.into_inner()).extend_from_slice(&chunk[..n]);
            }
            let _ = tx.send(());
        });
    }
    let left = deadline.saturating_duration_since(Instant::now());
    let eof = rx.recv_timeout(left).is_ok();
    let take = || std::mem::take(&mut *got.lock().unwrap_or_else(|e| e.into_inner()));
    if eof {
        // The output is complete; give the process the rest of the budget to exit.
        while Instant::now() < deadline {
            if let Ok(Some(_)) = child.try_wait() {
                return (take(), true);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    match &job {
        Some(j) => j.end(),
        None => {
            let _ = child.kill();
        }
    }
    (take(), eof)
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
