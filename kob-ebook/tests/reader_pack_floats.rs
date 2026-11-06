mod common;

use common::{Epub, PNG_1200X800, one, texts};
use kob_ebook::reader_pack::{Block, BlockKind, Chapter, Clear, Float, Side};

fn floated(css: &str, body: &str) -> Chapter {
    one(Epub::new()
        .css(css)
        .asset("pic.png", "image/png", PNG_1200X800)
        .doc("c1.xhtml", body))
}

fn kinds(ch: &Chapter) -> Vec<BlockKind> {
    ch.blocks.iter().map(|b| b.kind).collect()
}

fn image(ch: &Chapter) -> &Block {
    ch.blocks
        .iter()
        .find(|b| b.kind == BlockKind::Image)
        .unwrap_or_else(|| panic!("no image: {:?}", texts(&ch.blocks)))
}

#[test]
fn an_image_floated_inside_its_paragraph_comes_first_and_keeps_the_paragraph_spacing() {
    let ch = floated(
        "p { margin: 1em 0 0; } .imgLeft { float: left; width: 40%; margin-right: 0.5em; padding: 0.25em; }",
        r#"<p>Before.</p><p><img class="imgLeft" src="pic.png" alt="Bobo"/>Bobo reloaded his slingshot.</p>"#,
    );
    assert_eq!(
        kinds(&ch),
        [BlockKind::Text, BlockKind::Image, BlockKind::Text]
    );
    let img = image(&ch);
    assert_eq!(
        img.float,
        Some(Float {
            side: Side::Left,
            gap: 0.75
        })
    );
    assert_eq!(img.image.as_ref().unwrap().column_width, Some(0.4));
    let text = &ch.blocks[2];
    assert_eq!(text.text(), "Bobo reloaded his slingshot.");
    assert_eq!(text.float, None);
    assert_eq!(
        img.space_before, text.space_before,
        "a float leaves the flow where it was"
    );
    assert_eq!(text.space_before, 1.0);
}

#[test]
fn a_floated_box_holding_only_an_image_floats_the_image_at_the_box_width() {
    let ch = floated(
        "p { margin: 0; } div.w10r { float: right; width: 15%; margin: 0 0 0 0.25em; } img.w100 { width: 100%; }",
        r#"<p>Before.</p><div class="w10r"><img class="w100" src="pic.png" alt=""/></div><p>In the summer of 2011.</p>"#,
    );
    assert_eq!(
        kinds(&ch),
        [BlockKind::Text, BlockKind::Image, BlockKind::Text]
    );
    let img = image(&ch);
    assert_eq!(
        img.float,
        Some(Float {
            side: Side::Right,
            gap: 0.25
        })
    );
    assert_eq!(img.image.as_ref().unwrap().column_width, Some(0.15));
}

#[test]
fn a_floated_initial_image_floats_before_its_paragraph() {
    let ch = floated(
        "span.dropcaps { float: left; margin-right: .09em; }",
        r#"<p><span class="dropcaps"><img src="pic.png" alt="A"/></span>T A CROSSROADS STATION.</p>"#,
    );
    assert_eq!(kinds(&ch), [BlockKind::Image, BlockKind::Text]);
    assert_eq!(image(&ch).float.map(|f| f.side), Some(Side::Left));
}

#[test]
fn what_cannot_float_as_an_image_flows_as_before() {
    let ch = floated(
        ".side { float: right; width: 30%; } .mid { float: left; }",
        r#"<div class="side"><p>A sidebar.</p><p>Of two paragraphs.</p></div>
<p>Text with <img class="mid" src="pic.png" alt=""/> a float in the middle.</p>
<div class="side"><img src="pic.png" alt=""/><p>Captioned.</p></div>"#,
    );
    assert!(
        ch.blocks
            .iter()
            .filter(|b| b.kind != BlockKind::Image)
            .all(|b| b.float.is_none()),
        "text never floats: {:?}",
        ch.blocks
    );
    let images: Vec<_> = ch
        .blocks
        .iter()
        .filter(|b| b.kind == BlockKind::Image)
        .collect();
    assert_eq!(images.len(), 2);
    assert_eq!(
        images[0].float.map(|f| f.side),
        Some(Side::Left),
        "an image floated mid-paragraph floats beside what follows"
    );
    assert_eq!(
        kinds(&ch)[2..4],
        [BlockKind::Text, BlockKind::Image],
        "after the text it was found in"
    );
    assert_eq!(images[1].float, None, "a box holding more than its image");
}

#[test]
fn clear_moves_a_block_below_the_floats_it_names() {
    let ch = floated(
        ".l { clear: left; } .r { clear: right; } .b { clear: both; } img { float: left; }",
        r#"<p><img src="pic.png" alt=""/>Beside.</p><p class="l">Left.</p><p class="r">Right.</p><div class="b"><p>Both.</p></div><p>None.</p>"#,
    );
    let clear = |t: &str| {
        ch.blocks
            .iter()
            .find(|b| b.text() == t)
            .unwrap_or_else(|| panic!("{t}"))
            .clear
    };
    assert_eq!(clear("Beside."), None);
    assert_eq!(clear("Left."), Some(Clear::Left));
    assert_eq!(clear("Right."), Some(Clear::Right));
    assert_eq!(clear("Both."), Some(Clear::Both));
    assert_eq!(clear("None."), None);
}

#[test]
fn floats_and_clears_serialise_as_the_app_reads_them() {
    let block = Block {
        kind: BlockKind::Image,
        float: Some(Float {
            side: Side::Right,
            gap: 0.5,
        }),
        clear: Some(Clear::Both),
        ..Block::default()
    };
    let json = serde_json::to_value(&block).unwrap();
    assert_eq!(
        json["float"],
        serde_json::json!({"side": "right", "gap": 0.5})
    );
    assert_eq!(json["clear"], "both");
    let plain = serde_json::to_value(Block::default()).unwrap();
    assert!(plain.get("float").is_none() && plain.get("clear").is_none());
}
