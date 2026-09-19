//! Markdown from the model, as Pango markup for GTK labels.

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Converts Markdown to Pango markup: emphasis, code, links, headings, and
/// lists. Anything else becomes plain text.
pub fn to_pango(markdown: &str) -> String {
    let mut out = String::new();
    // A stack of list counters: `None` for bullets, `Some(n)` for numbers.
    let mut lists: Vec<Option<u64>> = Vec::new();
    for event in Parser::new_ext(markdown, Options::ENABLE_STRIKETHROUGH) {
        match event {
            Event::Start(Tag::Strong) => out.push_str("<b>"),
            Event::End(TagEnd::Strong) => out.push_str("</b>"),
            Event::Start(Tag::Emphasis) => out.push_str("<i>"),
            Event::End(TagEnd::Emphasis) => out.push_str("</i>"),
            Event::Start(Tag::Strikethrough) => out.push_str("<s>"),
            Event::End(TagEnd::Strikethrough) => out.push_str("</s>"),
            Event::Start(Tag::Heading { level, .. }) => out.push_str(match level {
                HeadingLevel::H1 | HeadingLevel::H2 => "<span weight=\"bold\" size=\"larger\">",
                _ => "<span weight=\"bold\">",
            }),
            Event::End(TagEnd::Heading(_)) => out.push_str("</span>\n\n"),
            Event::Start(Tag::Link { dest_url, .. }) => {
                out.push_str(&format!("<a href=\"{}\">", escape(&dest_url)))
            }
            Event::End(TagEnd::Link) => out.push_str("</a>"),
            Event::Start(Tag::List(start)) => {
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                lists.push(start);
            }
            Event::End(TagEnd::List(_)) => {
                lists.pop();
                if lists.is_empty() {
                    out.push('\n');
                }
            }
            Event::Start(Tag::Item) => {
                let depth = lists.len().saturating_sub(1);
                out.push_str(&"    ".repeat(depth));
                match lists.last_mut() {
                    Some(Some(n)) => {
                        out.push_str(&format!("{n}. "));
                        *n += 1;
                    }
                    _ => out.push_str("• "),
                }
            }
            Event::End(TagEnd::Item) => {
                if !out.ends_with('\n') {
                    out.push('\n');
                }
            }
            Event::End(TagEnd::Paragraph) => {
                if lists.is_empty() {
                    out.push_str("\n\n");
                }
            }
            Event::Start(Tag::CodeBlock(_)) => out.push_str("<tt>"),
            Event::End(TagEnd::CodeBlock) => out.push_str("</tt>\n"),
            Event::Code(code) => out.push_str(&format!("<tt>{}</tt>", escape(&code))),
            Event::Text(text) => out.push_str(&escape(&text)),
            Event::SoftBreak | Event::HardBreak => out.push('\n'),
            Event::Rule => out.push_str("\n───\n"),
            Event::Html(html) | Event::InlineHtml(html) => out.push_str(&escape(&html)),
            _ => {}
        }
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_becomes_escaped_pango() {
        assert_eq!(
            to_pango("**Two** <things> & `code`"),
            "<b>Two</b> &lt;things&gt; &amp; <tt>code</tt>"
        );
        assert_eq!(to_pango("Done:\n\n- one\n- two"), "Done:\n\n• one\n• two");
        assert_eq!(to_pango("1. a\n2. b"), "1. a\n2. b");
        assert_eq!(
            to_pango("[site](https://x.example/?a=1&b=2)"),
            "<a href=\"https://x.example/?a=1&amp;b=2\">site</a>"
        );
    }
}
