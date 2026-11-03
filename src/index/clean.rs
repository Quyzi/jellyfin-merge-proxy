//! Jellyfin's `GetCleanValue`: the diacritics-free lower-case form that
//! search, `Name` sorting and tag matching compare on.

use unicode_normalization::char::is_combining_mark;
use unicode_normalization::UnicodeNormalization;

/// Whitespace-only input is returned unchanged. Otherwise: NFD, combining
/// marks dropped, letters NFD cannot decompose transliterated, lower case.
pub fn clean(s: &str) -> String {
    if s.trim().is_empty() {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    for c in s.nfd().filter(|c| !is_combining_mark(*c)) {
        match c {
            'ø' => out.push('o'),
            'Ø' => out.push('O'),
            'đ' | 'ð' => out.push('d'),
            'Đ' | 'Ð' => out.push('D'),
            'ł' => out.push('l'),
            'Ł' => out.push('L'),
            'ħ' => out.push('h'),
            'Ħ' => out.push('H'),
            'ı' => out.push('i'),
            'ß' => out.push_str("ss"),
            'æ' => out.push_str("ae"),
            'Æ' => out.push_str("AE"),
            'œ' => out.push_str("oe"),
            'Œ' => out.push_str("OE"),
            'þ' => out.push_str("th"),
            'Þ' => out.push_str("TH"),
            c => out.push(c),
        }
    }
    out.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Searching "amelie" must find "Amélie": accents are stripped.
    #[test]
    fn accents_are_dropped_and_case_folded() {
        assert_eq!(clean("Amélie"), "amelie");
        assert_eq!(clean("Straße Œuvre"), "strasse oeuvre");
    }

    /// Whitespace-only input is returned as given, like Jellyfin.
    #[test]
    fn whitespace_only_is_unchanged() {
        assert_eq!(clean("  "), "  ");
    }
}
