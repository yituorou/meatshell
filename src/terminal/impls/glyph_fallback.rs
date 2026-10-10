//! Per-glyph font fallback for the terminal grid (#461).
//!
//! Slint draws each terminal span with one `font-family` and does not reliably
//! fall back to another font for symbols the terminal font lacks: `➜` (U+279C)
//! in a custom PS1, `✔`/`✗` status marks and similar dingbats rendered as
//! empty cells. We know the terminal font's cmap, so any grapheme it cannot
//! draw is split into its own span and drawn with an installed system font
//! that can. Text the terminal font covers is untouched, so alignment of
//! ordinary output never changes.

use std::sync::{Arc, RwLock};

use slint::SharedString;

include!(concat!(env!("OUT_DIR"), "/bundled_font_coverage.rs"));

/// Family name of the font bundled with the app (renamed Cascadia Mono).
pub(crate) const BUNDLED_TERM_FONT: &str = "Meatshell Mono";

/// Unicode coverage of one font as sorted, inclusive codepoint ranges.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Coverage {
    ranges: Vec<(u32, u32)>,
}

impl Coverage {
    pub(crate) fn bundled() -> Self {
        Self {
            ranges: BUNDLED_FONT_COVERAGE.to_vec(),
        }
    }

    pub(crate) fn from_codepoints(mut codepoints: Vec<u32>) -> Self {
        codepoints.sort_unstable();
        codepoints.dedup();
        let mut ranges: Vec<(u32, u32)> = Vec::new();
        for cp in codepoints {
            match ranges.last_mut() {
                Some((_, end)) if *end + 1 == cp => *end = cp,
                _ => ranges.push((cp, cp)),
            }
        }
        Self { ranges }
    }

    /// Coverage of face `index` in a font file; `None` if it can't be parsed.
    pub(crate) fn from_font_data(data: &[u8], index: u32) -> Option<Self> {
        let face = ttf_parser::Face::parse(data, index).ok()?;
        let cmap = face.tables().cmap?;
        let mut codepoints = Vec::new();
        for subtable in cmap.subtables {
            if !subtable.is_unicode() {
                continue;
            }
            subtable.codepoints(|cp| {
                if subtable.glyph_index(cp).is_some_and(|glyph| glyph.0 != 0) {
                    codepoints.push(cp);
                }
            });
        }
        (!codepoints.is_empty()).then(|| Self::from_codepoints(codepoints))
    }

    pub(crate) fn contains(&self, ch: char) -> bool {
        let cp = ch as u32;
        self.ranges
            .binary_search_by(|&(start, end)| {
                if end < cp {
                    std::cmp::Ordering::Less
                } else if start > cp {
                    std::cmp::Ordering::Greater
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .is_ok()
    }
}

/// The terminal font's coverage plus ordered system fallbacks.
pub(crate) struct GlyphFallback {
    primary: Coverage,
    fallbacks: Vec<(SharedString, Coverage)>,
}

impl GlyphFallback {
    pub(crate) fn new(primary: Coverage, fallbacks: Vec<(SharedString, Coverage)>) -> Self {
        Self { primary, fallbacks }
    }

    /// Family to draw `grapheme` with when the terminal font lacks its base
    /// character, or `None` to keep the terminal font. ASCII, whitespace and
    /// controls never fall back; neither does a glyph no fallback covers
    /// (drawing it with the terminal font changes nothing).
    pub(crate) fn font_for(&self, grapheme: &str) -> Option<&SharedString> {
        let base = grapheme.chars().next()?;
        if base.is_ascii() || base.is_whitespace() || base.is_control() {
            return None;
        }
        if self.primary.contains(base) {
            return None;
        }
        self.fallbacks
            .iter()
            .find(|(_, coverage)| coverage.contains(base))
            .map(|(family, _)| family)
    }
}

static ACTIVE: RwLock<Option<Arc<GlyphFallback>>> = RwLock::new(None);

/// Install the fallback table for the current terminal font. `None` disables
/// fallback (unknown terminal font: we can't tell which glyphs it lacks).
pub(crate) fn install_glyph_fallback(fallback: Option<GlyphFallback>) {
    *ACTIVE.write().unwrap_or_else(|e| e.into_inner()) = fallback.map(Arc::new);
}

pub(crate) fn active_glyph_fallback() -> Option<Arc<GlyphFallback>> {
    ACTIVE.read().unwrap_or_else(|e| e.into_inner()).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_table_matches_known_font_gaps() {
        let bundled = Coverage::bundled();
        for ch in ['A', '→', '❯', '─', '▶', 'λ'] {
            assert!(bundled.contains(ch), "bundled font should cover {ch}");
        }
        // #461: the PS1 arrow and common status dingbats are missing.
        for ch in ['➜', '✔', '✗'] {
            assert!(
                !bundled.contains(ch),
                "bundled font unexpectedly covers {ch}"
            );
        }
    }

    #[test]
    fn routes_only_uncovered_glyphs_to_a_covering_fallback() {
        let symbols = Coverage::from_codepoints(vec!['➜' as u32, '✔' as u32, 'A' as u32]);
        let other = Coverage::from_codepoints(vec!['✗' as u32]);
        let fallback = GlyphFallback::new(
            Coverage::bundled(),
            vec![("Symbols".into(), symbols), ("Other".into(), other)],
        );
        assert_eq!(fallback.font_for("➜").map(|f| f.as_str()), Some("Symbols"));
        assert_eq!(fallback.font_for("✗").map(|f| f.as_str()), Some("Other"));
        assert_eq!(fallback.font_for("A"), None);
        assert_eq!(fallback.font_for("→"), None);
        assert_eq!(fallback.font_for(" "), None);
        // Nothing covers it: keep the terminal font.
        assert_eq!(fallback.font_for("\u{E0B0}"), None);
    }

    #[test]
    fn coverage_merges_adjacent_codepoints() {
        let coverage = Coverage::from_codepoints(vec![5, 3, 4, 10, 4]);
        assert_eq!(coverage.ranges, vec![(3, 5), (10, 10)]);
        assert!(coverage.contains('\u{4}'));
        assert!(!coverage.contains('\u{6}'));
    }
}
