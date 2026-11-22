//! Folding and ranking of the launcher (SPEC 5.2.5). This file holds `fold` for now (the query parser needs it); the
//! tiers, the bilingual keywords and the destructive rule arrive with L1a-4.

/// The base letters of U+00C0..=U+017F, one entry per code point; '\0' keeps the character as it is
/// (the multiplication and division signs, which the calculator reads).
const LATIN: [&str; 192] = [
    // U+00C0..U+00FF
    "a", "a", "a", "a", "a", "a", "ae", "c", "e", "e", "e", "e", "i", "i", "i", "i", //
    "d", "n", "o", "o", "o", "o", "o", "\0", "o", "u", "u", "u", "u", "y", "th", "ss", //
    "a", "a", "a", "a", "a", "a", "ae", "c", "e", "e", "e", "e", "i", "i", "i", "i", //
    "d", "n", "o", "o", "o", "o", "o", "\0", "o", "u", "u", "u", "u", "y", "th", "y", //
    // U+0100..U+017F
    "a", "a", "a", "a", "a", "a", "c", "c", "c", "c", "c", "c", "c", "c", "d", "d", //
    "d", "d", "e", "e", "e", "e", "e", "e", "e", "e", "e", "e", "g", "g", "g", "g", //
    "g", "g", "g", "g", "h", "h", "h", "h", "i", "i", "i", "i", "i", "i", "i", "i", //
    "i", "i", "ij", "ij", "j", "j", "k", "k", "k", "l", "l", "l", "l", "l", "l", "l", //
    "l", "l", "l", "n", "n", "n", "n", "n", "n", "n", "n", "n", "o", "o", "o", "o", //
    "o", "o", "oe", "oe", "r", "r", "r", "r", "r", "r", "s", "s", "s", "s", "s", "s", //
    "s", "s", "t", "t", "t", "t", "t", "t", "u", "u", "u", "u", "u", "u", "u", "u", //
    "u", "u", "u", "u", "w", "w", "y", "y", "y", "z", "z", "z", "z", "z", "z", "s", //
];

/// Appends `s` folded to `out`: lowercase, with the accented Latin letters of U+00C0..=U+017F reduced to their base
/// letters ("Attività" becomes "attivita", the sharp s becomes "ss"). Callers clear `out` first when they reuse it.
pub fn fold(s: &str, out: &mut String) {
    for c in s.chars() {
        let cp = c as u32;
        if (0xC0..=0x17F).contains(&cp) {
            match LATIN.get((cp - 0xC0) as usize) {
                Some(&"\0") | None => out.push(c),
                Some(base) => out.push_str(base),
            }
        } else {
            out.extend(c.to_lowercase());
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn folded(s: &str) -> String {
        let mut out = String::new();
        fold(s, &mut out);
        out
    }

    #[test]
    fn fold_removes_accents_and_case() {
        assert_eq!(folded("Attività"), "attivita");
        assert_eq!(folded("Gestione ATTIVITÀ"), "gestione attivita");
        assert_eq!(folded("Straße"), "strasse");
        assert_eq!(folded("Ærø Łódź Çñ Œ"), "aero lodz cn oe");
        assert_eq!(folded("Ångström"), "angstrom");
        assert_eq!(folded("ÀÈÌÒÙ àèìòù é"), "aeiou aeiou e");
    }

    #[test]
    fn fold_keeps_operators_and_other_scripts() {
        assert_eq!(folded("2 × 3 ÷ 4"), "2 × 3 ÷ 4");
        assert_eq!(folded("ΑΒΓ"), "αβγ");
        assert_eq!(folded("日本語"), "日本語");
        assert_eq!(folded(""), "");
    }

    #[test]
    fn fold_table_covers_the_range_with_ascii() {
        for cp in 0xC0u32..=0x17F {
            let c = char::from_u32(cp).unwrap();
            let f = folded(&c.to_string());
            if cp == 0xD7 || cp == 0xF7 {
                assert_eq!(f, c.to_string());
            } else {
                assert!(!f.is_empty() && f.chars().all(|x| x.is_ascii_lowercase()), "U+{cp:04X} -> {f:?}");
            }
        }
    }
}
