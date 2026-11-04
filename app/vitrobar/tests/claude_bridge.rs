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
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
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

/// How long the bridge takes here with no previous command: starting the exe is not part of
/// the bridge's budget, and under a loaded test run it alone can take most of a second.
fn baseline(dir: &Path) -> Duration {
    // Its own folder: the test's settings and cache stay as they are.
    let own = dir.join("baseline");
    std::fs::create_dir_all(&own).unwrap();
    let settings = settings_with(&own, None);
    bridge(&settings, &own, "powershell", SAMPLE).2
}

/// The bridge's 950 ms budget plus slack for the cut and the exit on a loaded
/// machine; the slow commands below sleep 8 s, far past it.
const OVER_BASELINE: Duration = Duration::from_millis(2500);

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
    assert!(
        saved.contains("\"five_hour\"") && saved.contains("42"),
        "{saved}"
    );
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
    assert!(!dir
        .path()
        .join("Vitrobar")
        .join("claude-usage.json")
        .exists());
}

#[test]
fn git_bash_runs_the_previous_status_line_with_the_same_input() {
    let Some(bash) = git_bash() else { return };
    let bash = bash.display().to_string();
    let dir = tempfile::tempdir().unwrap();
    let (out, ok, _) = bridge(
        &settings_with(dir.path(), Some("echo hi")),
        dir.path(),
        &bash,
        SAMPLE,
    );
    assert!(ok);
    assert!(out.contains("hi"), "{out:?}");
    // The same JSON on its stdin.
    let (out, _, _) = bridge(
        &settings_with(dir.path(), Some("cat")),
        dir.path(),
        &bash,
        SAMPLE,
    );
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
    // A cold powershell.exe can take longer than the bridge's budget (CI
    // runners), and a cut run prints nothing when nothing is cached yet. A cut
    // run caches nothing either, so a later attempt can only pass on output
    // PowerShell really produced.
    let run = |cmd: &str| {
        let settings = settings_with(dir.path(), Some(cmd));
        let mut out = String::new();
        for _ in 0..3 {
            let (o, ok, _) = bridge(&settings, dir.path(), "powershell", SAMPLE);
            assert!(ok);
            out = o;
            if !out.is_empty() {
                break;
            }
        }
        out
    };
    let out = run("Write-Output ps-hi");
    assert!(out.contains("ps-hi"), "{out:?}");
    let out = run("$input | Out-String");
    assert!(out.contains("rate_limits"), "{out:?}");
}

#[test]
fn a_hanging_previous_command_is_cut_off_after_a_second() {
    let dir = tempfile::tempdir().unwrap();
    let mut cases = vec![("powershell".to_string(), "Start-Sleep 8")];
    if let Some(bash) = git_bash() {
        cases.push((bash.display().to_string(), "sleep 8"));
    }
    for (shell, cmd) in cases {
        let base = baseline(dir.path());
        let (_, ok, took) = bridge(
            &settings_with(dir.path(), Some(cmd)),
            dir.path(),
            &shell,
            SAMPLE,
        );
        assert!(ok);
        assert!(
            took < base + OVER_BASELINE,
            "{shell}: {took:?} (baseline {base:?})"
        );
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
    let cmd = format!("[ -e \"{}\" ] && sleep 8; echo hello-cache", slash(&flag));
    let settings = settings_with(dir.path(), Some(&cmd));
    let (out, _, _) = bridge(&settings, dir.path(), &bash, SAMPLE);
    assert!(out.contains("hello-cache"), "{out:?}");
    std::fs::write(&flag, "").unwrap();
    let base = baseline(dir.path());
    let (out, ok, took) = bridge(&settings, dir.path(), &bash, SAMPLE);
    assert!(ok);
    assert!(took < base + OVER_BASELINE, "{took:?} (baseline {base:?})");
    assert!(
        out.contains("hello-cache"),
        "the last complete output: {out:?}"
    );
}

#[test]
fn a_slow_status_line_with_nothing_cached_prints_what_it_wrote_so_far() {
    let Some(bash) = git_bash() else { return };
    let bash = bash.display().to_string();
    let dir = tempfile::tempdir().unwrap();
    let base = baseline(dir.path());
    let settings = settings_with(dir.path(), Some("echo partial-line; sleep 8"));
    let (out, ok, took) = bridge(&settings, dir.path(), &bash, SAMPLE);
    assert!(ok);
    assert!(took < base + OVER_BASELINE, "{took:?} (baseline {base:?})");
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
        child
            .stdin
            .take()
            .unwrap()
            .write_all(SAMPLE.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert_eq!(text.trim(), "Opus", "{shell:?} running {cmd}");
    }
}
