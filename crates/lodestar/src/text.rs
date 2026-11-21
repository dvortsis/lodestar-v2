//! Text classes shared across the crate, written out from Unicode's own
//! tables so they don't depend on a platform or a crate's version.

/// Unicode's Default_Ignorable_Code_Point, as the Default_Ignorable_Code_Point
/// section of DerivedCoreProperties.txt lists it (Unicode 15; Unicode 16's
/// is the same): what renders as nothing, Cf or not. Beyond Cf, the
/// combining grapheme joiner, the Hangul fillers, the Khmer inherent
/// vowels, the Mongolian free variation selectors, the variation
/// selectors, the reserved U+2065 and U+FFF0–FFF8, and plane 14.
pub(crate) fn default_ignorable(c: char) -> bool {
    matches!(
        c,
        '\u{00ad}'
            | '\u{034f}'
            | '\u{061c}'
            | '\u{115f}'..='\u{1160}'
            | '\u{17b4}'..='\u{17b5}'
            | '\u{180b}'..='\u{180f}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}'
            | '\u{3164}'
            | '\u{fe00}'..='\u{fe0f}'
            | '\u{feff}'
            | '\u{ffa0}'
            | '\u{fff0}'..='\u{fff8}'
            | '\u{1bca0}'..='\u{1bca3}'
            | '\u{1d173}'..='\u{1d17a}'
            | '\u{e0000}'..='\u{e0fff}'
    )
}

#[cfg(test)]
mod tests {
    use super::default_ignorable;

    /// The table's edges and a few ordinary characters around them. Its
    /// exact equality with DerivedCoreProperties.txt over all of `char` is
    /// `windows::acl`'s `quoted_escapes_every_default_ignorable`, and its use
    /// in shortcut keys is `settings::preferences`'.
    #[test]
    fn edges_are_in_and_neighbours_out() {
        for c in [
            '\u{00ad}',
            '\u{034f}',
            '\u{200b}',
            '\u{2065}',
            '\u{fff8}',
            '\u{1bca0}',
            '\u{1d173}',
            '\u{1d17a}',
            '\u{e0000}',
            '\u{e0080}',
            '\u{e0fff}',
        ] {
            assert!(default_ignorable(c), "{:?}", c);
        }
        for c in [
            'a',
            ' ',
            '\u{0300}',
            '\u{fff9}',
            '\u{1d172}',
            '\u{1d17b}',
            '\u{e1000}',
        ] {
            assert!(!default_ignorable(c), "{:?}", c);
        }
    }
}
