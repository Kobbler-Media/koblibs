mod common;

use common::{Epub, PNG_1200X800, one, texts};
use kob_ebook::reader_pack::{
    Block, BlockKind, Borders, Chapter, Dropcap, Line, LineStyle, Measure, ReaderPack, RuleImage,
    RuleStyle, TextStyle,
};

fn pack(css: &str, body: &str) -> ReaderPack {
    Epub::new().css(css).doc("c1.xhtml", body).pack()
}

fn styled(css: &str, body: &str) -> Chapter {
    one(Epub::new().css(css).doc("c1.xhtml", body))
}

fn span_style(pack: &ReaderPack, block: &Block, text: &str) -> TextStyle {
    let span = block
        .spans
        .iter()
        .find(|s| s.text == text)
        .unwrap_or_else(|| panic!("{text}: {:?}", block.spans));
    pack.manifest.styles[span.style as usize].clone()
}

fn solid(width: f32, color: Option<&str>) -> Option<Line> {
    Some(Line {
        width,
        style: LineStyle::Solid,
        color: color.map(String::from),
    })
}

#[test]
fn table_cells_carry_the_book_s_borders() {
    let ch = styled(
        "table { border-collapse: collapse; } td { border: 2px solid #000; } th { border-bottom: 1px solid; }
         tr.total td { border-top: 4px double; }",
        "<table><tr><th>H</th></tr><tr><td>a</td></tr><tr class=\"total\"><td>b</td></tr></table>",
    );
    let table = ch.blocks[0].table.as_ref().expect("table");
    assert!(table.collapse);
    let cell = |r: usize| table.rows[r].cells[0].borders.clone().unwrap_or_default();
    assert_eq!(cell(0).bottom, solid(0.063, None));
    assert_eq!(cell(0).top, None);
    let a = cell(1);
    assert_eq!(
        (a.top.clone(), a.right.clone(), a.bottom.clone(), a.left),
        (
            solid(0.125, Some("#000000")),
            solid(0.125, Some("#000000")),
            solid(0.125, Some("#000000")),
            solid(0.125, Some("#000000"))
        )
    );
    assert_eq!(cell(2).top.map(|l| l.style), Some(LineStyle::Double));

    let plain = styled("", "<table><tr><td>a</td></tr></table>");
    let table = plain.blocks[0].table.as_ref().unwrap();
    assert!(!table.collapse);
    assert_eq!(table.rows[0].cells[0].borders, None);
}

#[test]
fn inline_backgrounds_and_borders() {
    let pack = pack(
        ".mark { background-color: #ffff00; } .tag { border: 1px solid #ff0000; } .clear { background-color: transparent; }
         .term { border-bottom: 1px dotted; }",
        r#"<p>A <span class="mark">marked</span> and <span class="tag">tagged</span> and <span class="term">term</span> and <span class="clear">clear</span> word.</p>"#,
    );
    let block = &pack.chapters[0].blocks[0];
    assert_eq!(
        span_style(&pack, block, "marked").background.as_deref(),
        Some("#ffff00")
    );
    let red = solid(0.063, Some("#ff0000"));
    assert_eq!(
        span_style(&pack, block, "tagged").borders,
        Some(Borders {
            top: red.clone(),
            right: red.clone(),
            bottom: red.clone(),
            left: red,
        })
    );
    let dotted = Some(Line {
        width: 0.063,
        style: LineStyle::Dotted,
        color: None,
    });
    assert_eq!(
        span_style(&pack, block, "term").borders,
        Some(Borders {
            bottom: dotted,
            ..Borders::default()
        }),
        "an underline drawn as a border keeps to its one side"
    );
    assert_eq!(
        span_style(&pack, block, " and clear word."),
        TextStyle::default(),
        "a transparent background is no style at all"
    );
}

#[test]
fn auto_margins_centre_a_block_of_its_own_width() {
    let ch = styled(
        "p { margin: 0; } .half { width: 50%; margin: 0 auto; } .em { width: 20em; margin-left: auto; margin-right: auto; }
         .left { width: 10em; margin-left: auto; } .boxed { width: 10em; margin: 0 auto; border: 2px solid; padding: 0 1em; }",
        r#"<div class="half"><p>Half.</p></div><p class="em">Twenty.</p><p class="left">Left.</p><p>Wide.</p><div class="boxed"><p>Boxed.</p></div>"#,
    );
    let measure = |t: &str| ch.blocks.iter().find(|b| b.text() == t).unwrap().centred;
    assert_eq!(measure("Half."), Some(Measure::Fraction(0.5)));
    assert_eq!(measure("Twenty."), Some(Measure::Em(20.0)));
    assert_eq!(measure("Left."), None, "only both margins auto centre");
    assert_eq!(measure("Wide."), None);
    let boxed = ch.blocks.iter().find(|b| b.text() == "Boxed.").unwrap();
    assert_eq!(boxed.centred, Some(Measure::Em(10.0)));
    let container = &boxed.containers[0];
    assert_eq!(container.centred, Some(Measure::Em(10.0)));
    assert_eq!(container.inner, 2.25, "border and padding on both sides");
}

#[test]
fn rounded_corners() {
    let ch = styled(
        ".r { border: 1px solid; border-radius: 0.5em; } .top { border: 1px solid; border-top-left-radius: 1em; border-top-right-radius: 1em; }",
        r#"<p class="r">Round.</p><p class="top">Top.</p><p>None.</p>"#,
    );
    let radius = |i: usize| ch.blocks[i].containers.first().and_then(|c| c.radius);
    assert_eq!(radius(0), Some([0.5, 0.5, 0.5, 0.5]));
    assert_eq!(
        radius(1),
        Some([1.0, 1.0, 0.0, 0.0]),
        "top-left, top-right, bottom-right, bottom-left"
    );
    assert_eq!(radius(2), None);
}

#[test]
fn first_letter_rules_make_a_dropcap() {
    let pack = pack(
        "p { margin: 0; } p.drop::first-letter { float: left; font-size: 3em; line-height: 0.7; }
         p.old:first-letter { float: left; font-size: 4.5em; }
         p.raised::first-letter { font-size: 2em; }
         p.drop { color: #333; }",
        r#"<p class="drop">Once upon a time.</p><p class="old">Long ago.</p><p class="raised">Raised.</p><p>Plain.</p>"#,
    );
    let ch = &pack.chapters[0];
    let dropcap = |i: usize| pack.manifest.styles[ch.blocks[i].style as usize].dropcap;
    assert_eq!(dropcap(0), Some(Dropcap { lines: 3, chars: 1 }));
    assert_eq!(dropcap(1), Some(Dropcap { lines: 5, chars: 1 }));
    assert_eq!(
        dropcap(2),
        None,
        "a big letter that does not float is no dropcap"
    );
    assert_eq!(dropcap(3), None);
    let block = &pack.manifest.styles[ch.blocks[0].style as usize];
    assert_eq!(
        block.color.as_deref(),
        Some("#333333"),
        "the paragraph keeps its own style"
    );
    assert_eq!(
        block.size, None,
        "the first letter's size stays off the paragraph"
    );
}

#[test]
fn a_rule_the_book_hides_is_gone_and_a_plain_one_stays_the_app_s() {
    let ch = styled(
        "hr.none { border: none; margin: 1em 0; } hr.zero { border-width: 0; }",
        r#"<p>a</p><hr/><p>b</p><hr class="none"/><p>c</p><hr class="zero"/><p>d</p>"#,
    );
    let kinds: Vec<_> = ch
        .blocks
        .iter()
        .map(|b| (b.kind, b.rule.is_some()))
        .collect();
    assert_eq!(
        kinds,
        [
            (BlockKind::Text, false),
            (BlockKind::Rule, false),
            (BlockKind::Text, false),
            (BlockKind::Text, false),
            (BlockKind::Text, false),
        ]
    );
    assert_eq!(texts(&ch.blocks)[3], "c");
    assert_eq!(
        ch.blocks[3].space_before, 1.0,
        "the hidden rule keeps its room"
    );
}

#[test]
fn vertical_align_lengths_shift_the_baseline() {
    let pack = pack(
        ".up { vertical-align: 0.5em; } .down { vertical-align: -4px; } .pct { vertical-align: 50%; line-height: 2; } .sup { vertical-align: super; }",
        r#"<p>x<span class="up">up</span><span class="down">down</span><span class="pct">pct</span><span class="sup">sup</span></p>"#,
    );
    let block = &pack.chapters[0].blocks[0];
    let shift = |t: &str| {
        let s = span_style(&pack, block, t);
        (s.shift, s.baseline)
    };
    assert_eq!(shift("up"), (Some(0.5), None));
    assert_eq!(shift("down"), (Some(-0.25), None));
    assert_eq!(
        shift("pct"),
        (Some(1.0), None),
        "a percentage of the line height"
    );
    assert_eq!(
        shift("sup"),
        (None, Some(kob_ebook::reader_pack::Baseline::Super))
    );
}

#[test]
fn a_rule_drawn_as_a_background_image_keeps_it() {
    let ch = one(
        Epub::new()
            .asset("images/stars.png", "image/png", PNG_1200X800)
            .css(
                "hr.stars { background: url('images/stars.png') no-repeat center; height: 1em; border: none; margin: 1em 0; }
                 hr.tiled { background-image: url(images/stars.png); background-repeat: repeat-x; height: 0.5em; border: none; }",
            )
            .doc(
                "c1.xhtml",
                r#"<p>a</p><hr class="stars"/><p>b</p><hr class="tiled"/><p>c</p>"#,
            ),
    );
    let rules: Vec<_> = ch
        .blocks
        .iter()
        .filter(|b| b.kind == BlockKind::Rule)
        .collect();
    let image = |repeat: bool| {
        Some(RuleImage {
            src: "OEBPS/images/stars.png".into(),
            width: Some(1200),
            height: Some(800),
            repeat,
        })
    };
    assert_eq!(
        rules[0].rule,
        Some(RuleStyle {
            line: None,
            length: None,
            height: Some(1.0),
            image: image(false),
        })
    );
    assert_eq!(
        rules[1].rule.as_ref().map(|r| (r.height, r.image.clone())),
        Some((Some(0.5), image(true)))
    );
}

#[test]
fn a_first_letter_dropcap_takes_its_punctuation_with_it() {
    let pack = pack(
        "p { margin: 0; } p::first-letter { float: left; font-size: 3em; } span.drop { float: left; font-size: 3em; }",
        r#"<p>“Hello,” she said.</p><p>(A) list.</p><p>Plain.</p><p>‘’Twas.</p>"#,
    );
    let ch = &pack.chapters[0];
    let chars: Vec<_> = ch
        .blocks
        .iter()
        .map(|b| {
            pack.manifest.styles[b.style as usize]
                .dropcap
                .map(|d| d.chars)
        })
        .collect();
    assert_eq!(chars, [Some(2), Some(3), Some(1), Some(3)]);
}
