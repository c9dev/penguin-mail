//! Cleans email HTML for display. Layout survives: tables, inline styles,
//! `<style>` blocks, and images. Anything that runs code, submits data, or
//! navigates by itself is removed. Remote loads are blocked separately, by
//! the WebView's content filter, so this module reads CSS only to find
//! rules and selectors, never to judge a value.
//!
//! The conversation page puts each cleaned body in its own shadow root, so
//! an email's `<style>` cannot restyle the page around it.
//!
//! Mail is shown on a white card, so an email's dark mode rules would put
//! its pale text on white. [`restyle`] takes those rules out and leaves the
//! light ones, which is what the sender designed for.
//!
//! The shadow root has no `<html>` or `<body>` of the message's own, so the
//! cleaner puts two elements in their place, as Gmail does: the styles on
//! those tags move onto them, and the message's `html` and `body` selectors
//! point at them. GitHub, for one, sets its font and line height on
//! `<body>`.

use std::borrow::Cow;
use std::collections::HashSet;

use ammonia::{Builder, UrlRelative};

const EXTRA_TAGS: [&str; 5] = ["style", "font", "center", "span", "div"];

const LAYOUT_ATTRIBUTES: [&str; 17] = [
    "style",
    "class",
    "align",
    "valign",
    "width",
    "height",
    "bgcolor",
    "color",
    "border",
    "dir",
    "face",
    "size",
    "cellpadding",
    "cellspacing",
    "colspan",
    "rowspan",
    "nowrap",
];

/// Sanitizes `html`. A `cid:` image source becomes `pictures` followed by
/// the escaped `Content-ID`, the address the conversation view serves that
/// picture at; with no `pictures` it loses its source.
pub fn sanitize_html(html: &str, pictures: Option<&str>) -> String {
    let root = Root::read(html, pictures);
    let pictures = pictures.map(str::to_string);
    let mut builder = Builder::default();
    builder
        .add_tags(&EXTRA_TAGS)
        .rm_clean_content_tags(&["style"])
        // A title names the page in a browser tab; its words are not part
        // of the message.
        .add_clean_content_tags(&["title"])
        .add_generic_attributes(&LAYOUT_ATTRIBUTES)
        // The marks other clients put on quoted history, which the page
        // folds away (see `crate::quoted`). Each body sits in its own
        // shadow root, so the ids cannot clash with the page's.
        .add_tag_attribute_values("blockquote", "type", &["cite"])
        .add_tag_attribute_values("div", "id", &["appendonsend", "divRplyFwdMsg"])
        .url_schemes(HashSet::from(["http", "https", "mailto", "cid", "data"]))
        .url_relative(UrlRelative::Deny)
        .link_rel(Some("noopener noreferrer"))
        .strip_comments(true)
        .attribute_filter(move |element, attribute, value| {
            filter_url(pictures.as_deref(), element, attribute, value)
        });
    let (styled, rewrote) = restyle(&builder.clean(html).to_string());
    name_images(&root.wrap(&styled, rewrote))
}

/// The class of the element that stands in for a message's `<html>`.
const HTML_CLASS: &str = "mailrs-html";
/// The class of the element that stands in for a message's `<body>`.
const BODY_CLASS: &str = "mailrs-body";

/// What a message's `<html>` and `<body>` tags ask of the page: their
/// styles, and the color `link` gives to links. Values go into a `style`
/// attribute this module writes and escapes, so they reach no further
/// than the styles the cleaner already keeps on any other element.
#[derive(Debug, Default)]
struct Root {
    html: String,
    body: String,
    link: Option<String>,
}

impl Root {
    fn read(source: &str, pictures: Option<&str>) -> Root {
        let mut root = Root::default();
        if let Some(attributes) = tag_attributes(source, "html") {
            root.html = value(&attributes, "style").trim().to_string();
        }
        let Some(attributes) = tag_attributes(source, "body") else {
            return root;
        };
        // The old attributes are hints a browser lays under the author's
        // CSS, so they come first and the `style` attribute overrides them.
        let mut body = Vec::new();
        if let Some(color) = color(value(&attributes, "bgcolor")) {
            body.push(format!("background-color:{color}"));
        }
        if let Some(color) = color(value(&attributes, "text")) {
            body.push(format!("color:{color}"));
        }
        if let Some(address) = background(pictures, value(&attributes, "background")) {
            body.push(format!("background-image:url('{address}')"));
        }
        let style = value(&attributes, "style").trim();
        if !style.is_empty() {
            body.push(style.to_string());
        }
        root.body = body.join(";");
        root.link = color(value(&attributes, "link"));
        root
    }

    /// `html` inside the two stand-ins, or as it was when the message
    /// gave them nothing to carry and no rule points at them.
    fn wrap(&self, html: &str, rewrote: bool) -> String {
        if self.html.is_empty() && self.body.is_empty() && self.link.is_none() && !rewrote {
            return html.to_string();
        }
        let mut out = String::with_capacity(html.len() + self.html.len() + self.body.len() + 120);
        // Zero weight and first in the body, so the sender's own rules
        // for links win over it, as they would over the attribute.
        if let Some(link) = &self.link {
            out.push_str(&format!(
                "<style>:where(.{BODY_CLASS}) a{{color:{link}}}</style>"
            ));
        }
        for (class, style) in [(HTML_CLASS, &self.html), (BODY_CLASS, &self.body)] {
            out.push_str(&format!("<div class=\"{class}\""));
            if !style.is_empty() {
                out.push_str(&format!(" style=\"{}\"", escape_attribute(style)));
            }
            out.push('>');
        }
        out.push_str(html);
        out.push_str("</div></div>");
        out
    }
}

/// The value of attribute `name`, or nothing.
fn value<'a>(attributes: &'a [(String, String)], name: &str) -> &'a str {
    attributes
        .iter()
        .find(|(n, _)| n == name)
        .map_or("", |(_, v)| v.as_str())
}

/// A color an old attribute names, when it is a plain name or a hex
/// value. Anything else, such as a value that tries to add declarations
/// of its own, is dropped. Three or six hex digits without `#` get one,
/// as a browser reads them.
fn color(value: &str) -> Option<String> {
    let value = value.trim();
    let bare = value.strip_prefix('#').unwrap_or(value);
    if bare.is_empty() || bare.len() > 20 || !bare.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    let hex = matches!(bare.len(), 3 | 6) && bare.chars().all(|c| c.is_ascii_hexdigit());
    Some(if value.starts_with('#') || hex {
        format!("#{bare}")
    } else {
        bare.to_string()
    })
}

/// A body's `background` picture, when its address is one the cleaner
/// lets a picture have, quoted for a CSS string.
fn background(pictures: Option<&str>, value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let address = filter_url(pictures, "img", "src", value)?;
    let lower = address.to_ascii_lowercase();
    let allowed = ["http://", "https://", "data:image/"]
        .iter()
        .any(|scheme| lower.starts_with(scheme))
        || pictures.is_some_and(|p| lower.starts_with(&p.to_ascii_lowercase()));
    allowed.then(|| {
        address
            .chars()
            .filter(|c| !c.is_control())
            .flat_map(|c| match c {
                '\\' | '\'' => vec!['\\', c],
                c => vec![c],
            })
            .collect()
    })
}

/// The attributes of the first `<name>` tag in `source`, names in lower
/// case and values with their character references decoded, or nothing
/// when there is no such tag. The first of two attributes with one name
/// counts, as in a browser.
fn tag_attributes(source: &str, name: &str) -> Option<Vec<(String, String)>> {
    // Compared in place rather than on a lower-case copy, since a long
    // newsletter would cost a copy of itself for each tag looked up.
    let bytes = source.as_bytes();
    let start = source.match_indices('<').find_map(|(at, _)| {
        let after = at + 1 + name.len();
        let word = bytes.get(at + 1..after)?;
        let ends = bytes
            .get(after)
            .is_some_and(|b| b.is_ascii_whitespace() || matches!(b, b'>' | b'/'));
        (ends && word.eq_ignore_ascii_case(name.as_bytes())).then_some(after)
    })?;
    let tag = &source[start..];
    let bytes = tag.as_bytes();
    let mut attributes: Vec<(String, String)> = Vec::new();
    let mut at = 0;
    loop {
        while bytes
            .get(at)
            .is_some_and(|b| b.is_ascii_whitespace() || *b == b'/')
        {
            at += 1;
        }
        if bytes.get(at).is_none_or(|b| *b == b'>') {
            break;
        }
        let name_start = at;
        while bytes
            .get(at)
            .is_some_and(|b| !b.is_ascii_whitespace() && !matches!(b, b'=' | b'>' | b'/'))
        {
            at += 1;
        }
        if at == name_start {
            // A stray `=`.
            at += 1;
            continue;
        }
        let name = tag[name_start..at].to_ascii_lowercase();
        while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        }
        let mut value = String::new();
        if bytes.get(at) == Some(&b'=') {
            at += 1;
            while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
                at += 1;
            }
            let end = match bytes.get(at) {
                Some(&quote) if quote == b'"' || quote == b'\'' => {
                    at += 1;
                    let end = tag[at..].find(quote as char).map_or(tag.len(), |i| at + i);
                    value = decode(&tag[at..end]);
                    (end + 1).min(tag.len())
                }
                _ => {
                    let end = tag[at..]
                        .find(|c: char| c.is_ascii_whitespace() || c == '>')
                        .map_or(tag.len(), |i| at + i);
                    value = decode(&tag[at..end]);
                    end
                }
            };
            at = end;
        }
        if !attributes.iter().any(|(n, _)| *n == name) {
            attributes.push((name, value));
        }
    }
    Some(attributes)
}

/// `value` with its character references decoded: the named ones mail
/// puts in attributes, and numeric ones. Others stay as written.
fn decode(value: &str) -> String {
    if !value.contains('&') {
        return value.to_string();
    }
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let decoded = rest[1..]
            .find(';')
            .filter(|end| *end <= 10)
            .and_then(|end| {
                let name = &rest[1..=end];
                let c = match name {
                    "quot" => Some('"'),
                    "amp" => Some('&'),
                    "apos" => Some('\''),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "nbsp" => Some('\u{a0}'),
                    _ => name.strip_prefix('#').and_then(|number| {
                        match number.strip_prefix(['x', 'X']) {
                            Some(hex) => u32::from_str_radix(hex, 16).ok(),
                            None => number.parse().ok(),
                        }
                        .and_then(char::from_u32)
                    }),
                }?;
                Some((c, end + 2))
            });
        match decoded {
            Some((c, used)) => {
                out.push(c);
                rest = &rest[used..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// `value` escaped for a double-quoted attribute.
fn escape_attribute(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    out
}

/// Gives every picture without a description an empty one.
///
/// A browser with nothing else to go on reads an image's source out, and
/// an inline picture's source is a kilobyte of base64. A sender who wrote
/// no description said nothing about the picture, so the page says
/// nothing rather than spelling the source out.
fn name_images(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(start) = rest.find("<img") {
        let Some(close) = rest[start..].find('>').map(|i| start + i) else {
            break;
        };
        let tag = &rest[start..close];
        out.push_str(&rest[..close]);
        if !has_alt(tag) {
            out.push_str(" alt=\"\"");
        }
        rest = &rest[close..];
    }
    out.push_str(rest);
    out
}

/// Whether an `<img>` tag carries an `alt` attribute of its own, rather
/// than an attribute whose name merely ends in those letters.
fn has_alt(tag: &str) -> bool {
    let lower = tag.to_ascii_lowercase();
    lower.match_indices("alt").any(|(at, _)| {
        let before = lower[..at].chars().next_back();
        before.is_some_and(char::is_whitespace) && lower[at + 3..].trim_start().starts_with('=')
    })
}

/// Rewrites a message's `<style>` blocks: takes out the `@media` blocks
/// that only apply in dark mode and the `color-scheme` declarations that
/// ask for one, and points `html` and `body` selectors at the elements
/// that stand in for those tags. Everything else stays as it was. Also
/// says whether any selector changed.
fn restyle(html: &str) -> (String, bool) {
    if !html.contains("<style") {
        return (html.to_string(), false);
    }
    // `prefers-color-scheme` contains this too.
    let dark = html.contains("color-scheme");
    let mut rewrote = false;
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(start) = rest.find("<style") {
        let Some(open) = rest[start..].find('>').map(|i| start + i + 1) else {
            break;
        };
        let end = rest[open..]
            .find("</style>")
            .map(|i| open + i)
            .unwrap_or(rest.len());
        out.push_str(&rest[..open]);
        let css = &rest[open..end];
        let css = if dark {
            Cow::Owned(clean_css(css))
        } else {
            Cow::Borrowed(css)
        };
        let (css, hit) = point_at_stand_ins(&css);
        rewrote |= hit;
        out.push_str(&css);
        rest = &rest[end..];
    }
    out.push_str(rest);
    (out, rewrote)
}

/// A stylesheet with its `html` and `body` selectors pointed at the
/// stand-ins, and whether any changed. It reads selectors at the top and
/// inside grouping rules such as `@media`, and copies declaration blocks
/// and other at-rules, such as `@font-face`, as they are.
fn point_at_stand_ins(css: &str) -> (String, bool) {
    let mut out = String::with_capacity(css.len() + 32);
    let mut changed = false;
    let mut rest = css;
    loop {
        let Some(stop) = rest.find(['{', '}', ';']) else {
            out.push_str(rest);
            break;
        };
        if rest.as_bytes()[stop] != b'{' {
            out.push_str(&rest[..=stop]);
            rest = &rest[stop + 1..];
            continue;
        }
        let prelude = &rest[..stop];
        let head = prelude.trim_start().to_ascii_lowercase();
        if head.starts_with('@') && holds_rules(&head) {
            out.push_str(&rest[..=stop]);
            rest = &rest[stop + 1..];
            continue;
        }
        if head.starts_with('@') {
            out.push_str(prelude);
        } else {
            let (selector, hit) = point_selector(prelude);
            changed |= hit;
            out.push_str(&selector);
        }
        let block = &rest[stop..];
        let after = skip_block(block).unwrap_or_default();
        out.push_str(&block[..block.len() - after.len()]);
        rest = after;
    }
    (out, changed)
}

/// Whether an at-rule's block holds rules with selectors, rather than
/// declarations or keyframes.
fn holds_rules(head: &str) -> bool {
    [
        "@media",
        "@supports",
        "@document",
        "@-moz-document",
        "@layer",
        "@container",
        "@scope",
    ]
    .iter()
    .any(|name| {
        head.strip_prefix(name).is_some_and(|after| {
            after.is_empty() || !after.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '-')
        })
    })
}

/// A selector list with each `html` and `body` type selector replaced by
/// its stand-in's class. A word that is part of a class, an id, a longer
/// name such as `tbody`, or an attribute selector stays.
fn point_selector(selector: &str) -> (String, bool) {
    let bytes = selector.as_bytes();
    let mut out = String::with_capacity(selector.len() + 16);
    let mut copied = 0;
    let mut changed = false;
    let mut in_brackets = false;
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'[' => in_brackets = true,
            b']' => in_brackets = false,
            _ => {}
        }
        let starts_word = bytes[at].is_ascii_alphabetic()
            && (at == 0
                || bytes[at - 1].is_ascii_whitespace()
                || matches!(bytes[at - 1], b',' | b'>' | b'+' | b'~' | b'('));
        if in_brackets || !starts_word {
            at += 1;
            continue;
        }
        let end = bytes[at..]
            .iter()
            .position(|b| !(b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')))
            .map_or(bytes.len(), |i| at + i);
        let word = &selector[at..end];
        let class = if word.eq_ignore_ascii_case("body") {
            Some(BODY_CLASS)
        } else if word.eq_ignore_ascii_case("html") {
            Some(HTML_CLASS)
        } else {
            None
        };
        if let Some(class) = class {
            out.push_str(&selector[copied..at]);
            out.push('.');
            out.push_str(class);
            copied = end;
            changed = true;
        }
        at = end;
    }
    out.push_str(&selector[copied..]);
    (out, changed)
}

/// One stylesheet without its dark mode rules.
fn clean_css(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(at) = rest.find('@') {
        let head_end = rest[at..]
            .find('{')
            .map(|i| at + i)
            .unwrap_or_else(|| rest.len());
        let head = &rest[at..head_end];
        if !is_dark_query(head) {
            out.push_str(&rest[..head_end.min(rest.len())]);
            rest = &rest[head_end.min(rest.len())..];
            // Step past the brace so the next search does not find this
            // rule again.
            if let Some(brace) = rest.strip_prefix('{') {
                out.push('{');
                rest = brace;
            }
            continue;
        }
        out.push_str(&rest[..at]);
        rest = skip_block(&rest[head_end.min(rest.len())..]).unwrap_or_default();
    }
    out.push_str(rest);
    strip_color_scheme(&out)
}

/// Whether an at-rule's head asks for dark mode alone. A query that also
/// covers light mode, such as `(prefers-color-scheme: no-preference)`,
/// stays.
fn is_dark_query(head: &str) -> bool {
    let head = head.to_ascii_lowercase();
    head.starts_with("@media")
        && head.contains("prefers-color-scheme")
        && head.contains("dark")
        && !head.contains("light")
}

/// The text after a balanced `{ ... }` block, or `None` when the braces
/// never close.
fn skip_block(css: &str) -> Option<&str> {
    let mut depth = 0usize;
    for (index, ch) in css.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&css[index + 1..]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Drops `color-scheme` declarations, which would otherwise flip the colors
/// a browser picks for form controls and scrollbars. The `prefers-color-scheme`
/// inside a media query is a different thing and stays.
fn strip_color_scheme(css: &str) -> String {
    let lower = css.to_ascii_lowercase();
    let mut out = String::with_capacity(css.len());
    let mut from = 0usize;
    while let Some(found) = lower[from..].find("color-scheme") {
        let at = from + found;
        let is_query = lower[..at].ends_with("prefers-");
        let after = at + "color-scheme".len();
        let declares = lower[after..].trim_start().starts_with(':');
        if is_query || !declares {
            out.push_str(&css[from..after]);
            from = after;
            continue;
        }
        // A declaration runs to its semicolon, or to the end of its block.
        let end = lower[after..]
            .find([';', '}'])
            .map(|i| after + i)
            .unwrap_or(css.len());
        out.push_str(&css[from..at]);
        from = match lower.as_bytes().get(end) {
            Some(b';') => end + 1,
            _ => end,
        };
    }
    out.push_str(&css[from..]);
    out
}

fn filter_url<'u>(
    pictures: Option<&str>,
    element: &str,
    attribute: &str,
    value: &'u str,
) -> Option<Cow<'u, str>> {
    if !matches!(attribute, "src" | "href") {
        return Some(Cow::Borrowed(value));
    }
    let lower = value.trim_start().to_ascii_lowercase();
    if let Some(cid) = lower.strip_prefix("cid:") {
        let key = &value.trim_start()[value.trim_start().len() - cid.len()..];
        let pictures = pictures.filter(|_| element == "img" && !key.is_empty())?;
        let address = format!("{pictures}{}", crate::open_thread::inline::escape(key));
        return Some(Cow::Owned(address));
    }
    if lower.starts_with("data:") {
        return (element == "img" && lower.starts_with("data:image/"))
            .then_some(Cow::Borrowed(value));
    }
    Some(Cow::Borrowed(value))
}

#[cfg(test)]
mod dark_tests {
    use super::sanitize_html;

    fn clean(html: &str) -> String {
        sanitize_html(html, None)
    }

    #[test]
    fn a_dark_mode_block_goes_and_the_light_rules_stay() {
        let html = "<style>.a{color:#222}@media (prefers-color-scheme: dark){.a{color:#eee}}\
                    .b{color:#444}</style><p class=\"a\">When</p>";
        let out = clean(html);
        assert!(out.contains(".a{color:#222}"), "{out}");
        assert!(out.contains(".b{color:#444}"), "{out}");
        assert!(!out.contains("#eee"), "{out}");
        assert!(!out.contains("prefers-color-scheme"), "{out}");
    }

    #[test]
    fn nested_braces_inside_a_dark_block_go_with_it() {
        let html = "<style>@media (prefers-color-scheme:dark){@supports (color:red){.a{color:#eee}}}\
                    .keep{color:#111}</style>";
        let out = clean(html);
        assert!(out.contains(".keep{color:#111}"), "{out}");
        assert!(!out.contains("#eee"), "{out}");
    }

    #[test]
    fn a_query_naming_both_schemes_stays() {
        let html = "<style>@media (prefers-color-scheme: dark),(prefers-color-scheme: light){.a{color:#777}}</style>";
        let out = clean(html);
        assert!(out.contains("#777"), "{out}");
    }

    #[test]
    fn other_at_rules_survive() {
        let html =
            "<style>@media (max-width:600px){.a{width:100%}}@font-face{font-family:x}</style>";
        let out = clean(html);
        assert!(out.contains("max-width:600px"), "{out}");
        assert!(out.contains("@font-face"), "{out}");
    }

    #[test]
    fn a_color_scheme_declaration_goes() {
        let html = "<style>:root{color-scheme:light dark;margin:0}</style>";
        let out = clean(html);
        assert!(!out.contains("color-scheme"), "{out}");
        assert!(out.contains("margin:0"), "{out}");
    }

    #[test]
    fn unclosed_braces_lose_nothing_after_them() {
        let html =
            "<style>@media (prefers-color-scheme:dark){.a{color:#eee}</style><p>Body text</p>";
        let out = clean(html);
        assert!(out.contains("Body text"), "{out}");
    }

    /// The shape Google Calendar sends: light colors for the page, dark
    /// ones behind a media query. The dark set would be pale text on the
    /// white card mail is shown on.
    #[test]
    fn a_calendar_invite_keeps_its_light_colors() {
        let html = "<style>.label{color:#616161}.value{color:#222}\
            @media (prefers-color-scheme: dark){.label{color:#9aa0a6 !important}\
            .value{color:#e8eaed !important}.card{background:#1f1f1f !important}}\
            .card{border:1px solid #dadce0;background:#fff}</style>\
            <div class=\"card\"><div class=\"label\">When</div>\
            <div class=\"value\">Friday 18 Sept 2026</div></div>";
        let out = clean(html);
        assert!(out.contains("color:#222"), "{out}");
        assert!(out.contains("color:#616161"), "{out}");
        assert!(out.contains("Friday 18 Sept 2026"), "{out}");
        assert!(!out.contains("#e8eaed"), "{out}");
        assert!(!out.contains("#1f1f1f"), "{out}");
    }

    #[test]
    fn mail_without_dark_rules_passes_through() {
        let html = "<style>.a{color:#222}</style><p>Hello</p>";
        let out = clean(html);
        assert!(
            out.contains(".a{color:#222}") && out.contains("Hello"),
            "{out}"
        );
    }
}

#[cfg(test)]
mod tests {
    /// A newsletter's `<title>` names the message in a browser tab. Its
    /// words once showed as a stray line above the message, because the
    /// cleaner dropped the tag and kept its text.
    #[test]
    fn a_title_s_words_stay_out_of_the_message() {
        let html = "<html><head><title>[c9dev/penguin-mail] Run failed</title></head>\
                    <body><p>Body</p></body></html>";
        let clean = sanitize_html(html, None);
        assert!(!clean.contains("Run failed"), "{clean}");
        assert!(clean.contains("<p>Body</p>"), "{clean}");
    }

    use super::sanitize_html;

    fn clean(html: &str) -> String {
        sanitize_html(html, None).to_lowercase()
    }

    #[test]
    fn scripts_and_event_handlers_are_removed() {
        let out = clean(
            r#"<script>alert(1)</script><p>hi</p><img src="https://x/a.png" onerror="alert(1)"><SCRIPT>x</SCRIPT>"#,
        );
        assert!(
            !out.contains("script") && !out.contains("alert") && !out.contains("onerror"),
            "{out}"
        );
        assert!(out.contains("<p>hi</p>"));
    }

    #[test]
    fn javascript_and_relative_links_lose_their_target() {
        let out = clean(
            r#"<a href="javascript:alert(1)">a</a><a HREF="JaVaScRiPt:x">b</a><a href="/local">c</a>"#,
        );
        assert!(
            !out.contains("javascript") && !out.contains("/local"),
            "{out}"
        );
    }

    #[test]
    fn frames_objects_and_forms_are_removed() {
        let out = clean(
            r#"<iframe src="https://evil"></iframe><object data="x"></object><embed src="y">
               <form action="https://evil"><input name="password"><button>go</button></form>"#,
        );
        for banned in [
            "iframe", "object", "embed", "form", "input", "button", "evil",
        ] {
            assert!(!out.contains(banned), "{banned} survived: {out}");
        }
    }

    #[test]
    fn documents_cannot_redirect_or_rebase() {
        let out = clean(
            r#"<meta http-equiv="refresh" content="0;url=https://evil"><base href="https://evil/"><link rel="stylesheet" href="https://evil/x.css">"#,
        );
        assert!(
            !out.contains("meta")
                && !out.contains("base")
                && !out.contains("link")
                && !out.contains("evil"),
            "{out}"
        );
    }

    #[test]
    fn svg_and_math_payloads_are_removed() {
        let out = clean(
            r#"<svg><script>alert(1)</script></svg><math><mtext><script>x</script></mtext></math>"#,
        );
        assert!(
            !out.contains("svg") && !out.contains("script") && !out.contains("math"),
            "{out}"
        );
    }

    #[test]
    fn layout_survives() {
        let out = clean(
            r##"<style>p { color: red }</style><table width="600" bgcolor="#fff"><tr><td style="padding:8px" align="center"><font face="Arial">x</font></td></tr></table>"##,
        );
        assert!(out.contains("<style>p { color: red }</style>"), "{out}");
        assert!(
            out.contains(r#"width="600""#)
                && out.contains(r#"style="padding:8px""#)
                && out.contains("<font")
        );
    }

    #[test]
    fn links_open_without_referrer() {
        let out = clean(r#"<a href="https://example.com/x">x</a>"#);
        assert!(
            out.contains(r#"href="https://example.com/x""#)
                && out.contains(r#"rel="noopener noreferrer""#),
            "{out}"
        );
    }

    #[test]
    fn inline_images_point_at_the_view_s_own_scheme() {
        let out = sanitize_html(
            r#"<img src="cid:logo@x"><img src=" CID:part 2"><a href="cid:logo@x">x</a>"#,
            Some("mailrs-cid:1/m1/0/"),
        );
        assert!(out.contains(r#"src="mailrs-cid:1/m1/0/logo@x""#), "{out}");
        assert!(out.contains(r#"src="mailrs-cid:1/m1/0/part%202""#), "{out}");
        assert!(!out.contains("href"), "only a picture is served: {out}");
        let none = sanitize_html(r#"<img src="cid:logo@x">"#, None);
        assert!(!none.contains("cid"), "{none}");
    }

    /// Mail cannot name the view's scheme itself: only a `cid:` source
    /// becomes one of its addresses.
    #[test]
    fn mail_cannot_reach_the_view_s_scheme_by_itself() {
        let out = clean(r#"<img src="mailrs-cid:1/m2/0/secret"><a href="mailrs:toggle/m1">x</a>"#);
        assert!(!out.contains("mailrs"), "{out}");
    }

    #[test]
    fn data_uris_only_work_as_images() {
        let out = clean(
            r#"<a href="data:text/html,<script>x</script>">a</a><img src="data:text/html,x"><img src="data:image/gif;base64,R0lG">"#,
        );
        assert!(!out.contains("data:text"), "{out}");
        assert!(out.contains("data:image/gif;base64,r0lg"), "{out}");
    }

    #[test]
    fn comments_are_stripped() {
        assert!(!clean("<!-- tracking --><p>x</p>").contains("tracking"));
    }

    #[test]
    fn a_picture_with_nothing_to_say_says_nothing() {
        // An image nobody described gets an empty description, so a
        // screen reader passes over it instead of reading the source.
        let out = clean(r#"<img src="data:image/gif;base64,R0lG">"#);
        assert!(out.contains("alt=\"\""), "{out}");
        // One the sender described keeps what they wrote, once.
        let out = clean(r#"<img src="https://x/a.png" alt="Our logo">"#);
        assert!(out.contains("alt=\"our logo\""), "{out}");
        assert_eq!(out.matches("alt=").count(), 1, "{out}");
    }
}

#[cfg(test)]
mod body_tests {
    use super::sanitize_html;

    fn clean(html: &str) -> String {
        sanitize_html(html, None)
    }

    /// The `style` attribute of the element with `class`, as written.
    fn style_of<'a>(html: &'a str, class: &str) -> &'a str {
        let open = format!("<div class=\"{class}\" style=\"");
        let from = html
            .find(&open)
            .map(|at| at + open.len())
            .unwrap_or_else(|| panic!("no {class}: {html}"));
        let to = html[from..].find('"').map_or(html.len(), |end| from + end);
        &html[from..to]
    }

    /// GitHub puts its font, size and line height on `<body>`. The cleaner
    /// dropped the tag and the page's own font showed instead.
    #[test]
    fn the_body_tag_s_styles_move_to_the_wrapper() {
        let out = clean(include_str!("demo/github-ci.html"));
        let body = style_of(&out, "mailrs-body");
        assert!(
            body.contains("font-family: -apple-system,BlinkMacSystemFont,Segoe UI,Helvetica"),
            "{body}"
        );
        assert!(
            body.contains("font-size: 14px; line-height: 1.5; margin: 0;"),
            "{body}"
        );
        let html = style_of(&out, "mailrs-html");
        assert!(html.contains("font-family: sans-serif"), "{html}");
        assert!(out.contains("<div class=\"mailrs-html\""), "{out}");
    }

    /// The old attributes are hints a browser lays under the sender's CSS,
    /// so they come first and the `style` attribute overrides them.
    #[test]
    fn body_attributes_come_before_the_sender_s_own_style() {
        let out = clean(
            "<html><body bgcolor=\"f4f4f4\" text=\"#333\" style=\"color:#111\"><p>Hi</p></body></html>",
        );
        assert_eq!(
            style_of(&out, "mailrs-body"),
            "background-color:#f4f4f4;color:#333;color:#111",
            "{out}"
        );
    }

    /// `link` colors links, under the sender's own rules for them.
    #[test]
    fn the_link_attribute_colors_links_before_the_sender_s_rules() {
        let out = clean(
            "<html><head><style>a{color:#c00}</style></head><body link=\"#0a0\"><a href=\"https://example.com/\">x</a></body></html>",
        );
        let hint = out.find(":where(.mailrs-body) a{color:#0a0}").expect(&out);
        let own = out.find("a{color:#c00}").expect(&out);
        assert!(hint < own, "{out}");
    }

    #[test]
    fn a_body_background_keeps_only_addresses_the_cleaner_allows() {
        let out = clean("<body background=\"https://example.com/paper.png\"><p>x</p></body>");
        assert!(
            style_of(&out, "mailrs-body")
                .contains("background-image:url('https://example.com/paper.png')"),
            "{out}"
        );
        let out = clean("<body background=\"javascript:alert(1)\"><p>x</p></body>");
        assert!(
            !out.contains("javascript") && !out.contains("url("),
            "{out}"
        );
    }

    #[test]
    fn a_body_attribute_cannot_break_out_of_its_style() {
        let out = clean(
            "<body style='x\"><script>alert(1)</script>' text=\"red;background:url(https://evil.example/)\" \
             link=\"red}</style><script>bad()</script>\"><p>x</p></body>",
        );
        assert!(
            !out.contains("<script") && !out.contains("evil") && !out.contains("bad()"),
            "{out}"
        );
        assert!(
            !out.contains(":where"),
            "a link color that is not a color is dropped: {out}"
        );
    }

    #[test]
    fn quoted_values_in_the_body_style_stay_quoted() {
        let out = clean("<body style=\"font-family:&quot;Segoe UI&quot;,Arial\"><p>x</p></body>");
        assert_eq!(
            style_of(&out, "mailrs-body"),
            "font-family:&quot;Segoe UI&quot;,Arial",
            "{out}"
        );
    }

    /// A message's `body` and `html` rules would match nothing in the
    /// page's shadow root, so they point at the wrappers instead.
    #[test]
    fn body_and_html_selectors_point_at_the_wrappers() {
        let out = clean(
            "<style>body{margin:0}html body .x,BODY>p{color:red}tbody td{padding:1px}\
             @media (max-width:600px){body{font-size:12px}}.a{background:url(body.png)}\
             @font-face{font-family:body}</style><p>x</p>",
        );
        for kept in [
            ".mailrs-body{margin:0}",
            ".mailrs-html .mailrs-body .x,.mailrs-body>p{color:red}",
            "tbody td{padding:1px}",
            "@media (max-width:600px){.mailrs-body{font-size:12px}}",
            ".a{background:url(body.png)}",
            "@font-face{font-family:body}",
        ] {
            assert!(out.contains(kept), "{kept} in {out}");
        }
        assert!(
            out.starts_with("<div class=\"mailrs-html\"><div class=\"mailrs-body\">"),
            "{out}"
        );
    }

    #[test]
    fn mail_with_nothing_on_its_body_is_not_wrapped() {
        assert_eq!(clean("<html><body><p>Hi</p></body></html>"), "<p>Hi</p>");
    }
}
