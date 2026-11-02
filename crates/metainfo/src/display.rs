//! The characters stripped from untrusted text before it's shown or kept as
//! a display name (feed titles and tags, digest fields).

/// Unicode general category Cf (format), as of Unicode 15.1. These are
/// invisible, and some reorder text (the bidi controls), so a name that
/// carries them can look like a different name or split a filter word.
/// Sorted and disjoint; checked against `unicode-properties` in the tests.
const FORMAT: [(char, char); 21] = [
    ('\u{00AD}', '\u{00AD}'),
    ('\u{0600}', '\u{0605}'),
    ('\u{061C}', '\u{061C}'),
    ('\u{06DD}', '\u{06DD}'),
    ('\u{070F}', '\u{070F}'),
    ('\u{0890}', '\u{0891}'),
    ('\u{08E2}', '\u{08E2}'),
    ('\u{180E}', '\u{180E}'),
    ('\u{200B}', '\u{200F}'),
    ('\u{202A}', '\u{202E}'),
    ('\u{2060}', '\u{2064}'),
    ('\u{2066}', '\u{206F}'),
    ('\u{FEFF}', '\u{FEFF}'),
    ('\u{FFF9}', '\u{FFFB}'),
    ('\u{110BD}', '\u{110BD}'),
    ('\u{110CD}', '\u{110CD}'),
    ('\u{13430}', '\u{1343F}'),
    ('\u{1BCA0}', '\u{1BCA3}'),
    ('\u{1D173}', '\u{1D17A}'),
    ('\u{E0001}', '\u{E0001}'),
    ('\u{E0020}', '\u{E007F}'),
];

/// Whether `c` is removed from untrusted display text: a control (C0, DEL,
/// C1), a format character (category Cf, which includes the zero-width
/// characters, U+FEFF, the soft hyphen and the bidi controls), a line or
/// paragraph separator (U+2028, U+2029), or a tag character
/// (U+E0000–E007F).
///
/// Joiners go too, so an emoji sequence joined by U+200D falls apart into
/// its parts. That is the accepted cost of a name that can't hide text.
pub fn hidden_char(c: char) -> bool {
    if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}' | '\u{E0000}'..='\u{E007F}') {
        return true;
    }
    if c < '\u{00AD}' {
        return false;
    }
    FORMAT
        .binary_search_by(|&(lo, hi)| {
            if hi < c {
                std::cmp::Ordering::Less
            } else if lo > c {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_properties::{GeneralCategory, UnicodeGeneralCategory};

    #[test]
    fn table_is_sorted_and_disjoint() {
        for w in FORMAT.windows(2) {
            assert!(w[0].0 <= w[0].1 && w[0].1 < w[1].0, "{w:?}");
        }
    }

    /// Every Cf character the reference knows is stripped, and nothing is
    /// stripped that isn't a control, Cf, a separator or a tag.
    #[test]
    fn matches_the_reference() {
        for c in (0..=0x10_FFFF).filter_map(char::from_u32) {
            let cf = c.general_category() == GeneralCategory::Format;
            if cf {
                assert!(hidden_char(c), "U+{:04X} is Cf", c as u32);
            }
            let expected = cf
                || c.is_control()
                || matches!(c, '\u{2028}' | '\u{2029}' | '\u{E0000}'..='\u{E007F}');
            assert_eq!(hidden_char(c), expected, "U+{:04X}", c as u32);
        }
    }

    #[test]
    fn each_class() {
        let stripped = [
            ('\u{0}', "C0"),
            ('\u{7F}', "DEL"),
            ('\u{85}', "C1"),
            ('\u{AD}', "soft hyphen"),
            ('\u{200B}', "zero-width space"),
            ('\u{200C}', "ZWNJ"),
            ('\u{200D}', "ZWJ"),
            ('\u{200E}', "LRM"),
            ('\u{202E}', "RLO"),
            ('\u{2060}', "word joiner"),
            ('\u{2064}', "invisible plus"),
            ('\u{2066}', "LRI"),
            ('\u{2069}', "PDI"),
            ('\u{061C}', "ALM"),
            ('\u{FEFF}', "BOM"),
            ('\u{2028}', "line separator"),
            ('\u{2029}', "paragraph separator"),
            ('\u{E0000}', "tag (unassigned)"),
            ('\u{E0041}', "tag A"),
            ('\u{E007F}', "cancel tag"),
        ];
        for (c, what) in stripped {
            assert!(hidden_char(c), "{what}");
        }
        for c in [
            'a',
            ' ',
            '\u{A0}',
            'é',
            '\u{301}',
            '中',
            '😀',
            '\u{FE0F}',
            '\u{E0080}',
        ] {
            assert!(!hidden_char(c), "U+{:04X}", c as u32);
        }
    }
}
