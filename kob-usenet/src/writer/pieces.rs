use std::ops::Range;

use crate::vendor::core::{FileMeta, Segments};
use crate::vendor::nntp::BodyExpectation;

#[derive(Clone, Debug)]
pub struct Piece {
    pub id: String,
    pub volume_offset: u64,
    pub size: u32,
    pub trusted: bool,
    pub src: Range<usize>,
    pub dest: u64,
}

impl Piece {
    pub fn expectation(&self) -> BodyExpectation {
        if !self.trusted {
            return BodyExpectation::NONE;
        }
        BodyExpectation {
            offset: Some(self.volume_offset),
            decoded_len: Some(u64::from(self.size)),
            ..BodyExpectation::NONE
        }
    }

    pub fn fits(&self, offset: u64, len: usize) -> bool {
        offset == self.volume_offset && len == self.size as usize
    }

    pub const fn len(&self) -> u64 {
        (self.src.end - self.src.start) as u64
    }
}

pub fn of(meta: &FileMeta) -> Vec<Piece> {
    match meta {
        FileMeta::Nzb(file) => {
            let size = file.segments.size();
            span(&file.segments, 0, 0..size, 0)
        }
        FileMeta::Multipart(file) => {
            let mut at = 0;
            let mut pieces = Vec::new();
            for part in &file.parts {
                let data = part.data.start..part.data.end;
                pieces.extend(span(&part.segments, part.offset, data, at));
                at += part.data.len();
            }
            pieces
        }
    }
}

fn span(segments: &Segments, start: u64, data: Range<u64>, dest: u64) -> Vec<Piece> {
    let mut pieces = Vec::new();
    let mut at = start;
    for segment in &segments.items {
        let end = at + u64::from(segment.size);
        let (from, to) = (at.max(data.start), end.min(data.end));
        if from < to {
            pieces.push(Piece {
                id: segment.id.clone(),
                volume_offset: at,
                size: segment.size,
                trusted: segments.sizes_trusted,
                src: usize::try_from(from - at).unwrap_or(usize::MAX)
                    ..usize::try_from(to - at).unwrap_or(usize::MAX),
                dest: dest + (from - data.start),
            });
        }
        at = end;
    }
    pieces
}
