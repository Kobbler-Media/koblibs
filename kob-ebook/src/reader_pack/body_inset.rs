//! Books converted from Mobipocket often set every paragraph in a blockquote
//! with a left margin, so the whole text sits off-centre and every paragraph
//! reads as a quote. When more than half of a book's running text shares one
//! such one-sided inset and next to nothing is set further left, that inset
//! is the book's body, not a quotation: it is taken off every block, so real
//! quotes keep only what sets them apart. Poems whose titles hang left of
//! their lines, or prose set left of an indented body, keep the book's layout.

use std::collections::HashMap;

use super::model::{Align, Block, BlockKind, Chapter, Frame, TextStyle};
use super::styles::round;

/// Text set left of the body beyond this share means the inset is design.
const OUTDENT_SHARE: f64 = 0.01;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    start: u32,
    end: u32,
    frame: Option<Frame>,
}

impl Key {
    fn of(block: &Block) -> Self {
        Self {
            start: block.inset_start.to_bits(),
            end: block.inset_end.to_bits(),
            frame: block.frame,
        }
    }
}

struct Weighed {
    key: Key,
    left_aligned: bool,
    weight: u64,
}

pub(crate) fn normalize(chapters: &mut [Chapter], styles: &[TextStyle]) {
    let weighed: Vec<Weighed> = chapters
        .iter()
        .flat_map(|c| &c.blocks)
        .filter(|b| matches!(b.kind, BlockKind::Text | BlockKind::Heading))
        .map(|b| Weighed {
            key: Key::of(b),
            left_aligned: styles
                .get(b.style as usize)
                .is_none_or(|s| s.align != Some(Align::Center)),
            weight: b.spans.iter().map(|s| s.text.chars().count() as u64).sum(),
        })
        .collect();
    let total: u64 = weighed.iter().map(|w| w.weight).sum();
    let mut by_key: HashMap<Key, u64> = HashMap::new();
    for w in &weighed {
        *by_key.entry(w.key).or_default() += w.weight;
    }
    let Some((body, weight)) = by_key.into_iter().max_by_key(|(_, w)| *w) else {
        return;
    };
    let (start, end) = (f32::from_bits(body.start), f32::from_bits(body.end));
    if start <= end || weight * 2 <= total {
        return;
    }
    let outdented: u64 = weighed
        .iter()
        .filter(|w| w.left_aligned && f32::from_bits(w.key.start) < start)
        .map(|w| w.weight)
        .sum();
    if outdented as f64 > total as f64 * OUTDENT_SHARE {
        return;
    }
    let shift = start - end;
    for chapter in chapters {
        let notes = chapter.notes.values_mut().flatten();
        for block in chapter.blocks.iter_mut().chain(notes) {
            unindent(block, body, shift);
        }
    }
}

fn unindent(block: &mut Block, body: Key, shift: f32) {
    if Key::of(block) == body && body.frame == Some(Frame::Quote) {
        block.frame = None;
        block.frame_plain = false;
    }
    let unshift = |inset: f32| {
        if inset > 0.0 {
            round((inset - shift).max(0.0))
        } else {
            inset
        }
    };
    block.inset_start = unshift(block.inset_start);
    for c in &mut block.containers {
        c.inset_start = unshift(c.inset_start);
    }
}
