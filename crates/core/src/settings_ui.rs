//! Settings-window control model (pure logic): what the window shows, how a choice changes
//! [`Settings`], and keyboard focus order. Rendering and hit-testing live in the app.

use crate::i18n::Key;
use crate::layout::Rect;
use crate::settings::{
    HoverEffect, IconSize, Material, Position, Settings, Shape, StartStyle, StockTaskbar,
    ThemeChoice,
};

pub const ACCENT_SWATCHES: &[Option<u32>] = &[
    None, // system
    Some(0x0078D4),
    Some(0x0E7C86),
    Some(0x7A5AF8),
    Some(0xC239B3),
    Some(0xE81123),
    Some(0xF7630C),
    Some(0x10893E),
    Some(0x5D5A58),
];

pub const LICENSE_URL: &str = "https://www.gnu.org/licenses/gpl-3.0.html";
/// Placeholder until the repository exists; update when it is pushed.
pub const SOURCE_URL: &str = "https://github.com/Giovix92/vitrobar";

const PAD: f32 = 24.0;
const ROW_H: f32 = 48.0;
const HEADER_H: f32 = 36.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Appearance,
    Behaviour,
    Widgets,
    Language,
    About,
}

impl Section {
    pub fn title(self) -> Key {
        match self {
            Section::Appearance => Key::Appearance,
            Section::Behaviour => Key::Behaviour,
            Section::Widgets => Key::Widgets,
            Section::Language => Key::Language,
            Section::About => Key::About,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ControlKind {
    Choice {
        options: Vec<Key>,
        selected: usize,
    },
    Toggle(bool),
    Swatches {
        colors: Vec<Option<u32>>,
        selected: usize,
    },
    Link(&'static str),
    Text(Key),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlId {
    Shape,
    Material,
    Hover,
    IconSize,
    Position,
    Theme,
    Accent,
    StartStyle,
    WinKey,
    StartWithWindows,
    ReserveFloating,
    StockTaskbar,
    WidgetMusic,
    WidgetClaude,
    ClaudeWidgetHint,
    Language,
    AboutVersion,
    AboutLicense,
    AboutSource,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Control {
    pub id: ControlId,
    pub section: Section,
    pub label: Key,
    pub kind: ControlKind,
    pub rect: Rect,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SettingsModel {
    pub controls: Vec<Control>,
    pub focus: usize,
}

const SHAPES: [Shape; 3] = [Shape::Pill, Shape::Islands, Shape::Full];
const MATERIALS: [Material; 4] = [
    Material::Frosted,
    Material::Smoked,
    Material::SolidBlack,
    Material::SolidWhite,
];
const HOVERS: [HoverEffect; 4] = [
    HoverEffect::Plate,
    HoverEffect::Lift,
    HoverEffect::Magnify,
    HoverEffect::Sweep,
];
const SIZES: [IconSize; 3] = [IconSize::Small, IconSize::Medium, IconSize::Large];
const POSITIONS: [Position; 2] = [Position::Bottom, Position::Top];
const THEMES: [ThemeChoice; 3] = [ThemeChoice::System, ThemeChoice::Light, ThemeChoice::Dark];
const STARTS: [StartStyle; 2] = [StartStyle::Panel, StartStyle::FullScreen];
const STOCKS: [StockTaskbar; 2] = [StockTaskbar::Hidden, StockTaskbar::AutoHide];

fn index_of<T: PartialEq>(all: &[T], v: &T) -> usize {
    all.iter().position(|x| x == v).unwrap_or(0)
}

/// Any language other than en/it (hand-edited file) is shown as "Same as Windows".
fn language_index(s: &Settings) -> usize {
    match s.language.as_deref() {
        Some("en") => 1,
        Some("it") => 2,
        _ => 0,
    }
}

/// Builds the control list in display order, stacked vertically: 24 DIP padding, a 36 DIP
/// header before each section, 48 DIP rows. Controls span `width_dip` minus the padding.
pub fn build_model(s: &Settings, width_dip: f32) -> SettingsModel {
    let mut controls = Vec::new();
    let mut y = PAD;
    let mut last: Option<Section> = None;
    let mut add = |id: ControlId, section: Section, label: Key, kind: ControlKind| {
        if last != Some(section) {
            y += HEADER_H;
            last = Some(section);
        }
        controls.push(Control {
            id,
            section,
            label,
            kind,
            rect: Rect {
                x: PAD,
                y,
                w: (width_dip - 2.0 * PAD).max(0.0),
                h: ROW_H,
            },
        });
        y += ROW_H;
    };
    let choice = |options: &[Key], selected: usize| ControlKind::Choice {
        options: options.to_vec(),
        selected,
    };
    use Section::*;

    add(
        ControlId::Shape,
        Appearance,
        Key::Shape,
        choice(
            &[Key::ShapePill, Key::ShapeIslands, Key::ShapeFull],
            index_of(&SHAPES, &s.shape),
        ),
    );
    add(
        ControlId::Material,
        Appearance,
        Key::Material,
        choice(
            &[
                Key::MaterialFrosted,
                Key::MaterialSmoked,
                Key::MaterialSolidBlack,
                Key::MaterialSolidWhite,
            ],
            index_of(&MATERIALS, &s.material),
        ),
    );
    add(
        ControlId::Hover,
        Appearance,
        Key::Hover,
        choice(
            &[
                Key::HoverPlate,
                Key::HoverLift,
                Key::HoverMagnify,
                Key::HoverSweep,
            ],
            index_of(&HOVERS, &s.hover),
        ),
    );
    add(
        ControlId::IconSize,
        Appearance,
        Key::IconSize,
        choice(
            &[Key::Small, Key::Medium, Key::Large],
            index_of(&SIZES, &s.icon_size),
        ),
    );
    add(
        ControlId::Position,
        Appearance,
        Key::Position,
        choice(&[Key::Bottom, Key::Top], index_of(&POSITIONS, &s.position)),
    );
    add(
        ControlId::Theme,
        Appearance,
        Key::Theme,
        choice(
            &[Key::ThemeSystem, Key::ThemeLight, Key::ThemeDark],
            index_of(&THEMES, &s.theme),
        ),
    );
    let mut colors = ACCENT_SWATCHES.to_vec();
    let selected = match colors.iter().position(|c| *c == s.accent) {
        Some(i) => i,
        None => {
            // A colour set by hand in the file: show it as an extra, selected swatch.
            colors.push(s.accent);
            colors.len() - 1
        }
    };
    add(
        ControlId::Accent,
        Appearance,
        Key::Accent,
        ControlKind::Swatches { colors, selected },
    );

    add(
        ControlId::StartStyle,
        Behaviour,
        Key::StartStyleKey,
        choice(
            &[Key::StartPanel, Key::StartFullScreen],
            index_of(&STARTS, &s.start_style),
        ),
    );
    add(
        ControlId::WinKey,
        Behaviour,
        Key::WinKeyOpensStart,
        ControlKind::Toggle(s.win_key_opens_start),
    );
    add(
        ControlId::StartWithWindows,
        Behaviour,
        Key::StartWithWindows,
        ControlKind::Toggle(s.start_with_windows),
    );
    add(
        ControlId::ReserveFloating,
        Behaviour,
        Key::ReserveSpace,
        ControlKind::Toggle(s.reserve_space_floating),
    );
    add(
        ControlId::StockTaskbar,
        Behaviour,
        Key::StockTaskbarKey,
        choice(
            &[Key::StockHidden, Key::StockAutoHide],
            index_of(&STOCKS, &s.stock_taskbar),
        ),
    );

    add(
        ControlId::WidgetMusic,
        Widgets,
        Key::MusicWidget,
        ControlKind::Toggle(s.widget_music),
    );
    add(
        ControlId::WidgetClaude,
        Widgets,
        Key::ClaudeWidget,
        ControlKind::Toggle(s.widget_claude),
    );
    add(
        ControlId::ClaudeWidgetHint,
        Widgets,
        Key::ClaudeWidgetHint,
        ControlKind::Text(Key::ClaudeWidgetHint),
    );

    add(
        ControlId::Language,
        Language,
        Key::Language,
        choice(
            &[Key::LanguageSystem, Key::LangEnglish, Key::LangItalian],
            language_index(s),
        ),
    );

    add(
        ControlId::AboutVersion,
        About,
        Key::Version,
        ControlKind::Text(Key::Version),
    );
    add(
        ControlId::AboutLicense,
        About,
        Key::License,
        ControlKind::Link(LICENSE_URL),
    );
    add(
        ControlId::AboutSource,
        About,
        Key::SourceCode,
        ControlKind::Link(SOURCE_URL),
    );

    SettingsModel { controls, focus: 0 }
}

fn set<T: PartialEq + Copy>(slot: &mut T, all: &[T], choice: usize) -> bool {
    match all.get(choice) {
        Some(&v) if v != *slot => {
            *slot = v;
            true
        }
        _ => false,
    }
}

fn set_bool(slot: &mut bool, choice: usize) -> bool {
    let v = choice != 0;
    let changed = *slot != v;
    *slot = v;
    changed
}

/// Applies a choice (option index, swatch index, or 0/1 for a toggle) to `s`. Returns whether
/// anything changed; an out-of-range index or a non-editable control changes nothing.
pub fn apply(s: &mut Settings, id: ControlId, choice: usize) -> bool {
    match id {
        ControlId::Shape => set(&mut s.shape, &SHAPES, choice),
        ControlId::Material => set(&mut s.material, &MATERIALS, choice),
        ControlId::Hover => set(&mut s.hover, &HOVERS, choice),
        ControlId::IconSize => set(&mut s.icon_size, &SIZES, choice),
        ControlId::Position => set(&mut s.position, &POSITIONS, choice),
        ControlId::Theme => set(&mut s.theme, &THEMES, choice),
        ControlId::Accent => match ACCENT_SWATCHES.get(choice) {
            Some(&c) if c != s.accent => {
                s.accent = c;
                true
            }
            _ => false,
        },
        ControlId::StartStyle => set(&mut s.start_style, &STARTS, choice),
        ControlId::WinKey => set_bool(&mut s.win_key_opens_start, choice),
        ControlId::StartWithWindows => set_bool(&mut s.start_with_windows, choice),
        ControlId::ReserveFloating => set_bool(&mut s.reserve_space_floating, choice),
        ControlId::StockTaskbar => set(&mut s.stock_taskbar, &STOCKS, choice),
        ControlId::WidgetMusic => set_bool(&mut s.widget_music, choice),
        ControlId::WidgetClaude => set_bool(&mut s.widget_claude, choice),
        ControlId::Language => {
            let new = match choice {
                0 => None,
                1 => Some("en".to_string()),
                2 => Some("it".to_string()),
                _ => return false,
            };
            if s.language == new {
                false
            } else {
                s.language = new;
                true
            }
        }
        ControlId::ClaudeWidgetHint
        | ControlId::AboutVersion
        | ControlId::AboutLicense
        | ControlId::AboutSource => false,
    }
}

fn focusable(c: &Control) -> bool {
    !matches!(c.kind, ControlKind::Text(_))
}

fn step_focus(m: &mut SettingsModel, forward: bool) {
    let n = m.controls.len();
    for k in 1..=n {
        let i = if forward {
            (m.focus + k) % n
        } else {
            (m.focus + n - k % n) % n
        };
        if focusable(&m.controls[i]) {
            m.focus = i;
            return;
        }
    }
}

/// The Claude widget's hint line: what it does, or why turning it on failed.
pub fn set_claude_hint(m: &mut SettingsModel, failed: bool) {
    let key = if failed {
        Key::ClaudeSetupFailed
    } else {
        Key::ClaudeWidgetHint
    };
    if let Some(c) = m
        .controls
        .iter_mut()
        .find(|c| c.id == ControlId::ClaudeWidgetHint)
    {
        c.kind = ControlKind::Text(key);
    }
}

/// Moves focus to the next focusable control (links are focusable, plain text is not),
/// wrapping at the end.
pub fn focus_next(m: &mut SettingsModel) {
    step_focus(m, true);
}

/// Moves focus to the previous focusable control, wrapping at the start.
pub fn focus_prev(m: &mut SettingsModel) {
    step_focus(m, false);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{tr, Lang, ALL_KEYS};

    fn find(m: &SettingsModel, id: ControlId) -> &Control {
        m.controls.iter().find(|c| c.id == id).unwrap()
    }

    #[test]
    fn settings_model_reflects_values_and_apply_changes() {
        let mut s = Settings::default();
        assert!(apply(&mut s, ControlId::Shape, 2));
        assert_eq!(s.shape, Shape::Full);
        assert!(!apply(&mut s, ControlId::Shape, 2));
        let m = build_model(&s, 600.0);
        assert!(matches!(
            find(&m, ControlId::Shape).kind,
            ControlKind::Choice { selected: 2, .. }
        ));
        assert!(!apply(&mut s, ControlId::Shape, 9));
        assert_eq!(s.shape, Shape::Full);
    }

    #[test]
    fn every_control_round_trips_through_apply() {
        let mut s = Settings::default();
        assert!(apply(&mut s, ControlId::Material, 3));
        assert_eq!(s.material, Material::SolidWhite);
        assert!(apply(&mut s, ControlId::Hover, 3));
        assert!(apply(&mut s, ControlId::IconSize, 0));
        assert_eq!(s.icon_size, IconSize::Small);
        assert!(apply(&mut s, ControlId::Position, 1));
        assert!(apply(&mut s, ControlId::Theme, 2));
        assert!(apply(&mut s, ControlId::Accent, 2));
        assert_eq!(s.accent, Some(0x0E7C86));
        assert!(apply(&mut s, ControlId::Accent, 0));
        assert_eq!(s.accent, None);
        assert!(apply(&mut s, ControlId::StartStyle, 1));
        assert!(apply(&mut s, ControlId::WinKey, 0));
        assert!(!s.win_key_opens_start);
        assert!(!apply(&mut s, ControlId::WinKey, 0));
        assert!(apply(&mut s, ControlId::StartWithWindows, 0));
        assert!(apply(&mut s, ControlId::ReserveFloating, 0));
        assert!(apply(&mut s, ControlId::StockTaskbar, 1));
        assert!(!apply(&mut s, ControlId::AboutLicense, 0));
        let m = build_model(&s, 600.0);
        assert!(matches!(
            find(&m, ControlId::WinKey).kind,
            ControlKind::Toggle(false)
        ));
        assert!(matches!(
            find(&m, ControlId::Accent).kind,
            ControlKind::Swatches { selected: 0, .. }
        ));
    }

    #[test]
    fn widget_toggles_set_the_widget_settings() {
        let mut s = Settings::default();
        assert!(apply(&mut s, ControlId::WidgetMusic, 1));
        assert!(s.widget_music && !s.widget_claude);
        assert!(!apply(&mut s, ControlId::WidgetMusic, 1));
        assert!(apply(&mut s, ControlId::WidgetClaude, 1));
        assert!(s.widget_claude);
        assert!(!apply(&mut s, ControlId::ClaudeWidgetHint, 1));
        let m = build_model(&s, 600.0);
        assert!(matches!(
            find(&m, ControlId::WidgetMusic).kind,
            ControlKind::Toggle(true)
        ));
        let ids: Vec<_> = m.controls.iter().map(|c| (c.id, c.section)).collect();
        let at = |id| ids.iter().position(|(i, _)| *i == id).unwrap();
        assert_eq!(ids[at(ControlId::WidgetMusic)].1, Section::Widgets);
        assert!(at(ControlId::StockTaskbar) < at(ControlId::WidgetMusic));
        assert!(at(ControlId::ClaudeWidgetHint) == at(ControlId::WidgetClaude) + 1);
        assert!(at(ControlId::ClaudeWidgetHint) < at(ControlId::Language));
        assert_eq!(
            find(&m, ControlId::ClaudeWidgetHint).kind,
            ControlKind::Text(Key::ClaudeWidgetHint)
        );
    }

    #[test]
    fn language_maps_to_option() {
        let mut s = Settings::default();
        assert!(!apply(&mut s, ControlId::Language, 0));
        assert!(apply(&mut s, ControlId::Language, 1));
        assert_eq!(s.language.as_deref(), Some("en"));
        assert!(apply(&mut s, ControlId::Language, 2));
        assert_eq!(s.language.as_deref(), Some("it"));
        assert!(!apply(&mut s, ControlId::Language, 3));
        assert!(apply(&mut s, ControlId::Language, 0));
        assert_eq!(s.language, None);
        s.language = Some("it".into());
        assert!(matches!(
            find(&build_model(&s, 600.0), ControlId::Language).kind,
            ControlKind::Choice { selected: 2, .. }
        ));
        // Language names stay in their own language.
        assert_eq!(tr(Lang::En, Key::LangItalian), "Italiano");
        assert_eq!(tr(Lang::It, Key::LangEnglish), "English");
    }

    #[test]
    fn custom_accent_shows_as_extra_swatch() {
        let s = Settings {
            accent: Some(0x123456),
            ..Settings::default()
        };
        match &find(&build_model(&s, 600.0), ControlId::Accent).kind {
            ControlKind::Swatches { colors, selected } => {
                assert_eq!(colors.len(), ACCENT_SWATCHES.len() + 1);
                assert_eq!(colors[*selected], Some(0x123456));
            }
            k => panic!("{k:?}"),
        }
    }

    #[test]
    fn material_choices_exclude_liquid() {
        let m = build_model(&Settings::default(), 600.0);
        match &find(&m, ControlId::Material).kind {
            ControlKind::Choice { options, .. } => assert_eq!(options.len(), 4),
            k => panic!("{k:?}"),
        }
    }

    #[test]
    fn model_order_and_geometry() {
        let m = build_model(&Settings::default(), 600.0);
        let ids: Vec<_> = m.controls.iter().map(|c| c.id).collect();
        assert_eq!(ids.len(), 19);
        assert_eq!(ids[0], ControlId::Shape);
        assert_eq!(ids[18], ControlId::AboutSource);
        assert_eq!(m.focus, 0);
        // 24 padding, then a 36 header, then 48 rows.
        assert_eq!(m.controls[0].rect.y, 24.0 + 36.0);
        assert_eq!(m.controls[0].rect.x, 24.0);
        assert_eq!(m.controls[0].rect.w, 552.0);
        assert_eq!(m.controls[1].rect.y, m.controls[0].rect.bottom());
        let b = find(&m, ControlId::StartStyle);
        let a = find(&m, ControlId::Accent);
        assert_eq!(b.rect.y, a.rect.bottom() + 36.0);
        for w in m.controls.windows(2) {
            assert!(w[1].rect.y >= w[0].rect.bottom());
        }
        assert_eq!(
            find(&m, ControlId::AboutVersion).kind,
            ControlKind::Text(Key::Version)
        );
        assert_eq!(
            find(&m, ControlId::AboutLicense).kind,
            ControlKind::Link("https://www.gnu.org/licenses/gpl-3.0.html")
        );
    }

    #[test]
    fn focus_cycles_and_skips_text() {
        let mut m = build_model(&Settings::default(), 600.0);
        let text = m
            .controls
            .iter()
            .position(|c| c.id == ControlId::AboutVersion)
            .unwrap();
        m.focus = text - 1;
        focus_next(&mut m);
        assert_eq!(m.controls[m.focus].id, ControlId::AboutLicense);
        focus_prev(&mut m);
        assert_eq!(m.controls[m.focus].id, ControlId::Language);
        m.focus = m.controls.len() - 1;
        focus_next(&mut m);
        assert_eq!(m.focus, 0);
        focus_prev(&mut m);
        assert_eq!(m.controls[m.focus].id, ControlId::AboutSource);
    }

    #[test]
    fn labels_are_translated() {
        let m = build_model(&Settings::default(), 600.0);
        for c in &m.controls {
            assert!(ALL_KEYS.contains(&c.label));
            assert!(!tr(Lang::It, c.label).is_empty());
        }
    }

    #[test]
    fn the_claude_hint_says_when_setup_failed() {
        let mut m = build_model(&Settings::default(), 600.0);
        set_claude_hint(&mut m, true);
        assert_eq!(
            find(&m, ControlId::ClaudeWidgetHint).kind,
            ControlKind::Text(Key::ClaudeSetupFailed)
        );
        set_claude_hint(&mut m, false);
        assert_eq!(
            find(&m, ControlId::ClaudeWidgetHint).kind,
            ControlKind::Text(Key::ClaudeWidgetHint)
        );
    }
}
