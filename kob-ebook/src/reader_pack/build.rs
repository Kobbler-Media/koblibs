use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io::{Cursor, Read};
use std::sync::Arc;

use boko::model::{AnchorTarget, TocEntry as BokoToc};
use boko::{Chapter as BokoChapter, Format, NodeId};
use zip::ZipArchive;

use super::blocks::{BookCx, ChapterSource, Notes, flatten_chapter, holds_blocks};
use super::fixed_page;
use super::kobo::{self, FixedPageSpans};
use super::model::{
    Block, BlockKind, BlockPos, Chapter, ChapterEntry, FixedLayout, FontFace, Link, LinkTarget,
    Manifest, Position, TocEntry,
};
use super::notes;
use super::opf;
use super::sidecar::{ChapterSidecar, push_step};
use super::source;
use super::styles::StyleTable;
use super::{READER_PACK_GENERATION, ReaderPack};
use crate::kepub::{self, KepubContext};
use crate::{EbookError, Result};

struct SpineItem {
    path: String,
    source: String,
    cfi: String,
    fixed_layout: Option<FixedLayout>,
    raw: Vec<u8>,
    boko: u32,
}

fn read_entry<R: Read + std::io::Seek>(zip: &mut ZipArchive<R>, name: &str) -> Option<Vec<u8>> {
    let mut entry = zip.by_name(name).ok()?;
    let mut buf = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut buf).ok()?;
    Some(buf)
}

pub(crate) fn build(epub: &[u8]) -> Result<ReaderPack> {
    let mut zip = ZipArchive::new(Cursor::new(epub))?;
    let names = zip
        .file_names()
        .map(|name| name.map(Cow::into_owned))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let name_set: HashSet<&str> = names.iter().map(String::as_str).collect();
    let opf_path = kepub::read_opf_path(&mut zip)?;
    let opf_bytes = read_entry(&mut zip, &opf_path).ok_or_else(|| {
        EbookError::EpubStructure(format!("OPF file not found in zip: {opf_path}"))
    })?;
    let pkg = opf::parse(&opf_bytes)?;
    let ctx = KepubContext::read(&mut zip);

    let book = boko::Book::from_bytes(epub, Format::Epub)?;
    let resolved = book.resolve_links()?;
    let mut chapters: Vec<Arc<BokoChapter>> = Vec::with_capacity(book.spine().len());
    let mut by_path: HashMap<String, VecDeque<u32>> = HashMap::new();
    for entry in book.spine() {
        chapters.push(book.load_chapter_cached(entry.id)?);
        if let Some(path) = book.source_id(entry.id) {
            by_path
                .entry(path.to_string())
                .or_default()
                .push_back(entry.id.0);
        }
    }

    let mut items = Vec::new();
    for itemref in &pkg.itemrefs {
        if itemref.idref.trim().is_empty() {
            continue;
        }
        let Some(href) = pkg.manifest.get(&itemref.idref) else {
            continue;
        };
        let Some(wanted) = kepub::resolve_relative(&opf_path, href) else {
            continue;
        };
        let path = if name_set.contains(wanted.as_str()) {
            wanted
        } else {
            match names.iter().find(|n| n.eq_ignore_ascii_case(&wanted)) {
                Some(n) => n.clone(),
                None => continue,
            }
        };
        let Some(raw) = read_entry(&mut zip, &path) else {
            continue;
        };
        let boko = match by_path.get_mut(&path).and_then(VecDeque::pop_front) {
            Some(id) => id,
            None => {
                chapters.push(Arc::new(boko::compile_html(
                    &String::from_utf8_lossy(&raw),
                    &[],
                )));
                chapters.len() as u32 - 1
            }
        };
        items.push(SpineItem {
            source: kepub::percent_decode(href.split('#').next().unwrap_or(href)),
            path,
            cfi: itemref.cfi.clone(),
            fixed_layout: pkg.fixed_layout(&itemref.properties),
            raw,
            boko,
        });
    }

    let faces = book.font_faces();
    let mut styles = StyleTable::new(faces.iter().map(|f| f.font_family.clone()).collect());
    let zip = RefCell::new(zip);
    let mut sources: HashMap<u32, ChapterSource> = HashMap::new();
    let mut fixed_pages = HashMap::new();
    for item in &items {
        let doc = source::scan(&item.raw);
        let linked = |href: &str| {
            let path = kepub::resolve_relative(&item.path, href)?;
            let bytes = read_entry(&mut zip.borrow_mut(), &path)?;
            Some(String::from_utf8_lossy(&bytes).into_owned())
        };
        let resolve = |href: &str| {
            kepub::resolve_relative(&item.path, href).unwrap_or_else(|| href.to_string())
        };
        let intrinsic = |src: &str| ctx.image_dims.get(src).copied();
        let fixed = fixed_page::extract(&item.raw, &linked, &resolve, &intrinsic, &mut styles);
        let excluded = fixed
            .as_ref()
            .map(|f| f.excluded.to_vec())
            .unwrap_or_default();
        if let Some(f) = fixed {
            fixed_pages.insert(item.boko, f);
        }
        let holds_blocks = holds_blocks(&chapters[item.boko as usize]);
        sources.insert(
            item.boko,
            ChapterSource {
                doc,
                excluded,
                holds_blocks,
            },
        );
    }

    let note_index = notes::index(&chapters, &resolved);
    let cx = BookCx {
        chapters: &chapters,
        sources: &sources,
        resolved: &resolved,
        notes: &note_index,
        image_dims: &ctx.image_dims,
    };

    let mut packs = Vec::with_capacity(items.len());
    let mut sidecars = Vec::with_capacity(items.len());
    for item in &items {
        let mut notes = Notes::default();
        let mut flat = flatten_chapter(&cx, item.boko, &mut styles, &mut notes);
        let fixed = fixed_pages.remove(&item.boko);
        if let Some(f) = &fixed {
            flat.blocks.insert(
                0,
                Block {
                    kind: BlockKind::FixedPage,
                    fixed: Some(f.page.clone()),
                    ..Block::default()
                },
            );
            flat.segments.insert(0, Vec::new());
            flat.elements = std::iter::once((0, steps_string(&f.fullpage)))
                .chain(flat.elements.into_iter().map(|(b, p)| (b + 1, p)))
                .collect();
            for (_, pos) in &mut flat.anchors {
                pos.block += 1;
            }
        }
        let last = flat.blocks.len().saturating_sub(1) as u32;
        let mut anchors = BTreeMap::new();
        for (id, pos) in flat.anchors {
            let pos = if pos.block > last {
                BlockPos {
                    block: last,
                    offset: 0,
                }
            } else {
                pos
            };
            anchors.entry(id).or_insert(pos);
        }
        let mut sidecar = ChapterSidecar::new(
            item.cfi.clone(),
            item.source.clone(),
            flat.segments,
            flat.elements,
        );
        let source = &sources[&item.boko];
        sidecar.kobo = kobo::map(
            &item.raw,
            &item.path,
            &source.doc,
            &ctx,
            &sidecar,
            &flat.blocks,
            fixed.as_ref().map(|_| FixedPageSpans {
                block: 0,
                excluded: &source.excluded,
            }),
        );
        sidecars.push(sidecar);
        packs.push(Chapter {
            blocks: flat.blocks,
            anchors,
            notes: notes.blocks,
        });
    }

    let index_of: HashMap<u32, u32> = items
        .iter()
        .enumerate()
        .map(|(i, it)| (it.boko, i as u32))
        .collect();
    let resolver = Resolver {
        chapters: &chapters,
        index_of: &index_of,
        anchors: packs.iter().map(|p| p.anchors.clone()).collect(),
    };
    for pack in &mut packs {
        for block in &mut pack.blocks {
            resolver.patch(block);
        }
        for blocks in pack.notes.values_mut() {
            for block in blocks {
                resolver.patch(block);
            }
        }
    }

    let metadata = book.metadata();
    let ncx_titles = toc_titles(book.toc());
    let book_title = metadata.title.trim().to_string();
    let mut sticky: Option<String> = None;
    let entries = items
        .iter()
        .zip(&sources_titles(&items, &sources))
        .map(|(item, doc_title)| {
            let title = match ncx_titles.get(&item.path.to_lowercase()) {
                Some(t) => {
                    sticky = Some(t.clone());
                    Some(t.clone())
                }
                None => sticky
                    .clone()
                    .or_else(|| doc_title.clone().filter(|t| t.trim() != book_title)),
            };
            ChapterEntry {
                href: item.path.clone(),
                title,
                weight: String::from_utf8_lossy(&item.raw).encode_utf16().count() as u64,
                fixed_layout: item.fixed_layout,
            }
        })
        .collect();

    let mut fonts: Vec<FontFace> = Vec::new();
    for f in faces {
        let face = FontFace {
            family: f.font_family,
            weight: if f.font_weight.0 == 0 {
                400
            } else {
                f.font_weight.0
            },
            italic: f.font_style != boko::style::FontStyle::Normal,
            src: f.src,
        };
        if !fonts.contains(&face) {
            fonts.push(face);
        }
    }

    let manifest = Manifest {
        generation: READER_PACK_GENERATION,
        title: metadata.title.clone(),
        authors: metadata.authors.clone(),
        language: Some(metadata.language.clone()).filter(|l| !l.trim().is_empty()),
        rtl: pkg.rtl,
        chapters: entries,
        toc: toc(book.toc(), &resolver),
        styles: styles.into_styles(),
        fonts,
    };
    Ok(ReaderPack {
        manifest,
        chapters: packs,
        sidecars,
    })
}

fn sources_titles(
    items: &[SpineItem],
    sources: &HashMap<u32, ChapterSource>,
) -> Vec<Option<String>> {
    items
        .iter()
        .map(|it| sources.get(&it.boko).and_then(|s| s.doc.title.clone()))
        .collect()
}

fn steps_string(steps: &[u32]) -> String {
    let mut out = String::new();
    for &s in steps {
        push_step(&mut out, s, None);
    }
    out
}

/// href (lowercased, no fragment) → title; the last entry for an href wins,
/// since Calibre emits a part label and then the chapter's own.
fn toc_titles(toc: &[BokoToc]) -> HashMap<String, String> {
    fn walk(entries: &[BokoToc], out: &mut HashMap<String, String>) {
        for e in entries {
            let title = e.title.trim();
            let href = e.href.split('#').next().unwrap_or("");
            if !title.is_empty() && !href.is_empty() {
                out.insert(href.to_lowercase(), title.to_string());
            }
            walk(&e.children, out);
        }
    }
    let mut out = HashMap::new();
    walk(toc, &mut out);
    out
}

fn toc(entries: &[BokoToc], resolver: &Resolver<'_>) -> Vec<TocEntry> {
    let mut out = Vec::new();
    for e in entries {
        let children = toc(&e.children, resolver);
        let target = match &e.target {
            Some(AnchorTarget::Internal(g)) => resolver.position(LinkTarget {
                chapter: g.chapter.0,
                node: Some(g.node.0),
            }),
            Some(AnchorTarget::Chapter(c)) => resolver.position(LinkTarget {
                chapter: c.0,
                node: None,
            }),
            _ => None,
        };
        match target {
            Some(target) => out.push(TocEntry {
                title: e.title.trim().to_string(),
                target,
                children,
            }),
            None => out.extend(children),
        }
    }
    out
}

struct Resolver<'a> {
    chapters: &'a [Arc<BokoChapter>],
    index_of: &'a HashMap<u32, u32>,
    anchors: Vec<BTreeMap<String, BlockPos>>,
}

impl Resolver<'_> {
    fn position(&self, target: LinkTarget) -> Option<Position> {
        let chapter = *self.index_of.get(&target.chapter)?;
        let at = target
            .node
            .and_then(|n| {
                self.chapters
                    .get(target.chapter as usize)?
                    .semantics
                    .id(NodeId(n))
            })
            .and_then(|id| self.anchors[chapter as usize].get(id))
            .copied()
            .unwrap_or_default();
        Some(Position {
            chapter,
            block: at.block,
            offset: at.offset,
        })
    }

    fn patch(&self, block: &mut Block) {
        for span in &mut block.spans {
            if let Some(target) = span.target.take() {
                span.link = self.position(target).map(|position| Link {
                    position: Some(position),
                    href: None,
                });
            }
        }
        if let Some(table) = &mut block.table {
            for cell in table.rows.iter_mut().flat_map(|r| r.cells.iter_mut()) {
                for b in &mut cell.blocks {
                    self.patch(b);
                }
            }
        }
    }
}
