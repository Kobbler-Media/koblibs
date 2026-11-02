//! boko's chapter tree → the flat leaf list a client lays out: containers
//! become insets, frames and collapsed vertical gaps on the leaves they hold.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use boko::model::{AnchorTarget, ResolvedLinks};
use boko::style::{BreakValue, Length, ListStyleType};
use boko::{Chapter, ComputedStyle, NodeId, Role, SourceSegment};

use super::leaf::{LeafText, PathCache, SpanAttrs};
use super::model::{Block, BlockKind, BlockPos, Cell, Frame, Image, Link, LinkTarget, Row, Table};
use super::notes::{NoteIndex, global, is_inline_role};
use super::sidecar::Segment;
use super::source::SourceDoc;
use super::styles::{StyleRole, StyleTable, em_or_zero, forced_break};

pub(crate) struct BookCx<'a> {
    pub chapters: &'a [Arc<Chapter>],
    pub sources: &'a HashMap<u32, ChapterSource>,
    pub resolved: &'a ResolvedLinks,
    pub notes: &'a NoteIndex,
    pub image_dims: &'a HashMap<String, (u32, u32)>,
}

#[derive(Debug, Default)]
pub(crate) struct ChapterSource {
    pub doc: SourceDoc,
    pub excluded: Vec<Vec<u32>>,
    pub holds_blocks: Vec<bool>,
}

#[derive(Default)]
pub(crate) struct Notes {
    pub blocks: BTreeMap<String, Vec<Block>>,
    owners: HashMap<String, (u32, u32)>,
}

#[derive(Debug, Default)]
pub(crate) struct Flat {
    pub blocks: Vec<Block>,
    pub segments: Vec<Vec<Segment>>,
    pub elements: BTreeMap<u32, String>,
    pub anchors: Vec<(String, BlockPos)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Flow,
    Note,
    Cell,
}

#[derive(Clone, Copy)]
struct LeafMode {
    dropcap: bool,
    verbatim: bool,
}

struct ListState {
    counter: u32,
    kind: Option<ListStyleType>,
}

struct Saved {
    insets: (f32, f32),
    frames: usize,
    lists: usize,
}

const IMAGE_TINY_EM: f32 = 2.0;
/// The browser default for a list's marker gutter (40px), which boko's user
/// agent sheet leaves out because its own exporters draw none.
const LIST_INSET_EM: f32 = 2.5;
const BLOCK_BREAK_EM: f32 = 1.0;

pub(crate) struct Flattener<'a> {
    book: &'a BookCx<'a>,
    chapter: u32,
    ch: &'a Chapter,
    source: Option<&'a ChapterSource>,
    styles: &'a mut StyleTable,
    notes: &'a mut Notes,
    mode: Mode,
    sources: Vec<SourceSegment>,
    holds_blocks: &'a [bool],
    paths: PathCache,
    out: Flat,
    gap: f32,
    pending_break: bool,
    marker: Option<String>,
    insets: (f32, f32),
    frames: Vec<Frame>,
    lists: Vec<ListState>,
}

pub(crate) fn flatten_chapter(
    book: &BookCx<'_>,
    chapter: u32,
    styles: &mut StyleTable,
    notes: &mut Notes,
) -> Flat {
    let ch = &book.chapters[chapter as usize];
    let mut f = Flattener::new(book, chapter, styles, notes, Mode::Flow);
    f.sources = ch.source.segments().collect();
    f.container(ch.root());
    f.out
}

impl<'a> Flattener<'a> {
    fn new(
        book: &'a BookCx<'a>,
        chapter: u32,
        styles: &'a mut StyleTable,
        notes: &'a mut Notes,
        mode: Mode,
    ) -> Self {
        Self {
            book,
            chapter,
            ch: &book.chapters[chapter as usize],
            source: book.sources.get(&chapter),
            styles,
            notes,
            mode,
            sources: Vec::new(),
            holds_blocks: book
                .sources
                .get(&chapter)
                .map(|s| s.holds_blocks.as_slice())
                .unwrap_or_default(),
            paths: PathCache::default(),
            out: Flat::default(),
            gap: 0.0,
            pending_break: false,
            marker: None,
            insets: (0.0, 0.0),
            frames: Vec::new(),
            lists: Vec::new(),
        }
    }

    fn nested(&mut self, chapter: u32, mode: Mode) -> Flattener<'_> {
        Flattener::new(self.book, chapter, self.styles, self.notes, mode)
    }

    fn style(&self, id: NodeId) -> Option<&'a ComputedStyle> {
        let ch: &'a Chapter = self.ch;
        ch.node(id).and_then(|n| ch.styles.get(n.style))
    }

    fn source_steps(&self, id: NodeId) -> Option<Vec<u32>> {
        let source = self.source?;
        if source.doc.elements.is_empty() && source.excluded.is_empty() {
            return None;
        }
        let path = self.ch.source.element(id)?;
        Some(self.ch.source.path_steps(path))
    }

    fn skipped(&self, id: NodeId) -> bool {
        if self.mode != Mode::Note && self.book.notes.removed.contains(&global(self.chapter, id)) {
            return true;
        }
        let Some(node) = self.ch.node(id) else {
            return true;
        };
        if node.role == Role::Text {
            return false;
        }
        let Some(steps) = self.source_steps(id) else {
            return false;
        };
        let source = self.source.expect("steps imply a source");
        source.excluded.iter().any(|e| steps.starts_with(e))
            || source.doc.elements.get(&steps).is_some_and(|e| e.hidden)
    }

    fn list_type(&self, id: NodeId) -> Option<ListStyleType> {
        let steps = self.source_steps(id)?;
        self.source?.doc.elements.get(&steps)?.list_type
    }

    fn svg_image(&self, id: NodeId) -> Option<(String, Option<(u32, u32)>)> {
        let steps = self.source_steps(id)?;
        let info = self.source?.doc.elements.get(&steps)?;
        let src = self
            .ch
            .semantics
            .href(id)
            .map(str::to_string)
            .or_else(|| info.href.clone())?;
        (info.tag == "image").then_some((src, info.width.zip(info.height)))
    }

    fn is_image(&self, id: NodeId) -> bool {
        self.ch.node(id).is_some_and(|n| n.role == Role::Image) || self.svg_image(id).is_some()
    }

    /// Inline content, unless it wraps blocks: an `<a id="x"/>` read as HTML
    /// never closes and swallows the rest of the document.
    fn is_inline(&self, id: NodeId) -> bool {
        let wraps_blocks = self
            .holds_blocks
            .get(id.0 as usize)
            .copied()
            .unwrap_or(false);
        (self.ch.node(id).is_some_and(|n| is_inline_role(n.role)) && !wraps_blocks)
            || self.is_image(id)
    }

    fn walk_block(&mut self, id: NodeId) {
        if self.skipped(id) {
            return;
        }
        let Some(node) = self.ch.node(id) else { return };
        match node.role {
            _ if self.is_inline(id) => self.leaf(id, &[id]),
            Role::Rule => {
                let saved = self.enter(id);
                let block = self.block(BlockKind::Rule, id);
                self.push(block, Some(id), None);
                self.exit(id, saved);
            }
            Role::Table if self.mode == Mode::Flow => self.table(id),
            _ => self.container(id),
        }
    }

    fn container(&mut self, id: NodeId) {
        let saved = self.enter(id);
        let mut run = Vec::new();
        for child in self.ch.children(id) {
            if self.skipped(child) {
                continue;
            }
            if self.is_inline(child) {
                run.push(child);
            } else {
                self.leaf(id, &run);
                run.clear();
                self.walk_block(child);
            }
        }
        self.leaf(id, &run);
        self.exit(id, saved);
    }

    fn anchor_here(&mut self, id: NodeId) {
        if let Some(anchor) = self.ch.semantics.id(id) {
            let block = self.out.blocks.len() as u32;
            self.out
                .anchors
                .push((anchor.to_string(), BlockPos { block, offset: 0 }));
        }
    }

    fn enter(&mut self, id: NodeId) -> Saved {
        self.anchor_here(id);
        let saved = Saved {
            insets: self.insets,
            frames: self.frames.len(),
            lists: self.lists.len(),
        };
        let Some(s) = self.style(id) else {
            return saved;
        };
        let size = s.font_size_abs.0;
        let role = self.ch.node(id).map(|n| n.role);
        let list = matches!(role, Some(Role::OrderedList | Role::UnorderedList));
        if !(list && !self.lists.is_empty()) {
            self.gap = self.gap.max(em_or_zero(s.margin_top, size));
        }
        self.gap += em_or_zero(s.padding_top, size);
        let start = em_or_zero(s.margin_left, size) + em_or_zero(s.padding_left, size);
        self.insets.0 += if list && start == 0.0 {
            LIST_INSET_EM
        } else {
            start
        };
        self.insets.1 += em_or_zero(s.margin_right, size) + em_or_zero(s.padding_right, size);
        if forced_break(s.break_before) {
            self.pending_break = true;
        }
        match role {
            Some(Role::BlockQuote) => self.frames.push(Frame::Quote),
            Some(Role::Sidebar) => self.frames.push(Frame::Aside),
            Some(Role::Figure) => self.frames.push(Frame::Figure),
            Some(Role::OrderedList | Role::UnorderedList) => self.lists.push(ListState {
                counter: self.ch.semantics.list_start(id).unwrap_or(1),
                kind: self.list_type(id),
            }),
            Some(Role::ListItem) => {
                let list = self.lists.last().and_then(|l| l.kind);
                let kind = self.list_type(id).or(list).unwrap_or(s.list_style_type);
                self.marker = self.next_marker(kind);
            }
            _ => {}
        }
        saved
    }

    fn exit(&mut self, id: NodeId, saved: Saved) {
        if let Some(s) = self.style(id) {
            let size = s.font_size_abs.0;
            let list = self
                .ch
                .node(id)
                .is_some_and(|n| matches!(n.role, Role::OrderedList | Role::UnorderedList));
            if !(list && saved.lists > 0) {
                self.gap = self.gap.max(em_or_zero(s.margin_bottom, size));
            }
            self.gap += em_or_zero(s.padding_bottom, size);
            if forced_break(s.break_after) {
                self.pending_break = true;
            }
        }
        if self.ch.node(id).is_some_and(|n| n.role == Role::ListItem) {
            self.marker = None;
        }
        self.insets = saved.insets;
        self.frames.truncate(saved.frames);
        self.lists.truncate(saved.lists);
    }

    fn next_marker(&mut self, kind: ListStyleType) -> Option<String> {
        let depth = self.lists.len().saturating_sub(1);
        let list = self.lists.last_mut()?;
        let n = list.counter;
        list.counter += 1;
        let bullet = |s: &str| Some(s.to_string());
        match kind {
            ListStyleType::None => None,
            ListStyleType::Disc => bullet(["•", "◦", "▪"][depth.min(2)]),
            ListStyleType::Circle => bullet("◦"),
            ListStyleType::Square => bullet("▪"),
            ListStyleType::Decimal => Some(format!("{n}.")),
            ListStyleType::LowerAlpha => Some(format!("{}.", alpha(n))),
            ListStyleType::UpperAlpha => Some(format!("{}.", alpha(n).to_uppercase())),
            ListStyleType::LowerRoman => Some(format!("{}.", roman(n))),
            ListStyleType::UpperRoman => Some(format!("{}.", roman(n).to_uppercase())),
        }
    }

    fn block(&mut self, kind: BlockKind, owner: NodeId) -> Block {
        let s = self.style(owner);
        Block {
            kind,
            space_before: std::mem::take(&mut self.gap),
            inset_start: self.insets.0,
            inset_end: self.insets.1,
            frame: self.frames.last().copied(),
            marker: self.marker.take(),
            break_before: std::mem::take(&mut self.pending_break),
            avoid_break_inside: s.is_some_and(|s| s.break_inside == BreakValue::Avoid),
            keep_with_next: s.is_some_and(|s| s.break_after == BreakValue::Avoid),
            ..Block::default()
        }
    }

    fn push(&mut self, block: Block, element: Option<NodeId>, segments: Option<Vec<Segment>>) {
        let index = self.out.blocks.len() as u32;
        if self.mode == Mode::Flow {
            if let Some(path) = element.and_then(|e| self.ch.source.element_path(e)) {
                self.out.elements.insert(index, path);
            }
            self.out.segments.push(segments.unwrap_or_default());
        }
        self.out.blocks.push(block);
    }

    fn leaf(&mut self, owner: NodeId, run: &[NodeId]) {
        if run.is_empty() {
            return;
        }
        let owner_role = self.ch.node(owner).map(|n| n.role);
        let verbatim = owner_role == Some(Role::CodeBlock);
        let mut text = LeafText::default();
        let mut images = Vec::new();
        let owner_style = self.ch.node(owner).map(|n| n.style).unwrap_or_default();
        let mode = LeafMode {
            dropcap: self.style(owner).is_some_and(|s| s.dropcap_lines > 0),
            verbatim,
        };
        let attrs = SpanAttrs::default();
        for &id in run {
            self.inline(id, owner_style, &attrs, mode, &mut text, &mut images);
        }
        if text.is_blank() {
            if text.spans.iter().any(|s| s.text.contains('\n')) {
                self.gap = self.gap.max(BLOCK_BREAK_EM);
            }
            let block = self.out.blocks.len() as u32;
            for (anchor, _) in text.anchors {
                self.out
                    .anchors
                    .push((anchor, BlockPos { block, offset: 0 }));
            }
            for &(image, _) in &images {
                self.image_block(owner, image);
            }
            return;
        }
        text.trim(verbatim);
        let (kind, level) = match owner_role {
            Some(Role::Heading(l)) => (BlockKind::Heading, Some(l.clamp(1, 6))),
            Some(Role::CodeBlock) => (BlockKind::Code, None),
            _ => (BlockKind::Text, None),
        };
        let mut block = self.block(kind, owner);
        block.level = level;
        block.keep_with_next |= kind == BlockKind::Heading;
        block.style = self
            .styles
            .of(self.chapter, self.ch, owner_style, StyleRole::Block);
        let segments = (self.mode == Mode::Flow).then(|| {
            let source = &self.ch.source;
            let paths = &mut self.paths;
            text.segments(self.ch.text_buffer(), &self.sources, &mut |p| {
                paths.get(source, p)
            })
        });
        let index = self.out.blocks.len() as u32;
        for (anchor, offset) in std::mem::take(&mut text.anchors) {
            self.out.anchors.push((
                anchor,
                BlockPos {
                    block: index,
                    offset,
                },
            ));
        }
        block.spans = text.spans;
        self.push(block, None, segments);
        for &(image, tiny) in &images {
            if !tiny {
                self.image_block(owner, image);
            }
        }
    }

    fn inline(
        &mut self,
        id: NodeId,
        style: boko::StyleId,
        attrs: &SpanAttrs,
        leaf: LeafMode,
        text: &mut LeafText,
        images: &mut Vec<(NodeId, bool)>,
    ) {
        if self.skipped(id) {
            return;
        }
        let Some(node) = self.ch.node(id) else { return };
        if let Some(anchor) = self.ch.semantics.id(id) {
            text.anchor(anchor);
        }
        if self.is_image(id) {
            images.push((id, self.is_tiny(id)));
            return;
        }
        match node.role {
            Role::Text => {
                let role = if leaf.dropcap
                    && text.len() == 0
                    && self
                        .ch
                        .styles
                        .get(style)
                        .is_some_and(|s| s.float != boko::style::Float::None)
                {
                    StyleRole::DropcapSpan
                } else {
                    StyleRole::Span
                };
                let attrs = SpanAttrs {
                    style: self.styles.of(self.chapter, self.ch, style, role),
                    ..attrs.clone()
                };
                text.push(
                    self.ch.text(node.text),
                    &attrs,
                    Some(node.text.start),
                    leaf.verbatim,
                );
            }
            Role::Break
                if self
                    .style(id)
                    .is_some_and(|s| s.display == boko::style::Display::None) => {}
            Role::Break => {
                let attrs = SpanAttrs {
                    style: self
                        .styles
                        .of(self.chapter, self.ch, style, StyleRole::Span),
                    ..attrs.clone()
                };
                text.push("\n", &attrs, None, leaf.verbatim);
            }
            Role::Math => {}
            _ => {
                let attrs = if node.role == Role::Link {
                    self.link_attrs(id, attrs)
                } else {
                    attrs.clone()
                };
                for child in self.ch.children(id) {
                    self.inline(child, node.style, &attrs, leaf, text, images);
                }
            }
        }
    }

    fn link_attrs(&mut self, id: NodeId, parent: &SpanAttrs) -> SpanAttrs {
        let g = global(self.chapter, id);
        let mut attrs = SpanAttrs {
            style: parent.style,
            ..SpanAttrs::default()
        };
        if let Some(note) = self.book.notes.links.get(&g) {
            attrs.note = Some(self.note(note.block, &note.key));
            return attrs;
        }
        match self.book.resolved.get(g) {
            Some(AnchorTarget::External(url)) => {
                attrs.link = Some(Link {
                    position: None,
                    href: Some(url.clone()),
                })
            }
            Some(AnchorTarget::Internal(t)) => {
                attrs.target = Some(LinkTarget {
                    chapter: t.chapter.0,
                    node: Some(t.node.0),
                })
            }
            Some(AnchorTarget::Chapter(c)) => {
                attrs.target = Some(LinkTarget {
                    chapter: c.0,
                    node: None,
                })
            }
            None => {}
        }
        attrs
    }

    /// The `notes` key for a note, flattening its content the first time.
    fn note(&mut self, block: boko::model::GlobalNodeId, key: &str) -> String {
        let owner = (block.chapter.0, block.node.0);
        let mut key = key.to_string();
        let mut n = 1;
        while let Some(&existing) = self.notes.owners.get(&key) {
            if existing == owner {
                return key;
            }
            n += 1;
            key = format!("{}~{n}", key.split('~').next().unwrap_or(&key));
        }
        self.notes.owners.insert(key.clone(), owner);
        let mut f = self.nested(block.chapter.0, Mode::Note);
        if f.is_inline(block.node) {
            f.leaf(block.node, &[block.node]);
        } else {
            f.container(block.node);
        }
        let blocks = f.out.blocks;
        self.notes.blocks.insert(key.clone(), blocks);
        key
    }

    fn is_tiny(&self, id: NodeId) -> bool {
        let css = self.style(id).map(|s| s.height);
        match css {
            Some(Length::Em(v)) | Some(Length::Rem(v)) => return v <= IMAGE_TINY_EM,
            Some(Length::Px(v)) if v > 0.0 => return v <= IMAGE_TINY_EM * super::styles::BASE_PX,
            _ => {}
        }
        self.image(id)
            .and_then(|i| i.height)
            .is_some_and(|h| h as f32 <= IMAGE_TINY_EM * super::styles::BASE_PX)
    }

    fn image(&self, id: NodeId) -> Option<Image> {
        let (src, attr_size) = match self.svg_image(id) {
            Some(svg) => svg,
            None => {
                let src = self.ch.semantics.src(id)?.to_string();
                let attrs = self
                    .source_steps(id)
                    .and_then(|s| self.source?.doc.elements.get(&s))
                    .and_then(|e| e.width.zip(e.height));
                (src, attrs)
            }
        };
        let size = self.book.image_dims.get(&src).copied().or(attr_size);
        Some(Image {
            alt: self
                .ch
                .semantics
                .alt(id)
                .filter(|a| !a.trim().is_empty())
                .map(str::to_string),
            width: size.map(|s| s.0),
            height: size.map(|s| s.1),
            src,
        })
    }

    fn image_block(&mut self, owner: NodeId, id: NodeId) {
        let Some(image) = self.image(id) else { return };
        let mut block = self.block(BlockKind::Image, owner);
        block.image = Some(image);
        self.push(block, Some(id), None);
    }

    fn table(&mut self, id: NodeId) {
        let saved = self.enter(id);
        let mut rows = Vec::new();
        let mut anchors = Vec::new();
        self.rows(id, false, &mut rows, &mut anchors);
        if !rows.is_empty() {
            let index = self.out.blocks.len() as u32;
            for anchor in anchors {
                self.out.anchors.push((
                    anchor,
                    BlockPos {
                        block: index,
                        offset: 0,
                    },
                ));
            }
            let mut block = self.block(BlockKind::Table, id);
            block.table = Some(Table { rows });
            self.push(block, Some(id), None);
        }
        self.exit(id, saved);
    }

    fn rows(&mut self, id: NodeId, header: bool, rows: &mut Vec<Row>, anchors: &mut Vec<String>) {
        for child in self.ch.children(id) {
            if self.skipped(child) {
                continue;
            }
            match self.ch.node(child).map(|n| n.role) {
                Some(Role::Caption) => self.walk_block(child),
                Some(Role::TableRow) => {
                    if let Some(id) = self.ch.semantics.id(child) {
                        anchors.push(id.to_string());
                    }
                    let row = self.row(child, header, anchors);
                    if !row.cells.is_empty() {
                        rows.push(row);
                    }
                }
                Some(Role::TableHead) => self.rows(child, true, rows, anchors),
                _ => self.rows(child, header, rows, anchors),
            }
        }
    }

    fn row(&mut self, id: NodeId, header: bool, anchors: &mut Vec<String>) -> Row {
        let mut cells = Vec::new();
        let mut all_header = true;
        let ch = self.ch;
        for cell in ch.children(id) {
            if self.skipped(cell) {
                continue;
            }
            all_header &= ch.semantics.is_header_cell(cell);
            let mut f = self.nested(self.chapter, Mode::Cell);
            f.container(cell);
            anchors.extend(f.out.anchors.into_iter().map(|(a, _)| a));
            cells.push(Cell {
                blocks: f.out.blocks,
                colspan: ch.semantics.col_span(cell).unwrap_or(1).max(1),
                rowspan: ch.semantics.row_span(cell).unwrap_or(1).max(1),
            });
        }
        Row {
            header: header || (all_header && !cells.is_empty()),
            cells,
        }
    }
}

pub(crate) fn holds_blocks(ch: &Chapter) -> Vec<bool> {
    let order: Vec<NodeId> = ch.iter_dfs().collect();
    let mut out = vec![false; ch.node_count()];
    for &id in order.iter().rev() {
        let Some(node) = ch.node(id) else { continue };
        if let Some(parent) = node.parent
            && (out[id.0 as usize] || !is_inline_role(node.role))
        {
            out[parent.0 as usize] = true;
        }
    }
    out
}

fn alpha(mut n: u32) -> String {
    let mut out = Vec::new();
    while n > 0 {
        n -= 1;
        out.push(b'a' + (n % 26) as u8);
        n /= 26;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

fn roman(mut n: u32) -> String {
    const TABLE: [(u32, &str); 13] = [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ];
    let mut out = String::new();
    for (value, numeral) in TABLE {
        while n >= value {
            out.push_str(numeral);
            n -= value;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn list_numbering() {
        let cases = [
            (1, "a", "i"),
            (4, "d", "iv"),
            (27, "aa", "xxvii"),
            (1994, "bxr", "mcmxciv"),
        ];
        for (n, a, r) in cases {
            assert_eq!(super::alpha(n), a);
            assert_eq!(super::roman(n), r);
        }
    }
}
