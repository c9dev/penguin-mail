//! The composer's body: one text buffer, and every edit the formatting
//! bar, the keys, the templates and the From row make to it.
//!
//! The buffer holds rich text, where tags carry the styles and the line
//! kinds, or Markdown source. [`Editor`] owns the buffer along with what
//! editing it has to remember: the pictures anchored in it, the style the
//! next typed character takes, and whether an edit in progress is its own.
//! The composer gives it commands and reads the body back out; it never
//! touches a tag.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;

use super::richbuffer::{self, Anchors};
use crate::compose::{LinePrefix, OutgoingAttachment, markdown_to_html, toggle_prefix};
use crate::richtext::{Block, BlockKind, RichBody, Style};
use crate::settings::ComposeFormat;

/// The body read out once, in the two forms a draft keeps.
pub struct Written {
    pub markdown: String,
    /// The styled body, while the writer is in rich text.
    pub rich: Option<RichBody>,
}

/// The words a link will go on, held by marks while the dialog that asks
/// for the address is open, so an edit under them cannot lose them.
pub struct Held {
    start: gtk::TextMark,
    end: gtk::TextMark,
    /// The words as they were when the dialog opened.
    pub text: String,
}

pub struct Editor {
    view: gtk::TextView,
    buffer: gtk::TextBuffer,
    anchors: RefCell<Anchors>,
    format: Cell<ComposeFormat>,
    /// The style the next typed character takes, and where it applies.
    typing: RefCell<Option<(i32, Style, Option<String>)>>,
    /// Text just inserted, waiting for its style: offset and length.
    inserted: RefCell<Vec<(i32, i32)>>,
    /// True while the editor changes the buffer itself, so its own edits
    /// are not styled as typing.
    busy: Cell<bool>,
}

impl Editor {
    /// Takes over `view`'s buffer, written as `format`.
    pub fn new(view: &gtk::TextView, format: ComposeFormat) -> Rc<Editor> {
        let buffer = view.buffer();
        richbuffer::install(&buffer);
        let editor = Rc::new(Editor {
            view: view.clone(),
            buffer,
            anchors: RefCell::new(Anchors::new()),
            format: Cell::new(format),
            typing: RefCell::new(None),
            inserted: RefCell::new(Vec::new()),
            busy: Cell::new(false),
        });
        editor.wire();
        editor
    }

    /// Typed text takes the style it follows and the kind of its line, and
    /// a cursor that moves away drops a style chosen for the old place.
    fn wire(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.buffer.connect_insert_text(move |_, at, text| {
            if let Some(editor) = weak.upgrade() {
                let length = text.chars().count() as i32;
                editor.inserted.borrow_mut().push((at.offset(), length));
            }
        });
        let weak = Rc::downgrade(self);
        self.buffer.connect_changed(move |_| {
            if let Some(editor) = weak.upgrade() {
                editor.after_edit();
            }
        });
        let weak = Rc::downgrade(self);
        self.buffer.connect_cursor_position_notify(move |_| {
            if let Some(editor) = weak.upgrade() {
                editor.cursor_moved();
            }
        });
    }

    pub fn format(&self) -> ComposeFormat {
        self.format.get()
    }

    /// Puts a draft's body in the buffer: `rich` when it brings its
    /// styling, else `markdown`, styled or as source depending on the
    /// format. The cursor goes to the start.
    pub fn fill(
        &self,
        markdown: &str,
        rich: Option<&RichBody>,
        attachments: &[OutgoingAttachment],
    ) {
        self.busy.set(true);
        match self.format.get() {
            ComposeFormat::Rich => {
                let parsed;
                let body = match rich {
                    Some(rich) => rich,
                    None => {
                        let mut body = RichBody::from_markdown(markdown);
                        // A reply and a forward start with blank lines to
                        // write on, which Markdown drops and the writer
                        // wants back.
                        let room = markdown.chars().take_while(|c| *c == '\n').count().min(2);
                        for _ in 0..room {
                            body.blocks.insert(0, Block::default());
                        }
                        parsed = body;
                        &parsed
                    }
                };
                richbuffer::write(
                    &self.view,
                    body,
                    attachments,
                    &mut self.anchors.borrow_mut(),
                );
            }
            ComposeFormat::Markdown => {
                self.anchors.borrow_mut().clear();
                self.buffer.set_text(markdown);
                style_quotes(&self.buffer);
            }
        }
        self.buffer.place_cursor(&self.buffer.start_iter());
        self.busy.set(false);
    }

    /// Turns a style on or off: over the selection, or for what comes next.
    /// In Markdown it puts the marks around the selection instead.
    pub fn toggle(&self, tag: &'static str) {
        let buffer = &self.buffer;
        if self.format.get() == ComposeFormat::Markdown {
            let (before, after) = match tag {
                "bold" => ("**", "**"),
                "italic" => ("*", "*"),
                "strike" => ("~~", "~~"),
                _ => ("`", "`"),
            };
            wrap_selection(buffer, before, after);
            return;
        }
        self.busy.set(true);
        if let Some((start, end)) = buffer.selection_bounds() {
            let on = !whole_selection_has(buffer, tag);
            if on {
                buffer.apply_tag_by_name(tag, &start, &end);
            } else {
                buffer.remove_tag_by_name(tag, &start, &end);
            }
        } else {
            let cursor = buffer.iter_at_mark(&buffer.get_insert());
            let (mut style, link) = self.next_style(&cursor);
            let on = !richbuffer::has(style, tag);
            match tag {
                "bold" => style.bold = on,
                "italic" => style.italic = on,
                "strike" => style.strike = on,
                _ => style.code = on,
            }
            *self.typing.borrow_mut() = Some((cursor.offset(), style, link));
        }
        self.busy.set(false);
    }

    /// The styles the formatting bar should show as on: those of the
    /// selection's first character, or what typing at the cursor would
    /// take. Markdown has none.
    pub fn style_here(&self) -> Style {
        if self.format.get() == ComposeFormat::Markdown {
            return Style::default();
        }
        match self.buffer.selection_bounds() {
            Some((start, _)) => richbuffer::style_at(&start).0,
            None => {
                let cursor = self.buffer.iter_at_mark(&self.buffer.get_insert());
                self.next_style(&cursor).0
            }
        }
    }

    /// The style typing at `at` would take, pending toggles included.
    fn next_style(&self, at: &gtk::TextIter) -> (Style, Option<String>) {
        if let Some((offset, style, link)) = self.typing.borrow().as_ref()
            && *offset == at.offset()
        {
            return (*style, link.clone());
        }
        richbuffer::style_before(at)
    }

    /// Makes the lines the cursor touches a list, a quote, or plain again
    /// when they already were one.
    pub fn list(&self, kind: BlockKind) {
        if self.format.get() == ComposeFormat::Markdown {
            let prefix = match kind {
                BlockKind::Numbered => LinePrefix::Numbered,
                BlockKind::Quote => LinePrefix::Quote,
                _ => LinePrefix::Bullet,
            };
            prefix_lines(&self.buffer, prefix);
            return;
        }
        self.set_block_lines(kind, true);
    }

    /// Makes the lines the cursor touches a paragraph, a heading or a code
    /// block. Unlike a list, these do not toggle back off.
    pub fn set_block(&self, kind: BlockKind) {
        if self.format.get() == ComposeFormat::Markdown {
            let marks = match kind {
                BlockKind::Heading(level) => "#".repeat(level as usize) + " ",
                BlockKind::Code => "    ".to_string(),
                _ => String::new(),
            };
            let buffer = &self.buffer;
            let line = buffer.iter_at_mark(&buffer.get_insert()).line();
            let mut at = buffer
                .iter_at_line(line)
                .unwrap_or_else(|| buffer.end_iter());
            buffer.insert(&mut at, &marks);
            return;
        }
        self.set_block_lines(kind, false);
    }

    fn set_block_lines(&self, kind: BlockKind, toggles: bool) {
        let buffer = &self.buffer;
        let (first, last) = self.lines_touched();
        let same = (first..=last).all(|line| richbuffer::kind_at(buffer, line) == kind);
        let wanted = if toggles && same {
            BlockKind::Paragraph
        } else {
            kind
        };
        self.busy.set(true);
        buffer.begin_user_action();
        for line in first..=last {
            richbuffer::set_kind(buffer, line, wanted);
        }
        richbuffer::renumber(buffer);
        buffer.end_user_action();
        self.busy.set(false);
    }

    /// The first and last line the selection, or the cursor, is on.
    fn lines_touched(&self) -> (i32, i32) {
        match self.buffer.selection_bounds() {
            Some((start, end)) => (start.line(), end.line()),
            None => {
                let cursor = self.buffer.iter_at_mark(&self.buffer.get_insert()).line();
                (cursor, cursor)
            }
        }
    }

    /// Starts a link on the selected words. In Markdown the marks go in
    /// around them at once and there is nothing to ask; in rich text the
    /// words are held for [`Editor::link`] while the writer gives the
    /// address.
    pub fn start_link(&self) -> Option<Held> {
        let buffer = &self.buffer;
        if self.format.get() == ComposeFormat::Markdown {
            wrap_selection(buffer, "[", "]()");
            return None;
        }
        let (start, end) = buffer.selection_bounds().unwrap_or_else(|| {
            let cursor = buffer.iter_at_mark(&buffer.get_insert());
            (cursor, cursor)
        });
        Some(Held {
            text: buffer.text(&start, &end, false).to_string(),
            start: buffer.create_mark(None, &start, true),
            end: buffer.create_mark(None, &end, false),
        })
    }

    /// Lets go of held words without linking them.
    pub fn drop_link(&self, held: Held) {
        self.buffer.delete_mark(&held.start);
        self.buffer.delete_mark(&held.end);
    }

    /// Puts `text`, linked to `url`, where the held words were.
    pub fn link(&self, held: Held, text: &str, url: &str) {
        let buffer = &self.buffer;
        let tag = richbuffer::link_tag(buffer, url);
        self.busy.set(true);
        buffer.begin_user_action();
        let (mut start, mut end) = (
            buffer.iter_at_mark(&held.start),
            buffer.iter_at_mark(&held.end),
        );
        let kind = richbuffer::block_tag(richbuffer::kind_at(buffer, start.line()));
        buffer.delete(&mut start, &mut end);
        let offset = start.offset();
        buffer.insert(&mut start, text);
        let (from, to) = (
            buffer.iter_at_offset(offset),
            buffer.iter_at_offset(offset + text.chars().count() as i32),
        );
        buffer.apply_tag(&tag, &from, &to);
        buffer.apply_tag_by_name(kind, &from, &to);
        buffer.place_cursor(&to);
        buffer.end_user_action();
        self.busy.set(false);
        self.drop_link(held);
    }

    /// Takes every style off the selection, or off the whole body, and
    /// makes its lines plain paragraphs.
    pub fn clear(&self) {
        if self.format.get() == ComposeFormat::Markdown {
            return;
        }
        let buffer = &self.buffer;
        let (start, end) = buffer
            .selection_bounds()
            .unwrap_or_else(|| (buffer.start_iter(), buffer.end_iter()));
        let (first, last) = (start.line(), end.line());
        self.busy.set(true);
        buffer.begin_user_action();
        buffer.remove_all_tags(&start, &end);
        for line in first..=last {
            richbuffer::set_kind(buffer, line, BlockKind::Paragraph);
        }
        buffer.end_user_action();
        self.busy.set(false);
    }

    /// Enter inside a list or quote: another item, or out of the list when
    /// the line is empty. False when Enter should do what it always does.
    pub fn enter(&self) -> bool {
        if self.format.get() != ComposeFormat::Rich {
            return false;
        }
        let buffer = &self.buffer;
        let cursor = buffer.iter_at_mark(&buffer.get_insert());
        let line = cursor.line();
        let kind = richbuffer::kind_at(buffer, line);
        if matches!(kind, BlockKind::Paragraph | BlockKind::Heading(_)) {
            return false;
        }
        self.busy.set(true);
        buffer.begin_user_action();
        if richbuffer::is_empty_line(buffer, line) {
            richbuffer::set_kind(buffer, line, BlockKind::Paragraph);
        } else {
            let mut at = buffer.iter_at_mark(&buffer.get_insert());
            buffer.insert(&mut at, "\n");
            let line = buffer.iter_at_mark(&buffer.get_insert()).line();
            richbuffer::set_kind(buffer, line, kind);
            let end = richbuffer::text_start(buffer, line);
            buffer.place_cursor(&end);
        }
        richbuffer::renumber(buffer);
        buffer.end_user_action();
        self.busy.set(false);
        true
    }

    /// Moves the body to the other way of writing. Rich text reads the
    /// buffer's text as Markdown and styles it, which is also how Format
    /// Markdown works while already in rich text. Markdown writes the
    /// styled body out as source.
    pub fn switch_format(&self, to: ComposeFormat, attachments: &[OutgoingAttachment]) {
        match to {
            ComposeFormat::Rich => {
                let body = RichBody::from_markdown(&self.source());
                self.format.set(ComposeFormat::Rich);
                self.busy.set(true);
                richbuffer::write(
                    &self.view,
                    &body,
                    attachments,
                    &mut self.anchors.borrow_mut(),
                );
                self.busy.set(false);
            }
            ComposeFormat::Markdown => {
                if self.format.get() == ComposeFormat::Markdown {
                    return;
                }
                let markdown = self.rich().to_markdown();
                self.format.set(ComposeFormat::Markdown);
                self.busy.set(true);
                self.anchors.borrow_mut().clear();
                self.buffer.set_text(&markdown);
                style_quotes(&self.buffer);
                self.busy.set(false);
            }
        }
    }

    /// Puts `body` in at the cursor: styled, or as Markdown.
    pub fn insert_body(&self, body: &RichBody) {
        self.busy.set(true);
        self.buffer.begin_user_action();
        match self.format.get() {
            ComposeFormat::Rich => richbuffer::insert(&self.buffer, body),
            ComposeFormat::Markdown => self.buffer.insert_at_cursor(&body.to_markdown()),
        }
        self.buffer.end_user_action();
        self.busy.set(false);
    }

    /// Shows the picture in `data` at the cursor, or names it there while
    /// the body is Markdown. `cid` is what the message calls it.
    pub fn insert_image(&self, cid: &str, filename: &str, data: &[u8]) {
        self.busy.set(true);
        match self.format.get() {
            ComposeFormat::Rich => {
                let mut at = self.buffer.iter_at_mark(&self.buffer.get_insert());
                richbuffer::insert_image(
                    &self.view,
                    &mut at,
                    cid,
                    data,
                    &mut self.anchors.borrow_mut(),
                );
            }
            ComposeFormat::Markdown => {
                let alt: String = filename
                    .chars()
                    .filter(|c| !matches!(c, '[' | ']'))
                    .collect();
                self.buffer
                    .insert_at_cursor(&format!("![{alt}](cid:{cid})"));
            }
        }
        self.busy.set(false);
    }

    /// The text in the buffer, markers and all.
    pub fn source(&self) -> String {
        self.buffer
            .text(&self.buffer.start_iter(), &self.buffer.end_iter(), false)
            .to_string()
    }

    /// The styled body. Only rich text has one to read.
    pub fn rich(&self) -> RichBody {
        richbuffer::read(&self.buffer, &self.anchors.borrow())
    }

    /// The body as Markdown, whichever way it is being written.
    pub fn markdown(&self) -> String {
        match self.format.get() {
            ComposeFormat::Rich => self.rich().to_markdown(),
            ComposeFormat::Markdown => self.source(),
        }
    }

    /// The body in both forms a draft keeps.
    pub fn written(&self) -> Written {
        Written {
            markdown: self.markdown(),
            rich: match self.format.get() {
                ComposeFormat::Rich => Some(self.rich()),
                ComposeFormat::Markdown => None,
            },
        }
    }

    /// The body as the HTML part of the message.
    pub fn html(&self) -> String {
        match self.format.get() {
            ComposeFormat::Rich => self.rich().to_html(),
            ComposeFormat::Markdown => markdown_to_html(&self.source()),
        }
    }

    /// Whether nothing has been written.
    pub fn is_empty(&self) -> bool {
        match self.format.get() {
            ComposeFormat::Rich => self.rich().is_empty(),
            ComposeFormat::Markdown => self.source().trim().is_empty(),
        }
    }

    /// Styles text as it is typed and keeps list numbers in order.
    fn after_edit(&self) {
        let ranges: Vec<(i32, i32)> = self.inserted.borrow_mut().drain(..).collect();
        if self.busy.get() {
            return;
        }
        let buffer = &self.buffer;
        if self.format.get() == ComposeFormat::Markdown {
            self.busy.set(true);
            style_quotes(buffer);
            self.busy.set(false);
            return;
        }
        self.busy.set(true);
        for (offset, length) in ranges {
            let (from, to) = (
                buffer.iter_at_offset(offset),
                buffer.iter_at_offset(offset + length),
            );
            let (style, link) = self.next_style(&from);
            for tag in richbuffer::STYLES {
                if richbuffer::has(style, tag) {
                    buffer.apply_tag_by_name(tag, &from, &to);
                } else {
                    buffer.remove_tag_by_name(tag, &from, &to);
                }
            }
            if let Some(url) = &link {
                buffer.apply_tag(&richbuffer::link_tag(buffer, url), &from, &to);
            }
            // The line keeps its kind, so typing at its end stays in it.
            let kind = richbuffer::kind_at(buffer, from.line());
            buffer.apply_tag_by_name(richbuffer::block_tag(kind), &from, &to);
            *self.typing.borrow_mut() = Some((to.offset(), style, link));
        }
        richbuffer::renumber(buffer);
        self.busy.set(false);
    }

    /// Forgets a style chosen for a place the cursor has left.
    fn cursor_moved(&self) {
        let cursor = self.buffer.iter_at_mark(&self.buffer.get_insert());
        let stale = self
            .typing
            .borrow()
            .as_ref()
            .is_some_and(|(offset, _, _)| *offset != cursor.offset());
        if stale {
            *self.typing.borrow_mut() = None;
        }
    }
}

/// Whether every character of the selection carries `tag`.
fn whole_selection_has(buffer: &gtk::TextBuffer, tag: &str) -> bool {
    let Some((start, end)) = buffer.selection_bounds() else {
        return false;
    };
    let Some(tag) = buffer.tag_table().lookup(tag) else {
        return false;
    };
    let mut iter = start;
    while iter < end {
        if !iter.has_tag(&tag) {
            return false;
        }
        iter.forward_char();
    }
    true
}

/// Adds or removes a list or quote prefix on every line the selection
/// touches, then selects the changed lines.
fn prefix_lines(buffer: &gtk::TextBuffer, prefix: LinePrefix) {
    let (mut start, mut end) = buffer.selection_bounds().unwrap_or_else(|| {
        let cursor = buffer.iter_at_mark(&buffer.get_insert());
        (cursor, cursor)
    });
    start.set_line_offset(0);
    if !end.ends_line() {
        end.forward_to_line_end();
    }
    let text = buffer.text(&start, &end, false).to_string();
    let changed = toggle_prefix(&text, prefix);
    buffer.begin_user_action();
    let offset = start.offset();
    buffer.delete(&mut start, &mut end);
    buffer.insert(&mut start, &changed);
    let first = buffer.iter_at_offset(offset);
    let last = buffer.iter_at_offset(offset + changed.chars().count() as i32);
    buffer.select_range(&first, &last);
    buffer.end_user_action();
}

/// Puts Markdown markers around the selection, or around the cursor when
/// nothing is selected. A link leaves the cursor between its parentheses.
/// Pressed again right before the closing marker, moves past it.
fn wrap_selection(buffer: &gtk::TextBuffer, before: &str, after: &str) {
    let (mut start, mut end) = buffer.selection_bounds().unwrap_or_else(|| {
        let cursor = buffer.iter_at_mark(&buffer.get_insert());
        (cursor, cursor)
    });
    let selected = start != end;
    // Pressed again inside empty markers: step out, or into a link's URL.
    if !selected {
        let mut ahead = start;
        ahead.forward_chars(after.chars().count() as i32);
        if buffer.text(&start, &ahead, false) == after {
            let step = if after == "]()" {
                2
            } else {
                after.len() as i32
            };
            buffer.place_cursor(&buffer.iter_at_offset(start.offset() + step));
            return;
        }
    }
    let text = buffer.text(&start, &end, false).to_string();
    buffer.begin_user_action();
    buffer.delete(&mut start, &mut end);
    let offset = start.offset();
    buffer.insert(&mut start, &format!("{before}{text}{after}"));
    let cursor = match (selected, after) {
        (true, "]()") => offset + (before.len() + text.chars().count() + 2) as i32,
        (true, _) => offset + (before.len() + text.chars().count() + after.len()) as i32,
        (false, _) => offset + before.len() as i32,
    };
    buffer.place_cursor(&buffer.iter_at_offset(cursor));
    buffer.end_user_action();
}

/// Dims lines that start with `>`, so quoted text reads as quoted while
/// the body is Markdown.
fn style_quotes(buffer: &gtk::TextBuffer) {
    buffer.remove_tag_by_name("quote", &buffer.start_iter(), &buffer.end_iter());
    for line in 0..buffer.line_count() {
        let Some(start) = buffer.iter_at_line(line) else {
            continue;
        };
        let mut end = start;
        if !end.ends_line() {
            end.forward_to_line_end();
        }
        if buffer
            .text(&start, &end, false)
            .trim_start()
            .starts_with('>')
        {
            buffer.apply_tag_by_name("quote", &start, &end);
        }
    }
}

/// The editor's checks. They run from the one GTK test in `richbuffer`,
/// because GTK belongs to the thread that starts it and the test harness
/// gives each test a thread of its own.
#[cfg(test)]
pub(super) mod checks {
    use super::*;

    pub fn run() {
        a_style_goes_on_the_selection_and_comes_off_again();
        a_style_chosen_at_the_cursor_goes_on_what_is_typed();
        lines_become_lists_and_headings();
        enter_carries_a_list_on_and_leaves_it_on_an_empty_item();
        a_link_goes_where_the_held_words_were();
        the_body_moves_to_markdown_and_back();
    }

    fn editor(markdown: &str) -> (gtk::TextView, Rc<Editor>) {
        let view = gtk::TextView::new();
        let editor = Editor::new(&view, ComposeFormat::Rich);
        editor.fill(markdown, None, &[]);
        (view, editor)
    }

    fn select(editor: &Editor, from: i32, to: i32) {
        let buffer = &editor.buffer;
        buffer.select_range(&buffer.iter_at_offset(from), &buffer.iter_at_offset(to));
    }

    fn cursor_at(editor: &Editor, offset: i32) {
        editor
            .buffer
            .place_cursor(&editor.buffer.iter_at_offset(offset));
    }

    fn a_style_goes_on_the_selection_and_comes_off_again() {
        let (_view, editor) = editor("plain words");
        select(&editor, 6, 11);
        editor.toggle("bold");
        assert_eq!(editor.markdown(), "plain **words**");
        assert!(editor.style_here().bold);
        editor.toggle("bold");
        assert_eq!(editor.markdown(), "plain words");
    }

    fn a_style_chosen_at_the_cursor_goes_on_what_is_typed() {
        let (_view, editor) = editor("plain");
        cursor_at(&editor, 5);
        editor.toggle("italic");
        assert!(editor.style_here().italic);
        editor.buffer.insert_at_cursor(" leaning");
        assert_eq!(editor.markdown(), "plain *leaning*");
    }

    fn lines_become_lists_and_headings() {
        let (_view, editor) = editor("one\ntwo\nthree");
        select(&editor, 0, 9);
        editor.list(BlockKind::Numbered);
        assert_eq!(editor.rich().to_plain(), "1. one\n2. two\n3. three");
        editor.list(BlockKind::Numbered);
        assert_eq!(editor.rich().to_plain(), "one\ntwo\nthree");
        cursor_at(&editor, 0);
        editor.list(BlockKind::Bullet);
        assert_eq!(editor.rich().to_plain(), "- one\ntwo\nthree");
        cursor_at(&editor, 12);
        editor.set_block(BlockKind::Heading(1));
        let kinds: Vec<BlockKind> = editor.rich().blocks.iter().map(|b| b.kind).collect();
        assert_eq!(
            kinds,
            [
                BlockKind::Bullet,
                BlockKind::Paragraph,
                BlockKind::Heading(1)
            ]
        );
    }

    fn enter_carries_a_list_on_and_leaves_it_on_an_empty_item() {
        let (_view, editor) = editor("- soup");
        let end = editor.buffer.end_iter();
        editor.buffer.place_cursor(&end);
        assert!(editor.enter());
        editor.buffer.insert_at_cursor("salad");
        assert_eq!(editor.rich().to_plain(), "- soup\n- salad");
        assert!(editor.enter());
        // Enter on the empty item leaves the list.
        assert!(editor.enter());
        let kinds: Vec<BlockKind> = editor.rich().blocks.iter().map(|b| b.kind).collect();
        assert_eq!(
            kinds,
            [BlockKind::Bullet, BlockKind::Bullet, BlockKind::Paragraph]
        );
        // A plain line leaves Enter to the text view.
        let (_view, editor) = editor_plain();
        assert!(!editor.enter());
    }

    fn editor_plain() -> (gtk::TextView, Rc<Editor>) {
        editor("just words")
    }

    fn a_link_goes_where_the_held_words_were() {
        let (_view, editor) = editor("see the menu today");
        select(&editor, 4, 12);
        let held = editor.start_link().expect("rich text holds the words");
        assert_eq!(held.text, "the menu");
        // Typing before the words while the dialog is open moves them on,
        // and the marks go with them.
        let mut start = editor.buffer.start_iter();
        editor.buffer.insert(&mut start, "Do ");
        editor.link(held, "our menu", "https://e.com");
        assert_eq!(editor.markdown(), "Do see [our menu](https://e.com) today");
    }

    fn the_body_moves_to_markdown_and_back() {
        let (_view, editor) = editor("Hi **Ann**\n\n- soup\n- salad\n\n> quoted");
        let rich = editor.rich();
        editor.switch_format(ComposeFormat::Markdown, &[]);
        assert_eq!(editor.format(), ComposeFormat::Markdown);
        assert_eq!(editor.source(), rich.to_markdown());
        assert_eq!(editor.written().rich, None);
        // Markdown has no styles to show on the bar.
        assert_eq!(editor.style_here(), Style::default());
        editor.switch_format(ComposeFormat::Rich, &[]);
        assert_eq!(editor.rich(), rich);
        assert_eq!(editor.written().rich, Some(rich));
    }
}
