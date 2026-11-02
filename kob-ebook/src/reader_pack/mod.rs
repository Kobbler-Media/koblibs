//! Reader packs: a book pre-digested for a client that lays text out itself
//! and never parses XHTML or CSS.
//!
//! A pack is a manifest plus one JSON chapter per compacted spine item, each a
//! flat list of leaf blocks addressed by [`Position`]s. Alongside every
//! chapter the server keeps a [`ChapterSidecar`] that maps those positions to
//! and from epubcfi (as foliate-js writes them) and Kobo `koboSpan` ids, so
//! the app, the web reader and a Kobo can share one reading position.

mod blocks;
mod build;
mod fixed_page;
mod kobo;
mod leaf;
mod model;
mod notes;
mod opf;
mod sidecar;
mod source;
mod styles;

use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub use model::{
    Align, Baseline, Block, BlockKind, BlockPos, Cell, Chapter, ChapterEntry, Dropcap, FixedLayout,
    FixedPage, FontFace, Fragment, Frame, Hyphens, Image, Link, Manifest, Position, Row, Span,
    SpreadSide, Table, TextStyle, TocEntry, Transform,
};
pub use sidecar::{ChapterSidecar, KoboEntry, Segment, resolve_cfi};

use crate::Result;

/// Revision of the pack format and of the builder's output. Bump it whenever
/// a rebuilt pack would differ, so stored packs can be found and rebuilt.
pub const READER_PACK_GENERATION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReaderPack {
    pub manifest: Manifest,
    pub chapters: Vec<Chapter>,
    pub sidecars: Vec<ChapterSidecar>,
}

pub fn build(epub: &[u8]) -> Result<ReaderPack> {
    build::build(epub)
}

/// Read an archive entry by the path a pack names it with. Fonts come back
/// de-obfuscated; everything else is read straight from the zip.
pub fn read_asset(epub_path: &Path, name: &str) -> Result<Vec<u8>> {
    let is_font = name.rsplit('.').next().is_some_and(|ext| {
        ["otf", "ttf", "woff", "woff2"]
            .iter()
            .any(|f| ext.eq_ignore_ascii_case(f))
    });
    if is_font {
        return Ok(boko::Book::open_format(epub_path, boko::Format::Epub)?.load_asset(name)?);
    }
    let mut zip = zip::ZipArchive::new(std::fs::File::open(epub_path)?)?;
    let mut entry = zip.by_name(name)?;
    let mut out = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut out)?;
    Ok(out)
}
