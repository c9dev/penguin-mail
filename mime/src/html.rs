//! HTML read without a browser: the text a reader would see, and the tags
//! around it.
//!
//! Scanning for `<` and `>` by hand stops at the first attribute whose
//! value holds a `>`, such as `title="a > b"`, and reads the rest of the
//! tag as words. [`walk`] runs html5ever's tokenizer instead, which knows
//! attribute quoting, comments, character references, and the raw text
//! inside `<script>` and `<style>`.

use std::cell::RefCell;

use html5ever::Attribute;
use html5ever::tendril::StrTendril;
use html5ever::tokenizer::states::RawKind;
use html5ever::tokenizer::{
    BufferQueue, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
};

/// One tag, as the tokenizer read it.
pub struct Tag<'a> {
    /// The name in lower case.
    pub name: &'a str,
    /// An end tag, such as `</p>`.
    pub closing: bool,
    attributes: &'a [Attribute],
}

impl Tag<'_> {
    /// The value of the attribute `name`, with its character references
    /// decoded. An attribute written with no value has an empty one.
    pub fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|a| &*a.name.local == name)
            .map(|a| &*a.value)
    }

    /// Every attribute value on the tag.
    pub fn values(&self) -> impl Iterator<Item = &str> {
        self.attributes.iter().map(|a| &*a.value)
    }
}

/// A piece of an HTML document, in the order it comes.
pub enum Piece<'a> {
    /// A run of text between two tags, character references decoded.
    Text(&'a str),
    Tag(Tag<'a>),
}

/// Hands every run of text and every tag in `html` to `visit`, in order.
/// Comments and the doctype are left out, and so is nothing else: the
/// text inside `<script>` and `<style>` arrives as text, for the caller
/// to drop.
pub fn walk(html: &str, visit: impl FnMut(Piece<'_>)) {
    let sink = Sink {
        visit: RefCell::new(visit),
        text: RefCell::new(String::new()),
    };
    let tokenizer = Tokenizer::new(sink, TokenizerOpts::default());
    let input = BufferQueue::default();
    input.push_back(StrTendril::from_slice(html));
    let _ = tokenizer.feed(&input);
    tokenizer.end();
    tokenizer.sink.flush();
}

struct Sink<F> {
    visit: RefCell<F>,
    /// Text gathered since the last tag. The tokenizer hands text over in
    /// pieces, split at references and line breaks, and a caller wants the
    /// run whole.
    text: RefCell<String>,
}

impl<F: FnMut(Piece<'_>)> Sink<F> {
    fn flush(&self) {
        let text = std::mem::take(&mut *self.text.borrow_mut());
        if !text.is_empty() {
            (self.visit.borrow_mut())(Piece::Text(&text));
        }
    }
}

impl<F: FnMut(Piece<'_>)> TokenSink for Sink<F> {
    type Handle = ();

    fn process_token(&self, token: Token, _line: u64) -> TokenSinkResult<()> {
        match token {
            Token::CharacterTokens(text) => self.text.borrow_mut().push_str(&text),
            Token::TagToken(tag) => {
                self.flush();
                let opening = tag.kind == TagKind::StartTag;
                (self.visit.borrow_mut())(Piece::Tag(Tag {
                    name: &tag.name,
                    closing: !opening,
                    attributes: &tag.attrs,
                }));
                // Without a tree builder the tokenizer has nobody to tell it
                // that a script's text is not markup, so the sink does.
                if opening {
                    let raw = match &*tag.name {
                        "script" => Some(RawKind::ScriptData),
                        "style" | "xmp" | "iframe" | "noembed" | "noframes" => {
                            Some(RawKind::Rawtext)
                        }
                        "title" | "textarea" => Some(RawKind::Rcdata),
                        _ => None,
                    };
                    if let Some(raw) = raw {
                        return TokenSinkResult::RawData(raw);
                    }
                }
            }
            Token::EOFToken => self.flush(),
            _ => {}
        }
        TokenSinkResult::Continue
    }
}

/// Tags whose content a reader never sees.
fn hidden(name: &str) -> bool {
    matches!(
        name,
        "head" | "title" | "style" | "script" | "noscript" | "template"
    )
}

/// Tags that start and end a line of their own.
fn block(name: &str) -> bool {
    matches!(
        name,
        "div"
            | "li"
            | "tr"
            | "ul"
            | "ol"
            | "table"
            | "blockquote"
            | "pre"
            | "section"
            | "article"
            | "header"
            | "footer"
            | "hr"
            | "dt"
            | "dd"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
    )
}

/// Plain text from HTML, the way a reader would take it in: what a browser
/// hides is dropped, a block or a `<br>` ends a line, a paragraph has a
/// blank line on either side, and runs of spaces and line breaks in the source
/// are one space, except inside `<pre>`.
pub fn html_to_text(html: &str) -> String {
    let mut text = Text::default();
    walk(html, |piece| match piece {
        Piece::Text(words) => text.words(words),
        Piece::Tag(tag) => text.tag(&tag),
    });
    text.finish()
}

#[derive(Default)]
struct Text {
    out: String,
    /// How deep inside tags whose content is hidden.
    hidden: usize,
    /// How deep inside `<pre>`.
    pre: usize,
    /// Whitespace came since the last word, and a space is owed before the
    /// next one.
    space: bool,
}

impl Text {
    fn words(&mut self, words: &str) {
        if self.hidden > 0 {
            return;
        }
        if self.pre > 0 {
            self.out.push_str(&words.replace('\u{a0}', " "));
            return;
        }
        for character in words.chars() {
            // A non-breaking space reads as a space.
            if character.is_whitespace() {
                self.space = true;
                continue;
            }
            if self.space && !self.out.is_empty() && !self.out.ends_with('\n') {
                self.out.push(' ');
            }
            self.space = false;
            self.out.push(character);
        }
    }

    fn tag(&mut self, tag: &Tag<'_>) {
        if hidden(tag.name) {
            self.hidden = match tag.closing {
                true => self.hidden.saturating_sub(1),
                false => self.hidden + 1,
            };
            return;
        }
        if self.hidden > 0 {
            return;
        }
        match tag.name {
            "br" => self.push_break(),
            // A paragraph stands apart from what comes before and after
            // it, the way its margins set it apart in a browser.
            "p" => self.end_line(2),
            "pre" => {
                self.end_line(1);
                self.pre = match tag.closing {
                    true => self.pre.saturating_sub(1),
                    false => self.pre + 1,
                };
            }
            name if block(name) => self.end_line(1),
            _ => {}
        }
    }

    fn push_break(&mut self) {
        self.out.push('\n');
        self.space = false;
    }

    /// Ends the current line so the text finishes with `breaks` line
    /// breaks. The start of the text needs none.
    fn end_line(&mut self, breaks: usize) {
        self.space = false;
        if self.out.is_empty() {
            return;
        }
        let have = self.out.len() - self.out.trim_end_matches('\n').len();
        for _ in have..breaks {
            self.out.push('\n');
        }
    }

    fn finish(self) -> String {
        let mut text = String::with_capacity(self.out.len());
        let mut blank = 0;
        for line in self.out.lines().map(str::trim_end) {
            if line.is_empty() {
                blank += 1;
                // One blank line is a paragraph break; more say nothing.
                if blank > 1 || text.is_empty() {
                    continue;
                }
            } else {
                blank = 0;
            }
            text.push_str(line);
            text.push('\n');
        }
        text.trim_end().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gmail_html_turns_into_lines() {
        assert_eq!(
            html_to_text(
                r#"<div dir="ltr">Ann Lee<div>Maple &amp; Finch</div><div><br></div></div>"#
            ),
            "Ann Lee\nMaple & Finch"
        );
        assert_eq!(html_to_text("<p>One</p><p>Two</p>"), "One\n\nTwo");
        assert_eq!(html_to_text("plain"), "plain");
    }

    #[test]
    fn what_a_browser_hides_stays_out_of_the_text() {
        let text = html_to_text(
            "<html><head><title>T</title><style>p{color:red}</style></head><body>\
             <p>Hello&nbsp;there</p><div>Line <b>two</b><br>three</div>\
             <script>if (a<b) x()</script><!-- <p>note</p> --><p>&amp; four</p></body></html>",
        );
        assert_eq!(text, "Hello there\n\nLine two\nthree\n\n& four");
    }

    #[test]
    fn a_greater_than_sign_inside_an_attribute_ends_no_tag() {
        let text = html_to_text(
            r#"<div title="a > b" data-x='c > d'>Hello</div><img alt=">"><p>there</p>"#,
        );
        assert_eq!(text, "Hello\n\nthere");
    }

    #[test]
    fn line_breaks_in_the_source_are_spaces_except_in_pre() {
        assert_eq!(
            html_to_text("<p>one\n   two</p><pre>a\n  b</pre>"),
            "one two\n\na\n  b"
        );
    }

    #[test]
    fn a_tag_gives_its_attributes_decoded() {
        let mut found = Vec::new();
        walk(
            r#"<a href="https://e.com/?a=1&amp;b=2" title='x > y'>go</a>"#,
            |piece| {
                if let Piece::Tag(tag) = piece
                    && !tag.closing
                {
                    found.push(tag.attribute("href").map(String::from));
                    found.push(tag.attribute("title").map(String::from));
                    found.push(tag.attribute("rel").map(String::from));
                }
            },
        );
        assert_eq!(
            found,
            [
                Some("https://e.com/?a=1&b=2".to_string()),
                Some("x > y".to_string()),
                None
            ]
        );
    }
}
