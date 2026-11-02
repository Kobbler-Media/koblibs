//! Where each `kobo.P.S` span a Kobo reports starts, as a block position:
//! the document's own spans when it is already a KEPUB, otherwise the spans
//! [`crate::kepub`] would number into the KEPUB made from it.

use quick_xml::events::{BytesStart, Event};

use super::leaf::utf16_len;
use super::model::{Block, BlockPos};
use super::sidecar::{ChapterSidecar, KoboEntry};
use super::source::{PathTracker, SPAN_TEXT_PREFIX, SourceDoc, dom_len};
use crate::Result;
use crate::kepub::{KepubContext, KepubSink, numbers_spans, walk_kepubify};

struct SpanStart {
    id: String,
    steps: Vec<u32>,
    offset: Option<u32>,
    text: String,
}

#[derive(Default)]
struct Recorder {
    tracker: PathTracker,
    spans: Vec<SpanStart>,
}

impl KepubSink for Recorder {
    fn source(&mut self, event: Event<'_>) -> Result<()> {
        match &event {
            Event::Start(_) => {
                self.tracker.start();
            }
            Event::End(_) => self.tracker.end(),
            Event::Empty(_) => {
                self.tracker.empty();
            }
            other => self.tracker.character_data(other),
        }
        Ok(())
    }

    fn injected(&mut self, _: Event<'_>) -> Result<()> {
        Ok(())
    }

    fn run(&mut self, span: Option<(u32, u32)>, text: &str) -> Result<()> {
        if let Some((para, seg)) = span
            && let Some((steps, offset)) = self.tracker.text_position()
        {
            self.spans.push(SpanStart {
                id: format!("kobo.{para}.{seg}"),
                steps,
                offset: Some(offset),
                text: text.chars().take(SPAN_TEXT_PREFIX).collect(),
            });
        }
        self.tracker.advance(dom_len(text));
        Ok(())
    }

    fn image(&mut self, (para, seg): (u32, u32), _: BytesStart<'_>) -> Result<()> {
        let steps = self.tracker.empty();
        self.spans.push(SpanStart {
            id: format!("kobo.{para}.{seg}"),
            steps,
            offset: None,
            text: String::new(),
        });
        Ok(())
    }
}

pub(crate) struct FixedPageSpans<'a> {
    pub block: u32,
    pub excluded: &'a [Vec<u32>],
}

/// boko parses a document as XML only when it looks like XHTML, and as HTML
/// otherwise; HTML parsing restructures the tree (an unclosed `<a/>` swallows
/// what follows), so source steps only line up with boko's in the XML case.
fn parsed_as_xml(input: &[u8]) -> bool {
    let head = &input[..input.len().min(500)];
    let has = |needle: &[u8]| head.windows(needle.len()).any(|w| w == needle);
    has(b"<?xml") || has(b"xmlns=")
}

/// Finds span text in the chapter's blocks, moving only forward, for
/// documents whose source steps cannot be trusted.
struct TextCursor<'a> {
    blocks: &'a [Block],
    texts: Vec<Option<String>>,
    block: usize,
    byte: usize,
}

const SEARCH_BLOCKS: usize = 64;

impl<'a> TextCursor<'a> {
    fn new(blocks: &'a [Block]) -> Self {
        Self {
            blocks,
            texts: vec![None; blocks.len()],
            block: 0,
            byte: 0,
        }
    }

    fn find(&mut self, text: &str) -> BlockPos {
        let needle: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let needle: String = needle.chars().take(SPAN_TEXT_PREFIX).collect();
        if !needle.is_empty() {
            let end = (self.block + SEARCH_BLOCKS).min(self.blocks.len());
            for b in self.block..end {
                let start = if b == self.block { self.byte } else { 0 };
                let hay = self.texts[b].get_or_insert_with(|| self.blocks[b].text());
                if let Some(i) = hay.get(start..).and_then(|h| h.find(&needle)) {
                    let at = start + i;
                    let offset = utf16_len(&hay[..at]);
                    (self.block, self.byte) = (b, at);
                    return BlockPos {
                        block: b as u32,
                        offset,
                    };
                }
            }
        }
        let (blocks, byte) = (self.blocks, self.byte);
        let offset = self
            .texts
            .get_mut(self.block)
            .map(|t| {
                let hay = t.get_or_insert_with(|| blocks[self.block].text());
                utf16_len(&hay[..byte.min(hay.len())])
            })
            .unwrap_or(0);
        BlockPos {
            block: self.block as u32,
            offset,
        }
    }
}

pub(crate) fn map(
    input: &[u8],
    doc_path: &str,
    doc: &SourceDoc,
    ctx: &KepubContext,
    sidecar: &ChapterSidecar,
    blocks: &[Block],
    fixed: Option<FixedPageSpans<'_>>,
) -> Vec<KoboEntry> {
    let starts: Vec<SpanStart> = if !doc.kobo_spans.is_empty() {
        doc.kobo_spans
            .iter()
            .map(|s| SpanStart {
                id: s.id.clone(),
                steps: s.steps.clone(),
                offset: None,
                text: s.text.clone(),
            })
            .collect()
    } else if numbers_spans(input, doc_path, ctx).unwrap_or(false) {
        let mut recorder = Recorder::default();
        if walk_kepubify(input, &mut recorder).is_err() {
            return Vec::new();
        }
        recorder.spans
    } else {
        Vec::new()
    };
    let by_steps = parsed_as_xml(input);
    let mut cursor = TextCursor::new(blocks);
    let mut out: Vec<KoboEntry> = starts
        .into_iter()
        .map(|s| {
            let on_page = fixed
                .as_ref()
                .filter(|f| f.excluded.iter().any(|e| s.steps.starts_with(e)));
            let pos = match on_page {
                Some(f) => BlockPos {
                    block: f.block,
                    offset: 0,
                },
                None if by_steps => sidecar.resolve_local(&s.steps, s.offset),
                None => cursor.find(&s.text),
            };
            KoboEntry(s.id, pos.block, pos.offset)
        })
        .collect();
    out.sort_by_key(|k| (k.1, k.2));
    out
}
