//! The rich body: what the composer holds while you write, and what it
//! turns into on the way out.
//!
//! A rich body is a list of blocks, each a line with a kind (paragraph,
//! heading, list item, quote, code) and the styled runs of text on it.
//! Nothing here touches GTK: the composer maps the text buffer's tags onto
//! these blocks and back, so the HTML the reader gets, the plain text part
//! beside it, and the Markdown a writer can switch to all come from one
//! place and are covered by unit tests.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// What a line is: a paragraph unless the writer made it something else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
}

/// A run of text that shares one style, one link, or one image.
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq, Default)]
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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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

    #[test]
    fn an_empty_body_knows_it_is_empty() {
        assert!(RichBody::default().is_empty());
        assert!(RichBody::from_markdown("   \n\n").is_empty());
        assert!(!RichBody::from_markdown("hi").is_empty());
    }
}
