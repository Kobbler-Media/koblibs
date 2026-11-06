//! CSS `float` and `clear` as a pack carries them. Only images float: a
//! floated box of text is laid out in the flow, as it was before floats.

use boko::ComputedStyle;
use boko::style::{Clear as CssClear, Float as CssFloat};

use super::model::{Clear, Float, Side};
use super::styles::{em_or_zero, round};

pub(crate) fn float(s: &ComputedStyle) -> Option<Float> {
    let size = s.font_size_abs.0;
    let (side, margin, padding) = match s.float {
        CssFloat::None => return None,
        CssFloat::Left => (Side::Left, s.margin_right, s.padding_right),
        CssFloat::Right => (Side::Right, s.margin_left, s.padding_left),
    };
    Some(Float {
        side,
        gap: round((em_or_zero(margin, size) + em_or_zero(padding, size)).max(0.0)),
    })
}

pub(crate) fn clear(s: &ComputedStyle) -> Option<Clear> {
    match s.clear {
        CssClear::None => None,
        CssClear::Left => Some(Clear::Left),
        CssClear::Right => Some(Clear::Right),
        CssClear::Both => Some(Clear::Both),
    }
}

pub(crate) fn merge(a: Option<Clear>, b: Option<Clear>) -> Option<Clear> {
    match (a, b) {
        (Some(x), Some(y)) if x != y => Some(Clear::Both),
        (a, b) => a.or(b),
    }
}
