//! `vitrobar --claude-statusline`: the bridge Claude Code runs as its status line (ruling P1).
//! Runs the built exe with its settings and data folder in temp directories. The previous
//! status line runs in the shell Claude Code would use (Git Bash, else PowerShell); each test
//! names the shell through `VITROBAR_STATUS_SHELL`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use vitro_win::claude_usage::{status_shell, StatusShell, STATUS_SHELL_ENV};

const SAMPLE: &str = r#"{"model":{"display_name":"Opus"},"rate_limits":{"five_hour":{"used_percentage":42,"resets_at":1793476800},"seven_day":{"used_percentage":18,"resets_at":1793900000}}}"#;

/// Git Bash on this machine, or `None` (the bash tests then say so and pass).
fn git_bash() -> Option<PathBuf> {
    match std::env::var_os(STATUS_SHELL_ENV) {
        Some(_) => None,
        None => match status_shell() {
            StatusShell::GitBash(p) => Some(p),
            StatusShell::PowerShell => {
                eprintln!("no Git Bash here: bash cases skipped");
                None
            }
        },
    }
}

/// Runs the bridge with `stdin` and `shell` (a bash.exe path or "powershell"); returns
/// (stdout, exit success, how long it took).
fn bridge(settings: &Path, local: &Path, shell: &str, stdin: &str) -> (String, bool, Duration) {
    let t = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_vitrobar"))
        .arg("--claude-statusline")
        .env("VITROBAR_SETTINGS", settings)
        .env("LOCALAPPDATA", local)
        .env(STATUS_SHELL_ENV, shell)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.success(),
        t.elapsed(),
    )
}

fn settings_with(dir: &Path, prev: Option<&str>) -> PathBuf {
    let p = dir.join("settings.toml");
    let mut text = String::from("first-run-done = true\nstart-with-windows = false\n");
    if let Some(cmd) = prev {
        // A TOML literal string: the commands below never contain a single quote.
        text.push_str(&format!("claude-prev-statusline = '{cmd}'\n"));
    }
    std::fs::write(&p, text).unwrap();
    p
}

fn slash(p: &Path) -> String {
    p.display().to_string().replace('\\', "/")
}

#[test]
fn saves_the_limits_and_exits_fast() {
    let dir = tempfile::tempdir().unwrap();
    let settings = settings_with(dir.path(), None);
    let (out, ok, took) = bridge(&settings, dir.path(), "powershell", SAMPLE);
    assert!(ok);
    assert!(took < Duration::from_secs(1), "{took:?}");
    let saved =
        std::fs::read_to_string(dir.path().join("Vitrobar").join("claude-usage.json")).unwrap();
    assert!(saved.contains("\"five_hour\"") && saved.contains("42"), "{saved}");
    // No previous command: a minimal line with the model's name.
    assert_eq!(out.trim(), "Opus");
}

#[test]
fn odd_input_never_fails_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let settings = settings_with(dir.path(), None);
    for input in ["", "not json", "{\"model\":{}}"] {
        let (out, ok, _) = bridge(&settings, dir.path(), "powershell", input);
        assert!(ok, "{input:?}");
        assert!(out.trim().is_empty(), "{input:?} -> {out:?}");
    }
    assert!(!dir.path().join("Vitrobar").join("claude-usage.json").exists());
}

#[test]
fn git_bash_runs_the_previous_status_line_with_the_same_input() {
    let Some(bash) = git_bash() else { return };
    let bash = bash.display().to_string();
    let dir = tempfile::tempdir().unwrap();
    let (out, ok, _) = bridge(&settings_with(dir.path(), Some("echo hi")), dir.path(), &bash, SAMPLE);
    assert!(ok);
    assert!(out.contains("hi"), "{out:?}");
    // The same JSON on its stdin.
    let (out, _, _) = bridge(&settings_with(dir.path(), Some("cat")), dir.path(), &bash, SAMPLE);
    assert!(out.contains("rate_limits"), "{out:?}");
    // `~` expands as it does for Claude Code (scripts like ~/.claude/statusline.sh).
    let (out, _, _) = bridge(
        &settings_with(dir.path(), Some("test -d ~ && echo tilde-ok")),
        dir.path(),
        &bash,
        SAMPLE,
    );
    assert!(out.contains("tilde-ok"), "{out:?}");
}

#[test]
fn powershell_runs_the_previous_status_line_without_git_bash() {
    let dir = tempfile::tempdir().unwrap();
    let settings = settings_with(dir.path(), Some("Write-Output ps-hi"));
    let (out, ok, _) = bridge(&settings, dir.path(), "powershell", SAMPLE);
    assert!(ok);
    assert!(out.contains("ps-hi"), "{out:?}");
    let settings = settings_with(dir.path(), Some("$input | Out-String"));
    let (out, _, _) = bridge(&settings, dir.path(), "powershell", SAMPLE);
    assert!(out.contains("rate_limits"), "{out:?}");
}

#[test]
fn a_hanging_previous_command_is_cut_off_after_a_second() {
    let dir = tempfile::tempdir().unwrap();
    let mut cases = vec![("powershell".to_string(), "Start-Sleep 5")];
    if let Some(bash) = git_bash() {
        cases.push((bash.display().to_string(), "sleep 5"));
    }
    for (shell, cmd) in cases {
        let (_, ok, took) = bridge(&settings_with(dir.path(), Some(cmd)), dir.path(), &shell, SAMPLE);
        assert!(ok);
        assert!(took < Duration::from_millis(1600), "{shell}: {took:?}");
    }
}

#[test]
fn a_slow_previous_status_line_prints_its_last_complete_output() {
    let Some(bash) = git_bash() else { return };
    let bash = bash.display().to_string();
    let dir = tempfile::tempdir().unwrap();
    let flag = dir.path().join("slow");
    // Fast while the flag file is missing, slower than the bridge's budget once it exists;
    // it prints only at the end either way.
    let cmd = format!("[ -e \"{}\" ] && sleep 3; echo hello-cache", slash(&flag));
    let settings = settings_with(dir.path(), Some(&cmd));
    let (out, _, _) = bridge(&settings, dir.path(), &bash, SAMPLE);
    assert!(out.contains("hello-cache"), "{out:?}");
    std::fs::write(&flag, "").unwrap();
    let (out, ok, took) = bridge(&settings, dir.path(), &bash, SAMPLE);
    assert!(ok);
    assert!(took < Duration::from_millis(1600), "{took:?}");
    assert!(out.contains("hello-cache"), "the last complete output: {out:?}");
}

#[test]
fn a_slow_status_line_with_nothing_cached_prints_what_it_wrote_so_far() {
    let Some(bash) = git_bash() else { return };
    let bash = bash.display().to_string();
    let dir = tempfile::tempdir().unwrap();
    let settings = settings_with(dir.path(), Some("echo partial-line; sleep 3"));
    let (out, ok, took) = bridge(&settings, dir.path(), &bash, SAMPLE);
    assert!(ok);
    assert!(took < Duration::from_millis(1600), "{took:?}");
    assert!(out.contains("partial-line"), "{out:?}");
}

/// The command Vitrobar writes into Claude Code's settings starts the bridge in the shell
/// Claude Code uses (a quoted Windows path alone fails in both).
#[test]
fn the_written_bridge_command_starts_the_bridge_in_each_shell() {
    use vitro_win::claude_usage::bridge_command;
    let exe = Path::new(env!("CARGO_BIN_EXE_vitrobar"));
    let dir = tempfile::tempdir().unwrap();
    let settings = settings_with(dir.path(), None);
    let mut shells = vec![StatusShell::PowerShell];
    if let Some(b) = git_bash() {
        shells.push(StatusShell::GitBash(b));
    }
    for shell in shells {
        let cmd = bridge_command(exe, &shell);
        let mut c = match &shell {
            StatusShell::GitBash(b) => {
                let mut c = Command::new(b);
                c.args(["-c", &cmd]);
                c
            }
            StatusShell::PowerShell => {
                let mut c = Command::new("powershell.exe");
                c.args(["-NoProfile", "-NonInteractive", "-Command", &cmd]);
                c
            }
        };
        let mut child = c
            .env("VITROBAR_SETTINGS", &settings)
            .env("LOCALAPPDATA", dir.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(SAMPLE.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert_eq!(text.trim(), "Opus", "{shell:?} running {cmd}");
    }
}
