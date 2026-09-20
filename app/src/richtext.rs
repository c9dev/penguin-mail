//! The rich body: what the composer holds while you write, and what it
//! turns into on the way out.
//!
//! A rich body is a list of blocks, each a line with a kind (paragraph,
//! heading, list item, quote, code) and the styled runs of text on it.
//! Nothing here touches GTK: the composer maps the text buffer's tags onto
//! these blocks and back, so the HTML the reader gets, the plain text part
//! beside it, and the Markdown a writer can switch to all come from one
//! place and are covered by unit tests.

use mailrs_gmail::convert::unescape_snippet;
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use serde::{Deserialize, Serialize};

/// What a line is: a paragraph unless the writer made it something else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BlockKind {
    #[default]
    Paragraph,
    /// A heading, level 1 to 3.
    Heading(u8),
    Bullet,
    Numbered,
    Quote,
    /// A line of a code block.
    Code,
}

/// The character styles a run of text can carry at once.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
}

/// A run of text that shares one style, one link, or one image.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Span {
    pub text: String,
    pub style: Style,
    /// Where the run links to.
    pub link: Option<String>,
    /// An image's source, such as `cid:…`. Its `text` is the alt text.
    pub image: Option<String>,
}

impl Span {
    pub fn plain(text: impl Into<String>) -> Span {
        Span {
            text: text.into(),
            style: Style::default(),
            link: None,
            image: None,
        }
    }

    pub fn image(alt: impl Into<String>, src: impl Into<String>) -> Span {
        Span {
            image: Some(src.into()),
            ..Span::plain(alt)
        }
    }
}

/// One line of the body.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Block {
    pub kind: BlockKind,
    pub spans: Vec<Span>,
}

impl Block {
    pub fn new(kind: BlockKind, spans: Vec<Span>) -> Block {
        Block { kind, spans }
    }

    /// The line's text with every style dropped.
    pub fn text(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }

    fn is_blank(&self) -> bool {
        self.spans
            .iter()
            .all(|s| s.image.is_none() && s.text.trim().is_empty())
    }
}

/// The composer's formatted text: its lines, in order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RichBody {
    pub blocks: Vec<Block>,
}

/// The inline styles the HTML carries. Many mail clients drop a `<style>`
/// block, so every rule sits on its element.
const FONT: &str = "font-family:-apple-system,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;font-size:14px;line-height:1.5";
pub(crate) const PARAGRAPH: &str = "margin:0 0 1em";
pub(crate) const QUOTE: &str =
    "margin:0 0 0 0.8ex;border-left:2px solid #ccc;padding-left:1ex;color:#555";
pub(crate) const PRE: &str = "background:#f6f6f8;padding:10px;border-radius:6px;overflow:auto";
pub(crate) const IMAGE: &str = "max-width:100%;height:auto";
const CODE: &str = "font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;background:#f6f6f8;padding:1px 3px;border-radius:4px";

/// Wraps a message body in the font every part of the app sends.
pub fn document(body: &str) -> String {
    format!("<div style=\"{FONT}\">{body}</div>")
}

pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

impl RichBody {
    pub fn is_empty(&self) -> bool {
        self.blocks.iter().all(Block::is_blank)
    }

    /// The body as email HTML.
    pub fn to_html(&self) -> String {
        let mut out = String::new();
        let mut rest = self.blocks.as_slice();
        while let Some(first) = rest.first() {
            let kind = first.kind;
            let run = rest.iter().take_while(|b| b.kind == kind).count();
            let (group, tail) = rest.split_at(run);
            rest = tail;
            match kind {
                BlockKind::Heading(level) => {
                    for block in group {
                        let level = level.clamp(1, 3);
                        out.push_str(&format!(
                            "<h{level} style=\"margin:0 0 0.5em\">{}</h{level}>",
                            spans_to_html(&block.spans)
                        ));
                    }
                }
                BlockKind::Bullet | BlockKind::Numbered => {
                    let list = if kind == BlockKind::Bullet {
                        "ul"
                    } else {
                        "ol"
                    };
                    out.push_str(&format!(
                        "<{list} style=\"margin:0 0 1em;padding-left:1.4em\">"
                    ));
                    for block in group {
                        out.push_str(&format!("<li>{}</li>", spans_to_html(&block.spans)));
                    }
                    out.push_str(&format!("</{list}>"));
                }
                BlockKind::Quote => {
                    out.push_str(&format!(
                        "<blockquote style=\"{QUOTE}\"><p style=\"{PARAGRAPH}\">{}</p></blockquote>",
                        lines_to_html(group)
                    ));
                }
                BlockKind::Code => {
                    let text: Vec<String> = group.iter().map(|b| escape(&b.text())).collect();
                    out.push_str(&format!(
                        "<pre style=\"{PRE}\"><code>{}</code></pre>",
                        text.join("\n")
                    ));
                }
                BlockKind::Paragraph => {
                    // Blank lines end a paragraph; the rest join with breaks,
                    // the way a single newline reads in any mail client.
                    for run in split_on_blanks(group) {
                        out.push_str(&format!(
                            "<p style=\"{PARAGRAPH}\">{}</p>",
                            lines_to_html(run)
                        ));
                    }
                }
            }
        }
        document(&out)
    }

    /// The body as plain text, for the part beside the HTML: no markers
    /// around the words, and every link's target spelled out.
    pub fn to_plain(&self) -> String {
        let mut out: Vec<String> = Vec::new();
        let mut number = 0;
        for block in &self.blocks {
            let mut line = String::new();
            for span in &block.spans {
                match (&span.image, &span.link) {
                    (Some(_), _) => line.push_str(&format!("[{}]", span.text)),
                    (None, Some(url)) if url != &span.text && !url.starts_with("mailto:") => {
                        line.push_str(&format!("{} <{url}>", span.text));
                    }
                    _ => line.push_str(&span.text),
                }
            }
            if block.kind == BlockKind::Numbered {
                number += 1;
            } else {
                number = 0;
            }
            out.push(match block.kind {
                BlockKind::Bullet => format!("- {line}"),
                BlockKind::Numbered => format!("{number}. {line}"),
                BlockKind::Quote => format!("> {line}").trim_end().to_string(),
                _ => line,
            });
        }
        out.join("\n").trim_end().to_string()
    }

    /// The body as Markdown, for a writer who switches to it and for the
    /// draft Gmail keeps.
    pub fn to_markdown(&self) -> String {
        let mut out: Vec<String> = Vec::new();
        let mut number = 0;
        let mut fenced = false;
        for block in &self.blocks {
            if block.kind == BlockKind::Code && !fenced {
                out.push("```".into());
                fenced = true;
            }
            if block.kind != BlockKind::Code && fenced {
                out.push("```".into());
                fenced = false;
            }
            if block.kind == BlockKind::Numbered {
                number += 1;
            } else {
                number = 0;
            }
            if block.kind == BlockKind::Code {
                out.push(block.text());
                continue;
            }
            let body: String = block.spans.iter().map(span_to_markdown).collect();
            out.push(match block.kind {
                BlockKind::Heading(level) => {
                    format!("{} {body}", "#".repeat(level.clamp(1, 3) as usize))
                }
                BlockKind::Bullet => format!("- {body}"),
                BlockKind::Numbered => format!("{number}. {body}"),
                BlockKind::Quote => format!("> {body}").trim_end().to_string(),
                BlockKind::Paragraph | BlockKind::Code => body,
            });
        }
        if fenced {
            out.push("```".into());
        }
        out.join("\n")
    }

    /// Reads Markdown into blocks, which is what the Format Markdown
    /// command and every draft written as Markdown come in through.
    /// Nested lists flatten to one level.
    pub fn from_markdown(markdown: &str) -> RichBody {
        let mut builder = Builder::default();
        let options = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES;
        for event in Parser::new_ext(markdown, options) {
            builder.take(event);
        }
        builder.finish()
    }

    /// Reads email HTML back into blocks, which is how a draft saved in
    /// Gmail comes back as the writer left it.
    ///
    /// It knows the shape [`RichBody::to_html`] writes. Mail written
    /// anywhere else, such as Gmail's own composer, keeps its words and
    /// whatever styling it spells the same way, and markup this does not
    /// know becomes plain text rather than disappearing.
    pub fn from_html(html: &str) -> RichBody {
        let mut builder = HtmlBuilder::default();
        // ASCII lowercasing keeps byte offsets, so indexes into `lower`
        // fit `html`.
        let lower = html.to_ascii_lowercase();
        let mut i = 0;
        while i < html.len() {
            let Some(offset) = html[i..].find('<') else {
                builder.text(&html[i..]);
                break;
            };
            builder.text(&html[i..i + offset]);
            let start = i + offset;
            let Some(length) = html[start..].find('>') else {
                // A `<` with nothing closing it is text, not a tag.
                builder.text(&html[start..]);
                break;
            };
            let end = start + length + 1;
            let inside = &lower[start + 1..end - 1];
            let closing = inside.starts_with('/');
            let name: String = inside
                .trim_start_matches('/')
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect();
            if name.is_empty() {
                // A comment, or a `<` the writer meant as a `<`. Reading
                // on from the next character finds the tags after it.
                if inside.starts_with('!') {
                    i = end;
                } else {
                    builder.text("<");
                    i = start + 1;
                }
                continue;
            }
            builder.tag(&name, closing, &html[start + 1..end - 1]);
            i = end;
        }
        builder.finish()
    }
}

fn span_to_markdown(span: &Span) -> String {
    if let Some(src) = &span.image {
        return format!("![{}]({src})", span.text);
    }
    let mut text = span.text.clone();
    if text.is_empty() {
        return text;
    }
    // Markers sit inside the run, so leading and trailing spaces stay out.
    let head: String = text.chars().take_while(|c| c.is_whitespace()).collect();
    let tail: String = text
        .chars()
        .rev()
        .take_while(|c| c.is_whitespace())
        .collect();
    if head.len() + tail.len() >= text.len() {
        return text;
    }
    text = text[head.len()..text.len() - tail.len()].to_string();
    if span.style.code {
        text = format!("`{text}`");
    }
    if span.style.bold {
        text = format!("**{text}**");
    }
    if span.style.italic {
        text = format!("*{text}*");
    }
    if span.style.strike {
        text = format!("~~{text}~~");
    }
    if let Some(url) = &span.link {
        text = format!("[{text}]({url})");
    }
    format!("{head}{text}{tail}")
}

fn spans_to_html(spans: &[Span]) -> String {
    let mut out = String::new();
    for span in spans {
        if let Some(src) = &span.image {
            out.push_str(&format!(
                "<img style=\"{IMAGE}\" src=\"{}\" alt=\"{}\" />",
                escape(src),
                escape(&span.text)
            ));
            continue;
        }
        let mut text = escape(&span.text);
        if span.style.code {
            text = format!("<code style=\"{CODE}\">{text}</code>");
        }
        if span.style.bold {
            text = format!("<strong>{text}</strong>");
        }
        if span.style.italic {
            text = format!("<em>{text}</em>");
        }
        if span.style.strike {
            text = format!("<del>{text}</del>");
        }
        if let Some(url) = &span.link {
            text = format!("<a href=\"{}\">{text}</a>", escape(url));
        }
        out.push_str(&text);
    }
    out
}

/// Several lines inside one HTML block, separated by breaks.
fn lines_to_html(blocks: &[Block]) -> String {
    blocks
        .iter()
        .map(|b| spans_to_html(&b.spans))
        .collect::<Vec<_>>()
        .join("<br />")
}

/// Groups of lines with the blank ones between them dropped.
fn split_on_blanks(blocks: &[Block]) -> Vec<&[Block]> {
    blocks
        .split(|b| b.is_blank())
        .filter(|run| !run.is_empty())
        .collect()
}

/// Builds blocks from Markdown events.
#[derive(Default)]
struct Builder {
    body: RichBody,
    spans: Vec<Span>,
    style: Style,
    link: Option<String>,
    /// The list kinds we are inside, innermost last.
    lists: Vec<BlockKind>,
    quoted: bool,
    /// The heading level or code block we are inside.
    block: Option<BlockKind>,
    open: bool,
}

impl Builder {
    fn kind(&self) -> BlockKind {
        if let Some(kind) = self.block {
            return kind;
        }
        if let Some(list) = self.lists.last() {
            return *list;
        }
        if self.quoted {
            return BlockKind::Quote;
        }
        BlockKind::Paragraph
    }

    fn push(&mut self, text: &str) {
        let span = Span {
            text: text.to_string(),
            style: self.style,
            link: self.link.clone(),
            image: None,
        };
        match self.spans.last_mut() {
            Some(last)
                if last.style == span.style && last.link == span.link && last.image.is_none() =>
            {
                last.text.push_str(text)
            }
            _ => self.spans.push(span),
        }
    }

    /// Ends the line being built.
    fn line(&mut self) {
        let kind = self.kind();
        let spans = std::mem::take(&mut self.spans);
        if spans.is_empty() && !self.open {
            return;
        }
        self.body.blocks.push(Block { kind, spans });
    }

    /// A blank line between blocks, unless one is already there.
    fn gap(&mut self) {
        if self
            .body
            .blocks
            .last()
            .is_some_and(|b| b.kind == BlockKind::Paragraph && b.is_blank())
        {
            return;
        }
        if !self.body.blocks.is_empty() {
            self.body.blocks.push(Block::default());
        }
    }

    fn take(&mut self, event: Event<'_>) {
        match event {
            Event::Start(Tag::Paragraph) | Event::Start(Tag::Item) => self.open = true,
            Event::End(TagEnd::Paragraph) => {
                self.line();
                self.open = false;
                if self.lists.is_empty() {
                    self.gap();
                }
            }
            Event::End(TagEnd::Item) => {
                if !self.spans.is_empty() || self.open {
                    self.line();
                }
                self.open = false;
            }
            Event::Start(Tag::Heading { level, .. }) => {
                self.block = Some(BlockKind::Heading(match level {
                    HeadingLevel::H1 => 1,
                    HeadingLevel::H2 => 2,
                    _ => 3,
                }));
                self.open = true;
            }
            Event::End(TagEnd::Heading(_)) => {
                self.line();
                self.open = false;
                self.block = None;
                self.gap();
            }
            Event::Start(Tag::List(start)) => {
                self.lists.push(match start {
                    Some(_) => BlockKind::Numbered,
                    None => BlockKind::Bullet,
                });
            }
            Event::End(TagEnd::List(_)) => {
                self.lists.pop();
                if self.lists.is_empty() {
                    self.gap();
                }
            }
            Event::Start(Tag::BlockQuote(_)) => self.quoted = true,
            Event::End(TagEnd::BlockQuote(_)) => {
                self.quoted = false;
                self.gap();
            }
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(_) | CodeBlockKind::Indented)) => {
                self.block = Some(BlockKind::Code);
                self.open = true;
            }
            Event::End(TagEnd::CodeBlock) => {
                if !self.spans.is_empty() || self.open {
                    self.line();
                }
                self.open = false;
                self.block = None;
                self.gap();
            }
            Event::Start(Tag::Strong) => self.style.bold = true,
            Event::End(TagEnd::Strong) => self.style.bold = false,
            Event::Start(Tag::Emphasis) => self.style.italic = true,
            Event::End(TagEnd::Emphasis) => self.style.italic = false,
            Event::Start(Tag::Strikethrough) => self.style.strike = true,
            Event::End(TagEnd::Strikethrough) => self.style.strike = false,
            Event::Start(Tag::Link { dest_url, .. }) => self.link = Some(dest_url.to_string()),
            Event::End(TagEnd::Link) => self.link = None,
            Event::Start(Tag::Image { dest_url, .. }) => {
                self.spans.push(Span::image("", dest_url.to_string()));
            }
            Event::End(TagEnd::Image) => {}
            Event::Text(text) => {
                // Inside an image, the text is its alt text.
                if let Some(last) = self.spans.last_mut()
                    && last.image.is_some()
                    && last.text.is_empty()
                {
                    last.text = text.to_string();
                    return;
                }
                // A fenced block's text ends in a newline that closes it,
                // not a line of its own.
                let text = match self.block {
                    Some(BlockKind::Code) => text.strip_suffix('\n').unwrap_or(&text).to_string(),
                    _ => text.to_string(),
                };
                let mut lines = text.split('\n').peekable();
                while let Some(line) = lines.next() {
                    self.push(line);
                    if lines.peek().is_some() {
                        self.line();
                    }
                }
            }
            Event::Code(text) => {
                let was = self.style.code;
                self.style.code = true;
                self.push(&text);
                self.style.code = was;
            }
            Event::SoftBreak | Event::HardBreak => self.line(),
            Event::Rule => {
                self.line();
                self.body
                    .blocks
                    .push(Block::new(BlockKind::Paragraph, vec![Span::plain("———")]));
            }
            Event::Html(text) | Event::InlineHtml(text) => self.push(&text),
            _ => {}
        }
    }

    fn finish(mut self) -> RichBody {
        self.line();
        while self
            .body
            .blocks
            .last()
            .is_some_and(|b| b.kind == BlockKind::Paragraph && b.is_blank())
        {
            self.body.blocks.pop();
        }
        self.body
    }
}

/// What a tag opens, for the tags that hold lines of their own.
fn container(name: &str) -> Option<BlockKind> {
    Some(match name {
        "ul" => BlockKind::Bullet,
        "ol" => BlockKind::Numbered,
        "blockquote" => BlockKind::Quote,
        "pre" => BlockKind::Code,
        _ => return None,
    })
}

/// Tags that end the line they sit on.
fn breaks_line(name: &str) -> bool {
    matches!(
        name,
        "p" | "div" | "li" | "tr" | "td" | "table" | "section" | "article" | "hr" | "dt" | "dd"
    ) || heading_level(name).is_some()
        || container(name).is_some()
}

fn heading_level(name: &str) -> Option<u8> {
    match name {
        "h1" => Some(1),
        "h2" => Some(2),
        "h3" | "h4" | "h5" | "h6" => Some(3),
        _ => None,
    }
}

/// The value `wanted` has in a tag's text, quoted or bare.
fn attribute(tag: &str, wanted: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(offset) = lower[from..].find(wanted) {
        let at = from + offset;
        from = at + wanted.len();
        // Inside a longer name, such as `data-src`, it is a different word.
        if lower[..at]
            .chars()
            .last()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            continue;
        }
        let Some(rest) = tag[from..].trim_start().strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim_start();
        let value = match rest.chars().next() {
            Some(quote @ ('"' | '\'')) => {
                let rest = &rest[1..];
                &rest[..rest.find(quote).unwrap_or(rest.len())]
            }
            _ => &rest[..rest.find(char::is_whitespace).unwrap_or(rest.len())],
        };
        return Some(unescape_snippet(value));
    }
    None
}

/// One more level deep, or one less.
fn step(depth: usize, back: bool) -> usize {
    match back {
        true => depth.saturating_sub(1),
        false => depth + 1,
    }
}

/// Builds blocks from HTML tags and the text between them.
#[derive(Default)]
struct HtmlBuilder {
    body: RichBody,
    spans: Vec<Span>,
    /// How deep we are inside each style, since tags nest.
    bold: usize,
    italic: usize,
    strike: usize,
    code: usize,
    links: Vec<String>,
    /// The lists, quotes and code blocks we are inside, innermost last.
    open: Vec<BlockKind>,
    heading: Option<u8>,
    /// Inside `<pre>`, where the spaces and the line breaks are the text.
    pre: usize,
    /// A tag whose content the reader never sees, such as `<style>`.
    hidden: Option<String>,
}

impl HtmlBuilder {
    fn kind(&self) -> BlockKind {
        if let Some(level) = self.heading {
            return BlockKind::Heading(level);
        }
        self.open.last().copied().unwrap_or(BlockKind::Paragraph)
    }

    fn style(&self) -> Style {
        Style {
            bold: self.bold > 0,
            italic: self.italic > 0,
            strike: self.strike > 0,
            code: self.code > 0 && self.pre == 0,
        }
    }

    fn ends_with_space(&self) -> bool {
        self.spans
            .last()
            .is_some_and(|span| span.text.ends_with(' '))
    }

    fn push(&mut self, text: &str) {
        let span = Span {
            text: text.to_string(),
            style: self.style(),
            link: self.links.last().cloned(),
            image: None,
        };
        match self.spans.last_mut() {
            Some(last)
                if last.style == span.style && last.link == span.link && last.image.is_none() =>
            {
                last.text.push_str(text)
            }
            _ => self.spans.push(span),
        }
    }

    /// Ends the line being built. Inside `<pre>` a blank line is a line.
    fn line(&mut self) {
        if self.spans.is_empty() && self.pre == 0 {
            return;
        }
        let kind = self.kind();
        let spans = std::mem::take(&mut self.spans);
        self.body.blocks.push(Block { kind, spans });
    }

    /// A blank line between blocks, unless one is already there.
    fn gap(&mut self) {
        if !self.open.is_empty() || self.body.blocks.is_empty() {
            return;
        }
        if self
            .body
            .blocks
            .last()
            .is_some_and(|b| b.kind == BlockKind::Paragraph && b.spans.is_empty())
        {
            return;
        }
        self.body.blocks.push(Block::default());
    }

    fn text(&mut self, raw: &str) {
        if self.hidden.is_some() || raw.is_empty() {
            return;
        }
        let decoded = unescape_snippet(raw);
        if self.pre > 0 {
            let mut lines = decoded.split('\n').peekable();
            while let Some(line) = lines.next() {
                if !line.is_empty() {
                    self.push(line);
                }
                if lines.peek().is_some() {
                    self.line();
                }
            }
            return;
        }
        // Outside `<pre>`, any run of spaces and line breaks is one space.
        let mut collapsed = String::with_capacity(decoded.len());
        let mut space = false;
        for character in decoded.chars() {
            if character.is_whitespace() {
                space = true;
                continue;
            }
            if space && !collapsed.is_empty() {
                collapsed.push(' ');
            }
            space = false;
            collapsed.push(character);
        }
        let leading = decoded.starts_with(char::is_whitespace);
        let room = !self.spans.is_empty() && !self.ends_with_space();
        if collapsed.is_empty() {
            // Space between two tags belongs to the words around it.
            if room {
                self.push(" ");
            }
            return;
        }
        if leading && room {
            self.push(" ");
        }
        self.push(&collapsed);
        if space {
            self.push(" ");
        }
    }

    fn tag(&mut self, name: &str, closing: bool, tag: &str) {
        if let Some(hidden) = self.hidden.clone() {
            if closing && hidden == name {
                self.hidden = None;
            }
            return;
        }
        match name {
            "style" | "script" | "head" | "title" | "noscript" if !closing => {
                self.hidden = Some(name.to_string());
                return;
            }
            "br" => {
                self.line();
                return;
            }
            "img" if !closing => {
                if let Some(source) = attribute(tag, "src") {
                    let alt = attribute(tag, "alt").unwrap_or_default();
                    self.spans.push(Span::image(alt, source));
                }
                return;
            }
            "b" | "strong" => self.bold = step(self.bold, closing),
            "i" | "em" => self.italic = step(self.italic, closing),
            "s" | "del" | "strike" => self.strike = step(self.strike, closing),
            "code" | "tt" | "kbd" | "samp" => self.code = step(self.code, closing),
            "a" => match closing {
                true => {
                    self.links.pop();
                }
                false => {
                    if let Some(href) = attribute(tag, "href") {
                        self.links.push(href);
                    }
                }
            },
            _ => {}
        }
        if !breaks_line(name) {
            return;
        }
        self.line();
        if let Some(level) = heading_level(name) {
            match closing {
                true => {
                    self.heading = None;
                    self.gap();
                }
                false => self.heading = Some(level),
            }
            return;
        }
        if let Some(kind) = container(name) {
            match closing {
                true => {
                    if let Some(at) = self.open.iter().rposition(|open| *open == kind) {
                        self.open.remove(at);
                    }
                    if kind == BlockKind::Code {
                        self.pre = step(self.pre, true);
                    }
                    self.gap();
                }
                false => {
                    self.open.push(kind);
                    if kind == BlockKind::Code {
                        self.pre += 1;
                    }
                }
            }
            return;
        }
        // A paragraph leaves a blank line behind it; a div or a table row
        // does not, because mail clients write one of those per line.
        if name == "p" && closing {
            self.gap();
        }
    }

    fn finish(mut self) -> RichBody {
        self.pre = 0;
        self.line();
        while self
            .body
            .blocks
            .last()
            .is_some_and(|b| b.kind == BlockKind::Paragraph && b.spans.is_empty())
        {
            self.body.blocks.pop();
        }
        for block in &mut self.body.blocks {
            if block.kind == BlockKind::Code {
                continue;
            }
            // Spaces that only came from the markup read as ragged text.
            if let Some(first) = block.spans.first_mut() {
                first.text = first.text.trim_start().to_string();
            }
            if let Some(last) = block.spans.last_mut() {
                last.text = last.text.trim_end().to_string();
            }
            block
                .spans
                .retain(|span| !span.text.is_empty() || span.image.is_some());
        }
        self.body
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bold(text: &str) -> Span {
        Span {
            style: Style {
                bold: true,
                ..Style::default()
            },
            ..Span::plain(text)
        }
    }

    fn linked(text: &str, url: &str) -> Span {
        Span {
            link: Some(url.into()),
            ..Span::plain(text)
        }
    }

    fn body(blocks: Vec<Block>) -> RichBody {
        RichBody { blocks }
    }

    #[test]
    fn styled_runs_become_html() {
        let doc = body(vec![Block::new(
            BlockKind::Paragraph,
            vec![
                Span::plain("Hi "),
                bold("Ann"),
                Span::plain(", see "),
                linked("the menu", "https://example.com/menu"),
            ],
        )]);
        let html = doc.to_html();
        assert!(html.contains("<strong>Ann</strong>"), "{html}");
        assert!(
            html.contains("<a href=\"https://example.com/menu\">the menu</a>"),
            "{html}"
        );
        assert_eq!(
            doc.to_plain(),
            "Hi Ann, see the menu <https://example.com/menu>"
        );
    }

    #[test]
    fn lists_quotes_and_code_each_get_their_own_html_block() {
        let doc = body(vec![
            Block::new(BlockKind::Heading(2), vec![Span::plain("Order")]),
            Block::new(BlockKind::Bullet, vec![Span::plain("milk")]),
            Block::new(BlockKind::Bullet, vec![Span::plain("eggs")]),
            Block::new(BlockKind::Numbered, vec![Span::plain("first")]),
            Block::new(BlockKind::Numbered, vec![Span::plain("second")]),
            Block::new(BlockKind::Quote, vec![Span::plain("she said")]),
            Block::new(BlockKind::Code, vec![Span::plain("cargo test")]),
        ]);
        let html = doc.to_html();
        assert!(
            html.contains("<h2 style=\"margin:0 0 0.5em\">Order</h2>"),
            "{html}"
        );
        assert!(html.contains("<li>milk</li><li>eggs</li></ul>"), "{html}");
        assert!(
            html.contains("<li>first</li><li>second</li></ol>"),
            "{html}"
        );
        assert!(html.contains("<blockquote style="), "{html}");
        assert!(html.contains("<pre style="), "{html}");
        assert_eq!(
            doc.to_plain(),
            "Order\n- milk\n- eggs\n1. first\n2. second\n> she said\ncargo test"
        );
    }

    #[test]
    fn a_blank_line_ends_a_paragraph_and_a_single_break_stays_a_break() {
        let doc = body(vec![
            Block::new(BlockKind::Paragraph, vec![Span::plain("One")]),
            Block::new(BlockKind::Paragraph, vec![Span::plain("still one")]),
            Block::default(),
            Block::new(BlockKind::Paragraph, vec![Span::plain("Two")]),
        ]);
        let html = doc.to_html();
        assert!(html.contains("One<br />still one</p>"), "{html}");
        assert!(
            html.contains("<p style=\"margin:0 0 1em\">Two</p>"),
            "{html}"
        );
    }

    #[test]
    fn html_escapes_what_the_writer_typed() {
        let doc = body(vec![Block::new(
            BlockKind::Paragraph,
            vec![Span::plain("a < b & \"c\"")],
        )]);
        assert!(
            doc.to_html().contains("a &lt; b &amp; &quot;c&quot;"),
            "{}",
            doc.to_html()
        );
    }

    #[test]
    fn markdown_becomes_blocks() {
        let doc = RichBody::from_markdown(
            "# Title\n\nHi **Ann**, see [the menu](https://e.com).\nSecond line.\n\n- milk\n- eggs\n\n> quoted\n\n1. one\n2. two",
        );
        let kinds: Vec<BlockKind> = doc.blocks.iter().map(|b| b.kind).collect();
        assert_eq!(
            kinds,
            [
                BlockKind::Heading(1),
                BlockKind::Paragraph,
                BlockKind::Paragraph,
                BlockKind::Paragraph,
                BlockKind::Paragraph,
                BlockKind::Bullet,
                BlockKind::Bullet,
                BlockKind::Paragraph,
                BlockKind::Quote,
                BlockKind::Paragraph,
                BlockKind::Numbered,
                BlockKind::Numbered,
            ],
            "{doc:#?}"
        );
        assert_eq!(doc.blocks[2].spans[1], bold("Ann"));
        assert_eq!(doc.blocks[2].spans[3], linked("the menu", "https://e.com"));
        assert_eq!(doc.blocks[3].text(), "Second line.");
    }

    #[test]
    fn markdown_round_trips_through_blocks() {
        let markdown = "# Title\n\nHi **Ann**, see [the menu](https://e.com).\nSecond *line*.\n\n- milk\n- ~~eggs~~\n\n> quoted\n\n1. one\n2. two\n\n```\ncargo test\n```\n\nBye.";
        let once = RichBody::from_markdown(markdown);
        let again = RichBody::from_markdown(&once.to_markdown());
        assert_eq!(once, again, "{}", once.to_markdown());
        assert_eq!(once.to_html(), again.to_html());
    }

    #[test]
    fn markdown_and_rich_html_agree_on_the_same_body() {
        let markdown = "Hi **Ann**\n\n- milk\n- eggs";
        let rich = RichBody::from_markdown(markdown).to_html();
        assert!(rich.contains("<strong>Ann</strong>"), "{rich}");
        assert!(rich.contains("<li>milk</li>"), "{rich}");
        assert!(!rich.contains("**"), "{rich}");
    }

    #[test]
    fn an_image_keeps_its_source_through_every_form() {
        let doc = body(vec![Block::new(
            BlockKind::Paragraph,
            vec![Span::plain("Look: "), Span::image("map", "cid:map1@mailrs")],
        )]);
        assert!(
            doc.to_html().contains("src=\"cid:map1@mailrs\""),
            "{}",
            doc.to_html()
        );
        assert_eq!(doc.to_plain(), "Look: [map]");
        assert_eq!(doc.to_markdown(), "Look: ![map](cid:map1@mailrs)");
        assert_eq!(RichBody::from_markdown(&doc.to_markdown()), doc);
    }

    fn written_body() -> RichBody {
        body(vec![
            Block::new(BlockKind::Heading(2), vec![Span::plain("Order")]),
            Block::default(),
            Block::new(
                BlockKind::Paragraph,
                vec![
                    Span::plain("Hi "),
                    bold("Ann"),
                    Span::plain(", see "),
                    linked("the menu", "https://e.com/menu"),
                    Span::plain("."),
                ],
            ),
            Block::new(BlockKind::Paragraph, vec![Span::plain("Friday works.")]),
            Block::default(),
            Block::new(BlockKind::Bullet, vec![Span::plain("soup")]),
            Block::new(BlockKind::Bullet, vec![Span::plain("salad")]),
            Block::default(),
            Block::new(BlockKind::Numbered, vec![Span::plain("first")]),
            Block::new(BlockKind::Numbered, vec![Span::plain("second")]),
            Block::default(),
            Block::new(BlockKind::Quote, vec![Span::plain("she said")]),
            Block::new(BlockKind::Quote, vec![Span::plain("and then some")]),
            Block::default(),
            Block::new(BlockKind::Code, vec![Span::plain("cargo test")]),
            Block::new(BlockKind::Code, vec![Span::plain("cargo fmt")]),
            Block::default(),
            Block::new(
                BlockKind::Paragraph,
                vec![Span::plain("Bye. "), Span::image("map", "cid:map1@mailrs")],
            ),
        ])
    }

    #[test]
    fn a_body_comes_back_from_its_own_html() {
        let wanted = written_body();
        let read_back = RichBody::from_html(&wanted.to_html());
        assert_eq!(read_back, wanted, "{}", read_back.to_markdown());
        assert_eq!(read_back.to_html(), wanted.to_html());
    }

    #[test]
    fn styled_words_survive_the_html() {
        let body = RichBody::from_html(
            "<div><p>Hi <strong>Ann</strong>, <em>Friday</em> <del>or Monday</del> \
             works. See <a href=\"https://e.com?a=1&amp;b=2\">the menu</a>.</p></div>",
        );
        assert_eq!(
            body.to_plain(),
            "Hi Ann, Friday or Monday works. See the menu <https://e.com?a=1&b=2>."
        );
        assert_eq!(body.blocks[0].spans[1], bold("Ann"));
        assert!(body.blocks[0].spans[3].style.italic);
        assert!(body.blocks[0].spans[5].style.strike);
    }

    #[test]
    fn html_from_another_client_opens_sensibly() {
        // What Gmail's own composer writes, more or less.
        let body = RichBody::from_html(
            "<div dir=\"ltr\"><div>Hi&nbsp;Ann,</div><div><br></div>\
             <div>Bringing <b>soup</b> and <i>salad</i>.</div>\
             <div><span style=\"color:#111\">See you Friday.</span></div>\
             <ul><li>one</li><li>two</li></ul></div>",
        );
        assert_eq!(
            body.to_plain(),
            "Hi Ann,\nBringing soup and salad.\nSee you Friday.\n- one\n- two"
        );
        assert!(body.blocks[1].spans[1].style.bold);
    }

    #[test]
    fn markup_it_does_not_know_still_reads_as_text() {
        let body = RichBody::from_html(
            "<html><head><style>p{color:red}</style></head><body>\
             <table><tr><td>Left</td><td>Right</td></tr></table>\
             <marquee>Moving on</marquee><p>a &lt; b &amp; c</p>\
             <script>alert('x')</script></body></html>",
        );
        let plain = body.to_plain();
        assert!(plain.contains("Left"), "{plain}");
        assert!(plain.contains("Moving on"), "{plain}");
        assert!(plain.contains("a < b & c"), "{plain}");
        assert!(!plain.contains("color:red"), "{plain}");
        assert!(!plain.contains("alert"), "{plain}");
    }

    #[test]
    fn broken_html_neither_panics_nor_loses_the_words() {
        for html in [
            "",
            "<",
            "<p>unclosed",
            "no tags at all",
            "<p>a < b</p>",
            "<b><i>deep</b></i>",
            "<a href>bare</a>",
            "<img>",
            "<pre>code without an end",
            "</p></div></ul>",
            "<p>&amp;&#x1F600; &notreal;</p>",
        ] {
            let body = RichBody::from_html(html);
            let _ = body.to_html();
            let _ = body.to_markdown();
        }
        assert_eq!(RichBody::from_html("<p>unclosed").to_plain(), "unclosed");
        assert_eq!(RichBody::from_html("<p>a < b</p>").to_plain(), "a < b");
        assert!(RichBody::from_html("").is_empty());
    }

    #[test]
    fn an_empty_body_knows_it_is_empty() {
        assert!(RichBody::default().is_empty());
        assert!(RichBody::from_markdown("   \n\n").is_empty());
        assert!(!RichBody::from_markdown("hi").is_empty());
    }
}
