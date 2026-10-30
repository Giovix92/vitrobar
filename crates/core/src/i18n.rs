//! English / Italian UI strings.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    It,
}

/// "it", "it-IT", "IT-it" -> Italian (primary subtag, case-insensitive); anything else English.
pub fn lang_from_tag(tag: &str) -> Lang {
    let primary = tag.split(['-', '_']).next().unwrap_or("");
    if primary.eq_ignore_ascii_case("it") {
        Lang::It
    } else {
        Lang::En
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Pin,
    Unpin,
    NewWindow,
    CloseWindow,
    CloseAllWindows,
    Shape,
    ShapePill,
    ShapeIslands,
    ShapeFull,
    Material,
    MaterialFrosted,
    MaterialSmoked,
    MaterialSolidBlack,
    MaterialSolidWhite,
    Hover,
    HoverPlate,
    HoverLift,
    HoverMagnify,
    HoverSweep,
    IconSize,
    Small,
    Medium,
    Large,
    Position,
    Bottom,
    Top,
    StartWithWindows,
    ExitVitrobar,
}

pub const ALL_KEYS: &[Key] = &[
    Key::Pin,
    Key::Unpin,
    Key::NewWindow,
    Key::CloseWindow,
    Key::CloseAllWindows,
    Key::Shape,
    Key::ShapePill,
    Key::ShapeIslands,
    Key::ShapeFull,
    Key::Material,
    Key::MaterialFrosted,
    Key::MaterialSmoked,
    Key::MaterialSolidBlack,
    Key::MaterialSolidWhite,
    Key::Hover,
    Key::HoverPlate,
    Key::HoverLift,
    Key::HoverMagnify,
    Key::HoverSweep,
    Key::IconSize,
    Key::Small,
    Key::Medium,
    Key::Large,
    Key::Position,
    Key::Bottom,
    Key::Top,
    Key::StartWithWindows,
    Key::ExitVitrobar,
];

pub fn tr(lang: Lang, key: Key) -> &'static str {
    match (lang, key) {
        (Lang::En, Key::Pin) => "Pin to bar",
        (Lang::It, Key::Pin) => "Aggiungi alla barra",
        (Lang::En, Key::Unpin) => "Unpin from bar",
        (Lang::It, Key::Unpin) => "Rimuovi dalla barra",
        (Lang::En, Key::NewWindow) => "New window",
        (Lang::It, Key::NewWindow) => "Nuova finestra",
        (Lang::En, Key::CloseWindow) => "Close window",
        (Lang::It, Key::CloseWindow) => "Chiudi finestra",
        (Lang::En, Key::CloseAllWindows) => "Close all windows",
        (Lang::It, Key::CloseAllWindows) => "Chiudi tutte le finestre",
        (Lang::En, Key::Shape) => "Shape",
        (Lang::It, Key::Shape) => "Forma",
        (Lang::En, Key::ShapePill) => "Pill",
        (Lang::It, Key::ShapePill) => "Pillola",
        (Lang::En, Key::ShapeIslands) => "Islands",
        (Lang::It, Key::ShapeIslands) => "Isole",
        (Lang::En, Key::ShapeFull) => "Full width",
        (Lang::It, Key::ShapeFull) => "Larghezza intera",
        (Lang::En, Key::Material) => "Material",
        (Lang::It, Key::Material) => "Materiale",
        (Lang::En, Key::MaterialFrosted) => "Frosted glass",
        (Lang::It, Key::MaterialFrosted) => "Vetro smerigliato",
        (Lang::En, Key::MaterialSmoked) => "Smoked glass",
        (Lang::It, Key::MaterialSmoked) => "Vetro fumé",
        (Lang::En, Key::MaterialSolidBlack) => "Black",
        (Lang::It, Key::MaterialSolidBlack) => "Nero",
        (Lang::En, Key::MaterialSolidWhite) => "White",
        (Lang::It, Key::MaterialSolidWhite) => "Bianco",
        (Lang::En, Key::Hover) => "Hover effect",
        (Lang::It, Key::Hover) => "Effetto al passaggio",
        (Lang::En, Key::HoverPlate) => "Glass plate",
        (Lang::It, Key::HoverPlate) => "Piastra di vetro",
        (Lang::En, Key::HoverLift) => "Lift",
        (Lang::It, Key::HoverLift) => "Sollevamento",
        (Lang::En, Key::HoverMagnify) => "Magnify",
        (Lang::It, Key::HoverMagnify) => "Ingrandimento",
        (Lang::En, Key::HoverSweep) => "Light sweep",
        (Lang::It, Key::HoverSweep) => "Riflesso",
        (Lang::En, Key::IconSize) => "Icon size",
        (Lang::It, Key::IconSize) => "Dimensione icone",
        (Lang::En, Key::Small) => "Small",
        (Lang::It, Key::Small) => "Piccole",
        (Lang::En, Key::Medium) => "Medium",
        (Lang::It, Key::Medium) => "Medie",
        (Lang::En, Key::Large) => "Large",
        (Lang::It, Key::Large) => "Grandi",
        (Lang::En, Key::Position) => "Position",
        (Lang::It, Key::Position) => "Posizione",
        (Lang::En, Key::Bottom) => "Bottom",
        (Lang::It, Key::Bottom) => "In basso",
        (Lang::En, Key::Top) => "Top",
        (Lang::It, Key::Top) => "In alto",
        (Lang::En, Key::StartWithWindows) => "Start with Windows",
        (Lang::It, Key::StartWithWindows) => "Avvia con Windows",
        (Lang::En, Key::ExitVitrobar) => "Exit Vitrobar",
        (Lang::It, Key::ExitVitrobar) => "Esci da Vitrobar",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lang_tags() {
        assert_eq!(lang_from_tag("it-CH"), Lang::It);
        assert_eq!(lang_from_tag("it"), Lang::It);
        assert_eq!(lang_from_tag("IT-it"), Lang::It);
        assert_eq!(lang_from_tag("de-DE"), Lang::En);
        assert_eq!(lang_from_tag(""), Lang::En);
    }
    #[test]
    fn every_key_translated() {
        for &k in ALL_KEYS {
            assert!(!tr(Lang::En, k).is_empty());
            assert!(!tr(Lang::It, k).is_empty());
        }
        assert_eq!(ALL_KEYS.len(), 28);
    }
    #[test]
    fn spot_checks() {
        assert_eq!(tr(Lang::En, Key::ExitVitrobar), "Exit Vitrobar");
        assert_eq!(tr(Lang::It, Key::MaterialSmoked), "Vetro fumé");
        assert_eq!(tr(Lang::En, Key::HoverSweep), "Light sweep");
    }
}
