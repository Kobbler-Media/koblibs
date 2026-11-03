//! Native ebook conversion pipeline.
//!
//! Source format → EPUB via [boko], in-process. EPUB → KEPUB via a homegrown
//! kepubifier (koboSpan injection, body div wrapping, OPF cover-image fix).
//!
//! Designed to replace the Calibre `ebook-convert` shell-out for formats boko
//! supports (EPUB, MOBI, AZW3, KFX) — call sites should fall back to Calibre
//! for the long-tail formats ([`native_supports`] returns `false` for those).

mod fixed_layout;
mod image_size;
mod kepub;
mod meta;
pub mod reader_pack;
mod simple_css;

pub use meta::{Metadata, read_cover_bytes, read_metadata};

use std::io::Cursor;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EbookError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("boko: {0}")]
    Boko(#[from] boko::Error),
    #[error("zip: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("xml: {0}")]
    Xml(#[from] quick_xml::Error),
    #[error("xml attr: {0}")]
    XmlAttr(#[from] quick_xml::events::attributes::AttrError),
    #[error("source format not supported by native pipeline: {0}")]
    UnsupportedSource(String),
    #[error("epub structure: {0}")]
    EpubStructure(String),
}

pub type Result<T> = std::result::Result<T, EbookError>;

/// Whether the file extension is something the native (boko) pipeline can read.
/// Returns false for PDF/FB2/RTF/TXT/HTML/DOCX/LIT/PDB — caller should keep the
/// Calibre fallback for those.
pub fn native_supports(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|s| s.to_str())
            .map(str::to_lowercase)
            .as_deref(),
        Some("epub" | "kepub" | "mobi" | "azw" | "azw3" | "kfx"),
    )
}

/// Convert any boko-supported source file to EPUB bytes (in-memory).
pub fn convert_to_epub_bytes(input: &Path) -> Result<Vec<u8>> {
    if !native_supports(input) {
        return Err(EbookError::UnsupportedSource(
            input
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("?")
                .to_string(),
        ));
    }
    use boko::Exporter as _;
    let book = boko::Book::open(input)?;
    let mut buf = Cursor::new(Vec::new());
    boko::EpubExporter::new().export(&book, &mut buf)?;
    let epub = buf.into_inner();

    // boko exports a KF8 fixed-layout book's pages and stylesheet but not the
    // EXTH header that said the book was fixed-layout at all, so the result
    // reads as reflowable and every page draws at its literal pixel size. See
    // [`fixed_layout`].
    match read_fixed_layout(input) {
        Some(layout) if !fixed_layout::already_declared(&epub) => {
            fixed_layout::apply(&epub, &layout)
        }
        _ => Ok(epub),
    }
}

/// The fixed-layout declaration in a KF8 source file, if it makes one.
///
/// Reads a prefix rather than the whole book: the EXTH header lives in the
/// container's first record, and these files run to tens of megabytes.
fn read_fixed_layout(input: &Path) -> Option<fixed_layout::FixedLayout> {
    use std::io::Read as _;

    let ext = input
        .extension()
        .and_then(|s| s.to_str())
        .map(str::to_lowercase);
    if !matches!(ext.as_deref(), Some("mobi" | "azw" | "azw3")) {
        return None;
    }

    let mut prefix = Vec::new();
    std::fs::File::open(input)
        .ok()?
        .take(EXTH_PREFIX_BYTES)
        .read_to_end(&mut prefix)
        .ok()?;
    fixed_layout::from_kf8(&prefix)
}

/// How much of a KF8 file to read looking for its EXTH header. The header sits
/// in the first PalmDB record, a few hundred bytes in.
const EXTH_PREFIX_BYTES: u64 = 256 * 1024;

/// Whether a file we produced lost the fixed layout its source declared.
///
/// A book converted before [`fixed_layout`] existed looks perfectly healthy —
/// nothing is missing and nothing is malformed — while rendering every page at
/// a fraction of the screen. Nothing else would ever find it, so this gives a
/// caller a way to ask.
///
/// False for a reflowable source, and for a produced file that declares the
/// layout already. Takes the source's bytes rather than its path so a caller
/// that has already read the file does not read it twice; a source that is not
/// a KF8 container answers `false` without being parsed.
pub fn fixed_layout_lost(source: &[u8], produced: &[u8]) -> bool {
    fixed_layout::from_kf8(source).is_some() && !fixed_layout::already_declared(produced)
}

/// Convert any boko-supported source file straight to KEPUB bytes. Uses an
/// ephemeral in-memory EPUB as the bridge between boko's exporter and the
/// kepubifier — no disk intermediate. EPUB inputs skip the boko round-trip.
pub fn convert_to_kepub_bytes(input: &Path) -> Result<Vec<u8>> {
    let ext = input
        .extension()
        .and_then(|s| s.to_str())
        .map(str::to_lowercase);
    let epub_bytes = match ext.as_deref() {
        Some("epub" | "kepub") => std::fs::read(input)?,
        _ => convert_to_epub_bytes(input)?,
    };
    kepubify_bytes(&epub_bytes)
}

/// Apply the Kobo KEPUB transformation to an existing EPUB byte stream.
pub fn kepubify_bytes(epub: &[u8]) -> Result<Vec<u8>> {
    kepub::transform_epub(epub)
}

/// Which revision of the KEPUB transform is current.
///
/// Stamped into every KEPUB this crate writes and readable back with
/// [`kepub_generation`], so a caller can find the files an older transform
/// produced and re-make them. Without it, a KEPUB that already exists looks
/// finished no matter how much the transform has moved on, and improvements
/// only ever reach books converted after the change.
///
/// **Bump this whenever the transform's output changes in a way worth
/// re-converting for.** Re-conversion is not free downstream — a Kobo
/// re-downloads the book and may lose its reading position — so a change that
/// only affects new conversions, or that no reader would notice, does not earn
/// a bump.
///
/// - `1` — koboSpans, the body div wrapper, the OPF cover-image fix.
/// - `2` — full-page scaling for documents that are a single image.
/// - `3` — Kindle fixed-layout pages: the CSS background becomes a scaling
///   SVG and the overlay text moves into a hidden wrapper.
pub const KEPUB_GENERATION: u32 = 3;

/// The generation stamp on an existing KEPUB, or `0` if it carries none —
/// which means it predates stamping, or another tool wrote it. Either way it is
/// older than anything this crate produces now.
pub fn kepub_generation(kepub: &[u8]) -> u32 {
    kepub::read_generation(kepub)
}

#[cfg(test)]
mod smoke {
    //! Smoke tests against real EPUB/MOBI files. Gated on `KOB_EBOOK_FIXTURE_DIR` pointing at
    //! a directory of fixtures, so they don't run in CI and carry no path specific to any one
    //! machine.
    use super::*;
    use std::path::PathBuf;

    fn fixture_root() -> Option<PathBuf> {
        std::env::var("KOB_EBOOK_FIXTURE_DIR")
            .ok()
            .map(PathBuf::from)
    }

    fn assert_kepub_has_kobospans(bytes: &[u8]) {
        use std::io::Read;
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("open kepub");
        let mut saw_kobo_span = false;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).expect("zip entry");
            let name = entry.name().expect("utf-8 name").into_owned();
            if !name.ends_with(".html") && !name.ends_with(".xhtml") {
                continue;
            }
            let mut content = String::new();
            entry.read_to_string(&mut content).expect("read entry");
            if content.contains("koboSpan") {
                saw_kobo_span = true;
                break;
            }
        }
        assert!(saw_kobo_span, "no XHTML in the kepub contains koboSpan");
    }

    #[test]
    fn epub_roundtrips_to_kepub() {
        let Some(root) = fixture_root() else { return };
        let epub = root
            .join("Joseph.Heller-Catch-22.02.Closing.Time.(v5.0).(epub.mobi)")
            .join("Joseph Heller - Catch-22 02 Closing Time (retail).epub");
        let bytes = std::fs::read(&epub).expect("read epub");
        let kepub = kepubify_bytes(&bytes).expect("kepubify");
        std::fs::write("/tmp/closing_time.kepub.epub", &kepub).expect("write");
        assert!(!kepub.is_empty());
        assert_kepub_has_kobospans(&kepub);
    }

    #[test]
    fn mobi_converts_to_epub_then_kepub() {
        let Some(root) = fixture_root() else { return };
        let mobi = root
            .join("Joseph.Heller-Catch-22.02.Closing.Time.(v5.0).(epub.mobi)")
            .join("Joseph Heller - Catch-22 02 Closing Time (v5.0).mobi");
        let epub = convert_to_epub_bytes(&mobi).expect("mobi->epub");
        assert!(epub.starts_with(b"PK"), "epub should be a zip");
        let kepub = kepubify_bytes(&epub).expect("epub->kepub");
        assert_kepub_has_kobospans(&kepub);
    }

    #[test]
    fn mobi_streams_directly_to_kepub() {
        let Some(root) = fixture_root() else { return };
        let mobi = root
            .join("Stephen.King-11.22.63.(v5.0)")
            .join("Stephen King - 11_22_63 (v5.0).mobi");
        let kepub = convert_to_kepub_bytes(&mobi).expect("mobi->kepub");
        assert!(kepub.starts_with(b"PK"));
        assert_kepub_has_kobospans(&kepub);
    }

    #[test]
    fn epub_metadata_extraction() {
        let Some(root) = fixture_root() else { return };
        let epub = root
            .join("Joseph.Heller-Catch-22.02.Closing.Time.(v5.0).(epub.mobi)")
            .join("Joseph Heller - Catch-22 02 Closing Time (retail).epub");
        let m = read_metadata(&epub).expect("read epub meta");
        let title = m.title.as_deref().unwrap_or("");
        assert!(
            title.contains("Closing Time"),
            "expected Closing Time, got {title:?}"
        );
        assert!(
            m.authors.iter().any(|a| a.contains("Heller")),
            "expected Heller author, got {:?}",
            m.authors
        );
    }

    #[test]
    fn mobi_metadata_extraction() {
        let Some(root) = fixture_root() else { return };
        let mobi = root
            .join("Stephen.King-11.22.63.(v5.0)")
            .join("Stephen King - 11_22_63 (v5.0).mobi");
        let m = read_metadata(&mobi).expect("read mobi meta");
        let title = m.title.as_deref().unwrap_or("");
        assert!(title.contains("11/22/63"), "got {title:?}");
        assert!(
            m.authors.iter().any(|a| a.contains("King")),
            "expected King author, got {:?}",
            m.authors
        );
    }

    #[test]
    fn epub_cover_extraction() {
        let Some(root) = fixture_root() else { return };
        let epub = root
            .join("Joseph.Heller-Catch-22.02.Closing.Time.(v5.0).(epub.mobi)")
            .join("Joseph Heller - Catch-22 02 Closing Time (retail).epub");
        let cover = read_cover_bytes(&epub).expect("read cover");
        let bytes = cover.expect("cover should exist");
        assert!(bytes.len() > 1024, "cover too small: {}", bytes.len());
        // Magic bytes: JPEG (FF D8 FF) or PNG (89 50 4E 47).
        let starts_jpeg = bytes.starts_with(&[0xFF, 0xD8, 0xFF]);
        let starts_png = bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]);
        assert!(
            starts_jpeg || starts_png,
            "cover not JPEG/PNG: first bytes {:02x?}",
            &bytes[..bytes.len().min(8)]
        );
    }

    #[test]
    fn timmy_failure_azw3_pages_become_svgs() {
        // The Timmy Failure series is Kindle fixed layout: every page is a
        // CSS background image with the text floating on top of it. Before
        // the fifth transform step, kepubifying this book dropped every
        // illustration, because the background never appears in the markup.
        let Some(root) = fixture_root() else { return };
        let azw3 = root
            .join("Stephan Pastis/Timmy Failure/06 - The Cat Stole My Pants")
            .join("The Cat Stole My Pants.azw3");
        let kepub = convert_to_kepub_bytes(&azw3).expect("azw3->kepub");
        use std::io::Read;
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(&kepub)).expect("open kepub");
        let mut svg_docs = 0usize;
        let mut text_page: Option<String> = None;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).expect("zip entry");
            let name = entry.name().expect("utf-8 name").into_owned();
            if !name.ends_with(".html") && !name.ends_with(".xhtml") {
                continue;
            }
            let mut content = String::new();
            entry.read_to_string(&mut content).expect("read entry");
            if !content.contains("kobo-fullpage") {
                continue;
            }
            svg_docs += 1;
            // Keep one page that carries text, to prove the hidden wrapper
            // kept it for search and TTS.
            if text_page.is_none() && content.contains("koboSpan") {
                text_page = Some(content);
            }
        }
        assert!(
            svg_docs > 100,
            "expected the whole book rewritten, got {svg_docs} svg pages"
        );
        let page = text_page.expect("no rewritten page carried text");
        assert!(
            page.contains(r#"viewBox="0 0 600 914""#),
            "svg missing the page viewBox: {page}"
        );
        assert!(
            page.contains(r#"<div class="kobo-page-text" hidden="hidden">"#),
            "text not kept in a hidden wrapper: {page}"
        );
        assert!(
            !page.contains(r#"id="KF_"#) && !page.contains(r#"class="lpage"#),
            "background identity not stripped, the CSS would draw the page twice: {page}"
        );
    }
}
