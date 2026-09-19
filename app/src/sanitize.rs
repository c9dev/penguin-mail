//! Cleans email HTML for display. Layout survives: tables, inline styles,
//! `<style>` blocks, and images. Anything that runs code, submits data, or
//! navigates by itself is removed. Remote loads are blocked separately, by
//! the WebView's content filter, so this module does not parse CSS.
//!
//! The conversation page puts each cleaned body in its own shadow root, so
//! an email's `<style>` cannot restyle the page around it.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use ammonia::{Builder, UrlRelative};

const EXTRA_TAGS: [&str; 5] = ["style", "font", "center", "span", "div"];

const LAYOUT_ATTRIBUTES: [&str; 17] = [
    "style", "class", "align", "valign", "width", "height", "bgcolor", "color", "border", "dir", "face",
    "size", "cellpadding", "cellspacing", "colspan", "rowspan", "nowrap",
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
        .attribute_filter(move |element, attribute, value| filter_url(&images, element, attribute, value));
    builder.clean(html).to_string()
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
        return (element == "img").then(|| images.get(key).cloned().map(Cow::Owned)).flatten();
    }
    if lower.starts_with("data:") {
        return (element == "img" && lower.starts_with("data:image/")).then_some(Cow::Borrowed(value));
    }
    Some(Cow::Borrowed(value))
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
        let out = clean(r#"<script>alert(1)</script><p>hi</p><img src="https://x/a.png" onerror="alert(1)"><SCRIPT>x</SCRIPT>"#);
        assert!(!out.contains("script") && !out.contains("alert") && !out.contains("onerror"), "{out}");
        assert!(out.contains("<p>hi</p>"));
    }

    #[test]
    fn javascript_and_relative_links_lose_their_target() {
        let out = clean(r#"<a href="javascript:alert(1)">a</a><a HREF="JaVaScRiPt:x">b</a><a href="/local">c</a>"#);
        assert!(!out.contains("javascript") && !out.contains("/local"), "{out}");
    }

    #[test]
    fn frames_objects_and_forms_are_removed() {
        let out = clean(
            r#"<iframe src="https://evil"></iframe><object data="x"></object><embed src="y">
               <form action="https://evil"><input name="password"><button>go</button></form>"#,
        );
        for banned in ["iframe", "object", "embed", "form", "input", "button", "evil"] {
            assert!(!out.contains(banned), "{banned} survived: {out}");
        }
    }

    #[test]
    fn documents_cannot_redirect_or_rebase() {
        let out = clean(
            r#"<meta http-equiv="refresh" content="0;url=https://evil"><base href="https://evil/"><link rel="stylesheet" href="https://evil/x.css">"#,
        );
        assert!(!out.contains("meta") && !out.contains("base") && !out.contains("link") && !out.contains("evil"), "{out}");
    }

    #[test]
    fn svg_and_math_payloads_are_removed() {
        let out = clean(r#"<svg><script>alert(1)</script></svg><math><mtext><script>x</script></mtext></math>"#);
        assert!(!out.contains("svg") && !out.contains("script") && !out.contains("math"), "{out}");
    }

    #[test]
    fn layout_survives() {
        let out = clean(
            r##"<style>p { color: red }</style><table width="600" bgcolor="#fff"><tr><td style="padding:8px" align="center"><font face="Arial">x</font></td></tr></table>"##,
        );
        assert!(out.contains("<style>p { color: red }</style>"), "{out}");
        assert!(out.contains(r#"width="600""#) && out.contains(r#"style="padding:8px""#) && out.contains("<font"));
    }

    #[test]
    fn links_open_without_referrer() {
        let out = clean(r#"<a href="https://example.com/x">x</a>"#);
        assert!(out.contains(r#"href="https://example.com/x""#) && out.contains(r#"rel="noopener noreferrer""#), "{out}");
    }

    #[test]
    fn inline_images_become_data_uris() {
        let images = HashMap::from([("logo@x".to_string(), "data:image/png;base64,AAAA".to_string())]);
        let out = sanitize_html(r#"<img src="cid:logo@x"><img src="cid:missing@x">"#, &images);
        assert!(out.contains(r#"src="data:image/png;base64,AAAA""#), "{out}");
        assert!(!out.contains("missing"), "{out}");
    }

    #[test]
    fn data_uris_only_work_as_images() {
        let out = clean(r#"<a href="data:text/html,<script>x</script>">a</a><img src="data:text/html,x"><img src="data:image/gif;base64,R0lG">"#);
        assert!(!out.contains("data:text"), "{out}");
        assert!(out.contains("data:image/gif;base64,r0lg"), "{out}");
    }

    #[test]
    fn comments_are_stripped() {
        assert!(!clean("<!-- tracking --><p>x</p>").contains("tracking"));
    }
}
