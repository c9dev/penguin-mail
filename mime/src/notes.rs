//! An event's description as a person reads and types it.
//!
//! Google Calendar keeps a description as HTML once someone has edited it
//! in Google's own editor (`<br>` for a line break, `<a href>` for a
//! link), and as the plain text another client wrote otherwise. Its web
//! page shows either one. [`text`] turns what Google holds into lines to
//! edit, with each link's address kept in the text, and [`html`] turns the
//! edited lines back into HTML Google shows with working links.

use std::fmt::Write as _;

use crate::html::{Piece, Tag, block, hidden, walk};

/// The description as lines to read and edit. Plain text comes back as
/// it is. HTML loses its tags: a `<br>`, a block or a raw line break ends
/// a line, and a link whose words are not an address gets its address
/// after them in brackets, so saving the lines keeps the link.
pub fn text(description: &str) -> String {
    if !is_html(description) {
        return description.to_string();
    }
    let mut lines = Lines::default();
    walk(description, |piece| match piece {
        Piece::Text(words) => lines.words(words),
        Piece::Tag(tag) => lines.tag(&tag),
    });
    lines.finish()
}

/// Edited lines as a description Google shows as they were typed: each
/// line break a `<br>`, each `http` or `https` address a link, and the
/// spaces HTML would fold kept as non-breaking ones. [`text`] reads it
/// back to the same lines.
pub fn html(text: &str) -> String {
    let lines: Vec<String> = text.split('\n').map(line_html).collect();
    lines.join("<br>")
}

/// Whether a description holds markup: a tag, a comment or a character
/// reference, anything the tokenizer reads as more than its own text.
fn is_html(description: &str) -> bool {
    let mut tags = false;
    let mut plain = String::with_capacity(description.len());
    walk(description, |piece| match piece {
        Piece::Text(words) => plain.push_str(words),
        Piece::Tag(_) => tags = true,
    });
    tags || plain != description
}

#[derive(Default)]
struct Lines {
    out: String,
    /// How deep inside tags whose content is hidden.
    hidden: usize,
    /// Whitespace came since the last word, and a space is owed before the
    /// next one.
    space: bool,
    /// The open link's address and where its words start in `out`.
    link: Option<(String, usize)>,
}

impl Lines {
    fn words(&mut self, words: &str) {
        if self.hidden > 0 {
            return;
        }
        for character in words.chars() {
            match character {
                '\n' => self.push_break(),
                // A non-breaking space is one the writer meant, so it stays.
                '\u{a0}' => {
                    self.owed_space();
                    self.out.push(' ');
                }
                c if c.is_whitespace() => self.space = true,
                c => {
                    self.owed_space();
                    self.out.push(c);
                }
            }
        }
    }

    fn owed_space(&mut self) {
        if self.space && !self.out.is_empty() && !self.out.ends_with('\n') {
            self.out.push(' ');
        }
        self.space = false;
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
        match (tag.name, tag.closing) {
            ("br", _) => self.push_break(),
            ("p", _) => self.end_line(2),
            ("li", false) => {
                self.end_line(1);
                self.out.push_str("- ");
            }
            ("a", false) => {
                self.link = tag.attribute("href").map(|href| (href.trim().to_string(), self.out.len()));
            }
            ("a", true) => self.close_link(),
            (name, _) if block(name) => self.end_line(1),
            _ => {}
        }
    }

    /// Puts the link's address after its words, unless the words are an
    /// address already. Google sends a pasted address wrapped in a link
    /// through its own redirect, and the address shown is the one to keep.
    fn close_link(&mut self) {
        let Some((href, start)) = self.link.take() else {
            return;
        };
        if href.is_empty() {
            return;
        }
        let words = self.out[start..].trim();
        let shown = words == href
            || href.strip_prefix("mailto:") == Some(words)
            || words.starts_with("https://")
            || words.starts_with("http://");
        if shown {
            return;
        }
        if words.is_empty() {
            self.owed_space();
            self.out.push_str(&href);
        } else {
            let _ = write!(self.out, " ({href})");
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
        let lines: Vec<&str> = self.out.split('\n').map(str::trim_end).collect();
        lines.join("\n").trim_matches('\n').to_string()
    }
}

/// One typed line as HTML. A space at the start of the line or after
/// another space becomes `&nbsp;`, since HTML folds those away.
fn line_html(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(start) = find_address(rest) {
        out.push_str(&escape(&rest[..start], out.is_empty()));
        let candidate = &rest[start..];
        let end = candidate
            .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"'))
            .unwrap_or(candidate.len());
        // Punctuation after an address ends the sentence, not the address.
        let address = candidate[..end].trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']']);
        let escaped = escape(address, false);
        let _ = write!(out, "<a href=\"{escaped}\">{escaped}</a>");
        rest = &candidate[address.len()..];
    }
    out.push_str(&escape(rest, out.is_empty()));
    out
}

fn find_address(line: &str) -> Option<usize> {
    [line.find("https://"), line.find("http://")].into_iter().flatten().min()
}

/// `text` with the characters HTML reads as markup written as references.
/// `at_start` says the text starts a line, where a space would be lost.
fn escape(text: &str, at_start: bool) -> String {
    let mut out = String::with_capacity(text.len());
    let mut after_space = at_start;
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            ' ' if after_space => out.push_str("&nbsp;"),
            c => out.push(c),
        }
        after_space = c == ' ';
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn google_s_line_breaks_and_bold_read_as_lines() {
        assert_eq!(
            text("Line one<br>Line two<br><br><b>Bring</b> the numbers"),
            "Line one\nLine two\n\nBring the numbers"
        );
    }

    #[test]
    fn paragraphs_and_lists_read_as_lines() {
        assert_eq!(
            text("<p>Agenda</p><ul><li>Budget</li><li>Hiring</li></ul>"),
            "Agenda\n\n- Budget\n- Hiring"
        );
    }

    #[test]
    fn a_link_keeps_its_address_after_its_words() {
        assert_eq!(
            text(r#"See <a href="https://example.com/a?b=1&amp;c=2">the agenda</a> first"#),
            "See the agenda (https://example.com/a?b=1&c=2) first"
        );
    }

    #[test]
    fn a_link_that_shows_an_address_keeps_only_that() {
        assert_eq!(
            text(r#"<a href="https://www.google.com/url?q=https://example.com">https://example.com</a>"#),
            "https://example.com"
        );
        assert_eq!(text(r#"<a href="mailto:ann@example.com">ann@example.com</a>"#), "ann@example.com");
    }

    #[test]
    fn character_references_are_decoded() {
        assert_eq!(text("Tom &amp; Jerry&nbsp;&lt;3"), "Tom & Jerry <3");
    }

    #[test]
    fn a_line_break_in_the_source_stays_a_line_break() {
        // Google's page shows a raw line break inside HTML as one, so a
        // description mixing both reads the same here.
        assert_eq!(text("Join <b>here</b>\nMeeting 123"), "Join here\nMeeting 123");
    }

    #[test]
    fn plain_text_stays_as_it_is() {
        let plain = "Room 5, 2nd floor\nhttps://meet.example.com/x\nif a < b & c > d";
        assert_eq!(text(plain), plain);
    }

    #[test]
    fn typed_lines_go_out_as_html_with_links() {
        assert_eq!(
            html("Agenda\nSee https://example.com/a?b=1&c=2.\nTom & Jerry <3"),
            "Agenda<br>See <a href=\"https://example.com/a?b=1&amp;c=2\">https://example.com/a?b=1&amp;c=2</a>.<br>\
             Tom &amp; Jerry &lt;3"
        );
    }

    #[test]
    fn typed_lines_read_back_as_they_were_typed() {
        for typed in [
            "",
            "One line",
            "Agenda\n\n- Budget\n- Hiring",
            "See the agenda (https://example.com/a?b=1&c=2) first",
            "Tom & Jerry <3 &amp; \"quotes\"",
            "  indented  twice",
            "<b>not bold</b>",
        ] {
            assert_eq!(text(&html(typed)), typed, "{typed:?}");
        }
    }

    #[test]
    fn google_html_read_and_written_once_reads_the_same_again() {
        let google = r#"Line one<br><a href="https://example.com/doc">the doc</a><ul><li>A</li></ul>"#;
        let read = text(google);
        assert_eq!(text(&html(&read)), read);
    }
}
