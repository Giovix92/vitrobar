//! Persistent user settings (TOML).
//!
//! Runtime location used by the app: `%APPDATA%\Vitrobar\settings.toml`.

use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

macro_rules! setting_enum {
    ($(#[$m:meta])* $name:ident { $first:ident $(, $rest:ident)* $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
        #[serde(rename_all = "kebab-case")]
        pub enum $name {
            #[default]
            $first,
            $($rest),*
        }
    };
}

setting_enum!(Shape {
    Pill,
    Islands,
    Full
});
setting_enum!(Material {
    Frosted,
    Liquid,
    Smoked,
    SolidBlack,
    SolidWhite
});
setting_enum!(HoverEffect {
    Plate,
    Lift,
    Magnify,
    Sweep
});
setting_enum!(IconSize {
    Medium,
    Small,
    Large
});
setting_enum!(Position { Bottom, Top });
setting_enum!(StockTaskbar { Hidden, AutoHide });
setting_enum!(StartStyle { Panel, FullScreen });
setting_enum!(ThemeChoice {
    System,
    Light,
    Dark
});

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Pin {
    pub aumid: Option<String>,
    pub path: Option<String>,
    pub name: String,
    /// The shortcut this pin was imported from. Launching opens it (keeping its arguments,
    /// such as a web app's id); `aumid` and `path` still decide which windows it groups.
    pub link: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Settings {
    pub shape: Shape,
    pub material: Material,
    pub hover: HoverEffect,
    pub icon_size: IconSize,
    pub position: Position,
    pub reserve_space_floating: bool,
    pub stock_taskbar: StockTaskbar,
    pub theme: ThemeChoice,
    /// `None` = Windows accent, otherwise `0xRRGGBB`.
    pub accent: Option<u32>,
    pub battery_fallback: bool,
    pub live_refraction: bool,
    pub start_with_windows: bool,
    /// `None` = system language.
    pub language: Option<String>,
    pub pins: Vec<Pin>,
    /// Tray identity keys shown outside the overflow.
    pub tray_promoted: Vec<String>,
    pub first_run_done: bool,
    pub start_style: StartStyle,
    /// The Windows key opens the Vitrobar Start instead of the stock one.
    pub win_key_opens_start: bool,
    /// Start-panel pins. Filled from `pins` on the first open (see `start_pins_initialized`).
    pub start_pins: Vec<Pin>,
    /// The Start pins were set up once (copied from the bar on the first open): an empty
    /// list then means the user unpinned everything, not "fill from the bar".
    pub start_pins_initialized: bool,
    /// Bar widgets, off by default: the Music pill and the Claude-limits pill.
    pub widget_music: bool,
    pub widget_claude: bool,
    /// The `statusLine` entry of Claude Code's settings that the Claude widget replaced, kept
    /// so turning the widget off puts it back exactly (`None` = there was none).
    pub claude_prev_statusline: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            shape: Shape::default(),
            material: Material::default(),
            hover: HoverEffect::default(),
            icon_size: IconSize::default(),
            position: Position::default(),
            reserve_space_floating: true,
            stock_taskbar: StockTaskbar::default(),
            theme: ThemeChoice::default(),
            accent: None,
            battery_fallback: true,
            live_refraction: false,
            start_with_windows: true,
            language: None,
            pins: Vec::new(),
            tray_promoted: Vec::new(),
            first_run_done: false,
            start_style: StartStyle::default(),
            win_key_opens_start: true,
            start_pins: Vec::new(),
            start_pins_initialized: false,
            widget_music: false,
            widget_claude: false,
            claude_prev_statusline: None,
        }
    }
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name: OsString = path.file_name().unwrap_or_default().to_owned();
    name.push(suffix);
    path.with_file_name(name)
}

/// How [`load_with_status`] found the settings file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadStatus {
    /// No file (or it cannot be read): a genuine first run.
    Missing,
    Loaded,
    /// The file exists but does not parse. It was copied aside (see [`load_with_status`]);
    /// this is not a first run.
    Corrupt,
}

/// Loads settings. See [`load_with_status`].
pub fn load(path: &Path) -> Settings {
    load_with_status(path).0
}

/// Loads settings. A missing file gives defaults (a first run). A file that exists but fails
/// to parse is copied to `<name>.bad-<unix ms>` (earlier copies are kept) and gives defaults
/// with `first_run_done = true`: the user has run Vitrobar before, so nothing of the first
/// run (pin import, "Start with Windows" registration) happens again. The caller must not
/// touch the `Run` value either ([`LoadStatus::Corrupt`]).
pub fn load_with_status(path: &Path) -> (Settings, LoadStatus) {
    let Ok(bytes) = std::fs::read(path) else {
        return (Settings::default(), LoadStatus::Missing);
    };
    let parsed = std::str::from_utf8(&bytes)
        .ok()
        .and_then(|t| t.parse::<toml::Table>().ok());
    match parsed {
        Some(t) => (from_table(&t), LoadStatus::Loaded),
        None => {
            if !identical_bad_copy_exists(path, &bytes) {
                let _ = std::fs::write(bad_copy_path(path), &bytes);
            }
            let s = Settings {
                first_run_done: true,
                ..Settings::default()
            };
            (s, LoadStatus::Corrupt)
        }
    }
}

/// Builds settings field by field: a missing or invalid value gives that field's default
/// and nothing else; unknown keys are ignored (keys are case-sensitive: `Shape` is not
/// `shape`). One exception: a `first-run-done` that is present but not a boolean reads as
/// `true`, so a damaged value never re-runs the first-run import over the user's pins.
fn from_table(t: &toml::Table) -> Settings {
    let d = Settings::default();
    fn get<T: serde::de::DeserializeOwned>(t: &toml::Table, key: &str, default: T) -> T {
        t.get(key)
            .cloned()
            .and_then(|v| v.try_into().ok())
            .unwrap_or(default)
    }
    /// A list keeps the elements that deserialize and drops only the bad ones; a value that
    /// is not an array gives an empty list.
    fn get_list<T: serde::de::DeserializeOwned>(t: &toml::Table, key: &str) -> Vec<T> {
        match t.get(key) {
            Some(toml::Value::Array(a)) => {
                a.iter().filter_map(|v| v.clone().try_into().ok()).collect()
            }
            _ => Vec::new(),
        }
    }
    Settings {
        shape: get(t, "shape", d.shape),
        material: get(t, "material", d.material),
        hover: get(t, "hover", d.hover),
        icon_size: get(t, "icon-size", d.icon_size),
        position: get(t, "position", d.position),
        reserve_space_floating: get(t, "reserve-space-floating", d.reserve_space_floating),
        stock_taskbar: get(t, "stock-taskbar", d.stock_taskbar),
        theme: get(t, "theme", d.theme),
        accent: get(t, "accent", d.accent),
        battery_fallback: get(t, "battery-fallback", d.battery_fallback),
        live_refraction: get(t, "live-refraction", d.live_refraction),
        start_with_windows: get(t, "start-with-windows", d.start_with_windows),
        language: get(t, "language", d.language),
        pins: get_list(t, "pins"),
        tray_promoted: get_list(t, "tray-promoted"),
        first_run_done: match t.get("first-run-done") {
            None => d.first_run_done,
            Some(v) => v.as_bool().unwrap_or(true),
        },
        start_style: get(t, "start-style", d.start_style),
        win_key_opens_start: get(t, "win-key-opens-start", d.win_key_opens_start),
        start_pins: get_list(t, "start-pins"),
        start_pins_initialized: get(t, "start-pins-initialized", d.start_pins_initialized),
        widget_music: get(t, "widget-music", d.widget_music),
        widget_claude: get(t, "widget-claude", d.widget_claude),
        claude_prev_statusline: get(t, "claude-prev-statusline", d.claude_prev_statusline),
    }
}

/// True if some `<name>.bad-*` next to `path` already holds exactly these bytes.
fn identical_bad_copy_exists(path: &Path, bytes: &[u8]) -> bool {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return false;
    };
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    let prefix = format!("{}.bad-", name.to_string_lossy());
    let Ok(rd) = std::fs::read_dir(dir) else {
        return false;
    };
    rd.flatten().any(|e| {
        e.file_name().to_string_lossy().starts_with(&prefix)
            && std::fs::read(e.path()).is_ok_and(|b| b == bytes)
    })
}

/// `<name>.bad-<unix ms>`, with a counter if that name is taken: never overwrites an
/// earlier copy.
fn bad_copy_path(path: &Path) -> PathBuf {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let first = with_suffix(path, &format!(".bad-{ms}"));
    let mut candidate = first.clone();
    let mut n = 1;
    while candidate.exists() {
        candidate = with_suffix(path, &format!(".bad-{ms}-{n}"));
        n += 1;
    }
    candidate
}

/// Atomically writes settings (`<path>.tmp` then rename), creating the folder if needed.
pub fn save(path: &Path, s: &Settings) -> io::Result<()> {
    let text =
        toml::to_string_pretty(s).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = with_suffix(path, ".tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn missing_file_gives_defaults() {
        let d = tempfile::tempdir().unwrap();
        let s = load(&d.path().join("settings.toml"));
        assert_eq!(s.shape, Shape::Pill);
        assert_eq!(s.material, Material::Frosted);
        assert_eq!(s.hover, HoverEffect::Plate);
        assert!(s.reserve_space_floating);
        assert!(s.start_with_windows);
        assert!(!s.first_run_done);
    }

    #[test]
    fn roundtrip() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.toml");
        let original = Settings {
            shape: Shape::Islands,
            material: Material::SolidBlack,
            stock_taskbar: StockTaskbar::AutoHide,
            accent: Some(0x112233),
            language: Some("it".into()),
            pins: vec![Pin {
                aumid: Some("Microsoft.Edge".into()),
                path: None,
                name: "Edge".into(),
                link: Some(r"C:\x\Edge.lnk".into()),
            }],
            tray_promoted: vec!["vol".into()],
            ..Settings::default()
        };
        save(&p, &original).unwrap();
        assert!(!d.path().join("settings.toml.tmp").exists());
        assert_eq!(load(&p), original);
    }

    #[test]
    fn roundtrip_every_field_non_default() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.toml");
        let pin = |n: &str| Pin {
            aumid: Some(format!("{n}!App")),
            path: Some(format!(r"C:\Apps\{n}.exe")),
            name: n.into(),
            link: Some(format!(r"C:\Links\{n}.lnk")),
        };
        // Spelled out (no `..Default`): a new field must be added here to compile.
        let original = Settings {
            shape: Shape::Full,
            material: Material::Liquid,
            hover: HoverEffect::Sweep,
            icon_size: IconSize::Large,
            position: Position::Top,
            reserve_space_floating: false,
            stock_taskbar: StockTaskbar::AutoHide,
            theme: ThemeChoice::Dark,
            accent: Some(0xA1B2C3),
            battery_fallback: false,
            live_refraction: true,
            start_with_windows: false,
            language: Some("it".into()),
            pins: vec![pin("Edge"), pin("Paint")],
            tray_promoted: vec!["vol".into(), "net".into()],
            first_run_done: true,
            start_style: StartStyle::FullScreen,
            win_key_opens_start: false,
            start_pins: vec![pin("Calc")],
            start_pins_initialized: true,
            widget_music: true,
            widget_claude: true,
            claude_prev_statusline: Some(r#"{"type":"command"}"#.into()),
        };
        let d0 = Settings::default();
        assert_ne!(original.hover, d0.hover);
        assert_ne!(original.icon_size, d0.icon_size);
        assert_ne!(original.position, d0.position);
        assert_ne!(original.theme, d0.theme);
        assert_ne!(original.start_style, d0.start_style);
        save(&p, &original).unwrap();
        assert_eq!(load(&p), original);
    }

    #[test]
    fn mis_cased_keys_are_ignored() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.toml");
        let text = [
            "Shape = \"full\"",
            "ICON-SIZE = \"large\"",
            "position = \"top\"",
        ];
        fs::write(&p, text.join("\n")).unwrap();
        let s = load(&p);
        assert_eq!(s.shape, Shape::Pill);
        assert_eq!(s.icon_size, IconSize::Medium);
        assert_eq!(s.position, Position::Top);
    }

    #[test]
    fn an_invalid_first_run_done_counts_as_done() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.toml");
        for bad in ["\"yes\"", "1", "\"false\"", "[]"] {
            fs::write(&p, format!("first-run-done = {bad}")).unwrap();
            assert!(load(&p).first_run_done, "{bad} must read as done");
        }
        fs::write(&p, "first-run-done = false").unwrap();
        assert!(!load(&p).first_run_done);
        fs::write(&p, "shape = \"full\"").unwrap();
        assert!(!load(&p).first_run_done, "absent is still a first run");
    }

    #[test]
    fn save_creates_the_parent_directory() {
        let d = tempfile::tempdir().unwrap();
        let p = d
            .path()
            .join("Vitrobar")
            .join("nested")
            .join("settings.toml");
        save(&p, &Settings::default()).unwrap();
        assert_eq!(load(&p), Settings::default());
    }

    #[test]
    fn partial_file_keeps_other_defaults() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.toml");
        fs::write(&p, "shape = \"full\"").unwrap();
        let s = load(&p);
        assert_eq!(s.shape, Shape::Full);
        assert_eq!(s.material, Material::Frosted);
    }

    #[test]
    fn unknown_keys_ignored() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.toml");
        fs::write(&p, "shape = \"full\"\nfoo = 1").unwrap();
        assert_eq!(load(&p).shape, Shape::Full);
    }

    #[test]
    fn corrupt_file_gives_defaults_and_keeps_backup() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.toml");
        fs::write(&p, "shape = [").unwrap();
        let (s, status) = load_with_status(&p);
        assert_eq!(status, LoadStatus::Corrupt);
        // Defaults, but not a first run: nothing is imported or registered again.
        assert!(s.first_run_done);
        assert_eq!(
            s,
            Settings {
                first_run_done: true,
                ..Settings::default()
            }
        );
        let bad: Vec<_> = fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| n.starts_with("settings.toml.bad-"))
            .collect();
        assert_eq!(bad.len(), 1, "{bad:?}");
        assert_eq!(fs::read(d.path().join(&bad[0])).unwrap(), b"shape = [");
    }

    #[test]
    fn earlier_bad_copies_are_kept() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.toml");
        fs::write(&p, "shape = [").unwrap();
        let _ = load(&p);
        fs::write(&p, "material = {").unwrap();
        let _ = load(&p);
        let mut contents: Vec<Vec<u8>> = fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("settings.toml.bad-")
            })
            .map(|e| fs::read(e.path()).unwrap())
            .collect();
        contents.sort();
        assert_eq!(
            contents,
            vec![b"material = {".to_vec(), b"shape = [".to_vec()]
        );
    }

    #[test]
    fn missing_and_good_files_report_their_status() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.toml");
        assert_eq!(load_with_status(&p).1, LoadStatus::Missing);
        fs::write(&p, "shape = \"full\"").unwrap();
        assert_eq!(load_with_status(&p).1, LoadStatus::Loaded);
    }

    #[test]
    fn kebab_case_spellings() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.toml");
        fs::write(
            &p,
            "material = \"solid-black\"\nstock-taskbar = \"auto-hide\"\nicon-size = \"large\"",
        )
        .unwrap();
        let s = load(&p);
        assert_eq!(s.material, Material::SolidBlack);
        assert_eq!(s.stock_taskbar, StockTaskbar::AutoHide);
        assert_eq!(s.icon_size, IconSize::Large);
    }

    #[test]
    fn old_file_without_new_fields_loads() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.toml");
        fs::write(
            &p,
            "shape = \"islands\"
first-run-done = true",
        )
        .unwrap();
        let (s, st) = load_with_status(&p);
        assert_eq!(st, LoadStatus::Loaded);
        assert_eq!(s.shape, Shape::Islands);
        assert_eq!(s.start_style, StartStyle::Panel);
        assert!(s.win_key_opens_start);
        assert!(s.start_pins.is_empty());
    }

    #[test]
    fn one_bad_value_resets_only_that_field() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.toml");
        fs::write(
            &p,
            "shape = \"hexagon\"
position = \"top\"",
        )
        .unwrap();
        let (s, st) = load_with_status(&p);
        assert_eq!(st, LoadStatus::Loaded);
        assert_eq!(s.shape, Shape::Pill);
        assert_eq!(s.position, Position::Top);
        assert!(!d.path().read_dir().unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".bad-")));
    }

    #[test]
    fn wrong_type_resets_only_that_field() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.toml");
        fs::write(
            &p,
            "accent = \"red\"
win-key-opens-start = false
start-style = 3",
        )
        .unwrap();
        let s = load(&p);
        assert_eq!(s.accent, None);
        assert!(!s.win_key_opens_start);
        assert_eq!(s.start_style, StartStyle::Panel);
    }

    #[test]
    fn identical_corruption_writes_one_bad_copy() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.toml");
        fs::write(&p, "shape = [").unwrap();
        assert_eq!(load_with_status(&p).1, LoadStatus::Corrupt);
        assert_eq!(load_with_status(&p).1, LoadStatus::Corrupt);
        let n = fs::read_dir(d.path())
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("settings.toml.bad-")
            })
            .count();
        assert_eq!(n, 1);
    }

    #[test]
    fn one_bad_list_element_keeps_the_others() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.toml");
        fs::write(
            &p,
            "tray-promoted = [\"a\", 5, \"b\"]
start-pins = 7

[[pins]]
name = \"Good\"

[[pins]]
name = 3
",
        )
        .unwrap();
        let s = load(&p);
        assert_eq!(s.pins.len(), 1);
        assert_eq!(s.pins[0].name, "Good");
        assert_eq!(s.tray_promoted, vec!["a".to_string(), "b".to_string()]);
        assert!(s.start_pins.is_empty());
    }

    #[test]
    fn different_corruption_writes_a_second_bad_copy() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.toml");
        fs::write(&p, "shape = [").unwrap();
        let _ = load(&p);
        fs::write(&p, "material = {").unwrap();
        let _ = load(&p);
        let _ = load(&p);
        let n = fs::read_dir(d.path())
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("settings.toml.bad-")
            })
            .count();
        assert_eq!(n, 2);
    }

    #[test]
    fn new_fields_roundtrip() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.toml");
        let o = Settings {
            start_style: StartStyle::FullScreen,
            win_key_opens_start: false,
            start_pins: vec![Pin {
                name: "X".into(),
                ..Pin::default()
            }],
            start_pins_initialized: true,
            ..Settings::default()
        };
        save(&p, &o).unwrap();
        assert_eq!(load(&p), o);
    }

    #[test]
    fn widget_fields_default_off_and_round_trip() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.toml");
        let s = Settings::default();
        assert!(!s.widget_music && !s.widget_claude && s.claude_prev_statusline.is_none());
        let o = Settings {
            widget_music: true,
            claude_prev_statusline: Some("node x.js".into()),
            ..Settings::default()
        };
        save(&p, &o).unwrap();
        let text = fs::read_to_string(&p).unwrap();
        assert!(text.contains("widget-music = true"), "{text}");
        assert!(text.contains("claude-prev-statusline"), "{text}");
        assert_eq!(load(&p), o);
    }

    #[test]
    fn a_bad_widget_value_resets_only_that_field() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.toml");
        fs::write(
            &p,
            "widget-music = \"yes\"
widget-claude = true
claude-prev-statusline = 5",
        )
        .unwrap();
        let (s, st) = load_with_status(&p);
        assert_eq!(st, LoadStatus::Loaded);
        assert!(!s.widget_music);
        assert!(s.widget_claude);
        assert_eq!(s.claude_prev_statusline, None);
    }
}
