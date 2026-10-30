//! Start panel search: scoring, ranking and the Settings-pages table.

use crate::calc::{eval, looks_like_math};
use crate::i18n::{tr, Key, Lang};
use crate::settings::Pin;

pub struct FileHit {
    pub name: String,
    pub path: String,
}

pub struct SettingsPage {
    pub key: Key,
    pub uri: &'static str,
    pub keywords: &'static [&'static str],
}

macro_rules! page {
    ($key:ident, $uri:expr, [$($kw:expr),*]) => {
        SettingsPage { key: Key::$key, uri: $uri, keywords: &[$($kw),*] }
    };
}

pub const SETTINGS_PAGES: &[SettingsPage] = &[
    page!(
        Display,
        "ms-settings:display",
        ["screen", "resolution", "scale", "monitor"]
    ),
    page!(
        Sound,
        "ms-settings:sound",
        ["audio", "volume", "speaker", "microphone"]
    ),
    page!(Bluetooth, "ms-settings:bluetooth", ["devices", "pair"]),
    page!(
        WiFi,
        "ms-settings:network-wifi",
        ["wireless", "network", "internet"]
    ),
    page!(
        Personalization,
        "ms-settings:personalization",
        ["theme", "wallpaper", "background", "colors"]
    ),
    page!(
        AppsSettings,
        "ms-settings:appsfeatures",
        ["uninstall", "programs", "installed"]
    ),
    page!(
        WindowsUpdate,
        "ms-settings:windowsupdate",
        ["update", "upgrade"]
    ),
    page!(
        Power,
        "ms-settings:powersleep",
        ["battery", "sleep", "energy"]
    ),
    page!(
        Mouse,
        "ms-settings:mousetouchpad",
        ["touchpad", "cursor", "pointer"]
    ),
    page!(Keyboard, "ms-settings:typing", ["typing", "input"]),
    page!(
        Notifications,
        "ms-settings:notifications",
        ["alerts", "focus"]
    ),
    page!(
        DefaultApps,
        "ms-settings:defaultapps",
        ["browser", "open with"]
    ),
    page!(
        Storage,
        "ms-settings:storagesense",
        ["disk", "space", "cleanup"]
    ),
    page!(
        Privacy,
        "ms-settings:privacy",
        ["permissions", "camera", "location"]
    ),
    page!(DateTime, "ms-settings:dateandtime", ["clock", "time zone"]),
    // The "Language" page: its title key is Region ("Language & region").
    page!(Region, "ms-settings:regionlanguage", ["region", "locale"]),
    page!(
        Accounts,
        "ms-settings:yourinfo",
        ["user", "profile", "sign-in"]
    ),
    page!(Printers, "ms-settings:printers", ["scanner", "print"]),
    page!(NightLight, "ms-settings:nightlight", ["blue light", "warm"]),
];

#[derive(Debug, Clone, PartialEq)]
pub enum ResultKind {
    Math(f64),
    /// Index into the `apps` slice.
    App(usize),
    /// Index into `SETTINGS_PAGES`.
    Setting(usize),
    /// Index into the `files` slice.
    File(usize),
}

pub struct SearchResults {
    /// Display order; `items[0]` is what Enter runs.
    pub items: Vec<ResultKind>,
}

const MAX_APPS: usize = 6;
const MAX_SETTINGS: usize = 3;
const MAX_FILES: usize = 6;

fn fold_char(c: char) -> Option<char> {
    Some(match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
        'è' | 'é' | 'ê' | 'ë' => 'e',
        'ì' | 'í' | 'î' | 'ï' => 'i',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' => 'o',
        'ù' | 'ú' | 'û' | 'ü' => 'u',
        'ç' => 'c',
        'ñ' => 'n',
        // Combining diacritical marks (decomposed accents).
        '\u{300}'..='\u{36F}' => return None,
        c => c,
    })
}

/// Lowercases and folds accents (à->a, é->e, ç->c, ...), dropping combining marks.
pub fn fold(s: &str) -> String {
    s.chars()
        .flat_map(|c| c.to_lowercase())
        .filter_map(fold_char)
        .collect()
}

fn is_sep(c: char) -> bool {
    matches!(c, ' ' | '-' | '_' | '.')
}

/// Higher is better; `None` means no match.
pub fn score(query: &str, candidate: &str) -> Option<u32> {
    score_folded(&fold(query), &fold(candidate))
}

fn score_folded(q: &str, c: &str) -> Option<u32> {
    if q.is_empty() {
        return None;
    }
    if q == c {
        return Some(1000);
    }
    if c.starts_with(q) {
        return Some(800);
    }
    let words: Vec<&str> = c.split(is_sep).filter(|w| !w.is_empty()).collect();
    if let Some(i) = words.iter().position(|w| w.starts_with(q)) {
        return Some(600u32.saturating_sub(i as u32));
    }
    let initials: String = words.iter().filter_map(|w| w.chars().next()).collect();
    if initials.starts_with(q) {
        return Some(500);
    }
    if c.contains(q) {
        return Some(300);
    }
    // In-order subsequence: penalise the characters skipped between first and last match.
    let qc: Vec<char> = q.chars().collect();
    let mut next = 0;
    let (mut first, mut last) = (0, 0);
    for (i, ch) in c.chars().enumerate() {
        if next < qc.len() && ch == qc[next] {
            if next == 0 {
                first = i;
            }
            last = i;
            next += 1;
        }
    }
    if next == qc.len() {
        let gaps = (last - first + 1 - qc.len()) as u32;
        return Some(100u32.saturating_sub(gaps).max(1));
    }
    None
}

/// Scores each candidate and returns up to `cap` indices, best first. A candidate may offer
/// several strings (a page title plus keywords); its score is the best of them, and ties go
/// to the shorter then alphabetically-first primary name.
fn rank<'a>(q: &str, cands: impl Iterator<Item = (usize, Vec<&'a str>)>, cap: usize) -> Vec<usize> {
    let mut scored: Vec<(u32, usize, String, usize)> = cands
        .filter_map(|(idx, names)| {
            let best = names
                .iter()
                .filter_map(|n| score_folded(q, &fold(n)))
                .max()?;
            let primary = fold(names[0]);
            Some((best, primary.chars().count(), primary, idx))
        })
        .collect();
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
            .then(a.3.cmp(&b.3))
    });
    scored.into_iter().take(cap).map(|s| s.3).collect()
}

pub fn search(query: &str, apps: &[Pin], files: &[FileHit], lang: Lang) -> SearchResults {
    let trimmed = query.trim();
    let mut items = Vec::new();
    if trimmed.is_empty() {
        return SearchResults { items };
    }
    if looks_like_math(trimmed) {
        if let Some(v) = eval(trimmed) {
            items.push(ResultKind::Math(v));
        }
    }
    let q = fold(trimmed);
    items.extend(
        rank(
            &q,
            apps.iter()
                .enumerate()
                .map(|(i, p)| (i, vec![p.name.as_str()])),
            MAX_APPS,
        )
        .into_iter()
        .map(ResultKind::App),
    );
    items.extend(
        rank(
            &q,
            SETTINGS_PAGES.iter().enumerate().map(|(i, p)| {
                let mut names = vec![tr(lang, p.key)];
                names.extend(p.keywords.iter().copied());
                (i, names)
            }),
            MAX_SETTINGS,
        )
        .into_iter()
        .map(ResultKind::Setting),
    );
    items.extend(
        rank(
            &q,
            files
                .iter()
                .enumerate()
                .map(|(i, f)| (i, vec![f.name.as_str()])),
            MAX_FILES,
        )
        .into_iter()
        .map(ResultKind::File),
    );
    SearchResults { items }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Lang;
    use crate::settings::Pin;
    fn pin(name: &str) -> Pin {
        Pin {
            name: name.to_string(),
            ..Default::default()
        }
    }
    #[test]
    fn prefix_beats_substring() {
        assert!(score("chr", "Google Chrome").unwrap() < score("chr", "Chrome").unwrap());
    }
    #[test]
    fn acronym() {
        assert!(score("vsc", "Visual Studio Code").is_some());
    }
    #[test]
    fn accents_folded() {
        assert!(score("impostazioni", "Impostazióni").is_some());
        assert!(score("e", "è").is_some());
    }
    #[test]
    fn math_first() {
        let r = search("2+2", &[pin("2+2 Game")], &[], Lang::En);
        assert_eq!(r.items[0], ResultKind::Math(4.0));
    }
    #[test]
    fn caps_per_kind() {
        let apps: Vec<Pin> = (0..20).map(|i| pin(&format!("App {i}"))).collect();
        let r = search("app", &apps, &[], Lang::En);
        assert_eq!(
            r.items
                .iter()
                .filter(|k| matches!(k, ResultKind::App(_)))
                .count(),
            6
        );
    }
    #[test]
    fn settings_by_italian_title_and_english_keyword() {
        let it = search("suono", &[], &[], Lang::It);
        let en = search("volume", &[], &[], Lang::It);
        assert!(matches!(it.items[0], ResultKind::Setting(_)));
        assert!(matches!(en.items[0], ResultKind::Setting(_)));
    }
    #[test]
    fn blank_query_is_empty() {
        assert!(search("   ", &[pin("a")], &[], Lang::En).items.is_empty());
    }
    #[test]
    fn fold_lowercases_and_strips_marks() {
        assert_eq!(fold("PERCHÉ çàòùì"), "perche caoui");
        assert_eq!(fold("e\u{301}"), "e");
    }
    #[test]
    fn settings_table_is_complete() {
        assert_eq!(SETTINGS_PAGES.len(), 19);
        assert!(SETTINGS_PAGES
            .iter()
            .all(|p| p.uri.starts_with("ms-settings:")));
    }
    #[test]
    fn long_query_no_panic() {
        let q = "a".repeat(256);
        let _ = search(&q, &[pin("a")], &[], Lang::En);
    }
}
