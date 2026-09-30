//! Stray Markdown: marks such as `# `, `- ` or `**` sitting in a rich
//! body as plain characters, because someone typed them or pasted them
//! from a place that only offered plain text.
//!
//! The composer asks two things here. [`reads_as_markdown`] decides
//! whether some text was written as Markdown, and it is strict about it:
//! a lone asterisk or a hyphen in a sentence is prose, so a letter never
//! sets it off. [`conversions`] says which lines of a rich body to replace
//! with styled ones, leaving the quoted history, the signature, and every
//! line already styled as they are.

use std::ops::Range;

use crate::richtext::{Block, BlockKind, RichBody, Style, span_to_markdown};

/// Whether `text` was written as Markdown.
///
/// It takes structure to say yes: a heading line, two list lines, a pair
/// of code fences, a table, a `**bold**` pair or a `[text](url)` link.
/// Lines indented as code are passed over, since a `# ` there is a
/// comment in a shell script.
pub fn reads_as_markdown(text: &str) -> bool {
    let lines: Vec<&str> = text.lines().collect();
    let (mut list, mut fences) = (0, 0);
    for (index, line) in lines.iter().enumerate() {
        let rest = line.trim_start_matches(' ');
        if line.len() - rest.len() > 3 {
            continue;
        }
        if rest.starts_with("```") || rest.starts_with("~~~") {
            fences += 1;
            if fences == 2 {
                return true;
            }
            continue;
        }
        if is_list_line(rest) {
            list += 1;
        }
        let table = rest.contains('|')
            && lines
                .get(index + 1)
                .is_some_and(|next| is_table_rule(next));
        if list >= 2
            || table
            || is_heading(rest)
            || has_bold_pair(rest)
            || has_link(rest)
        {
            return true;
        }
    }
    false
}

/// Whether a clipboard offering `mime_types` holds plain text and nothing
/// richer: no HTML or RTF to keep the look of, no picture and no files.
pub fn plain_text_only(mime_types: &[&str]) -> bool {
    let richer = |mime: &&str| {
        let mime = mime.to_ascii_lowercase();
        mime.starts_with("text/html")
            || mime.contains("rtf")
            || mime.starts_with("image/")
            || mime.starts_with("text/uri-list")
    };
    mime_types.iter().any(|mime| mime.starts_with("text/plain"))
        && !mime_types.iter().any(richer)
}

/// `# Title` to `###### Title`: hashes, a space, then words.
fn is_heading(line: &str) -> bool {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    (1..=6).contains(&hashes)
        && line[hashes..].starts_with(' ')
        && !line[hashes..].trim().is_empty()
}

/// `- item`, `* item`, `+ item`, `1. item` or `1) item`.
fn is_list_line(line: &str) -> bool {
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    let rest = match digits {
        0 => line.strip_prefix(['-', '*', '+']),
        1..=9 => line[digits..].strip_prefix(['.', ')']),
        _ => None,
    };
    rest.is_some_and(|rest| rest.starts_with(' ') && !rest.trim().is_empty())
}

/// The line under a table's header: `---|---`, with or without the
/// outer bars and the colons that align a column.
fn is_table_rule(line: &str) -> bool {
    let line = line.trim();
    let line = line.strip_prefix('|').unwrap_or(line);
    let line = line.strip_suffix('|').unwrap_or(line);
    let cells: Vec<&str> = line.split('|').map(str::trim).collect();
    cells.len() >= 2
        && cells.iter().all(|cell| {
            let dashes = cell.trim_start_matches(':').trim_end_matches(':');
            dashes.len() >= 3 && dashes.chars().all(|c| c == '-')
        })
}

/// Two `**` with words hard against them on the inside.
fn has_bold_pair(line: &str) -> bool {
    let mut from = 0;
    while let Some(at) = line[from..].find("**") {
        let opens = from + at + 2;
        let inside = &line[opens..];
        if let Some(first) = inside.chars().next()
            && !first.is_whitespace()
            && first != '*'
            && let Some(close) = inside[first.len_utf8()..].find("**")
            && !inside[..first.len_utf8() + close].ends_with(char::is_whitespace)
        {
            return true;
        }
        from = opens;
    }
    false
}

/// `[words](address)`, with no space in the address.
fn has_link(line: &str) -> bool {
    let mut from = 0;
    while let Some(at) = line[from..].find("](") {
        let at = from + at;
        let (before, after) = (&line[..at], &line[at + 2..]);
        if let Some(open) = before.rfind('[')
            && !before[open + 1..].trim().is_empty()
            && let Some(close) = after.find(')')
            && close > 0
            && !after[..close].contains(char::is_whitespace)
        {
            return true;
        }
        from = at + 2;
    }
    false
}

/// A run of the body's lines and the styled lines that replace them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversion {
    /// The lines replaced, as indices into the body's blocks.
    pub lines: Range<usize>,
    pub body: RichBody,
}

/// Whether the writer's part of `body` still holds Markdown to format.
pub fn left_in(body: &RichBody) -> bool {
    runs(&body.blocks).any(|run| reads_as_markdown(&loose_text(&body.blocks[run])))
}

/// The runs of `body` that hold Markdown, each with its styled lines.
pub fn conversions(body: &RichBody) -> Vec<Conversion> {
    runs(&body.blocks)
        .filter(|run| reads_as_markdown(&loose_text(&body.blocks[run.clone()])))
        .map(|run| {
            let markdown: Vec<String> = body.blocks[run.clone()]
                .iter()
                .map(line_as_markdown)
                .collect();
            Conversion {
                lines: run,
                body: styled(&markdown.join("\n")),
            }
        })
        .collect()
}

/// `markdown` as styled lines. A picture it names becomes a link to the
/// picture, because only a file attached to the message can show one.
pub fn styled(markdown: &str) -> RichBody {
    let mut body = RichBody::from_markdown(markdown);
    for span in body.blocks.iter_mut().flat_map(|b| b.spans.iter_mut()) {
        if let Some(source) = span.image.take() {
            if span.text.trim().is_empty() {
                span.text = source.clone();
            }
            span.link = Some(source);
        }
    }
    body
}

/// How many of `blocks` the writer wrote: the lines above the signature's
/// `--` and above the first quoted line.
fn written_part(blocks: &[Block]) -> usize {
    let quote = blocks
        .iter()
        .position(|b| b.kind == BlockKind::Quote)
        .unwrap_or(blocks.len());
    blocks[..quote]
        .iter()
        .rposition(|b| b.kind == BlockKind::Paragraph && b.text().trim_end() == "--")
        .unwrap_or(quote)
}

/// Whether a line may hold marks nobody has formatted: a paragraph with
/// no picture in it. A picture lives in the buffer as a widget, which no
/// rewrite of the line could carry over.
fn is_loose(block: &Block) -> bool {
    block.kind == BlockKind::Paragraph && block.spans.iter().all(|s| s.image.is_none())
}

/// The runs of loose lines in the writer's part, without the blank lines
/// at either end.
fn runs(blocks: &[Block]) -> impl Iterator<Item = Range<usize>> + '_ {
    let written = &blocks[..written_part(blocks)];
    let mut at = 0;
    std::iter::from_fn(move || {
        while at < written.len() {
            let start = at + written[at..].iter().take_while(|b| !is_loose(b)).count();
            let end = start + written[start..].iter().take_while(|b| is_loose(b)).count();
            at = end.max(start + 1);
            let lines = &written[start..end];
            let first = start + lines.iter().take_while(|b| b.is_blank()).count();
            let last = end - lines.iter().rev().take_while(|b| b.is_blank()).count();
            if first < last {
                return Some(first..last);
            }
        }
        None
    })
}

/// The run's words as the detector should see them. A code span keeps
/// its own marks, so it drops out.
fn loose_text(blocks: &[Block]) -> String {
    let lines: Vec<String> = blocks
        .iter()
        .map(|b| {
            b.spans
                .iter()
                .map(|s| if s.style.code { " " } else { s.text.as_str() })
                .collect()
        })
        .collect();
    lines.join("\n")
}

/// The line as Markdown: its plain words as typed, so their marks count,
/// and its styled words written out with marks, so they keep their style.
fn line_as_markdown(block: &Block) -> String {
    block
        .spans
        .iter()
        .map(|span| match span.style == Style::default() && span.link.is_none() {
            true => span.text.clone(),
            false => span_to_markdown(span),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::richtext::Span;

    #[test]
    fn ordinary_prose_is_not_markdown() {
        for prose in [
            "Hi Ann,\n\nSee you on Friday - bring the charts.\n\nDana",
            "Prices went up 5*3 times; the 2 * 4 grid stays.",
            "- just one line with a hyphen",
            "Call me at #3 or come by.",
            "#hashtag and #another",
            "I'd rate it ** out of five.",
            "Use [brackets] (like this) freely.",
            "a | b",
            "Thanks!\n-- \nDana",
        ] {
            assert!(!reads_as_markdown(prose), "{prose:?}");
        }
    }

    #[test]
    fn only_a_clipboard_of_plain_text_counts() {
        assert!(plain_text_only(&["text/plain;charset=utf-8", "text/plain"]));
        assert!(plain_text_only(&["UTF8_STRING", "text/plain", "TEXT"]));
        assert!(!plain_text_only(&["text/html", "text/plain"]));
        assert!(!plain_text_only(&["text/plain", "text/rtf"]));
        assert!(!plain_text_only(&["image/png", "text/plain"]));
        assert!(!plain_text_only(&["text/uri-list", "text/plain"]));
        assert!(!plain_text_only(&["image/png"]));
        assert!(!plain_text_only(&[]));
    }

    #[test]
    fn a_heading_line_is_markdown() {
        assert!(reads_as_markdown("# Plan\n\nWe ship on Friday."));
        assert!(reads_as_markdown("Notes\n\n### Next steps"));
    }

    #[test]
    fn two_list_lines_are_markdown() {
        assert!(reads_as_markdown("Bring:\n- soup\n- salad"));
        assert!(reads_as_markdown("1. first\n2. second"));
        assert!(reads_as_markdown("* one\n* two"));
    }

    #[test]
    fn a_code_fence_pair_is_markdown() {
        assert!(reads_as_markdown("Run this:\n```\ncargo test\n```"));
        assert!(!reads_as_markdown("Three ticks ``` once."));
    }

    #[test]
    fn a_bold_pair_is_markdown() {
        assert!(reads_as_markdown("This is **important** to read."));
        assert!(!reads_as_markdown("This is ** not ** bold."));
    }

    #[test]
    fn a_link_is_markdown() {
        assert!(reads_as_markdown("See [the plan](https://e.com/plan)."));
        assert!(!reads_as_markdown("See [the plan] (later)."));
    }

    #[test]
    fn a_table_is_markdown() {
        assert!(reads_as_markdown("| Day | Dish |\n|-----|------|\n| Mon | Soup |"));
        assert!(reads_as_markdown("Day | Dish\n--- | ---\nMon | Soup"));
    }

    #[test]
    fn marks_inside_a_code_block_do_not_count() {
        // Four spaces make a code block, where `# ` is a shell comment.
        assert!(!reads_as_markdown("Try:\n\n    # comment"));
    }

    fn paragraph(text: &str) -> Block {
        Block::new(BlockKind::Paragraph, vec![Span::plain(text)])
    }

    fn lines(texts: &[&str]) -> RichBody {
        RichBody {
            blocks: texts.iter().map(|t| paragraph(t)).collect(),
        }
    }

    #[test]
    fn typed_markdown_becomes_styled_lines() {
        let body = lines(&["Hi Ann,", "", "# Plan", "", "- soup", "- salad", "", "Dana"]);
        assert!(left_in(&body));
        let found = conversions(&body);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].lines, 0..8);
        let kinds: Vec<BlockKind> = found[0].body.blocks.iter().map(|b| b.kind).collect();
        assert!(kinds.contains(&BlockKind::Heading(1)), "{kinds:?}");
        assert_eq!(
            kinds.iter().filter(|k| **k == BlockKind::Bullet).count(),
            2,
            "{kinds:?}"
        );
    }

    #[test]
    fn blank_lines_around_a_run_stay_where_they_are() {
        let body = lines(&["", "", "- soup", "- salad", ""]);
        let found = conversions(&body);
        assert_eq!(found[0].lines, 2..4);
    }

    #[test]
    fn prose_is_left_alone() {
        let body = lines(&["Hi Ann,", "", "See you Friday - bring soup.", "", "Dana"]);
        assert!(!left_in(&body));
        assert!(conversions(&body).is_empty());
    }

    #[test]
    fn styled_lines_split_the_runs_and_keep_their_style() {
        let mut body = lines(&["- a", "- b", "", "x", "- c", "- d"]);
        body.blocks[3] = Block::new(BlockKind::Heading(2), vec![Span::plain("Kept")]);
        let found = conversions(&body);
        let ranges: Vec<Range<usize>> = found.iter().map(|c| c.lines.clone()).collect();
        assert_eq!(ranges, [0..2, 4..6]);
    }

    #[test]
    fn a_styled_word_keeps_its_style_through_the_conversion() {
        let bold = Span {
            style: Style {
                bold: true,
                ..Style::default()
            },
            ..Span::plain("Ann")
        };
        let body = RichBody {
            blocks: vec![
                Block::new(BlockKind::Paragraph, vec![Span::plain("- hi "), bold.clone()]),
                paragraph("- bye"),
            ],
        };
        let found = conversions(&body);
        assert_eq!(found[0].body.blocks[0].kind, BlockKind::Bullet);
        assert_eq!(found[0].body.blocks[0].spans[1], bold);
    }

    #[test]
    fn the_quote_and_the_signature_stay_as_they_are() {
        let mut body = lines(&["Thanks", "", "--", "- sig one", "- sig two"]);
        assert!(!left_in(&body), "the signature is not the writer's to format");
        body = lines(&["Thanks"]);
        body.blocks.push(Block::new(
            BlockKind::Quote,
            vec![Span::plain("On Monday, Ann wrote:")],
        ));
        body.blocks.push(paragraph("- quoted one"));
        body.blocks.push(paragraph("- quoted two"));
        assert!(!left_in(&body));
        assert!(conversions(&body).is_empty());
    }

    #[test]
    fn a_code_span_hides_its_marks() {
        let code = Span {
            style: Style {
                code: true,
                ..Style::default()
            },
            ..Span::plain("**not bold**")
        };
        let body = RichBody {
            blocks: vec![Block::new(
                BlockKind::Paragraph,
                vec![Span::plain("Type "), code],
            )],
        };
        assert!(!left_in(&body));
    }

    #[test]
    fn a_line_with_a_picture_is_never_rewritten() {
        let body = RichBody {
            blocks: vec![
                paragraph("- one"),
                Block::new(
                    BlockKind::Paragraph,
                    vec![Span::plain("- two "), Span::image("map", "cid:m@x")],
                ),
                paragraph("- three"),
                paragraph("- four"),
            ],
        };
        let found = conversions(&body);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].lines, 2..4);
    }

    #[test]
    fn a_picture_named_in_markdown_becomes_a_link() {
        let body = lines(&["# Map", "![the map](https://e.com/map.png)"]);
        let found = conversions(&body);
        let spans: Vec<&Span> = found[0].body.blocks.iter().flat_map(|b| &b.spans).collect();
        assert!(spans.iter().all(|s| s.image.is_none()), "{spans:?}");
        assert!(
            spans
                .iter()
                .any(|s| s.link.as_deref() == Some("https://e.com/map.png")),
            "{spans:?}"
        );
    }
}
