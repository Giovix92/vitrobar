//! Bar widgets (spec §5.7): what the pills say and how wide they are, and the music detail
//! panel's rows. Pure; the app feeds it service snapshots.

use crate::i18n::{tr, Key, Lang};
use serde::{Deserialize, Serialize};
use unicode_segmentation::UnicodeSegmentation;

use crate::panel_kit::{Button, Part, Row};
use crate::style::Rgba;

/// The pill's look, shared with the bar visual that draws it (DIPs).
pub const PILL_FONT_DIP: f32 = 13.0;
pub const PILL_PAD_DIP: f32 = 10.0;
pub const PILL_GAP_DIP: f32 = 6.0;
/// The glyph's box: a little wider than the font size.
pub const PILL_GLYPH_W_DIP: f32 = PILL_FONT_DIP * 1.2;
/// The music pill's artwork, in the glyph's place (the glyph then shows over it).
pub const PILL_ART_DIP: f32 = 20.0;

/// Width of the pill's leading box: the artwork or the glyph.
pub fn pill_lead_w(art: bool) -> f32 {
    if art {
        PILL_ART_DIP
    } else {
        PILL_GLYPH_W_DIP
    }
}

/// The bar's battery glyph (Segoe Fluent Icons, the horizontal battery the stock taskbar
/// shows): eleven fill levels, with a plug while charging or a leaf with energy saver on.
pub fn battery_glyph(percent: u8, charging: bool, saver: bool) -> char {
    const PLAIN: [u32; 11] = [
        0xE850, 0xE851, 0xE852, 0xE853, 0xE854, 0xE855, 0xE856, 0xE857, 0xE858, 0xE859, 0xE83F,
    ];
    const CHARGING: [u32; 11] = [
        0xE85A, 0xE85B, 0xE85C, 0xE85D, 0xE85E, 0xE85F, 0xE860, 0xE861, 0xE862, 0xE83E, 0xEA93,
    ];
    const SAVER: [u32; 11] = [
        0xE863, 0xE864, 0xE865, 0xE866, 0xE867, 0xE868, 0xE869, 0xE86A, 0xE86B, 0xEA94, 0xEA95,
    ];
    let level = (usize::from(percent.min(100)) + 5) / 10;
    let set = if charging {
        &CHARGING
    } else if saver {
        &SAVER
    } else {
        &PLAIN
    };
    char::from_u32(set[level]).unwrap_or('\u{E83F}')
}

/// Claude's orange, for the Claude pill and panel when widgets take their app's colour.
pub const CLAUDE_TINT: Rgba = Rgba(0xD9, 0x77, 0x57, 255);

/// The colour of a music player, from the name Windows gives its media session (an AUMID
/// or an executable); `None` for players without a colour of their own (browsers…).
pub fn player_tint(app: &str) -> Option<Rgba> {
    let a = app.to_ascii_lowercase();
    let has = |k: &str| a.contains(k);
    Some(if has("spotify") {
        Rgba(0x1D, 0xB9, 0x54, 255)
    } else if has("applemusic") || has("itunes") {
        Rgba(0xFA, 0x24, 0x3C, 255)
    } else if has("zunemusic") || has("mediaplayer") || has("wmplayer") {
        Rgba(0x00, 0x78, 0xD4, 255)
    } else if has("deezer") {
        Rgba(0xA2, 0x38, 0xFF, 255)
    } else if has("soundcloud") {
        Rgba(0xFF, 0x55, 0x00, 255)
    } else if has("tidal") {
        Rgba(0x00, 0xCC, 0xD3, 255)
    } else if has("amazonmusic") || has("amazon music") {
        Rgba(0x25, 0xD1, 0xDA, 255)
    } else if has("youtubemusic") || has("youtube music") {
        Rgba(0xFF, 0x00, 0x33, 255)
    } else if has("vlc") {
        Rgba(0xFF, 0x88, 0x00, 255)
    } else if has("foobar") || has("musicbee") || has("aimp") {
        Rgba(0x3A, 0x8E, 0xE6, 255)
    } else {
        return None;
    })
}

/// The music pill never grows past this; its text is cut with an ellipsis.
pub const MUSIC_PILL_MAX_DIP: f32 = 180.0;
/// Characters (grapheme clusters) the music pill's text is cut to before measuring.
pub const MUSIC_PILL_MAX_CHARS: usize = 32;

/// What the music widget knows about the current media session (the app converts the
/// Windows snapshot into this).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MusicView {
    /// The player, as Windows names its session (an AUMID or an exe name).
    pub app: String,
    pub title: String,
    pub artist: String,
    pub playing: bool,
    pub can_prev: bool,
    pub can_next: bool,
    pub can_play_pause: bool,
}

/// "Title — Artist" cut to `max_chars` grapheme clusters (the last one an ellipsis); title
/// only when there is no artist; empty (the widget hides) with no session or no title.
pub fn music_pill_text(s: Option<&MusicView>, max_chars: usize) -> String {
    let Some(v) = s else {
        return String::new();
    };
    let (title, artist) = (v.title.trim(), v.artist.trim());
    if title.is_empty() {
        return String::new();
    }
    let full = if artist.is_empty() {
        title.to_string()
    } else {
        format!("{title} — {artist}")
    };
    ellipsize(&full, max_chars)
}

/// `s` cut to `max` grapheme clusters, the last of them an ellipsis.
fn ellipsize(s: &str, max: usize) -> String {
    let starts = cluster_starts(s);
    if starts.len() <= max {
        return s.to_string();
    }
    let cut = starts[max.saturating_sub(1)];
    format!("{}…", s[..cut].trim_end())
}

/// Byte offsets where (extended) grapheme clusters start.
fn cluster_starts(s: &str) -> Vec<usize> {
    s.grapheme_indices(true).map(|(i, _)| i).collect()
}

/// The pill's width for text `text` measuring `text_w_dip`, capped at `max_dip`; 0 for empty
/// text, which makes the layout hide the slot.
pub fn pill_width(text: &str, text_w_dip: f32, max_dip: f32, art: bool) -> f32 {
    if text.is_empty() {
        return 0.0;
    }
    (2.0 * PILL_PAD_DIP + pill_lead_w(art) + PILL_GAP_DIP + text_w_dip.max(0.0)).min(max_dip)
}

/// Whether `x_dip` (from the pill's left edge) falls on the pill's glyph button (or the
/// artwork in its place) rather than on its text.
pub fn on_pill_glyph(x_dip: f32, art: bool) -> bool {
    x_dip < PILL_PAD_DIP + pill_lead_w(art) + PILL_GAP_DIP / 2.0
}

/// Segoe Fluent Icons: "Play", "Pause", "Previous", "Next".
pub const G_PLAY: char = '\u{E768}';
pub const G_PAUSE: char = '\u{E769}';
pub const G_PREV: char = '\u{E892}';
pub const G_NEXT: char = '\u{E893}';

/// The pill's glyph: what a click on it does (pause while playing, play otherwise).
pub fn music_glyph(playing: bool) -> char {
    if playing {
        G_PAUSE
    } else {
        G_PLAY
    }
}

/// A readable player name from the session's id: `Spotify.exe` → "Spotify",
/// `Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic` → "ZuneMusic".
pub fn app_label(id: &str) -> String {
    let id = id.trim();
    // A packaged app, "Family_publisher!AppId": the app id's last dotted part reads best.
    let id = match id.rsplit_once('!') {
        Some((_, app)) => app.rsplit('.').next().unwrap_or(app),
        None => id,
    };
    match id.len().checked_sub(4) {
        Some(n) if id.is_char_boundary(n) && id[n..].eq_ignore_ascii_case(".exe") => {
            id[..n].to_string()
        }
        _ => id.to_string(),
    }
}

// ---- the detail panel ---------------------------------------------------------------------

pub const ID_ART: u32 = 1;
pub const ID_PREV: u32 = 2;
pub const ID_PLAY: u32 = 3;
pub const ID_NEXT: u32 = 4;

/// The artwork square and the hole that holds it with the title and artist beside it.
pub const ART_DIP: f32 = 64.0;
pub const ART_ROW_H_DIP: f32 = ART_DIP + 8.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MusicAction {
    Prev,
    PlayPause,
    Next,
}

pub fn music_action(p: Part) -> Option<MusicAction> {
    match p {
        Part::Button(ID_PREV) => Some(MusicAction::Prev),
        Part::Button(ID_PLAY) => Some(MusicAction::PlayPause),
        Part::Button(ID_NEXT) => Some(MusicAction::Next),
        _ => None,
    }
}

/// Artwork with title and artist (drawn by the app), the transport buttons the session
/// allows, and the player's name.
pub fn music_rows(v: &MusicView, _lang: Lang) -> Vec<Row> {
    let button = |id, glyph| Button {
        id,
        glyph: Some(glyph),
        text: String::new(),
    };
    let mut buttons = Vec::new();
    if v.can_prev {
        buttons.push(button(ID_PREV, G_PREV));
    }
    if v.can_play_pause {
        buttons.push(button(ID_PLAY, music_glyph(v.playing)));
    }
    if v.can_next {
        buttons.push(button(ID_NEXT, G_NEXT));
    }
    let mut rows = vec![Row::Custom {
        id: ID_ART,
        h: ART_ROW_H_DIP,
    }];
    if !buttons.is_empty() {
        rows.push(Row::Buttons(buttons));
    }
    let app = app_label(&v.app);
    if !app.is_empty() {
        rows.push(Row::Text(app));
    }
    rows
}

// ---- Claude limits (ruling P1: from Claude Code's own status line) --------------------------

/// One rate-limit window as Claude Code reports it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Window {
    /// Percent used, 0..=100.
    pub used: f32,
    /// Epoch seconds when the window starts over.
    pub resets_at: Option<i64>,
}

/// What the status-line bridge saved: the 5-hour and the weekly window, and when.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ClaudeUsage {
    pub five_hour: Option<Window>,
    pub seven_day: Option<Window>,
    /// Epoch seconds when it was saved.
    pub at: i64,
}

/// Saved data older than this shows its age on the pill ("· 3 h ago"): the limits are shared
/// with claude.ai and the Claude apps, and only a running Claude Code brings new numbers.
pub const STALE_AFTER_S: i64 = 3600;

/// "3 h ago" / "2 d ago" (whole hours under a day, whole days after).
pub fn age_text(secs: i64, lang: Lang) -> String {
    let h = secs.max(0) / 3600;
    let (n, unit) = if h < 24 {
        (h.max(1), "h")
    } else {
        (
            h / 24,
            match lang {
                Lang::En => "d",
                Lang::It => "g",
            },
        )
    };
    match lang {
        Lang::En => format!("{n} {unit} ago"),
        Lang::It => format!("{n} {unit} fa"),
    }
}
/// The Claude pill's cap.
pub const CLAUDE_PILL_MAX_DIP: f32 = 200.0;
/// Segoe Fluent Icons "AreaChart".
pub const G_CLAUDE: char = '\u{E9D2}';

/// Reads `rate_limits.five_hour` / `.seven_day` (`used_percentage`, `resets_at` as epoch
/// seconds, epoch milliseconds or an RFC 3339 string) from the JSON Claude Code pipes to its
/// status-line command. `None` when the input is not JSON, has no `rate_limits`, or neither
/// window. Other fields are ignored.
pub fn parse_statusline(json: &str, now: i64) -> Option<ClaudeUsage> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let limits = v.get("rate_limits")?;
    let five_hour = limits.get("five_hour").and_then(window_of);
    let seven_day = limits.get("seven_day").and_then(window_of);
    if five_hour.is_none() && seven_day.is_none() {
        return None;
    }
    Some(ClaudeUsage {
        five_hour,
        seven_day,
        at: now,
    })
}

fn window_of(w: &serde_json::Value) -> Option<Window> {
    let used = w.get("used_percentage")?.as_f64()?;
    if !used.is_finite() {
        return None;
    }
    let resets_at = match w.get("resets_at") {
        Some(serde_json::Value::Number(n)) => n.as_f64().filter(|f| f.is_finite()).map(|f| {
            // Epoch milliseconds are three orders of magnitude past any epoch seconds.
            let secs = if f.abs() > 1e12 { f / 1000.0 } else { f };
            secs.round() as i64
        }),
        Some(serde_json::Value::String(t)) => parse_rfc3339(t),
        _ => None,
    };
    Some(Window {
        used: used.clamp(0.0, 100.0) as f32,
        resets_at,
    })
}

/// "2026-10-31T18:00:00Z", "2026-10-31T19:30:00.250+01:30" → epoch seconds (fraction
/// dropped).
fn parse_rfc3339(t: &str) -> Option<i64> {
    let t = t.trim();
    let b = t.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') {
        return None;
    }
    if b[13] != b':' || b[16] != b':' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let s = t.get(r)?;
        s.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| s.parse().ok())?
    };
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let mut rest = &t[19..];
    if let Some(frac) = rest.strip_prefix('.') {
        let n = frac.bytes().take_while(u8::is_ascii_digit).count();
        if n == 0 {
            return None;
        }
        rest = &frac[n..];
    }
    let offset = match rest {
        "Z" | "z" => 0,
        _ => {
            let sign = match rest.as_bytes().first()? {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let o = rest.get(1..)?;
            if o.len() != 5 || o.as_bytes()[2] != b':' {
                return None;
            }
            let oh: i64 = o.get(0..2)?.parse().ok()?;
            let om: i64 = o.get(3..5)?.parse().ok()?;
            sign * (oh * 3600 + om * 60)
        }
    };
    Some(days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + sec - offset)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (H. Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// "5h 42% · 7d 18%"; one window → only that one; a window whose reset time has passed shows
/// 0 %; data older than [`STALE_AFTER_S`] gets its age (" · 3 h ago"); no data → "Claude: no
/// data".
pub fn claude_pill_text(u: Option<&ClaudeUsage>, now: i64, lang: Lang) -> String {
    let no_data = || tr(lang, Key::ClaudeNoData).to_string();
    let Some(u) = u else {
        return no_data();
    };
    let mut parts: Vec<String> = [("5h", u.five_hour), ("7d", u.seven_day)]
        .into_iter()
        .filter_map(|(label, w)| Some(format!("{label} {:.0}%", shown_used(&w?, now))))
        .collect();
    if parts.is_empty() {
        return no_data();
    }
    if now - u.at > STALE_AFTER_S {
        parts.push(age_text(now - u.at, lang));
    }
    parts.join(" · ")
}

/// Percent used as it stands now: a window whose reset time has passed starts over at 0.
fn shown_used(w: &Window, now: i64) -> f32 {
    match w.resets_at {
        Some(t) if t <= now => 0.0,
        _ => w.used.round(),
    }
}

/// The next moment the pill's text changes by itself: a window resets (it then reads 0 %),
/// the data turns stale, or its age ticks over (hourly, daily after a day). `None` when
/// nothing is ahead of `now`.
pub fn next_pill_change(u: &ClaudeUsage, now: i64) -> Option<i64> {
    // The first second at which `claude_pill_text` reads differently.
    let age = now - u.at;
    let stale = if age <= STALE_AFTER_S {
        u.at.saturating_add(STALE_AFTER_S + 1)
    } else {
        let step = if age < 86_400 { 3600 } else { 86_400 };
        u.at.saturating_add((age / step + 1) * step)
    };
    [u.five_hour, u.seven_day]
        .into_iter()
        .flatten()
        .filter_map(|w| w.resets_at)
        .chain(Some(stale))
        .filter(|&t| t > now)
        .min()
}

/// "resets in 2 h 10 min" (whole minutes, rounded up; days from 24 h on); "resets now" once
/// the time has come.
pub fn resets_in(resets_at: i64, now: i64, lang: Lang) -> String {
    let left = resets_at - now;
    if left <= 0 {
        return tr(lang, Key::ResetsNow).to_string();
    }
    let minutes = (left + 59) / 60;
    let (d, h, m) = (minutes / 1440, minutes / 60 % 24, minutes % 60);
    let day = match lang {
        Lang::En => "d",
        Lang::It => "g",
    };
    let span = if d > 0 {
        if h > 0 {
            format!("{d} {day} {h} h")
        } else {
            format!("{d} {day}")
        }
    } else if h > 0 {
        if m > 0 {
            format!("{h} h {m} min")
        } else {
            format!("{h} h")
        }
    } else {
        format!("{m} min")
    };
    match lang {
        Lang::En => format!("resets in {span}"),
        Lang::It => format!("si azzera tra {span}"),
    }
}

/// "as of 14:05" from a preformatted local time.
pub fn as_of(hhmm: &str, lang: Lang) -> String {
    match lang {
        Lang::En => format!("as of {hhmm}"),
        Lang::It => format!("aggiornato alle {hhmm}"),
    }
}

pub const ID_BAR_5H: u32 = 11;
pub const ID_BAR_7D: u32 = 12;
/// The progress bar's hole.
pub const BAR_ROW_H_DIP: f32 = 12.0;

/// The detail panel: per window a header with its percentage, a progress bar hole and when it
/// resets; then when the data was saved and where it comes from.
pub fn claude_rows(u: Option<&ClaudeUsage>, now: i64, as_of_hhmm: &str, lang: Lang) -> Vec<Row> {
    let mut rows = Vec::new();
    let windows = u.map_or([None, None], |u| {
        [
            u.five_hour.map(|w| (Key::ClaudeFiveHour, ID_BAR_5H, w)),
            u.seven_day.map(|w| (Key::ClaudeWeekly, ID_BAR_7D, w)),
        ]
    });
    for (key, id, w) in windows.into_iter().flatten() {
        rows.push(Row::Header {
            text: tr(lang, key).to_string(),
            trailing: Some(format!("{:.0}%", shown_used(&w, now))),
            actions: Vec::new(),
        });
        rows.push(Row::Custom {
            id,
            h: BAR_ROW_H_DIP,
        });
        if let Some(t) = w.resets_at {
            rows.push(Row::Text(resets_in(t, now, lang)));
        }
    }
    if rows.is_empty() {
        rows.push(Row::Text(tr(lang, Key::ClaudeNoData).to_string()));
    } else {
        rows.push(Row::Separator);
        rows.push(Row::Text(as_of(as_of_hhmm, lang)));
    }
    rows.push(Row::Text(tr(lang, Key::ClaudeUpdatedHint).to_string()));
    rows
}

/// The fill of a window's progress bar, 0..=1 (0 once it has reset).
pub fn bar_fill(w: &Window, now: i64) -> f32 {
    (shown_used(w, now) / 100.0).clamp(0.0, 1.0)
}

#[cfg(test)]
mod claude_tests {
    use super::*;

    const NOW: i64 = 1_793_469_600; // 2026-10-31T18:00:00Z

    fn win(used: f32, resets_at: Option<i64>) -> Window {
        Window { used, resets_at }
    }

    #[test]
    fn parses_a_full_status_line() {
        let json = r#"{
            "model": {"display_name": "Opus"},
            "rate_limits": {
                "five_hour": {"used_percentage": 42.5, "resets_at": 1793476800},
                "seven_day": {"used_percentage": 18, "resets_at": 1793900000}
            },
            "extra": [1, 2, 3]
        }"#;
        let u = parse_statusline(json, NOW).unwrap();
        assert_eq!(u.five_hour, Some(win(42.5, Some(1_793_476_800))));
        assert_eq!(u.seven_day, Some(win(18.0, Some(1_793_900_000))));
        assert_eq!(u.at, NOW);
    }

    #[test]
    fn resets_at_in_ms_or_as_a_string() {
        let ms = r#"{"rate_limits":{"five_hour":{"used_percentage":1,"resets_at":1793476800000}}}"#;
        assert_eq!(
            parse_statusline(ms, NOW)
                .unwrap()
                .five_hour
                .unwrap()
                .resets_at,
            Some(1_793_476_800)
        );
        let iso = r#"{"rate_limits":{"five_hour":{"used_percentage":1,"resets_at":"2026-10-31T18:00:00Z"}}}"#;
        assert_eq!(
            parse_statusline(iso, NOW)
                .unwrap()
                .five_hour
                .unwrap()
                .resets_at,
            Some(NOW)
        );
        let offset = r#"{"rate_limits":{"five_hour":{"used_percentage":1,"resets_at":"2026-10-31T19:30:00.250+01:30"}}}"#;
        assert_eq!(
            parse_statusline(offset, NOW)
                .unwrap()
                .five_hour
                .unwrap()
                .resets_at,
            Some(NOW)
        );
        let junk = r#"{"rate_limits":{"five_hour":{"used_percentage":1,"resets_at":"soon"}}}"#;
        assert_eq!(
            parse_statusline(junk, NOW)
                .unwrap()
                .five_hour
                .unwrap()
                .resets_at,
            None
        );
    }

    #[test]
    fn only_the_weekly_window() {
        let json = r#"{"rate_limits":{"seven_day":{"used_percentage":7}}}"#;
        let u = parse_statusline(json, NOW).unwrap();
        assert_eq!(u.five_hour, None);
        assert_eq!(u.seven_day, Some(win(7.0, None)));
    }

    #[test]
    fn nothing_usable_is_none() {
        assert_eq!(parse_statusline(r#"{"model":{}}"#, NOW), None);
        assert_eq!(parse_statusline(r#"{"rate_limits":{}}"#, NOW), None);
        assert_eq!(
            parse_statusline(r#"{"rate_limits":{"five_hour":{}}}"#, NOW),
            None
        );
        assert_eq!(parse_statusline("not json at all", NOW), None);
        assert_eq!(parse_statusline("", NOW), None);
        assert_eq!(parse_statusline("[1,2]", NOW), None);
    }

    #[test]
    fn used_is_clamped() {
        let json = r#"{"rate_limits":{"five_hour":{"used_percentage":140},"seven_day":{"used_percentage":-3}}}"#;
        let u = parse_statusline(json, NOW).unwrap();
        assert_eq!(u.five_hour.unwrap().used, 100.0);
        assert_eq!(u.seven_day.unwrap().used, 0.0);
    }

    #[test]
    fn pill_texts() {
        let both = ClaudeUsage {
            five_hour: Some(win(42.4, Some(NOW + 3600))),
            seven_day: Some(win(17.6, Some(NOW + 86_400))),
            at: NOW - 60,
        };
        assert_eq!(
            claude_pill_text(Some(&both), NOW, Lang::En),
            "5h 42% · 7d 18%"
        );
        assert_eq!(
            claude_pill_text(Some(&both), NOW, Lang::It),
            "5h 42% · 7d 18%"
        );
        let one = ClaudeUsage {
            five_hour: None,
            ..both
        };
        assert_eq!(claude_pill_text(Some(&one), NOW, Lang::En), "7d 18%");
        let old = ClaudeUsage {
            at: NOW - STALE_AFTER_S - 1,
            ..one
        };
        assert_eq!(
            claude_pill_text(Some(&old), NOW, Lang::En),
            "7d 18% · 1 h ago"
        );
        assert_eq!(
            claude_pill_text(Some(&old), NOW, Lang::It),
            "7d 18% · 1 h fa"
        );
        let older = ClaudeUsage {
            at: NOW - 3 * 86_400 - 5,
            ..one
        };
        assert_eq!(
            claude_pill_text(Some(&older), NOW, Lang::En),
            "7d 18% · 3 d ago"
        );
        assert_eq!(age_text(5 * 3600 + 59, Lang::It), "5 h fa");
        assert_eq!(age_text(26 * 3600, Lang::It), "1 g fa");
        assert_eq!(claude_pill_text(None, NOW, Lang::En), "Claude: no data");
        assert_eq!(claude_pill_text(None, NOW, Lang::It), "Claude: nessun dato");
    }

    #[test]
    fn a_window_that_has_reset_reads_zero() {
        let u = ClaudeUsage {
            five_hour: Some(win(90.0, Some(NOW - 1))),
            seven_day: Some(win(30.0, None)),
            at: NOW - 600,
        };
        assert_eq!(claude_pill_text(Some(&u), NOW, Lang::En), "5h 0% · 7d 30%");
        assert_eq!(bar_fill(&u.five_hour.unwrap(), NOW), 0.0);
        assert_eq!(bar_fill(&u.seven_day.unwrap(), NOW), 0.3);
    }

    #[test]
    fn the_pill_changes_at_the_next_reset_or_when_it_goes_stale() {
        let u = ClaudeUsage {
            five_hour: Some(win(100.0, Some(NOW + 600))),
            seven_day: Some(win(40.0, Some(NOW + 86_400))),
            at: NOW - 60,
        };
        // The 5-hour reset comes first.
        assert_eq!(next_pill_change(&u, NOW), Some(NOW + 600));
        // After it: the stale moment (at + 1 h) comes before the weekly reset.
        assert_eq!(
            next_pill_change(&u, NOW + 600),
            Some(NOW - 60 + STALE_AFTER_S + 1)
        );
        // Stale: the age ticks every hour ...
        assert_eq!(
            next_pill_change(&u, NOW + 2 * 3600),
            Some(NOW - 60 + 3 * 3600)
        );
        // ... and every day after a day (the weekly reset comes first here).
        assert_eq!(
            next_pill_change(&u, NOW + 86_400 - 30),
            Some(NOW + 86_400 - 60 + 60)
        );
        let old = ClaudeUsage {
            five_hour: None,
            seven_day: Some(win(1.0, None)),
            at: NOW - 3 * 86_400,
        };
        assert_eq!(next_pill_change(&old, NOW), Some(NOW + 86_400));
        let none = ClaudeUsage {
            five_hour: None,
            seven_day: Some(win(1.0, None)),
            at: NOW,
        };
        assert_eq!(next_pill_change(&none, NOW), Some(NOW + STALE_AFTER_S + 1));
    }

    #[test]
    fn resets_in_rounds_up_to_minutes() {
        assert_eq!(resets_in(NOW + 59, NOW, Lang::En), "resets in 1 min");
        assert_eq!(resets_in(NOW + 60, NOW, Lang::En), "resets in 1 min");
        assert_eq!(resets_in(NOW + 61, NOW, Lang::En), "resets in 2 min");
        assert_eq!(
            resets_in(NOW + 2 * 3600 + 600, NOW, Lang::En),
            "resets in 2 h 10 min"
        );
        assert_eq!(resets_in(NOW + 3600, NOW, Lang::En), "resets in 1 h");
        assert_eq!(
            resets_in(NOW + 2 * 3600 + 600, NOW, Lang::It),
            "si azzera tra 2 h 10 min"
        );
        assert_eq!(
            resets_in(NOW + 3 * 86_400 + 4 * 3600, NOW, Lang::En),
            "resets in 3 d 4 h"
        );
        assert_eq!(
            resets_in(NOW + 3 * 86_400 + 4 * 3600, NOW, Lang::It),
            "si azzera tra 3 g 4 h"
        );
        assert_eq!(resets_in(NOW, NOW, Lang::En), "resets now");
        assert_eq!(resets_in(NOW - 5, NOW, Lang::It), "si azzera ora");
    }

    #[test]
    fn the_panel_rows() {
        let u = ClaudeUsage {
            five_hour: Some(win(42.0, Some(NOW + 600))),
            seven_day: None,
            at: NOW - 60,
        };
        let rows = claude_rows(Some(&u), NOW, "17:59", Lang::En);
        assert_eq!(
            rows[0],
            Row::Header {
                text: "5-hour limit".into(),
                trailing: Some("42%".into()),
                actions: Vec::new(),
            }
        );
        assert_eq!(
            rows[1],
            Row::Custom {
                id: ID_BAR_5H,
                h: BAR_ROW_H_DIP
            }
        );
        assert_eq!(rows[2], Row::Text("resets in 10 min".into()));
        assert!(!rows
            .iter()
            .any(|r| matches!(r, Row::Custom { id: ID_BAR_7D, .. })));
        assert!(rows.contains(&Row::Text("as of 17:59".into())));
        assert_eq!(
            rows.last(),
            Some(&Row::Text(tr(Lang::En, Key::ClaudeUpdatedHint).into()))
        );
        let none = claude_rows(None, NOW, "", Lang::It);
        assert_eq!(none[0], Row::Text("Claude: nessun dato".into()));
        assert_eq!(
            none.last(),
            Some(&Row::Text(tr(Lang::It, Key::ClaudeUpdatedHint).into()))
        );
    }

    #[test]
    fn the_saved_file_round_trips() {
        let u = ClaudeUsage {
            five_hour: Some(win(42.5, Some(1))),
            seven_day: None,
            at: 7,
        };
        let text = serde_json::to_string(&u).unwrap();
        assert!(
            text.contains("\"five_hour\"") && text.contains("\"at\":7"),
            "{text}"
        );
        assert_eq!(serde_json::from_str::<ClaudeUsage>(&text).unwrap(), u);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(title: &str, artist: &str) -> MusicView {
        MusicView {
            app: "Spotify.exe".into(),
            title: title.into(),
            artist: artist.into(),
            playing: true,
            can_prev: true,
            can_next: true,
            can_play_pause: true,
        }
    }

    #[test]
    fn pill_text_is_title_and_artist() {
        let v = view("Clair de lune", "Debussy");
        assert_eq!(music_pill_text(Some(&v), 40), "Clair de lune — Debussy");
    }

    #[test]
    fn empty_artist_gives_the_title_only() {
        let v = view("Podcast 12", "  ");
        assert_eq!(music_pill_text(Some(&v), 40), "Podcast 12");
    }

    #[test]
    fn no_session_or_no_title_hides_the_pill() {
        assert_eq!(music_pill_text(None, 40), "");
        assert_eq!(music_pill_text(Some(&view("", "Someone")), 40), "");
        assert_eq!(pill_width("", 0.0, MUSIC_PILL_MAX_DIP, true), 0.0);
    }

    #[test]
    fn ellipsis_cuts_at_max_chars() {
        let v = view("Abcdefghij", "Klm");
        // 8 clusters: 7 of text and the ellipsis.
        assert_eq!(music_pill_text(Some(&v), 8), "Abcdefg…");
        // A cut right after a space drops the space.
        assert_eq!(music_pill_text(Some(&view("Ab cd", "")), 4), "Ab…");
        // Exactly max_chars fits uncut.
        assert_eq!(music_pill_text(Some(&view("Abcd", "")), 4), "Abcd");
    }

    #[test]
    fn ellipsis_keeps_whole_grapheme_clusters() {
        // Family (ZWJ sequence), a flag (two regional indicators), a thumbs-up with a skin
        // tone, an accented "e" written as e + combining acute.
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        let flag = "\u{1F1EE}\u{1F1F9}";
        let thumb = "\u{1F44D}\u{1F3FD}";
        let e = "e\u{0301}";
        let title = format!("{family}{flag}{thumb}{e}xyz");
        let v = view(&title, "");
        assert_eq!(music_pill_text(Some(&v), 3), format!("{family}{flag}…"));
        assert_eq!(
            music_pill_text(Some(&v), 4),
            format!("{family}{flag}{thumb}…")
        );
        assert_eq!(
            music_pill_text(Some(&v), 5),
            format!("{family}{flag}{thumb}{e}…")
        );
        // A heart with its emoji variation selector stays whole.
        let heart = "\u{2764}\u{FE0F}";
        let v = view(&format!("{heart}{heart}{heart}"), "");
        assert_eq!(music_pill_text(Some(&v), 2), format!("{heart}…"));
        // Keycap "1️⃣".
        let one = "1\u{FE0F}\u{20E3}";
        let v = view(&format!("{one}{one}{one}"), "");
        assert_eq!(music_pill_text(Some(&v), 2), format!("{one}…"));
    }

    #[test]
    fn pill_width_adds_the_glyph_and_padding_and_caps() {
        let w = pill_width("Song", 40.0, MUSIC_PILL_MAX_DIP, false);
        assert_eq!(
            w,
            2.0 * PILL_PAD_DIP + PILL_GLYPH_W_DIP + PILL_GAP_DIP + 40.0
        );
        assert_eq!(
            pill_width("Song", 40.0, MUSIC_PILL_MAX_DIP, true),
            2.0 * PILL_PAD_DIP + PILL_ART_DIP + PILL_GAP_DIP + 40.0
        );
        assert_eq!(
            pill_width("Long", 900.0, MUSIC_PILL_MAX_DIP, false),
            MUSIC_PILL_MAX_DIP
        );
    }

    #[test]
    fn the_glyph_half_of_the_pill() {
        assert!(on_pill_glyph(0.0, false));
        assert!(on_pill_glyph(PILL_PAD_DIP + PILL_GLYPH_W_DIP, false));
        assert!(!on_pill_glyph(
            PILL_PAD_DIP + PILL_GLYPH_W_DIP + PILL_GAP_DIP,
            false
        ));
        assert!(on_pill_glyph(PILL_PAD_DIP + PILL_ART_DIP, true));
        assert!(!on_pill_glyph(120.0, true));
    }

    #[test]
    fn battery_glyph_levels_and_states() {
        assert_eq!(battery_glyph(0, false, false), '\u{E850}');
        assert_eq!(battery_glyph(100, false, false), '\u{E83F}');
        assert_eq!(battery_glyph(44, false, false), '\u{E854}');
        assert_eq!(battery_glyph(46, false, false), '\u{E855}');
        assert_eq!(battery_glyph(100, true, false), '\u{EA93}');
        assert_eq!(battery_glyph(17, false, true), '\u{E865}');
        // Charging wins over the saver (as the stock icon).
        assert_eq!(
            battery_glyph(17, true, true),
            battery_glyph(17, true, false)
        );
    }

    #[test]
    fn players_have_their_colours() {
        assert_eq!(
            player_tint("SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify"),
            Some(Rgba(0x1D, 0xB9, 0x54, 255))
        );
        assert_eq!(player_tint("Spotify.exe"), player_tint("spotify"));
        assert!(player_tint("AppleInc.AppleMusicWin_nzyj5cx40ttqa!App").is_some());
        assert!(player_tint("Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic").is_some());
        // A browser plays anything: no colour of its own.
        assert_eq!(player_tint("MSEdge"), None);
        assert_eq!(player_tint("chrome.exe"), None);
    }

    #[test]
    fn app_labels() {
        assert_eq!(app_label("Spotify.exe"), "Spotify");
        assert_eq!(app_label("chrome.EXE"), "chrome");
        assert_eq!(
            app_label("Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic"),
            "ZuneMusic"
        );
        assert_eq!(
            app_label("SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify"),
            "Spotify"
        );
        assert_eq!(app_label(""), "");
    }

    #[test]
    fn panel_rows_show_only_the_allowed_buttons() {
        let mut v = view("Song", "Band");
        let rows = music_rows(&v, Lang::En);
        assert_eq!(
            rows[0],
            Row::Custom {
                id: ID_ART,
                h: ART_ROW_H_DIP
            }
        );
        let ids = |rows: &[Row]| -> Vec<u32> {
            rows.iter()
                .find_map(|r| match r {
                    Row::Buttons(b) => Some(b.iter().map(|b| b.id).collect()),
                    _ => None,
                })
                .unwrap_or_default()
        };
        assert_eq!(ids(&rows), vec![ID_PREV, ID_PLAY, ID_NEXT]);
        assert!(rows.contains(&Row::Text("Spotify".into())));
        v.can_prev = false;
        v.can_next = false;
        assert_eq!(ids(&music_rows(&v, Lang::En)), vec![ID_PLAY]);
        v.can_play_pause = false;
        assert!(ids(&music_rows(&v, Lang::It)).is_empty());
    }

    #[test]
    fn the_play_button_shows_what_it_will_do() {
        let mut v = view("Song", "Band");
        let glyph = |v: &MusicView| {
            music_rows(v, Lang::En).iter().find_map(|r| match r {
                Row::Buttons(b) => b.iter().find(|b| b.id == ID_PLAY).and_then(|b| b.glyph),
                _ => None,
            })
        };
        assert_eq!(glyph(&v), Some(G_PAUSE));
        v.playing = false;
        assert_eq!(glyph(&v), Some(G_PLAY));
        assert_eq!(
            music_action(Part::Button(ID_PLAY)),
            Some(MusicAction::PlayPause)
        );
        assert_eq!(music_action(Part::Custom(ID_ART)), None);
    }

    #[test]
    fn button_construction_is_glyph_only() {
        let rows = music_rows(&view("Song", "Band"), Lang::En);
        let all: Vec<&Button> = rows
            .iter()
            .flat_map(|r| match r {
                Row::Buttons(b) => b.iter().collect(),
                _ => Vec::new(),
            })
            .collect();
        assert!(all.iter().all(|b| b.text.is_empty() && b.glyph.is_some()));
    }
}
