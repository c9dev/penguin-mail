//! Pictures held aside while Format rewrites the lines around them.
//!
//! Format reads the writer's lines as Markdown and styles them again, and
//! Markdown has no way to read a picture. Each one goes into the lines as
//! the object replacement character, the one a text buffer reports in a
//! picture's place, and the formatted lines name the picture where that
//! character ends up. Markdown keeps the order of the characters it reads,
//! so the first placeholder after the rewrite is the first picture before
//! it, and the buffer keeps each picture in the anchor it already had.

use crate::richtext::{RichBody, Span};

/// What stands in for a picture while its lines are rewritten.
pub const HELD: char = '\u{fffc}';

/// Puts a placeholder where each picture in `body` sits, and gives back
/// the pictures in the order they appeared.
pub fn hold(body: &mut RichBody) -> Vec<Span> {
    let mut held = Vec::new();
    for span in body.blocks.iter_mut().flat_map(|b| b.spans.iter_mut()) {
        if span.image.is_some() {
            held.push(std::mem::replace(span, Span::plain(HELD)));
        }
    }
    held
}

/// Puts `pictures` back in `body`, one at each placeholder, in order. A
/// placeholder left with no picture to show goes.
pub fn restore(body: &mut RichBody, pictures: &[Span]) {
    let mut pictures = pictures.iter();
    for block in &mut body.blocks {
        if !block.spans.iter().any(|s| s.text.contains(HELD)) {
            continue;
        }
        let mut spans = Vec::with_capacity(block.spans.len() + 2);
        for span in block.spans.drain(..) {
            if span.image.is_some() || !span.text.contains(HELD) {
                spans.push(span);
                continue;
            }
            let mut pieces = span.text.split(HELD).peekable();
            while let Some(piece) = pieces.next() {
                if !piece.is_empty() {
                    spans.push(Span {
                        text: piece.to_string(),
                        ..span.clone()
                    });
                }
                if pieces.peek().is_some()
                    && let Some(picture) = pictures.next()
                {
                    spans.push(picture.clone());
                }
            }
        }
        block.spans = spans;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::richtext::{Block, BlockKind};

    fn picture(cid: &str) -> Span {
        Span::image("image", format!("cid:{cid}"))
    }

    #[test]
    fn a_picture_becomes_a_placeholder_in_its_line() {
        let mut body = RichBody {
            blocks: vec![Block::new(
                BlockKind::Paragraph,
                vec![Span::plain("Look "), picture("a"), Span::plain(" here")],
            )],
        };
        let held = hold(&mut body);
        assert_eq!(held, vec![picture("a")]);
        assert_eq!(body.blocks[0].text(), "Look \u{fffc} here");
        assert!(body.blocks[0].spans.iter().all(|s| s.image.is_none()));
    }

    #[test]
    fn pictures_come_back_in_order_through_markdown() {
        // Markdown typed as plain lines, with a picture after the heading
        // and one at the start of the second item.
        let line = |spans: Vec<Span>| Block::new(BlockKind::Paragraph, spans);
        let mut lines = RichBody {
            blocks: vec![
                line(vec![Span::plain("# Plan"), picture("a")]),
                line(vec![]),
                line(vec![Span::plain("- soup")]),
                line(vec![Span::plain("- "), picture("b"), Span::plain("salad")]),
            ],
        };
        let mut wanted = RichBody::from_markdown("# Plan\n\n- soup\n- salad");
        wanted.blocks[0].spans.push(picture("a"));
        wanted.blocks[3].spans.insert(0, picture("b"));
        let held = hold(&mut lines);
        // Format reads the lines' text as the writer typed it.
        let typed: Vec<String> = lines.blocks.iter().map(Block::text).collect();
        let mut formatted = RichBody::from_markdown(&typed.join("\n"));
        restore(&mut formatted, &held);
        assert_eq!(formatted, wanted, "{}", formatted.to_markdown());
    }

    #[test]
    fn a_placeholder_in_styled_words_splits_them() {
        let mut body = RichBody::from_markdown(&format!("**bold {HELD} words**"));
        restore(&mut body, &[picture("a")]);
        let spans = &body.blocks[0].spans;
        assert_eq!(spans.len(), 3, "{spans:?}");
        assert_eq!(spans[0].text, "bold ");
        assert!(spans[0].style.bold);
        assert_eq!(spans[1], picture("a"));
        assert_eq!(spans[2].text, " words");
        assert!(spans[2].style.bold);
    }

    #[test]
    fn a_placeholder_with_no_picture_goes() {
        let mut body = RichBody::from_markdown(&format!("one {HELD}{HELD} two"));
        restore(&mut body, &[picture("a")]);
        assert_eq!(body.blocks[0].spans[1], picture("a"));
        assert_eq!(body.blocks[0].text(), "one image two");
    }
}
