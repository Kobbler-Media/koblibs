//! What a block's element draws around itself: CSS borders and backgrounds
//! become [`Container`]s, an `hr`'s own border its [`RuleStyle`].

use boko::ComputedStyle;
use boko::style::{BackgroundRepeat, BorderStyle, Color, Length};

use super::model::{Borders, Container, Line, LineStyle, Measure, RuleImage, RuleStyle};
use super::styles::{BASE_PX, em, em_or_zero, hex, round};

const MEDIUM_PX: f32 = 3.0;
const HAIRLINE_PX: f32 = 1.0;

pub(crate) fn borders(s: &ComputedStyle) -> Option<Borders> {
    let size = s.font_size_abs.0;
    let b = Borders {
        top: line(
            s.border_style_top,
            s.border_width_top,
            s.border_color_top,
            size,
        ),
        right: line(
            s.border_style_right,
            s.border_width_right,
            s.border_color_right,
            size,
        ),
        bottom: line(
            s.border_style_bottom,
            s.border_width_bottom,
            s.border_color_bottom,
            size,
        ),
        left: line(
            s.border_style_left,
            s.border_width_left,
            s.border_color_left,
            size,
        ),
    };
    (b.top.is_some() || b.right.is_some() || b.bottom.is_some() || b.left.is_some()).then_some(b)
}

pub(crate) fn container(s: &ComputedStyle) -> Option<Container> {
    let background = background(s);
    let sides = borders(s);
    if sides.is_none() && background.is_none() {
        return None;
    }
    let Borders {
        top,
        right,
        bottom,
        left,
    } = sides.unwrap_or_default();
    Some(Container {
        top,
        right,
        bottom,
        left,
        background,
        radius: radius(s),
        ..Container::default()
    })
}

pub(crate) fn background(s: &ComputedStyle) -> Option<String> {
    s.background_color.filter(|c| c.a != 0).map(hex)
}

pub(crate) fn width(line: &Option<Line>) -> f32 {
    line.as_ref().map_or(0.0, |l| l.width)
}

fn radius(s: &ComputedStyle) -> Option<[f32; 4]> {
    let size = s.font_size_abs.0;
    let corners = [
        s.border_radius_top_left,
        s.border_radius_top_right,
        s.border_radius_bottom_right,
        s.border_radius_bottom_left,
    ]
    .map(|r| match r {
        Length::Percent(_) | Length::Auto => 0.0,
        r => em_or_zero(r, size),
    });
    corners.iter().any(|r| *r > 0.0).then_some(corners)
}

/// A box centred by `margin: auto` keeps the width the book gave it.
pub(crate) fn centred(s: &ComputedStyle) -> Option<Measure> {
    if s.margin_left != Length::Auto || s.margin_right != Length::Auto {
        return None;
    }
    match s.width {
        Length::Auto => None,
        Length::Percent(w) => (w > 0.0 && w < 100.0).then(|| Measure::Fraction(round(w / 100.0))),
        w => em(w, s.font_size_abs.0)
            .filter(|w| *w > 0.0)
            .map(Measure::Em),
    }
}

pub(crate) enum RuleLook {
    /// Left at the browser's default: the reader draws its own.
    Default,
    Hidden,
    Drawn(RuleStyle),
}

pub(crate) fn rule(s: &ComputedStyle) -> RuleLook {
    let size = s.font_size_abs.0;
    let untouched = [
        (s.border_style_top, s.border_width_top, s.border_color_top),
        (
            s.border_style_right,
            s.border_width_right,
            s.border_color_right,
        ),
        (
            s.border_style_bottom,
            s.border_width_bottom,
            s.border_color_bottom,
        ),
        (
            s.border_style_left,
            s.border_width_left,
            s.border_color_left,
        ),
    ]
    .iter()
    .all(|&(style, width, color)| {
        style == BorderStyle::Inset && width == Length::Px(HAIRLINE_PX) && color.is_none()
    });
    let image = s
        .background_image
        .as_ref()
        .filter(|src| !src.starts_with("data:"))
        .map(|src| RuleImage {
            src: src.clone(),
            width: None,
            height: None,
            repeat: s.background_repeat != BackgroundRepeat::NoRepeat,
        });
    if untouched && background(s).is_none() && image.is_none() {
        return RuleLook::Default;
    }
    let border = line(
        s.border_style_top,
        s.border_width_top,
        s.border_color_top,
        size,
    )
    .or_else(|| {
        line(
            s.border_style_bottom,
            s.border_width_bottom,
            s.border_color_bottom,
            size,
        )
    });
    let filled = || {
        let color = s.background_color.filter(|c| c.a != 0)?;
        Some(Line {
            width: em(s.height, size)
                .filter(|w| *w > 0.0)
                .unwrap_or(HAIRLINE_PX / BASE_PX),
            style: LineStyle::Solid,
            color: Some(hex(color)),
        })
    };
    let line = border.or_else(filled);
    if line.is_none() && image.is_none() {
        return RuleLook::Hidden;
    }
    RuleLook::Drawn(RuleStyle {
        height: image
            .as_ref()
            .and_then(|_| em(s.height, size))
            .filter(|h| *h > 0.0),
        line,
        length: rule_length(s),
        image,
    })
}

/// A rule set off by percentage margins is a fraction of the column wide,
/// which a fixed inset could only approximate.
pub(crate) fn percent_margin(len: Length) -> bool {
    matches!(len, Length::Percent(_))
}

fn rule_length(s: &ComputedStyle) -> Option<f32> {
    let pct = |l: Length| match l {
        Length::Percent(v) => v,
        _ => 0.0,
    };
    let fraction = match s.width {
        Length::Percent(w) => w / 100.0,
        _ if percent_margin(s.margin_left) || percent_margin(s.margin_right) => {
            1.0 - (pct(s.margin_left) + pct(s.margin_right)) / 100.0
        }
        _ => return None,
    };
    Some(round(fraction)).filter(|f| *f > 0.0 && *f < 1.0)
}

pub(crate) fn column_width(s: &ComputedStyle) -> Option<f32> {
    match s.width {
        Length::Percent(w) if w > 0.0 => Some(round((w / 100.0).min(1.0))),
        _ => None,
    }
}

/// Whether a blockquote's box is the user agent's, untouched by the book.
pub(crate) fn plain_quote(s: &ComputedStyle) -> bool {
    let size = s.font_size_abs.0;
    let is = |len: Length, ua_em: f32| (em_or_zero(len, size) - ua_em * size).abs() < 0.01;
    is(s.margin_top, 1.0)
        && is(s.margin_bottom, 1.0)
        && is(s.margin_left, 2.5)
        && is(s.margin_right, 2.5)
        && [
            s.padding_top,
            s.padding_right,
            s.padding_bottom,
            s.padding_left,
        ]
        .iter()
        .all(|p| is(*p, 0.0))
        && container(s).is_none()
}

fn line(style: BorderStyle, width: Length, color: Option<Color>, size: f32) -> Option<Line> {
    let style = match style {
        BorderStyle::None => return None,
        BorderStyle::Dashed => LineStyle::Dashed,
        BorderStyle::Dotted => LineStyle::Dotted,
        BorderStyle::Double => LineStyle::Double,
        BorderStyle::Solid
        | BorderStyle::Groove
        | BorderStyle::Ridge
        | BorderStyle::Inset
        | BorderStyle::Outset => LineStyle::Solid,
    };
    let width = match width {
        Length::Auto => round(MEDIUM_PX / BASE_PX),
        Length::Percent(_) => return None,
        w => em_or_zero(w, size),
    };
    if width <= 0.0 || color.is_some_and(|c| c.a == 0) {
        return None;
    }
    Some(Line {
        width,
        style,
        color: color.map(hex),
    })
}
