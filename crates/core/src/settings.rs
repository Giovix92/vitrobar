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
        .and_then(|t| toml::from_str::<Settings>(t).ok());
    match parsed {
        Some(s) => (s, LoadStatus::Loaded),
        None => {
            let _ = std::fs::write(bad_copy_path(path), &bytes);
            let s = Settings {
                first_run_done: true,
                ..Settings::default()
            };
            (s, LoadStatus::Corrupt)
        }
    }
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
}
