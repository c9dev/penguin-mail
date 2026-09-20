//! Cleans email HTML for display. Layout survives: tables, inline styles,
//! `<style>` blocks, and images. Anything that runs code, submits data, or
//! navigates by itself is removed. Remote loads are blocked separately, by
//! the WebView's content filter, so this module does not parse CSS.
//!
//! The conversation page puts each cleaned body in its own shadow root, so
//! an email's `<style>` cannot restyle the page around it.
//!
//! Mail is shown on a white card, so an email's dark mode rules would put
//! its pale text on white. [`drop_dark_rules`] takes those rules out and
//! leaves the light ones, which is what the sender designed for.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

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

/// Sanitizes `html`. `inline_images` maps a `Content-ID` (without angle
/// brackets) to a `data:` URI; `cid:` image sources become those URIs, and
/// images whose content is unknown lose their source.
pub fn sanitize_html(html: &str, inline_images: &HashMap<String, String>) -> String {
    let images = inline_images.clone();
    let mut builder = Builder::default();
    builder
        .add_tags(&EXTRA_TAGS)
        .rm_clean_content_tags(&["style"])
        .add_generic_attributes(&LAYOUT_ATTRIBUTES)
        .url_schemes(HashSet::from(["http", "https", "mailto", "cid", "data"]))
        .url_relative(UrlRelative::Deny)
        .link_rel(Some("noopener noreferrer"))
        .strip_comments(true)
        .attribute_filter(move |element, attribute, value| {
            filter_url(&images, element, attribute, value)
        });
    drop_dark_rules(&builder.clean(html).to_string())
}

/// Removes `@media` blocks that only apply in dark mode, and the
/// `color-scheme` declarations that ask for one, from a message's `<style>`
/// blocks. Everything outside those blocks stays as it was.
fn drop_dark_rules(html: &str) -> String {
    if !html.contains("prefers-color-scheme") && !html.contains("color-scheme") {
        return html.to_string();
    }
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
        out.push_str(&clean_css(&rest[open..end]));
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
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
    images: &HashMap<String, String>,
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
        return (element == "img")
            .then(|| images.get(key).cloned().map(Cow::Owned))
            .flatten();
    }
    if lower.starts_with("data:") {
        return (element == "img" && lower.starts_with("data:image/"))
            .then_some(Cow::Borrowed(value));
    }
    Some(Cow::Borrowed(value))
}

#[cfg(test)]
mod dark_tests {
    use std::collections::HashMap;

    use super::sanitize_html;

    fn clean(html: &str) -> String {
        sanitize_html(html, &HashMap::new())
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
    use std::collections::HashMap;

    use super::sanitize_html;

    fn clean(html: &str) -> String {
        sanitize_html(html, &HashMap::new()).to_lowercase()
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
    fn inline_images_become_data_uris() {
        let images = HashMap::from([(
            "logo@x".to_string(),
            "data:image/png;base64,AAAA".to_string(),
        )]);
        let out = sanitize_html(
            r#"<img src="cid:logo@x"><img src="cid:missing@x">"#,
            &images,
        );
        assert!(out.contains(r#"src="data:image/png;base64,AAAA""#), "{out}");
        assert!(!out.contains("missing"), "{out}");
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
}
