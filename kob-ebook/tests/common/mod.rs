#![allow(dead_code)]

use std::io::{Cursor, Write};

use kob_ebook::reader_pack::{Block, Chapter, ReaderPack};
use zip::write::{SimpleFileOptions, ZipWriter};

pub const PNG_1200X800: &[u8] = &[
    0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13, b'I', b'H', b'D', b'R', 0, 0,
    0x04, 0xb0, 0, 0, 0x03, 0x20, 8, 2, 0, 0, 0, 0, 0, 0, 0,
];

pub const PNG_16X16: &[u8] = &[
    0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13, b'I', b'H', b'D', b'R', 0, 0, 0,
    16, 0, 0, 0, 16, 8, 2, 0, 0, 0, 0, 0, 0, 0,
];

/// An XHTML document around `body`, linking `style.css`.
pub fn xhtml(title: &str, body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops">
<head><title>{title}</title><link rel="stylesheet" type="text/css" href="style.css"/></head>
<body>{body}</body>
</html>"#
    )
}

struct Item {
    id: String,
    href: String,
    media: String,
    itemref: Option<String>,
}

/// An in-memory EPUB: documents in `OEBPS/`, spine in insertion order.
pub struct Epub {
    title: String,
    metadata: String,
    spine_attrs: String,
    items: Vec<Item>,
    files: Vec<(String, Vec<u8>)>,
    ncx: Vec<(String, String)>,
    css: String,
}

impl Default for Epub {
    fn default() -> Self {
        Self {
            title: "The Book".into(),
            metadata: String::new(),
            spine_attrs: String::new(),
            items: Vec::new(),
            files: Vec::new(),
            ncx: Vec::new(),
            css: String::new(),
        }
    }
}

impl Epub {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn title(mut self, title: &str) -> Self {
        self.title = title.into();
        self
    }

    pub fn metadata(mut self, xml: &str) -> Self {
        self.metadata.push_str(xml);
        self
    }

    pub fn spine_attrs(mut self, attrs: &str) -> Self {
        self.spine_attrs = attrs.into();
        self
    }

    pub fn css(mut self, css: &str) -> Self {
        self.css.push_str(css);
        self
    }

    /// A spine document; `itemref` is the itemref's attributes.
    pub fn doc_with(mut self, href: &str, itemref: &str, content: &str) -> Self {
        let id = format!("i{}", self.items.len());
        self.items.push(Item {
            id,
            href: href.into(),
            media: "application/xhtml+xml".into(),
            itemref: Some(itemref.into()),
        });
        self.files
            .push((format!("OEBPS/{href}"), content.as_bytes().to_vec()));
        self
    }

    pub fn doc(self, href: &str, body: &str) -> Self {
        let content = xhtml(href, body);
        self.doc_with(href, "", &content)
    }

    /// A spine entry whose manifest item names `href` without adding a file.
    pub fn missing(mut self, href: &str) -> Self {
        let id = format!("i{}", self.items.len());
        self.items.push(Item {
            id,
            href: href.into(),
            media: "application/xhtml+xml".into(),
            itemref: Some(String::new()),
        });
        self
    }

    pub fn asset(mut self, path: &str, media: &str, bytes: &[u8]) -> Self {
        let id = format!("i{}", self.items.len());
        self.items.push(Item {
            id,
            href: path.into(),
            media: media.into(),
            itemref: None,
        });
        self.files.push((format!("OEBPS/{path}"), bytes.to_vec()));
        self
    }

    pub fn file(mut self, path: &str, bytes: &[u8]) -> Self {
        self.files.push((path.into(), bytes.to_vec()));
        self
    }

    pub fn ncx(mut self, label: &str, src: &str) -> Self {
        self.ncx.push((label.into(), src.into()));
        self
    }

    pub fn build(self) -> Vec<u8> {
        let mut manifest = String::new();
        let mut spine = String::new();
        for item in &self.items {
            manifest.push_str(&format!(
                r#"<item id="{}" href="{}" media-type="{}"/>"#,
                item.id, item.href, item.media
            ));
            if let Some(attrs) = &item.itemref {
                spine.push_str(&format!(r#"<itemref idref="{}" {attrs}/>"#, item.id));
            }
        }
        let has_ncx = !self.ncx.is_empty();
        if has_ncx {
            manifest.push_str(
                r#"<item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>"#,
            );
        }
        manifest.push_str(r#"<item id="css" href="style.css" media-type="text/css"/>"#);
        let opf = format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="uid">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>{}</dc:title><dc:creator>Ann Author</dc:creator><dc:language>en</dc:language>
    <dc:identifier id="uid">urn:uuid:1</dc:identifier>{}
  </metadata>
  <manifest>{manifest}</manifest>
  <spine{}{}>{spine}</spine>
</package>"#,
            self.title,
            self.metadata,
            if has_ncx { r#" toc="ncx""# } else { "" },
            if self.spine_attrs.is_empty() {
                String::new()
            } else {
                format!(" {}", self.spine_attrs)
            },
        );
        let nav_points: String = self
            .ncx
            .iter()
            .enumerate()
            .map(|(i, (label, src))| {
                format!(
                    r#"<navPoint id="n{i}" playOrder="{}"><navLabel><text>{label}</text></navLabel><content src="{src}"/></navPoint>"#,
                    i + 1
                )
            })
            .collect();
        let ncx = format!(
            r#"<?xml version="1.0" encoding="utf-8"?><ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1"><head/><docTitle><text>{}</text></docTitle><navMap>{nav_points}</navMap></ncx>"#,
            self.title
        );

        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        let stored =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        let mut put = |name: &str, bytes: &[u8]| {
            zip.start_file(name, stored).unwrap();
            zip.write_all(bytes).unwrap();
        };
        put("mimetype", b"application/epub+zip");
        put(
            "META-INF/container.xml",
            br#"<?xml version="1.0"?><container xmlns="urn:oasis:names:tc:opendocument:xmlns:container" version="1.0"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#,
        );
        put("OEBPS/content.opf", opf.as_bytes());
        if has_ncx {
            put("OEBPS/toc.ncx", ncx.as_bytes());
        }
        put("OEBPS/style.css", self.css.as_bytes());
        for (name, bytes) in &self.files {
            put(name, bytes);
        }
        zip.finish().unwrap().into_inner()
    }

    pub fn pack(self) -> ReaderPack {
        kob_ebook::reader_pack::build(&self.build()).expect("reader pack")
    }
}

/// A single-document book's only chapter.
pub fn chapter(body: &str) -> Chapter {
    one(Epub::new().doc("c1.xhtml", body))
}

pub fn one(epub: Epub) -> Chapter {
    let mut pack = epub.pack();
    assert_eq!(pack.chapters.len(), 1);
    pack.chapters.remove(0)
}

pub fn texts(blocks: &[Block]) -> Vec<String> {
    blocks.iter().map(Block::text).collect()
}

pub fn utf16_slice(s: &str, from: u32) -> String {
    String::from_utf16_lossy(&s.encode_utf16().skip(from as usize).collect::<Vec<_>>())
}
