mod common;

use std::io::{Cursor, Read};

use common::{Epub, PNG_1200X800, utf16_slice, xhtml};
use kob_ebook::reader_pack::{
    BlockKind, ChapterSidecar, FixedLayout, Position, ReaderPack, SpreadSide, read_asset,
    resolve_cfi,
};
use quick_xml::events::Event;

const CHAPTER: &str = include_str!("fixtures/chapter.xhtml");
const KEPUB: &str = include_str!("fixtures/kepub.xhtml");

fn fixture_book() -> Epub {
    Epub::new()
        .doc_with("chapter.xhtml", r#"id="ch1""#, CHAPTER)
        .doc_with("kepub.xhtml", "", KEPUB)
        .doc(
            "mixed.xhtml",
            "<h1>Mixed  <i>content</i></h1><p>  Lead\n  space, <b>bold</b>&#160;and&amp;more</p><pre>  keep\n    this</pre><p>after<br/>break é😀 x</p>",
        )
}

fn find(pack: &ReaderPack, chapter: usize, needle: &str) -> (u32, u32) {
    for (b, block) in pack.chapters[chapter].blocks.iter().enumerate() {
        let text = block.text();
        if let Some(i) = text.find(needle) {
            return (b as u32, text[..i].encode_utf16().count() as u32);
        }
    }
    panic!("{needle:?} not in chapter {chapter}");
}

#[test]
fn spine_prefixes_match_foliate() {
    let pack = fixture_book().pack();
    let spines: Vec<_> = pack.sidecars.iter().map(|s| s.spine.as_str()).collect();
    assert_eq!(spines, ["/6/2[ch1]", "/6/4", "/6/6"]);
    let sources: Vec<_> = pack.sidecars.iter().map(|s| s.source.as_str()).collect();
    assert_eq!(sources, ["chapter.xhtml", "kepub.xhtml", "mixed.xhtml"]);
}

#[test]
fn text_cfis_match_foliate() {
    let pack = fixture_book().pack();
    let cases = [
        (0, "One", "/4/2[c1]/2/1:10"),
        (0, "bright", "/4/2[c1]/4/2/1:0"),
        (0, "cold day", "/4/2[c1]/4/3:1"),
        (0, "striking", "/4/2[c1]/4/5:29"),
        (0, "nose", "/4/2[c1]/4/5:65"),
        (0, "graph", "/4/2[c1]/8[p^[2^]]/2/2/1:0"),
        (0, "émoji", "/4/2[c1]/8[p^[2^]]/3:19"),
        (0, "here.", "/4/2[c1]/8[p^[2^]]/3:28"),
        (0, "line two", "/4/2[c1]/10/3:0"),
        (0, "Quoted", "/4/2[c1]/14/2/1:0"),
        (1, "Loose", "/4/1:0"),
        (
            1,
            "second",
            "/4/2[book-columns]/2[book-inner]/2/4[kobo.1.2]/1:6",
        ),
        (
            1,
            "sentence!",
            "/4/2[book-columns]/2[book-inner]/2/4[kobo.1.2]/1:16",
        ),
        (1, "keeps", "/4/2[book-columns]/2[book-inner]/4/1:9"),
        (
            1,
            "nested",
            "/4/2[book-columns]/2[book-inner]/6/2/2/2/2/2/1:0",
        ),
        (1, "odd", "/4/2[book-columns]/2[book-inner]/8/1:161"),
    ];
    for (chapter, needle, local) in cases {
        let (block, offset) = find(&pack, chapter, needle);
        let sidecar = &pack.sidecars[chapter];
        let want = format!("epubcfi({}!{local})", sidecar.spine);
        assert_eq!(
            sidecar.cfi_at(block, offset),
            Some(want.clone()),
            "{needle}"
        );
        let back = resolve_cfi(&pack.sidecars, &want).unwrap();
        assert_eq!(
            back,
            Position {
                chapter: chapter as u32,
                block,
                offset
            },
            "{needle}"
        );
    }
}

#[test]
fn every_text_offset_round_trips() {
    let checked = assert_cfis_round_trip(&fixture_book().pack());
    assert!(checked > 400, "only {checked} offsets checked");
}

/// Every non-whitespace offset of every text block survives
/// `cfi_at` → `resolve_cfi`. Returns how many offsets it checked.
fn assert_cfis_round_trip(pack: &ReaderPack) -> usize {
    let mut checked = 0;
    for (c, chapter) in pack.chapters.iter().enumerate() {
        for (b, block) in chapter.blocks.iter().enumerate() {
            if !matches!(
                block.kind,
                BlockKind::Text | BlockKind::Heading | BlockKind::Code
            ) {
                continue;
            }
            let mut offset = 0u32;
            for ch in block.text().chars() {
                if !ch.is_whitespace() {
                    let cfi = pack.sidecars[c].cfi_at(b as u32, offset).expect("cfi");
                    let pos = resolve_cfi(&pack.sidecars, &cfi);
                    let want = Position {
                        chapter: c as u32,
                        block: b as u32,
                        offset,
                    };
                    assert_eq!(pos, Some(want), "{cfi} in {:?}", block.text());
                    checked += 1;
                }
                offset += ch.len_utf16() as u32;
            }
        }
    }
    checked
}

/// Books in `$KOB_EBOOK_CORPUS`, when set: every pack builds, and its CFIs
/// and Kobo spans hold up the same way the fixtures' do.
#[test]
fn corpus() {
    let Ok(dir) = std::env::var("KOB_EBOOK_CORPUS") else {
        return;
    };
    let mut books: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "epub"))
        .collect();
    books.sort();
    for path in books {
        let epub = std::fs::read(&path).unwrap();
        let started = std::time::Instant::now();
        let pack = kob_ebook::reader_pack::build(&epub)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let built = started.elapsed();
        let cfis = assert_cfis_round_trip(&pack);
        let spans = assert_kobo_spans_match(&epub, &pack);
        eprintln!(
            "{}: {built:?}, {cfis} offsets, {spans} spans",
            path.display()
        );
    }
}

#[test]
fn foreign_cfi_forms_resolve() {
    let pack = fixture_book().pack();
    let bright = find(&pack, 0, "bright");
    let cases = [
        ("epubcfi(/6/2!/4/2[c1]/4/2/1:0)", bright),
        ("epubcfi(/6/2[ch1]!/4/2[c1]/4,/2/1:0,/5:20)", bright),
        ("/6/2[ch1]!/4/2[c1]/4/2/1:0", bright),
        ("epubcfi(/6/2[ch1]!/4/2[c1]/4)", find(&pack, 0, "It was")),
        ("epubcfi(/6/2[ch1]!/4/2[c1]/6)", find(&pack, 0, "Second")),
        ("epubcfi(/6/4)", (0, 0)),
    ];
    for (cfi, (block, offset)) in cases {
        let pos = resolve_cfi(&pack.sidecars, cfi).unwrap_or_else(|| panic!("{cfi}"));
        assert_eq!((pos.block, pos.offset), (block, offset), "{cfi}");
    }
    assert_eq!(resolve_cfi(&pack.sidecars, "epubcfi(/6/40!/4/2)"), None);
    let end = resolve_cfi(&pack.sidecars, "epubcfi(/6/2!/4/2[c1]/99/1:0)").unwrap();
    assert_eq!(end.block as usize, pack.chapters[0].blocks.len() - 1);
}

#[test]
fn sidecars_survive_json() {
    let pack = fixture_book().pack();
    let json = serde_json::to_string(&pack.sidecars).unwrap();
    let back: Vec<ChapterSidecar> = serde_json::from_str(&json).unwrap();
    let (block, offset) = find(&pack, 0, "graph");
    let cfi = back[0].cfi_at(block, offset).unwrap();
    assert_eq!(
        resolve_cfi(&back, &cfi),
        Some(Position {
            chapter: 0,
            block,
            offset
        })
    );
    let chapter_json = serde_json::to_value(&pack.chapters[0]).unwrap();
    assert!(chapter_json.to_string().contains(r#""kind":"heading""#));
    assert!(!chapter_json.to_string().contains("target"));
}

#[test]
fn non_text_blocks_map_to_elements() {
    let pack = Epub::new()
        .asset("big.png", "image/png", PNG_1200X800)
        .doc(
            "c1.xhtml",
            r#"<p>a</p><div><img src="big.png" alt=""/></div><hr/><p>b</p>"#,
        )
        .pack();
    let s = &pack.sidecars[0];
    assert_eq!(s.cfi_at(1, 0).as_deref(), Some("epubcfi(/6/2!/4/4/2)"));
    assert_eq!(s.cfi_at(2, 0).as_deref(), Some("epubcfi(/6/2!/4/6)"));
    assert_eq!(
        resolve_cfi(&pack.sidecars, "epubcfi(/6/2!/4/4/2)").map(|p| p.block),
        Some(1)
    );
}

#[test]
fn spine_compaction_and_weights() {
    let pack = Epub::new()
        .doc("a.xhtml", "<p>A</p>")
        .missing("gone.xhtml")
        .doc_with("Upper.xhtml", "", &xhtml("u", "<p>é😀</p>"))
        .metadata("")
        .build();
    let pack = {
        let mut bytes = pack;
        let opf = String::from_utf8(read_entry(&bytes, "OEBPS/content.opf")).unwrap();
        bytes = rewrite_entry(
            &bytes,
            "OEBPS/content.opf",
            opf.replace(r#"href="Upper.xhtml""#, r#"href="upper.xhtml""#)
                .as_bytes(),
        );
        kob_ebook::reader_pack::build(&bytes).unwrap()
    };
    let hrefs: Vec<_> = pack
        .manifest
        .chapters
        .iter()
        .map(|c| c.href.as_str())
        .collect();
    assert_eq!(hrefs, ["OEBPS/a.xhtml", "OEBPS/Upper.xhtml"]);
    assert_eq!(texts_of(&pack, 1), ["é😀"]);
    let doc = xhtml("u", "<p>é😀</p>");
    assert_eq!(
        pack.manifest.chapters[1].weight,
        doc.encode_utf16().count() as u64
    );
    assert_eq!(
        pack.sidecars[1].spine, "/6/6",
        "the itemref keeps its own step"
    );
}

fn texts_of(pack: &ReaderPack, chapter: usize) -> Vec<String> {
    pack.chapters[chapter]
        .blocks
        .iter()
        .map(|b| b.text())
        .collect()
}

fn read_entry(zip: &[u8], name: &str) -> Vec<u8> {
    let mut archive = zip::ZipArchive::new(Cursor::new(zip)).unwrap();
    let mut out = Vec::new();
    archive
        .by_name(name)
        .unwrap()
        .read_to_end(&mut out)
        .unwrap();
    out
}

fn rewrite_entry(zip: &[u8], name: &str, bytes: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut archive = zip::ZipArchive::new(Cursor::new(zip)).unwrap();
    let mut out = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).unwrap();
        let entry_name = entry.name().to_string();
        let mut data = Vec::new();
        entry.read_to_end(&mut data).unwrap();
        out.start_file(&entry_name, zip::write::SimpleFileOptions::default())
            .unwrap();
        out.write_all(if entry_name == name { bytes } else { &data })
            .unwrap();
    }
    out.finish().unwrap().into_inner()
}

#[test]
fn chapter_titles() {
    let pack = Epub::new()
        .title("11/22/63")
        .doc_with("cover.xhtml", "", &xhtml("Cover", "<p>c</p>"))
        .doc_with("front.xhtml", "", &xhtml("11/22/63", "<p>f</p>"))
        .doc_with("ch1.xhtml", "", &xhtml("11/22/63", "<p>1</p>"))
        .doc_with("ch1-cont.xhtml", "", &xhtml("11/22/63", "<p>1b</p>"))
        .doc_with("ch2.xhtml", "", &xhtml("11/22/63", "<p>2</p>"))
        .ncx("PART 1", "ch1.xhtml")
        .ncx("Chapter One", "ch1.xhtml#start")
        .ncx("Chapter Two", "ch2.xhtml")
        .pack();
    let titles: Vec<_> = pack
        .manifest
        .chapters
        .iter()
        .map(|c| c.title.as_deref())
        .collect();
    assert_eq!(
        titles,
        [
            Some("Cover"),
            None,
            Some("Chapter One"),
            Some("Chapter One"),
            Some("Chapter Two")
        ]
    );
}

#[test]
fn toc_targets_are_positions() {
    let pack = Epub::new()
        .doc("a.xhtml", r#"<p>a</p><h2 id="s2">Two</h2>"#)
        .doc("b.xhtml", "<p>b</p>")
        .ncx("Start", "a.xhtml")
        .ncx("Section", "a.xhtml#s2")
        .ncx("Broken", "a.xhtml#nope")
        .ncx("B", "b.xhtml")
        .pack();
    let toc: Vec<_> = pack
        .manifest
        .toc
        .iter()
        .map(|e| (e.title.as_str(), e.target))
        .collect();
    let at = |chapter, block| Position {
        chapter,
        block,
        offset: 0,
    };
    assert_eq!(
        toc,
        [("Start", at(0, 0)), ("Section", at(0, 1)), ("B", at(1, 0))]
    );
}

#[test]
fn rendition_and_direction() {
    let props = [
        "page-spread-left",
        "rendition:page-spread-center",
        "rendition:spread-none",
        "",
        "rendition:page-spread-right",
    ];
    let mut epub = Epub::new()
        .metadata(r#"<meta property="rendition:layout">pre-paginated</meta><meta property="rendition:spread">auto</meta>"#)
        .spine_attrs(r#"page-progression-direction="rtl""#);
    for (i, p) in props.iter().enumerate() {
        epub = epub.doc_with(
            &format!("p{i}.xhtml"),
            &format!(r#"properties="{p}""#),
            &xhtml("p", "<p>x</p>"),
        );
    }
    let pack = epub.pack();
    assert!(pack.manifest.rtl);
    let fl = |side, spreads| Some(FixedLayout { side, spreads });
    let layouts: Vec<_> = pack
        .manifest
        .chapters
        .iter()
        .map(|c| c.fixed_layout)
        .collect();
    assert_eq!(
        layouts,
        [
            fl(SpreadSide::Left, true),
            fl(SpreadSide::Center, true),
            fl(SpreadSide::Auto, false),
            fl(SpreadSide::Auto, true),
            fl(SpreadSide::Right, true),
        ]
    );

    let pack = Epub::new()
        .metadata(r#"<meta property="rendition:spread">none</meta>"#)
        .doc("a.xhtml", "<p>a</p>")
        .doc_with(
            "b.xhtml",
            r#"properties="rendition:layout-pre-paginated page-spread-right""#,
            &xhtml("b", "<p>b</p>"),
        )
        .pack();
    assert!(!pack.manifest.rtl);
    let layouts: Vec<_> = pack
        .manifest
        .chapters
        .iter()
        .map(|c| c.fixed_layout)
        .collect();
    assert_eq!(layouts, [None, fl(SpreadSide::Right, false)]);
}

#[test]
fn manifest_metadata_and_fonts() {
    let pack = Epub::new()
        .title("Nineteen")
        .css(r#"@font-face { font-family: "Libre"; font-weight: bold; src: url(fonts/a.otf); } p { font-family: "Libre", serif; } h1 { font-family: Unknown, monospace; }"#)
        .asset("fonts/a.otf", "font/otf", b"OTTO")
        .doc("a.xhtml", "<h1>T</h1><p>a</p>")
        .pack();
    let m = &pack.manifest;
    assert_eq!(
        (
            m.title.as_str(),
            m.authors.as_slice(),
            m.language.as_deref()
        ),
        ("Nineteen", &["Ann Author".to_string()][..], Some("en"))
    );
    assert_eq!(m.fonts.len(), 1);
    assert_eq!(
        (
            m.fonts[0].family.as_str(),
            m.fonts[0].weight,
            m.fonts[0].src.as_str()
        ),
        ("Libre", 700, "OEBPS/fonts/a.otf")
    );
    let blocks = &pack.chapters[0].blocks;
    assert_eq!(
        m.styles[blocks[1].style as usize].family.as_deref(),
        Some("Libre")
    );
    assert_eq!(
        m.styles[blocks[0].style as usize].family.as_deref(),
        Some("monospace")
    );
}

/// Each koboSpan's text, in document order, from an XHTML document.
fn kobo_span_texts(doc: &[u8]) -> Vec<(String, String)> {
    let mut reader = quick_xml::Reader::from_reader(doc);
    let mut buf = Vec::new();
    let mut out: Vec<(String, String)> = Vec::new();
    let mut open = false;
    loop {
        match reader.read_event_into(&mut buf).unwrap() {
            Event::Start(e) if e.name().as_ref() == b"span" => {
                let id = e
                    .attributes()
                    .flatten()
                    .find(|a| a.key.as_ref() == b"id")
                    .map(|a| String::from_utf8_lossy(&a.value).into_owned())
                    .unwrap_or_default();
                if id.starts_with("kobo.") {
                    out.push((id, String::new()));
                    open = true;
                }
            }
            Event::End(e) if e.name().as_ref() == b"span" => open = false,
            Event::Text(t) if open => out.last_mut().unwrap().1.push_str(&t.decode().unwrap()),
            Event::Empty(e) if open && e.name().as_ref() == b"img" => {}
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

fn normalized(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn kobo_spans_of_a_generated_kepub_map_to_their_text() {
    let epub = fixture_book()
        .asset("big.png", "image/png", PNG_1200X800)
        .doc(
            "more.xhtml",
            r#"<h2>Head. Line</h2><p>One. Two? “Three!” Four; five</p><p>Pic <img src="big.png" alt=""/> after. &#8220;Quoted&#8221; end.</p><ul><li>Item. Next</li></ul>"#,
        )
        .build();
    let pack = kob_ebook::reader_pack::build(&epub).unwrap();
    let checked = assert_kobo_spans_match(&epub, &pack);
    assert!(checked > 15, "only {checked} spans checked");
}

/// Kepubify the book and check every span the KEPUB carries maps to where
/// its text starts in the pack. Returns how many spans it checked.
fn assert_kobo_spans_match(epub: &[u8], pack: &ReaderPack) -> usize {
    let kepub = kob_ebook::kepubify_bytes(epub).unwrap();
    let mut checked = 0;
    for (c, entry) in pack.manifest.chapters.iter().enumerate() {
        let doc = read_entry(&kepub, &entry.href);
        let sidecar = &pack.sidecars[c];
        for (id, text) in kobo_span_texts(&doc) {
            let (block, offset) = sidecar
                .position_of_kobo_span(&id)
                .unwrap_or_else(|| panic!("{id} unmapped in {}", entry.href));
            let want = normalized(&text);
            if want.is_empty() {
                continue;
            }
            let block = &pack.chapters[c].blocks[block as usize];
            if block.kind != BlockKind::Text
                && block.kind != BlockKind::Heading
                && block.kind != BlockKind::Code
            {
                continue;
            }
            let at = normalized(&utf16_slice(&block.text(), offset));
            assert!(
                at.starts_with(&want),
                "{} {id}: {want:?} vs {at:?}",
                entry.href
            );
            checked += 1;
        }
    }
    checked
}

#[test]
fn existing_kobo_spans_are_used_directly() {
    let pack = fixture_book().pack();
    let s = &pack.sidecars[1];
    let first = find(&pack, 1, "The first");
    let second = find(&pack, 1, "The second");
    assert_eq!(s.position_of_kobo_span("kobo.1.1"), Some(first));
    assert_eq!(s.position_of_kobo_span("kobo.1.2"), Some(second));
    assert_eq!(
        s.kobo_span_at(second.0, second.1 + 3).as_deref(),
        Some("kobo.1.2")
    );
    assert_eq!(s.kobo.len(), 2);
}

#[test]
fn gen3_fixed_page() {
    let page = r#"<?xml version='1.0' encoding='UTF-8'?>
<html xmlns="http://www.w3.org/1999/xhtml">
<head><title>Pants</title><link rel="stylesheet" type="text/css" href="styles/style0000.css"/></head>
<body>
<div id="book-columns"><div id="book-inner">
<div class="kobo-fullpage"><svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" version="1.1" viewBox="0 0 600 914" preserveAspectRatio="xMidYMid meet"><image width="600" height="914" xlink:href="images/image_0013.jpg"/></svg></div>
<div class="kobo-page-text" hidden="hidden">
<div class="fs"><div><div style="position: relative;overflow: hidden;width:600;height:914;">
<div class="page_6_1"><span class="justify style2"><span class="koboSpan" id="kobo.0.1">It happened when we were touring the </span></span></div>
<div class="page_6_2"><span class="justify style2"><span class="koboSpan" id="kobo.0.2">house of a <b>famous</b> author. </span></span></div>
</div></div></div>
</div>
<p>After the page.</p>
</div></div>
</body></html>"#;
    let css = "div.fs { height: 914px; width:600px; position: relative; }
div.page_6_1 { top:35.61%; left:22.66%; position:absolute; width:378.19px;text-align:justify; }
div.page_6_2 { top:38.91%; left:16.66%; position:absolute; width:232.75px; }
.style2 { font-family:ebheofmnimrod;font-size:108.12%;color:#242323; }";
    let pack = Epub::new()
        .metadata(r#"<meta property="rendition:layout">pre-paginated</meta>"#)
        .file("OEBPS/styles/style0000.css", css.as_bytes())
        .doc_with("part0006.html", "", page)
        .pack();
    let blocks = &pack.chapters[0].blocks;
    assert_eq!(blocks[0].kind, BlockKind::FixedPage);
    let fixed = blocks[0].fixed.as_ref().unwrap();
    assert_eq!(fixed.image.src, "OEBPS/images/image_0013.jpg");
    assert_eq!((fixed.page_width, fixed.page_height), (600, 914));
    assert_eq!(fixed.fragments.len(), 2);
    let f1 = &fixed.fragments[0];
    assert!((f1.top - 0.3561).abs() < 1e-4 && (f1.left - 0.2266).abs() < 1e-4);
    assert!((f1.width - 378.19).abs() < 0.01);
    assert!((f1.font_size - 16.0 * 1.0812).abs() < 0.01);
    assert_eq!(f1.color.as_deref(), Some("#242323"));
    assert_eq!(f1.align, kob_ebook::reader_pack::Align::Justify);
    let text = |f: &kob_ebook::reader_pack::Fragment| {
        f.spans.iter().map(|s| s.text.as_str()).collect::<String>()
    };
    assert_eq!(text(f1), "It happened when we were touring the");
    assert_eq!(text(&fixed.fragments[1]), "house of a famous author.");
    let bold = fixed.fragments[1]
        .spans
        .iter()
        .find(|s| s.text == "famous")
        .unwrap();
    assert_eq!(pack.manifest.styles[bold.style as usize].weight, Some(700));
    let rest: Vec<_> = blocks[1..].iter().map(|b| b.text()).collect();
    assert_eq!(rest, ["After the page."]);
    assert_eq!(
        pack.sidecars[0].position_of_kobo_span("kobo.0.2"),
        Some((0, 0))
    );
    assert_eq!(
        pack.sidecars[0].cfi_at(0, 0).as_deref(),
        Some("epubcfi(/6/2!/4/2/2/2)")
    );
}

#[test]
fn fixed_page_without_positioned_text_is_artwork_only() {
    let page = xhtml(
        "p",
        r#"<div class="kobo-fullpage"><svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 600 914"><image width="600" height="914" xlink:href="images/page001.jpg"/></svg></div>
<div class="kobo-page-text" hidden="hidden"><p>And that's when I see him.</p></div>"#,
    );
    let pack = Epub::new().doc_with("p.xhtml", "", &page).pack();
    let blocks = &pack.chapters[0].blocks;
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].kind, BlockKind::Image);
    assert_eq!(
        blocks[0].image.as_ref().unwrap().src,
        "OEBPS/images/page001.jpg"
    );
}

#[test]
fn assets_read_by_archive_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("book.epub");
    std::fs::write(
        &path,
        Epub::new()
            .asset("big.png", "image/png", PNG_1200X800)
            .doc("a.xhtml", "<p>a</p>")
            .build(),
    )
    .unwrap();
    assert_eq!(read_asset(&path, "OEBPS/big.png").unwrap(), PNG_1200X800);
    assert!(read_asset(&path, "OEBPS/nope.png").is_err());
    assert!(read_asset(&path, "OEBPS/nope.otf").is_err());
}
