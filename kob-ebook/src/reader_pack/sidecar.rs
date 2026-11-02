use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use super::model::{BlockPos, Position};

/// Block text `[block_offset, block_offset + len)` maps one-to-one onto the
/// source chunk `path` from `src_offset` on. All offsets UTF-16.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment(pub u32, pub u32, pub String, pub u32);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KoboEntry(pub String, pub u32, pub u32);

/// The server-side half of a chapter: where its blocks came from in the
/// source document, as epubcfi and as Kobo span ids.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChapterSidecar {
    pub spine: String,
    pub source: String,
    pub blocks: Vec<Vec<Segment>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub elements: BTreeMap<u32, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kobo: Vec<KoboEntry>,
    #[serde(skip)]
    index: OnceLock<Index>,
}

#[derive(Debug, Clone, Default)]
struct Index {
    spine: Vec<u32>,
    /// Text segments and text-less blocks in document order.
    entries: Vec<Entry>,
    elements: HashMap<Vec<u32>, BlockPos>,
}

#[derive(Debug, Clone)]
struct Entry {
    steps: Vec<u32>,
    src_start: u32,
    src_end: u32,
    pos: BlockPos,
    element: bool,
}

impl Entry {
    fn key(&self) -> (&[u32], u32) {
        (&self.steps, self.src_start)
    }
}

impl ChapterSidecar {
    pub(crate) fn new(
        spine: String,
        source: String,
        blocks: Vec<Vec<Segment>>,
        elements: BTreeMap<u32, String>,
    ) -> Self {
        Self {
            spine,
            source,
            blocks,
            elements,
            kobo: Vec::new(),
            index: OnceLock::new(),
        }
    }

    pub fn cfi_at(&self, block: u32, offset: u32) -> Option<String> {
        let local = self.local_cfi_at(block, offset)?;
        Some(format!("epubcfi({}!{local})", self.spine))
    }

    fn local_cfi_at(&self, block: u32, offset: u32) -> Option<String> {
        let segments = self.blocks.get(block as usize)?;
        let at = |s: &Segment, o: u32| format!("{}:{}", s.2, s.3 + o - s.0);
        if let Some(s) = segments
            .iter()
            .find(|s| s.0 <= offset && offset < s.0 + s.1)
        {
            return Some(at(s, offset));
        }
        if let Some(s) = segments.iter().find(|s| s.0 + s.1 == offset) {
            return Some(at(s, offset));
        }
        if let Some(s) = segments.iter().find(|s| s.0 > offset) {
            return Some(at(s, s.0));
        }
        if let Some(s) = segments.last() {
            return Some(at(s, s.0 + s.1));
        }
        self.elements.get(&block).cloned()
    }

    pub fn kobo_span_at(&self, block: u32, offset: u32) -> Option<String> {
        let i = self
            .kobo
            .partition_point(|k| (k.1, k.2) <= (block, offset))
            .checked_sub(1)?;
        Some(self.kobo[i].0.clone())
    }

    pub fn position_of_kobo_span(&self, span: &str) -> Option<(u32, u32)> {
        self.kobo.iter().find(|k| k.0 == span).map(|k| (k.1, k.2))
    }

    fn index(&self) -> &Index {
        self.index.get_or_init(|| {
            let texts = self
                .blocks
                .iter()
                .enumerate()
                .flat_map(|(block, segments)| {
                    segments.iter().map(move |s| Entry {
                        steps: parse_steps(&s.2).0,
                        src_start: s.3,
                        src_end: s.3 + s.1,
                        pos: BlockPos {
                            block: block as u32,
                            offset: s.0,
                        },
                        element: false,
                    })
                });
            let elements = self.elements.iter().map(|(&block, path)| Entry {
                steps: parse_steps(path).0,
                src_start: 0,
                src_end: 0,
                pos: BlockPos { block, offset: 0 },
                element: true,
            });
            let mut entries: Vec<Entry> = texts.chain(elements).collect();
            entries.sort_by(|a, b| a.key().cmp(&b.key()).then(b.element.cmp(&a.element)));
            let elements = entries
                .iter()
                .filter(|e| e.element)
                .map(|e| (e.steps.clone(), e.pos))
                .collect();
            Index {
                spine: parse_steps(&self.spine).0,
                entries,
                elements,
            }
        })
    }

    /// Resolve an in-document position: an exact segment hit, else the
    /// text-less block (a table, a fixed page) it lies inside, else the first
    /// block content after it in document order, else the chapter end.
    pub(crate) fn resolve_local(&self, steps: &[u32], offset: Option<u32>) -> BlockPos {
        let index = self.index();
        let entries = &index.entries;
        if offset.is_none()
            && let Some(pos) = index.elements.get(steps)
        {
            return *pos;
        }
        let off = offset.unwrap_or(0);
        let i = entries.partition_point(|e| e.key() <= (steps, off));
        if let Some(e) = i.checked_sub(1).and_then(|j| entries.get(j))
            && !e.element
            && e.steps == steps
            && off <= e.src_end
        {
            return BlockPos {
                block: e.pos.block,
                offset: e.pos.offset + off - e.src_start,
            };
        }
        if let Some(pos) = (1..steps.len())
            .rev()
            .find_map(|n| index.elements.get(&steps[..n]))
        {
            return *pos;
        }
        entries.get(i).map(|e| e.pos).unwrap_or_else(|| self.end())
    }

    fn end(&self) -> BlockPos {
        let Some(last) = self.blocks.len().checked_sub(1) else {
            return BlockPos::default();
        };
        let offset = self.blocks[last]
            .iter()
            .map(|s| s.0 + s.1)
            .max()
            .unwrap_or(0);
        BlockPos {
            block: last as u32,
            offset,
        }
    }
}

/// Resolve an epubcfi (point, element or range — a range resolves to its
/// start) against the sidecars of a whole book. The spine step's `[id]`
/// assertion is optional.
pub fn resolve_cfi(sidecars: &[ChapterSidecar], cfi: &str) -> Option<Position> {
    let body = cfi.trim();
    let body = body
        .strip_prefix("epubcfi(")
        .and_then(|s| s.strip_suffix(')'))
        .unwrap_or(body);
    let mut parts = split_unescaped(body, '!');
    let spine = parse_steps(parts.next()?).0;
    let (chapter, sidecar) = sidecars
        .iter()
        .enumerate()
        .find(|(_, s)| s.index().spine == spine)?;
    let pos = match parts.next() {
        Some(local) => {
            let mut range = split_unescaped(local, ',');
            let parent = range.next().unwrap_or("");
            let (mut steps, mut offset) = parse_steps(parent);
            if let Some(start) = range.next() {
                let (more, start_offset) = parse_steps(start);
                steps.extend(more);
                offset = start_offset;
            }
            sidecar.resolve_local(&steps, offset)
        }
        None => BlockPos::default(),
    };
    Some(Position {
        chapter: chapter as u32,
        block: pos.block,
        offset: pos.offset,
    })
}

fn split_unescaped(s: &str, sep: char) -> impl Iterator<Item = &str> {
    let mut rest = Some(s);
    std::iter::from_fn(move || {
        let current = rest?;
        let mut escaped = false;
        for (i, c) in current.char_indices() {
            if escaped {
                escaped = false;
            } else if c == '^' {
                escaped = true;
            } else if c == sep {
                rest = Some(&current[i + 1..]);
                return Some(&current[..i]);
            }
        }
        rest = None;
        Some(current)
    })
}

/// Step indices and the terminal character offset of a CFI path, ignoring
/// id assertions, text assertions and spatial/temporal offsets.
pub(crate) fn parse_steps(path: &str) -> (Vec<u32>, Option<u32>) {
    let mut steps = Vec::new();
    let mut offset = None;
    let bytes = path.as_bytes();
    let mut i = 0;
    let number = |i: &mut usize| {
        let start = *i;
        while *i < bytes.len() && bytes[*i].is_ascii_digit() {
            *i += 1;
        }
        path[start..*i].parse::<u32>().ok()
    };
    while i < bytes.len() {
        match bytes[i] {
            b'/' => {
                i += 1;
                if let Some(n) = number(&mut i) {
                    steps.push(n);
                }
                offset = None;
            }
            b':' => {
                i += 1;
                offset = number(&mut i);
            }
            b'[' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b']' {
                    i += if bytes[i] == b'^' { 2 } else { 1 };
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    (steps, offset)
}

pub(crate) fn push_step(out: &mut String, index: u32, id: Option<&str>) {
    use std::fmt::Write as _;
    let _ = write!(out, "/{index}");
    if let Some(id) = id.filter(|id| !id.is_empty()) {
        out.push('[');
        for c in id.chars() {
            if matches!(c, '^' | '[' | ']' | '(' | ')' | ',' | ';' | '=') {
                out.push('^');
            }
            out.push(c);
        }
        out.push(']');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_steps_offsets_and_assertions() {
        let cases: &[(&str, &[u32], Option<u32>)] = &[
            ("/6/4[ch^]1]", &[6, 4], None),
            ("/4/2[c1]/3:17", &[4, 2, 3], Some(17)),
            ("/4/2/1:3[,a^,b]", &[4, 2, 1], Some(3)),
            ("/4/2/1:3~1.5@2:3", &[4, 2, 1], Some(3)),
        ];
        for (path, steps, offset) in cases {
            assert_eq!(parse_steps(path), (steps.to_vec(), *offset), "{path}");
        }
    }

    #[test]
    fn escapes_ids_like_foliate() {
        let mut out = String::new();
        push_step(&mut out, 4, Some("a[1],b"));
        assert_eq!(out, "/4[a^[1^]^,b]");
    }
}
