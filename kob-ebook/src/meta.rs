//! Metadata + cover extraction backed by boko. Read-only — write goes
//! elsewhere (Calibre `ebook-meta -i` for now, possibly homegrown later).

use std::path::Path;

use crate::Result;

/// Cross-format metadata pulled from a book file. Mirrors what kobbler
/// historically got from `ebook-meta`. All fields best-effort — boko's parser
/// returns what the source file actually carries, which varies wildly.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Metadata {
    pub title: Option<String>,
    pub authors: Vec<String>,
    /// First ISBN-10 / ISBN-13 found in any identifier field. Best-effort —
    /// boko surfaces only one identifier per book, so EPUBs that bury the
    /// ISBN behind a UUID identifier will return None.
    pub isbn: Option<String>,
    pub publisher: Option<String>,
    pub pubdate: Option<String>,
    pub description: Option<String>,
    pub series: Option<String>,
    pub series_index: Option<f64>,
    pub language: Option<String>,
    pub tags: Vec<String>,
}

/// Open `path` with boko and extract metadata. Returns
/// [`crate::EbookError::UnsupportedSource`] for formats boko can't read.
pub fn read_metadata(path: &Path) -> Result<Metadata> {
    if !crate::native_supports(path) {
        return Err(crate::EbookError::UnsupportedSource(
            path.extension()
                .and_then(|s| s.to_str())
                .unwrap_or("?")
                .to_string(),
        ));
    }
    let book = boko::Book::open(path)?;
    Ok(map_metadata(book.metadata()))
}

/// Open `path` with boko and pull the cover image bytes (if any). Returns
/// `Ok(None)` if the book declares no cover or the cover asset is missing.
///
/// boko stores `metadata.cover_image` as the OPF-relative href, but
/// `load_asset` looks paths up verbatim in the ZIP index. So we resolve by
/// suffix-matching against `list_assets()` — covers anywhere in OEBPS/OPS/
/// ops/ etc. without us needing to re-parse container.xml.
pub fn read_cover_bytes(path: &Path) -> Result<Option<Vec<u8>>> {
    if !crate::native_supports(path) {
        return Err(crate::EbookError::UnsupportedSource(
            path.extension()
                .and_then(|s| s.to_str())
                .unwrap_or("?")
                .to_string(),
        ));
    }
    let book = boko::Book::open(path)?;
    let cover_href = match book.metadata().cover_image.clone() {
        Some(p) if !p.is_empty() => p,
        _ => return Ok(None),
    };

    // Try the literal href first (works for MOBI/AZW3 backends, EPUBs without
    // an OPF subdirectory).
    if let Ok(bytes) = book.load_asset(&cover_href)
        && !bytes.is_empty()
    {
        return Ok(Some(bytes));
    }

    // Fall back to suffix match against the full asset list.
    let cover_href_norm = cover_href.replace('\\', "/");
    let asset = book
        .list_assets()
        .iter()
        .find(|a| a.replace('\\', "/").ends_with(&cover_href_norm));
    let Some(asset) = asset else {
        return Ok(None);
    };
    match book.load_asset(asset) {
        Ok(bytes) if !bytes.is_empty() => Ok(Some(bytes)),
        _ => Ok(None),
    }
}

fn map_metadata(m: &boko::Metadata) -> Metadata {
    Metadata {
        title: opt(&m.title),
        authors: m
            .authors
            .iter()
            .filter(|a| !a.is_empty())
            .cloned()
            .collect(),
        isbn: extract_isbn(&m.identifier),
        publisher: m.publisher.clone(),
        pubdate: m.date.clone(),
        description: m.description.clone(),
        series: m.collection.as_ref().map(|c| c.name.clone()),
        series_index: m.collection.as_ref().and_then(|c| c.position),
        language: opt(&m.language),
        tags: m
            .subjects
            .iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect(),
    }
}

fn opt(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// Pull an ISBN-13 or ISBN-10 out of an arbitrary identifier string. Handles
/// `urn:isbn:9781234567890`, `isbn:9781234567890`, bare digits, and ISBN-10
/// with check-digit `X`. Strips letters/separators/colons before matching, so
/// `isbn:978-1-4516-2728-2` works the same as `9781451627282`.
fn extract_isbn(raw: &str) -> Option<String> {
    if raw.is_empty() {
        return None;
    }

    // Keep digits + ASCII X (uppercased), drop everything else.
    let cleaned: String = raw
        .chars()
        .filter_map(|c| match c {
            '0'..='9' => Some(c),
            'X' | 'x' => Some('X'),
            _ => None,
        })
        .collect();

    // ISBN-13: 13 chars, all digits, starts with 978/979.
    if cleaned.len() == 13
        && cleaned.chars().all(|c| c.is_ascii_digit())
        && (cleaned.starts_with("978") || cleaned.starts_with("979"))
    {
        return Some(cleaned);
    }
    // ISBN-10: 10 chars, first 9 digits, last digit-or-X.
    if cleaned.len() == 10
        && cleaned[..9].chars().all(|c| c.is_ascii_digit())
        && cleaned
            .chars()
            .last()
            .is_some_and(|c| c.is_ascii_digit() || c == 'X')
    {
        return Some(cleaned);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isbn13_from_urn() {
        assert_eq!(
            extract_isbn("urn:isbn:9781451627282"),
            Some("9781451627282".to_string())
        );
    }

    #[test]
    fn isbn13_with_dashes() {
        assert_eq!(
            extract_isbn("isbn:978-1-4516-2728-2"),
            Some("9781451627282".to_string())
        );
    }

    #[test]
    fn isbn13_bare() {
        assert_eq!(
            extract_isbn("9781451627282"),
            Some("9781451627282".to_string())
        );
    }

    #[test]
    fn isbn10_with_x_check_digit() {
        assert_eq!(
            extract_isbn("isbn:0306406152"),
            Some("0306406152".to_string())
        );
        assert_eq!(extract_isbn("043942089X"), Some("043942089X".to_string()));
    }

    #[test]
    fn uuid_returns_none() {
        assert_eq!(
            extract_isbn("urn:uuid:9e8dd05e-87a2-4398-baf4-3209742728fe"),
            None
        );
    }

    #[test]
    fn empty_returns_none() {
        assert_eq!(extract_isbn(""), None);
    }
}
