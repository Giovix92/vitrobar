//! The input-language indicator's text (spec §5.4): a short name for the active keyboard
//! layout's language, as the stock taskbar shows it ("ITA", "ENG").

/// The short name for language id `langid` (the low word of an `HKL`); for a language not in
/// the table, the first three letters of `locale_name` (e.g. "ga-IE" → "GAI"), uppercased.
pub fn ime_short_name(langid: u16, locale_name: &str) -> String {
    // Primary language ids (the low 10 bits), as the stock taskbar abbreviates them.
    let known = match langid & 0x3FF {
        0x09 => "ENG",
        0x10 => "ITA",
        0x07 => "DEU",
        0x0C => "FRA",
        0x0A => "ESP",
        0x16 => "POR",
        0x13 => "NLD",
        0x15 => "POL",
        // The stock taskbar writes Russian in Cyrillic.
        0x19 => "РУС",
        0x11 => "JPN",
        0x12 => "KOR",
        0x04 => "CHN",
        _ => "",
    };
    if !known.is_empty() {
        return known.to_string();
    }
    locale_name
        .chars()
        .filter(char::is_ascii_alphabetic)
        .take(3)
        .collect::<String>()
        .to_ascii_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_languages() {
        assert_eq!(ime_short_name(0x0410, "it-IT"), "ITA");
        assert_eq!(ime_short_name(0x0810, "it-CH"), "ITA");
        assert_eq!(ime_short_name(0x0409, "en-US"), "ENG");
        assert_eq!(ime_short_name(0x0809, "en-GB"), "ENG");
        assert_eq!(ime_short_name(0x0407, "de-DE"), "DEU");
        assert_eq!(ime_short_name(0x040C, "fr-FR"), "FRA");
        assert_eq!(ime_short_name(0x0C0A, "es-ES"), "ESP");
    }

    #[test]
    fn unknown_languages_use_the_locale_name() {
        assert_eq!(ime_short_name(0x083C, "ga-IE"), "GAI");
        assert_eq!(ime_short_name(0x0000, "xy"), "XY");
        assert_eq!(ime_short_name(0x0000, ""), "");
    }
}
