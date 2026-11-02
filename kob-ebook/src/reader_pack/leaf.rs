//! One leaf's text as it is gathered: spans, anchors, and which stretches of
//! boko's text buffer it was copied from, so the CFI sidecar can map block
//! offsets back to the source after trimming.

use std::collections::HashMap;

use boko::{PathId, SourceSegment};

use super::model::{LinkTarget, Span};
use super::sidecar::Segment;

pub(crate) fn utf16_len(s: &str) -> u32 {
    if s.is_ascii() {
        s.len() as u32
    } else {
        s.encode_utf16().count() as u32
    }
}

fn byte_at_utf16(s: &str, units: u32) -> usize {
    let mut seen = 0u32;
    for (i, c) in s.char_indices() {
        if seen >= units {
            return i;
        }
        seen += c.len_utf16() as u32;
    }
    s.len()
}

#[derive(Debug, Clone, Copy)]
struct Piece {
    block_offset: u32,
    buf_start: u32,
    buf_len: u32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct SpanAttrs {
    pub style: u32,
    pub link: Option<super::model::Link>,
    pub note: Option<String>,
    pub target: Option<LinkTarget>,
}

#[derive(Debug, Default)]
pub(crate) struct LeafText {
    pub spans: Vec<Span>,
    pub anchors: Vec<(String, u32)>,
    len: u32,
    pieces: Vec<Piece>,
    window: Option<(u32, u32)>,
}

impl LeafText {
    pub fn len(&self) -> u32 {
        self.len
    }

    pub fn anchor(&mut self, id: &str) {
        self.anchors.push((id.to_string(), self.len));
    }

    /// Append text. Outside verbatim content a space that would follow
    /// another space is collapsed away, as the renderer would.
    pub fn push(&mut self, text: &str, attrs: &SpanAttrs, buf_start: Option<u32>, verbatim: bool) {
        let mut text = text;
        let mut buf_start = buf_start;
        if !verbatim && self.spans.last().is_some_and(|s| s.text.ends_with(' ')) {
            let kept = text.trim_start_matches(' ');
            buf_start = buf_start.map(|b| b + (text.len() - kept.len()) as u32);
            text = kept;
        }
        if text.is_empty() {
            return;
        }
        if let Some(buf_start) = buf_start {
            self.pieces.push(Piece {
                block_offset: self.len,
                buf_start,
                buf_len: text.len() as u32,
            });
        }
        self.len += utf16_len(text);
        match self.spans.last_mut() {
            Some(last)
                if last.style == attrs.style
                    && last.link == attrs.link
                    && last.note == attrs.note
                    && last.target == attrs.target =>
            {
                last.text.push_str(text)
            }
            _ => self.spans.push(Span {
                text: text.to_string(),
                style: attrs.style,
                link: attrs.link.clone(),
                note: attrs.note.clone(),
                target: attrs.target,
            }),
        }
    }

    pub fn is_blank(&self) -> bool {
        self.spans.iter().all(|s| s.text.trim().is_empty())
    }

    /// Trim the leaf's edges — every whitespace character, or for code only
    /// the line breaks before its first line and whitespace after its last —
    /// keeping anchors and buffer pieces in step.
    pub fn trim(&mut self, verbatim: bool) {
        let lead_ws = |c: char| {
            if verbatim {
                c == '\n' || c == '\r'
            } else {
                c.is_whitespace()
            }
        };
        let mut lead = 0u32;
        while let Some(first) = self.spans.first_mut() {
            let kept = first.text.trim_start_matches(lead_ws);
            lead += utf16_len(&first.text[..first.text.len() - kept.len()]);
            if kept.is_empty() {
                self.spans.remove(0);
            } else {
                first.text = kept.to_string();
                break;
            }
        }
        let mut trail = 0u32;
        while let Some(last) = self.spans.last_mut() {
            let kept = last.text.trim_end().len();
            trail += utf16_len(&last.text[kept..]);
            if kept == 0 {
                self.spans.pop();
            } else {
                last.text.truncate(kept);
                break;
            }
        }
        if lead == 0 && trail == 0 {
            return;
        }
        let end = self.len.saturating_sub(trail).max(lead);
        self.len = end - lead;
        for (_, off) in &mut self.anchors {
            *off = (*off).clamp(lead, end) - lead;
        }
        self.window = Some((lead, end));
    }

    pub fn segments(
        &self,
        buffer: &str,
        sources: &[SourceSegment],
        path: &mut dyn FnMut(PathId) -> String,
    ) -> Vec<Segment> {
        let (lead, end) = self.window.unwrap_or((0, u32::MAX));
        let mut out: Vec<Segment> = Vec::new();
        for p in &self.pieces {
            let Some(text) = buffer.get(p.buf_start as usize..(p.buf_start + p.buf_len) as usize)
            else {
                continue;
            };
            let start16 = p.block_offset;
            let end16 = start16 + utf16_len(text);
            let (lo, hi) = (start16.max(lead), end16.min(end));
            if lo >= hi {
                continue;
            }
            let bs = p.buf_start as usize + byte_at_utf16(text, lo - start16);
            let be = p.buf_start as usize + byte_at_utf16(text, hi - start16);
            let block_offset = lo - lead;
            let mut i = sources.partition_point(|s| (s.buf_start + s.buf_len) as usize <= bs);
            while let Some(s) = sources.get(i)
                && (s.buf_start as usize) < be
            {
                i += 1;
                let (ss, se) = (s.buf_start as usize, (s.buf_start + s.buf_len) as usize);
                let (os, oe) = (bs.max(ss), be.min(se));
                if os >= oe {
                    continue;
                }
                let at = block_offset + utf16_len(&buffer[bs..os]);
                let len = utf16_len(&buffer[os..oe]);
                let src = s.src_offset + utf16_len(&buffer[ss..os]);
                let chunk = path(s.chunk);
                match out.last_mut() {
                    Some(last)
                        if last.2 == chunk && last.0 + last.1 == at && last.3 + last.1 == src =>
                    {
                        last.1 += len;
                    }
                    _ => out.push(Segment(at, len, chunk, src)),
                }
            }
        }
        out
    }
}

#[derive(Default)]
pub(crate) struct PathCache(HashMap<PathId, String>);

impl PathCache {
    pub fn get(&mut self, map: &boko::SourceMap, id: PathId) -> String {
        self.0
            .entry(id)
            .or_insert_with(|| map.path_string(id))
            .clone()
    }
}
