//! The gen-3 KEPUB fixed-layout page: `div.kobo-fullpage` holding the page
//! artwork as `svg > image`, beside a hidden `div.kobo-page-text` whose
//! absolutely positioned fragments are the page's visible text. Read from the
//! raw document because boko drops hidden content. Ported from the old
//! client's `EpubXhtmlParser.extractFixedPage`.

use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;

use super::model::{Align, Baseline, FixedPage, Fragment, Image, Span, TextStyle};
use super::source::{PathTracker, attr};
use super::styles::{BASE_PX, StyleTable};
use crate::simple_css::{self, SimpleSelector};

const DEFAULT_PAGE: (u32, u32) = (600, 914);

pub(crate) struct Extracted {
    pub page: FixedPage,
    /// Steps of the two source elements, which leave the reading flow.
    pub excluded: [Vec<u32>; 2],
    pub fullpage: Vec<u32>,
}

#[derive(Debug, Default)]
struct El {
    tag: String,
    id: Option<String>,
    class: String,
    style: String,
    src: Option<String>,
    alt: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    steps: Vec<u32>,
    children: Vec<Node>,
}

#[derive(Debug)]
enum Node {
    El(El),
    Text(String),
}

impl El {
    fn elements(&self) -> impl Iterator<Item = &El> {
        self.children.iter().filter_map(|c| match c {
            Node::El(e) => Some(e),
            Node::Text(_) => None,
        })
    }

    fn has_class(&self, class: &str) -> bool {
        self.class.split_whitespace().any(|c| c == class)
    }

    fn find(&self, pred: &dyn Fn(&El) -> bool) -> Option<&El> {
        if pred(self) {
            return Some(self);
        }
        self.elements().find_map(|e| e.find(pred))
    }

    fn whole_text(&self, out: &mut String) {
        for c in &self.children {
            match c {
                Node::Text(t) => out.push_str(t),
                Node::El(e) => e.whole_text(out),
            }
        }
    }

    fn has_text(&self) -> bool {
        let mut s = String::new();
        self.whole_text(&mut s);
        !s.trim().is_empty()
    }
}

#[derive(Debug, Default, Clone)]
struct Props {
    align: Option<Align>,
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    absolute: bool,
    top_pct: Option<f32>,
    top_px: Option<f32>,
    left_pct: Option<f32>,
    left_px: Option<f32>,
    width_px: Option<f32>,
    font_pct: Option<f32>,
    font_px: Option<f32>,
    color: Option<String>,
}

impl Props {
    fn parse(body: &str) -> Self {
        let mut p = Props::default();
        for (prop, value) in simple_css::declarations(body) {
            let value = value.to_ascii_lowercase();
            let num = |suffix: &str| {
                value
                    .strip_suffix(suffix)
                    .and_then(|v| v.trim().parse::<f32>().ok())
            };
            match prop.as_str() {
                "text-align" => {
                    p.align = if value.contains("center") {
                        Some(Align::Center)
                    } else if value.contains("right") {
                        Some(Align::Right)
                    } else if value.contains("justify") {
                        Some(Align::Justify)
                    } else if value.contains("left") {
                        Some(Align::Start)
                    } else {
                        None
                    }
                }
                "font-weight" => {
                    p.bold |= value.contains("bold") || value.parse::<u32>().is_ok_and(|w| w >= 700)
                }
                "font-style" => p.italic |= value.contains("italic"),
                "text-decoration" => {
                    p.underline |= value.contains("underline");
                    p.strike |= value.contains("line-through");
                }
                "position" => p.absolute |= value == "absolute",
                "top" => {
                    p.top_pct = num("%").map(|v| v / 100.0);
                    p.top_px = if p.top_pct.is_none() {
                        num("px").or_else(|| value.parse().ok())
                    } else {
                        None
                    };
                }
                "left" => {
                    p.left_pct = num("%").map(|v| v / 100.0);
                    p.left_px = if p.left_pct.is_none() {
                        num("px").or_else(|| value.parse().ok())
                    } else {
                        None
                    };
                }
                "width" => p.width_px = num("px").or_else(|| value.parse().ok()),
                "font-size" => {
                    p.font_pct = num("%");
                    p.font_px = if p.font_pct.is_none() {
                        num("px")
                    } else {
                        None
                    };
                }
                "color" => p.color = parse_hex_color(&value),
                _ => {}
            }
        }
        p
    }

    fn merge(mut self, o: &Props) -> Self {
        self.align = o.align.or(self.align);
        self.bold |= o.bold;
        self.italic |= o.italic;
        self.underline |= o.underline;
        self.strike |= o.strike;
        self.absolute |= o.absolute;
        self.top_pct = o.top_pct.or(self.top_pct);
        self.top_px = o.top_px.or(self.top_px);
        self.left_pct = o.left_pct.or(self.left_pct);
        self.left_px = o.left_px.or(self.left_px);
        self.width_px = o.width_px.or(self.width_px);
        self.font_pct = o.font_pct.or(self.font_pct);
        self.font_px = o.font_px.or(self.font_px);
        self.color = o.color.clone().or(self.color);
        self
    }
}

fn parse_hex_color(value: &str) -> Option<String> {
    let hex = value.trim().strip_prefix('#')?;
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let rgb: String = match hex.len() {
        3 => hex.chars().flat_map(|c| [c, c]).collect(),
        6 => hex.to_string(),
        8 => hex[2..].to_string(),
        _ => return None,
    };
    Some(format!("#{}", rgb.to_ascii_lowercase()))
}

struct Sheet(Vec<(SimpleSelector, Props)>);

impl Sheet {
    fn parse(css: &str) -> Self {
        let cleaned = simple_css::strip_comments(css);
        let mut rules = Vec::new();
        for (selector, body) in simple_css::rules(&cleaned) {
            if selector.starts_with('@') {
                continue;
            }
            let props = Props::parse(body);
            for sel in selector.split(',') {
                if let Some(sel) = simple_css::parse_simple_selector(sel.trim()) {
                    rules.push((sel, props.clone()));
                }
            }
        }
        rules.sort_by_key(|(s, _)| s.specificity());
        Sheet(rules)
    }

    fn props(&self, el: &El) -> Props {
        let mut out = Props::default();
        for (sel, props) in &self.0 {
            if sel.matches(&el.tag, el.id.as_deref(), &el.class) {
                out = out.merge(props);
            }
        }
        out.merge(&Props::parse(&el.style))
    }
}

/// Parse the document into a small tree, collecting its stylesheet text
/// (`<style>` contents and `<link rel=stylesheet>` hrefs) on the way.
fn parse_tree(input: &[u8]) -> Option<(El, String, Vec<String>)> {
    let mut reader = Reader::from_reader(input);
    let mut buf = Vec::new();
    let mut tracker = PathTracker::default();
    let mut stack: Vec<El> = vec![El::default()];
    let mut style_text = String::new();
    let mut links = Vec::new();
    let mut in_style = false;
    loop {
        let event = reader.read_event_into(&mut buf).ok()?;
        match &event {
            Event::Start(e) | Event::Empty(e) => {
                let steps = tracker.start().to_vec();
                let el = element(e, steps);
                if el.tag == "link"
                    && attr(e, b"rel").is_some_and(|r| {
                        r.to_ascii_lowercase()
                            .split_whitespace()
                            .any(|t| t == "stylesheet")
                    })
                    && let Some(href) = attr(e, b"href")
                {
                    links.push(href);
                }
                if matches!(event, Event::Empty(_)) {
                    tracker.end();
                    stack.last_mut()?.children.push(Node::El(el));
                } else {
                    in_style = el.tag == "style";
                    stack.push(el);
                }
            }
            Event::End(_) => {
                tracker.end();
                in_style = false;
                let el = stack.pop()?;
                stack.last_mut()?.children.push(Node::El(el));
            }
            Event::Eof => break,
            other => {
                tracker.character_data(other);
                let text = match other {
                    Event::Text(t) => t.decode().ok().map(|s| s.into_owned()),
                    Event::CData(c) => Some(String::from_utf8_lossy(c).into_owned()),
                    Event::GeneralRef(r) => Some(match r.resolve_char_ref() {
                        Ok(Some(c)) => c.to_string(),
                        _ => match &**r {
                            b"amp" => "&".into(),
                            b"lt" => "<".into(),
                            b"gt" => ">".into(),
                            b"quot" => "\"".into(),
                            b"apos" => "'".into(),
                            _ => "\u{a0}".into(),
                        },
                    }),
                    _ => None,
                };
                if let Some(text) = text {
                    if in_style {
                        style_text.push_str(&text);
                    }
                    stack.last_mut()?.children.push(Node::Text(text));
                }
            }
        }
        buf.clear();
    }
    let root = stack.into_iter().next()?;
    Some((root, style_text, links))
}

fn element(e: &BytesStart, steps: Vec<u32>) -> El {
    let num = |name: &[u8]| attr(e, name).and_then(|v| v.trim().parse::<u32>().ok());
    El {
        tag: String::from_utf8_lossy(e.local_name().as_ref()).to_ascii_lowercase(),
        id: attr(e, b"id"),
        class: attr(e, b"class").unwrap_or_default(),
        style: attr(e, b"style").unwrap_or_default(),
        src: attr(e, b"src")
            .or_else(|| attr(e, b"href"))
            .filter(|s| !s.trim().is_empty()),
        alt: attr(e, b"alt").filter(|s| !s.trim().is_empty()),
        width: num(b"width"),
        height: num(b"height"),
        steps,
        children: Vec::new(),
    }
}

/// `linked` reads a stylesheet by its href as written in the document;
/// `resolve` turns the artwork's href into an archive path; `intrinsic`
/// measures it.
pub(crate) fn extract(
    input: &[u8],
    linked: &dyn Fn(&str) -> Option<String>,
    resolve: &dyn Fn(&str) -> String,
    intrinsic: &dyn Fn(&str) -> Option<(u32, u32)>,
    styles: &mut StyleTable,
) -> Option<Extracted> {
    if !input
        .windows(b"kobo-fullpage".len())
        .any(|w| w == b"kobo-fullpage")
    {
        return None;
    }
    let (root, mut css, links) = parse_tree(input)?;
    for href in links {
        if let Some(sheet) = linked(&href) {
            css.push('\n');
            css.push_str(&sheet);
        }
    }
    let sheet = Sheet::parse(&css);
    let body = root.find(&|e| e.tag == "body")?;
    let (parent, fullpage) = find_fullpage(body)?;
    let layer = parent.elements().find(|e| e.has_class("kobo-page-text"))?;
    let img = fullpage.find(&|e| e.tag == "image")?;
    let src = resolve(img.src.as_deref()?);
    let size = match (img.width, img.height) {
        (Some(w), Some(h)) if w > 0 && h > 0 => Some((w, h)),
        _ => None,
    };
    let (w, h) = size.or_else(|| intrinsic(&src)).unwrap_or(DEFAULT_PAGE);
    let mut fragments = Vec::new();
    collect_fragments(layer, &sheet, (w as f32, h as f32), styles, &mut fragments);
    if fragments.is_empty() {
        return None;
    }
    Some(Extracted {
        page: FixedPage {
            image: Image {
                src,
                alt: img.alt.clone(),
                width: Some(w),
                height: Some(h),
            },
            page_width: w,
            page_height: h,
            fragments,
        },
        excluded: [fullpage.steps.clone(), layer.steps.clone()],
        fullpage: fullpage.steps.clone(),
    })
}

fn find_fullpage(el: &El) -> Option<(&El, &El)> {
    for child in el.elements() {
        if child.tag == "div"
            && child.has_class("kobo-fullpage")
            && child.find(&|e| e.tag == "image").is_some()
        {
            return Some((el, child));
        }
        if let Some(found) = find_fullpage(child) {
            return Some(found);
        }
    }
    None
}

fn collect_fragments(
    el: &El,
    sheet: &Sheet,
    (pw, ph): (f32, f32),
    styles: &mut StyleTable,
    out: &mut Vec<Fragment>,
) {
    for child in el.elements() {
        let css = sheet.props(child);
        let positioned = css.absolute && (css.top_pct.is_some() || css.top_px.is_some());
        if !positioned {
            collect_fragments(child, sheet, (pw, ph), styles, out);
            continue;
        }
        if !child.has_text() {
            continue;
        }
        let chain = text_chain(child);
        let mut font_size = BASE_PX;
        let mut color = None;
        for el in &chain {
            let p = sheet.props(el);
            if let Some(px) = p.font_px {
                font_size = px;
            } else if let Some(pct) = p.font_pct {
                font_size *= pct / 100.0;
            }
            if p.color.is_some() {
                color = p.color;
            }
        }
        let mut spans = Vec::new();
        inline_spans(child, &Inline::default(), sheet, styles, &mut spans);
        normalize_spans(&mut spans);
        out.push(Fragment {
            top: css
                .top_pct
                .unwrap_or_else(|| css.top_px.unwrap_or(0.0) / ph),
            left: css
                .left_pct
                .unwrap_or_else(|| css.left_px.unwrap_or(0.0) / pw),
            width: css.width_px.unwrap_or(pw),
            font_size,
            color,
            align: css.align.unwrap_or(Align::Start),
            spans,
        });
    }
}

/// From the fragment down to its deepest text-bearing descendant, following
/// the first child that carries text at each level.
fn text_chain(el: &El) -> Vec<&El> {
    let mut chain = vec![el];
    let mut cur = el;
    while let Some(next) = cur.elements().find(|c| c.has_text()) {
        chain.push(next);
        cur = next;
    }
    chain
}

#[derive(Debug, Default, Clone)]
struct Inline {
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    mono: bool,
    baseline: Option<Baseline>,
    size: Option<f32>,
}

impl Inline {
    fn apply(&self, el: &El, sheet: &Sheet) -> Inline {
        let mut s = self.clone();
        match el.tag.as_str() {
            "b" | "strong" => s.bold = true,
            "i" | "em" | "cite" => s.italic = true,
            "u" => s.underline = true,
            "s" | "del" | "strike" => s.strike = true,
            "code" | "tt" | "kbd" | "samp" => s.mono = true,
            "sup" => s.baseline = Some(Baseline::Super),
            "sub" => s.baseline = Some(Baseline::Sub),
            "small" => s.size = Some(0.85),
            "big" => s.size = Some(1.2),
            _ => {}
        }
        let css = sheet.props(el);
        s.bold |= css.bold;
        s.italic |= css.italic;
        s.underline |= css.underline;
        s.strike |= css.strike;
        s
    }

    fn style(&self) -> TextStyle {
        TextStyle {
            size: self.size,
            weight: self.bold.then_some(700),
            italic: self.italic,
            family: self.mono.then(|| "monospace".to_string()),
            underline: self.underline,
            strike: self.strike,
            baseline: self.baseline,
            ..TextStyle::default()
        }
    }
}

fn inline_spans(
    el: &El,
    parent: &Inline,
    sheet: &Sheet,
    styles: &mut StyleTable,
    out: &mut Vec<Span>,
) {
    let style = parent.apply(el, sheet);
    for child in &el.children {
        match child {
            Node::Text(t) => {
                let id = styles.intern(style.style());
                push_text(out, t, id);
            }
            Node::El(e) if e.tag == "br" => {
                let id = styles.intern(style.style());
                push_text(out, LINE_BREAK, id);
            }
            Node::El(e) if matches!(e.tag.as_str(), "img" | "image" | "svg") => {}
            Node::El(e) => inline_spans(e, &style, sheet, styles, out),
        }
    }
}

fn push_text(out: &mut Vec<Span>, text: &str, style: u32) {
    match out.last_mut() {
        Some(last) if last.style == style => last.text.push_str(text),
        _ => out.push(Span {
            text: text.to_string(),
            style,
            ..Span::default()
        }),
    }
}

/// Stands in for `<br>` until whitespace is collapsed, so it survives.
const LINE_BREAK: &str = "\u{0}";

/// Collapse whitespace the way the page would render it, keeping `<br>`
/// line breaks, and trim the fragment's edges.
fn normalize_spans(spans: &mut Vec<Span>) {
    let mut prev_space = true;
    for span in spans.iter_mut() {
        let mut text = String::with_capacity(span.text.len());
        for c in span.text.chars() {
            if c == '\u{0}' {
                if text.ends_with(' ') {
                    text.pop();
                }
                text.push('\n');
                prev_space = true;
            } else if c.is_whitespace() {
                if !prev_space {
                    text.push(' ');
                }
                prev_space = true;
            } else {
                text.push(c);
                prev_space = false;
            }
        }
        span.text = text;
    }
    if let Some(last) = spans.last_mut() {
        let trimmed = last.text.trim_end_matches(' ').len();
        last.text.truncate(trimmed);
    }
    spans.retain(|s| !s.text.is_empty());
}
