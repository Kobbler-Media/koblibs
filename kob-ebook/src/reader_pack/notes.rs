//! Which links point at footnotes and endnotes, decided once for the whole
//! book so a footnote `<aside>` can leave the reading flow of whichever
//! chapter holds it.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use boko::model::{AnchorTarget, ChapterId, GlobalNodeId};
use boko::{Chapter, NodeId, Role};

#[derive(Debug, Default)]
pub(crate) struct NoteIndex {
    pub links: HashMap<GlobalNodeId, NoteRef>,
    pub removed: HashSet<GlobalNodeId>,
}

#[derive(Debug, Clone)]
pub(crate) struct NoteRef {
    pub block: GlobalNodeId,
    pub key: String,
}

pub(crate) fn is_inline_role(role: Role) -> bool {
    matches!(
        role,
        Role::Text | Role::Inline | Role::Link | Role::Break | Role::Image | Role::Math
    )
}

const NOTE_TYPES: [&str; 4] = ["noteref", "footnote", "endnote", "rearnote"];

fn has_type(epub_type: Option<&str>, wanted: &[&str]) -> bool {
    epub_type.is_some_and(|t| t.split_whitespace().any(|w| wanted.contains(&w)))
}

pub(crate) fn text_of(ch: &Chapter, node: NodeId, out: &mut String) {
    for child in ch.children(node) {
        if let Some(n) = ch.node(child) {
            if n.role == Role::Text {
                out.push_str(ch.text(n.text));
            } else {
                text_of(ch, child, out);
            }
        }
    }
}

/// A link text like `1`, `[12]`, `*`, `†` or `iv`.
fn is_note_marker(text: &str) -> bool {
    let t = text
        .trim()
        .trim_start_matches(['[', '('])
        .trim_end_matches([']', ')'])
        .trim();
    let n = t.chars().count();
    (1..=4).contains(&n)
        && t.chars().all(|c| {
            c.is_ascii_digit()
                || matches!(c, '*' | '†' | '‡' | 'i' | 'v' | 'x' | 'l' | 'c' | 'd' | 'm')
        })
}

fn block_ancestor(ch: &Chapter, mut node: NodeId) -> NodeId {
    while let Some(n) = ch.node(node) {
        if !is_inline_role(n.role) {
            return node;
        }
        match n.parent {
            Some(p) => node = p,
            None => return node,
        }
    }
    node
}

/// Whether `target` comes before any text of `block`: an inline anchor
/// opening its paragraph, the usual shape of a numbered endnote.
fn opens_block(ch: &Chapter, block: NodeId, target: NodeId) -> bool {
    first_of(ch, block, target) == Some(true)
}

fn first_of(ch: &Chapter, node: NodeId, target: NodeId) -> Option<bool> {
    if node == target {
        return Some(true);
    }
    let n = ch.node(node)?;
    if n.role == Role::Text {
        return (!ch.text(n.text).trim().is_empty()).then_some(false);
    }
    ch.children(node).find_map(|c| first_of(ch, c, target))
}

pub(crate) fn index(chapters: &[Arc<Chapter>], links: &boko::model::ResolvedLinks) -> NoteIndex {
    let mut out = NoteIndex::default();
    for (source, target) in links.iter() {
        let AnchorTarget::Internal(target) = target else {
            continue;
        };
        let (Some(sch), Some(tch)) = (
            chapters.get(source.chapter.0 as usize),
            chapters.get(target.chapter.0 as usize),
        ) else {
            continue;
        };
        let Some(tnode) = tch.node(target.node) else {
            continue;
        };
        let target_type = tch.semantics.epub_type(target.node);
        let typed = [sch.semantics.epub_type(source.node), target_type]
            .into_iter()
            .any(|t| has_type(t, &NOTE_TYPES));
        let aside = tnode.role == Role::Sidebar;
        let block_target = !is_inline_role(tnode.role);
        let block = block_ancestor(tch, target.node);
        let marker = || {
            let mut text = String::new();
            text_of(sch, source.node, &mut text);
            is_note_marker(&text) && (block_target || opens_block(tch, block, target.node))
        };
        if !(typed || aside || marker()) {
            continue;
        }
        let block = GlobalNodeId::new(target.chapter, block);
        let key = tch
            .semantics
            .id(target.node)
            .map(str::to_string)
            .unwrap_or_else(|| format!("n{}", target.node.0));
        if let Some(b) = tch.node(block.node)
            && b.role == Role::Sidebar
            && has_type(tch.semantics.epub_type(block.node), &["footnote"])
        {
            out.removed.insert(block);
        }
        out.links.insert(source, NoteRef { block, key });
    }
    out
}

pub(crate) fn global(chapter: u32, node: NodeId) -> GlobalNodeId {
    GlobalNodeId::new(ChapterId(chapter), node)
}
