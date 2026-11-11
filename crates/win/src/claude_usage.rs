//! The Claude limits widget's data (ruling P1): Claude Code's status line runs
//! `vitrobar.exe --claude-statusline`, which saves the rate-limit windows to a small JSON file
//! here; the bar watches that file's folder. No credentials are read and nothing goes over the
//! network.
//!
//! Also the one change Vitrobar makes to Claude Code's settings: wrapping the user's
//! `statusLine` command in the bridge when the widget is turned on, and putting it back,
//! exactly, when it is turned off. A command the user changed meanwhile is left alone.

use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use vitro_core::widgets::ClaudeUsage;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WAIT_OBJECT_0, WPARAM};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadDirectoryChangesW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED,
    FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE,
    FILE_NOTIFY_CHANGE_SIZE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Threading::{
    CreateEventW, ResetEvent, SetEvent, WaitForMultipleObjects, INFINITE,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

/// The file the bridge writes, in `%LOCALAPPDATA%\Vitrobar`.
pub const DATA_FILE: &str = "claude-usage.json";
/// Overrides the Claude Code settings file (tests never touch the real one).
pub const CLAUDE_SETTINGS_ENV: &str = "VITROBAR_CLAUDE_SETTINGS";
/// The argument that makes `vitrobar.exe` the status-line bridge.
pub const BRIDGE_ARG: &str = "--claude-statusline";

#[derive(Debug)]
pub enum SetupError {
    Io(std::io::Error),
    /// The settings file is not a JSON object; it is left untouched.
    Malformed,
}

impl std::fmt::Display for SetupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Malformed => f.write_str("Claude Code's settings file is not a JSON object"),
        }
    }
}

impl From<std::io::Error> for SetupError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// What [`wrap_statusline_at`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wrap {
    /// The bridge is in place; this was the user's command before (to store and restore).
    Wrapped(Option<String>),
    /// The bridge was already there (keep the stored previous command). A bridge that ran
    /// another path is pointed at this one.
    AlreadyOurs,
}

/// `%LOCALAPPDATA%\Vitrobar`.
pub fn data_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .filter(|v| !v.is_empty())
        .map(|v| PathBuf::from(v).join("Vitrobar"))
}

pub fn data_path() -> Option<PathBuf> {
    data_dir().map(|d| d.join(DATA_FILE))
}

/// `VITROBAR_CLAUDE_SETTINGS` if set, else `%USERPROFILE%\.claude\settings.json`.
pub fn claude_settings_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(CLAUDE_SETTINGS_ENV).filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    std::env::var_os("USERPROFILE")
        .filter(|p| !p.is_empty())
        .map(|p| PathBuf::from(p).join(".claude").join("settings.json"))
}

/// Writes `u` to `path` atomically (a temp file next to it, then a replacing rename).
pub fn save_at(path: &Path, u: &ClaudeUsage) -> std::io::Result<()> {
    let text = serde_json::to_string(u).map_err(std::io::Error::other)?;
    write_atomic(path, text.as_bytes())
}

/// `bytes` to `path` through a temp file in the same folder and a rename that replaces the
/// old file (`MoveFileExW` with `MOVEFILE_REPLACE_EXISTING`): a reader sees the old file or
/// the new one, never half of one.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let written = std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// The saved data; `None` when missing or unreadable.
pub fn read_at(path: &Path) -> Option<ClaudeUsage> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

pub fn read() -> Option<ClaudeUsage> {
    read_at(&data_path()?)
}

/// The shell Claude Code runs status-line commands with on Windows: Git Bash when it is
/// installed, else PowerShell (Claude Code docs, "Status line" > "Windows configuration").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusShell {
    GitBash(PathBuf),
    PowerShell,
}

/// Overrides the detected shell: `powershell`, or the path of a `bash.exe` (tests).
pub const STATUS_SHELL_ENV: &str = "VITROBAR_STATUS_SHELL";

/// Where Git Bash may be, in the order to try: `CLAUDE_CODE_GIT_BASH_PATH`, next to the
/// `git.exe` found on PATH (`<root>\cmd\git.exe`, `<root>\bin\git.exe` or
/// `<root>\mingw64\bin\git.exe` → `<root>\bin\bash.exe`), then `<Program Files>\Git`.
pub fn git_bash_candidates(
    env_path: Option<&Path>,
    git_exe: Option<&Path>,
    program_files: Option<&Path>,
) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = env_path.map(Path::to_path_buf).into_iter().collect();
    if let Some(dir) = git_exe.and_then(Path::parent) {
        // ...\cmd or ...\bin is one level under the Git root, ...\mingw64\bin two.
        let mingw = dir.parent().filter(|p| {
            p.file_name()
                .is_some_and(|n| n.eq_ignore_ascii_case("mingw64"))
        });
        let root = mingw.and_then(Path::parent).or_else(|| dir.parent());
        if let Some(root) = root {
            out.push(root.join("bin").join("bash.exe"));
        }
    }
    if let Some(pf) = program_files {
        out.push(pf.join("Git").join("bin").join("bash.exe"));
    }
    out.dedup();
    out
}

/// [`STATUS_SHELL_ENV`]'s value as a shell.
pub fn parse_shell_override(v: &str) -> StatusShell {
    if v.trim().eq_ignore_ascii_case("powershell") {
        StatusShell::PowerShell
    } else {
        StatusShell::GitBash(PathBuf::from(v.trim()))
    }
}

/// The shell Claude Code would pick on this machine.
pub fn status_shell() -> StatusShell {
    if let Some(v) = std::env::var_os(STATUS_SHELL_ENV).filter(|v| !v.is_empty()) {
        return parse_shell_override(&v.to_string_lossy());
    }
    let env = std::env::var_os("CLAUDE_CODE_GIT_BASH_PATH").map(PathBuf::from);
    let git = find_on_path("git.exe");
    let pf = std::env::var_os("ProgramFiles").map(PathBuf::from);
    git_bash_candidates(env.as_deref(), git.as_deref(), pf.as_deref())
        .into_iter()
        .find(|p| p.is_file())
        .map_or(StatusShell::PowerShell, StatusShell::GitBash)
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// The status-line command that runs `exe` as the bridge, written for `shell`: Git Bash
/// takes a double-quoted path with forward slashes (backslashes are escapes there);
/// PowerShell needs the call operator before a quoted path.
pub fn bridge_command(exe: &Path, shell: &StatusShell) -> String {
    let path = exe.display().to_string();
    match shell {
        StatusShell::GitBash(_) => format!("\"{}\" {BRIDGE_ARG}", path.replace('\\', "/")),
        StatusShell::PowerShell => format!("& '{}' {BRIDGE_ARG}", path.replace('\'', "''")),
    }
}

/// Whether `cmd` is a Vitrobar bridge (any install path).
pub fn is_bridge(cmd: &str) -> bool {
    cmd.trim_end().ends_with(BRIDGE_ARG)
}

/// Points Claude Code's `statusLine` at the bridge in the settings file `path` (missing file:
/// created), keeping every other key and the other keys of `statusLine`; 2-space JSON. A
/// bridge already there but running another exe (moved, renamed, another copy) is switched
/// to `bridge`: a missing exe would leave the widget's data frozen without a word.
pub fn wrap_statusline_at(path: &Path, bridge: &str) -> Result<Wrap, SetupError> {
    let mut root = read_settings(path)?;
    let line = root.get("statusLine").and_then(Value::as_object);
    let prev = line
        .and_then(|l| l.get("command"))
        .and_then(Value::as_str)
        .map(str::to_string);
    if prev.as_deref().is_some_and(is_bridge) {
        repoint(path, &mut root, bridge)?;
        return Ok(Wrap::AlreadyOurs);
    }
    let mut line = line.cloned().unwrap_or_default();
    line.insert("type".into(), Value::String("command".into()));
    line.insert("command".into(), Value::String(bridge.to_string()));
    root.insert("statusLine".into(), Value::Object(line));
    write_settings(path, &root)?;
    Ok(Wrap::Wrapped(prev))
}

/// At start-up with the widget on: a bridge in the settings file `path` that runs another exe
/// is switched to `bridge`. Any other command (the user's own, or none) is left alone, as is
/// the file when nothing changes. True if the file was rewritten.
pub fn repoint_statusline_at(path: &Path, bridge: &str) -> Result<bool, SetupError> {
    let mut root = read_settings(path)?;
    let cmd = root
        .get("statusLine")
        .and_then(|l| l.get("command"))
        .and_then(Value::as_str);
    if !cmd.is_some_and(is_bridge) {
        return Ok(false);
    }
    repoint(path, &mut root, bridge)
}

/// Writes `bridge` as the status-line command of `root` (whose command is a bridge) unless
/// it already is.
fn repoint(path: &Path, root: &mut Map<String, Value>, bridge: &str) -> Result<bool, SetupError> {
    let Some(Value::Object(line)) = root.get_mut("statusLine") else {
        return Ok(false);
    };
    if line.get("command").and_then(Value::as_str) == Some(bridge) {
        return Ok(false);
    }
    line.insert("command".into(), Value::String(bridge.to_string()));
    write_settings(path, root)?;
    Ok(true)
}

/// Puts `prev` back as the `statusLine` command (or removes `statusLine` when there was none)
/// if the current command is the bridge; otherwise leaves the file alone.
pub fn unwrap_statusline_at(path: &Path, prev: Option<&str>) -> Result<(), SetupError> {
    let mut root = read_settings(path)?;
    let ours = root
        .get("statusLine")
        .and_then(|l| l.get("command"))
        .and_then(Value::as_str)
        .is_some_and(is_bridge);
    if !ours {
        return Ok(());
    }
    match prev {
        Some(cmd) => {
            if let Some(Value::Object(line)) = root.get_mut("statusLine") {
                line.insert("command".into(), Value::String(cmd.to_string()));
            }
        }
        None => {
            root.shift_remove("statusLine");
        }
    }
    write_settings(path, &root)
}

/// The settings object; a missing file is an empty one.
fn read_settings(path: &Path) -> Result<Map<String, Value>, SetupError> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(e) => return Err(e.into()),
    };
    match serde_json::from_str(&text) {
        Ok(Value::Object(m)) => Ok(m),
        _ => Err(SetupError::Malformed),
    }
}

/// 2-space indented, like Claude Code writes it.
fn write_settings(path: &Path, root: &Map<String, Value>) -> Result<(), SetupError> {
    let mut text = serde_json::to_string_pretty(root).map_err(|_| SetupError::Malformed)?;
    text.push('\n');
    Ok(write_atomic(path, text.as_bytes())?)
}

pub fn wrap_statusline(exe: &Path) -> Result<Wrap, SetupError> {
    let path = claude_settings_path().ok_or(SetupError::Malformed)?;
    wrap_statusline_at(&path, &bridge_command(exe, &status_shell()))
}

pub fn repoint_statusline(exe: &Path) -> Result<bool, SetupError> {
    let path = claude_settings_path().ok_or(SetupError::Malformed)?;
    repoint_statusline_at(&path, &bridge_command(exe, &status_shell()))
}

pub fn unwrap_statusline(prev: Option<&str>) -> Result<(), SetupError> {
    let path = claude_settings_path().ok_or(SetupError::Malformed)?;
    unwrap_statusline_at(&path, prev)
}

/// Watches the data folder for the bridge's file (`ReadDirectoryChangesW`, overlapped, on a
/// worker thread; nothing polls). Dropping it stops the worker.
pub struct Watcher {
    /// Manual-reset event that ends the worker.
    stop: isize,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Watcher {
    fn drop(&mut self) {
        // SAFETY: the event handle is ours until the worker has finished (joined below).
        unsafe {
            let _ = SetEvent(HANDLE(self.stop as *mut _));
        }
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
        // SAFETY: the worker no longer waits on it.
        unsafe {
            let _ = CloseHandle(HANDLE(self.stop as *mut _));
        }
    }
}

/// Watches `%LOCALAPPDATA%\Vitrobar` for [`DATA_FILE`]; posts the payload-free `wake_msg` to
/// `notify` when it changes (the folder is created when missing).
pub fn watch(notify: HWND, wake_msg: u32) -> std::io::Result<Watcher> {
    let dir = data_dir().ok_or_else(|| std::io::Error::other("LOCALAPPDATA is not set"))?;
    let notify = notify.0 as isize;
    watch_dir(&dir, DATA_FILE, move || {
        // SAFETY: a payload-free post to a window of this process.
        unsafe {
            let _ = PostMessageW(Some(HWND(notify as *mut _)), wake_msg, WPARAM(0), LPARAM(0));
        }
    })
}

/// Calls `wake` (on the worker) whenever file `name` in `dir` is written, created or renamed
/// into place.
pub fn watch_dir(
    dir: &Path,
    name: &str,
    wake: impl Fn() + Send + 'static,
) -> std::io::Result<Watcher> {
    std::fs::create_dir_all(dir)?;
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: `wide` is a NUL-terminated path that outlives the call.
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            FILE_LIST_DIRECTORY.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
            None,
        )
    }
    .map_err(std::io::Error::other)?;
    // SAFETY: plain event creation; closed by the worker (io) and by Drop (stop).
    let events = unsafe {
        (
            CreateEventW(None, true, false, None),
            CreateEventW(None, true, false, None),
        )
    };
    let (io, stop) = match events {
        (Ok(io), Ok(stop)) => (io, stop),
        (a, b) => {
            // SAFETY: closing what was created.
            unsafe {
                let _ = CloseHandle(handle);
                for h in [a, b].into_iter().flatten() {
                    let _ = CloseHandle(h);
                }
            }
            return Err(std::io::Error::last_os_error());
        }
    };
    let name: Vec<u16> = name.encode_utf16().collect();
    let (h, ioh, stoph) = (handle.0 as isize, io.0 as isize, stop.0 as isize);
    let worker = std::thread::Builder::new()
        .name("vitro-claude-watch".into())
        .spawn(move || {
            watch_worker(
                HANDLE(h as *mut _),
                HANDLE(ioh as *mut _),
                HANDLE(stoph as *mut _),
                &name,
                &wake,
            )
        });
    match worker {
        Ok(w) => Ok(Watcher {
            stop: stop.0 as isize,
            worker: Some(w),
        }),
        Err(e) => {
            // SAFETY: the worker never started, so the handles are still ours.
            unsafe {
                let _ = CloseHandle(handle);
                let _ = CloseHandle(io);
                let _ = CloseHandle(stop);
            }
            Err(e)
        }
    }
}

fn watch_worker(dir: HANDLE, io: HANDLE, stop: HANDLE, name: &[u16], wake: &dyn Fn()) {
    // DWORD-aligned, as ReadDirectoryChangesW requires.
    let mut buf = vec![0u32; 4096];
    let bytes = (buf.len() * 4) as u32;
    loop {
        let mut ov = OVERLAPPED {
            hEvent: io,
            ..Default::default()
        };
        // SAFETY: `buf` and `ov` stay alive and unmoved until the read completes or is
        // cancelled and waited for below.
        let started = unsafe {
            let _ = ResetEvent(io);
            ReadDirectoryChangesW(
                dir,
                buf.as_mut_ptr().cast(),
                bytes,
                false,
                FILE_NOTIFY_CHANGE_FILE_NAME
                    | FILE_NOTIFY_CHANGE_LAST_WRITE
                    | FILE_NOTIFY_CHANGE_SIZE,
                None,
                Some(&mut ov),
                None,
            )
        };
        if started.is_err() {
            break;
        }
        // SAFETY: both handles are valid for the worker's lifetime.
        let which = unsafe { WaitForMultipleObjects(&[io, stop], false, INFINITE) };
        if which != WAIT_OBJECT_0 {
            // Stop (or a failed wait): cancel the read and wait for it to let go of `buf`.
            // SAFETY: as above.
            unsafe {
                let _ = CancelIoEx(dir, Some(&ov));
                let mut n = 0;
                let _ = GetOverlappedResult(dir, &ov, &mut n, true);
            }
            break;
        }
        let mut n = 0u32;
        // SAFETY: the read has completed.
        if unsafe { GetOverlappedResult(dir, &ov, &mut n, false) }.is_err() {
            break;
        }
        // n == 0: the buffer overflowed and the changes were lost; assume ours was one.
        if n == 0
            || names_in(&buf, n as usize)
                .iter()
                .any(|f| same_name(f, name))
        {
            wake();
        }
    }
    // SAFETY: the worker's own handles; nothing uses them after this.
    unsafe {
        let _ = CloseHandle(dir);
        let _ = CloseHandle(io);
    }
}

/// The file names in a `FILE_NOTIFY_INFORMATION` chain of `len` bytes.
fn names_in(buf: &[u32], len: usize) -> Vec<Vec<u16>> {
    let bytes: Vec<u8> = buf.iter().flat_map(|w| w.to_ne_bytes()).take(len).collect();
    let mut out = Vec::new();
    let mut at = 0usize;
    let u32_at = |i: usize| -> Option<u32> {
        Some(u32::from_ne_bytes(bytes.get(i..i + 4)?.try_into().ok()?))
    };
    while let (Some(next), Some(name_len)) = (u32_at(at), u32_at(at + 8)) {
        let start = at + 12;
        let Some(raw) = bytes.get(start..start + name_len as usize) else {
            break;
        };
        out.push(
            raw.as_chunks::<2>()
                .0
                .iter()
                .map(|&c| u16::from_ne_bytes(c))
                .collect(),
        );
        if next == 0 {
            break;
        }
        at += next as usize;
    }
    out
}

fn same_name(a: &[u16], b: &[u16]) -> bool {
    String::from_utf16_lossy(a).eq_ignore_ascii_case(&String::from_utf16_lossy(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vitro_core::widgets::Window;

    const EXE: &str = r"C:\Program Files\Vitrobar\vitrobar.exe";

    fn exe() -> PathBuf {
        PathBuf::from(EXE)
    }

    fn ours() -> String {
        bridge_command(&exe(), &StatusShell::GitBash(PathBuf::from("bash.exe")))
    }

    #[test]
    fn the_bridge_command_suits_the_shell() {
        assert_eq!(
            ours(),
            r#""C:/Program Files/Vitrobar/vitrobar.exe" --claude-statusline"#
        );
        assert_eq!(
            bridge_command(&exe(), &StatusShell::PowerShell),
            r"& 'C:\Program Files\Vitrobar\vitrobar.exe' --claude-statusline"
        );
        let odd = PathBuf::from(r"C:\Users\O'Neil\vitrobar.exe");
        assert_eq!(
            bridge_command(&odd, &StatusShell::PowerShell),
            r"& 'C:\Users\O''Neil\vitrobar.exe' --claude-statusline"
        );
        assert!(is_bridge(&bridge_command(&exe(), &StatusShell::PowerShell)));
    }

    #[test]
    fn git_bash_is_looked_for_where_claude_code_finds_it() {
        let c = git_bash_candidates(
            Some(Path::new(r"E:\bash.exe")),
            Some(Path::new(r"D:\Git\cmd\git.exe")),
            Some(Path::new(r"C:\Program Files")),
        );
        assert_eq!(
            c,
            vec![
                PathBuf::from(r"E:\bash.exe"),
                PathBuf::from(r"D:\Git\bin\bash.exe"),
                PathBuf::from(r"C:\Program Files\Git\bin\bash.exe"),
            ]
        );
        let c = git_bash_candidates(None, Some(Path::new(r"D:\Git\mingw64\bin\git.exe")), None);
        assert_eq!(c, vec![PathBuf::from(r"D:\Git\bin\bash.exe")]);
        let c = git_bash_candidates(None, Some(Path::new(r"D:\Git\bin\git.exe")), None);
        assert_eq!(c, vec![PathBuf::from(r"D:\Git\bin\bash.exe")]);
        assert!(git_bash_candidates(None, None, None).is_empty());
    }

    #[test]
    fn the_shell_override() {
        assert_eq!(parse_shell_override("powershell"), StatusShell::PowerShell);
        assert_eq!(parse_shell_override("PowerShell"), StatusShell::PowerShell);
        assert_eq!(
            parse_shell_override(r"D:\Git\bin\bash.exe"),
            StatusShell::GitBash(PathBuf::from(r"D:\Git\bin\bash.exe"))
        );
    }

    fn json(path: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn wrap_with_no_file_creates_the_status_line() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        assert_eq!(
            wrap_statusline_at(&p, &ours()).unwrap(),
            Wrap::Wrapped(None)
        );
        assert_eq!(
            json(&p),
            serde_json::json!({"statusLine": {"type": "command", "command": ours()}})
        );
    }

    #[test]
    fn wrap_keeps_the_rest_and_returns_the_old_command() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        let before = "{\n  \"model\": \"opus\",\n  \"statusLine\": {\n    \"type\": \"command\",\n    \"command\": \"node ~/line.js\",\n    \"padding\": 2\n  },\n  \"zeta\": [1, 2]\n}\n";
        std::fs::write(&p, before).unwrap();
        assert_eq!(
            wrap_statusline_at(&p, &ours()).unwrap(),
            Wrap::Wrapped(Some("node ~/line.js".into()))
        );
        let text = std::fs::read_to_string(&p).unwrap();
        // Key order kept, 2-space indentation.
        let (m, s, z) = (
            text.find("\"model\"").unwrap(),
            text.find("\"statusLine\"").unwrap(),
            text.find("\"zeta\"").unwrap(),
        );
        assert!(m < s && s < z, "{text}");
        assert!(text.contains("\n  \"model\": \"opus\""), "{text}");
        let v = json(&p);
        assert_eq!(v["statusLine"]["padding"], 2);
        assert_eq!(v["statusLine"]["command"], ours());
        assert_eq!(v["zeta"], serde_json::json!([1, 2]));
    }

    #[test]
    fn wrapping_twice_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        std::fs::write(
            &p,
            r#"{"statusLine":{"type":"command","command":"echo hi"}}"#,
        )
        .unwrap();
        wrap_statusline_at(&p, &ours()).unwrap();
        let once = std::fs::read_to_string(&p).unwrap();
        assert_eq!(wrap_statusline_at(&p, &ours()).unwrap(), Wrap::AlreadyOurs);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), once);
    }

    #[test]
    fn a_bridge_at_an_old_path_moves_to_the_new_one() {
        // The exe moved (folder renamed, another copy started): a status line that still
        // runs the old path runs nothing, and the widget's data would stop updating.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        std::fs::write(
            &p,
            r#"{"a":1,"statusLine":{"type":"command","command":"\"C:/old/vitrobar.exe\" --claude-statusline","padding":0}}"#,
        )
        .unwrap();
        assert_eq!(wrap_statusline_at(&p, &ours()).unwrap(), Wrap::AlreadyOurs);
        assert_eq!(
            json(&p),
            serde_json::json!({"a": 1, "statusLine": {"type": "command", "command": ours(), "padding": 0}})
        );
    }

    #[test]
    fn repoint_moves_an_old_bridge_and_leaves_the_rest_alone() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        let old = r#"{"statusLine":{"type":"command","command":"\"C:/old/vitrobar.exe\" --claude-statusline"}}"#;
        std::fs::write(&p, old).unwrap();
        assert!(repoint_statusline_at(&p, &ours()).unwrap());
        assert_eq!(json(&p)["statusLine"]["command"], ours());
        // Already right: the file is not written again.
        let once = std::fs::read_to_string(&p).unwrap();
        assert!(!repoint_statusline_at(&p, &ours()).unwrap());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), once);
        // The user's own command, or none: untouched.
        for theirs in [
            r#"{"statusLine":{"type":"command","command":"their-own"}}"#,
            r#"{"a":1}"#,
        ] {
            std::fs::write(&p, theirs).unwrap();
            assert!(!repoint_statusline_at(&p, &ours()).unwrap());
            assert_eq!(std::fs::read_to_string(&p).unwrap(), theirs);
        }
    }

    #[test]
    fn unwrap_restores_the_previous_command_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        std::fs::write(
            &p,
            r#"{"a":1,"statusLine":{"type":"command","command":"bash -c 'x \"y\"'","padding":0}}"#,
        )
        .unwrap();
        let Wrap::Wrapped(prev) = wrap_statusline_at(&p, &ours()).unwrap() else {
            panic!("not wrapped");
        };
        unwrap_statusline_at(&p, prev.as_deref()).unwrap();
        assert_eq!(
            json(&p),
            serde_json::json!({"a": 1, "statusLine": {"type": "command", "command": "bash -c 'x \"y\"'", "padding": 0}})
        );
    }

    #[test]
    fn unwrap_without_a_previous_command_removes_the_status_line() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        std::fs::write(&p, r#"{"theme":"dark"}"#).unwrap();
        wrap_statusline_at(&p, &ours()).unwrap();
        unwrap_statusline_at(&p, None).unwrap();
        assert_eq!(json(&p), serde_json::json!({"theme": "dark"}));
    }

    #[test]
    fn unwrap_leaves_a_command_the_user_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        let theirs = r#"{"statusLine":{"type":"command","command":"their-own"}}"#;
        std::fs::write(&p, theirs).unwrap();
        unwrap_statusline_at(&p, Some("old")).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), theirs);
    }

    #[test]
    fn malformed_settings_are_an_error_and_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        for bad in ["{ not json", "[1, 2]", "\"text\""] {
            std::fs::write(&p, bad).unwrap();
            assert!(matches!(
                wrap_statusline_at(&p, &ours()),
                Err(SetupError::Malformed)
            ));
            assert!(matches!(
                unwrap_statusline_at(&p, None),
                Err(SetupError::Malformed)
            ));
            assert_eq!(std::fs::read_to_string(&p).unwrap(), bad);
        }
    }

    #[test]
    fn the_data_file_round_trips_and_bad_files_read_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(DATA_FILE);
        assert_eq!(read_at(&p), None);
        let u = ClaudeUsage {
            five_hour: Some(Window {
                used: 42.0,
                resets_at: Some(10),
            }),
            seven_day: None,
            at: 5,
        };
        save_at(&p, &u).unwrap();
        assert_eq!(read_at(&p), Some(u));
        // No temp file is left behind.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        std::fs::write(&p, "garbage").unwrap();
        assert_eq!(read_at(&p), None);
    }

    #[test]
    fn bridge_commands_are_recognised() {
        assert!(is_bridge(&ours()));
        assert!(is_bridge(
            "\"D:\\other\\vitrobar.exe\" --claude-statusline  "
        ));
        assert!(!is_bridge("node line.js"));
    }
}

#[cfg(test)]
mod watch_tests {
    use super::*;
    use std::sync::mpsc::channel;
    use std::time::Duration;

    #[test]
    fn a_new_data_file_wakes_the_watcher_and_others_do_not() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = channel();
        let w = watch_dir(dir.path(), DATA_FILE, move || {
            let _ = tx.send(());
        })
        .unwrap();
        std::fs::write(dir.path().join("other.txt"), "x").unwrap();
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
        save_at(
            &dir.path().join(DATA_FILE),
            &ClaudeUsage {
                five_hour: None,
                seven_day: None,
                at: 1,
            },
        )
        .unwrap();
        assert!(rx.recv_timeout(Duration::from_secs(5)).is_ok());
        drop(w);
    }

    #[test]
    fn dropping_the_watcher_stops_it_promptly() {
        let dir = tempfile::tempdir().unwrap();
        let w = watch_dir(dir.path(), DATA_FILE, || {}).unwrap();
        let t = std::time::Instant::now();
        drop(w);
        assert!(t.elapsed() < Duration::from_secs(2));
    }
}
