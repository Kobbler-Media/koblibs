//! A quick-xml walk of the raw chapter document, numbering nodes as EPUB CFI
//! steps exactly as boko's source map and foliate-js do: elements even,
//! the character data between two elements on the odd index between them,
//! offsets in UTF-16 with line endings normalized as an XML parser would.

use std::collections::HashMap;

use boko::ListStyleType;
use quick_xml::events::{BytesRef, BytesStart, Event};
use quick_xml::reader::Reader;

#[derive(Debug, Default)]
pub(crate) struct PathTracker {
    path: Vec<u32>,
    frames: Vec<Frame>,
}

#[derive(Debug, Default)]
struct Frame {
    elements: u32,
    chunk_len: u32,
}

impl PathTracker {
    pub fn start(&mut self) -> &[u32] {
        if let Some(parent) = self.frames.last_mut() {
            parent.elements += 1;
            parent.chunk_len = 0;
            self.path.push(parent.elements * 2);
        }
        self.frames.push(Frame::default());
        &self.path
    }

    pub fn end(&mut self) {
        if self.frames.pop().is_some() && self.frames.len() <= self.path.len() {
            self.path.pop();
        }
    }

    pub fn empty(&mut self) -> Vec<u32> {
        let path = self.start().to_vec();
        self.end();
        path
    }

    /// Where the next character of character data lands: its chunk's path
    /// and the UTF-16 offset within that chunk.
    pub fn text_position(&self) -> Option<(Vec<u32>, u32)> {
        let frame = self.frames.last()?;
        let mut path = self.path.clone();
        path.push(frame.elements * 2 + 1);
        Some((path, frame.chunk_len))
    }

    pub fn advance(&mut self, utf16_len: u32) {
        if let Some(frame) = self.frames.last_mut() {
            frame.chunk_len += utf16_len;
        }
    }

    /// Feed a source event that is not an element boundary handled by the
    /// caller: character data advances the chunk, everything else is ignored.
    pub fn character_data(&mut self, event: &Event<'_>) {
        let len = match event {
            Event::Text(t) => t.decode().map(|s| dom_len(&s)).unwrap_or(0),
            Event::CData(c) => dom_len(&String::from_utf8_lossy(c)),
            Event::GeneralRef(r) => ref_len(r),
            _ => return,
        };
        self.advance(len);
    }
}

pub(crate) fn dom_len(text: &str) -> u32 {
    let mut len = 0u32;
    let mut prev_cr = false;
    for c in text.chars() {
        if !(prev_cr && c == '\n') {
            len += c.len_utf16() as u32;
        }
        prev_cr = c == '\r';
    }
    len
}

fn ref_len(r: &BytesRef<'_>) -> u32 {
    match r.resolve_char_ref() {
        Ok(Some(c)) => c.len_utf16() as u32,
        _ => 1,
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ElementInfo {
    pub tag: String,
    pub hidden: bool,
    pub href: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// The HTML `type` of a list or list item, which boko does not map.
    pub list_type: Option<ListStyleType>,
}

fn list_type(value: &str) -> Option<ListStyleType> {
    Some(match value.trim() {
        "1" => ListStyleType::Decimal,
        "a" => ListStyleType::LowerAlpha,
        "A" => ListStyleType::UpperAlpha,
        "i" => ListStyleType::LowerRoman,
        "I" => ListStyleType::UpperRoman,
        v if v.eq_ignore_ascii_case("disc") => ListStyleType::Disc,
        v if v.eq_ignore_ascii_case("circle") => ListStyleType::Circle,
        v if v.eq_ignore_ascii_case("square") => ListStyleType::Square,
        _ => return None,
    })
}

/// What boko's IR does not keep about the source document.
#[derive(Debug, Default)]
pub(crate) struct SourceDoc {
    pub title: Option<String>,
    /// Hidden elements and images, keyed by their step indices.
    pub elements: HashMap<Vec<u32>, ElementInfo>,
    pub kobo_spans: Vec<KoboSpan>,
}

/// A `koboSpan` the document already carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KoboSpan {
    pub id: String,
    pub steps: Vec<u32>,
    /// The start of its text, enough to find it again by content.
    pub text: String,
}

pub(crate) const SPAN_TEXT_PREFIX: usize = 128;

pub(crate) fn attr(e: &BytesStart, name: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.local_name().as_ref() == name)
        .map(|a| {
            a.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .map(|v| v.into_owned())
                .unwrap_or_else(|_| String::from_utf8_lossy(&a.value).into_owned())
        })
}

fn has_attr(e: &BytesStart, name: &[u8]) -> bool {
    e.attributes()
        .flatten()
        .any(|a| a.key.local_name().as_ref() == name)
}

fn px(value: Option<String>) -> Option<u32> {
    let v = value?;
    let v = v.trim().trim_end_matches("px").trim();
    v.parse::<f32>()
        .ok()
        .filter(|n| *n > 0.0)
        .map(|n| n.round() as u32)
}

pub(crate) fn scan(input: &[u8]) -> SourceDoc {
    let mut doc = SourceDoc::default();
    let mut reader = Reader::from_reader(input);
    let mut buf = Vec::new();
    let mut tracker = PathTracker::default();
    let mut title: Option<String> = None;
    let mut in_title = false;
    let mut depth = 0usize;
    let mut span_text: Option<(usize, usize)> = None;
    while let Ok(event) = reader.read_event_into(&mut buf) {
        match &event {
            Event::Start(e) | Event::Empty(e) => {
                let empty = matches!(event, Event::Empty(_));
                let local = e.local_name().as_ref().to_ascii_lowercase();
                let path = tracker.start().to_vec();
                let spans = doc.kobo_spans.len();
                observe_element(&mut doc, e, &local, path);
                if doc.kobo_spans.len() > spans && !empty {
                    span_text = Some((spans, depth));
                }
                if !empty {
                    depth += 1;
                }
                if !empty && local == b"title" && doc.title.is_none() && title.is_none() {
                    in_title = true;
                    title = Some(String::new());
                }
                if empty {
                    tracker.end();
                }
            }
            Event::End(_) => {
                tracker.end();
                depth = depth.saturating_sub(1);
                if span_text.is_some_and(|(_, d)| d == depth) {
                    span_text = None;
                }
                if in_title {
                    in_title = false;
                    doc.title = title.take();
                }
            }
            Event::Eof => break,
            other => {
                if in_title
                    && let (Event::Text(t), Some(title)) = (other, title.as_mut())
                    && let Ok(s) = t.decode()
                {
                    title.push_str(&s);
                }
                if let (Some((i, _)), Event::Text(t)) = (span_text, other)
                    && let Ok(s) = t.decode()
                    && doc.kobo_spans[i].text.len() < SPAN_TEXT_PREFIX
                {
                    doc.kobo_spans[i].text.push_str(&s);
                }
                tracker.character_data(other);
            }
        }
        buf.clear();
    }
    doc.title = doc
        .title
        .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|t| !t.is_empty());
    doc
}

fn observe_element(doc: &mut SourceDoc, e: &BytesStart, local: &[u8], path: Vec<u32>) {
    let hidden = has_attr(e, b"hidden");
    let image = matches!(local, b"img" | b"image");
    let listed = if matches!(local, b"ol" | b"ul" | b"li") {
        attr(e, b"type").and_then(|t| list_type(&t))
    } else {
        None
    };
    if local == b"span"
        && let Some(id) = attr(e, b"id")
        && id.starts_with("kobo.")
        && attr(e, b"class").is_some_and(|c| c.split_whitespace().any(|c| c == "koboSpan"))
    {
        doc.kobo_spans.push(KoboSpan {
            id,
            steps: path.clone(),
            text: String::new(),
        });
    }
    if !(hidden || image || listed.is_some()) {
        return;
    }
    let info = ElementInfo {
        tag: String::from_utf8_lossy(local).into_owned(),
        hidden,
        href: if image {
            attr(e, b"src").or_else(|| attr(e, b"href"))
        } else {
            None
        },
        width: if image { px(attr(e, b"width")) } else { None },
        height: if image { px(attr(e, b"height")) } else { None },
        list_type: listed,
    };
    doc.elements.insert(path, info);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_like_boko() {
        let doc = br#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><head><title> A
  title </title></head><body>
<p>a</p><!-- c --><div hidden="hidden"><img src="x.png" width="10" height="20px"/></div><p><span class="koboSpan" id="kobo.1.1">t</span></p></body></html>"#;
        let src = scan(doc);
        assert_eq!(src.title.as_deref(), Some("A title"));
        assert!(src.elements[&vec![4, 4]].hidden);
        let img = &src.elements[&vec![4, 4, 2]];
        assert_eq!(
            (img.tag.as_str(), img.width, img.height),
            ("img", Some(10), Some(20))
        );
        assert_eq!(
            src.kobo_spans,
            vec![KoboSpan {
                id: "kobo.1.1".into(),
                steps: vec![4, 6, 2],
                text: "t".into()
            }]
        );
    }

    #[test]
    fn crlf_counts_once() {
        assert_eq!(dom_len("a\r\nb\rc\u{1F600}"), 7);
    }
}
