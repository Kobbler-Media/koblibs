mod common;

use common::{Epub, one};
use kob_ebook::reader_pack::{Block, BlockKind, Chapter, Frame};

fn styled(css: &str, body: &str) -> Chapter {
    one(Epub::new().css(css).doc("c1.xhtml", body))
}

fn first_text(ch: &Chapter) -> &Block {
    ch.blocks.iter().find(|b| !b.text().is_empty()).unwrap()
}

#[test]
fn blank_break_lines_hold_a_line_each() {
    let css = "p { margin: 0; line-height: 1.2; } .tall { min-height: 32px; } .gap { margin-top: 1em; } .h { height: 3em; } .pct { height: 100%; } .abs { position: absolute; height: 40em; }";
    let cases = [
        ("one spacer", r#"<p><br/></p><p>Title</p>"#, 1.2),
        (
            "three spacers",
            r#"<p><br/></p><p><br/></p><p><br/></p><p>Title</p>"#,
            3.6,
        ),
        ("two breaks in one", r#"<p><br/><br/></p><p>Title</p>"#, 2.4),
        (
            "min-height wins",
            r#"<p class="tall"><br/></p><p>Title</p>"#,
            2.0,
        ),
        (
            "margin below a spacer does not collapse into it",
            r#"<p><br/></p><p class="gap">Title</p>"#,
            2.2,
        ),
        (
            "no-break spaces hold a line",
            r#"<p>&#160;</p><p> &#160; </p><p>Title</p>"#,
            2.4,
        ),
        (
            "a no-break space after a break is a line of its own",
            r#"<p><br/>&#160;</p><p>Title</p>"#,
            2.4,
        ),
        ("plain spaces hold nothing", r#"<p> </p><p>Title</p>"#, 0.0),
        (
            "an empty element keeps its height",
            r#"<div class="h"></div><p>Title</p>"#,
            3.0,
        ),
        (
            "an empty element keeps its min-height",
            r#"<div class="tall"></div><p>Title</p>"#,
            2.0,
        ),
        (
            "an absolutely positioned frame holds no room",
            r#"<div class="abs"></div><div class="abs"><p>&#160;</p><p><br/></p></div><p>Title</p>"#,
            0.0,
        ),
        (
            "a percentage height of an auto-height parent is auto",
            r#"<div class="pct"></div><p>Title</p>"#,
            0.0,
        ),
    ];
    for (name, body, want) in cases {
        let ch = styled(css, body);
        assert_eq!(ch.blocks.len(), 1, "{name}");
        assert!(
            (first_text(&ch).space_before - want).abs() < 0.01,
            "{name}: {}",
            ch.blocks[0].space_before
        );
    }
}

fn paragraphs(class: &str, n: usize) -> String {
    (0..n)
        .map(|i| format!(r#"<blockquote class="{class}">Body paragraph {i} with enough words to carry weight.</blockquote>"#))
        .collect()
}

fn summary_of(blocks: &[Block]) -> Vec<(f32, f32, Option<Frame>)> {
    blocks
        .iter()
        .map(|b| (b.inset_start, b.inset_end, b.frame))
        .collect()
}

fn summary(ch: &Chapter) -> Vec<(f32, f32, Option<Frame>)> {
    summary_of(&ch.blocks)
}

#[test]
fn a_one_sided_inset_on_all_body_text_is_dropped() {
    let css = "blockquote { margin: 0; } .b { margin-left: 2em; text-indent: 2em; } .q { margin-left: 4em; } h1 { margin: 0; text-align: center; }";
    let body = format!(
        r#"<h1>Chapter</h1>{}<blockquote class="q">A real quote.</blockquote>{}"#,
        paragraphs("b", 4),
        paragraphs("b", 4)
    );
    let ch = styled(css, &body);
    let s = summary(&ch);
    assert_eq!(s[0], (0.0, 0.0, None), "heading");
    assert!(
        s[1..5]
            .iter()
            .chain(&s[6..])
            .all(|b| *b == (0.0, 0.0, None)),
        "body: {s:?}"
    );
    assert_eq!(
        s[5],
        (2.0, 0.0, Some(Frame::Quote)),
        "the quote keeps its own inset"
    );
}

#[test]
fn insets_that_are_not_a_one_sided_body_inset_stay() {
    let cases = [
        (
            "symmetric body margins",
            "blockquote { margin: 0 1em; }",
            (1.0, 1.0),
        ),
        ("unindented body", "blockquote { margin: 0; }", (0.0, 0.0)),
    ];
    for (name, css, want) in cases {
        let ch = styled(css, &paragraphs("b", 4));
        assert!(
            summary(&ch)
                .iter()
                .all(|b| (b.0, b.1) == want && b.2 == Some(Frame::Quote)),
            "{name}: {:?}",
            summary(&ch)
        );
    }
}

#[test]
fn notes_lose_the_body_inset_too() {
    let css = "blockquote { margin: 0; } .b { margin-left: 2em; }";
    let pack = Epub::new()
        .css(css)
        .doc(
            "c1.xhtml",
            &format!(
                r##"<blockquote class="b">Read<a href="notes.xhtml#en1">1</a>.</blockquote>{}"##,
                paragraphs("b", 6)
            ),
        )
        .doc(
            "notes.xhtml",
            r#"<blockquote class="b" id="en1"><a href="c1.xhtml">1.</a> A note.</blockquote>"#,
        )
        .pack();
    let note = &pack.chapters[0].notes["en1"];
    assert_eq!(summary_of(note), [(0.0, 0.0, None)]);
}

#[test]
fn text_set_left_of_the_body_keeps_the_body_inset() {
    let css = "blockquote { margin: 0 0 0 4em; } h2 { margin: 0; text-align: left; } .hang { margin-left: -1em; }";
    let poem = |title: &str| format!("<h2>{title}</h2>{}", paragraphs("line", 3));
    let body = format!(
        "{}{}",
        poem("A rather long poem title to hang left"),
        poem("Another title hanging left of it")
    );
    let ch = styled(css, &body);
    assert!(
        ch.blocks
            .iter()
            .filter(|b| b.kind == BlockKind::Text)
            .all(|b| b.inset_start == 4.0),
        "{:?}",
        summary(&ch)
    );

    let hanging = styled(
        "blockquote { margin: 0 0 0 2em; } p.hang { margin: 0 0 0 -1em; }",
        &format!(r#"<p class="hang">Hangs.</p>{}"#, paragraphs("b", 6)),
    );
    assert_eq!(
        summary(&hanging)[0],
        (-1.0, 0.0, None),
        "a hanging inset is the book's own"
    );
}
