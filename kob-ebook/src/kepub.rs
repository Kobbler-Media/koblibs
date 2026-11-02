//! EPUB → KEPUB transformation.
//!
//! Walks an EPUB ZIP, transforms XHTML manifest items + the OPF, leaves
//! everything else untouched. The XHTML transform mirrors pgaskin/kepubify's
//! mandatory steps:
//!
//! 1. Inject `<style id="kobostylehacks">` into `<head>`.
//! 2. Wrap `<body>` contents in `<div id="book-columns"><div id="book-inner">`.
//! 3. Wrap text fragments in `<span class="koboSpan" id="kobo.{para}.{seg}">`.
//!
//! The OPF transform adds `properties="cover-image"` to whatever manifest item
//! the book identifies as the cover.
//!
//! A fourth step applies only to documents whose whole body is one image —
//! plate pages, maps, comic panels. See [`standalone_image_src`].
//!
//! A fifth step applies only to documents whose whole page is a CSS
//! background image — the Kindle fixed-layout shape, where the text is an
//! overlay on the page, not content beside it. See [`full_page_background_plan`].

use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read, Seek, Write};

use quick_xml::Writer;
use quick_xml::events::{BytesEnd, BytesStart, BytesText, Event};
use quick_xml::reader::Reader;
use zip::CompressionMethod;
use zip::write::{SimpleFileOptions, ZipWriter};

use crate::image_size;
use crate::simple_css;
use crate::{EbookError, Result};

const KOBO_STYLE_HACK: &str = "div#book-inner { margin-top: 0; margin-bottom: 0;}";

/// Page-filling rules for a document rewritten by [`write_full_page_image`].
///
/// The SVG does the aspect-preserving scaling; this only has to give it a box
/// with the page's height to scale into. It is emitted *instead of*
/// [`KOBO_STYLE_HACK`]'s document, never alongside it, so the margin reset is
/// repeated here.
const FULL_PAGE_IMAGE_CSS: &str = concat!(
    "div#book-inner { margin-top: 0; margin-bottom: 0;}",
    "html, body { height: 100%; margin: 0; padding: 0; }",
    "div#book-columns, div#book-inner, div.kobo-fullpage { height: 100%; }",
    "div.kobo-fullpage svg { display: block; width: 100%; height: 100%; }"
);

/// Non-whitespace characters a body may hold and still count as "just an
/// image". A folio or a plate number rides along; a caption or a paragraph
/// does not, because the rewrite discards whatever text it finds.
const STANDALONE_IMAGE_TEXT_BUDGET: usize = 24;

/// Zip entry recording which kepubifier wrote a KEPUB.
///
/// Makes the file self-describing: a caller can ask an existing KEPUB which
/// transform produced it without keeping a parallel record in a database that
/// could drift from the file it describes.
///
/// `META-INF/` on purpose, and not somewhere more obvious:
///
/// - it is outside the OPF manifest, so no reader lists it or renders it, and
///   Calibre already keeps `META-INF/calibre_bookmarks.txt` there — an extra
///   entry in this directory is well-trodden ground rather than a novelty;
/// - it survives a metadata write. `metadata_write` shells out to Calibre's
///   `ebook-meta`, which rewrites the EPUB's OPF and leaves other entries
///   alone. A stamp held in the zip's *archive comment* would not survive
///   that, and every metadata write would silently make the book look
///   unconverted;
/// - it is one entry per file rather than a mark inside each document, so it
///   also covers a source that arrived already kepubified, whose documents
///   this transform passes through untouched.
const GENERATION_STAMP_PATH: &str = "META-INF/kobbler-kepub.txt";

/// Read the generation stamp off a KEPUB, or `0` for one made before stamping
/// (or by some other tool). Either way, older than what this crate writes now.
pub(crate) fn read_generation(kepub: &[u8]) -> u32 {
    let Ok(mut archive) = zip::ZipArchive::new(Cursor::new(kepub)) else {
        return 0;
    };
    let Ok(mut entry) = archive.by_name(GENERATION_STAMP_PATH) else {
        return 0;
    };
    let mut text = String::new();
    if entry.read_to_string(&mut text).is_err() {
        return 0;
    }
    text.trim().parse().unwrap_or(0)
}

/// Drive the full EPUB→KEPUB transformation over a byte buffer.
pub(crate) fn transform_epub(input: &[u8]) -> Result<Vec<u8>> {
    let mut archive = zip::ZipArchive::new(Cursor::new(input))?;

    // Discover OPF path via container.xml so we know which file to OPF-transform
    // and which manifest items are XHTML content.
    let opf_path = read_opf_path(&mut archive)?;
    let xhtml_paths = read_xhtml_manifest(&mut archive, &opf_path)?;
    // Collected up front: the XHTML pass below holds a borrow on the archive
    // for each entry, so it cannot go back for an image's header or a
    // stylesheet mid-stream.
    let ctx = KepubContext::read(&mut archive);

    let mut out = ZipWriter::new(Cursor::new(Vec::new()));
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let deflate = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    // Per the EPUB spec, `mimetype` must be the first entry, stored uncompressed.
    out.start_file("mimetype", stored)?;
    out.write_all(b"application/epub+zip")?;

    let mut written = HashSet::with_capacity(archive.len());
    written.insert("mimetype".to_string());

    // Written before the copy loop so that a stamp already in the input (this
    // book has been through an earlier kepubifier) is skipped as "already
    // written" rather than copied over the current one.
    out.start_file(GENERATION_STAMP_PATH, deflate)?;
    out.write_all(crate::KEPUB_GENERATION.to_string().as_bytes())?;
    written.insert(GENERATION_STAMP_PATH.to_string());

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let name = file.name().to_string();
        if written.contains(&name) {
            continue;
        }
        if file.is_dir() {
            continue;
        }
        let mut buf = Vec::with_capacity(file.size() as usize);
        file.read_to_end(&mut buf)?;
        drop(file);

        let bytes = if name == opf_path {
            transform_opf(&buf)?
        } else if xhtml_paths.contains(&name) {
            transform_xhtml(&buf, &name, &ctx)?
        } else {
            buf
        };

        out.start_file(&name, deflate)?;
        out.write_all(&bytes)?;
        written.insert(name);
    }

    let cursor = out.finish()?;
    Ok(cursor.into_inner())
}

/// What the XHTML transform needs to know about the rest of the archive.
pub(crate) struct KepubContext {
    pub image_dims: HashMap<String, (u32, u32)>,
    css_rules: HashMap<String, Vec<CssRule>>,
    archive_names: HashSet<String>,
}

impl KepubContext {
    pub(crate) fn read<R: Read + Seek>(archive: &mut zip::ZipArchive<R>) -> Self {
        Self {
            image_dims: read_image_dimensions(archive),
            css_rules: read_css_rules(archive),
            archive_names: read_entry_names(archive),
        }
    }
}

pub(crate) fn read_opf_path<R: Read + Seek>(archive: &mut zip::ZipArchive<R>) -> Result<String> {
    let mut container = archive
        .by_name("META-INF/container.xml")
        .map_err(|_| EbookError::EpubStructure("missing META-INF/container.xml".into()))?;
    let mut buf = String::new();
    container.read_to_string(&mut buf)?;

    let mut reader = Reader::from_str(&buf);
    let mut reader_buf = Vec::new();
    loop {
        match reader.read_event_into(&mut reader_buf)? {
            Event::Empty(e) | Event::Start(e) if e.name().as_ref() == b"rootfile" => {
                for attr in e.attributes() {
                    let attr = attr?;
                    if attr.key.as_ref() == b"full-path" {
                        return Ok(String::from_utf8_lossy(attr.value.as_ref()).into_owned());
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
        reader_buf.clear();
    }
    Err(EbookError::EpubStructure(
        "container.xml missing rootfile/@full-path".into(),
    ))
}

pub(crate) fn read_xhtml_manifest<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    opf_path: &str,
) -> Result<HashSet<String>> {
    let mut opf = archive
        .by_name(opf_path)
        .map_err(|_| EbookError::EpubStructure(format!("OPF file not found in zip: {opf_path}")))?;
    let mut buf = String::new();
    opf.read_to_string(&mut buf)?;
    drop(opf);

    let opf_dir = match opf_path.rfind('/') {
        Some(i) => &opf_path[..=i],
        None => "",
    };

    let mut paths = HashSet::new();
    let mut reader = Reader::from_str(&buf);
    let mut reader_buf = Vec::new();
    loop {
        match reader.read_event_into(&mut reader_buf)? {
            Event::Empty(e) | Event::Start(e) if e.name().as_ref() == b"item" => {
                let mut href = None;
                let mut media_type = None;
                for attr in e.attributes() {
                    let attr = attr?;
                    match attr.key.as_ref() {
                        b"href" => href = Some(attr.value.into_owned()),
                        b"media-type" => media_type = Some(attr.value.into_owned()),
                        _ => {}
                    }
                }
                if let (Some(h), Some(mt)) = (href, media_type) {
                    let mt = String::from_utf8_lossy(&mt);
                    if mt == "application/xhtml+xml" || mt == "text/html" {
                        let h = String::from_utf8_lossy(&h);
                        paths.insert(format!("{opf_dir}{h}"));
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
        reader_buf.clear();
    }
    Ok(paths)
}

// --- OPF transform ---

fn transform_opf(input: &[u8]) -> Result<Vec<u8>> {
    // Resolve which manifest item id is the cover via `<meta name="cover" content="..."/>`,
    // defaulting to id="cover".
    let mut reader = Reader::from_reader(input);
    let mut cover_id: Option<String> = None;
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Empty(e) | Event::Start(e) if e.name().as_ref() == b"meta" => {
                let mut is_cover_meta = false;
                let mut content = None;
                for attr in e.attributes() {
                    let attr = attr?;
                    match attr.key.as_ref() {
                        b"name" if attr.value.as_ref() == b"cover" => is_cover_meta = true,
                        b"content" => content = Some(attr.value.into_owned()),
                        _ => {}
                    }
                }
                if is_cover_meta && let Some(c) = content {
                    cover_id = Some(String::from_utf8_lossy(&c).into_owned());
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    let cover_id = cover_id.unwrap_or_else(|| "cover".to_string());

    // Now walk again, rewriting the matching <item> to add properties="cover-image"
    // and dropping calibre:timestamp meta entries.
    let mut reader = Reader::from_reader(input);
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    let mut buf = Vec::new();
    loop {
        let event = reader.read_event_into(&mut buf)?;
        match &event {
            Event::Empty(e) | Event::Start(e) if e.name().as_ref() == b"item" => {
                let mut id_match = false;
                let mut has_cover_property = false;
                for attr in e.attributes().flatten() {
                    if attr.key.as_ref() == b"id" && attr.value.as_ref() == cover_id.as_bytes() {
                        id_match = true;
                    }
                    if attr.key.as_ref() == b"properties"
                        && attr.value.as_ref().windows(11).any(|w| w == b"cover-image")
                    {
                        has_cover_property = true;
                    }
                }

                if id_match && !has_cover_property {
                    let mut new_e = BytesStart::new(
                        std::str::from_utf8(e.name().as_ref())
                            .unwrap_or("item")
                            .to_string(),
                    );
                    let mut existing_props: Option<Vec<u8>> = None;
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"properties" {
                            existing_props = Some(attr.value.into_owned());
                        } else {
                            new_e.push_attribute(attr);
                        }
                    }
                    let merged: String = match existing_props {
                        Some(v) if !v.is_empty() => {
                            let s = String::from_utf8_lossy(&v).into_owned();
                            format!("{s} cover-image")
                        }
                        _ => "cover-image".to_string(),
                    };
                    new_e.push_attribute(("properties", merged.as_str()));
                    if matches!(event, Event::Empty(_)) {
                        writer.write_event(Event::Empty(new_e))?;
                    } else {
                        writer.write_event(Event::Start(new_e))?;
                    }
                    buf.clear();
                    continue;
                }
            }
            Event::Empty(e) | Event::Start(e) if e.name().as_ref() == b"meta" => {
                let drop_me = e
                    .attributes()
                    .flatten()
                    .any(|a| a.key.as_ref() == b"name" && a.value.as_ref() == b"calibre:timestamp");
                if drop_me {
                    if matches!(event, Event::Start(_)) {
                        // Skip until the matching End — calibre:timestamp metas are usually
                        // self-closing, but be defensive.
                        loop {
                            let inner = reader.read_event_into(&mut buf)?;
                            if matches!(&inner, Event::End(end) if end.name().as_ref() == b"meta") {
                                break;
                            }
                            if matches!(inner, Event::Eof) {
                                break;
                            }
                        }
                    }
                    buf.clear();
                    continue;
                }
            }
            _ => {}
        }
        if matches!(event, Event::Eof) {
            break;
        }
        writer.write_event(event)?;
        buf.clear();
    }
    Ok(writer.into_inner().into_inner())
}

// --- XHTML transform ---

#[derive(Default)]
struct ContentState {
    body_depth: u32,
    skip_depth: u32,
    para: u32,
    seg: u32,
    inc_para_next: bool,
}

fn is_skip_tag(name: &[u8]) -> bool {
    matches!(
        name,
        b"script" | b"style" | b"pre" | b"audio" | b"video" | b"svg" | b"math"
    )
}

fn is_para_trigger(name: &[u8]) -> bool {
    matches!(
        name,
        b"p" | b"ol" | b"ul" | b"table" | b"h1" | b"h2" | b"h3" | b"h4" | b"h5" | b"h6"
    )
}

fn detect_kepubified(input: &[u8]) -> bool {
    // Cheap substring check — if the document already has a koboSpan we skip
    // the transform to stay idempotent.
    input
        .windows(b"class=\"koboSpan\"".len())
        .any(|w| w == b"class=\"koboSpan\"")
        || input
            .windows(b"class='koboSpan'".len())
            .any(|w| w == b"class='koboSpan'")
}

/// Intrinsic dimensions of every raster image in the archive, keyed by zip
/// entry path. Images we cannot measure are simply absent — a page whose image
/// is missing here keeps the ordinary transform.
fn read_image_dimensions<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> HashMap<String, (u32, u32)> {
    let mut dims = HashMap::new();
    for i in 0..archive.len() {
        let Ok(mut entry) = archive.by_index(i) else {
            continue;
        };
        let name = entry.name().to_string();
        let is_raster = name.rsplit('.').next().is_some_and(|ext| {
            ["jpg", "jpeg", "png", "gif"]
                .iter()
                .any(|known| ext.eq_ignore_ascii_case(known))
        });
        if !is_raster {
            continue;
        }
        // The header is all we need, but a JPEG's frame marker can sit past an
        // EXIF thumbnail — so take a prefix rather than the first few bytes,
        // and no more, since a comic can carry hundreds of these.
        let mut head = Vec::new();
        if entry
            .by_ref()
            .take(64 * 1024)
            .read_to_end(&mut head)
            .is_err()
        {
            continue;
        }
        if let Some(wh) = image_size::dimensions(&head) {
            dims.insert(name, wh);
        }
    }
    dims
}

/// The `src` of the single image that makes up this document's entire body, if
/// that is what the document is.
///
/// Plate pages, maps and comic panels are one image alone in a `<p>`, and a
/// reader sizes them from their intrinsic pixels: `max-width: 100%` caps an
/// image but never enlarges one, so a 400px map lands at 400px on a 1264px
/// panel — a third of the page, centred, unreadable. Those pages are worth
/// rewriting to scale. A page that also carries prose is not: there the image
/// is an illustration among text and blowing it up would be wrong.
///
/// Returns `None` for anything else — several images, meaningful text, or an
/// existing `<svg>` (already the full-page idiom, so leave it be).
fn standalone_image_src(input: &[u8]) -> Result<Option<String>> {
    let mut reader = Reader::from_reader(input);
    let mut buf = Vec::new();
    let mut body_depth = 0u32;
    let mut skip_depth = 0u32;
    let mut text_budget = STANDALONE_IMAGE_TEXT_BUDGET;
    let mut src: Option<String> = None;
    let mut image_count = 0usize;

    loop {
        let event = reader.read_event_into(&mut buf)?;
        let is_start = matches!(event, Event::Start(_));
        match &event {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) => {
                let lower = e.name().as_ref().to_ascii_lowercase();
                if lower == b"body" {
                    body_depth += 1;
                } else if body_depth > 0 {
                    if lower == b"svg" || lower == b"image" || lower == b"iframe" {
                        return Ok(None);
                    }
                    if lower == b"img" {
                        image_count += 1;
                        if image_count > 1 {
                            return Ok(None);
                        }
                        for attr in e.attributes() {
                            let attr = attr?;
                            if attr.key.as_ref().eq_ignore_ascii_case(b"src") {
                                // Kept in its escaped form: it goes back out as
                                // an attribute value unchanged, and only the
                                // zip lookup needs it decoded.
                                src =
                                    Some(String::from_utf8_lossy(attr.value.as_ref()).into_owned());
                            }
                        }
                    }
                    if is_start && is_skip_tag(&lower) {
                        skip_depth += 1;
                    }
                }
            }
            Event::End(e) => {
                let lower = e.name().as_ref().to_ascii_lowercase();
                if lower == b"body" && body_depth > 0 {
                    body_depth -= 1;
                } else if body_depth > 0 && is_skip_tag(&lower) && skip_depth > 0 {
                    skip_depth -= 1;
                }
            }
            Event::Text(t) if body_depth > 0 && skip_depth == 0 => {
                let raw = t.decode().map_err(quick_xml::Error::from)?;
                let visible = raw.chars().filter(|c| !c.is_whitespace()).count();
                text_budget = match text_budget.checked_sub(visible) {
                    Some(left) => left,
                    None => return Ok(None),
                };
            }
            _ => {}
        }
        buf.clear();
    }

    Ok(if image_count == 1 { src } else { None })
}

/// Resolve a document-relative href to a zip entry path, undoing XML entity
/// and percent encoding so it can be matched against the archive's own names.
pub(crate) fn resolve_relative(doc_path: &str, href: &str) -> Option<String> {
    let href = href.split(['#', '?']).next().unwrap_or(href);
    if href.is_empty() || href.starts_with("data:") || href.contains("://") {
        return None;
    }
    let href = quick_xml::escape::unescape(href).unwrap_or(std::borrow::Cow::Borrowed(href));
    let href = percent_decode(&href);
    if let Some(abs) = href.strip_prefix('/') {
        return Some(abs.to_string());
    }
    let mut parts: Vec<&str> = doc_path.split('/').collect();
    parts.pop(); // the linking document's own filename
    for seg in href.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    Some(parts.join("/"))
}

pub(crate) fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(hi), Some(lo)) = (
                (b[i + 1] as char).to_digit(16),
                (b[i + 2] as char).to_digit(16),
            )
        {
            out.push((hi * 16 + lo) as u8);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Rewrite a standalone image page so the image scales to fill the page.
///
/// The body becomes a single SVG sized in the image's own pixels: `viewBox`
/// plus `preserveAspectRatio="xMidYMid meet"` scales it up or down to fit
/// while keeping its aspect, which plain CSS cannot do — setting `width` and
/// `height` together distorts, and setting either alone overflows the other
/// axis. This is the idiom publishers use for plate pages, so readers handle
/// it well.
///
/// The head is carried over untouched (title, the book's own stylesheets) and
/// only the injected style differs. No koboSpans are emitted: there is no text
/// on the page to anchor them to.
fn write_full_page_image(input: &[u8], src_attr: &str, width: u32, height: u32) -> Result<Vec<u8>> {
    let mut reader = Reader::from_reader(input);
    let mut writer: Writer<Cursor<Vec<u8>>> = Writer::new(Cursor::new(Vec::new()));
    let mut buf = Vec::new();
    let mut head_style_emitted = false;
    let mut body_depth = 0u32;

    let body = format!(
        concat!(
            r#"<div id="book-columns"><div id="book-inner"><div class="kobo-fullpage">"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink""#,
            r#" version="1.1" viewBox="0 0 {w} {h}" preserveAspectRatio="xMidYMid meet">"#,
            r#"<image width="{w}" height="{h}" xlink:href="{src}"/>"#,
            r#"</svg></div></div></div>"#,
        ),
        w = width,
        h = height,
        // Already in its escaped attribute form — see `standalone_image_src`.
        src = src_attr,
    );

    loop {
        let event = reader.read_event_into(&mut buf)?;
        match event {
            Event::Eof => break,
            Event::Start(e) if e.name().as_ref().eq_ignore_ascii_case(b"body") => {
                writer.write_event(Event::Start(e))?;
                writer.write_event(Event::Text(BytesText::from_escaped(body.as_str())))?;
                body_depth += 1;
            }
            Event::End(e) if e.name().as_ref().eq_ignore_ascii_case(b"body") => {
                body_depth = body_depth.saturating_sub(1);
                writer.write_event(Event::End(e))?;
            }
            // Everything the original body held is replaced by the SVG.
            _ if body_depth > 0 => {}
            Event::End(e) => {
                if e.name().as_ref().eq_ignore_ascii_case(b"head") && !head_style_emitted {
                    let mut style = BytesStart::new("style");
                    style.push_attribute(("type", "text/css"));
                    style.push_attribute(("class", "kobostylehacks"));
                    writer.write_event(Event::Start(style))?;
                    writer.write_event(Event::Text(BytesText::new(FULL_PAGE_IMAGE_CSS)))?;
                    writer.write_event(Event::End(BytesEnd::new("style")))?;
                    head_style_emitted = true;
                }
                writer.write_event(Event::End(e))?;
            }
            other => writer.write_event(other)?,
        }
        buf.clear();
    }

    Ok(writer.into_inner().into_inner())
}

// --- Full-page CSS background (KF8 fixed layout) ---

/// A CSS rule simple enough to match against an element by id or class.
///
/// Only bare compound selectors are kept — `#id`, `.class`, `tag.class`,
/// `tag#id` — and only the declarations [`find_full_page_background`] needs.
/// Anything with combinators, pseudo-classes or attribute selectors is
/// dropped: matching those properly needs a real CSS engine, and a wrong
/// match would rewrite a page that is not a page.
#[derive(Debug, Default, Clone)]
struct CssRule {
    id: Option<String>,
    class: Option<String>,
    /// The content of `background-image: url(...)`, still relative.
    url: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    /// `background-size: <w>px <h>px`, when written in px.
    background_size: Option<(u32, u32)>,
}

/// The rules of every stylesheet in the archive, keyed by zip entry path.
fn read_css_rules<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> HashMap<String, Vec<CssRule>> {
    let mut out = HashMap::new();
    for i in 0..archive.len() {
        let Ok(mut entry) = archive.by_index(i) else {
            continue;
        };
        let name = entry.name().to_string();
        let is_css = name
            .rsplit('.')
            .next()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("css"));
        if !is_css {
            continue;
        }
        let mut buf = String::new();
        if entry.read_to_string(&mut buf).is_err() {
            continue;
        }
        out.insert(name, scan_css_rules(&buf));
    }
    out
}

/// The zip entry names in the archive, for resolving a background `url()`
/// against what is actually in the file.
fn read_entry_names<R: Read + Seek>(archive: &mut zip::ZipArchive<R>) -> HashSet<String> {
    let mut out = HashSet::new();
    for i in 0..archive.len() {
        if let Ok(entry) = archive.by_index(i) {
            out.insert(entry.name().to_string());
        }
    }
    out
}

/// Split a stylesheet into the simple rules it holds. Rules without an id or
/// a class are dropped: a bare `div { background-image: ... }` would match
/// every div in the document, and that is not a page marker.
fn scan_css_rules(css: &str) -> Vec<CssRule> {
    let cleaned = simple_css::strip_comments(css);
    let mut rules = Vec::new();
    for (selector, body) in simple_css::rules(&cleaned) {
        for sel in selector.split(',') {
            if let Some(sel) = simple_css::parse_simple_selector(sel.trim())
                && (sel.id.is_some() || sel.class.is_some())
            {
                let rule = CssRule {
                    id: sel.id,
                    class: sel.class,
                    ..CssRule::default()
                };
                rules.push(apply_declarations(rule, body));
            }
        }
    }
    rules
}

fn apply_declarations(mut rule: CssRule, body: &str) -> CssRule {
    for (prop, value) in simple_css::declarations(body) {
        match prop.as_str() {
            "background-image" => {
                if let Some(url) = parse_css_url(value) {
                    rule.url = Some(url);
                }
            }
            "width" => rule.width = parse_px(value),
            "height" => rule.height = parse_px(value),
            "background-size" => rule.background_size = parse_px_pair(value),
            _ => {}
        }
    }
    rule
}

/// The content of `url(...)`, unquoted. Percent and entity encoding are left
/// alone: [`resolve_relative`] undoes both before the archive lookup.
fn parse_css_url(value: &str) -> Option<String> {
    let value = value.trim();
    let start = value.find("url(")? + 4;
    let end = value[start..].find(')')? + start;
    let inner = value[start..end].trim();
    let inner = inner
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| inner.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(inner);
    if inner.is_empty() {
        None
    } else {
        Some(inner.to_string())
    }
}

/// `600px` → `600`. Integers only: KF8 stylesheets write integers, and a
/// fractional or unitless size is not a page size this rewrite can trust.
fn parse_px(value: &str) -> Option<u32> {
    let digits = value.trim().strip_suffix("px")?;
    let n: u32 = digits.trim().parse().ok()?;
    (n > 0).then_some(n)
}

/// `600px 914px` → `(600, 914)`. `cover`, `contain` and single values do not
/// name a size, so they do not answer.
fn parse_px_pair(value: &str) -> Option<(u32, u32)> {
    let mut parts = value.split_whitespace().map(parse_px);
    let w = parts.next()??;
    let h = parts.next()??;
    Some((w, h))
}

/// The whole page expressed as a CSS background, if that is what this document is.
///
/// Kindle fixed-layout books draw each page as a background image on a
/// full-page div and float the text on top of it, absolutely positioned. The
/// background never appears in the markup as an `<img>` — it lives in the
/// stylesheet, keyed by the page div's id or class:
///
/// ```css
/// #KF_041_img { background-image: url(images/image_0046.jpg); }
/// div.lpage   { position: absolute; width: 600px; height: 914px; }
/// ```
///
/// Recognition is by the element's own rules: an id or class that resolves to
/// a `background-image`, plus a declared size. The text may live inside the
/// element (boko's AZW3 export) or beside it as a sibling (its EPUB export);
/// either way the page is the background and everything else in the document
/// is overlay.
struct FullPageBackground {
    /// The background's `url(...)` content, as written in the stylesheet.
    url: String,
    /// The size the element's own rules declare for it.
    size: (u32, u32),
    /// The id the background came from, if any rule matched on one.
    matched_id: Option<String>,
    /// The classes any rule matched on.
    matched_classes: Vec<String>,
}

fn find_full_page_background(input: &[u8], rules: &[CssRule]) -> Option<FullPageBackground> {
    let mut reader = Reader::from_reader(input);
    let mut buf = Vec::new();
    let mut body_depth = 0u32;
    let mut candidate: Option<FullPageBackground> = None;

    loop {
        let event = reader.read_event_into(&mut buf).ok()?;
        match &event {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) => {
                let lower = e.name().as_ref().to_ascii_lowercase();
                if lower == b"body" {
                    body_depth += 1;
                } else if body_depth > 0
                    && let Some(found) = match_background_element(e, rules)
                {
                    // Two full-page backgrounds on one page is a spread or a
                    // collision, not a page this rewrite can handle.
                    if candidate.is_some() {
                        return None;
                    }
                    candidate = Some(found);
                }
            }
            Event::End(e) => {
                let lower = e.name().as_ref().to_ascii_lowercase();
                if lower == b"body" && body_depth > 0 {
                    body_depth -= 1;
                }
            }
            _ => {}
        }
        buf.clear();
    }

    candidate
}

/// Merge every rule that could apply to the element, in stylesheet order. A
/// later declaration wins a field, like the cascade within one specificity —
/// so the url may come from the id rule and the size from a class rule, as
/// KF8 books write it.
fn match_background_element(e: &BytesStart, rules: &[CssRule]) -> Option<FullPageBackground> {
    let mut id: Option<String> = None;
    let mut classes: Vec<String> = Vec::new();
    for attr in e.attributes().flatten() {
        match attr.key.as_ref() {
            b"id" => id = Some(String::from_utf8_lossy(&attr.value).into_owned()),
            b"class" => {
                for c in String::from_utf8_lossy(&attr.value).split_whitespace() {
                    classes.push(c.to_string());
                }
            }
            _ => {}
        }
    }

    let mut bg = FullPageBackground {
        url: String::new(),
        size: (0, 0),
        matched_id: None,
        matched_classes: Vec::new(),
    };
    let mut has_url = false;
    let mut width = None;
    let mut height = None;
    let mut background_size = None;
    for rule in rules {
        let hit = match (&rule.id, &rule.class) {
            (Some(rid), _) => id.as_deref() == Some(rid.as_str()),
            (None, Some(rc)) => classes.iter().any(|c| c == rc),
            (None, None) => false,
        };
        if !hit {
            continue;
        }
        if let Some(rid) = &rule.id {
            bg.matched_id = Some(rid.clone());
        }
        if let Some(rc) = &rule.class
            && !bg.matched_classes.contains(rc)
        {
            bg.matched_classes.push(rc.clone());
        }
        if let Some(u) = &rule.url {
            bg.url = u.clone();
            has_url = true;
        }
        if let Some(w) = rule.width {
            width = Some(w);
        }
        if let Some(h) = rule.height {
            height = Some(h);
        }
        if let Some(bs) = rule.background_size {
            background_size = Some(bs);
        }
    }
    if !has_url {
        return None;
    }
    let size = match (width, height) {
        (Some(w), Some(h)) => (w, h),
        _ => background_size?,
    };
    bg.size = size;
    Some(bg)
}

/// The page size a document declares in its viewport meta, if it declares
/// one in pixels. `width=device-width` names no size and does not answer.
fn viewport_size(input: &[u8]) -> Option<(u32, u32)> {
    let mut reader = Reader::from_reader(input);
    let mut buf = Vec::new();
    loop {
        let event = reader.read_event_into(&mut buf).ok()?;
        match &event {
            Event::Empty(e) | Event::Start(e) if e.name().as_ref() == b"meta" => {
                let mut name = None;
                let mut content = None;
                for attr in e.attributes().flatten() {
                    match attr.key.as_ref() {
                        b"name" => name = Some(String::from_utf8_lossy(&attr.value).into_owned()),
                        b"content" => {
                            content = Some(String::from_utf8_lossy(&attr.value).into_owned());
                        }
                        _ => {}
                    }
                }
                if name.as_deref() == Some("viewport")
                    && let Some(c) = content
                    && let Some(wh) = parse_viewport_content(&c)
                {
                    return Some(wh);
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    None
}

fn parse_viewport_content(content: &str) -> Option<(u32, u32)> {
    let mut width = None;
    let mut height = None;
    for part in content.split(',') {
        let Some((k, v)) = part.split_once('=') else {
            continue;
        };
        let k = k.trim().to_ascii_lowercase();
        let Ok(n) = v.trim().parse::<u32>() else {
            continue;
        };
        match k.as_str() {
            "width" => width = Some(n),
            "height" => height = Some(n),
            _ => {}
        }
    }
    match (width, height) {
        (Some(w), Some(h)) if w > 0 && h > 0 => Some((w, h)),
        _ => None,
    }
}

/// The rewrite for a document whose page is a CSS background, if the page
/// size can be established.
///
/// The element's own rules are the evidence the background is the whole
/// page, not a corner of it, so they must name a size. The page size itself
/// comes from the document's viewport when it has one — which is where
/// [`crate::fixed_layout`] puts the EXTH header's — else the image's
/// intrinsic dimensions, else the declared size. A declared size that does
/// not meet the page's means something else, and the ordinary transform is
/// the safer outcome.
struct FullPagePlan {
    /// The background's `url(...)` content, re-rooted if needed so it
    /// resolves from the document.
    href: String,
    /// The viewBox for the SVG.
    size: (u32, u32),
    matched_id: Option<String>,
    matched_classes: Vec<String>,
}

fn full_page_background_plan(
    input: &[u8],
    doc_path: &str,
    image_dims: &HashMap<String, (u32, u32)>,
    css_rules: &HashMap<String, Vec<CssRule>>,
    archive_names: &HashSet<String>,
) -> Option<FullPagePlan> {
    let (rules, sheets) = linked_stylesheet_rules(input, doc_path, css_rules);
    let bg = find_full_page_background(input, &rules)?;
    let page = viewport_size(input)
        .or_else(|| resolve_relative(doc_path, &bg.url).and_then(|e| image_dims.get(&e).copied()))
        .unwrap_or(bg.size);
    if bg.size != page {
        return None;
    }
    Some(FullPagePlan {
        href: resolve_background_href(doc_path, &sheets, &bg.url, archive_names),
        size: page,
        matched_id: bg.matched_id,
        matched_classes: bg.matched_classes,
    })
}

/// The rules the document's linked stylesheets carry, in link order, plus
/// the stylesheets' own paths for re-rooting a `url()` written relative to
/// one of them.
fn linked_stylesheet_rules(
    input: &[u8],
    doc_path: &str,
    css_rules: &HashMap<String, Vec<CssRule>>,
) -> (Vec<CssRule>, Vec<String>) {
    let mut rules = Vec::new();
    let mut sheets = Vec::new();
    let mut reader = Reader::from_reader(input);
    let mut buf = Vec::new();
    while let Ok(event) = reader.read_event_into(&mut buf) {
        match &event {
            Event::Empty(e) | Event::Start(e) if e.name().as_ref() == b"link" => {
                let mut rel = None;
                let mut href = None;
                for attr in e.attributes().flatten() {
                    match attr.key.as_ref() {
                        b"rel" => rel = Some(String::from_utf8_lossy(&attr.value).into_owned()),
                        b"href" => href = Some(String::from_utf8_lossy(&attr.value).into_owned()),
                        _ => {}
                    }
                }
                let is_stylesheet = rel
                    .map(|r| {
                        r.to_ascii_lowercase()
                            .split_whitespace()
                            .any(|t| t == "stylesheet")
                    })
                    .unwrap_or(false);
                if is_stylesheet
                    && let Some(h) = href
                    && let Some(path) = resolve_relative(doc_path, &h)
                    && let Some(r) = css_rules.get(&path)
                {
                    sheets.push(path);
                    rules.extend(r.iter().cloned());
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    (rules, sheets)
}

/// The `xlink:href` for the background: the `url()` as written when it
/// already resolves from the document (boko writes its stylesheets' urls
/// that way), re-rooted from the linked stylesheet when it resolves from
/// there instead, and left as written when neither holds — at which point
/// the reader gets the publisher's own string rather than a guess.
fn resolve_background_href(
    doc_path: &str,
    sheets: &[String],
    url: &str,
    archive_names: &HashSet<String>,
) -> String {
    if let Some(entry) = resolve_relative(doc_path, url)
        && archive_names.contains(&entry)
    {
        return url.to_string();
    }
    for sheet in sheets {
        if let Some(entry) = resolve_relative(sheet, url)
            && archive_names.contains(&entry)
        {
            let sheet_dir = match sheet.rfind('/') {
                Some(i) => &sheet[..=i],
                None => "",
            };
            let rel_to_sheet = entry.strip_prefix(sheet_dir).unwrap_or(&entry);
            if let Some(rerooted) = resolve_relative(doc_path, rel_to_sheet) {
                return rerooted;
            }
        }
    }
    url.to_string()
}

/// Remove the id and classes the background rules matched on, so the
/// stylesheet can no longer draw the page under the SVG. Leaving them in
/// would have the fixed-layout renderers on a Kobo and in foliate honour
/// both and draw the page twice.
fn strip_background_identity(input: &[u8], plan: &FullPagePlan) -> Result<Vec<u8>> {
    let mut reader = Reader::from_reader(input);
    let mut writer: Writer<Cursor<Vec<u8>>> = Writer::new(Cursor::new(Vec::new()));
    let mut buf = Vec::new();
    let mut body_depth = 0u32;
    let mut rewritten = false;

    loop {
        let event = reader.read_event_into(&mut buf)?;
        let mut skip_emit = false;
        match &event {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) if !rewritten => {
                let lower = e.name().as_ref().to_ascii_lowercase();
                if lower == b"body" {
                    body_depth += 1;
                } else if body_depth > 0 && is_page_element(e, plan) {
                    rewritten = true;
                    let mut rebuilt = BytesStart::new(
                        std::str::from_utf8(e.name().as_ref())
                            .unwrap_or("div")
                            .to_string(),
                    );
                    for attr in e.attributes().flatten() {
                        let key = attr.key.as_ref();
                        let value: &str = &String::from_utf8_lossy(&attr.value);
                        if key == b"id" {
                            if plan.matched_id.as_deref() == Some(value) {
                                continue;
                            }
                            rebuilt.push_attribute(attr);
                        } else if key == b"class" {
                            let tokens: Vec<&str> = value.split_whitespace().collect();
                            let kept: Vec<&str> = tokens
                                .iter()
                                .copied()
                                .filter(|t| !plan.matched_classes.iter().any(|c| c == t))
                                .collect();
                            if kept.len() == tokens.len() {
                                rebuilt.push_attribute(attr);
                            } else if !kept.is_empty() {
                                rebuilt.push_attribute(("class", kept.join(" ").as_str()));
                            }
                        } else {
                            rebuilt.push_attribute(attr);
                        }
                    }
                    if matches!(event, Event::Empty(_)) {
                        writer.write_event(Event::Empty(rebuilt))?;
                    } else {
                        writer.write_event(Event::Start(rebuilt))?;
                    }
                    skip_emit = true;
                }
            }
            Event::End(e) => {
                let lower = e.name().as_ref().to_ascii_lowercase();
                if lower == b"body" && body_depth > 0 {
                    body_depth -= 1;
                }
            }
            _ => {}
        }
        if !skip_emit {
            writer.write_event(event)?;
        }
        buf.clear();
    }

    Ok(writer.into_inner().into_inner())
}

/// The element the background rules matched: the first one in document
/// order, the same element [`find_full_page_background`] picked as the
/// candidate.
fn is_page_element(e: &BytesStart, plan: &FullPagePlan) -> bool {
    for attr in e.attributes().flatten() {
        match attr.key.as_ref() {
            b"id" => {
                if plan.matched_id.as_deref() == Some(String::from_utf8_lossy(&attr.value).as_ref())
                {
                    return true;
                }
            }
            b"class" => {
                let value = String::from_utf8_lossy(&attr.value);
                if value
                    .split_whitespace()
                    .any(|t| plan.matched_classes.iter().any(|c| c == t))
                {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// Put the page's SVG in front of the text and hide the text.
///
/// Runs over the kepubified document: the head gains the page-filling rules,
/// the body gains the SVG right inside `#book-inner`, and everything the
/// body already held goes into a `hidden` wrapper after it — still there for
/// search and TTS, invisible to the renderer.
fn add_full_page_svg(
    input: &[u8],
    plan: &FullPagePlan,
    width: u32,
    height: u32,
) -> Result<Vec<u8>> {
    let mut reader = Reader::from_reader(input);
    let mut writer: Writer<Cursor<Vec<u8>>> = Writer::new(Cursor::new(Vec::new()));
    let mut buf = Vec::new();
    let mut head_style_emitted = false;
    let mut body_depth = 0u32;
    // Elements open inside the body; the wrapper pair sits at the bottom of
    // the stack, so the close that leaves one element open is `#book-inner`'s.
    let mut open: Vec<Vec<u8>> = Vec::new();
    let mut hidden_opened = false;

    let body = format!(
        concat!(
            r#"<div class="kobo-fullpage">"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink""#,
            r#" version="1.1" viewBox="0 0 {w} {h}" preserveAspectRatio="xMidYMid meet">"#,
            r#"<image width="{w}" height="{h}" xlink:href="{src}"/>"#,
            r#"</svg></div><div class="kobo-page-text" hidden="hidden">"#,
        ),
        w = width,
        h = height,
        src = quick_xml::escape::escape(&plan.href),
    );

    loop {
        let event = reader.read_event_into(&mut buf)?;
        match event {
            Event::Eof => break,
            Event::Start(e) => {
                let lower = e.name().as_ref().to_ascii_lowercase();
                let in_body = body_depth > 0;
                if in_body
                    && !hidden_opened
                    && lower == b"div"
                    && attr_value(&e, b"id").as_deref() == Some("book-inner")
                {
                    writer.write_event(Event::Start(e))?;
                    writer.write_event(Event::Text(BytesText::from_escaped(body.as_str())))?;
                    hidden_opened = true;
                    open.push(lower);
                } else {
                    writer.write_event(Event::Start(e))?;
                    if lower == b"body" {
                        body_depth += 1;
                    }
                    if in_body {
                        open.push(lower);
                    }
                }
            }
            Event::End(e) => {
                let lower = e.name().as_ref().to_ascii_lowercase();
                if body_depth > 0 && lower == b"body" {
                    body_depth -= 1;
                    writer.write_event(Event::End(e))?;
                } else if body_depth > 0 {
                    let closing_inner = open.len() == 2 && open.get(1) == Some(&b"div".to_vec());
                    if closing_inner {
                        writer.write_event(Event::Text(BytesText::from_escaped("</div>")))?;
                    }
                    pop_resync(&mut open, &lower);
                    writer.write_event(Event::End(e))?;
                } else if lower == b"head" && !head_style_emitted {
                    emit_full_page_style(&mut writer)?;
                    head_style_emitted = true;
                    writer.write_event(Event::End(e))?;
                } else {
                    writer.write_event(Event::End(e))?;
                }
            }
            other => writer.write_event(other)?,
        }
        buf.clear();
    }

    Ok(writer.into_inner().into_inner())
}

/// Pop the element a close ends, resyncing by name when a malformed document
/// ends something else: a wrapper the stack lost track of must not swallow
/// the hidden div's close.
fn pop_resync(open: &mut Vec<Vec<u8>>, name: &[u8]) {
    match open.pop() {
        Some(top) if top == name => {}
        Some(_) => {
            if let Some(pos) = open.iter().rposition(|n| n == name) {
                open.truncate(pos);
            }
        }
        None => {}
    }
}

fn attr_value(e: &BytesStart, key: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == key)
        .map(|a| String::from_utf8_lossy(&a.value).into_owned())
}

fn emit_full_page_style<W: Write>(writer: &mut Writer<W>) -> Result<()> {
    let mut style = BytesStart::new("style");
    style.push_attribute(("type", "text/css"));
    style.push_attribute(("class", "kobostylehacks"));
    writer.write_event(Event::Start(style))?;
    writer.write_event(Event::Text(BytesText::new(FULL_PAGE_IMAGE_CSS)))?;
    writer.write_event(Event::End(BytesEnd::new("style")))?;
    Ok(())
}

fn transform_xhtml(input: &[u8], doc_path: &str, ctx: &KepubContext) -> Result<Vec<u8>> {
    // A KF8 fixed-layout page: the whole page is a CSS background and the
    // text is an overlay on it. Rewrite it to the scaling-SVG idiom the
    // renderer already understands, keeping the text hidden for search and
    // TTS. See [`full_page_background_plan`].
    if let Some(plan) = full_page_background_plan(
        input,
        doc_path,
        &ctx.image_dims,
        &ctx.css_rules,
        &ctx.archive_names,
    ) {
        let (w, h) = plan.size;
        let neutralized = strip_background_identity(input, &plan)?;
        let kepubified = kepubify_document(&neutralized)?;
        return add_full_page_svg(&kepubified, &plan, w, h);
    }

    // Ahead of the kepubified check: a book that arrived already kepubified
    // still has postage-stamp plate pages, and this is what fixes them.
    if let Some(src) = standalone_image_src(input)?
        && let Some(entry) = resolve_relative(doc_path, &src)
        && let Some(&(w, h)) = ctx.image_dims.get(&entry)
    {
        return write_full_page_image(input, &src, w, h);
    }

    kepubify_document(input)
}

/// The ordinary kepubify pass: koboSpans, the body div wrapper, the margin
/// hack — idempotent for a document that already carries them.
fn kepubify_document(input: &[u8]) -> Result<Vec<u8>> {
    if detect_kepubified(input) {
        return Ok(input.to_vec());
    }
    let mut sink = WriterSink(Writer::new(Cursor::new(Vec::new())));
    walk_kepubify(input, &mut sink)?;
    Ok(sink.0.into_inner().into_inner())
}

/// Receives the kepubify pass as it walks a document, so the koboSpan
/// numbering has exactly one implementation whether the output is a KEPUB
/// or a map of where each span would start.
pub(crate) trait KepubSink {
    /// An event of the source document, passed through unchanged.
    fn source(&mut self, event: Event<'_>) -> Result<()>;
    /// Markup the pass adds that the source never had.
    fn injected(&mut self, event: Event<'_>) -> Result<()>;
    /// A slice of a source text node, wrapped in `kobo.{para}.{seg}` when
    /// `span` is set.
    fn run(&mut self, span: Option<(u32, u32)>, text: &str) -> Result<()>;
    /// A source `<img/>`, wrapped in its own span.
    fn image(&mut self, span: (u32, u32), img: BytesStart<'_>) -> Result<()>;
}

struct WriterSink(Writer<Cursor<Vec<u8>>>);

impl KepubSink for WriterSink {
    fn source(&mut self, event: Event<'_>) -> Result<()> {
        Ok(self.0.write_event(event)?)
    }

    fn injected(&mut self, event: Event<'_>) -> Result<()> {
        Ok(self.0.write_event(event)?)
    }

    fn run(&mut self, span: Option<(u32, u32)>, text: &str) -> Result<()> {
        let text = Event::Text(BytesText::from_escaped(text));
        match span {
            Some((para, seg)) => {
                self.0
                    .write_event(Event::Start(make_kobo_span(para, seg)))?;
                self.0.write_event(text)?;
                self.0.write_event(Event::End(BytesEnd::new("span")))?;
            }
            None => self.0.write_event(text)?,
        }
        Ok(())
    }

    fn image(&mut self, (para, seg): (u32, u32), img: BytesStart<'_>) -> Result<()> {
        self.0
            .write_event(Event::Start(make_kobo_span(para, seg)))?;
        self.0.write_event(Event::Empty(img))?;
        self.0.write_event(Event::End(BytesEnd::new("span")))?;
        Ok(())
    }
}

/// Whether [`transform_xhtml`] numbers this document's text with
/// [`walk_kepubify`] — false when it is already kepubified or becomes a
/// plate page, which carries no spans at all.
pub(crate) fn numbers_spans(input: &[u8], doc_path: &str, ctx: &KepubContext) -> Result<bool> {
    if detect_kepubified(input) {
        return Ok(false);
    }
    if full_page_background_plan(
        input,
        doc_path,
        &ctx.image_dims,
        &ctx.css_rules,
        &ctx.archive_names,
    )
    .is_some()
    {
        return Ok(true);
    }
    let plate = standalone_image_src(input)?
        .and_then(|src| resolve_relative(doc_path, &src))
        .is_some_and(|entry| ctx.image_dims.contains_key(&entry));
    Ok(!plate)
}

pub(crate) fn walk_kepubify<S: KepubSink>(input: &[u8], sink: &mut S) -> Result<()> {
    let mut reader = Reader::from_reader(input);
    let mut buf = Vec::new();
    let mut state = ContentState::default();
    let mut head_style_emitted = false;

    loop {
        let event = reader.read_event_into(&mut buf)?;
        match event {
            Event::Eof => break,
            Event::Start(e) => {
                let lower = e.name().as_ref().to_ascii_lowercase();
                sink.source(Event::Start(e))?;
                if lower == b"body" {
                    state.body_depth += 1;
                    let mut book_columns = BytesStart::new("div");
                    book_columns.push_attribute(("id", "book-columns"));
                    sink.injected(Event::Start(book_columns))?;
                    let mut book_inner = BytesStart::new("div");
                    book_inner.push_attribute(("id", "book-inner"));
                    sink.injected(Event::Start(book_inner))?;
                } else if state.body_depth > 0 {
                    if is_skip_tag(&lower) {
                        state.skip_depth += 1;
                    } else if is_para_trigger(&lower) {
                        state.inc_para_next = true;
                    }
                }
            }
            Event::End(e) => {
                let lower = e.name().as_ref().to_ascii_lowercase();
                if lower == b"head" && !head_style_emitted {
                    // Match pgaskin/kepubify: <style type="text/css" class="kobostylehacks">.
                    let mut style = BytesStart::new("style");
                    style.push_attribute(("type", "text/css"));
                    style.push_attribute(("class", "kobostylehacks"));
                    sink.injected(Event::Start(style))?;
                    sink.injected(Event::Text(BytesText::new(KOBO_STYLE_HACK)))?;
                    sink.injected(Event::End(BytesEnd::new("style")))?;
                    head_style_emitted = true;
                }
                if lower == b"body" && state.body_depth > 0 {
                    sink.injected(Event::End(BytesEnd::new("div")))?;
                    sink.injected(Event::End(BytesEnd::new("div")))?;
                    state.body_depth -= 1;
                }
                if state.body_depth > 0 && is_skip_tag(&lower) && state.skip_depth > 0 {
                    state.skip_depth -= 1;
                }
                sink.source(Event::End(e))?;
            }
            Event::Empty(e) => {
                let lower = e.name().as_ref().to_ascii_lowercase();
                if state.body_depth > 0 && state.skip_depth == 0 && lower == b"img" {
                    state.para += 1;
                    state.seg = 1;
                    state.inc_para_next = false;
                    sink.image((state.para, state.seg), e)?;
                } else {
                    sink.source(Event::Empty(e))?;
                }
            }
            Event::Text(t) if state.body_depth > 0 && state.skip_depth == 0 => {
                let raw = t.decode().map_err(quick_xml::Error::from)?;
                emit_text_runs(sink, &mut state, raw.as_ref())?;
            }
            other => sink.source(other)?,
        }
        buf.clear();
    }
    Ok(())
}

fn make_kobo_span(para: u32, seg: u32) -> BytesStart<'static> {
    let mut span = BytesStart::new("span");
    span.push_attribute(("class", "koboSpan"));
    span.push_attribute(("id", format!("kobo.{para}.{seg}").as_str()));
    span
}

fn emit_text_runs<S: KepubSink>(
    sink: &mut S,
    state: &mut ContentState,
    raw_escaped: &str,
) -> Result<()> {
    for run in split_runs(raw_escaped) {
        if run.is_empty() {
            continue;
        }
        if run.bytes().all(|b| b.is_ascii_whitespace()) {
            sink.run(None, run)?;
        } else {
            if state.inc_para_next {
                state.para += 1;
                state.seg = 0;
                state.inc_para_next = false;
            }
            state.seg += 1;
            sink.run(Some((state.para, state.seg)), run)?;
        }
    }
    Ok(())
}

/// Split a text run on sentence boundaries: a sentence is "non-terminator chars
/// up to and including the next `[.!?;]`, optionally followed by one extra
/// closing-punctuation char (ASCII `")]}'` or any Unicode close/final/quote
/// punctuation), then any trailing whitespace before the next non-whitespace".
/// Whitespace-only runs survive as their own segments. Operates on raw
/// (still-escaped) XML text — `&amp;` etc. are passed through, which is fine
/// since sentence terminators are ASCII.
fn split_runs(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }

    fn is_extra_punct(c: char) -> bool {
        // Matches pgaskin/kepubify's InputExtra: any closing or quotation-style
        // punctuation that should glue to the preceding sentence terminator.
        matches!(c, '"' | '\'' | ')' | ']' | '}')
            || matches!(
                c,
                '\u{2018}' // ‘
                    | '\u{2019}' // ’
                    | '\u{201C}' // “
                    | '\u{201D}' // ”
                    | '\u{00BB}' // »
                    | '\u{300D}' // 」
                    | '\u{300F}' // 』
            )
    }

    let mut out = Vec::new();
    let mut indices = text.char_indices().peekable();
    let mut start = 0usize;
    while let Some((i, c)) = indices.next() {
        if matches!(c, '.' | '!' | '?' | ';') {
            // Look ahead: optionally consume one extra punct, then any whitespace.
            let mut end = i + c.len_utf8();
            if let Some(&(j, nc)) = indices.peek()
                && is_extra_punct(nc)
            {
                indices.next();
                end = j + nc.len_utf8();
            }
            while let Some(&(j, nc)) = indices.peek() {
                if nc.is_whitespace() {
                    indices.next();
                    end = j + nc.len_utf8();
                } else {
                    break;
                }
            }
            out.push(&text[start..end]);
            start = end;
        }
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_runs_basic() {
        let runs = split_runs("Hello world. Foo bar! Baz?");
        assert_eq!(runs, vec!["Hello world. ", "Foo bar! ", "Baz?"]);
    }

    #[test]
    fn split_runs_keeps_trailing_punct() {
        let runs = split_runs("She said \"go.\" Then left.");
        assert_eq!(runs, vec!["She said \"go.\" ", "Then left."]);
    }

    #[test]
    fn split_runs_whitespace_only() {
        let runs = split_runs("   ");
        assert_eq!(runs, vec!["   "]);
    }

    #[test]
    fn detect_kepubified_positive() {
        assert!(detect_kepubified(
            br#"<span class="koboSpan" id="kobo.1.1">Hi</span>"#
        ));
    }

    #[test]
    fn detect_kepubified_negative() {
        assert!(!detect_kepubified(b"<p>Plain XHTML</p>"));
    }

    #[test]
    fn xhtml_transform_inserts_spans_and_divs() {
        let input = br#"<?xml version="1.0" encoding="utf-8"?>
<html xmlns="http://www.w3.org/1999/xhtml"><head><title>x</title></head><body><p>Hello world. Foo!</p></body></html>"#;
        let out = transform_xhtml(
            input,
            "OEBPS/t.xhtml",
            &test_ctx(&HashMap::new(), &HashMap::new(), &HashSet::new()),
        )
        .unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(
            s.contains(r#"<div id="book-columns">"#),
            "missing book-columns: {s}"
        );
        assert!(
            s.contains(r#"<div id="book-inner">"#),
            "missing book-inner: {s}"
        );
        assert!(
            s.contains(r#"class="kobostylehacks""#),
            "missing kobostylehacks: {s}"
        );
        assert!(
            s.contains(r#"class="koboSpan" id="kobo.1.1""#),
            "missing first span: {s}"
        );
        assert!(
            s.contains(r#"class="koboSpan" id="kobo.1.2""#),
            "missing second span: {s}"
        );
    }

    #[test]
    fn xhtml_transform_skips_already_kepubified() {
        let input = br#"<?xml version="1.0"?>
<html><body><p><span class="koboSpan" id="kobo.1.1">Hi</span></p></body></html>"#;
        let out = transform_xhtml(
            input,
            "OEBPS/t.xhtml",
            &test_ctx(&HashMap::new(), &HashMap::new(), &HashSet::new()),
        )
        .unwrap();
        // Should be byte-identical when already kepubified.
        assert_eq!(out, input);
    }

    /// A plate page: one image, no prose. The publisher CSS caps it at
    /// `max-width: 100%`, which never enlarges, so this is the page that
    /// renders as a postage stamp before the rewrite.
    fn plate_page() -> &'static [u8] {
        br#"<?xml version="1.0" encoding="utf-8"?>
<html xmlns="http://www.w3.org/1999/xhtml"><head><title>Map</title>
<link href="../styles/stylesheet.css" rel="stylesheet" type="text/css"/></head>
<body><div class="body"><p class="CN"><img alt="Map of Roshar" src="../images/Ill10.jpg"/></p></div></body></html>"#
    }

    fn dims(entry: &str, w: u32, h: u32) -> HashMap<String, (u32, u32)> {
        HashMap::from([(entry.to_string(), (w, h))])
    }

    #[test]
    fn standalone_image_page_becomes_a_scaling_svg() {
        let out = transform_xhtml(
            plate_page(),
            "OEBPS/xhtml/ill10.xhtml",
            &test_ctx(
                &dims("OEBPS/images/Ill10.jpg", 1638, 2441),
                &HashMap::new(),
                &HashSet::new(),
            ),
        )
        .unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains(r#"viewBox="0 0 1638 2441""#), "no viewBox: {s}");
        assert!(
            s.contains(r#"preserveAspectRatio="xMidYMid meet""#),
            "aspect ratio not preserved: {s}"
        );
        assert!(
            s.contains(r#"xlink:href="../images/Ill10.jpg""#),
            "image src not carried over: {s}"
        );
        assert!(s.contains("div.kobo-fullpage"), "no full-page css: {s}");
        // The head is the book's own, untouched.
        assert!(s.contains("<title>Map</title>"), "lost the title: {s}");
        assert!(
            s.contains(r#"href="../styles/stylesheet.css""#),
            "lost the stylesheet link: {s}"
        );
        // The original <img> is gone — the SVG replaces it, it does not join it.
        assert!(!s.contains("<img"), "left the bare img behind: {s}");
    }

    #[test]
    fn standalone_image_page_is_fixed_even_when_already_kepubified() {
        let input = br#"<?xml version="1.0" encoding="utf-8"?>
<html xmlns="http://www.w3.org/1999/xhtml"><head><title>Map</title></head>
<body><div id="book-columns"><div id="book-inner"><p class="CN"><span class="koboSpan" id="kobo.1.1"><img src="../images/Ill10.jpg"/></span></p></div></div></body></html>"#;
        let out = transform_xhtml(
            input,
            "OEBPS/xhtml/ill10.xhtml",
            &test_ctx(
                &dims("OEBPS/images/Ill10.jpg", 1638, 2441),
                &HashMap::new(),
                &HashSet::new(),
            ),
        )
        .unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(
            s.contains(r#"viewBox="0 0 1638 2441""#),
            "already-kepubified plate page was left alone: {s}"
        );
    }

    #[test]
    fn image_among_prose_is_left_alone() {
        // The Ars Arcanum chart: a small image surrounded by explanatory text.
        // Blowing this one up would be wrong.
        let input = br#"<?xml version="1.0" encoding="utf-8"?>
<html xmlns="http://www.w3.org/1999/xhtml"><head><title>x</title></head>
<body><p class="centerimage1"><img src="Images/chart.jpg" class="epubimage"/></p>
<p class="para">The preceding list is an imperfect gathering of traditional Vorin symbolism.</p></body></html>"#;
        let out = transform_xhtml(
            input,
            "OEBPS/bm02.xhtml",
            &test_ctx(
                &dims("OEBPS/Images/chart.jpg", 362, 335),
                &HashMap::new(),
                &HashSet::new(),
            ),
        )
        .unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(!s.contains("<svg"), "rewrote an illustrated text page: {s}");
        assert!(s.contains("<img"), "lost the inline image: {s}");
        assert!(s.contains(r#"class="koboSpan""#), "lost the spans: {s}");
    }

    #[test]
    fn page_with_two_images_is_left_alone() {
        let input = br#"<?xml version="1.0"?>
<html><head><title>x</title></head><body><p><img src="a.jpg"/><img src="b.jpg"/></p></body></html>"#;
        let out = transform_xhtml(
            input,
            "t.xhtml",
            &test_ctx(&dims("a.jpg", 100, 100), &HashMap::new(), &HashSet::new()),
        )
        .unwrap();
        assert!(!String::from_utf8(out).unwrap().contains("<svg"));
    }

    #[test]
    fn page_whose_image_cannot_be_measured_is_left_alone() {
        // No entry in the dimension map — an SVG without a viewBox would not
        // scale, so the ordinary transform is the safer outcome.
        let out = transform_xhtml(
            plate_page(),
            "OEBPS/xhtml/ill10.xhtml",
            &test_ctx(&HashMap::new(), &HashMap::new(), &HashSet::new()),
        )
        .unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(!s.contains("<svg"), "rewrote an unmeasurable image: {s}");
        assert!(s.contains("<img"), "lost the image: {s}");
    }

    #[test]
    fn a_folio_number_still_counts_as_a_plate_page() {
        let input = br#"<?xml version="1.0"?>
<html><head><title>x</title></head><body><p><img src="a.jpg"/></p><p class="folio">447</p></body></html>"#;
        let out = transform_xhtml(
            input,
            "t.xhtml",
            &test_ctx(&dims("a.jpg", 800, 1200), &HashMap::new(), &HashSet::new()),
        )
        .unwrap();
        assert!(String::from_utf8(out).unwrap().contains("<svg"));
    }

    /// The smallest zip that `transform_epub` will accept: a container
    /// pointing at an OPF, and one XHTML document in the manifest.
    fn test_ctx(
        dims: &HashMap<String, (u32, u32)>,
        rules: &HashMap<String, Vec<CssRule>>,
        names: &HashSet<String>,
    ) -> KepubContext {
        KepubContext {
            image_dims: dims.clone(),
            css_rules: rules.clone(),
            archive_names: names.clone(),
        }
    }

    fn minimal_epub(doc: &str) -> Vec<u8> {
        use zip::write::SimpleFileOptions;
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default();
        let files = [
            ("mimetype", "application/epub+zip".to_string()),
            (
                "META-INF/container.xml",
                r#"<?xml version="1.0"?><container xmlns="urn:oasis:names:tc:opendocument:xmlns:container" version="1.0"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#.to_string(),
            ),
            (
                "OEBPS/content.opf",
                r#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0"><manifest><item id="c1" href="c1.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="c1"/></spine></package>"#.to_string(),
            ),
            ("OEBPS/c1.xhtml", doc.to_string()),
        ];
        for (name, body) in files {
            zip.start_file(name, opts).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn transform_stamps_the_current_generation() {
        let epub = minimal_epub(
            r#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><head><title>x</title></head><body><p>Hello.</p></body></html>"#,
        );
        assert_eq!(read_generation(&epub), 0, "input should carry no stamp");

        let kepub = transform_epub(&epub).unwrap();
        assert_eq!(read_generation(&kepub), crate::KEPUB_GENERATION);
    }

    #[test]
    fn re_running_replaces_an_older_stamp_rather_than_duplicating_it() {
        let epub = minimal_epub(
            r#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><head><title>x</title></head><body><p>Hello.</p></body></html>"#,
        );
        let once = transform_epub(&epub).unwrap();
        let twice = transform_epub(&once).unwrap();

        assert_eq!(read_generation(&twice), crate::KEPUB_GENERATION);
        // One stamp entry, not two — a zip may legally hold duplicate names,
        // and a second copy would make the read order decide the answer.
        let mut archive = zip::ZipArchive::new(Cursor::new(&twice)).unwrap();
        let stamps = (0..archive.len())
            .filter(|i| archive.by_index(*i).unwrap().name() == GENERATION_STAMP_PATH)
            .count();
        assert_eq!(stamps, 1, "expected exactly one stamp entry");
    }

    #[test]
    fn an_already_kepubified_book_still_gets_stamped() {
        // Its documents pass through the XHTML transform untouched, so the
        // stamp has to come from the archive pass or it would read as 0 for
        // ever and look permanently out of date.
        let epub = minimal_epub(
            r#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><head><title>x</title></head><body><div id="book-columns"><div id="book-inner"><p><span class="koboSpan" id="kobo.1.1">Hi.</span></p></div></div></body></html>"#,
        );
        let kepub = transform_epub(&epub).unwrap();
        assert_eq!(read_generation(&kepub), crate::KEPUB_GENERATION);
    }

    #[test]
    fn generation_of_a_non_zip_is_zero() {
        assert_eq!(read_generation(b"not a zip at all"), 0);
    }

    #[test]
    fn resolve_relative_walks_out_of_the_document_directory() {
        assert_eq!(
            resolve_relative("OEBPS/xhtml/ill10.xhtml", "../images/Ill10.jpg").as_deref(),
            Some("OEBPS/images/Ill10.jpg")
        );
        assert_eq!(
            resolve_relative("OEBPS/p1.xhtml", "Images/chart.jpg").as_deref(),
            Some("OEBPS/Images/chart.jpg")
        );
        assert_eq!(
            resolve_relative("OEBPS/p1.xhtml", "images/a%20b.jpg").as_deref(),
            Some("OEBPS/images/a b.jpg")
        );
        assert_eq!(
            resolve_relative("OEBPS/p1.xhtml", "data:image/png;base64,x"),
            None
        );
        assert_eq!(
            resolve_relative("OEBPS/p1.xhtml", "https://x.test/a.jpg"),
            None
        );
    }

    #[test]
    fn xhtml_transform_skips_script_text() {
        let input = br#"<?xml version="1.0"?>
<html><body><script>var x = "no spans here.";</script><p>Real text.</p></body></html>"#;
        let out = transform_xhtml(
            input,
            "OEBPS/t.xhtml",
            &test_ctx(&HashMap::new(), &HashMap::new(), &HashSet::new()),
        )
        .unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(
            !s.contains(r#"kobo.1.1">var"#),
            "wrapped script content: {s}"
        );
        assert!(
            s.contains(r#"class="koboSpan""#),
            "no spans on body text: {s}"
        );
    }
    // --- Full-page CSS background (KF8 fixed layout) ---

    /// The KF8 shape as boko exports it: the page is a div whose id carries
    /// the background url and whose class carries the page size, with the
    /// text nested inside.
    fn kf8_page_doc() -> &'static [u8] {
        br#"<?xml version="1.0" encoding="utf-8"?>
 <html xmlns="http://www.w3.org/1999/xhtml"><head><title>Page 10</title>
 <link rel="stylesheet" type="text/css" href="styles/style0000.css"/>
 <meta name="viewport" content="width=600, height=914"/></head>
 <body><div class="fs"><div id="KF_012_img" class="lpage">
 <div style="position: relative;overflow: hidden;width:600;height:914;">
 <div class="page_10_1"><span class="justify">And that's when I see him.</span></div>
 </div></div></div></body></html>"#
    }

    fn kf8_css() -> &'static str {
        concat!(
            "#KF_012_img { background-image: url(images/image_0017.jpg); }\n",
            "div.lpage { position: absolute; height: 914px; width: 600px; background-size: 600px 914px; }\n",
        )
    }

    fn kf8_rules() -> HashMap<String, Vec<CssRule>> {
        HashMap::from([(
            "OEBPS/styles/style0000.css".to_string(),
            scan_css_rules(kf8_css()),
        )])
    }

    fn kf8_names() -> HashSet<String> {
        HashSet::from([
            "OEBPS/styles/style0000.css".to_string(),
            "OEBPS/images/image_0017.jpg".to_string(),
        ])
    }

    #[test]
    fn kf8_page_becomes_a_scaling_svg() {
        let out = transform_xhtml(
            kf8_page_doc(),
            "OEBPS/part0010.html",
            &test_ctx(&HashMap::new(), &kf8_rules(), &kf8_names()),
        )
        .unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(
            s.contains(r#"viewBox="0 0 600 914""#),
            "no page viewBox: {s}"
        );
        assert!(
            s.contains(r#"xlink:href="images/image_0017.jpg""#),
            "background url not carried into the svg: {s}"
        );
        assert!(
            s.contains(r#"<div class="kobo-page-text" hidden="hidden">"#),
            "text not kept in a hidden wrapper: {s}"
        );
        assert!(
            s.contains("And that's when I see him."),
            "lost the page text: {s}"
        );
        // The identity the stylesheet keys on is gone, or the CSS would draw
        // the page a second time under the SVG.
        assert!(!s.contains(r#"id="KF_012_img""#), "id not stripped: {s}");
        assert!(!s.contains(r#"class="lpage""#), "class not stripped: {s}");
        // The text stayed koboSpanned for search and TTS.
        assert!(s.contains(r#"class="koboSpan""#), "no spans: {s}");
    }

    #[test]
    fn kf8_page_without_viewport_uses_the_image_dimensions() {
        let doc = br#"<?xml version="1.0"?>
 <html><head><title>P</title>
 <link rel="stylesheet" type="text/css" href="styles/style0000.css"/></head>
 <body><div id="KF_012_img" class="lpage"></div></body></html>"#;
        let out = transform_xhtml(
            doc,
            "OEBPS/part0010.html",
            &test_ctx(
                &dims("OEBPS/images/image_0017.jpg", 600, 914),
                &kf8_rules(),
                &kf8_names(),
            ),
        )
        .unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains(r#"viewBox="0 0 600 914""#), "no viewBox: {s}");
    }

    #[test]
    fn kf8_background_smaller_than_the_page_is_left_alone() {
        // A url that does not cover the page is a decorative corner, not a
        // page: rewriting it would crop the page down to the corner.
        let css = concat!(
            "#KF_012_img { background-image: url(images/image_0017.jpg); }\n",
            "div.lpage { position: absolute; height: 457px; width: 300px; }\n",
        );
        let rules = HashMap::from([(
            "OEBPS/styles/style0000.css".to_string(),
            scan_css_rules(css),
        )]);
        let out = transform_xhtml(
            kf8_page_doc(),
            "OEBPS/part0010.html",
            &test_ctx(&HashMap::new(), &rules, &kf8_names()),
        )
        .unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(!s.contains("<svg"), "rewrote a sub-page background: {s}");
        assert!(
            s.contains(r#"id="KF_012_img""#),
            "stripped an unclaimed id: {s}"
        );
    }

    #[test]
    fn kf8_background_without_a_declared_size_is_left_alone() {
        // The element's own rules are the evidence the background is the
        // whole page. A url with no size anywhere claims nothing.
        let css = "#KF_012_img { background-image: url(images/image_0017.jpg); }\n";
        let rules = HashMap::from([(
            "OEBPS/styles/style0000.css".to_string(),
            scan_css_rules(css),
        )]);
        let out = transform_xhtml(
            kf8_page_doc(),
            "OEBPS/part0010.html",
            &test_ctx(&HashMap::new(), &rules, &kf8_names()),
        )
        .unwrap();
        assert!(!String::from_utf8(out).unwrap().contains("<svg"));
    }

    #[test]
    fn kf8_page_text_as_a_sibling_is_hidden_too() {
        // Some exporters float the text beside the page div instead of
        // nesting it inside; the hidden wrapper must cover it either way.
        let doc = br#"<?xml version="1.0"?>
 <html><head><title>P</title>
 <link rel="stylesheet" type="text/css" href="styles/style0000.css"/>
 <meta name="viewport" content="width=600, height=914"/></head>
 <body><div id="KF_012_img" class="lpage"></div>
 <div class="text">Beside the page.</div></body></html>"#;
        let out = transform_xhtml(
            doc,
            "OEBPS/part0010.html",
            &test_ctx(&HashMap::new(), &kf8_rules(), &kf8_names()),
        )
        .unwrap();
        let s = String::from_utf8(out).unwrap();
        let hidden = s
            .find(r#"<div class="kobo-page-text" hidden="hidden">"#)
            .unwrap();
        let text = s.find("Beside the page.").unwrap();
        assert!(
            text > hidden,
            "sibling text not inside the hidden wrapper: {s}"
        );
    }

    #[test]
    fn kf8_rewrite_is_idempotent() {
        let once = transform_xhtml(
            kf8_page_doc(),
            "OEBPS/part0010.html",
            &test_ctx(&HashMap::new(), &kf8_rules(), &kf8_names()),
        )
        .unwrap();
        let twice = transform_xhtml(
            &once,
            "OEBPS/part0010.html",
            &test_ctx(&HashMap::new(), &kf8_rules(), &kf8_names()),
        )
        .unwrap();
        assert_eq!(once, twice, "second pass rewrote the first pass's output");
    }
}
