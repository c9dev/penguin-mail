//! Rich text inside the composer's text buffer: the tags that draw each
//! style, and the two conversions between the buffer and a [`RichBody`].
//!
//! Every style the writer applies is a named tag, and every line carries
//! the tag of its kind, so the buffer holds the whole body on its own. A
//! list line also starts with a marker the writer cannot edit, because
//! GTK draws no bullets of its own.

use gtk::prelude::*;
use gtk::{gdk, glib, pango};

use crate::compose::OutgoingAttachment;
use crate::richtext::{Block, BlockKind, RichBody, Span, Style};

/// The pictures shown in the text: the anchor each one sits in, and the
/// `cid:` the message refers to it by.
pub type Anchors = Vec<(gtk::TextChildAnchor, String)>;

/// The tag on the bullet or number a list line starts with.
pub const MARKER: &str = "marker";
/// Link tags carry their target in the name, one tag per address.
const LINK: &str = "link:";
/// The style tags, in the order the formatting bar shows them.
pub const STYLES: [&str; 4] = ["bold", "italic", "strike", "code"];
/// The line kinds, as tag names.
const BLOCKS: [&str; 8] = [
    "paragraph",
    "heading1",
    "heading2",
    "heading3",
    "bullet",
    "numbered",
    "quote",
    "code-block",
];

pub fn block_tag(kind: BlockKind) -> &'static str {
    match kind {
        BlockKind::Paragraph => "paragraph",
        BlockKind::Heading(1) => "heading1",
        BlockKind::Heading(2) => "heading2",
        BlockKind::Heading(_) => "heading3",
        BlockKind::Bullet => "bullet",
        BlockKind::Numbered => "numbered",
        BlockKind::Quote => "quote",
        BlockKind::Code => "code-block",
    }
}

fn block_kind(tag: &str) -> Option<BlockKind> {
    Some(match tag {
        "paragraph" => BlockKind::Paragraph,
        "heading1" => BlockKind::Heading(1),
        "heading2" => BlockKind::Heading(2),
        "heading3" => BlockKind::Heading(3),
        "bullet" => BlockKind::Bullet,
        "numbered" => BlockKind::Numbered,
        "quote" => BlockKind::Quote,
        "code-block" => BlockKind::Code,
        _ => return None,
    })
}

/// Adds every tag the rich body needs to `buffer`.
pub fn install(buffer: &gtk::TextBuffer) {
    let table = buffer.tag_table();
    let add = |tag: gtk::TextTag| table.add(&tag);
    add(gtk::TextTag::builder().name("bold").weight(700).build());
    add(gtk::TextTag::builder()
        .name("italic")
        .style(pango::Style::Italic)
        .build());
    add(gtk::TextTag::builder()
        .name("strike")
        .strikethrough(true)
        .build());
    add(gtk::TextTag::builder()
        .name("code")
        .family("monospace")
        .scale(0.94)
        .build());
    add(gtk::TextTag::builder().name("paragraph").build());
    for (name, scale, above) in [
        ("heading1", 1.5, 14),
        ("heading2", 1.3, 12),
        ("heading3", 1.15, 10),
    ] {
        add(gtk::TextTag::builder()
            .name(name)
            .weight(800)
            .scale(scale)
            .pixels_above_lines(above)
            .pixels_below_lines(2)
            .build());
    }
    for name in ["bullet", "numbered"] {
        add(gtk::TextTag::builder()
            .name(name)
            .left_margin(32)
            .indent(-16)
            .pixels_below_lines(2)
            .build());
    }
    add(gtk::TextTag::builder()
        .name("quote")
        .left_margin(26)
        .foreground("#777777")
        .style(pango::Style::Italic)
        .build());
    add(gtk::TextTag::builder()
        .name("code-block")
        .family("monospace")
        .scale(0.94)
        .left_margin(26)
        .build());
    add(gtk::TextTag::builder().name(MARKER).editable(false).build());
}

/// The tag for `url`, made the first time that address is used.
pub fn link_tag(buffer: &gtk::TextBuffer, url: &str) -> gtk::TextTag {
    let name = format!("{LINK}{url}");
    if let Some(tag) = buffer.tag_table().lookup(&name) {
        return tag;
    }
    let tag = gtk::TextTag::builder()
        .name(&name)
        .foreground("#1c71d8")
        .underline(pango::Underline::Single)
        .build();
    buffer.tag_table().add(&tag);
    tag
}

/// The styles and link on the character at `iter`.
pub fn style_at(iter: &gtk::TextIter) -> (Style, Option<String>) {
    let mut style = Style::default();
    let mut link = None;
    for tag in iter.tags() {
        let Some(name) = tag.name() else { continue };
        match name.as_str() {
            "bold" => style.bold = true,
            "italic" => style.italic = true,
            "strike" => style.strike = true,
            "code" => style.code = true,
            other => {
                if let Some(url) = other.strip_prefix(LINK) {
                    link = Some(url.to_string());
                }
            }
        }
    }
    (style, link)
}

/// What text typed at `iter` should look like: the style of the character
/// before it, which is how a word carries its styling on as you type, or
/// of the character after it at the start of a line.
pub fn style_before(iter: &gtk::TextIter) -> (Style, Option<String>) {
    let mut probe = *iter;
    if !probe.starts_line() {
        probe.backward_char();
    }
    if probe
        .tags()
        .iter()
        .any(|t| t.name().as_deref() == Some(MARKER))
    {
        return (Style::default(), None);
    }
    style_at(&probe)
}

/// The kind of line `line`.
pub fn kind_at(buffer: &gtk::TextBuffer, line: i32) -> BlockKind {
    let Some(start) = buffer.iter_at_line(line) else {
        return BlockKind::Paragraph;
    };
    for tag in start.tags() {
        if let Some(kind) = tag.name().as_deref().and_then(block_kind) {
            return kind;
        }
    }
    BlockKind::Paragraph
}

/// The start and end of `line`, whole.
fn line_bounds(buffer: &gtk::TextBuffer, line: i32) -> Option<(gtk::TextIter, gtk::TextIter)> {
    let start = buffer.iter_at_line(line)?;
    let mut end = start;
    if !end.ends_line() {
        end.forward_to_line_end();
    }
    Some((start, end))
}

/// The marker a list line begins with, ready to insert.
fn marker_text(kind: BlockKind, number: usize) -> Option<String> {
    match kind {
        BlockKind::Bullet => Some("• ".to_string()),
        BlockKind::Numbered => Some(format!("{number}. ")),
        _ => None,
    }
}

/// Where the line's text starts, past any marker.
pub fn text_start(buffer: &gtk::TextBuffer, line: i32) -> gtk::TextIter {
    let Some((start, end)) = line_bounds(buffer, line) else {
        return buffer.end_iter();
    };
    let mut iter = start;
    while iter < end
        && iter
            .tags()
            .iter()
            .any(|t| t.name().as_deref() == Some(MARKER))
    {
        iter.forward_char();
    }
    iter
}

/// Whether the line holds nothing but its marker.
pub fn is_empty_line(buffer: &gtk::TextBuffer, line: i32) -> bool {
    let Some((_, end)) = line_bounds(buffer, line) else {
        return true;
    };
    let start = text_start(buffer, line);
    buffer.text(&start, &end, false).trim().is_empty()
}

/// Makes `line` a line of `kind`: its tag, and its marker.
pub fn set_kind(buffer: &gtk::TextBuffer, line: i32, kind: BlockKind) {
    let Some((start, _)) = line_bounds(buffer, line) else {
        return;
    };
    // Out with the old marker, so a bullet does not keep a number.
    let text_at = text_start(buffer, line);
    if text_at.offset() > start.offset() {
        let (mut from, mut to) = (start, text_at);
        buffer.delete(&mut from, &mut to);
    }
    if let Some((start, end)) = line_bounds(buffer, line) {
        for tag in BLOCKS {
            buffer.remove_tag_by_name(tag, &start, &end);
        }
    }
    if let Some(marker) = marker_text(kind, 1) {
        let mut at = buffer
            .iter_at_line(line)
            .unwrap_or_else(|| buffer.end_iter());
        buffer.insert_with_tags_by_name(&mut at, &marker, &[MARKER, block_tag(kind)]);
    }
    if let Some((start, end)) = line_bounds(buffer, line) {
        buffer.apply_tag_by_name(block_tag(kind), &start, &end);
    }
}

/// Counts the numbered lists again, so they read 1, 2, 3 after an edit.
pub fn renumber(buffer: &gtk::TextBuffer) {
    let mut number = 0;
    for line in 0..buffer.line_count() {
        if kind_at(buffer, line) != BlockKind::Numbered {
            number = 0;
            continue;
        }
        number += 1;
        let start = buffer
            .iter_at_line(line)
            .unwrap_or_else(|| buffer.end_iter());
        let marker = text_start(buffer, line);
        let wanted = format!("{number}. ");
        if buffer.text(&start, &marker, false) == wanted {
            continue;
        }
        let (mut from, mut to) = (start, marker);
        buffer.delete(&mut from, &mut to);
        let mut at = buffer
            .iter_at_line(line)
            .unwrap_or_else(|| buffer.end_iter());
        buffer.insert_with_tags_by_name(&mut at, &wanted, &[MARKER, "numbered"]);
    }
}

/// The whole buffer as a rich body.
pub fn read(buffer: &gtk::TextBuffer, anchors: &Anchors) -> RichBody {
    let mut blocks = Vec::new();
    for line in 0..buffer.line_count() {
        let Some((start, end)) = line_bounds(buffer, line) else {
            continue;
        };
        let kind = kind_at(buffer, line);
        let mut spans: Vec<Span> = Vec::new();
        let mut iter = start;
        while iter < end {
            if iter
                .tags()
                .iter()
                .any(|t| t.name().as_deref() == Some(MARKER))
            {
                iter.forward_char();
                continue;
            }
            if let Some(anchor) = iter.child_anchor() {
                if let Some((_, cid)) = anchors.iter().find(|(a, _)| *a == anchor) {
                    spans.push(Span::image("image", format!("cid:{cid}")));
                }
                iter.forward_char();
                continue;
            }
            let (style, link) = style_at(&iter);
            let character = iter.char();
            match spans.last_mut() {
                Some(last) if last.style == style && last.link == link && last.image.is_none() => {
                    last.text.push(character)
                }
                _ => spans.push(Span {
                    text: character.to_string(),
                    style,
                    link,
                    image: None,
                }),
            }
            iter.forward_char();
        }
        blocks.push(Block { kind, spans });
    }
    RichBody { blocks }
}

/// Fills the view with `body`, drawing every style and showing every
/// picture the attachments still hold.
pub fn write(
    view: &gtk::TextView,
    body: &RichBody,
    attachments: &[OutgoingAttachment],
    anchors: &mut Anchors,
) {
    let buffer = view.buffer();
    let buffer = &buffer;
    anchors.clear();
    buffer.set_text("");
    let mut number = 0;
    for (index, block) in body.blocks.iter().enumerate() {
        let mut at = buffer.end_iter();
        if index > 0 {
            buffer.insert(&mut at, "\n");
        }
        number = if block.kind == BlockKind::Numbered {
            number + 1
        } else {
            0
        };
        let tag = block_tag(block.kind);
        if let Some(marker) = marker_text(block.kind, number) {
            let mut at = buffer.end_iter();
            buffer.insert_with_tags_by_name(&mut at, &marker, &[MARKER, tag]);
        }
        for span in &block.spans {
            let mut at = buffer.end_iter();
            if let Some(src) = &span.image {
                let cid = src.strip_prefix("cid:").unwrap_or(src);
                let found = attachments
                    .iter()
                    .find(|a| a.content_id.as_deref() == Some(cid));
                match found {
                    Some(attachment) => {
                        let start = at.offset();
                        insert_image(view, &mut at, cid, &attachment.data, anchors);
                        let (from, to) = (buffer.iter_at_offset(start), buffer.end_iter());
                        buffer.apply_tag_by_name(tag, &from, &to);
                    }
                    // A draft reopened without its pictures says where one
                    // was, rather than losing the line it sat on.
                    None => {
                        let name = match span.text.trim() {
                            "" => "image".to_string(),
                            alt => alt.to_string(),
                        };
                        buffer.insert_with_tags_by_name(&mut at, &format!("[{name}]"), &[tag]);
                    }
                }
                continue;
            }
            let mut names: Vec<&str> = vec![tag];
            for name in STYLES {
                if has(span.style, name) {
                    names.push(name);
                }
            }
            let link = span
                .link
                .as_ref()
                .and_then(|url| link_tag(buffer, url).name())
                .map(|name| name.to_string());
            if let Some(name) = &link {
                names.push(name);
            }
            buffer.insert_with_tags_by_name(&mut at, &span.text, &names);
        }
        // An empty line still belongs to its kind while the writer is on it.
        if block.spans.is_empty()
            && block.kind != BlockKind::Paragraph
            && let Some((start, end)) = line_bounds(buffer, buffer.line_count() - 1)
        {
            buffer.apply_tag_by_name(tag, &start, &end);
        }
    }
    buffer.place_cursor(&buffer.start_iter());
}

/// Puts the picture in `data` at `at`, held by an anchor the reader maps
/// back to `cid`.
pub fn insert_image(
    view: &gtk::TextView,
    at: &mut gtk::TextIter,
    cid: &str,
    data: &[u8],
    anchors: &mut Anchors,
) {
    let Ok(texture) = gdk::Texture::from_bytes(&glib::Bytes::from(data)) else {
        return;
    };
    let anchor = view.buffer().create_child_anchor(at);
    let picture = gtk::Picture::for_paintable(&texture);
    picture.set_can_shrink(true);
    picture.set_content_fit(gtk::ContentFit::Contain);
    // Big pictures come down to something the writer can see all of.
    let (width, height) = (texture.width() as f64, texture.height() as f64);
    let scale = (420.0 / width).min(260.0 / height).min(1.0);
    picture.set_size_request((width * scale) as i32, (height * scale) as i32);
    view.add_child_at_anchor(&picture, &anchor);
    picture.set_visible(true);
    anchors.push((anchor, cid.to_string()));
}

pub fn has(style: Style, name: &str) -> bool {
    match name {
        "bold" => style.bold,
        "italic" => style.italic,
        "strike" => style.strike,
        "code" => style.code,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Everything the buffer does sits in one test on purpose: GTK belongs
    /// to the thread that starts it, and the test harness hands each test
    /// its own.
    #[test]
    fn the_buffer_holds_a_rich_body_and_gives_it_back() {
        // No display means no GTK, which is how most machines run the suite.
        if gtk::init().is_err() {
            return;
        }
        a_body_reads_back_the_same();
        list_markers_stay_out_of_the_text();
        a_line_changes_kind_and_the_numbers_follow();
        typing_after_styled_words_carries_the_style_on();
    }

    fn buffer() -> (gtk::TextView, gtk::TextBuffer, Anchors) {
        let buffer = gtk::TextBuffer::new(None);
        install(&buffer);
        let view = gtk::TextView::with_buffer(&buffer);
        (view, buffer, Anchors::new())
    }

    fn body() -> RichBody {
        RichBody::from_markdown(
            "Hi **Ann**, see [the menu](https://e.com).\nFriday works.\n\n- soup\n- salad\n\n1. first\n2. second\n\n> quoted\n\n# Title",
        )
    }

    fn a_body_reads_back_the_same() {
        let (view, buffer, mut anchors) = buffer();
        let wanted = body();
        write(&view, &wanted, &[], &mut anchors);
        let read_back = read(&buffer, &anchors);
        assert_eq!(read_back, wanted, "{}", read_back.to_markdown());
        assert_eq!(read_back.to_html(), wanted.to_html());
        assert!(
            read_back.to_html().contains("<strong>Ann</strong>"),
            "{}",
            read_back.to_html()
        );
    }

    fn list_markers_stay_out_of_the_text() {
        let (view, buffer, mut anchors) = buffer();
        write(&view, &body(), &[], &mut anchors);
        let plain = read(&buffer, &anchors).to_plain();
        assert!(!plain.contains('\u{2022}'), "{plain}");
        assert!(plain.contains("- soup"), "{plain}");
        assert!(plain.contains("2. second"), "{plain}");
    }

    fn a_line_changes_kind_and_the_numbers_follow() {
        let (view, buffer, mut anchors) = buffer();
        let mut body = RichBody::default();
        for text in ["one", "two", "three"] {
            body.blocks.push(Block::new(
                BlockKind::Numbered,
                vec![crate::richtext::Span::plain(text)],
            ));
        }
        write(&view, &body, &[], &mut anchors);
        assert_eq!(
            read(&buffer, &anchors).to_plain(),
            "1. one\n2. two\n3. three"
        );
        set_kind(&buffer, 0, BlockKind::Quote);
        renumber(&buffer);
        assert_eq!(
            read(&buffer, &anchors).to_plain(),
            "> one\n1. two\n2. three"
        );
        assert_eq!(kind_at(&buffer, 0), BlockKind::Quote);
    }

    fn typing_after_styled_words_carries_the_style_on() {
        let (view, buffer, mut anchors) = buffer();
        write(
            &view,
            &RichBody::from_markdown("plain **bold**"),
            &[],
            &mut anchors,
        );
        let (style, link) = style_before(&buffer.end_iter());
        assert!(style.bold && link.is_none());
        // Inside the plain words, and at the start of the line, where the
        // line's own style stands in for the one before.
        assert_eq!(
            style_before(&buffer.iter_at_offset(3)),
            (Style::default(), None)
        );
        assert_eq!(style_before(&buffer.start_iter()), (Style::default(), None));
    }
}
