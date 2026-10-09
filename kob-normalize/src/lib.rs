//! Canonical normalization for scene-style release / pattern matching.
//!
//! Scene release titles use dots as universal separators, drop or replace
//! punctuation that metadata sources (IMDB, TMDB) carry verbatim, and strip
//! diacritics. This crate exposes a single canonical form plus two predicates
//! that hide the scene's apostrophe/hyphen ambiguity from callers:
//!
//! - [`canonical`] — lowercase, diacritic-free, smart-quote-normalized,
//!   ampersand expanded to " and ", separators collapsed to single spaces.
//!   Apostrophes are **dropped** ("Grey's" → "greys") and hyphens act as
//!   **separators** ("Spider-Man" → "spider man", "Author-Title" → two
//!   tokens), matching scene filename convention.
//! - [`contains_pattern`] — substring match that tries the apostrophe/hyphen
//!   variants on the pattern side so "Spider-Man" still matches "SpiderMan".
//! - [`titles_similar`] — token-overlap similarity with the same variant
//!   expansion on the wanted side, plus a prefix-word guard.
//!
//! Callers should reach for [`canonical`] for tokenization and the predicates
//! for yes/no questions. The internal variant expansion isn't exposed.

use unicode_normalization::UnicodeNormalization;

/// Apply the unambiguous transforms (lowercase, diacritics, smart quotes,
/// ampersand, non-ambiguous separators → space). Apostrophes and hyphens are
/// left in place for the caller to resolve.
fn base_transforms(s: &str) -> String {
    let lowered = s.to_lowercase();
    let stripped: String = lowered
        .nfd()
        .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
        .collect();
    let quotes_normalized = stripped
        .replace(['\u{2019}', '\u{2018}'], "'")
        .replace(['\u{201C}', '\u{201D}'], "\"");
    let and_expanded = quotes_normalized.replace('&', " and ");
    and_expanded.replace(['.', '_', ',', '!', '?', ':', ';', '/', '\\', '"'], " ")
}

fn collapse_whitespace(s: String) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The canonical form. Apostrophes are dropped (so "Grey's" → "greys",
/// exact-matching a release's "greys" token). Hyphens become spaces (so
/// "Author-Title" tokenizes as two separate words, matching scene convention).
/// Hyphenated names like "Spider-Man" lose the hyphen too ("spider man") —
/// use [`contains_pattern`] for the case where a release wrote "SpiderMan".
pub fn canonical(s: &str) -> String {
    let base = base_transforms(s);
    collapse_whitespace(base.replace('\'', "").replace('-', " "))
}

/// Pattern-side variants for substring/token matching against scene releases.
/// Scene releases handle apostrophes and hyphens two ways each — dropped
/// ("Margos", "SpiderMan") or as a separator that becomes a dot/space
/// ("Margo.s", "Spider.Man"). We try all four combinations on the pattern
/// so either release shape matches. Up to four deduplicated forms.
fn pattern_forms(s: &str) -> Vec<String> {
    let base = base_transforms(s);
    let mut out = Vec::with_capacity(4);
    for apos in ["", " "] {
        for hyphen in ["", " "] {
            let v = collapse_whitespace(base.replace('\'', apos).replace('-', hyphen));
            if !out.contains(&v) {
                out.push(v);
            }
        }
    }
    out
}

/// True iff `pattern` appears as a substring in the canonical form of `text`
/// under scene-release punctuation ambiguity. Use for pattern-vs-release-title
/// matching — the apostrophe-drop and hyphen-variant expansion happen inside.
pub fn contains_pattern(text: &str, pattern: &str) -> bool {
    let text_canonical = canonical(text);
    pattern_forms(pattern)
        .iter()
        .filter(|p| !p.is_empty())
        .any(|p| text_canonical.contains(p))
}

/// True iff `wanted` and `parsed` share enough tokens to be the same title.
/// Uses the canonical form on the parsed side (scene convention: hyphen is a
/// separator) and tries every apostrophe/hyphen variant on the wanted side.
///
/// Requires strictly more than half of the shorter title's tokens to match.
/// When parsed is longer and all wanted tokens appear consecutively, any
/// un-matched words *before* the match are treated as a distinct-show signal
/// and reject (e.g. "Paradise" vs "Hells Paradise").
pub fn titles_similar(wanted: &str, parsed: &str) -> bool {
    let parsed_tokens = tokens_filtered(&canonical(parsed));
    for w_form in pattern_forms(wanted) {
        let w_tokens = tokens_filtered(&w_form);
        if titles_similar_tokens(&w_tokens, &parsed_tokens) {
            return true;
        }
    }
    false
}

fn tokens_filtered(s: &str) -> Vec<String> {
    s.split_whitespace()
        .filter(|t| t.len() >= 2)
        .map(String::from)
        .collect()
}

fn titles_similar_tokens(wanted: &[String], parsed: &[String]) -> bool {
    if wanted.is_empty() || parsed.is_empty() {
        return true; // can't compare, don't reject
    }
    let (shorter, longer) = if wanted.len() <= parsed.len() {
        (wanted, parsed)
    } else {
        (parsed, wanted)
    };
    let overlap = shorter
        .iter()
        .filter(|t| longer.iter().any(|lt| lt == *t))
        .count();
    if overlap * 2 <= shorter.len() {
        return false;
    }
    // Parsed side: unmatched prefix words indicate a different show
    // ("Hells Paradise" ≠ "Paradise"). Wanted side having extra prefix words
    // is fine (release may drop articles — "Flash" matches "The Flash").
    let parsed_is_longer = parsed.len() > wanted.len();
    if parsed_is_longer
        && overlap == shorter.len()
        && longer.len() > shorter.len()
        && let Some(first_short) = shorter.first()
        && let Some(pos) = longer.iter().position(|t| t == first_short)
    {
        let prefix_extra = longer[..pos]
            .iter()
            .filter(|t| !shorter.contains(t))
            .count();
        if prefix_extra > 0 {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_lowercases_and_spaces_separators() {
        assert_eq!(canonical("Black.Rabbit.S01E01"), "black rabbit s01e01");
    }

    #[test]
    fn canonical_strips_diacritics() {
        assert_eq!(canonical("Pokémon"), "pokemon");
        assert_eq!(canonical("Café"), "cafe");
        assert_eq!(canonical("Jalapeño"), "jalapeno");
        assert_eq!(canonical("Über"), "uber");
    }

    #[test]
    fn canonical_normalizes_smart_quotes() {
        // \u{2019}/\u{2018} fold to '. Then ' is dropped in canonical form.
        assert_eq!(canonical("Margo\u{2019}s"), "margos");
        assert_eq!(canonical("\u{2018}Allo"), "allo");
    }

    #[test]
    fn canonical_expands_ampersand() {
        assert_eq!(canonical("Law & Order"), "law and order");
    }

    #[test]
    fn canonical_drops_apostrophe_and_spaces_hyphen() {
        // Apostrophe → dropped (matches scene "Margos" convention).
        // Hyphen → space (matches scene separator convention for filenames).
        assert_eq!(canonical("Grey's Anatomy"), "greys anatomy");
        assert_eq!(canonical("Spider-Man"), "spider man");
        assert_eq!(canonical("Artist_Album_Name"), "artist album name");
        assert_eq!(
            canonical("Brandon.Sanderson-Mistborn.The.Final.Empire"),
            "brandon sanderson mistborn the final empire"
        );
    }

    #[test]
    fn canonical_handles_slash_and_backslash() {
        assert_eq!(canonical("Face/Off"), "face off");
        assert_eq!(canonical(r"path\to\file"), "path to file");
    }

    #[test]
    fn contains_pattern_apostrophe_dropped_release() {
        assert!(contains_pattern(
            "margos.got.money.troubles.s01e01.1080p",
            "Margo's Got Money Troubles"
        ));
    }

    #[test]
    fn contains_pattern_apostrophe_as_separator_release() {
        assert!(contains_pattern(
            "margo.s.got.money.troubles.s01e01.1080p",
            "Margo's Got Money Troubles"
        ));
    }

    #[test]
    fn contains_pattern_smart_apostrophe() {
        assert!(contains_pattern(
            "margos.got.money.troubles.s01e01.1080p",
            "Margo\u{2019}s Got Money Troubles"
        ));
    }

    #[test]
    fn contains_pattern_hyphen_dropped_release() {
        assert!(contains_pattern(
            "spiderman.homecoming.2017.1080p",
            "Spider-Man"
        ));
    }

    #[test]
    fn contains_pattern_hyphen_as_separator_release() {
        assert!(contains_pattern(
            "spider.man.no.way.home.2021.1080p",
            "Spider-Man"
        ));
    }

    #[test]
    fn contains_pattern_hyphen_preserved_release() {
        assert!(contains_pattern(
            "spider-man.across.the.spider-verse.2023.2160p",
            "Spider-Man"
        ));
    }

    #[test]
    fn contains_pattern_diacritics_stripped_release() {
        assert!(contains_pattern(
            "pokemon.s01e01.indigo.league.1080p",
            "Pokémon"
        ));
    }

    #[test]
    fn contains_pattern_diacritics_in_release_only() {
        assert!(contains_pattern(
            "pokémon.s01e01.indigo.league.1080p",
            "Pokemon"
        ));
    }

    #[test]
    fn contains_pattern_underscore_separator() {
        assert!(contains_pattern(
            "radiohead_ok_computer_1997_flac",
            "Radiohead OK Computer"
        ));
    }

    #[test]
    fn contains_pattern_ampersand_vs_and() {
        assert!(contains_pattern(
            "law.and.order.s08e09.1080p",
            "Law & Order"
        ));
        assert!(contains_pattern("law.&.order.s08e09.720p", "Law and Order"));
    }

    #[test]
    fn contains_pattern_rejects_unrelated() {
        assert!(!contains_pattern(
            "breaking.bad.s01e01.1080p",
            "Stranger Things"
        ));
    }

    #[test]
    fn contains_pattern_empty_pattern_is_false() {
        assert!(!contains_pattern("anything", ""));
    }

    #[test]
    fn titles_similar_apostrophe_dropped_in_release() {
        assert!(titles_similar("Grey's Anatomy", "Greys Anatomy"));
    }

    #[test]
    fn titles_similar_detects_prefix_word_as_different_show() {
        // "Paradise" ≠ "Hells Paradise" — the extra word marks a different show.
        assert!(!titles_similar("Paradise", "Hells Paradise"));
        assert!(!titles_similar("The Good Doctor", "Good Trouble"));
    }

    #[test]
    fn titles_similar_tolerates_article_drops_on_release_side() {
        // Scene releases often drop articles; wanted with article matches.
        assert!(titles_similar("The Flash", "Flash"));
    }

    #[test]
    fn titles_similar_rejects_release_side_extra_prefix() {
        // Release has extra leading words → different show.
        assert!(!titles_similar("The Matrix", "Enter The Matrix"));
    }

    #[test]
    fn titles_similar_with_author_prefixed_release() {
        // `Author-Title` style release must not spuriously match the title alone.
        assert!(!titles_similar(
            "Name of the Wind",
            "Patrick Rothfuss Name of the Wind"
        ));
    }
}
