//! Bar widgets (spec §5.7): what the pills say and how wide they are, and the music detail
//! panel's rows. Pure; the app feeds it service snapshots.

use crate::i18n::Lang;
use unicode_segmentation::UnicodeSegmentation;

use crate::panel_kit::{Button, Part, Row};

/// The pill's look, shared with the bar visual that draws it (DIPs).
pub const PILL_FONT_DIP: f32 = 13.0;
pub const PILL_PAD_DIP: f32 = 10.0;
pub const PILL_GAP_DIP: f32 = 6.0;
/// The glyph's box: a little wider than the font size.
pub const PILL_GLYPH_W_DIP: f32 = PILL_FONT_DIP * 1.2;

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
pub fn pill_width(text: &str, text_w_dip: f32, max_dip: f32) -> f32 {
    if text.is_empty() {
        return 0.0;
    }
    (2.0 * PILL_PAD_DIP + PILL_GLYPH_W_DIP + PILL_GAP_DIP + text_w_dip.max(0.0)).min(max_dip)
}

/// Whether `x_dip` (from the pill's left edge) falls on the pill's glyph button rather than
/// on its text.
pub fn on_pill_glyph(x_dip: f32) -> bool {
    x_dip < PILL_PAD_DIP + PILL_GLYPH_W_DIP + PILL_GAP_DIP / 2.0
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
        assert_eq!(pill_width("", 0.0, MUSIC_PILL_MAX_DIP), 0.0);
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
        assert_eq!(music_pill_text(Some(&v), 4), format!("{family}{flag}{thumb}…"));
        assert_eq!(music_pill_text(Some(&v), 5), format!("{family}{flag}{thumb}{e}…"));
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
        let w = pill_width("Song", 40.0, MUSIC_PILL_MAX_DIP);
        assert_eq!(w, 2.0 * PILL_PAD_DIP + PILL_GLYPH_W_DIP + PILL_GAP_DIP + 40.0);
        assert_eq!(pill_width("Long", 900.0, MUSIC_PILL_MAX_DIP), MUSIC_PILL_MAX_DIP);
    }

    #[test]
    fn the_glyph_half_of_the_pill() {
        assert!(on_pill_glyph(0.0));
        assert!(on_pill_glyph(PILL_PAD_DIP + PILL_GLYPH_W_DIP));
        assert!(!on_pill_glyph(PILL_PAD_DIP + PILL_GLYPH_W_DIP + PILL_GAP_DIP));
        assert!(!on_pill_glyph(120.0));
    }

    #[test]
    fn app_labels() {
        assert_eq!(app_label("Spotify.exe"), "Spotify");
        assert_eq!(app_label("chrome.EXE"), "chrome");
        assert_eq!(
            app_label("Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic"),
            "ZuneMusic"
        );
        assert_eq!(app_label("SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify"), "Spotify");
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
        assert_eq!(music_action(Part::Button(ID_PLAY)), Some(MusicAction::PlayPause));
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
