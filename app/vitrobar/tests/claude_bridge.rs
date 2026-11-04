//! `vitrobar --claude-statusline`: the bridge Claude Code runs as its status line (ruling P1).
//! Runs the built exe with its settings and data folder in temp directories.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const SAMPLE: &str = r#"{"model":{"display_name":"Opus"},"rate_limits":{"five_hour":{"used_percentage":42,"resets_at":1793476800},"seven_day":{"used_percentage":18,"resets_at":1793900000}}}"#;

/// Runs the bridge with `stdin`; returns (stdout, exit success, how long it took).
fn bridge(settings: &Path, local: &Path, stdin: &str) -> (String, bool, Duration) {
    let t = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_vitrobar"))
        .arg("--claude-statusline")
        .env("VITROBAR_SETTINGS", settings)
        .env("LOCALAPPDATA", local)
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

fn settings_with(dir: &Path, prev: Option<&str>) -> std::path::PathBuf {
    let p = dir.join("settings.toml");
    let mut text = String::from("first-run-done = true\nstart-with-windows = false\n");
    if let Some(cmd) = prev {
        text.push_str(&format!("claude-prev-statusline = {}\n", toml_str(cmd)));
    }
    std::fs::write(&p, text).unwrap();
    p
}

fn toml_str(s: &str) -> String {
    format!("'{s}'")
}

#[test]
fn saves_the_limits_and_exits_fast() {
    let dir = tempfile::tempdir().unwrap();
    let settings = settings_with(dir.path(), None);
    let (out, ok, took) = bridge(&settings, dir.path(), SAMPLE);
    assert!(ok);
    assert!(took < Duration::from_secs(1), "{took:?}");
    let saved = std::fs::read_to_string(dir.path().join("Vitrobar").join("claude-usage.json")).unwrap();
    assert!(saved.contains("\"five_hour\"") && saved.contains("42"), "{saved}");
    // No previous command: a minimal line with the model's name.
    assert_eq!(out.trim(), "Opus");
}

#[test]
fn runs_the_previous_status_line_with_the_same_input() {
    let dir = tempfile::tempdir().unwrap();
    let settings = settings_with(dir.path(), Some("echo hi"));
    let (out, ok, _) = bridge(&settings, dir.path(), SAMPLE);
    assert!(ok);
    assert!(out.contains("hi"), "{out:?}");
    // The previous command sees the same JSON on its stdin.
    let settings = settings_with(dir.path(), Some("findstr display_name"));
    let (out, _, _) = bridge(&settings, dir.path(), SAMPLE);
    assert!(out.contains("rate_limits"), "{out:?}");
}

#[test]
fn a_hanging_previous_command_is_cut_off_after_a_second() {
    let dir = tempfile::tempdir().unwrap();
    let settings = settings_with(dir.path(), Some("ping -n 6 127.0.0.1 >nul"));
    let (_, ok, took) = bridge(&settings, dir.path(), SAMPLE);
    assert!(ok);
    assert!(took < Duration::from_millis(1600), "{took:?}");
}

#[test]
fn odd_input_never_fails_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let settings = settings_with(dir.path(), None);
    for input in ["", "not json", "{\"model\":{}}"] {
        let (out, ok, _) = bridge(&settings, dir.path(), input);
        assert!(ok, "{input:?}");
        assert!(out.trim().is_empty(), "{input:?} -> {out:?}");
    }
    assert!(!dir.path().join("Vitrobar").join("claude-usage.json").exists());
}

#[test]
fn a_slow_previous_status_line_prints_its_last_complete_output() {
    let dir = tempfile::tempdir().unwrap();
    let flag = dir.path().join("slow");
    // Fast while the flag file is missing, slower than the bridge's budget once it exists;
    // it prints only at the end either way.
    let cmd = format!(
        "(if exist \"{}\" ping -n 4 127.0.0.1 >nul) & echo hello-cache",
        flag.display()
    );
    let settings = settings_with(dir.path(), Some(&cmd));
    let (out, _, _) = bridge(&settings, dir.path(), SAMPLE);
    assert!(out.contains("hello-cache"), "{out:?}");
    std::fs::write(&flag, "").unwrap();
    let (out, ok, took) = bridge(&settings, dir.path(), SAMPLE);
    assert!(ok);
    assert!(took < Duration::from_millis(1600), "{took:?}");
    assert!(out.contains("hello-cache"), "the last complete output: {out:?}");
}

#[test]
fn a_slow_status_line_with_nothing_cached_prints_what_it_wrote_so_far() {
    let dir = tempfile::tempdir().unwrap();
    let settings = settings_with(dir.path(), Some("echo partial-line & ping -n 4 127.0.0.1 >nul"));
    let (out, ok, took) = bridge(&settings, dir.path(), SAMPLE);
    assert!(ok);
    assert!(took < Duration::from_millis(1600), "{took:?}");
    assert!(out.contains("partial-line"), "{out:?}");
}
