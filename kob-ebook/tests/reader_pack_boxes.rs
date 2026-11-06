mod common;

use common::{Epub, PNG_1200X800, one, texts};
use kob_ebook::reader_pack::{
    Baseline, Block, Chapter, Container, Frame, Line, LineStyle, RuleStyle,
};

fn styled(css: &str, body: &str) -> Chapter {
    one(Epub::new().css(css).doc("c1.xhtml", body))
}

fn by_text<'a>(ch: &'a Chapter, text: &str) -> &'a Block {
    ch.blocks
        .iter()
        .find(|b| b.text() == text)
        .unwrap_or_else(|| panic!("{text}: {:?}", texts(&ch.blocks)))
}

fn ids(block: &Block) -> Vec<u32> {
    block.containers.iter().map(|c| c.id).collect()
}

fn line(width: f32, style: LineStyle, color: Option<&str>) -> Option<Line> {
    Some(Line {
        width,
        style,
        color: color.map(String::from),
    })
}

#[test]
fn a_bordered_box_spans_its_blocks_and_keeps_its_own_identity() {
    let ch = styled(
        "p { margin: 0; }
         .box { border: 2px solid #333; padding: 0.5em; margin: 1em 2em; background-color: #ddd; }",
        r#"<p>Before.</p><div class="box"><p>One.</p><p>Two.</p></div><div class="box"><p>Three.</p></div><p>After.</p>"#,
    );
    let (one, two, three) = (
        by_text(&ch, "One."),
        by_text(&ch, "Two."),
        by_text(&ch, "Three."),
    );
    assert!(by_text(&ch, "Before.").containers.is_empty());
    assert!(by_text(&ch, "After.").containers.is_empty());
    assert_eq!(ids(one), ids(two), "one box around both paragraphs");
    assert_eq!(ids(one).len(), 1);
    assert_ne!(ids(one), ids(three), "adjacent boxes stay apart");

    let solid = line(0.125, LineStyle::Solid, Some("#333333"));
    assert_eq!(
        one.containers[0],
        Container {
            id: one.containers[0].id,
            inset_start: 2.0,
            inset_end: 2.0,
            space_top: 0.625,
            space_bottom: 0.625,
            top: solid.clone(),
            right: solid.clone(),
            bottom: solid.clone(),
            left: solid,
            background: Some("#dddddd".into()),
            ..Container::default()
        }
    );
    assert_eq!((one.inset_start, one.inset_end), (2.625, 2.625));
    let spacing: Vec<_> = ["One.", "Two.", "Three.", "After."]
        .iter()
        .map(|t| by_text(&ch, t).space_before)
        .collect();
    assert_eq!(
        spacing,
        [1.625, 0.0, 2.25, 1.625],
        "margins collapse outside a border, never through it"
    );
}

#[test]
fn nested_boxes_list_outermost_first() {
    let ch = styled(
        "p { margin: 0; } .outer { border: 1px solid; } .inner { border-left: 4px solid; margin: 0 1em; }",
        r#"<div class="outer"><p>Out.</p><div class="inner"><p>In.</p></div></div>"#,
    );
    let (out, inner) = (by_text(&ch, "Out."), by_text(&ch, "In."));
    assert_eq!(ids(out).len(), 1);
    assert_eq!(ids(inner).len(), 2);
    assert_eq!(ids(inner)[0], ids(out)[0]);
    let nested = &inner.containers[1];
    assert_eq!(nested.inset_start, 1.063);
    assert_eq!((nested.top.clone(), nested.right.clone()), (None, None));
    assert_eq!(nested.left, line(0.25, LineStyle::Solid, None));
}

#[test]
fn border_sides_styles_and_default_width() {
    let ch = styled(
        ".t { border-top: 3px dashed red; border-bottom: thin dotted; }
         .d { border-left: 0.2em double blue; }
         .m { border: solid; }
         .none { border: currentColor none 0; }
         .zero { border: 0 solid; }",
        r#"<p class="t">T</p><p class="d">D</p><p class="m">M</p><p class="none">N</p><p class="zero">Z</p>"#,
    );
    let t = &by_text(&ch, "T").containers[0];
    assert_eq!(t.top, line(0.188, LineStyle::Dashed, Some("#ff0000")));
    assert_eq!(t.bottom, line(0.063, LineStyle::Dotted, None));
    assert_eq!((t.left.clone(), t.right.clone()), (None, None));
    let d = &by_text(&ch, "D").containers[0];
    assert_eq!(d.left, line(0.2, LineStyle::Double, Some("#0000ff")));
    let m = &by_text(&ch, "M").containers[0];
    assert_eq!(m.right, line(0.188, LineStyle::Solid, None), "medium");
    assert!(by_text(&ch, "N").containers.is_empty());
    assert!(by_text(&ch, "Z").containers.is_empty());
}

#[test]
fn a_shaded_paragraph_is_a_box_of_its_own() {
    let ch = styled(
        ".shade { background-color: #c0c0c0; } .clear { background-color: transparent; }",
        r#"<p class="shade">Shaded.</p><p class="clear">Clear.</p>"#,
    );
    let shaded = &by_text(&ch, "Shaded.").containers;
    assert_eq!(shaded.len(), 1);
    assert_eq!(shaded[0].background.as_deref(), Some("#c0c0c0"));
    assert_eq!(shaded[0].top, None);
    assert!(by_text(&ch, "Clear.").containers.is_empty());
}

#[test]
fn rules_draw_their_own_line_at_their_own_length() {
    let ch = styled(
        "hr { margin: 0.9em 40%; border-top: 2px solid; }
         hr.bar { margin: 0.5em 0; height: 2px; background-color: #000; border: none; }
         hr.wide { margin: 0; width: 50%; border: none; border-bottom: 1px dashed #808080; }",
        r#"<p>a</p><hr/><p>b</p><hr class="bar"/><p>c</p><hr class="wide"/><p>d</p>"#,
    );
    let rules: Vec<_> = ch.blocks.iter().filter(|b| b.text().is_empty()).collect();
    assert_eq!(rules.len(), 3);
    assert!(rules.iter().all(|r| r.containers.is_empty()));
    assert_eq!(
        rules[0].rule,
        Some(RuleStyle {
            line: line(0.125, LineStyle::Solid, None),
            length: Some(0.2),
            height: None,
            image: None,
        })
    );
    assert_eq!(
        (rules[0].inset_start, rules[0].inset_end),
        (0.0, 0.0),
        "a percentage margin becomes the rule's length, not a fixed inset"
    );
    assert_eq!(
        rules[1].rule.as_ref().and_then(|r| r.line.clone()),
        line(0.125, LineStyle::Solid, Some("#000000"))
    );
    assert_eq!(
        rules[2].rule,
        Some(RuleStyle {
            line: line(0.063, LineStyle::Dashed, Some("#808080")),
            length: Some(0.5),
            height: None,
            image: None,
        })
    );
}

#[test]
fn quotes_say_whether_the_book_styled_them() {
    let ch = styled(
        "p { margin: 0; } blockquote.flush { margin: 0.5em 0; } blockquote.boxed { border-left: 3px solid; }",
        r#"<blockquote><p>Plain.</p></blockquote><blockquote class="flush"><p>Flush.</p></blockquote><blockquote class="boxed"><p>Boxed.</p></blockquote>"#,
    );
    let plain = by_text(&ch, "Plain.");
    assert_eq!((plain.frame, plain.frame_plain), (Some(Frame::Quote), true));
    let flush = by_text(&ch, "Flush.");
    assert_eq!(
        (flush.frame, flush.frame_plain),
        (Some(Frame::Quote), false)
    );
    let boxed = by_text(&ch, "Boxed.");
    assert!(!boxed.frame_plain);
    assert_eq!(boxed.containers.len(), 1);
}

#[test]
fn images_keep_a_percentage_width() {
    let ch = one(
        Epub::new()
            .asset("big.png", "image/png", PNG_1200X800)
            .css("img.part { width: 40%; } img.px { width: 300px; }")
            .doc(
                "c1.xhtml",
                r#"<div><img class="part" src="big.png" alt=""/></div><div><img class="px" src="big.png" alt=""/></div><div><img src="big.png" alt=""/></div>"#,
            ),
    );
    let widths: Vec<_> = ch
        .blocks
        .iter()
        .map(|b| b.image.as_ref().and_then(|i| i.column_width))
        .collect();
    assert_eq!(widths, [Some(0.4), None, None]);
}

#[test]
fn top_aligned_inlines_read_as_superscript() {
    let pack = Epub::new()
        .css("sup { vertical-align: top; } .low { vertical-align: text-bottom; } td { vertical-align: top; }")
        .doc(
            "c1.xhtml",
            r#"<p>Mark<sup>®</sup> and <span class="low">low</span></p><table><tr><td>cell</td></tr></table>"#,
        )
        .pack();
    let ch = &pack.chapters[0];
    let baseline = |block: &Block, t: &str| {
        let span = block.spans.iter().find(|s| s.text == t).unwrap();
        pack.manifest.styles[span.style as usize].baseline
    };
    assert_eq!(baseline(&ch.blocks[0], "®"), Some(Baseline::Super));
    assert_eq!(baseline(&ch.blocks[0], "low"), Some(Baseline::Sub));
    let table = ch.blocks[1].table.as_ref().unwrap();
    assert_eq!(baseline(&table.rows[0].cells[0].blocks[0], "cell"), None);
}

#[test]
fn a_bordered_table_is_one_box_and_its_cells_none() {
    let ch = styled(
        "table, td { border: 1px solid black; }",
        "<table><tr><td>a</td><td>b</td></tr></table>",
    );
    assert_eq!(ch.blocks.len(), 1);
    assert_eq!(ch.blocks[0].containers.len(), 1);
    let cells = &ch.blocks[0].table.as_ref().unwrap().rows[0].cells;
    assert!(
        cells
            .iter()
            .all(|c| c.blocks.iter().all(|b| b.containers.is_empty()))
    );
}
