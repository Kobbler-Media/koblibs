use std::collections::HashMap;

use boko::ComputedStyle;
use boko::style::{
    BreakValue, Color, Display, FontStyle, FontVariant, Hyphens as BokoHyphens, Length, TextAlign,
    TextTransform, VerticalAlign,
};

use super::decor;
use super::model::{Align, Baseline, Dropcap, Hyphens, TextStyle, Transform};

/// The CSS default font size, which `px` lengths are measured against.
pub(crate) const BASE_PX: f32 = 16.0;

const NORMAL_LINE_HEIGHT: f32 = 1.2;

/// What a percentage of the containing block's width comes to, in em: a
/// reader column is around thirty em wide, and the client has no box to
/// resolve one against.
const PERCENT_BASE_EM: f32 = 30.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum StyleRole {
    Block,
    Span,
    /// A dropcap's own span: its letters render large from the block's
    /// dropcap, so its authored size would be applied twice.
    DropcapSpan,
}

/// The book-wide interned [`TextStyle`] table. Index 0 is the default.
pub(crate) struct StyleTable {
    styles: Vec<TextStyle>,
    interned: HashMap<String, u32>,
    cache: HashMap<(u32, u32, StyleRole), u32>,
    font_families: Vec<String>,
}

impl StyleTable {
    pub fn new(font_families: Vec<String>) -> Self {
        let mut table = Self {
            styles: Vec::new(),
            interned: HashMap::new(),
            cache: HashMap::new(),
            font_families,
        };
        table.intern(TextStyle::default());
        table
    }

    pub fn intern(&mut self, style: TextStyle) -> u32 {
        let key = serde_json::to_string(&style).unwrap_or_default();
        if let Some(&i) = self.interned.get(&key) {
            return i;
        }
        let i = self.styles.len() as u32;
        self.styles.push(style);
        self.interned.insert(key, i);
        i
    }

    pub fn of(
        &mut self,
        chapter: u32,
        ch: &boko::Chapter,
        id: boko::StyleId,
        role: StyleRole,
    ) -> u32 {
        if let Some(&i) = self.cache.get(&(chapter, id.0, role)) {
            return i;
        }
        let style = ch
            .styles
            .get(id)
            .map(|s| self.convert(s, role))
            .unwrap_or_default();
        let i = self.intern(style);
        self.cache.insert((chapter, id.0, role), i);
        i
    }

    /// A one-letter dropcap widened to the whole CSS first letter: the
    /// punctuation before and after it goes large with it.
    pub fn with_first_letter(&mut self, id: u32, text: &str) -> u32 {
        let Some(dropcap) = self.styles[id as usize].dropcap.filter(|d| d.chars == 1) else {
            return id;
        };
        let chars = first_letter_len(text);
        if chars <= 1 {
            return id;
        }
        let mut style = self.styles[id as usize].clone();
        style.dropcap = Some(Dropcap {
            chars: chars.min(u8::MAX as usize) as u8,
            ..dropcap
        });
        self.intern(style)
    }

    pub fn into_styles(self) -> Vec<TextStyle> {
        self.styles
    }

    fn convert(&self, s: &ComputedStyle, role: StyleRole) -> TextStyle {
        let size = s.font_size_abs.0;
        let mut out = TextStyle {
            size: (role != StyleRole::DropcapSpan)
                .then(|| round(size))
                .filter(|v| *v != 1.0 && *v > 0.0),
            weight: Some(s.font_weight.0).filter(|w| *w != 0 && *w != 400),
            italic: matches!(s.font_style, FontStyle::Italic | FontStyle::Oblique),
            small_caps: s.font_variant == FontVariant::SmallCaps,
            family: s.font_family.as_deref().and_then(|f| self.family(f)),
            letter_spacing: em(s.letter_spacing, size).filter(|v| *v != 0.0),
            word_spacing: em(s.word_spacing, size).filter(|v| *v != 0.0),
            color: s.color.filter(|c| c.a != 0).map(hex),
            transform: match s.text_transform {
                TextTransform::Uppercase => Some(Transform::Uppercase),
                TextTransform::Lowercase => Some(Transform::Lowercase),
                TextTransform::Capitalize => Some(Transform::Capitalize),
                TextTransform::None => None,
            },
            underline: s.text_decoration_underline,
            strike: s.text_decoration_line_through,
            baseline: baseline(s),
            ..TextStyle::default()
        };
        if s.display == Display::Inline {
            out.shift = shift(s);
            out.background = decor::background(s);
            out.borders = decor::borders(s);
        }
        if role == StyleRole::Block {
            out.align = match s.text_align {
                TextAlign::Start => None,
                TextAlign::End => Some(Align::End),
                TextAlign::Left => Some(Align::Left),
                TextAlign::Right => Some(Align::Right),
                TextAlign::Center => Some(Align::Center),
                TextAlign::Justify => Some(Align::Justify),
            };
            out.indent = em(s.text_indent, size)
                .map(|v| round(v / size))
                .filter(|v| *v != 0.0);
            out.line_height = match s.line_height {
                Length::Em(v) => Some(v),
                Length::Px(v) => Some(v / (BASE_PX * size)),
                Length::Rem(v) => Some(v / size),
                Length::Percent(v) => Some(v / 100.0),
                Length::Auto => None,
            }
            .map(round)
            .filter(|v| *v > 0.0);
            out.hyphens = match s.hyphens {
                BokoHyphens::Auto => Some(Hyphens::Auto),
                BokoHyphens::None => Some(Hyphens::None),
                BokoHyphens::Manual => None,
            };
            out.widows = Some(s.widows).filter(|v| *v > 0);
            out.orphans = Some(s.orphans).filter(|v| *v > 0);
            out.dropcap = (s.dropcap_lines > 0).then_some(Dropcap {
                lines: s.dropcap_lines,
                chars: s.dropcap_chars.max(1),
            });
        }
        out
    }

    /// The first family in the list that names an embedded font, or a
    /// generic family the client maps to one of its own.
    fn family(&self, list: &str) -> Option<String> {
        list.split(',').find_map(|f| {
            let f = f.trim().trim_matches(['"', '\'']).trim();
            if let Some(known) = self
                .font_families
                .iter()
                .find(|k| k.eq_ignore_ascii_case(f))
            {
                return Some(known.clone());
            }
            let lower = f.to_ascii_lowercase();
            matches!(lower.as_str(), "serif" | "sans-serif" | "monospace").then_some(lower)
        })
    }
}

/// The length in UTF-16 units, the client's string offsets, of the CSS first
/// letter: leading punctuation, one letter or digit, and the punctuation after it.
fn first_letter_len(text: &str) -> usize {
    let mut chars = text.chars().peekable();
    let mut len = 0;
    while let Some(&c) = chars.peek()
        && is_first_letter_punctuation(c)
    {
        len += c.len_utf16();
        chars.next();
    }
    match chars.next() {
        Some(c) if c.is_alphanumeric() => len += c.len_utf16(),
        _ => return len.max(1),
    }
    while let Some(&c) = chars.peek()
        && is_first_letter_punctuation(c)
    {
        len += c.len_utf16();
        chars.next();
    }
    len
}

/// Open, close, initial, final and other punctuation, which CSS counts into a
/// first letter; dashes and connectors it leaves out.
fn is_first_letter_punctuation(c: char) -> bool {
    matches!(
        c,
        '!' | '"'
            | '#'
            | '%'
            | '&'
            | '\''
            | '('
            | ')'
            | '*'
            | ','
            | '.'
            | '/'
            | ':'
            | ';'
            | '?'
            | '@'
            | '['
            | '\\'
            | ']'
            | '{'
            | '}'
            | '¡'
            | '§'
            | '«'
            | '¶'
            | '·'
            | '»'
            | '¿'
            | '‘'
            | '’'
            | '‚'
            | '‛'
            | '“'
            | '”'
            | '„'
            | '‟'
            | '‹'
            | '›'
            | '…'
            | '‰'
            | '′'
            | '″'
            | '〈'
            | '〉'
            | '「'
            | '」'
            | '『'
            | '』'
    )
}

/// `top` and `bottom` on an inline lift or drop it the way `super` and
/// `sub` do; on a table cell they place its content instead.
fn baseline(s: &ComputedStyle) -> Option<Baseline> {
    let inline = s.display == Display::Inline;
    match s.vertical_align {
        VerticalAlign::Super => Some(Baseline::Super),
        VerticalAlign::Sub => Some(Baseline::Sub),
        VerticalAlign::Top | VerticalAlign::TextTop if inline => Some(Baseline::Super),
        VerticalAlign::Bottom | VerticalAlign::TextBottom if inline => Some(Baseline::Sub),
        _ => None,
    }
}

/// A `vertical-align` length, in the span's own em; a percentage is of its line height.
fn shift(s: &ComputedStyle) -> Option<f32> {
    let size = s.font_size_abs.0;
    let line_height = match s.line_height {
        Length::Em(v) => v,
        Length::Px(v) => v / (BASE_PX * size),
        Length::Rem(v) => v / size,
        Length::Percent(v) => v / 100.0,
        Length::Auto => NORMAL_LINE_HEIGHT,
    };
    let em = match s.vertical_align_length {
        Length::Auto => return None,
        Length::Em(v) => v,
        Length::Px(v) => v / (BASE_PX * size),
        Length::Rem(v) => v / size,
        Length::Percent(v) => v / 100.0 * line_height,
    };
    Some(round(em)).filter(|v| *v != 0.0)
}

pub(crate) fn hex(c: Color) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
}

pub(crate) fn round(v: f32) -> f32 {
    (v * 1000.0).round() / 1000.0
}

/// A length in em of the base font size; `font_size` is the element's own
/// size as a multiple of the base, for `em` lengths.
pub(crate) fn em(len: Length, font_size: f32) -> Option<f32> {
    match len {
        Length::Auto => None,
        Length::Px(v) => Some(round(v / BASE_PX)),
        Length::Em(v) => Some(round(v * font_size)),
        Length::Rem(v) => Some(round(v)),
        Length::Percent(v) => Some(round(v / 100.0 * PERCENT_BASE_EM)),
    }
}

pub(crate) fn em_or_zero(len: Length, font_size: f32) -> f32 {
    em(len, font_size).unwrap_or(0.0)
}

pub(crate) fn forced_break(v: BreakValue) -> bool {
    matches!(v, BreakValue::Always)
}
