//! What the Start panel lists: every installed app (the shell's `AppsFolder`, as the stock
//! Start menu's "All apps" shows it) and the recently opened files. Read-only and blocking;
//! needs COM (STA) on the calling thread.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use vitro_core::search::fold;
use vitro_core::settings::Pin;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::System::Com::{CoTaskMemFree, IBindCtx};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::UI::Shell::{
    BHID_EnumItems, IEnumShellItems, IShellItem, SHCreateItemFromParsingName, SIGDN,
    SIGDN_NORMALDISPLAY, SIGDN_PARENTRELATIVEPARSING,
};

use crate::pins_import::{known_folder_path, read_link};

/// One recently opened file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentFile {
    /// The file's name (with extension).
    pub name: String,
    /// Its full path.
    pub path: String,
}

/// `item`'s name of kind `sigdn`, `None` when it has none (or it is empty).
pub(crate) fn item_name(item: &IShellItem, sigdn: SIGDN) -> Option<String> {
    // SAFETY: the returned string is ours and freed right after copying it.
    unsafe {
        let p = item.GetDisplayName(sigdn).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s.filter(|s| !s.is_empty())
    }
}

/// Whether an `AppsFolder` entry belongs in the app list: uninstallers, readmes and web
/// links are not apps.
fn keep_app(name: &str, aumid: &str) -> bool {
    let n = name.to_lowercase();
    let a = aumid.to_lowercase();
    !(n.starts_with("uninstall")
        || n.contains("readme")
        || a.starts_with("http:")
        || a.starts_with("https:")
        || a.starts_with("ms-"))
}

/// Drops later entries with an AUMID already seen (ignoring case) and sorts by name, case-
/// and accent-insensitively (the search normalisation), ties by the name as written.
fn dedupe_and_sort(apps: Vec<Pin>) -> Vec<Pin> {
    let mut seen = HashSet::new();
    let mut out: Vec<(String, Pin)> = apps
        .into_iter()
        .filter(|p| seen.insert(p.aumid.as_deref().unwrap_or_default().to_lowercase()))
        .map(|p| (fold(&p.name), p))
        .collect();
    out.sort_by(|(fa, a), (fb, b)| fa.cmp(fb).then_with(|| a.name.cmp(&b.name)));
    out.into_iter().map(|(_, p)| p).collect()
}

fn enum_folder(parsing_name: &str) -> Option<IEnumShellItems> {
    let name = HSTRING::from(parsing_name);
    // SAFETY: `name` outlives the calls; plain shell calls on the caller's STA thread.
    unsafe {
        let folder: IShellItem =
            SHCreateItemFromParsingName(PCWSTR(name.as_ptr()), None::<&IBindCtx>).ok()?;
        folder
            .BindToHandler(None::<&IBindCtx>, &BHID_EnumItems)
            .ok()
    }
}

/// Every app in the shell's `AppsFolder`: `name` is its display name, `aumid` its parsing
/// name (the AUMID for packaged apps and apps that declare one, otherwise the program's
/// path, possibly as `{known folder}\rest`; `shell:AppsFolder\<aumid>` opens any of them).
/// Filtered by [`keep_app`], deduplicated by AUMID and sorted by folded name.
pub fn list_all_apps() -> Vec<Pin> {
    let Some(items) = enum_folder("shell:AppsFolder") else {
        return Vec::new();
    };
    let mut apps = Vec::new();
    loop {
        let mut batch: [Option<IShellItem>; 32] = Default::default();
        let mut fetched = 0u32;
        // SAFETY: `batch` and `fetched` are valid out-parameters for up to 32 items.
        let r = unsafe { items.Next(&mut batch, Some(&mut fetched)) };
        if r.is_err() || fetched == 0 {
            break;
        }
        for item in batch.iter().take(fetched as usize).flatten() {
            let (Some(name), Some(aumid)) = (
                item_name(item, SIGDN_NORMALDISPLAY),
                item_name(item, SIGDN_PARENTRELATIVEPARSING),
            ) else {
                continue;
            };
            if keep_app(&name, &aumid) {
                apps.push(Pin {
                    aumid: Some(aumid),
                    path: None,
                    name,
                    link: None,
                });
            }
        }
    }
    dedupe_and_sort(apps)
}

/// Whether Windows keeps recently opened files (Settings › Personalization › Start ›
/// "Show recently opened items": `Start_TrackDocs`, on when absent). When off, the Start
/// panel shows no recent files and does not read the Recent folder.
pub fn recent_docs_enabled() -> bool {
    let mut value: u32 = 1;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: `value` and `size` are locals passed with their sizes.
    let err = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced"),
            w!("Start_TrackDocs"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    err.is_err() || value != 0
}

/// The file system path behind an `AppsFolder` parsing name, when it is one: an absolute
/// path (`C:\...`) or a known folder followed by a path (`{GUID}\rest`). AUMIDs give `None`.
fn parsing_name_path(name: &str, known: impl Fn(&str) -> Option<String>) -> Option<String> {
    let b = name.as_bytes();
    if b.len() > 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\' {
        return Some(name.to_string());
    }
    let (guid, rest) = name.split_once('\\')?;
    if !guid.starts_with('{') || rest.is_empty() {
        return None;
    }
    let root = known(guid)?;
    Some(format!("{}\\{}", root.trim_end_matches('\\'), rest))
}

/// Where a pin's program lives, for "Open file location": its shortcut, its executable, or
/// the path behind its `AppsFolder` parsing name. `None` for packaged apps.
pub fn file_location(p: &Pin) -> Option<String> {
    let some = |v: &Option<String>| v.as_deref().filter(|s| !s.is_empty()).map(str::to_owned);
    some(&p.link)
        .or_else(|| some(&p.path))
        .or_else(|| parsing_name_path(p.aumid.as_deref()?, known_folder_path))
}

/// The bar pin for an `AppsFolder` entry (see [`list_all_apps`]): a desktop program whose
/// parsing name is a path becomes a path pin (the bar groups and launches those by their
/// executable); a real AUMID stays an AUMID pin. Other pins are returned unchanged.
pub fn bar_pin(p: &Pin) -> Pin {
    if p.link.is_some() || p.path.is_some() {
        return p.clone();
    }
    match p
        .aumid
        .as_deref()
        .and_then(|a| parsing_name_path(a, known_folder_path))
    {
        Some(path) => Pin {
            aumid: None,
            path: Some(path),
            name: p.name.clone(),
            link: None,
        },
        None => p.clone(),
    }
}

fn recent_folder() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join(r"Microsoft\Windows\Recent"))
}

/// The `.lnk` files in `dir`, newest first (by modification time).
fn links_newest_first(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut links: Vec<(SystemTime, PathBuf)> = rd
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("lnk"))
        })
        .map(|e| {
            let m = e
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            (m, e.path())
        })
        .collect();
    links.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    links.into_iter().map(|(_, p)| p).collect()
}

/// Up to `max` recently opened files, newest first: the shortcuts Windows keeps in
/// `%APPDATA%\Microsoft\Windows\Recent`, read (never resolved) and kept only when they point
/// at a regular file that still exists. Each file appears once.
pub fn recent_files(max: usize) -> Vec<RecentFile> {
    recent_folder()
        .map(|dir| recent_in(&dir, max))
        .unwrap_or_default()
}

/// [`recent_files`] for the shortcuts in `dir`.
fn recent_in(dir: &Path, max: usize) -> Vec<RecentFile> {
    let mut out: Vec<RecentFile> = Vec::new();
    if max == 0 {
        return out;
    }
    let mut seen = HashSet::new();
    for link in links_newest_first(dir) {
        let Some(info) = read_link(&link) else {
            continue;
        };
        let target = Path::new(&info.target);
        if info.target.is_empty() || !target.is_file() {
            continue;
        }
        let Some(name) = target.file_name() else {
            continue;
        };
        if !seen.insert(info.target.to_lowercase()) {
            continue;
        }
        out.push(RecentFile {
            name: name.to_string_lossy().into_owned(),
            path: info.target.clone(),
        });
        if out.len() >= max {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use windows::core::Interface;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, IPersistFile, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};

    /// Writes a shortcut at `lnk` pointing at `target`, last modified `age_s` seconds ago.
    fn make_link(lnk: &Path, target: &Path, age_s: u64) {
        // SAFETY: in-process COM on this test thread; the strings outlive the calls.
        unsafe {
            let link: IShellLinkW =
                CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
            link.SetPath(&HSTRING::from(target.as_os_str())).unwrap();
            link.cast::<IPersistFile>()
                .unwrap()
                .Save(&HSTRING::from(lnk.as_os_str()), true)
                .unwrap();
        }
        let f = std::fs::File::options().write(true).open(lnk).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(age_s))
            .unwrap();
    }

    #[test]
    fn recent_in_keeps_existing_files_newest_first() {
        // SAFETY: per-thread apartment for the shortcut objects (left to thread exit).
        let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        let root = std::env::temp_dir().join(format!("vitrobar-recent-{}", std::process::id()));
        let (files, links) = (root.join("files"), root.join("links"));
        std::fs::create_dir_all(&files).unwrap();
        std::fs::create_dir_all(&links).unwrap();
        let old = files.join("old.txt");
        let new = files.join("new.txt");
        let gone = files.join("gone.txt");
        for f in [&old, &new, &gone] {
            std::fs::write(f, "x").unwrap();
        }
        make_link(&links.join("old.txt.lnk"), &old, 300);
        make_link(&links.join("new.txt.lnk"), &new, 10);
        make_link(&links.join("again.lnk"), &new, 5);
        make_link(&links.join("gone.txt.lnk"), &gone, 1);
        make_link(&links.join("files.lnk"), &files, 2);
        std::fs::remove_file(&gone).unwrap();

        let got = recent_in(&links, 10);
        let names: Vec<&str> = got.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            ["new.txt", "old.txt"],
            "no folder, no missing file, once each"
        );
        // The same file, however the temp folder is spelled (CI's is an 8.3 short path, the
        // shortcut resolves to the long one).
        let same = |a: &str, b: &std::path::Path| {
            std::fs::canonicalize(a).ok() == std::fs::canonicalize(b).ok()
        };
        assert!(same(&got[0].path, &new), "{} vs {}", got[0].path, new.display());
        assert_eq!(recent_in(&links, 1).len(), 1);
        assert!(recent_in(&links, 0).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    fn pin(name: &str, aumid: &str) -> Pin {
        Pin {
            aumid: Some(aumid.into()),
            path: None,
            name: name.into(),
            link: None,
        }
    }

    #[test]
    fn parsing_names_that_are_paths() {
        let known = |g: &str| (g == "{AAAA}").then(|| "C:\\Program Files\\".to_string());
        assert_eq!(
            parsing_name_path(r"C:\Tools\x.exe", known).as_deref(),
            Some(r"C:\Tools\x.exe")
        );
        assert_eq!(
            parsing_name_path(r"{AAAA}\App\app.exe", known).as_deref(),
            Some(r"C:\Program Files\App\app.exe")
        );
        assert_eq!(parsing_name_path(r"{BBBB}\App\app.exe", known), None);
        assert_eq!(
            parsing_name_path("Microsoft.Paint_8wekyb3d8bbwe!App", known),
            None
        );
        assert_eq!(parsing_name_path("Chrome", known), None);
    }

    #[test]
    fn bar_pin_keeps_aumids_and_resolves_paths() {
        let packaged = pin("Paint", "Microsoft.Paint_8wekyb3d8bbwe!App");
        assert_eq!(bar_pin(&packaged), packaged);
        let desktop = pin("Tool", r"C:\Tools\tool.exe");
        let b = bar_pin(&desktop);
        assert_eq!(
            (b.aumid, b.path.as_deref()),
            (None, Some(r"C:\Tools\tool.exe"))
        );
        assert_eq!(
            file_location(&desktop).as_deref(),
            Some(r"C:\Tools\tool.exe")
        );
        assert_eq!(file_location(&packaged), None);
    }

    #[test]
    fn keeps_apps_but_not_uninstallers_readmes_or_links() {
        assert!(keep_app("Paint", "Microsoft.Paint_8wekyb3d8bbwe!App"));
        assert!(keep_app("Install Helper", r"C:\x\install.exe"));
        assert!(!keep_app("Uninstall Foo", r"C:\foo\unins000.exe"));
        assert!(!keep_app("UNINSTALL", "x"));
        assert!(!keep_app("Foo ReadMe", r"C:\foo\readme.txt"));
        assert!(!keep_app("Foo website", "https://example.com/"));
        assert!(!keep_app("Foo", "HTTP://example.com/"));
        assert!(!keep_app("Settings link", "ms-settings:display"));
    }

    #[test]
    fn dedupes_by_aumid_and_sorts_folded() {
        let v = dedupe_and_sort(vec![
            pin("Zeta", "z"),
            pin("écran", "e"),
            pin("Alpha", "A"),
            pin("alpha copy", "a"),
            pin("Être", "x"),
            pin("beta", "b"),
        ]);
        let names: Vec<&str> = v.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Alpha", "beta", "écran", "Être", "Zeta"]);
    }
}
