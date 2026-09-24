//! Builds the HTML document the conversation WebView shows. The page has no
//! JavaScript: expanding a message or saving an attachment is a `mailrs:`
//! link that the app intercepts.

use std::collections::HashMap;
use std::fmt::Write;

use mailrs_domain::{Address, MessageBody, MessageMeta, Provenance};

use crate::format::{color_for, full_date, header_date, human_size, initials};
use mailrs_domain::translate::{fill, fill_plural, gettext};

/// How long a message's fold takes to open or close.
pub const FOLD_MS: u32 = 240;

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
    /// Gmail's attachment id to a small `data:` URI, for the picture on an
    /// attachment row. A row without one falls back to the paperclip.
    pub thumbnails: &'a HashMap<String, String>,
    /// The body's HTML, already cleaned, or `None` for a body drawn from
    /// its text. Cleaning a long message costs milliseconds, so whoever
    /// builds the page keeps the result and passes it in here.
    pub sanitized: Option<Sanitized<'a>>,
}

/// A body's cleaned HTML, and whether it chooses its own colours. Mail
/// that does is written for a white page and keeps one; mail that does not
/// takes the window's colours.
#[derive(Debug, Clone, Copy)]
pub struct Sanitized<'a> {
    pub html: &'a str,
    pub paints: bool,
}

/// What the top of the page says about the thread as a whole.
pub struct Head<'a> {
    pub subject: &'a str,
    /// How many messages the thread has.
    pub count: usize,
    /// Whether remote images and styles may load.
    pub allow_remote: bool,
}

/// A whole thread, for the tests that read one page at once.
#[cfg(test)]
pub struct Conversation<'a> {
    pub subject: &'a str,
    pub messages: Vec<MessageView<'a>>,
    /// The account's own addresses, shown as "me" in recipient lists.
    pub me: &'a [String],
    /// Contact photos by lower-case sender address, as `data:` URIs. The
    /// page loads nothing from disk, so a photo travels inline.
    pub photos: &'a HashMap<String, String>,
    /// Whether remote images and styles may load.
    pub allow_remote: bool,
}

/// The whole page: the head, one article per message, and the end.
#[cfg(test)]
pub fn render(conversation: &Conversation, theme: &Theme) -> String {
    let mut html = head(
        &Head {
            subject: conversation.subject,
            count: conversation.messages.len(),
            allow_remote: conversation.allow_remote,
        },
        theme,
    );
    for view in &conversation.messages {
        html.push_str(&article(view, conversation.me, conversation.photos));
    }
    html.push_str(TAIL);
    html
}

/// What closes the page after the last article.
pub const TAIL: &str = "</body></html>";

/// Everything before the first message: the document's head with its
/// policy and stylesheet, and the thread's subject and count. A page is
/// patched one article at a time only while this stays the same.
pub fn head(conversation: &Head, theme: &Theme) -> String {
    let mut html = String::with_capacity(16 * 1024);
    html.push_str("<!doctype html><html");
    // Without this a screen reader reads the page in whatever voice it
    // started in, which turns Portuguese into nonsense.
    let language = page_language();
    if !language.is_empty() {
        let _ = write!(html, " lang=\"{}\"", escape(&language));
    }
    html.push_str("><head><meta charset=\"utf-8\">");
    let remote = if conversation.allow_remote {
        " https: http:"
    } else {
        ""
    };
    let _ = write!(
        html,
        "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; script-src 'none'; \
         style-src 'unsafe-inline'{remote}; img-src data: mailrs-cid:{remote}; font-src data:{remote}\">"
    );
    let _ = write!(html, "<style>{}</style></head><body>", page_css(theme));
    let subject = if conversation.subject.trim().is_empty() {
        gettext("(no subject)")
    } else {
        conversation.subject.to_string()
    };
    let count = conversation.count;
    let many = fill_plural(
        "{count} message",
        "{count} messages",
        count,
        &[("count", &count.to_string())],
    );
    let _ = write!(
        html,
        "<header class=\"thread\"><h1>{}</h1><p>{}</p></header>",
        escape(&subject),
        escape(&many),
    );
    html
}

/// One message's `<article>`, whole, as a patch puts it in place of the
/// one before.
pub fn article(view: &MessageView, me: &[String], photos: &HashMap<String, String>) -> String {
    let mut html = String::new();
    render_message(&mut html, view, me, photos);
    html
}

fn render_message(
    html: &mut String,
    view: &MessageView,
    me: &[String],
    photos: &HashMap<String, String>,
) {
    let meta = view.meta;
    // A closed message is also shut, which keeps its body out of layout: a
    // forty message newsletter thread with one message open finished
    // loading in 180 ms instead of 710. The view shuts a message it closes
    // only once the fold has finished moving, since a shut body has no
    // height to animate from.
    let state = if view.expanded {
        "expanded"
    } else {
        "collapsed shut"
    };
    let unread = if meta.is_unread() { " unread" } else { "" };
    let (name, address) = match &meta.from {
        Some(from) => (from.display().to_string(), from.email.clone()),
        None => (gettext("Unknown sender"), String::new()),
    };
    let photo = photos.get(address.trim().to_lowercase().as_str());
    // The whole header toggles the message, so the toggle is a layer
    // under it. The face and the name sit above that layer and open the
    // sender's card instead.
    let card = format!("mailrs:contact/{}", escape(&address));
    let _ = write!(
        html,
        "<article class=\"message {state}{unread}\" id=\"m-{id}\"><div class=\"header\">\
         <a class=\"toggle\" href=\"mailrs:toggle/{id}\" aria-label=\"{toggle}\"></a>",
        id = escape(&meta.id),
        toggle = escape(&gettext("Show or hide this message")),
    );
    let contact = escape(&gettext("Contact"));
    // The face carries no meaning a reader needs, and the initials behind
    // it are two letters of the name said beside them, so the link says
    // whose card it opens and the picture itself says nothing.
    let opens = escape(&fill(
        &gettext("Contact card for {person}"),
        &[("person", &name)],
    ));
    match photo {
        Some(uri) => {
            let _ = write!(
                html,
                "<a class=\"avatar\" href=\"{card}\" title=\"{contact}\" \
                 aria-label=\"{opens}\"><img src=\"{uri}\" alt=\"\"></a>"
            );
        }
        None => {
            let _ = write!(
                html,
                "<a class=\"avatar\" href=\"{card}\" title=\"{contact}\" \
                 aria-label=\"{opens}\" style=\"background:{color}\">{initials}</a>",
                color = color_for(if address.is_empty() { &name } else { &address }),
                initials = escape(&initials(&name)),
            );
        }
    }
    // The thread's subject is the page's only h1; each message's sender
    // is its heading under it, so a reader can jump message to message.
    let _ = write!(
        html,
        "<span class=\"who\" role=\"heading\" aria-level=\"2\">\
         <a class=\"name\" href=\"{card}\">{name}</a>",
        name = escape(&name),
    );
    if !address.is_empty() && address != name {
        let _ = write!(html, "<span class=\"address\">{}</span>", escape(&address));
    }
    let _ = write!(
        html,
        "</span><span class=\"date\">{}</span><span class=\"chev\" aria-hidden=\"true\"></span>",
        escape(&header_date(meta.date, chrono::Local::now())),
    );
    render_details(html, meta, me, view);
    let _ = write!(
        html,
        "<span class=\"line snippet\">{}</span></div>",
        escape(&meta.snippet)
    );
    // The body and its files live in one fold, which is what the expand
    // animation grows and shrinks. A collapsed message keeps them in the
    // page at zero height rather than dropping them, so the two states
    // have something to move between.
    html.push_str("<div class=\"fold\"><div class=\"folded\">");
    render_body(html, view);
    html.push_str("</div></div></article>");
}

fn render_body(html: &mut String, view: &MessageView) {
    match &view.body {
        BodyState::Loading => {
            let _ = write!(
                html,
                "<div class=\"body status\">{}</div>",
                escape(&gettext("Loading…"))
            );
        }
        BodyState::Failed(reason) => {
            let said = fill(
                &gettext("This message could not be loaded: {reason}"),
                &[("reason", reason)],
            );
            let _ = write!(html, "<div class=\"body status\">{}</div>", escape(&said));
        }
        BodyState::Loaded(body) => {
            if let Some(clean) = view.sanitized {
                let _ = write!(
                    html,
                    "<div class=\"body html{plain}\"><template shadowrootmode=\"open\"><style>{HTML_BODY_CSS}</style>\
                     <div class=\"root\">{html}</div></template></div>",
                    html = clean.html,
                    plain = if clean.paints { "" } else { " plain" },
                );
            } else {
                let _ = write!(
                    html,
                    "<div class=\"body text\">{}</div>",
                    render_text(body.text.as_deref().unwrap_or(""))
                );
            }
            render_attachments(html, &view.meta.id, body, view.thumbnails);
        }
    }
}

/// The recipients line, and under it everything the headers say about
/// where the message came from.
///
/// It is a `<details>` element, so the arrow opens and closes it with no
/// JavaScript: the page carries none, and a link that opened a panel
/// would cost a round trip through the app and a redraw.
fn render_details(html: &mut String, meta: &MessageMeta, me: &[String], view: &MessageView) {
    let to = escape(&fill(
        &gettext("to {recipients}"),
        &[("recipients", &recipients(meta, me))],
    ));
    // Who it is from, who it went to, when, and about what: all of that
    // comes off the metadata every message already has, so the panel opens
    // on any message. The three lines below it need headers that arrive
    // with the body, and a message read before this app learned to keep
    // them has none; those lines are left out rather than the whole panel.
    let provenance = match &view.body {
        BodyState::Loaded(body) => Some(&body.provenance),
        _ => None,
    };
    let empty = Provenance::default();
    let provenance = provenance.unwrap_or(&empty);
    let _ = write!(
        html,
        "<details class=\"line to\"><summary>{to}</summary><table class=\"details\">"
    );
    let mut row = |name: String, value: String| {
        let _ = write!(html, "<tr><th>{}</th><td>{value}</td></tr>", escape(&name));
    };
    row(
        gettext("from"),
        match meta.from.as_ref() {
            Some(from) if from.name.is_some() => format!(
                "<b>{}</b> &lt;{}&gt;",
                escape(from.display()),
                escape(&from.email)
            ),
            Some(from) => escape(&from.email),
            None => String::new(),
        },
    );
    row(gettext("to"), escape(&addresses(&meta.to)));
    if !meta.cc.is_empty() {
        row(gettext("cc"), escape(&addresses(&meta.cc)));
    }
    row(gettext("date"), escape(&full_date(meta.date)));
    row(gettext("subject"), escape(&meta.subject));
    if let Some(mailed_by) = &provenance.mailed_by {
        row(gettext("mailed-by"), escape(mailed_by));
    }
    if let Some(signed_by) = &provenance.signed_by {
        row(gettext("signed-by"), escape(signed_by));
    }
    if let Some(encrypted) = provenance.encrypted {
        row(
            gettext("security"),
            match encrypted {
                true => escape(&gettext("Standard encryption (TLS)")),
                // Worth saying plainly. Mail that crossed the internet in
                // the clear could be read on the way.
                false => format!(
                    "<span class=\"warn\">{}</span>",
                    escape(&gettext("Not encrypted in transit"))
                ),
            },
        );
    }
    html.push_str("</table></details>");
}

/// "Ann Lee <ann@example.com>, bo@example.com", for the details table.
fn addresses(list: &[Address]) -> String {
    list.iter()
        .map(|address| match &address.name {
            Some(name) => format!("{name} <{}>", address.email),
            None => address.email.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn render_attachments(
    html: &mut String,
    message_id: &str,
    body: &MessageBody,
    thumbnails: &HashMap<String, String>,
) {
    let listed: Vec<(usize, &mailrs_domain::Attachment)> = body
        .attachments
        .iter()
        .enumerate()
        .filter(|(_, a)| !shown_in_body(a, body))
        .collect();
    if listed.is_empty() {
        return;
    }
    let id = escape(message_id);
    let count = listed.len();
    let heading = fill_plural(
        "{count} attachment",
        "{count} attachments",
        count,
        &[("count", &count.to_string())],
    );
    let _ = write!(
        html,
        "<div class=\"attachments\" role=\"list\" aria-label=\"{}\">",
        escape(&heading),
    );
    for (index, attachment) in &listed {
        let thumbnail = attachment
            .attachment_id
            .as_deref()
            .and_then(|key| thumbnails.get(key));
        let face = match thumbnail {
            Some(uri) => format!("<img class=\"thumb\" src=\"{uri}\" alt=\"\">"),
            None => "<span class=\"clip\"></span>".to_string(),
        };
        // The download link is an empty square with a background image,
        // so its name has to be given; the open link's own words are the
        // file name and its size, which is not what pressing it does.
        let _ = write!(
            html,
            "<span class=\"attachment\" role=\"listitem\">\
             <a class=\"open\" href=\"mailrs:preview/{id}/{index}\" \
             title=\"{look}\" aria-label=\"{opens}\">{face}<span class=\"file\">{}</span>\
             <span class=\"size\">{}</span></a>\
             <a class=\"get\" href=\"mailrs:attachment/{id}/{index}\" \
             title=\"{save}\" aria-label=\"{gets}\"></a></span>",
            escape(&attachment.filename),
            human_size(attachment.size),
            look = escape(&gettext("Quick Look")),
            save = escape(&gettext("Save to Downloads")),
            opens = escape(&fill(
                &gettext("Open {file}, {size}"),
                &[
                    ("file", &attachment.filename),
                    ("size", &human_size(attachment.size)),
                ],
            )),
            gets = escape(&fill(
                &gettext("Save {file} to Downloads"),
                &[("file", &attachment.filename)],
            )),
        );
    }
    if count > 1 {
        let all = fill_plural(
            "Save All ({count})",
            "Save All ({count})",
            count,
            &[("count", &count.to_string())],
        );
        let _ = write!(
            html,
            "<a class=\"attachment all\" role=\"listitem\" \
             href=\"mailrs:attachments/{id}\" \
             title=\"{title}\" aria-label=\"{title}\"><span class=\"file\">{}</span></a>",
            escape(&all),
            title = escape(&gettext("Save every attachment to a folder")),
        );
    }
    html.push_str("</div>");
}

/// The language tag to put on the page, written the way a screen reader
/// wants it: `pt-PT` rather than `pt_PT.UTF-8`. Empty when the desktop
/// names no language, and then the page claims none rather than English.
fn page_language() -> String {
    language_tag(
        ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"]
            .into_iter()
            .filter_map(|name| std::env::var(name).ok()),
    )
}

/// The tag the first usable locale in `asked` gives. `LANGUAGE` holds a
/// list, best first; the others hold one locale, with an encoding and a
/// variant that no tag wants. `C` and `POSIX` name no language at all.
fn language_tag(asked: impl IntoIterator<Item = String>) -> String {
    for value in asked {
        let tag = value
            .split(':')
            .next()
            .unwrap_or_default()
            .split(['.', '@'])
            .next()
            .unwrap_or_default();
        if tag.is_empty() || tag == "C" || tag == "POSIX" {
            continue;
        }
        return tag.replace('_', "-");
    }
    String::new()
}

/// Whether the message already shows this attachment where the reader is
/// looking, so a row for it would be a second copy. Only an image the HTML
/// points at by `cid:` counts. A `Content-ID` on its own does not: Apple
/// Mail and Outlook put one on files they mean you to save, and treating
/// that as shown is what made an attached photo vanish from the message it
/// arrived in.
pub fn shown_in_body(attachment: &mailrs_domain::Attachment, body: &MessageBody) -> bool {
    let Some(cid) = attachment.content_id.as_deref() else {
        return false;
    };
    if !attachment.mime_type.starts_with("image/") {
        return false;
    }
    body.html
        .as_deref()
        .is_some_and(|html| crate::compose::refers_to_cid(html, cid))
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
        0 => gettext("undisclosed recipients"),
        1..=3 => names.join(", "),
        n => fill_plural(
            "{named}, and {count} other",
            "{named}, and {count} others",
            n - 2,
            &[
                ("named", &names[..2].join(", ")),
                ("count", &(n - 2).to_string()),
            ],
        ),
    }
}

fn label(address: &Address, me: &[String]) -> String {
    if me.iter().any(|m| m.eq_ignore_ascii_case(&address.email)) {
        gettext("me")
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
:host(.plain) .root{color:var(--fg)}:host(.plain) a{color:var(--accent)}\
.root{font:14px/1.5 -apple-system,\"Adwaita Sans\",Cantarell,\"Segoe UI\",Roboto,Helvetica,Arial,sans-serif;\
color:#1d1d20;overflow-wrap:anywhere;overflow-x:auto}\
img{max-width:100% !important;height:auto !important}\
table{max-width:100% !important}td,th{overflow-wrap:anywhere}a{color:#1c71d8}";

fn page_css(theme: &Theme) -> String {
    // A message sits on a surface a step away from the page: lighter in a
    // dark window, darker in a light one, so an open message reads as a
    // sheet rather than as more page.
    let (bg, fg, dim, card, line, hover, surface) = if theme.dark {
        (
            "#222226",
            "#ffffff",
            "rgba(255,255,255,0.58)",
            "rgba(255,255,255,0.08)",
            "rgba(255,255,255,0.09)",
            "rgba(255,255,255,0.04)",
            "#2b2b30",
        )
    } else {
        (
            "#ffffff",
            "rgba(0,0,6,0.84)",
            "rgba(0,0,6,0.52)",
            "rgba(0,0,6,0.05)",
            "rgba(0,0,6,0.08)",
            "rgba(0,0,6,0.03)",
            "#f4f4f6",
        )
    };
    format!(
        ":root{{color-scheme:{scheme};--bg:{bg};--fg:{fg};--dim:{dim};--card:{card};--line:{line};--hover:{hover};--surface:{surface};--accent:{accent}}}\
html{{background:var(--bg)}}\
body{{margin:0 auto;max-width:980px;padding:28px 36px 64px;color:var(--fg);\
font:15px/1.5 \"Adwaita Sans\",Cantarell,system-ui,sans-serif;-webkit-font-smoothing:antialiased}}\
.thread h1{{font-size:24px;line-height:1.25;font-weight:750;letter-spacing:-0.01em;margin:0}}\
.thread p{{margin:4px 0 18px;color:var(--dim);font-size:13px}}\
.message{{border-top:1px solid var(--line);padding:16px 12px 18px;margin:0 -12px;border-radius:12px}}\
.message.collapsed:hover{{background:var(--hover)}}\
.header{{position:relative;display:grid;grid-template-columns:40px minmax(0,1fr) auto auto;\
column-gap:12px;align-items:center;color:inherit}}\
.chev{{width:16px;height:16px;justify-self:end;background:var(--dim);opacity:0;\
-webkit-mask:url(\"{CHEV}\") center/14px no-repeat;\
transition:transform 200ms cubic-bezier(0.23,1,0.32,1),opacity 120ms ease}}\
.message:hover .chev,.expanded .chev,.toggle:focus-visible~.chev{{opacity:.7}}\
.expanded .chev{{transform:rotate(180deg)}}\
.toggle:focus-visible{{outline:2px solid var(--accent);outline-offset:-3px;border-radius:12px}}\
.toggle{{position:absolute;inset:0}}\
.avatar{{position:relative;grid-row:span 2;width:40px;height:40px;border-radius:50%;display:flex;align-items:center;\
justify-content:center;overflow:hidden;color:#fff;font-weight:700;font-size:15px;letter-spacing:0.02em;text-decoration:none}}\
.avatar img{{width:100%;height:100%;object-fit:cover}}\
.who{{position:relative;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}}\
.name{{font-weight:700;color:inherit;text-decoration:none}}\
.name:hover{{text-decoration:underline}}\
.unread .name::before{{content:'';display:inline-block;width:8px;height:8px;border-radius:50%;\
background:var(--accent);margin-right:7px;vertical-align:1px}}\
.address{{color:var(--dim);font-size:13px;margin-left:8px}}\
.date{{color:var(--dim);font-size:13px;white-space:nowrap}}\
.line{{grid-column:2 / span 2;color:var(--dim);font-size:13px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}}\
details.to{{overflow:visible;white-space:normal}}\
details.to>summary{{list-style:none;cursor:default;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;\
width:fit-content;max-width:100%;padding-right:16px;position:relative}}\
details.to>summary::-webkit-details-marker{{display:none}}\
details.to>summary::after{{content:\"\";position:absolute;right:2px;top:.45em;width:0;height:0;\
border:4px solid transparent;border-top-color:var(--dim)}}\
details.to[open]>summary::after{{top:.2em;border-top-color:transparent;border-bottom-color:var(--dim)}}\
details.to>summary:hover{{color:var(--fg)}}\
table.details{{margin:8px 0 2px;border-collapse:collapse;font-size:13px;line-height:1.45}}\
table.details th{{text-align:right;font-weight:normal;color:var(--dim);padding:1px 10px 1px 0;\
vertical-align:top;white-space:nowrap}}\
table.details td{{text-align:left;color:var(--fg);padding:1px 0;word-break:break-word}}\
table.details .warn{{color:#c0392b}}\
.collapsed .to,.collapsed .address,.expanded .snippet{{display:none}}\
.collapsed .toggle{{cursor:pointer}}\
.fold{{display:grid;grid-template-rows:1fr;\
transition:grid-template-rows {fold}ms cubic-bezier(0.23,1,0.32,1),\
opacity 180ms cubic-bezier(0.23,1,0.32,1)}}\
.folded{{overflow:hidden;min-height:0}}\
.collapsed .fold{{grid-template-rows:0fr;opacity:0}}\
.shut .folded{{content-visibility:hidden}}\
.message{{transition:background-color 120ms ease}}\
.attachment,.attachment .get{{transition:background-color 120ms ease,opacity 120ms ease}}\
@media (prefers-reduced-motion:reduce){{.fold,.message{{transition:none}}}}\
.body{{margin:14px 0 2px 52px}}\
.text{{white-space:pre-wrap;overflow-wrap:anywhere}}\
.body.text,.body.status,.html{{background:var(--surface);border-radius:12px;padding:14px;\
border:1px solid var(--line);overflow:hidden;margin-left:0}}\
.html{{background:#fff}}\
.html.plain{{background:var(--surface)}}\
.status{{color:var(--dim);font-style:italic}}\
blockquote.quote{{margin:6px 0;padding:0 0 0 12px;border-left:3px solid color-mix(in srgb,var(--accent) 45%,transparent);color:var(--dim)}}\
.signature{{color:var(--dim)}}\
a{{color:var(--accent)}}\
.attachments{{display:flex;flex-wrap:wrap;gap:8px;margin:14px 0 0 52px}}\
.attachment{{display:inline-flex;align-items:center;border-radius:10px;background:var(--card);\
color:inherit;text-decoration:none;font-size:13px;max-width:340px;overflow:hidden}}\
.attachment:hover{{background:color-mix(in srgb,var(--card) 100%,var(--fg) 6%)}}\
.attachment .open{{display:inline-flex;align-items:center;gap:8px;padding:8px 4px 8px 12px;\
color:inherit;text-decoration:none;min-width:0}}\
.attachment.all{{padding:8px 12px}}\
.attachment .file{{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}}\
.attachment .size{{color:var(--dim);white-space:nowrap}}\
.attachment .get{{width:28px;align-self:stretch;flex:none;background:var(--dim);\
-webkit-mask:url(\"{DOWN}\") center/16px no-repeat;opacity:.6}}\
.attachment .get:hover{{opacity:1;background:var(--accent)}}\
.thumb{{width:32px;height:32px;flex:none;border-radius:5px;object-fit:cover;background:var(--card)}}\
.clip{{width:16px;height:16px;flex:none;background:var(--dim);-webkit-mask:url(\"{CLIP}\") center/contain no-repeat}}\
@media (max-width:560px){{body{{padding:18px 14px 40px}}.body,.attachments{{margin-left:0}}.thread h1{{font-size:21px}}\
.address{{display:none}}.message{{padding:14px 8px 16px;margin:0 -8px}}.chev{{display:none}}}}",
        fold = FOLD_MS,
        scheme = if theme.dark { "dark" } else { "light" },
        accent = theme.accent,
    )
}

const CLIP: &str = "data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 16 16'>\
<path fill='black' d='M10.5 2A3.5 3.5 0 0 0 7 5.5v5a1.5 1.5 0 0 0 3 0V6h-1v4.5a.5.5 0 0 1-1 0v-5a2.5 2.5 0 0 1 5 0v6a3.5 3.5 0 0 1-7 0V5H5v6.5a4.5 4.5 0 0 0 9 0v-6A3.5 3.5 0 0 0 10.5 2z'/></svg>";

/// The chevron beside a message's date, which says the header opens it.
const CHEV: &str = "data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 16 16'>\
<path fill='black' d='M8 10.9 3.3 6.2l1-1L8 8.9l3.7-3.7 1 1z'/></svg>";

const DOWN: &str = "data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 16 16'>\
<path fill='black' d='M7.5 1.5h1v8.3l3-3 .7.7-4.2 4.2-4.2-4.2.7-.7 3 3V1.5zM3 13h10v1H3z'/></svg>";

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use mailrs_domain::{Address, Attachment, MessageBody, MessageMeta};

    use super::*;

    fn meta(id: &str, name: &str, labels: &[&str]) -> MessageMeta {
        let mut meta = MessageMeta {
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
            held: Default::default(),
            roles: vec![],
            list_unsubscribe: None,
            one_click: false,
        };
        let labels: Vec<String> = labels.iter().map(|l| l.to_string()).collect();
        mailrs_gmail::labels::set_label_ids(&mut meta, &labels);
        meta
    }

    fn theme() -> Theme {
        Theme {
            dark: false,
            accent: "#3584e4".into(),
        }
    }

    fn page(subject: &str, views: Vec<MessageView>) -> String {
        page_with(subject, views, &HashMap::new())
    }

    fn page_with(
        subject: &str,
        views: Vec<MessageView>,
        photos: &HashMap<String, String>,
    ) -> String {
        let me = ["me@example.com".to_string()];
        render(
            &Conversation {
                subject,
                messages: views,
                me: &me,
                photos,
                allow_remote: false,
            },
            &theme(),
        )
    }

    /// Writes a page with one long collapsed message to `PM_DUMP_PAGE`,
    /// for a by-hand check of the fold in a real engine. Does nothing
    /// without that variable.
    #[test]
    fn dump_a_page_for_the_animation_check() {
        let Ok(path) = std::env::var("PM_DUMP_PAGE") else {
            return;
        };
        let one = meta("m1", "Kites", &[]);
        let body = MessageBody {
            text: Some("line\n".repeat(40)),
            ..Default::default()
        };
        let no_thumbs = HashMap::new();
        let html = page(
            "Kites",
            vec![MessageView {
                meta: &one,
                body: BodyState::Loaded(&body),
                expanded: false,
                thumbnails: &no_thumbs,
                sanitized: None,
            }],
        );
        std::fs::write(path, html).expect("the page is written");
    }

    #[test]
    fn mail_that_picks_no_colours_takes_the_window_s_own() {
        let plain = meta("m1", "Kites", &[]);
        let body = MessageBody {
            html: Some("<div dir=\"ltr\">Hello,<br><br>Monday works.</div>".into()),
            ..Default::default()
        };
        let no_thumbs = HashMap::new();
        let view = |body: &'static MessageBody, meta: &'static MessageMeta, paints| MessageView {
            meta,
            body: BodyState::Loaded(body),
            expanded: true,
            thumbnails: &no_thumbs,
            // Both fixtures are already clean.
            sanitized: body.html.as_deref().map(|html| Sanitized { html, paints }),
        };
        let html = page(
            "Kites",
            vec![view(
                Box::leak(Box::new(body)),
                Box::leak(Box::new(plain)),
                false,
            )],
        );
        assert!(html.contains("body html plain"), "{html}");

        let painted = meta("m2", "Sale", &[]);
        let loud = MessageBody {
            html: Some("<table bgcolor=\"#ffffff\"><tr><td>Sale</td></tr></table>".into()),
            ..Default::default()
        };
        let html = page(
            "Sale",
            vec![view(
                Box::leak(Box::new(loud)),
                Box::leak(Box::new(painted)),
                true,
            )],
        );
        assert!(
            html.contains("body html\""),
            "a newsletter keeps its white page: {html}"
        );
    }

    #[test]
    fn subjects_and_names_are_escaped() {
        let evil = meta("m1", "<img src=x onerror=alert(1)>", &[]);
        let body = MessageBody {
            text: Some("hi".into()),
            ..Default::default()
        };
        let no_thumbs = HashMap::new();
        let html = page(
            "<script>alert(1)</script>",
            vec![MessageView {
                meta: &evil,
                body: BodyState::Loaded(&body),
                expanded: true,
                thumbnails: &no_thumbs,
                sanitized: None,
            }],
        );
        assert!(
            !html.contains("<script>alert") && !html.contains("<img src=x"),
            "{html}"
        );
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn a_contact_photo_replaces_the_initials() {
        let from_ann = meta("m1", "Ann Lee", &[]);
        let body = MessageBody::default();
        let no_thumbs = HashMap::new();
        let view = || MessageView {
            meta: &from_ann,
            body: BodyState::Loaded(&body),
            expanded: false,
            thumbnails: &no_thumbs,
            sanitized: None,
        };
        let initials = page("Hi", vec![view()]);
        assert!(initials.contains(">AL</a>"), "{initials}");

        let photos = HashMap::from([(
            "ann@example.com".to_string(),
            "data:image/jpeg;base64,AAAA".to_string(),
        )]);
        let with_photo = page_with("Hi", vec![view()], &photos);
        assert!(
            with_photo.contains("<img src=\"data:image/jpeg;base64,AAAA\" alt=\"\">"),
            "{with_photo}"
        );
        assert!(!with_photo.contains(">AL</a>"));
        // Either way the face opens the sender's card.
        for html in [initials, with_photo] {
            assert!(
                html.contains("href=\"mailrs:contact/ann@example.com\""),
                "{html}"
            );
        }
    }

    #[test]
    fn collapsed_messages_show_a_snippet_and_expanded_ones_a_body() {
        let first = meta("m1", "Ann", &[]);
        let second = meta("m2", "Ann", &["UNREAD"]);
        let body = MessageBody {
            text: Some("the body".into()),
            ..Default::default()
        };
        let no_thumbs = HashMap::new();
        let html = page(
            "Hello",
            vec![
                MessageView {
                    meta: &first,
                    body: BodyState::Loaded(&body),
                    expanded: false,
                    thumbnails: &no_thumbs,
                    sanitized: None,
                },
                MessageView {
                    meta: &second,
                    body: BodyState::Loaded(&body),
                    expanded: true,
                    thumbnails: &no_thumbs,
                    sanitized: None,
                },
            ],
        );
        assert!(html.contains("message collapsed shut\" id=\"m-m1\""));
        assert!(
            html.contains(".shut .folded{content-visibility:hidden}"),
            "a message closed since the page loaded is not laid out"
        );
        assert!(
            html.contains(".collapsed .fold{grid-template-rows:0fr;opacity:0}"),
            "a collapsed message keeps its fold at no height"
        );
        assert_eq!(
            html.matches("<div class=\"fold\">").count(),
            2,
            "each message's body sits in a fold the animation can move"
        );
        assert!(html.contains("snippet of m1") && html.contains("snippet of m2"));
        assert_eq!(
            html.matches("the body").count(),
            2,
            "every body is in the page, so toggling needs no reload"
        );
        assert!(
            html.contains("href=\"mailrs:toggle/m1\"")
                && html.contains("href=\"mailrs:toggle/m2\"")
        );
        assert!(html.contains("message expanded unread"));
        assert!(html.contains("to me, Bob Smith"));
        assert!(html.contains("2 messages"));
    }

    #[test]
    fn an_html_body_sits_inside_a_shadow_root() {
        let m = meta("m1", "Ann", &[]);
        let body = MessageBody {
            html: Some("<p>Hi</p><script>bad()</script>".into()),
            ..Default::default()
        };
        let no_thumbs = HashMap::new();
        let html = page(
            "x",
            vec![MessageView {
                meta: &m,
                body: BodyState::Loaded(&body),
                expanded: true,
                thumbnails: &no_thumbs,
                sanitized: Some(Sanitized {
                    html: "<p>Hi</p>",
                    paints: false,
                }),
            }],
        );
        assert!(html.contains("<template shadowrootmode=\"open\">"));
        assert!(
            html.contains("<div class=\"root\"><p>Hi</p></div>") && !html.contains("bad()"),
            "the page draws the cleaned copy it was given: {html}"
        );
    }

    #[test]
    fn an_image_the_body_shows_gets_no_row_and_everything_else_does() {
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
            html: Some("<p>Hi</p><img src=\"cid:logo\">".into()),
            attachments: vec![
                attachment("logo.png", "image/png", Some("logo")),
                attachment("report.pdf", "application/pdf", None),
                // Apple Mail and Outlook put a Content-ID on a photo they
                // mean you to save. The body never names it, so it is a file.
                attachment("holiday.jpg", "image/jpeg", Some("logo2")),
                attachment("invite.ics", "text/calendar", Some("cal")),
            ],
            ..Default::default()
        };
        let no_thumbs = HashMap::new();
        let html = page(
            "x",
            vec![MessageView {
                meta: &m,
                body: BodyState::Loaded(&body),
                expanded: true,
                thumbnails: &no_thumbs,
                sanitized: None,
            }],
        );
        assert!(html.contains("href=\"mailrs:attachment/m1/1\""));
        assert!(html.contains("report.pdf") && html.contains("2.0 KB"));
        assert!(html.contains("holiday.jpg"), "the body never shows it");
        assert!(html.contains("invite.ics"), "a file is a file, cid or not");
        assert!(!html.contains("logo.png"), "the body already shows it");
    }

    #[test]
    fn a_picture_shows_on_its_row_and_several_files_offer_save_all() {
        let m = meta("m1", "Ann", &[]);
        let attachment = |name: &str, mime: &str, id: &str| Attachment {
            part_id: name.into(),
            filename: name.into(),
            mime_type: mime.into(),
            size: 2048,
            attachment_id: Some(id.into()),
            content_id: None,
        };
        let body = MessageBody {
            text: Some("two files".into()),
            attachments: vec![
                attachment("cat.png", "image/png", "att-1"),
                attachment("report.pdf", "application/pdf", "att-2"),
            ],
            ..Default::default()
        };
        let mut no_thumbs = HashMap::new();
        no_thumbs.insert(
            "att-1".to_string(),
            "data:image/png;base64,AAAA".to_string(),
        );
        let html = page(
            "x",
            vec![MessageView {
                meta: &m,
                body: BodyState::Loaded(&body),
                expanded: true,
                thumbnails: &no_thumbs,
                sanitized: None,
            }],
        );
        assert!(html.contains("<img class=\"thumb\" src=\"data:image/png;base64,AAAA\""));
        // The file with no picture keeps the paperclip.
        assert!(html.contains("<span class=\"clip\"></span><span class=\"file\">report.pdf"));
        assert!(html.contains("href=\"mailrs:preview/m1/0\""));
        assert!(html.contains("href=\"mailrs:attachment/m1/1\""));
        assert!(html.contains("href=\"mailrs:attachments/m1\""));
        assert!(html.contains("Save All (2)"));
        // Each row is a list item, and both of its links say which file
        // they act on rather than "Quick Look" twice over.
        assert!(html.contains("role=\"list\" aria-label=\"2 attachments\""));
        assert!(
            html.contains("aria-label=\"Open cat.png, 2.0 KB\""),
            "{html}"
        );
        assert!(
            html.contains("aria-label=\"Save report.pdf to Downloads\""),
            "{html}"
        );
    }

    #[test]
    fn the_page_names_the_language_it_is_written_in() {
        let asked = |list: &[&str]| language_tag(list.iter().map(|s| s.to_string()));
        assert_eq!(asked(&["pt_PT:pt", "pt_PT.UTF-8"]), "pt-PT");
        assert_eq!(asked(&["", "C", "de_DE.UTF-8"]), "de-DE");
        assert_eq!(asked(&["ca_ES@valencia"]), "ca-ES");
        assert_eq!(asked(&["", "POSIX"]), "");
        assert_eq!(asked(&[]), "");
    }

    #[test]
    fn a_sender_is_the_heading_under_the_subject() {
        let m = meta("m1", "Ann", &[]);
        let thumbs = HashMap::new();
        let html = page(
            "Rent",
            vec![MessageView {
                meta: &m,
                body: BodyState::Loading,
                expanded: true,
                thumbnails: &thumbs,
                sanitized: None,
            }],
        );
        assert!(html.contains("<h1>Rent</h1>"), "{html}");
        assert!(
            html.contains("<span class=\"who\" role=\"heading\" aria-level=\"2\">"),
            "{html}"
        );
        assert!(
            html.contains("aria-label=\"Contact card for Ann\""),
            "{html}"
        );
    }

    #[test]
    fn one_attachment_offers_no_save_all() {
        let m = meta("m1", "Ann", &[]);
        let body = MessageBody {
            text: Some("one file".into()),
            attachments: vec![Attachment {
                part_id: "2".into(),
                filename: "report.pdf".into(),
                mime_type: "application/pdf".into(),
                size: 2048,
                attachment_id: Some("att-1".into()),
                content_id: None,
            }],
            ..Default::default()
        };
        let no_thumbs = HashMap::new();
        let html = page(
            "x",
            vec![MessageView {
                meta: &m,
                body: BodyState::Loaded(&body),
                expanded: true,
                thumbnails: &no_thumbs,
                sanitized: None,
            }],
        );
        assert!(!html.contains("Save All"));
    }

    #[test]
    fn the_details_panel_says_who_really_sent_it() {
        let m = meta("m1", "Ann", &["bo@example.com"]);
        let body = MessageBody {
            text: Some("Hello".into()),
            provenance: mailrs_domain::Provenance {
                mailed_by: Some("bounce.example.net".into()),
                signed_by: Some("example.net".into()),
                encrypted: Some(true),
            },
            ..Default::default()
        };
        let no_thumbs = HashMap::new();
        let html = page(
            "x",
            vec![MessageView {
                meta: &m,
                body: BodyState::Loaded(&body),
                expanded: true,
                thumbnails: &no_thumbs,
                sanitized: None,
            }],
        );
        assert!(html.contains("<details class=\"line to\">"), "it opens");
        assert!(html.contains("<th>mailed-by</th><td>bounce.example.net</td>"));
        assert!(html.contains("<th>signed-by</th><td>example.net</td>"));
        assert!(html.contains("Standard encryption (TLS)"));
        assert!(html.contains("<th>subject</th>"));
        // The page carries no JavaScript, so the panel must open on its own.
        assert!(!html.contains("mailrs:details"));
    }

    #[test]
    fn the_panel_opens_on_a_message_whose_origins_are_unknown() {
        let m = meta("m1", "Ann", &[]);
        let body = MessageBody {
            text: Some("Hello".into()),
            ..Default::default()
        };
        let no_thumbs = HashMap::new();
        let html = page(
            "x",
            vec![MessageView {
                meta: &m,
                body: BodyState::Loaded(&body),
                expanded: true,
                thumbnails: &no_thumbs,
                sanitized: None,
            }],
        );
        // From, to, date and subject come off the metadata, so they are
        // there whatever the headers did or did not say.
        assert!(html.contains("<details class=\"line to\">"));
        assert!(html.contains("<th>subject</th>"));
        assert!(!html.contains("mailed-by"), "nothing invented");
        assert!(!html.contains("signed-by"));
        assert!(!html.contains("encryption"));
    }

    #[test]
    fn mail_that_crossed_the_internet_in_the_clear_says_so() {
        let m = meta("m1", "Ann", &[]);
        let body = MessageBody {
            text: Some("Hello".into()),
            provenance: mailrs_domain::Provenance {
                encrypted: Some(false),
                ..Default::default()
            },
            ..Default::default()
        };
        let no_thumbs = HashMap::new();
        let html = page(
            "x",
            vec![MessageView {
                meta: &m,
                body: BodyState::Loaded(&body),
                expanded: true,
                thumbnails: &no_thumbs,
                sanitized: None,
            }],
        );
        assert!(html.contains("Not encrypted in transit"));
    }

    #[test]
    fn a_photo_with_a_content_id_and_no_html_still_gets_a_row() {
        let m = meta("m1", "Ann", &[]);
        let body = MessageBody {
            text: Some("Here is the photo.".into()),
            html: None,
            attachments: vec![Attachment {
                part_id: "2".into(),
                filename: "cat.png".into(),
                mime_type: "image/png".into(),
                size: 4096,
                attachment_id: Some("att-1".into()),
                content_id: Some("img1@mailrs".into()),
            }],
            ..Default::default()
        };
        let no_thumbs = HashMap::new();
        let html = page(
            "x",
            vec![MessageView {
                meta: &m,
                body: BodyState::Loaded(&body),
                expanded: true,
                thumbnails: &no_thumbs,
                sanitized: None,
            }],
        );
        assert!(html.contains("cat.png"));
    }

    #[test]
    fn loading_and_failure_states_render() {
        let m = meta("m1", "Ann", &[]);
        let no_thumbs = HashMap::new();
        let loading = page(
            "x",
            vec![MessageView {
                meta: &m,
                body: BodyState::Loading,
                expanded: true,
                thumbnails: &no_thumbs,
                sanitized: None,
            }],
        );
        assert!(loading.contains("Loading…"));
        let failed = page(
            "x",
            vec![MessageView {
                meta: &m,
                body: BodyState::Failed("offline <now>"),
                expanded: true,
                thumbnails: &no_thumbs,
                sanitized: None,
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
    fn remote_content_is_refused_until_allowed() {
        let me: [String; 0] = [];
        let photos = HashMap::new();
        let blocked = render(
            &Conversation {
                subject: "x",
                messages: vec![],
                me: &me,
                photos: &photos,
                allow_remote: false,
            },
            &theme(),
        );
        assert!(
            blocked.contains("img-src data: mailrs-cid:;") && blocked.contains("script-src 'none'"),
            "{blocked}"
        );
        let allowed = render(
            &Conversation {
                subject: "x",
                messages: vec![],
                me: &me,
                photos: &photos,
                allow_remote: true,
            },
            &theme(),
        );
        assert!(allowed.contains("img-src data: mailrs-cid: https: http:"));
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
