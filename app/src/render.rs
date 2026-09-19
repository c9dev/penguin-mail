//! Builds the HTML document the conversation WebView shows. The page has no
//! JavaScript: expanding a message or saving an attachment is a `mailrs:`
//! link that the app intercepts.

use std::collections::HashMap;
use std::fmt::Write;

use mailrs_domain::{Address, MessageBody, MessageMeta};

use crate::format::{color_for, full_date, human_size, initials};
use crate::sanitize::sanitize_html;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    pub dark: bool,
    /// CSS colour of the desktop accent.
    pub accent: String,
}

pub enum BodyState<'a> {
    Loading,
    Loaded(&'a MessageBody),
    Failed(&'a str),
}

pub struct MessageView<'a> {
    pub meta: &'a MessageMeta,
    pub body: BodyState<'a>,
    pub expanded: bool,
    /// `Content-ID` to `data:` URI for this message's inline images.
    pub inline_images: &'a HashMap<String, String>,
}

pub struct Conversation<'a> {
    pub subject: &'a str,
    pub messages: Vec<MessageView<'a>>,
    /// The account's own addresses, shown as "me" in recipient lists.
    pub me: &'a [String],
}

pub fn render(conversation: &Conversation, theme: &Theme) -> String {
    let mut html = String::with_capacity(16 * 1024);
    html.push_str("<!doctype html><html><head><meta charset=\"utf-8\">");
    let _ = write!(html, "<style>{}</style></head><body>", page_css(theme));
    let subject = if conversation.subject.trim().is_empty() {
        "(no subject)"
    } else {
        conversation.subject
    };
    let count = conversation.messages.len();
    let _ = write!(
        html,
        "<header class=\"thread\"><h1>{}</h1><p>{count} message{}</p></header>",
        escape(subject),
        if count == 1 { "" } else { "s" }
    );
    for view in &conversation.messages {
        render_message(&mut html, view, conversation.me);
    }
    html.push_str("</body></html>");
    html
}

fn render_message(html: &mut String, view: &MessageView, me: &[String]) {
    let meta = view.meta;
    let state = if view.expanded {
        "expanded"
    } else {
        "collapsed"
    };
    let unread = if meta.is_unread() { " unread" } else { "" };
    let (name, address) = match &meta.from {
        Some(from) => (from.display().to_string(), from.email.clone()),
        None => ("Unknown sender".to_string(), String::new()),
    };
    let _ = write!(
        html,
        "<article class=\"message {state}{unread}\" id=\"m-{id}\"><a class=\"header\" href=\"mailrs:toggle/{id}\">\
         <span class=\"avatar\" style=\"background:{color}\">{initials}</span>\
         <span class=\"who\"><span class=\"name\">{name}</span>",
        id = escape(&meta.id),
        color = color_for(if address.is_empty() { &name } else { &address }),
        initials = escape(&initials(&name)),
        name = escape(&name),
    );
    if view.expanded && !address.is_empty() && address != name {
        let _ = write!(html, "<span class=\"address\">{}</span>", escape(&address));
    }
    let _ = write!(
        html,
        "</span><span class=\"date\">{}</span>",
        escape(&full_date(meta.date))
    );
    if view.expanded {
        let _ = write!(
            html,
            "<span class=\"line\">to {}</span></a>",
            escape(&recipients(meta, me))
        );
        render_body(html, view);
    } else {
        let _ = write!(
            html,
            "<span class=\"line\">{}</span></a>",
            escape(&meta.snippet)
        );
    }
    html.push_str("</article>");
}

fn render_body(html: &mut String, view: &MessageView) {
    match &view.body {
        BodyState::Loading => html.push_str("<div class=\"body status\">Loading…</div>"),
        BodyState::Failed(reason) => {
            let _ = write!(
                html,
                "<div class=\"body status\">This message could not be loaded: {}</div>",
                escape(reason)
            );
        }
        BodyState::Loaded(body) => {
            if let Some(source) = body.html.as_deref().filter(|h| !h.trim().is_empty()) {
                let _ = write!(
                    html,
                    "<div class=\"body html\"><template shadowrootmode=\"open\"><style>{HTML_BODY_CSS}</style>\
                     <div class=\"root\">{}</div></template></div>",
                    sanitize_html(source, view.inline_images)
                );
            } else {
                let _ = write!(
                    html,
                    "<div class=\"body text\">{}</div>",
                    render_text(body.text.as_deref().unwrap_or(""))
                );
            }
            render_attachments(html, &view.meta.id, body);
        }
    }
}

fn render_attachments(html: &mut String, message_id: &str, body: &MessageBody) {
    let listed: Vec<(usize, &mailrs_domain::Attachment)> = body
        .attachments
        .iter()
        .enumerate()
        .filter(|(_, a)| {
            a.content_id
                .as_ref()
                .is_none_or(|_| !a.mime_type.starts_with("image/"))
        })
        .collect();
    if listed.is_empty() {
        return;
    }
    html.push_str("<div class=\"attachments\">");
    for (index, attachment) in listed {
        let _ = write!(
            html,
            "<a class=\"attachment\" href=\"mailrs:attachment/{}/{index}\" title=\"Save to Downloads\">\
             <span class=\"clip\"></span><span class=\"file\">{}</span><span class=\"size\">{}</span></a>",
            escape(message_id),
            escape(&attachment.filename),
            human_size(attachment.size)
        );
    }
    html.push_str("</div>");
}

/// "me, Bob Smith, and 2 others".
fn recipients(meta: &MessageMeta, me: &[String]) -> String {
    let names: Vec<String> = meta
        .to
        .iter()
        .chain(&meta.cc)
        .map(|a| label(a, me))
        .collect();
    match names.len() {
        0 => "undisclosed recipients".into(),
        1..=3 => names.join(", "),
        n => format!("{}, and {} others", names[..2].join(", "), n - 2),
    }
}

fn label(address: &Address, me: &[String]) -> String {
    if me.iter().any(|m| m.eq_ignore_ascii_case(&address.email)) {
        "me".into()
    } else {
        address.display().to_string()
    }
}

/// Plain text as HTML: quoted lines become nested blockquotes, a `-- `
/// line starts a dimmed signature, and web addresses become links.
pub fn render_text(text: &str) -> String {
    let mut out = String::new();
    let mut quote: Vec<&str> = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        if let Some(rest) = line.trim_start().strip_prefix('>') {
            quote.push(rest.strip_prefix(' ').unwrap_or(rest));
            continue;
        }
        flush_quote(&mut out, &mut quote);
        if line == "-- " || line == "--" {
            let rest: Vec<&str> = lines.by_ref().collect();
            let _ = write!(
                out,
                "<div class=\"signature\">-- \n{}</div>",
                render_text(&rest.join("\n"))
            );
            break;
        }
        out.push_str(&linkify(line));
        out.push('\n');
    }
    flush_quote(&mut out, &mut quote);
    while out.ends_with('\n') {
        out.pop();
    }
    out
}

fn flush_quote(out: &mut String, quote: &mut Vec<&str>) {
    if !quote.is_empty() {
        let _ = write!(
            out,
            "<blockquote class=\"quote\">{}</blockquote>",
            render_text(&quote.join("\n"))
        );
        quote.clear();
    }
}

/// Escapes a line and turns `http` and `https` addresses into links.
fn linkify(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(start) = find_url(rest) {
        out.push_str(&escape(&rest[..start]));
        let candidate = &rest[start..];
        let end = candidate
            .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\''))
            .unwrap_or(candidate.len());
        let url = candidate[..end].trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']']);
        let _ = write!(out, "<a href=\"{0}\">{0}</a>", escape(url));
        rest = &candidate[url.len()..];
    }
    out.push_str(&escape(rest));
    out
}

fn find_url(s: &str) -> Option<usize> {
    [s.find("https://"), s.find("http://")]
        .into_iter()
        .flatten()
        .min()
}

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Styles for an HTML body inside its shadow root. Most email HTML assumes
/// dark text on white, so the body keeps that in both themes.
const HTML_BODY_CSS: &str = ":host{all:initial;display:block;contain:content}\
.root{font:14px/1.5 -apple-system,\"Adwaita Sans\",Cantarell,\"Segoe UI\",Roboto,Helvetica,Arial,sans-serif;\
color:#1d1d20;overflow-wrap:anywhere;overflow-x:auto}\
img{max-width:100%;height:auto}a{color:#1c71d8}";

fn page_css(theme: &Theme) -> String {
    let (bg, fg, dim, card, line, hover) = if theme.dark {
        (
            "#222226",
            "#ffffff",
            "rgba(255,255,255,0.58)",
            "rgba(255,255,255,0.08)",
            "rgba(255,255,255,0.09)",
            "rgba(255,255,255,0.04)",
        )
    } else {
        (
            "#ffffff",
            "rgba(0,0,6,0.84)",
            "rgba(0,0,6,0.52)",
            "rgba(0,0,6,0.05)",
            "rgba(0,0,6,0.08)",
            "rgba(0,0,6,0.03)",
        )
    };
    format!(
        ":root{{color-scheme:{scheme};--bg:{bg};--fg:{fg};--dim:{dim};--card:{card};--line:{line};--hover:{hover};--accent:{accent}}}\
html{{background:var(--bg)}}\
body{{margin:0 auto;max-width:980px;padding:28px 36px 64px;color:var(--fg);\
font:15px/1.5 \"Adwaita Sans\",Cantarell,system-ui,sans-serif;-webkit-font-smoothing:antialiased}}\
.thread h1{{font-size:24px;line-height:1.25;font-weight:750;letter-spacing:-0.01em;margin:0}}\
.thread p{{margin:4px 0 18px;color:var(--dim);font-size:13px}}\
.message{{border-top:1px solid var(--line);padding:14px 10px;margin:0 -10px;border-radius:12px}}\
.message.collapsed:hover{{background:var(--hover)}}\
.header{{display:grid;grid-template-columns:40px minmax(0,1fr) auto;column-gap:12px;align-items:center;color:inherit;text-decoration:none}}\
.avatar{{grid-row:span 2;width:40px;height:40px;border-radius:50%;display:flex;align-items:center;justify-content:center;\
color:#fff;font-weight:700;font-size:15px;letter-spacing:0.02em}}\
.who{{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}}\
.name{{font-weight:700}}.unread .name::before{{content:'';display:inline-block;width:8px;height:8px;border-radius:50%;\
background:var(--accent);margin-right:7px;vertical-align:1px}}\
.address{{color:var(--dim);font-size:13px;margin-left:8px}}\
.date{{color:var(--dim);font-size:13px;white-space:nowrap}}\
.line{{grid-column:2 / span 2;color:var(--dim);font-size:13px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}}\
.body{{margin:16px 0 4px 52px}}\
.text{{white-space:pre-wrap;overflow-wrap:anywhere}}\
.html{{background:#fff;border-radius:12px;padding:18px;border:1px solid var(--line);overflow:hidden}}\
.status{{color:var(--dim);font-style:italic}}\
blockquote.quote{{margin:6px 0;padding:0 0 0 12px;border-left:3px solid color-mix(in srgb,var(--accent) 45%,transparent);color:var(--dim)}}\
.signature{{color:var(--dim)}}\
a{{color:var(--accent)}}\
.attachments{{display:flex;flex-wrap:wrap;gap:8px;margin:14px 0 0 52px}}\
.attachment{{display:inline-flex;align-items:center;gap:8px;padding:8px 12px;border-radius:10px;background:var(--card);\
color:inherit;text-decoration:none;font-size:13px;max-width:320px}}\
.attachment:hover{{background:color-mix(in srgb,var(--card) 100%,var(--fg) 6%)}}\
.attachment .file{{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}}\
.attachment .size{{color:var(--dim);white-space:nowrap}}\
.clip{{width:16px;height:16px;flex:none;background:var(--dim);-webkit-mask:url(\"{CLIP}\") center/contain no-repeat}}",
        scheme = if theme.dark { "dark" } else { "light" },
        accent = theme.accent,
    )
}

const CLIP: &str = "data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 16 16'>\
<path fill='black' d='M10.5 2A3.5 3.5 0 0 0 7 5.5v5a1.5 1.5 0 0 0 3 0V6h-1v4.5a.5.5 0 0 1-1 0v-5a2.5 2.5 0 0 1 5 0v6a3.5 3.5 0 0 1-7 0V5H5v6.5a4.5 4.5 0 0 0 9 0v-6A3.5 3.5 0 0 0 10.5 2z'/></svg>";

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use mailrs_domain::{Address, Attachment, MessageBody, MessageMeta};

    use super::*;

    fn meta(id: &str, name: &str, labels: &[&str]) -> MessageMeta {
        MessageMeta {
            account_id: 1,
            id: id.into(),
            thread_id: "t1".into(),
            rfc822_msgid: None,
            from: Some(Address {
                name: Some(name.into()),
                email: "ann@example.com".into(),
            }),
            to: vec![
                Address {
                    name: None,
                    email: "me@example.com".into(),
                },
                Address {
                    name: Some("Bob Smith".into()),
                    email: "bob@example.com".into(),
                },
            ],
            cc: vec![],
            subject: "Hello".into(),
            date: 1_700_000_000_000,
            snippet: format!("snippet of {id}"),
            size: 10,
            has_attachments: false,
            label_ids: labels.iter().map(|l| l.to_string()).collect(),
        }
    }

    fn theme() -> Theme {
        Theme {
            dark: false,
            accent: "#3584e4".into(),
        }
    }

    fn page(subject: &str, views: Vec<MessageView>) -> String {
        let me = ["me@example.com".to_string()];
        render(
            &Conversation {
                subject,
                messages: views,
                me: &me,
            },
            &theme(),
        )
    }

    #[test]
    fn subjects_and_names_are_escaped() {
        let evil = meta("m1", "<img src=x onerror=alert(1)>", &[]);
        let body = MessageBody {
            text: Some("hi".into()),
            ..Default::default()
        };
        let images = HashMap::new();
        let html = page(
            "<script>alert(1)</script>",
            vec![MessageView {
                meta: &evil,
                body: BodyState::Loaded(&body),
                expanded: true,
                inline_images: &images,
            }],
        );
        assert!(
            !html.contains("<script>alert") && !html.contains("<img src=x"),
            "{html}"
        );
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn collapsed_messages_show_a_snippet_and_expanded_ones_a_body() {
        let first = meta("m1", "Ann", &[]);
        let second = meta("m2", "Ann", &["UNREAD"]);
        let body = MessageBody {
            text: Some("the body".into()),
            ..Default::default()
        };
        let images = HashMap::new();
        let html = page(
            "Hello",
            vec![
                MessageView {
                    meta: &first,
                    body: BodyState::Loaded(&body),
                    expanded: false,
                    inline_images: &images,
                },
                MessageView {
                    meta: &second,
                    body: BodyState::Loaded(&body),
                    expanded: true,
                    inline_images: &images,
                },
            ],
        );
        assert!(html.contains("snippet of m1"));
        assert!(!html.contains("snippet of m2"));
        assert_eq!(html.matches("the body").count(), 1);
        assert!(
            html.contains("href=\"mailrs:toggle/m1\"")
                && html.contains("href=\"mailrs:toggle/m2\"")
        );
        assert!(html.contains("message expanded unread"));
        assert!(html.contains("to me, Bob Smith"));
        assert!(html.contains("2 messages"));
    }

    #[test]
    fn html_bodies_are_sanitized_inside_a_shadow_root() {
        let m = meta("m1", "Ann", &[]);
        let body = MessageBody {
            html: Some("<p>Hi</p><script>bad()</script>".into()),
            ..Default::default()
        };
        let images = HashMap::new();
        let html = page(
            "x",
            vec![MessageView {
                meta: &m,
                body: BodyState::Loaded(&body),
                expanded: true,
                inline_images: &images,
            }],
        );
        assert!(html.contains("<template shadowrootmode=\"open\">"));
        assert!(
            html.contains("<p>Hi</p>") && !html.contains("bad()"),
            "{html}"
        );
    }

    #[test]
    fn attachments_link_to_downloads_but_inline_images_do_not() {
        let m = meta("m1", "Ann", &[]);
        let attachment = |name: &str, mime: &str, cid: Option<&str>| Attachment {
            part_id: name.into(),
            filename: name.into(),
            mime_type: mime.into(),
            size: 2048,
            attachment_id: Some(format!("att-{name}")),
            content_id: cid.map(str::to_string),
        };
        let body = MessageBody {
            text: Some("see attached".into()),
            html: None,
            attachments: vec![
                attachment("logo.png", "image/png", Some("logo")),
                attachment("report.pdf", "application/pdf", None),
            ],
        };
        let images = HashMap::new();
        let html = page(
            "x",
            vec![MessageView {
                meta: &m,
                body: BodyState::Loaded(&body),
                expanded: true,
                inline_images: &images,
            }],
        );
        assert!(html.contains("href=\"mailrs:attachment/m1/1\""));
        assert!(html.contains("report.pdf") && html.contains("2.0 KB"));
        assert!(!html.contains("logo.png"));
    }

    #[test]
    fn loading_and_failure_states_render() {
        let m = meta("m1", "Ann", &[]);
        let images = HashMap::new();
        let loading = page(
            "x",
            vec![MessageView {
                meta: &m,
                body: BodyState::Loading,
                expanded: true,
                inline_images: &images,
            }],
        );
        assert!(loading.contains("Loading…"));
        let failed = page(
            "x",
            vec![MessageView {
                meta: &m,
                body: BodyState::Failed("offline <now>"),
                expanded: true,
                inline_images: &images,
            }],
        );
        assert!(failed.contains("could not be loaded: offline &lt;now&gt;"));
    }

    #[test]
    fn plain_text_quotes_signatures_and_links() {
        let html = render_text(
            "Sure, see https://example.com/a?b=1&c=2.\n> On Monday you wrote:\n>> deeper\n> back\nThanks\n-- \nAnn <ann@example.com>",
        );
        assert!(
            html.contains("<a href=\"https://example.com/a?b=1&amp;c=2\">"),
            "{html}"
        );
        assert!(
            html.contains("2</a>."),
            "trailing dot stays outside the link: {html}"
        );
        assert!(html.contains("<blockquote class=\"quote\">On Monday you wrote:\n<blockquote class=\"quote\">deeper</blockquote>back</blockquote>"), "{html}");
        assert!(
            html.contains("<div class=\"signature\">-- \nAnn &lt;ann@example.com&gt;</div>"),
            "{html}"
        );
    }

    #[test]
    fn many_recipients_are_summarised() {
        let mut m = meta("m1", "Ann", &[]);
        m.cc = (0..4)
            .map(|i| Address {
                name: Some(format!("P{i}")),
                email: format!("p{i}@x.com"),
            })
            .collect();
        assert_eq!(
            recipients(&m, &["me@example.com".into()]),
            "me, Bob Smith, and 4 others"
        );
    }

    #[test]
    fn the_dark_theme_changes_the_page_colours() {
        let dark = page_css(&Theme {
            dark: true,
            accent: "#fff".into(),
        });
        assert!(dark.contains("color-scheme:dark") && dark.contains("#222226"));
    }
}
