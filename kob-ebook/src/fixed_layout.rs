//! Carrying a KF8 fixed-layout declaration into the EPUB.
//!
//! Amazon's fixed-layout books — comics, illustrated children's fiction, the
//! Timmy Failure series — say so in the AZW3's EXTH header rather than in the
//! markup:
//!
//! ```text
//! EXTH 122 fixed-layout       = true
//! EXTH 124 orientation-lock   = portrait
//! EXTH 126 original-resolution = 600x914
//! ```
//!
//! boko exports the pages and their stylesheet faithfully and drops all three.
//! What comes out is an EPUB whose every page is a `600px` box of absolutely
//! positioned text over a background image, and whose package file never says
//! the book is pre-paginated. Nothing is missing and nothing is malformed, so
//! no reader complains — they each just treat it as reflowable and draw that
//! box at its literal size. On a 1264px panel a 600px page is a stamp in the
//! middle of the screen, which is exactly what a reader reports as "the images
//! are too small", and no amount of re-converting changes it.
//!
//! The remedy is to say in EPUB 3 what the EXTH header said: `rendition:layout`
//! on the package, and a viewport on every content document. A reader that
//! honours those scales the page to the screen, which is the whole point of
//! declaring a fixed layout.

use std::io::{Cursor, Read, Write};

use quick_xml::Writer;
use quick_xml::events::{BytesStart, BytesText, Event};
use quick_xml::reader::Reader;
use zip::CompressionMethod;
use zip::write::{SimpleFileOptions, ZipWriter};

use crate::Result;

/// The EPUB 3 rendition vocabulary, needed as a `prefix` declaration on the
/// package element for EPUB 3.0 (3.0.1 reserved it, but declaring it costs
/// nothing and older readers want it).
const RENDITION_VOCAB: &str = "rendition: http://www.idpf.org/vocab/rendition/#";

/// A fixed layout, as the source declared it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FixedLayout {
    pub width: u32,
    pub height: u32,
    /// `portrait` or `landscape` when the book locks one, else none.
    pub orientation: Option<String>,
}

/// Read the fixed-layout declaration out of a MOBI/AZW/AZW3 file, if it makes
/// one.
///
/// Returns `None` for a reflowable book, for anything that is not a PalmDB
/// container, and for a fixed-layout book with no usable page size — without
/// real dimensions there is no viewport to write, and a `rendition:layout`
/// with nothing to scale to would be worse than saying nothing at all.
pub(crate) fn from_kf8(source: &[u8]) -> Option<FixedLayout> {
    let exth = read_exth(source)?;

    if exth.get(&122).map(String::as_str) != Some("true") {
        return None;
    }

    let (width, height) = exth.get(&126).and_then(|r| parse_resolution(r))?;
    if width == 0 || height == 0 {
        return None;
    }

    let orientation = exth
        .get(&124)
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| s == "portrait" || s == "landscape");

    Some(FixedLayout {
        width,
        height,
        orientation,
    })
}

/// `600x914` → `(600, 914)`.
fn parse_resolution(raw: &str) -> Option<(u32, u32)> {
    let (w, h) = raw.trim().split_once(['x', 'X'])?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// EXTH records from a PalmDB container's first record, keyed by type.
///
/// Only the string-valued types this module cares about are decoded; the rest
/// are skipped without being interpreted, so a binary record cannot derail the
/// walk.
fn read_exth(source: &[u8]) -> Option<std::collections::HashMap<u32, String>> {
    // PalmDB: record count at 76, then an 8-byte entry per record whose first
    // four bytes are that record's offset into the file.
    let count = u16::from_be_bytes(source.get(76..78)?.try_into().ok()?) as usize;
    if count < 2 {
        return None;
    }
    let start = read_u32(source, 78)? as usize;
    let end = read_u32(source, 86)? as usize;
    let rec0 = source.get(start..end.min(source.len()))?;

    // The MOBI header sits after the 16-byte PalmDOC header.
    if rec0.get(16..20)? != b"MOBI" {
        return None;
    }
    let mobi_len = read_u32(rec0, 20)? as usize;

    // Bit 0x40 of the flags at MOBI+0x70 says an EXTH header follows the
    // MOBI header.
    if read_u32(rec0, 16 + 0x70)? & 0x40 == 0 {
        return None;
    }

    let exth = rec0.get(16 + mobi_len..)?;
    if exth.get(0..4)? != b"EXTH" {
        return None;
    }
    let record_count = read_u32(exth, 8)? as usize;

    let mut out = std::collections::HashMap::new();
    let mut pos = 12usize;
    for _ in 0..record_count {
        let kind = read_u32(exth, pos)?;
        let len = read_u32(exth, pos + 4)? as usize;
        // A length that does not clear its own header would not advance the
        // cursor, and the walk would spin here forever.
        if len < 8 {
            break;
        }
        if matches!(kind, 122 | 124 | 126)
            && let Some(value) = exth.get(pos + 8..pos + len)
        {
            out.insert(kind, String::from_utf8_lossy(value).into_owned());
        }
        pos += len;
    }
    Some(out)
}

fn read_u32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// Declare `layout` in an EPUB that does not already declare one.
///
/// Two edits, both additive: `rendition:layout` (plus orientation and spread)
/// on the package's metadata, and a viewport on every content document. Books
/// that already say they are pre-paginated are returned untouched — a
/// publisher's own declaration is better informed than one reconstructed from
/// an EXTH header.
pub(crate) fn apply(epub: &[u8], layout: &FixedLayout) -> Result<Vec<u8>> {
    let mut archive = zip::ZipArchive::new(Cursor::new(epub))?;
    let opf_path = crate::kepub::read_opf_path(&mut archive)?;
    let xhtml_paths = crate::kepub::read_xhtml_manifest(&mut archive, &opf_path)?;

    let mut out = ZipWriter::new(Cursor::new(Vec::new()));
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let deflate = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    out.start_file("mimetype", stored)?;
    out.write_all(b"application/epub+zip")?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let name = file.name()?.into_owned();
        if name == "mimetype" || file.is_dir() {
            continue;
        }
        let mut buf = Vec::with_capacity(file.size() as usize);
        file.read_to_end(&mut buf)?;
        drop(file);

        let bytes = if name == opf_path {
            declare_in_opf(&buf, layout)?
        } else if xhtml_paths.contains(&name) {
            declare_viewport(&buf, layout)?
        } else {
            buf
        };

        out.start_file(&name, deflate)?;
        out.write_all(&bytes)?;
    }

    Ok(out.finish()?.into_inner())
}

/// Whether an EPUB already says it is pre-paginated, in which case there is
/// nothing for [`apply`] to add.
pub(crate) fn already_declared(epub: &[u8]) -> bool {
    let Ok(mut archive) = zip::ZipArchive::new(Cursor::new(epub)) else {
        return false;
    };
    let Ok(opf_path) = crate::kepub::read_opf_path(&mut archive) else {
        return false;
    };
    let Ok(mut opf) = archive.by_name(&opf_path) else {
        return false;
    };
    let mut buf = Vec::new();
    if opf.read_to_end(&mut buf).is_err() {
        return false;
    }
    contains_pre_paginated(&buf)
}

fn contains_pre_paginated(opf: &[u8]) -> bool {
    opf.windows(b"pre-paginated".len())
        .any(|w| w == b"pre-paginated")
}

/// Add the rendition metadata to the package document.
fn declare_in_opf(input: &[u8], layout: &FixedLayout) -> Result<Vec<u8>> {
    let mut reader = Reader::from_reader(input);
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    let mut buf = Vec::new();

    loop {
        let event = reader.read_event_into(&mut buf)?;
        match &event {
            // The rendition vocabulary has to be in scope for `rendition:layout`
            // to mean anything to a strict reader.
            Event::Start(e) if e.name().as_ref() == "package" => {
                let mut declared = false;
                let mut rebuilt = BytesStart::new("package");
                for attr in e.attributes().flatten() {
                    if attr.key.as_ref() == "prefix" {
                        declared = true;
                        let existing = attr.value.into_owned();
                        let merged = if existing.contains("rendition:") {
                            existing
                        } else {
                            format!("{existing} {RENDITION_VOCAB}")
                        };
                        rebuilt.push_attribute(("prefix", merged.as_str()));
                    } else {
                        rebuilt.push_attribute(attr);
                    }
                }
                if !declared {
                    rebuilt.push_attribute(("prefix", RENDITION_VOCAB));
                }
                writer.write_event(Event::Start(rebuilt))?;
                buf.clear();
                continue;
            }
            // Appended at the end of the metadata block, so it cannot land
            // before `dcterms:modified` or any other element whose position
            // the spec constrains.
            Event::End(e) if e.name().as_ref() == "metadata" => {
                write_rendition_metas(&mut writer, layout)?;
            }
            // A self-closing `<metadata/>` has no end tag to append to, so it
            // has to be opened up to hold the declaration.
            Event::Empty(e) if e.name().as_ref() == "metadata" => {
                let mut opened = BytesStart::new("metadata");
                for attr in e.attributes().flatten() {
                    opened.push_attribute(attr);
                }
                writer.write_event(Event::Start(opened))?;
                write_rendition_metas(&mut writer, layout)?;
                writer.write_event(Event::End(quick_xml::events::BytesEnd::new("metadata")))?;
                buf.clear();
                continue;
            }
            Event::Eof => break,
            _ => {}
        }
        writer.write_event(event)?;
        buf.clear();
    }

    Ok(writer.into_inner().into_inner())
}

fn write_rendition_metas<W: Write>(writer: &mut Writer<W>, layout: &FixedLayout) -> Result<()> {
    write_rendition_meta(writer, "rendition:layout", "pre-paginated")?;
    if let Some(orientation) = &layout.orientation {
        write_rendition_meta(writer, "rendition:orientation", orientation)?;
    }
    // A device shows one page at a time; a synthetic spread would pair pages
    // the illustrator never laid out facing each other.
    write_rendition_meta(writer, "rendition:spread", "none")
}

fn write_rendition_meta<W: Write>(
    writer: &mut Writer<W>,
    property: &str,
    value: &str,
) -> Result<()> {
    let mut meta = BytesStart::new("meta");
    meta.push_attribute(("property", property));
    writer.write_event(Event::Start(meta))?;
    writer.write_event(Event::Text(BytesText::new(value)))?;
    writer.write_event(Event::End(quick_xml::events::BytesEnd::new("meta")))?;
    Ok(())
}

/// Give a content document the viewport a pre-paginated page needs.
///
/// Without this the `rendition:layout` says "scale this page" and gives no page
/// size to scale from, which readers treat as no declaration at all.
fn declare_viewport(input: &[u8], layout: &FixedLayout) -> Result<Vec<u8>> {
    if has_viewport(input) {
        return Ok(input.to_vec());
    }

    let content = format!("width={}, height={}", layout.width, layout.height);
    let mut reader = Reader::from_reader(input);
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    let mut buf = Vec::new();
    let mut injected = false;

    loop {
        let event = reader.read_event_into(&mut buf)?;
        match &event {
            Event::End(e) if !injected && e.name().as_ref() == "head" => {
                let mut meta = BytesStart::new("meta");
                meta.push_attribute(("name", "viewport"));
                meta.push_attribute(("content", content.as_str()));
                writer.write_event(Event::Empty(meta))?;
                injected = true;
            }
            Event::Eof => break,
            _ => {}
        }
        writer.write_event(event)?;
        buf.clear();
    }

    Ok(writer.into_inner().into_inner())
}

fn has_viewport(doc: &[u8]) -> bool {
    doc.windows(b"\"viewport\"".len())
        .any(|w| w == b"\"viewport\"" || w == b"'viewport'")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a PalmDB/MOBI container carrying the given EXTH string records.
    fn azw3_with(records: &[(u32, &str)]) -> Vec<u8> {
        let mut exth = Vec::new();
        exth.extend_from_slice(b"EXTH");
        exth.extend_from_slice(&0u32.to_be_bytes()); // header length, unread
        exth.extend_from_slice(&(records.len() as u32).to_be_bytes());
        for (kind, value) in records {
            exth.extend_from_slice(&kind.to_be_bytes());
            exth.extend_from_slice(&(8 + value.len() as u32).to_be_bytes());
            exth.extend_from_slice(value.as_bytes());
        }

        let mobi_len = 0xE8usize;
        let mut rec0 = vec![0u8; 16 + mobi_len];
        rec0[16..20].copy_from_slice(b"MOBI");
        rec0[20..24].copy_from_slice(&(mobi_len as u32).to_be_bytes());
        rec0[16 + 0x70..16 + 0x74].copy_from_slice(&0x40u32.to_be_bytes());
        rec0.extend_from_slice(&exth);

        // 78 bytes of PalmDB header, then two 8-byte record entries.
        let header_len = 78 + 16;
        let mut out = vec![0u8; header_len];
        out[76..78].copy_from_slice(&2u16.to_be_bytes());
        out[78..82].copy_from_slice(&(header_len as u32).to_be_bytes());
        out[86..90].copy_from_slice(&((header_len + rec0.len()) as u32).to_be_bytes());
        out.extend_from_slice(&rec0);
        out.extend_from_slice(b"tail");
        out
    }

    #[test]
    fn reads_a_fixed_layout_declaration() {
        let azw3 = azw3_with(&[
            (122, "true"),
            (124, "portrait"),
            (126, "600x914"),
            (129, "kindle:embed:0099"),
        ]);
        assert_eq!(
            from_kf8(&azw3),
            Some(FixedLayout {
                width: 600,
                height: 914,
                orientation: Some("portrait".to_string()),
            })
        );
    }

    #[test]
    fn a_reflowable_book_declares_nothing() {
        assert_eq!(from_kf8(&azw3_with(&[(126, "600x914")])), None);
        assert_eq!(from_kf8(&azw3_with(&[(122, "false")])), None);
    }

    /// A fixed layout with no page size cannot be described to a reader, and a
    /// `rendition:layout` with no viewport renders worse than none at all.
    #[test]
    fn a_fixed_layout_without_a_page_size_is_not_usable() {
        assert_eq!(from_kf8(&azw3_with(&[(122, "true")])), None);
        assert_eq!(from_kf8(&azw3_with(&[(122, "true"), (126, "0x914")])), None);
        assert_eq!(from_kf8(&azw3_with(&[(122, "true"), (126, "wide")])), None);
    }

    #[test]
    fn an_unlocked_orientation_is_left_unset() {
        let azw3 = azw3_with(&[(122, "true"), (124, "none"), (126, "1424x2170")]);
        let layout = from_kf8(&azw3).expect("fixed layout");
        assert_eq!(layout.orientation, None);
        assert_eq!((layout.width, layout.height), (1424, 2170));
    }

    #[test]
    fn a_file_that_is_not_a_palmdb_is_not_probed() {
        assert_eq!(from_kf8(b"PK\x03\x04 this is a zip"), None);
        assert_eq!(from_kf8(b""), None);
    }

    /// A zero-length EXTH record would leave the cursor where it was.
    #[test]
    fn a_malformed_record_length_does_not_hang() {
        let mut azw3 = azw3_with(&[(122, "true"), (126, "600x914")]);
        // Blank the first record's length field: 78 + 16 header, +16 PalmDOC/MOBI
        // magic offset, + MOBI header, + "EXTH" + len + count, + 4 for the type.
        let exth_at = 78 + 16 + 16 + 0xE8;
        let len_at = exth_at + 12 + 4;
        azw3[len_at..len_at + 4].copy_from_slice(&0u32.to_be_bytes());
        assert_eq!(from_kf8(&azw3), None);
    }

    fn layout() -> FixedLayout {
        FixedLayout {
            width: 600,
            height: 914,
            orientation: Some("portrait".to_string()),
        }
    }

    #[test]
    fn the_package_gains_the_rendition_metadata() {
        let opf = br#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="BookId">
  <metadata><dc:title>x</dc:title></metadata>
  <manifest/>
</package>"#;
        let out = declare_in_opf(opf, &layout()).unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains(r#"prefix="rendition:"#), "{out}");
        assert!(
            out.contains(r#"<meta property="rendition:layout">pre-paginated</meta>"#),
            "{out}"
        );
        assert!(
            out.contains(r#"<meta property="rendition:orientation">portrait</meta>"#),
            "{out}"
        );
        // Added inside metadata, not after it.
        let layout_at = out.find("rendition:layout").unwrap();
        assert!(layout_at < out.find("</metadata>").unwrap(), "{out}");
    }

    #[test]
    fn an_existing_prefix_declaration_is_kept() {
        let opf = br#"<package prefix="foaf: http://xmlns.com/foaf/spec/" version="3.0"><metadata><dc:title>x</dc:title></metadata></package>"#;
        let out = String::from_utf8(declare_in_opf(opf, &layout()).unwrap()).unwrap();
        assert!(out.contains("foaf: http://xmlns.com/foaf/spec/"), "{out}");
        assert!(out.contains("rendition: http://www.idpf.org"), "{out}");
    }

    /// `<metadata/>` carries no end tag to append to.
    #[test]
    fn a_self_closing_metadata_block_is_opened_up() {
        let opf = br#"<package version="3.0"><metadata/><manifest/></package>"#;
        let out = String::from_utf8(declare_in_opf(opf, &layout()).unwrap()).unwrap();
        assert!(out.contains("<metadata>"), "{out}");
        assert!(out.contains("rendition:layout"), "{out}");
        assert!(out.contains("</metadata>"), "{out}");
        assert!(out.contains("<manifest/>"), "{out}");
    }

    #[test]
    fn an_unlocked_orientation_is_not_written() {
        let free = FixedLayout {
            width: 600,
            height: 914,
            orientation: None,
        };
        let opf = br#"<package version="3.0"><metadata/></package>"#;
        let out = String::from_utf8(declare_in_opf(opf, &free).unwrap()).unwrap();
        assert!(out.contains("rendition:layout"), "{out}");
        assert!(!out.contains("rendition:orientation"), "{out}");
    }

    #[test]
    fn a_document_gains_a_viewport() {
        let doc = b"<html><head><title>x</title></head><body><p>hi</p></body></html>";
        let out = String::from_utf8(declare_viewport(doc, &layout()).unwrap()).unwrap();
        assert!(
            out.contains(r#"<meta name="viewport" content="width=600, height=914"/>"#),
            "{out}"
        );
        assert!(out.find("viewport").unwrap() < out.find("</head>").unwrap());
        assert!(out.contains("<p>hi</p>"), "{out}");
    }

    #[test]
    fn a_document_that_has_a_viewport_is_left_alone() {
        let doc =
            br#"<html><head><meta name="viewport" content="width=1x1"/></head><body/></html>"#;
        let out = declare_viewport(doc, &layout()).unwrap();
        assert_eq!(out, doc.to_vec());
    }

    #[test]
    fn a_publishers_own_declaration_wins() {
        assert!(contains_pre_paginated(
            br#"<meta property="rendition:layout">pre-paginated</meta>"#
        ));
        assert!(!contains_pre_paginated(br#"<metadata/>"#));
    }
}
