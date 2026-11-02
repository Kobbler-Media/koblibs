//! Just enough CSS reading for the passes that look at a stylesheet without a
//! cascade: rules split naively on braces, bare compound selectors, and
//! `prop: value` declarations. Anything cleverer belongs to boko's engine.

pub(crate) fn strip_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        match rest[start + 2..].find("*/") {
            Some(end) => rest = &rest[start + 2 + end + 2..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// `(selector group, declaration block)` pairs of comment-free CSS.
pub(crate) fn rules(css: &str) -> impl Iterator<Item = (&str, &str)> {
    let mut rest = css;
    std::iter::from_fn(move || {
        let open = rest.find('{')?;
        let close = open + rest[open..].find('}')?;
        let selector = rest[..open].trim();
        let body = &rest[open + 1..close];
        rest = &rest[close + 1..];
        Some((selector, body))
    })
}

/// `(lowercased property, trimmed value)` pairs of a declaration block or a
/// `style` attribute.
pub(crate) fn declarations(body: &str) -> impl Iterator<Item = (String, &str)> {
    body.split(';').filter_map(|decl| {
        let (prop, value) = decl.split_once(':')?;
        Some((prop.trim().to_ascii_lowercase(), value.trim()))
    })
}

#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct SimpleSelector {
    pub tag: Option<String>,
    pub id: Option<String>,
    pub class: Option<String>,
}

impl SimpleSelector {
    pub fn specificity(&self) -> u32 {
        u32::from(self.tag.is_some())
            + 10 * u32::from(self.class.is_some())
            + 100 * u32::from(self.id.is_some())
    }

    pub fn matches(&self, tag: &str, id: Option<&str>, classes: &str) -> bool {
        self.tag
            .as_deref()
            .is_none_or(|t| t.eq_ignore_ascii_case(tag))
            && self.id.as_deref().is_none_or(|want| id == Some(want))
            && self
                .class
                .as_deref()
                .is_none_or(|want| classes.split_whitespace().any(|c| c == want))
    }
}

/// A bare compound selector — `tag`, `#id`, `.class`, `tag.class`, `tag#id` —
/// or `None` for combinators, pseudo-classes, attribute selectors and
/// compounds with more than one id or class.
pub(crate) fn parse_simple_selector(sel: &str) -> Option<SimpleSelector> {
    let mut rest = sel;
    let mut out = SimpleSelector::default();
    if !rest.is_empty() && !(rest.starts_with('#') || rest.starts_with('.')) {
        let end = rest.find(['#', '.']).unwrap_or(rest.len());
        let tag = &rest[..end];
        if !(tag == "*" || tag.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')) {
            return None;
        }
        if tag != "*" {
            out.tag = Some(tag.to_ascii_lowercase());
        }
        rest = &rest[end..];
    }
    while !rest.is_empty() {
        let head = rest.as_bytes()[0];
        let body = &rest[1..];
        let ident = body.split(['#', '.']).next().unwrap_or("");
        let valid = !ident.is_empty()
            && ident
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !valid {
            return None;
        }
        let slot = match head {
            b'#' => &mut out.id,
            b'.' => &mut out.class,
            _ => return None,
        };
        if slot.is_some() {
            return None;
        }
        *slot = Some(ident.to_string());
        rest = &body[ident.len()..];
    }
    (out != SimpleSelector::default()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_compounds_only() {
        type Parts<'a> = (Option<&'a str>, Option<&'a str>, Option<&'a str>);
        let cases: &[(&str, Option<Parts>)] = &[
            ("div", Some((Some("div"), None, None))),
            (".a", Some((None, None, Some("a")))),
            ("p.a", Some((Some("p"), None, Some("a")))),
            ("#x", Some((None, Some("x"), None))),
            ("*.a", Some((None, None, Some("a")))),
            ("div p", None),
            ("a:hover", None),
            (".a.b", None),
            ("", None),
        ];
        for (sel, want) in cases {
            let got =
                parse_simple_selector(sel).map(|s| (s.tag.clone(), s.id.clone(), s.class.clone()));
            let want = want.map(|(t, i, c)| {
                (
                    t.map(String::from),
                    i.map(String::from),
                    c.map(String::from),
                )
            });
            assert_eq!(got, want, "{sel}");
        }
    }

    #[test]
    fn rules_and_declarations_split() {
        let css = strip_comments("/* x */ a { color: red; } .b{top:1px;LEFT : 2px}");
        let got: Vec<_> = rules(&css)
            .map(|(s, b)| {
                (
                    s.to_string(),
                    declarations(b)
                        .map(|(p, v)| format!("{p}={v}"))
                        .collect::<Vec<_>>(),
                )
            })
            .collect();
        assert_eq!(
            got,
            vec![
                ("a".into(), vec!["color=red".to_string()]),
                (".b".into(), vec!["top=1px".into(), "left=2px".into()]),
            ]
        );
    }
}
