mod common;

use common::{Epub, PNG_16X16, PNG_1200X800, chapter, one, texts};
use kob_ebook::reader_pack::{
    Align, Block, BlockKind, Dropcap, Frame, Image, Link, Position, TextStyle,
};

fn style(pack_styles: &[TextStyle], block: &Block) -> TextStyle {
    pack_styles[block.style as usize].clone()
}

#[test]
fn blocks_are_flat_trimmed_and_drop_empty_leaves() {
    let ch = chapter(
        "<div><p>  One  </p><p> </p><div><p>Two <em>and</em>\n  a half</p></div></div><p></p>",
    );
    assert_eq!(texts(&ch.blocks), ["One", "Two and a half"]);
    assert!(ch.blocks.iter().all(|b| b.kind == BlockKind::Text));
}

#[test]
fn headings_code_breaks_and_rules() {
    let ch = chapter(
        "<h2>Title</h2><p>line one<br/>line two</p><hr/><pre>fn main() {\n    go();\n}\n</pre>",
    );
    let kinds: Vec<_> = ch.blocks.iter().map(|b| (b.kind, b.level)).collect();
    assert_eq!(
        kinds,
        [
            (BlockKind::Heading, Some(2)),
            (BlockKind::Text, None),
            (BlockKind::Rule, None),
            (BlockKind::Code, None),
        ]
    );
    assert!(ch.blocks[0].keep_with_next);
    assert_eq!(ch.blocks[1].text(), "line one\nline two");
    assert_eq!(ch.blocks[3].text(), "fn main() {\n    go();\n}");
}

#[test]
fn paragraph_styles() {
    let epub = Epub::new()
        .css(
            ".j { text-align: justify; text-indent: 1.5em; }
             .sc { font-variant: small-caps; }
             .drop { float: left; font-size: 3em; }",
        )
        .doc(
            "c1.xhtml",
            r#"<p class="j">Justified.</p><p class="sc">Small caps.</p><p><span class="drop">O</span>nce upon a time.</p>"#,
        );
    let pack = epub.pack();
    let styles = &pack.manifest.styles;
    assert_eq!(styles[0], TextStyle::default());
    let blocks = &pack.chapters[0].blocks;
    let justified = style(styles, &blocks[0]);
    assert_eq!(
        (justified.align, justified.indent),
        (Some(Align::Justify), Some(1.5))
    );
    assert!(style(styles, &blocks[1]).small_caps);
    let dropped = style(styles, &blocks[2]);
    assert_eq!(dropped.dropcap, Some(Dropcap { lines: 3, chars: 1 }));
    let first_span = &styles[blocks[2].spans[0].style as usize];
    assert_eq!(
        first_span.size, None,
        "the dropcap's size comes from the block"
    );
    assert_eq!(blocks[2].text(), "Once upon a time.");
}

#[test]
fn inline_styles_are_interned_per_span() {
    let pack = Epub::new()
        .doc(
            "c1.xhtml",
            "<p>plain <b>bold</b> <i>it</i> x<sup>2</sup></p>",
        )
        .pack();
    let spans = &pack.chapters[0].blocks[0].spans;
    let styled: Vec<_> = spans
        .iter()
        .map(|s| {
            (
                s.text.as_str(),
                pack.manifest.styles[s.style as usize].clone(),
            )
        })
        .collect();
    let bold = styled.iter().find(|(t, _)| *t == "bold").unwrap();
    assert_eq!(bold.1.weight, Some(700));
    let it = styled.iter().find(|(t, _)| *t == "it").unwrap();
    assert!(it.1.italic);
    let sup = styled.iter().find(|(t, _)| *t == "2").unwrap();
    assert_eq!(
        sup.1.baseline,
        Some(kob_ebook::reader_pack::Baseline::Super)
    );
}

#[test]
fn margins_collapse_and_containers_become_insets() {
    let epub = Epub::new()
        .css(
            "p { margin: 0 0 0.5em 0; } blockquote { margin: 1em 2em; padding: 0; }
             .first { margin-top: 3em; }",
        )
        .doc(
            "c1.xhtml",
            r#"<p>Before.</p><blockquote><blockquote><p class="first">Deep.</p><p>Deeper.</p></blockquote></blockquote><p>After.</p>"#,
        );
    let ch = one(epub);
    let summary: Vec<_> = ch
        .blocks
        .iter()
        .map(|b| {
            (
                b.text(),
                b.space_before,
                b.inset_start,
                b.inset_end,
                b.frame,
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("Before.".into(), 0.0, 0.0, 0.0, None),
            ("Deep.".into(), 3.0, 4.0, 4.0, Some(Frame::Quote)),
            ("Deeper.".into(), 0.5, 4.0, 4.0, Some(Frame::Quote)),
            ("After.".into(), 1.0, 0.0, 0.0, None),
        ]
    );
}

#[test]
fn list_markers() {
    let ch = chapter(
        r#"<ol start="3"><li>three</li><li>four<ul><li>nested</li><li><p>para</p></li></ul></li></ol><ol type="a"><li>alpha</li></ol><p>x</p><ol type="i"><li>a</li><li>b</li></ol>"#,
    );
    let markers: Vec<_> = ch
        .blocks
        .iter()
        .map(|b| (b.text(), b.marker.clone()))
        .collect();
    let m = |t: &str, k: &str| (t.to_string(), Some(k.to_string()));
    assert_eq!(
        markers,
        [
            m("three", "3."),
            m("four", "4."),
            m("nested", "◦"),
            m("para", "◦"),
            m("alpha", "a."),
            ("x".to_string(), None),
            m("a", "i."),
            m("b", "ii.")
        ]
    );
    assert!(ch.blocks[2].inset_start > ch.blocks[1].inset_start);
}

#[test]
fn top_level_bullets() {
    let ch = chapter("<ul><li>one</li><li>two<ul><li>in</li></ul></li></ul>");
    let markers: Vec<_> = ch
        .blocks
        .iter()
        .map(|b| b.marker.clone().unwrap_or_default())
        .collect();
    assert_eq!(markers, ["•", "•", "◦"]);
}

#[test]
fn break_before_reaches_the_first_leaf() {
    let ch = chapter(
        r#"<p>a</p><section style="page-break-before: always"><div><p>b</p></div><p>c</p></section>"#,
    );
    let breaks: Vec<_> = ch.blocks.iter().map(|b| b.break_before).collect();
    assert_eq!(breaks, [false, true, false]);
}

#[test]
fn images() {
    let epub = Epub::new()
        .asset("big.png", "image/png", PNG_1200X800)
        .asset("tiny.png", "image/png", PNG_16X16)
        .doc(
            "c1.xhtml",
            r#"<p><img src="big.png" alt="Map"/></p>
<p>Text with <img src="tiny.png" alt=""/> a glyph and <img src="big.png" alt=""/> a figure.</p>
<div><svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 10 10"><image width="600" height="800" xlink:href="big.png"/></svg></div>
<div><img src="missing.jpg" width="40" height="50" alt=""/></div>"#,
        );
    let ch = one(epub);
    let kinds: Vec<_> = ch.blocks.iter().map(|b| b.kind).collect();
    assert_eq!(
        kinds,
        [
            BlockKind::Image,
            BlockKind::Text,
            BlockKind::Image,
            BlockKind::Image,
            BlockKind::Image
        ]
    );
    let image = |src: &str, alt: Option<&str>, w: u32, h: u32| Image {
        src: src.into(),
        alt: alt.map(String::from),
        width: Some(w),
        height: Some(h),
    };
    assert_eq!(
        ch.blocks[0].image,
        Some(image("OEBPS/big.png", Some("Map"), 1200, 800))
    );
    assert_eq!(ch.blocks[1].text(), "Text with a glyph and a figure.");
    assert_eq!(
        ch.blocks[2].image,
        Some(image("OEBPS/big.png", None, 1200, 800))
    );
    assert_eq!(
        ch.blocks[3].image,
        Some(image("OEBPS/big.png", None, 1200, 800))
    );
    assert_eq!(
        ch.blocks[4].image,
        Some(image("OEBPS/missing.jpg", None, 40, 50))
    );
}

#[test]
fn hidden_elements_leave_the_flow() {
    let ch = chapter(
        r#"<p>shown</p><div hidden="hidden"><p>gone</p></div><p style="display:none">also</p>"#,
    );
    assert_eq!(texts(&ch.blocks), ["shown"]);
}

#[test]
fn tables() {
    let ch = chapter(
        r#"<table><caption>Cap</caption><thead><tr><th>H1</th><th>H2</th></tr></thead>
<tbody><tr><td colspan="2">wide <b>cell</b></td></tr><tr><td><table><tr><td>inner</td></tr></table></td><td><p>a</p><p>b</p></td></tr></tbody></table>"#,
    );
    assert_eq!(texts(&ch.blocks)[0], "Cap");
    let table = ch.blocks[1].table.as_ref().expect("table block");
    assert_eq!(ch.blocks[1].kind, BlockKind::Table);
    let rows: Vec<_> = table
        .rows
        .iter()
        .map(|r| {
            (
                r.header,
                r.cells
                    .iter()
                    .map(|c| (texts(&c.blocks), c.colspan))
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    let cell = |t: &[&str], span: u32| (t.iter().map(|s| s.to_string()).collect::<Vec<_>>(), span);
    assert_eq!(
        rows,
        [
            (true, vec![cell(&["H1"], 1), cell(&["H2"], 1)]),
            (false, vec![cell(&["wide cell"], 2)]),
            (false, vec![cell(&["inner"], 1), cell(&["a", "b"], 1)]),
        ]
    );
}

#[test]
fn footnotes_and_links() {
    let epub = Epub::new()
        .doc(
            "c1.xhtml",
            r##"<p id="top">Read<a epub:type="noteref" href="#fn1">1</a> and<sup><a href="notes.xhtml#en2">[2]</a></sup> then <a href="c2.xhtml#sec">jump</a> or <a href="https://example.com/">leave</a>.</p>
<aside epub:type="footnote" id="fn1"><p>Footnote text.</p></aside>
<p>Tail.</p>"##,
        )
        .doc("c2.xhtml", r#"<p>Intro.</p><h2 id="sec">Section</h2>"#)
        .doc("notes.xhtml", r#"<p>Notes</p><p id="en2"><a href="c1.xhtml">2.</a> Endnote text.</p>"#);
    let pack = epub.pack();
    let c1 = &pack.chapters[0];
    assert_eq!(
        texts(&c1.blocks),
        ["Read1 and[2] then jump or leave.", "Tail."]
    );
    let spans = &c1.blocks[0].spans;
    let by_text = |t: &str| {
        spans
            .iter()
            .find(|s| s.text == t)
            .unwrap_or_else(|| panic!("{t}: {spans:?}"))
    };
    assert_eq!(by_text("1").note.as_deref(), Some("fn1"));
    assert_eq!(by_text("[2]").note.as_deref(), Some("en2"));
    assert_eq!(
        by_text("jump").link,
        Some(Link {
            position: Some(Position {
                chapter: 1,
                block: 1,
                offset: 0
            }),
            href: None
        })
    );
    assert_eq!(
        by_text("leave")
            .link
            .as_ref()
            .and_then(|l| l.href.as_deref()),
        Some("https://example.com/")
    );
    assert_eq!(texts(&c1.notes["fn1"]), ["Footnote text."]);
    assert_eq!(texts(&c1.notes["en2"]), ["2. Endnote text."]);
    assert_eq!(
        texts(&pack.chapters[2].blocks),
        ["Notes", "2. Endnote text."],
        "endnotes stay in the flow"
    );
    assert_eq!(c1.anchors["top"].block, 0);
    assert_eq!(pack.chapters[1].anchors["sec"].block, 1);
}

#[test]
fn inline_anchors_carry_offsets() {
    let ch = chapter(r#"<p>  Lead <span id="mid">middle</span> end</p>"#);
    assert_eq!(ch.anchors["mid"].offset, 5);
}
