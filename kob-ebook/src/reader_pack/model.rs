use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

fn is_false(b: &bool) -> bool {
    !*b
}

fn is_zero_u32(n: &u32) -> bool {
    *n == 0
}

fn is_zero_f32(n: &f32) -> bool {
    *n == 0.0
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
pub struct Position {
    pub chapter: u32,
    pub block: u32,
    pub offset: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockPos {
    pub block: u32,
    pub offset: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub generation: u32,
    pub title: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authors: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub rtl: bool,
    pub chapters: Vec<ChapterEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub toc: Vec<TocEntry>,
    pub styles: Vec<TextStyle>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fonts: Vec<FontFace>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChapterEntry {
    pub href: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub weight: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixed_layout: Option<FixedLayout>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixedLayout {
    pub side: SpreadSide,
    pub spreads: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpreadSide {
    Auto,
    Left,
    Right,
    Center,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TocEntry {
    pub title: String,
    pub target: Position,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<TocEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FontFace {
    pub family: String,
    pub weight: u16,
    pub italic: bool,
    pub src: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TextStyle {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<u16>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub italic: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub small_caps: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub align: Option<Align>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indent: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_height: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub letter_spacing: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub word_spacing: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform: Option<Transform>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub underline: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub strike: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<Baseline>,
    /// A baseline raised (positive) or lowered by this many of the span's own em.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shift: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub borders: Option<Borders>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hyphens: Option<Hyphens>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub widows: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orphans: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dropcap: Option<Dropcap>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Align {
    Start,
    End,
    Left,
    Right,
    Center,
    Justify,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transform {
    Uppercase,
    Lowercase,
    Capitalize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Baseline {
    Super,
    Sub,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Hyphens {
    Auto,
    Manual,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dropcap {
    pub lines: u8,
    pub chars: u8,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Chapter {
    pub blocks: Vec<Block>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub anchors: BTreeMap<String, BlockPos>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub notes: BTreeMap<String, Vec<Block>>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    #[default]
    Text,
    Heading,
    Code,
    Image,
    Rule,
    Table,
    FixedPage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Frame {
    Quote,
    Aside,
    Figure,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Block {
    pub kind: BlockKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<u8>,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub style: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spans: Vec<Span>,
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub space_before: f32,
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub inset_start: f32,
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub inset_end: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<Frame>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub frame_plain: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub containers: Vec<Container>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub centred: Option<Measure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marker: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub break_before: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub keep_with_next: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub avoid_break_inside: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<Image>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<RuleStyle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table: Option<Table>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixed: Option<FixedPage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub float: Option<Float>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clear: Option<Clear>,
}

/// An image set against one edge of the column, the blocks after it flowing
/// beside it; `gap` is the room in em its margin and padding keep on the
/// side the text runs along.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Float {
    pub side: Side,
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub gap: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Clear {
    Left,
    Right,
    Both,
}

/// A decorated element around one or more blocks, repeated on each of them.
/// `id` is unique within the block list, so two adjacent boxes alike in every
/// other field stay two. Lengths are in em; the insets place its border edge,
/// the spaces sit between that edge and its first and last blocks.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Container {
    pub id: u32,
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub inset_start: f32,
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub inset_end: f32,
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub space_top: f32,
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub space_bottom: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top: Option<Line>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right: Option<Line>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bottom: Option<Line>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left: Option<Line>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    /// Corner radii in em: top-left, top-right, bottom-right, bottom-left.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radius: Option<[f32; 4]>,
    /// The content width of a box centred by `margin: auto`; `inner` is the
    /// border and padding it adds across both sides, in em.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub centred: Option<Measure>,
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub inner: f32,
}

/// A CSS width: in em, or as a fraction of the column it sits in.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Measure {
    Em(f32),
    Fraction(f32),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Borders {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top: Option<Line>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right: Option<Line>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bottom: Option<Line>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left: Option<Line>,
}

/// `color` is unset when the line takes the text colour.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Line {
    pub width: f32,
    pub style: LineStyle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LineStyle {
    Solid,
    Dashed,
    Dotted,
    Double,
}

/// A rule the book draws itself, as a line or as a picture; `length` is a
/// fraction of the column, `height` the rule's own in em.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuleStyle {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<Line>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<RuleImage>,
}

/// An ornament drawn as the rule, once and centred or tiled across it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuleImage {
    pub src: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub repeat: bool,
}

impl Block {
    pub fn text(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Span {
    pub text: String,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub style: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<Link>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip)]
    pub(crate) target: Option<LinkTarget>,
}

/// An internal link before the whole book is flattened: boko's chapter and
/// node, resolved into a [`Position`] once every chapter has its anchors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LinkTarget {
    pub chapter: u32,
    pub node: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Link {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Image {
    pub src: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column_width: Option<f32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Table {
    pub rows: Vec<Row>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub collapse: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Row {
    #[serde(default, skip_serializing_if = "is_false")]
    pub header: bool,
    pub cells: Vec<Cell>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    pub blocks: Vec<Block>,
    pub colspan: u32,
    pub rowspan: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub borders: Option<Borders>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FixedPage {
    pub image: Image,
    pub page_width: u32,
    pub page_height: u32,
    pub fragments: Vec<Fragment>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fragment {
    pub top: f32,
    pub left: f32,
    pub width: f32,
    pub font_size: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    pub align: Align,
    pub spans: Vec<Span>,
}
