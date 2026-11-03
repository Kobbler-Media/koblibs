//! The package document as the old client and foliate-js read it: spine
//! order, rendition hints, and each itemref's CFI step.

use std::collections::HashMap;

use quick_xml::events::Event;
use quick_xml::reader::Reader;

use super::model::{FixedLayout, SpreadSide};
use super::sidecar::push_step;
use super::source::attr;
use crate::Result;

#[derive(Debug, Default)]
pub(crate) struct Package {
    pub manifest: HashMap<String, String>,
    pub itemrefs: Vec<Itemref>,
    pub pre_paginated: bool,
    pub spreads: bool,
    pub rtl: bool,
}

#[derive(Debug, Default)]
pub(crate) struct Itemref {
    pub idref: String,
    pub properties: String,
    /// `CFI.fromElements` over the spine's itemrefs: the spine's step in the
    /// package, then the itemref's step in the spine.
    pub cfi: String,
}

pub(crate) fn parse(opf: &[u8]) -> Result<Package> {
    let mut reader = Reader::from_reader(opf);
    let mut buf = Vec::new();
    let mut pkg = Package {
        spreads: true,
        ..Package::default()
    };
    let mut depth = 0u32;
    let mut package_children = 0u32;
    let mut spine_step: Option<String> = None;
    let mut in_spine = false;
    let mut spine_children = 0u32;
    let mut in_metadata = false;
    let mut text_target: Option<TextTarget> = None;
    let mut text = String::new();
    let mut layout = None;
    let mut spread = None;

    loop {
        let event = reader.read_event_into(&mut buf)?;
        let (start, empty) = match &event {
            Event::Start(e) => (Some(e.clone()), false),
            Event::Empty(e) => (Some(e.clone()), true),
            _ => (None, false),
        };
        if let Some(e) = start {
            let local = e.local_name().as_ref().to_ascii_lowercase();
            if depth == 1 {
                package_children += 1;
                if local == "spine" && spine_step.is_none() {
                    let mut step = String::new();
                    push_step(&mut step, package_children * 2, attr(&e, "id").as_deref());
                    spine_step = Some(step);
                    in_spine = !empty;
                    pkg.rtl = attr(&e, "page-progression-direction").as_deref() == Some("rtl");
                }
                in_metadata = local == "metadata" && !empty;
            } else if depth == 2 && in_spine {
                spine_children += 1;
                if local == "itemref" {
                    let mut cfi = spine_step.clone().unwrap_or_default();
                    push_step(&mut cfi, spine_children * 2, attr(&e, "id").as_deref());
                    pkg.itemrefs.push(Itemref {
                        idref: attr(&e, "idref").unwrap_or_default(),
                        properties: attr(&e, "properties").unwrap_or_default(),
                        cfi,
                    });
                }
            }
            if local == "item"
                && let (Some(id), Some(href)) = (attr(&e, "id"), attr(&e, "href"))
                && !id.trim().is_empty()
                && !href.trim().is_empty()
            {
                pkg.manifest.insert(id, href);
            }
            if in_metadata && !empty {
                text_target = if local == "meta" {
                    match attr(&e, "property").as_deref() {
                        Some("rendition:layout") => Some(TextTarget::Layout),
                        Some("rendition:spread") => Some(TextTarget::Spread),
                        _ => None,
                    }
                } else {
                    None
                };
                text.clear();
            }
            if !empty {
                depth += 1;
            }
            buf.clear();
            continue;
        }
        match event {
            Event::Text(t) if text_target.is_some() => {
                text.push_str(t.as_ref());
            }
            Event::CData(c) if text_target.is_some() => {
                text.push_str(c.as_ref());
            }
            Event::End(_) => {
                depth = depth.saturating_sub(1);
                if let Some(target) = text_target.take() {
                    let value = text.trim().to_string();
                    match target {
                        TextTarget::Layout => layout = Some(value),
                        TextTarget::Spread => spread = Some(value),
                    }
                }
                if depth == 1 {
                    in_spine = false;
                    in_metadata = false;
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    pkg.pre_paginated = layout.as_deref() == Some("pre-paginated");
    pkg.spreads = spread.as_deref() != Some("none");
    Ok(pkg)
}

enum TextTarget {
    Layout,
    Spread,
}

impl Package {
    /// The old `EpubRendition.layoutFor`: itemref properties override the
    /// package-wide `rendition:*` metadata.
    pub fn fixed_layout(&self, properties: &str) -> Option<FixedLayout> {
        let props: Vec<&str> = properties.split_whitespace().collect();
        let has = |p: &str| props.contains(&p);
        let pre_paginated = if has("rendition:layout-pre-paginated") {
            true
        } else if has("rendition:layout-reflowable") {
            false
        } else {
            self.pre_paginated
        };
        if !pre_paginated {
            return None;
        }
        let spreads = if has("rendition:spread-none") {
            false
        } else if props.iter().any(|p| p.starts_with("rendition:spread-")) {
            true
        } else {
            self.spreads
        };
        let side = if has("page-spread-left") || has("rendition:page-spread-left") {
            SpreadSide::Left
        } else if has("page-spread-right") || has("rendition:page-spread-right") {
            SpreadSide::Right
        } else if has("rendition:page-spread-center") {
            SpreadSide::Center
        } else {
            SpreadSide::Auto
        };
        Some(FixedLayout { side, spreads })
    }
}
