//! First-run import of what the stock taskbar shows: its pinned apps and the tray icons
//! promoted out of its overflow. Read-only on the registry and the file system: shortcuts
//! are loaded (never resolved or saved), registry keys are opened with `KEY_READ`.
//!
//! Pins: the `.lnk` files of `User Pinned\TaskBar`, ordered like the stock taskbar by where
//! their names first occur in the Taskband `Favorites` value (an undocumented blob; it is
//! only scanned for text, and anything not found falls back to creation-time order). The
//! blob also names apps pinned from elsewhere: shortcuts that live in the Start menu folders
//! and packaged apps (`<family>!<app id>`), which are imported the same way.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use vitro_core::settings::Pin;
use vitro_core::tray_model::tray_key;
use windows::core::{Interface, BSTR, GUID, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID;
use windows::Win32::System::Com::{
    CoCreateInstance, CoTaskMemFree, IBindCtx, IPersistFile, CLSCTX_INPROC_SERVER, STGM_READ,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
    REG_ROUTINE_FLAGS, RRF_RT_REG_BINARY, RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::Win32::UI::Shell::{
    IShellItem, IShellLinkW, SHCreateItemFromParsingName, SHGetKnownFolderPath, ShellLink,
    KF_FLAG_DEFAULT, SIGDN_NORMALDISPLAY,
};

const TASKBAND: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\Taskband";
const NOTIFY_ICON_SETTINGS: &str = r"Control Panel\NotifyIconSettings";
/// Start-menu folders are searched this deep for shortcuts pinned from there.
const START_MENU_DEPTH: usize = 4;

// ---------------------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------------------

fn ascii_lower(u: u16) -> u16 {
    if (u16::from(b'A')..=u16::from(b'Z')).contains(&u) {
        u + 32
    } else {
        u
    }
}

fn lower_units(s: &str) -> Vec<u16> {
    s.encode_utf16().map(ascii_lower).collect()
}

/// A blob read as UTF-16LE text at both byte alignments, ASCII-lowercased.
struct BlobText {
    /// `units[a][i]` is the unit at byte offset `a + 2 * i`.
    units: [Vec<u16>; 2],
}

impl BlobText {
    fn new(blob: &[u8]) -> Self {
        let at = |align: usize| -> Vec<u16> {
            blob.get(align..)
                .unwrap_or_default()
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| ascii_lower(u16::from_le_bytes(*c)))
                .collect()
        };
        Self {
            units: [at(0), at(1)],
        }
    }

    fn matches_at(&self, byte_off: usize, pat: &[u16]) -> bool {
        let units = &self.units[byte_off % 2];
        let i = byte_off / 2;
        units.get(i..i + pat.len()) == Some(pat)
    }

    /// Byte offsets of `pat`, ascending.
    fn occurrences<'a>(&'a self, pat: &'a [u16]) -> impl Iterator<Item = usize> + 'a {
        let len = 2 * self.units[0].len() + 1;
        (0..len).filter(move |&off| !pat.is_empty() && self.matches_at(off, pat))
    }

    /// First offset of `name` that is not just the tail of a longer name in `names` found
    /// right there ("Code.lnk" inside "Visual Studio Code.lnk").
    fn first(&self, name: &[u16], names: &[Vec<u16>]) -> Option<usize> {
        self.occurrences(name).find(|&off| {
            !names.iter().any(|longer| {
                longer.len() > name.len()
                    && longer.ends_with(name)
                    && off
                        .checked_sub(2 * (longer.len() - name.len()))
                        .is_some_and(|o| self.matches_at(o, longer))
            })
        })
    }
}

/// Byte offset of the first occurrence of `needle` in `blob` as UTF-16LE text, at any byte
/// alignment (the blob does not keep its strings aligned), ASCII case-insensitive.
#[cfg(test)]
fn find_utf16(blob: &[u8], needle: &str) -> Option<usize> {
    BlobText::new(blob).first(&lower_units(needle), &[])
}

/// First offset of each of `names` in `blob` (see [`BlobText::first`]).
fn offsets_in_blob(names: &[String], blob: &[u8]) -> Vec<Option<usize>> {
    let text = BlobText::new(blob);
    let pats: Vec<Vec<u16>> = names.iter().map(|n| lower_units(n)).collect();
    pats.iter().map(|p| text.first(p, &pats)).collect()
}

/// Stable order by offset, items without one last (in input order).
fn by_offset<T>(mut items: Vec<(Option<usize>, T)>) -> Vec<T> {
    items.sort_by_key(|(off, _)| off.unwrap_or(usize::MAX));
    items.into_iter().map(|(_, t)| t).collect()
}

/// Orders `names` (shortcut file names) by where they first occur in `blob` as UTF-16
/// text; names that do not occur keep their input order after the others.
pub fn order_by_blob(names: &[String], blob: &[u8]) -> Vec<String> {
    by_offset(
        offsets_in_blob(names, blob)
            .into_iter()
            .zip(names.iter().cloned())
            .collect(),
    )
}

fn aumid_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '!')
}

/// `<name>_<13-char publisher id>!<app id>`: a packaged app's AUMID.
fn is_packaged_aumid(s: &str) -> bool {
    let Some((family, app)) = s.split_once('!') else {
        return false;
    };
    let Some((name, publisher)) = family.rsplit_once('_') else {
        return false;
    };
    !name.is_empty()
        && !app.is_empty()
        && !app.contains('!')
        && publisher.len() == 13
        && publisher
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// Packaged-app AUMIDs written in `blob` as UTF-16 text (either byte alignment), each with
/// the byte offset of its first occurrence, in offset order.
fn packaged_aumids(blob: &[u8]) -> Vec<(usize, String)> {
    let mut found: Vec<(usize, String)> = Vec::new();
    for align in 0..2 {
        let units: Vec<u16> = blob[align.min(blob.len())..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        let mut start = 0;
        while start < units.len() {
            let ok = |u: u16| char::from_u32(u32::from(u)).is_some_and(aumid_char);
            if !ok(units[start]) {
                start += 1;
                continue;
            }
            let mut end = start;
            while end < units.len() && ok(units[end]) {
                end += 1;
            }
            let run = String::from_utf16_lossy(&units[start..end]);
            if is_packaged_aumid(&run) {
                let off = align + 2 * start;
                match found.iter_mut().find(|(_, a)| *a == run) {
                    Some(e) if e.0 > off => e.0 = off,
                    Some(_) => {}
                    None => found.push((off, run)),
                }
            }
            start = end;
        }
    }
    found.sort();
    found
}

/// `{GUID}\rest` (a known folder, as NotifyIconSettings writes paths) -> the folder's path
/// joined with `rest`; other paths unchanged. `None` when the folder cannot be resolved.
fn expand_known_folder(path: &str, resolve: impl Fn(&str) -> Option<String>) -> Option<String> {
    if !path.starts_with('{') {
        return Some(path.to_string());
    }
    let close = path.find('}')?;
    let base = resolve(&path[..=close])?;
    let rest = path[close + 1..].trim_start_matches('\\');
    Some(if rest.is_empty() {
        base
    } else {
        format!("{}\\{rest}", base.trim_end_matches('\\'))
    })
}

/// `{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}` (any case) -> the GUID.
fn parse_guid(s: &str) -> Option<GUID> {
    let inner = s.strip_prefix('{')?.strip_suffix('}')?;
    let parts: Vec<&str> = inner.split('-').collect();
    let lens: Vec<usize> = parts.iter().map(|p| p.len()).collect();
    if lens != [8, 4, 4, 4, 12] || !inner.bytes().all(|b| b == b'-' || b.is_ascii_hexdigit()) {
        return None;
    }
    u128::from_str_radix(&parts.concat(), 16)
        .ok()
        .map(GUID::from_u128)
}

/// The `tray_identity_key` of one NotifyIconSettings entry: its icon GUID, else its
/// (expanded) executable and uid. `None` when it has neither.
fn promoted_key(icon_guid: Option<&str>, exe: Option<&str>, uid: Option<u32>) -> Option<String> {
    if let Some(g) = icon_guid.filter(|g| parse_guid(g).is_some()) {
        return Some(g.to_lowercase());
    }
    let exe = exe.filter(|e| !e.is_empty())?;
    Some(tray_key(None, exe, uid?))
}

// ---------------------------------------------------------------------------------------
// Registry (read-only)
// ---------------------------------------------------------------------------------------

fn wide(s: &str) -> HSTRING {
    HSTRING::from(s)
}

/// An open registry key, closed on drop.
struct Key(HKEY);

impl Key {
    fn open_current_user(path: &str) -> Option<Self> {
        let mut h = HKEY::default();
        let p = wide(path);
        // SAFETY: `p` outlives the call; `h` is an out-parameter. Read access only.
        let err = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, &p, Some(0), KEY_READ, &mut h) };
        (err == ERROR_SUCCESS).then_some(Self(h))
    }

    fn subkeys(&self) -> Vec<String> {
        let mut out = Vec::new();
        for index in 0u32.. {
            let mut buf = [0u16; 256];
            let mut len = buf.len() as u32;
            // SAFETY: `buf` and `len` describe a valid output buffer for the call.
            let err = unsafe {
                RegEnumKeyExW(
                    self.0,
                    index,
                    Some(PWSTR(buf.as_mut_ptr())),
                    &mut len,
                    None,
                    None,
                    None,
                    None,
                )
            };
            if err != ERROR_SUCCESS {
                break;
            }
            out.push(String::from_utf16_lossy(&buf[..len as usize]));
        }
        out
    }

    /// The raw bytes of value `name` of subkey `sub` (`""` = this key) of type `ty`.
    fn value(&self, sub: &str, name: &str, ty: REG_ROUTINE_FLAGS) -> Option<Vec<u8>> {
        let (sub, name) = (wide(sub), wide(name));
        let mut size = 0u32;
        // SAFETY: size query: no data pointer, `size` is an out-parameter.
        let err = unsafe { RegGetValueW(self.0, &sub, &name, ty, None, None, Some(&mut size)) };
        if err != ERROR_SUCCESS || size == 0 || size > 1 << 20 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        // SAFETY: `data` holds `size` bytes, as passed alongside.
        let err = unsafe {
            RegGetValueW(
                self.0,
                &sub,
                &name,
                ty,
                None,
                Some(data.as_mut_ptr().cast()),
                Some(&mut size),
            )
        };
        if err != ERROR_SUCCESS {
            return None;
        }
        data.truncate(size as usize);
        Some(data)
    }

    fn dword(&self, sub: &str, name: &str) -> Option<u32> {
        let b = self.value(sub, name, RRF_RT_REG_DWORD)?;
        Some(u32::from_le_bytes(b.get(..4)?.try_into().ok()?))
    }

    fn string(&self, sub: &str, name: &str) -> Option<String> {
        let b = self.value(sub, name, RRF_RT_REG_SZ)?;
        let units: Vec<u16> = b
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .take_while(|&u| u != 0)
            .collect();
        Some(String::from_utf16_lossy(&units))
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: closes the key this value opened.
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

fn known_folder_path(guid: &str) -> Option<String> {
    let id = parse_guid(guid)?;
    // SAFETY: `id` is a local GUID; the returned string is ours to free with CoTaskMemFree.
    unsafe {
        let p = SHGetKnownFolderPath(&id, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s
    }
}

/// Identity keys of the tray icons Windows shows outside its overflow
/// (`HKCU\Control Panel\NotifyIconSettings\*` with `IsPromoted` = 1), in the
/// `tray_identity_key` format. Entries without a usable identity are skipped.
pub fn import_promoted_tray() -> Vec<String> {
    let Some(root) = Key::open_current_user(NOTIFY_ICON_SETTINGS) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for sub in root.subkeys() {
        if root.dword(&sub, "IsPromoted") != Some(1) {
            continue;
        }
        let guid = root.string(&sub, "IconGuid");
        let exe = root
            .string(&sub, "ExecutablePath")
            .and_then(|p| expand_known_folder(&p, known_folder_path));
        let uid = root.dword(&sub, "UID");
        if let Some(k) = promoted_key(guid.as_deref(), exe.as_deref(), uid) {
            if !out.contains(&k) {
                out.push(k);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------
// Shortcuts (read-only)
// ---------------------------------------------------------------------------------------

/// What a shortcut points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkInfo {
    /// Target file (empty for shortcuts to shell items without a path).
    pub target: String,
    /// `System.AppUserModel.ID` stored in the shortcut.
    pub aumid: Option<String>,
}

/// Reads a `.lnk` without resolving or saving it. Needs COM on the calling thread.
pub fn read_link(path: &Path) -> Option<LinkInfo> {
    let file = HSTRING::from(path.as_os_str());
    // SAFETY: plain in-process COM calls; `file` and `buf` outlive the calls that use them.
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        link.cast::<IPersistFile>()
            .ok()?
            .Load(&file, STGM_READ)
            .ok()?;
        let mut buf = [0u16; 1024];
        let target = match link.GetPath(&mut buf, std::ptr::null_mut(), 0) {
            Ok(()) => {
                let n = buf.iter().position(|&u| u == 0).unwrap_or(buf.len());
                String::from_utf16_lossy(&buf[..n])
            }
            Err(_) => String::new(),
        };
        let aumid = link
            .cast::<IPropertyStore>()
            .ok()
            .and_then(|store| store.GetValue(&PKEY_AppUserModel_ID).ok())
            .and_then(|pv| BSTR::try_from(&pv).ok())
            .map(|b| b.to_string())
            .filter(|s| !s.is_empty());
        Some(LinkInfo { target, aumid })
    }
}

/// Display name of an installed packaged app, `None` if it is not installed.
pub(crate) fn packaged_app_name(aumid: &str) -> Option<String> {
    let name = HSTRING::from(format!(r"shell:AppsFolder\{aumid}"));
    // SAFETY: `name` outlives the call; the display name is ours to free.
    unsafe {
        let item: IShellItem =
            SHCreateItemFromParsingName(PCWSTR(name.as_ptr()), None::<&IBindCtx>).ok()?;
        let p = item.GetDisplayName(SIGDN_NORMALDISPLAY).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s.filter(|s| !s.is_empty())
    }
}

fn is_lnk(p: &Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case("lnk"))
}

/// `.lnk` files directly in `dir`, with their creation times.
fn links_in(dir: &Path) -> Vec<(PathBuf, SystemTime)> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    rd.flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()) && is_lnk(&e.path()))
        .map(|e| {
            let created = e
                .metadata()
                .and_then(|m| m.created())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            (e.path(), created)
        })
        .collect()
}

/// `.lnk` files under `dir` (depth-limited, sorted for a stable result).
pub(crate) fn links_under(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let Ok(t) = e.file_type() else { continue };
        let p = e.path();
        if t.is_dir() && depth > 0 {
            links_under(&p, depth - 1, out);
        } else if t.is_file() && is_lnk(&p) {
            out.push(p);
        }
    }
}

fn env_path(var: &str) -> Option<PathBuf> {
    std::env::var_os(var).map(PathBuf::from)
}

fn pinned_folder() -> Option<PathBuf> {
    Some(env_path("APPDATA")?.join(r"Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar"))
}

pub(crate) fn start_menu_folders() -> Vec<PathBuf> {
    [
        ("APPDATA", r"Microsoft\Windows\Start Menu\Programs"),
        ("ProgramData", r"Microsoft\Windows\Start Menu\Programs"),
    ]
    .iter()
    .filter_map(|(var, rel)| Some(env_path(var)?.join(rel)))
    .collect()
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn read_favorites_blob() -> Option<Vec<u8>> {
    Key::open_current_user(TASKBAND)?.value("", "Favorites", RRF_RT_REG_BINARY)
}

/// One thing the stock taskbar has pinned.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Source {
    Link(PathBuf),
    Packaged(String),
}

pub(crate) fn pin_from_link(path: &Path) -> Option<Pin> {
    let info = read_link(path)?;
    let name = path.file_stem()?.to_string_lossy().into_owned();
    let target = (!info.target.is_empty()).then_some(info.target);
    if target.is_none() && info.aumid.is_none() {
        return None;
    }
    Some(Pin {
        aumid: info.aumid,
        path: target,
        name,
        link: Some(path.to_string_lossy().into_owned()),
    })
}

fn pin_from(source: &Source) -> Option<Pin> {
    match source {
        Source::Link(p) => pin_from_link(p),
        Source::Packaged(aumid) => Some(Pin {
            name: packaged_app_name(aumid)?,
            aumid: Some(aumid.clone()),
            path: None,
            link: None,
        }),
    }
}

/// What the blob (or, without one, the pinned folder) says is pinned, in taskbar order.
fn pinned_sources() -> Vec<Source> {
    let mut folder = pinned_folder().map(|d| links_in(&d)).unwrap_or_default();
    folder.sort_by_key(|(p, created)| (*created, file_name(p)));
    let mut links: Vec<PathBuf> = folder.into_iter().map(|(p, _)| p).collect();
    let in_folder = links.len();
    let blob = read_favorites_blob().unwrap_or_default();
    if !blob.is_empty() {
        // Shortcuts pinned from the Start menu stay where they are; the blob names them.
        let mut seen: HashSet<String> = links.iter().map(|p| file_name(p).to_lowercase()).collect();
        let mut others = Vec::new();
        for dir in start_menu_folders() {
            links_under(&dir, START_MENU_DEPTH, &mut others);
        }
        links.extend(
            others
                .into_iter()
                .filter(|p| seen.insert(file_name(p).to_lowercase())),
        );
    }
    let names: Vec<String> = links.iter().map(|p| file_name(p)).collect();
    let mut items: Vec<(Option<usize>, Source)> = Vec::new();
    for (k, (off, p)) in offsets_in_blob(&names, &blob)
        .into_iter()
        .zip(links)
        .enumerate()
    {
        // Start-menu shortcuts only count when the blob names them.
        if k < in_folder || off.is_some() {
            items.push((off, Source::Link(p)));
        }
    }
    for (off, aumid) in packaged_aumids(&blob) {
        items.push((Some(off), Source::Packaged(aumid)));
    }
    by_offset(items)
}

/// The stock taskbar's pinned apps, in its order. Anything that cannot be read is skipped;
/// duplicates (same AUMID or target) are dropped. Needs COM on the calling thread.
pub fn import_windows_pins() -> Vec<Pin> {
    let mut out: Vec<Pin> = Vec::new();
    for source in pinned_sources() {
        let Some(pin) = pin_from(&source) else {
            continue;
        };
        let same = |o: &Pin| match (&o.aumid, &pin.aumid) {
            (Some(a), Some(b)) => a == b,
            (None, None) => {
                matches!((&o.path, &pin.path), (Some(a), Some(b)) if a.eq_ignore_ascii_case(b))
            }
            _ => false,
        };
        if !out.iter().any(same) {
            out.push(pin);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn pins_import_orders_by_first_occurrence_in_blob() {
        let mut blob = vec![0u8, 1, 2];
        blob.extend(utf16("a.lnk"));
        blob.extend([0, 0, 9, 9]);
        blob.extend(utf16("c.lnk"));
        blob.extend([0, 0]);
        assert_eq!(
            order_by_blob(&names(&["b.lnk", "a.lnk", "c.lnk"]), &blob),
            names(&["a.lnk", "c.lnk", "b.lnk"])
        );
    }

    #[test]
    fn pins_import_empty_blob_keeps_input_order() {
        let n = names(&["b.lnk", "a.lnk", "c.lnk"]);
        assert_eq!(order_by_blob(&n, &[]), n);
    }

    #[test]
    fn pins_import_matches_any_alignment_and_case_but_not_name_tails() {
        // Odd byte offset, different case, right after an arbitrary timestamp byte.
        let mut blob = vec![7u8, 0x5D, 0];
        blob.extend(utf16("SPOTIFY.LNK"));
        assert_eq!(find_utf16(&blob, "Spotify.lnk"), Some(3));
        // "Code.lnk" found only as the tail of another candidate does not count...
        let both = names(&["Code.lnk", "Visual Studio Code.lnk"]);
        let mut blob = utf16("\u{1}Visual Studio Code.lnk\0");
        assert_eq!(offsets_in_blob(&both, &blob), [None, Some(2)]);
        assert_eq!(
            order_by_blob(&both, &blob),
            names(&["Visual Studio Code.lnk", "Code.lnk"])
        );
        // ...but does once it also occurs on its own.
        blob.extend(utf16("Code.lnk"));
        assert_eq!(offsets_in_blob(&both, &blob), [Some(48), Some(2)]);
    }

    #[test]
    fn pins_import_finds_packaged_aumids() {
        let mut blob = vec![0u8; 3];
        blob.extend(utf16("Microsoft.WindowsTerminal_8wekyb3d8bbwe!App"));
        blob.extend([0, 0]);
        blob.extend(utf16("not_an!aumid C:\\x.exe"));
        blob.extend([0]);
        blob.extend(utf16("MSTeams_8wekyb3d8bbwe!MSTeams"));
        blob.extend(utf16("\0Microsoft.WindowsTerminal_8wekyb3d8bbwe!App"));
        let found = packaged_aumids(&blob);
        let aumids: Vec<&str> = found.iter().map(|(_, a)| a.as_str()).collect();
        assert_eq!(
            aumids,
            [
                "Microsoft.WindowsTerminal_8wekyb3d8bbwe!App",
                "MSTeams_8wekyb3d8bbwe!MSTeams"
            ]
        );
        assert_eq!(found[0].0, 3, "first occurrence");
        assert!(!is_packaged_aumid("Chrome"));
        assert!(!is_packaged_aumid("a_8wekyb3d8bbwe!"));
        assert!(!is_packaged_aumid("a_8WEKYB3D8BBWE!App"));
    }

    #[test]
    fn pins_import_merges_by_offset_with_unfound_last() {
        let v = by_offset(vec![
            (None, "x"),
            (Some(9), "b"),
            (Some(2), "a"),
            (None, "y"),
        ]);
        assert_eq!(v, ["a", "b", "x", "y"]);
    }

    #[test]
    fn promoted_keys_follow_tray_identity_format() {
        let resolve = |g: &str| {
            (g == "{6D809377-6AF0-444B-8957-A3773F02200E}").then(|| r"C:\Program Files".to_string())
        };
        assert_eq!(
            expand_known_folder(
                r"{6D809377-6AF0-444B-8957-A3773F02200E}\RustDesk\rustdesk.exe",
                resolve
            )
            .as_deref(),
            Some(r"C:\Program Files\RustDesk\rustdesk.exe")
        );
        assert_eq!(
            expand_known_folder(r"C:\a\b.exe", resolve).as_deref(),
            Some(r"C:\a\b.exe")
        );
        assert_eq!(
            expand_known_folder(r"{00000000-0000-0000-0000-000000000000}\x", resolve),
            None
        );
        // GUID icons: the GUID, lowercase, as tray_identity_key gives it.
        assert_eq!(
            promoted_key(
                Some("{CAD4462E-5B6C-4F95-8A98-4CD14081AE05}"),
                Some(r"C:\x.exe"),
                None
            )
            .as_deref(),
            Some("{cad4462e-5b6c-4f95-8a98-4cd14081ae05}")
        );
        assert_eq!(
            promoted_key(
                None,
                Some(r"C:\Program Files\AnyDesk\AnyDesk.exe"),
                Some(1001)
            )
            .as_deref(),
            Some(r"c:\program files\anydesk\anydesk.exe|1001")
        );
        assert_eq!(promoted_key(None, Some(r"C:\x.exe"), None), None, "no uid");
        assert_eq!(promoted_key(None, None, Some(1)), None);
        assert_eq!(promoted_key(Some("garbage"), None, Some(1)), None);
    }

    #[test]
    fn parse_guid_matches_the_tray_model_format() {
        let g = parse_guid("{6D809377-6AF0-444B-8957-A3773F02200E}").unwrap();
        assert_eq!(g, GUID::from_u128(0x6D809377_6AF0_444B_8957_A3773F02200E));
        assert!(parse_guid("6D809377-6AF0-444B-8957-A3773F02200E").is_none());
        assert!(parse_guid("{6D809377-6AF0-444B-8957-A3773F02200}").is_none());
    }
}
