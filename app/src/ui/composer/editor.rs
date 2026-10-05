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

use super::removals::{Removal, Removals};
use super::richbuffer::{self, Anchors};
use crate::compose::{
    LineChange, LinePrefix, OutgoingAttachment, Unfolding, markdown_to_html, signature_change,
    toggle_prefix,
};
use crate::richtext::{Block, BlockKind, RichBody, Style};
use crate::settings::ComposeFormat;
use crate::stray_markdown;

/// The body read out once, in the two forms a draft keeps.
pub struct Written {
    pub markdown: String,
    /// The styled body, while the writer is in rich text.
    pub rich: Option<RichBody>,
}

/// Markdown pasted as styled text, held by marks while the toast that
/// offers the literal text instead is up.
pub struct Pasted {
    start: gtk::TextMark,
    end: gtk::TextMark,
    /// The text the clipboard held.
    markdown: String,
    /// The pasted lines as they first stood in the buffer, to tell them
    /// from lines the writer has edited since.
    shown: String,
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
    /// The tags of text deleted from a rich body, which Undo and Redo put
    /// back with the text, since GTK's undo history keeps no tags.
    removals: RefCell<Removals<gtk::TextTag>>,
    /// True while an Undo or Redo changes the buffer.
    replaying: Cell<bool>,
    /// Where the history the writer unfolded starts in the buffer, and
    /// its blocks, to style them again after a Redo.
    history_start: RefCell<Option<(gtk::TextMark, RichBody)>>,
    /// The first and last line an edit touched since the buffer last
    /// settled, so the upkeep after it looks at those and no others.
    touched: Cell<Option<(i32, i32)>>,
    /// Lines the per-keystroke upkeep looked at, for the checks.
    #[cfg(test)]
    looked_at: Cell<usize>,
    /// Times the styled body was read out of the buffer.
    #[cfg(test)]
    reads: Cell<usize>,
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
            removals: RefCell::new(Removals::default()),
            replaying: Cell::new(false),
            history_start: RefCell::new(None),
            touched: Cell::new(None),
            #[cfg(test)]
            looked_at: Cell::new(0),
            #[cfg(test)]
            reads: Cell::new(0),
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
                let breaks = text.matches('\n').count() as i32;
                editor.touch(at.line(), at.line() + breaks);
            }
        });
        // The line a deletion joins up is the one left to look at.
        let weak = Rc::downgrade(self);
        self.buffer.connect_delete_range(move |_, start, end| {
            if let Some(editor) = weak.upgrade() {
                editor.touch(start.line(), start.line());
                editor.keep_removal(start, end);
            }
        });
        for signal in ["undo", "redo"] {
            for (after, replaying) in [(false, true), (true, false)] {
                let weak = Rc::downgrade(self);
                self.buffer.connect_local(signal, after, move |_| {
                    if let Some(editor) = weak.upgrade() {
                        editor.replaying.set(replaying);
                    }
                    None
                });
            }
        }
        let weak = Rc::downgrade(self);
        self.buffer.connect_changed(move |_| {
            if let Some(editor) = weak.upgrade() {
                editor.after_edit();
            }
        });
        // After GTK's own handler, once the text is back.
        let weak = Rc::downgrade(self);
        self.buffer.connect_local("redo", true, move |_| {
            if let Some(editor) = weak.upgrade() {
                editor.restyle_history();
            }
            None
        });
        let weak = Rc::downgrade(self);
        self.buffer.connect_cursor_position_notify(move |_| {
            if let Some(editor) = weak.upgrade() {
                editor.cursor_moved();
            }
        });
    }

    /// Keeps the tags of rich text about to be deleted, for an Undo or a
    /// Redo that brings the text back.
    fn keep_removal(&self, start: &gtk::TextIter, end: &gtk::TextIter) {
        if self.format.get() != ComposeFormat::Rich {
            return;
        }
        self.removals.borrow_mut().removed(Removal {
            at: start.offset(),
            text: self.buffer.slice(start, end, true).to_string(),
            runs: richbuffer::tag_runs(start, end),
        });
    }

    /// Gives text an Undo or a Redo just put back the tags it had when it
    /// left. False when the text is not one the editor saw deleted.
    fn put_back(&self, offset: i32, length: i32) -> bool {
        let buffer = &self.buffer;
        let text = buffer.slice(
            &buffer.iter_at_offset(offset),
            &buffer.iter_at_offset(offset + length),
            true,
        );
        let runs = match self.removals.borrow().find(offset, &text) {
            Some(removal) => removal.runs.clone(),
            None => return false,
        };
        richbuffer::apply_runs(buffer, offset, &runs);
        true
    }

    fn touch(&self, first: i32, last: i32) {
        let touched = match self.touched.get() {
            Some((from, to)) => (from.min(first), to.max(last)),
            None => (first, last),
        };
        self.touched.set(Some(touched));
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
                style_quotes(&self.buffer, 0, self.buffer.line_count());
            }
        }
        self.buffer.place_cursor(&self.buffer.start_iter());
        // The text the draft replaced is beyond the reach of Undo.
        self.removals.borrow_mut().clear();
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
        richbuffer::renumber(buffer, first, last);
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
        // Marks hold the range while the markers before it go.
        let held = (
            buffer.create_mark(None, &start, true),
            buffer.create_mark(None, &end, false),
        );
        self.busy.set(true);
        buffer.begin_user_action();
        // The markers go first, while their tag still says which characters
        // they are. Taking the tags off first left "• " and "1. " behind as
        // words the writer had never typed.
        for line in first..=last {
            richbuffer::set_kind(buffer, line, BlockKind::Paragraph);
        }
        let (start, end) = (buffer.iter_at_mark(&held.0), buffer.iter_at_mark(&held.1));
        buffer.remove_all_tags(&start, &end);
        // The sweep took the paragraph tag as well, which the lines keep.
        for line in first..=last {
            richbuffer::set_kind(buffer, line, BlockKind::Paragraph);
        }
        richbuffer::renumber(buffer, first, last);
        buffer.end_user_action();
        self.busy.set(false);
        buffer.delete_mark(&held.0);
        buffer.delete_mark(&held.1);
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
        richbuffer::renumber(buffer, line - 1, line + 1);
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
            // Formatting a rich body again is one step of Undo, which gives
            // back the body as it was, styles and all.
            ComposeFormat::Rich if self.format.get() == ComposeFormat::Rich => {
                let body = RichBody::from_markdown(&self.source());
                let buffer = &self.buffer;
                self.busy.set(true);
                buffer.begin_user_action();
                let (mut start, mut end) = buffer.bounds();
                buffer.delete(&mut start, &mut end);
                self.anchors.borrow_mut().clear();
                richbuffer::insert(buffer, &body);
                buffer.end_user_action();
                self.busy.set(false);
            }
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
                self.removals.borrow_mut().clear();
                self.busy.set(false);
            }
            ComposeFormat::Markdown => {
                if self.format.get() == ComposeFormat::Markdown {
                    return;
                }
                let markdown = self.rich().to_markdown();
                self.removals.borrow_mut().clear();
                self.format.set(ComposeFormat::Markdown);
                self.busy.set(true);
                self.anchors.borrow_mut().clear();
                self.buffer.set_text(&markdown);
                style_quotes(&self.buffer, 0, self.buffer.line_count());
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

    /// Puts `markdown`, read as Markdown and styled, in place of the
    /// selection or at the cursor, as one step of Undo. The paste stays
    /// held until [`Editor::unpaste`] or [`Editor::let_go`].
    pub fn paste_markdown(&self, markdown: &str) -> Pasted {
        let buffer = &self.buffer;
        self.busy.set(true);
        buffer.begin_user_action();
        buffer.delete_selection(true, true);
        let start = buffer.create_mark(None, &buffer.iter_at_mark(&buffer.get_insert()), true);
        richbuffer::insert(buffer, &stray_markdown::styled(markdown));
        let end = buffer.create_mark(None, &buffer.iter_at_mark(&buffer.get_insert()), false);
        buffer.end_user_action();
        self.busy.set(false);
        let shown = self.between(&start, &end);
        Pasted {
            start,
            end,
            markdown: markdown.to_string(),
            shown,
        }
    }

    /// Puts the literal text of `pasted` back in place of its styled
    /// lines, as a plain paste would have left it. False, and nothing
    /// changed, when the writer has edited the pasted lines since.
    pub fn unpaste(&self, pasted: Pasted) -> bool {
        let buffer = &self.buffer;
        let same = self.between(&pasted.start, &pasted.end) == pasted.shown;
        if same {
            buffer.begin_user_action();
            let (mut from, mut to) = (
                buffer.iter_at_mark(&pasted.start),
                buffer.iter_at_mark(&pasted.end),
            );
            buffer.delete(&mut from, &mut to);
            buffer.insert(&mut from, &pasted.markdown);
            buffer.place_cursor(&from);
            buffer.end_user_action();
        }
        self.let_go(pasted);
        same
    }

    /// Stops holding a paste.
    pub fn let_go(&self, pasted: Pasted) {
        self.buffer.delete_mark(&pasted.start);
        self.buffer.delete_mark(&pasted.end);
    }

    fn between(&self, start: &gtk::TextMark, end: &gtk::TextMark) -> String {
        let buffer = &self.buffer;
        buffer
            .slice(&buffer.iter_at_mark(start), &buffer.iter_at_mark(end), true)
            .to_string()
    }

    /// Whether the writer's words in a rich body still hold Markdown that
    /// nobody formatted, such as marks typed by hand.
    pub fn markdown_left(&self) -> bool {
        self.format.get() == ComposeFormat::Rich && stray_markdown::left_in(&self.written_lines())
    }

    /// Styles the Markdown left in the writer's words, as one step of
    /// Undo. Lines already styled, the signature and the quoted history
    /// stay as they are. False when there was nothing to format.
    pub fn format_stray_markdown(&self) -> bool {
        if self.format.get() != ComposeFormat::Rich {
            return false;
        }
        let found = stray_markdown::conversions(&self.written_lines());
        if found.is_empty() {
            return false;
        }
        let buffer = &self.buffer;
        // A cursor inside a run that is rewritten ends up after the new
        // lines, where the writer was typing.
        let cursor = buffer.create_mark(None, &buffer.iter_at_mark(&buffer.get_insert()), false);
        self.busy.set(true);
        buffer.begin_user_action();
        // From the bottom up, so the line numbers above stay true.
        for conversion in found.iter().rev() {
            let (first, last) = (
                conversion.lines.start as i32,
                conversion.lines.end as i32 - 1,
            );
            let mut from = buffer
                .iter_at_line(first)
                .unwrap_or_else(|| buffer.end_iter());
            let mut to = line_end(buffer, last);
            buffer.delete(&mut from, &mut to);
            buffer.place_cursor(&from);
            richbuffer::insert(buffer, &conversion.body);
        }
        buffer.end_user_action();
        self.busy.set(false);
        buffer.place_cursor(&buffer.iter_at_mark(&cursor));
        buffer.delete_mark(&cursor);
        true
    }

    /// The lines that may hold the writer's words: those above the history
    /// they unfolded, down to the first quoted line. Reading on through a
    /// long quote would cost a pause for lines that are never formatted.
    fn written_lines(&self) -> RichBody {
        let buffer = &self.buffer;
        let end = match self.history_present() {
            Some(true) => self
                .history_start
                .borrow()
                .as_ref()
                .map_or(buffer.line_count(), |(mark, _)| {
                    buffer.iter_at_mark(mark).line() + 1
                }),
            _ => buffer.line_count(),
        };
        let end = (0..end)
            .find(|line| richbuffer::kind_at(buffer, *line) == BlockKind::Quote)
            .map_or(end, |quote| quote + 1);
        richbuffer::read_until(buffer, &self.anchors.borrow(), end)
    }

    /// Adds the history the writer unfolded at the end of the body, one
    /// blank line under their words, as one step of Undo. It makes the same
    /// body `compose::unfolded_markdown` and `compose::unfolded_rich` make,
    /// so an unfolded quote left alone goes out as a folded one would. The
    /// cursor stays where it was.
    pub fn append_history(&self, history: &Unfolding) {
        let buffer = &self.buffer;
        let cursor = buffer.create_mark(None, &buffer.iter_at_mark(&buffer.get_insert()), true);
        self.busy.set(true);
        buffer.begin_user_action();
        // The blank lines and spaces that end the body go, as the trim in
        // `unfolded_markdown` drops them.
        let mut cut = buffer.end_iter();
        loop {
            let mut before = cut;
            if !before.backward_char() || !before.char().is_whitespace() {
                break;
            }
            cut = before;
        }
        let mut end = buffer.end_iter();
        buffer.delete(&mut cut, &mut end);
        // Where the history starts. It stays left of anything inserted at
        // that spot, the blank lines an Undo puts back included, so the
        // text after it is the history and nothing else.
        let start = buffer.create_mark(None, &cut, true);
        let old = self
            .history_start
            .replace(Some((start, history.rich.clone())));
        if let Some((old, _)) = old {
            buffer.delete_mark(&old);
        }
        match self.format.get() {
            ComposeFormat::Rich => {
                buffer.place_cursor(&buffer.end_iter());
                // The first empty block ends the writer's own line, the
                // second is the blank line above the history.
                let mut blocks = vec![Block::default(), Block::default()];
                blocks.extend(history.rich.blocks.iter().cloned());
                richbuffer::insert(buffer, &RichBody { blocks });
            }
            ComposeFormat::Markdown => {
                let mut at = buffer.end_iter();
                let first = at.line();
                buffer.insert(&mut at, &format!("\n\n{}", history.markdown));
                style_quotes(buffer, first, buffer.line_count());
            }
        }
        buffer.end_user_action();
        self.busy.set(false);
        buffer.place_cursor(&buffer.iter_at_mark(&cursor));
        buffer.delete_mark(&cursor);
    }

    /// Whether the history [`Editor::append_history`] added is still in
    /// the body: `false` once an Undo has taken it back out, `true` again
    /// after a Redo. `None` when nothing was unfolded here. The composer
    /// asks after each Undo and Redo, so the quote goes back to its fold
    /// rather than out of the message.
    pub fn history_present(&self) -> Option<bool> {
        let start = self.history_start.borrow().as_ref()?.0.clone();
        let buffer = &self.buffer;
        let from = buffer.iter_at_mark(&start);
        let text = buffer.text(&from, &buffer.end_iter(), false);
        Some(!text.trim().is_empty())
    }

    /// Gives the unfolded history its line kinds back after a Redo put its
    /// text back. GTK's undo history holds text and not tags, so the quote
    /// would otherwise return as plain paragraphs and go out that way.
    fn restyle_history(&self) {
        if self.history_present() != Some(true) {
            return;
        }
        let Some((start, rich)) = self.history_start.borrow().clone() else {
            return;
        };
        let buffer = &self.buffer;
        let line = buffer.iter_at_mark(&start).line();
        self.busy.set(true);
        match self.format.get() {
            // Line one of what went in ends the writer's line and line two
            // is blank; the history's blocks follow.
            ComposeFormat::Rich => {
                for (index, block) in rich.blocks.iter().enumerate() {
                    richbuffer::set_kind(buffer, line + 2 + index as i32, block.kind);
                }
            }
            ComposeFormat::Markdown => {
                style_quotes(buffer, line, buffer.line_count());
            }
        }
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

    /// Puts the signature of `new` in place of `old`'s, when the one under
    /// the writer's words is still `old` as it was left. Only those lines
    /// change, so the cursor stays where it was and Undo still reaches
    /// everything typed before. False when nothing changed.
    pub fn swap_signature(&self, old: &str, new: &str) -> bool {
        let Some(change) = signature_change(&self.lines_to_quote(), old, new) else {
            return false;
        };
        self.replace_lines(&change);
        true
    }

    /// The lines of the body as text, down to the first quoted one. A
    /// quoted line in rich text has no `>` of its own, so it gets one here
    /// for [`signature_change`] to recognise it by.
    fn lines_to_quote(&self) -> Vec<String> {
        let buffer = &self.buffer;
        let rich = self.format.get() == ComposeFormat::Rich;
        let mut lines = Vec::new();
        for line in 0..buffer.line_count() {
            let start = match rich {
                true => richbuffer::text_start(buffer, line),
                false => buffer
                    .iter_at_line(line)
                    .unwrap_or_else(|| buffer.end_iter()),
            };
            let text = buffer
                .text(&start, &line_end(buffer, line), false)
                .to_string();
            let quoted = match rich {
                true => richbuffer::kind_at(buffer, line) == BlockKind::Quote,
                false => text.trim_start().starts_with('>'),
            };
            match rich && quoted {
                true => lines.push(format!("> {text}")),
                false => lines.push(text),
            }
            if quoted {
                break;
            }
        }
        lines
    }

    /// Makes `change` to the buffer's lines as one step of Undo. In rich
    /// text each new line is read as Markdown, the way the signature first
    /// arrived, and becomes a paragraph.
    fn replace_lines(&self, change: &LineChange) {
        let buffer = &self.buffer;
        let count = buffer.line_count() as usize;
        let line_start = |line: usize| {
            buffer
                .iter_at_line(line as i32)
                .unwrap_or_else(|| buffer.end_iter())
        };
        let past = change.first + change.removed;
        // Removed lines that end the body leave no line after them to hold
        // the break, so the one before theirs goes instead.
        let to_end = past >= count;
        let mut start = line_start(change.first);
        let mut end = match to_end {
            true => buffer.end_iter(),
            false => line_start(past),
        };
        if to_end && change.lines.is_empty() && change.first > 0 {
            start.backward_char();
        }
        self.busy.set(true);
        buffer.begin_user_action();
        buffer.delete(&mut start, &mut end);
        let mut at = start;
        if change.first >= count && !change.lines.is_empty() {
            buffer.insert(&mut at, "\n");
        }
        for (index, line) in change.lines.iter().enumerate() {
            if index > 0 {
                buffer.insert(&mut at, "\n");
            }
            match self.format.get() {
                ComposeFormat::Rich => {
                    let body = RichBody::from_markdown(line);
                    let spans = body.blocks.first().map_or(&[][..], |b| &b.spans[..]);
                    richbuffer::insert_spans(buffer, &mut at, spans, BlockKind::Paragraph);
                }
                ComposeFormat::Markdown => buffer.insert(&mut at, line),
            }
        }
        if !to_end && !change.lines.is_empty() {
            buffer.insert(&mut at, "\n");
        }
        if self.format.get() == ComposeFormat::Markdown {
            let first = change.first as i32;
            style_quotes(buffer, first, first + change.lines.len() as i32);
        }
        buffer.end_user_action();
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
        #[cfg(test)]
        self.reads.set(self.reads.get() + 1);
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
    /// The body in both forms a draft keeps, read out of the buffer once:
    /// a long reply takes a fifth of a second to read.
    pub fn written(&self) -> Written {
        match self.format.get() {
            ComposeFormat::Rich => {
                let rich = self.rich();
                Written {
                    markdown: rich.to_markdown(),
                    rich: Some(rich),
                }
            }
            ComposeFormat::Markdown => Written {
                markdown: self.source(),
                rich: None,
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
        let touched = self.touched.take();
        if self.busy.get() {
            return;
        }
        let Some((first, last)) = touched else {
            return;
        };
        let buffer = &self.buffer;
        if self.format.get() == ComposeFormat::Markdown {
            self.busy.set(true);
            let _looked = style_quotes(buffer, first, last);
            #[cfg(test)]
            self.looked_at.set(self.looked_at.get() + _looked);
            self.busy.set(false);
            return;
        }
        self.busy.set(true);
        let replaying = self.replaying.get();
        for (offset, length) in ranges {
            if replaying && self.put_back(offset, length) {
                continue;
            }
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
        // Undo and Redo bring the markers back as text with the rest, and
        // a change of ours in between would leave GTK's history out of
        // step with the buffer.
        if !replaying {
            let _looked = richbuffer::renumber(buffer, first, last);
            #[cfg(test)]
            self.looked_at.set(self.looked_at.get() + _looked);
        }
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

/// Where `line` ends, before its line break.
fn line_end(buffer: &gtk::TextBuffer, line: i32) -> gtk::TextIter {
    let mut end = buffer
        .iter_at_line(line)
        .unwrap_or_else(|| buffer.end_iter());
    if !end.ends_line() {
        end.forward_to_line_end();
    }
    end
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

/// Dims the lines from `first` to `last` that start with `>`, so quoted
/// text reads as quoted while the body is Markdown. A line's look depends
/// on nothing but its own text, so an edit needs only its own lines done.
/// Returns how many lines it looked at.
fn style_quotes(buffer: &gtk::TextBuffer, first: i32, last: i32) -> usize {
    let last = last.min(buffer.line_count() - 1);
    let mut looked = 0;
    for line in first.max(0)..=last {
        let Some(start) = buffer.iter_at_line(line) else {
            continue;
        };
        looked += 1;
        let end = line_end(buffer, line);
        buffer.remove_tag_by_name("quote", &start, &end);
        if buffer
            .text(&start, &end, false)
            .trim_start()
            .starts_with('>')
        {
            buffer.apply_tag_by_name("quote", &start, &end);
        }
    }
    looked
}

/// The editor's checks. They run from the one GTK test in `richbuffer`,
/// because GTK belongs to the thread that starts it and the test harness
/// gives each test a thread of its own.
#[cfg(test)]
pub(super) mod checks {
    use super::*;
    use crate::compose::restyle_signature;

    pub fn run() {
        a_style_goes_on_the_selection_and_comes_off_again();
        a_style_chosen_at_the_cursor_goes_on_what_is_typed();
        lines_become_lists_and_headings();
        enter_carries_a_list_on_and_leaves_it_on_an_empty_item();
        a_link_goes_where_the_held_words_were();
        the_body_moves_to_markdown_and_back();
        clearing_formatting_takes_the_list_markers_too();
        a_new_signature_leaves_the_cursor_and_undo_alone();
        a_keystroke_looks_at_its_own_lines_only();
        the_body_is_read_once_for_a_draft();
        an_unfolded_quote_lands_where_the_folded_one_would_go();
        typed_markdown_formats_in_place_as_one_undo_step();
        format_keeps_styled_words_and_the_quote();
        pasted_markdown_arrives_styled_and_goes_back_to_plain();
        a_markdown_draft_opens_styled();
        undo_after_format_brings_the_earlier_styles_back();
        undo_after_format_markdown_brings_the_earlier_body_back();
        undo_brings_deleted_words_back_with_their_styles();
    }

    /// The Markdown bar's Format, undone in one step, gives back the body
    /// as it was, bold word and all, and Redo gives back the lists.
    fn undo_after_format_brings_the_earlier_styles_back() {
        let (_view, editor) = opened("Hi **Ann**.");
        editor.buffer.place_cursor(&editor.buffer.end_iter());
        editor.buffer.insert_at_cursor("\n- soup\n- salad");
        let before = editor.rich();
        assert!(editor.format_stray_markdown());
        let formatted = editor.rich();
        assert_eq!(formatted.blocks[2].kind, BlockKind::Bullet, "{formatted:?}");
        editor.buffer.undo();
        assert_eq!(editor.rich(), before, "{}", editor.markdown());
        editor.buffer.redo();
        assert_eq!(editor.rich(), formatted, "{}", editor.markdown());
        editor.buffer.undo();
        assert_eq!(editor.rich(), before, "{}", editor.markdown());
    }

    /// Format Markdown in a rich body is one step of Undo too, and the
    /// step puts back the styles the body had.
    fn undo_after_format_markdown_brings_the_earlier_body_back() {
        let (_view, editor) = opened("Hi **Ann**.");
        editor.buffer.place_cursor(&editor.buffer.end_iter());
        editor.buffer.insert_at_cursor("\n# Plan");
        let before = editor.rich();
        editor.switch_format(ComposeFormat::Rich, &[]);
        let formatted = editor.rich();
        assert_eq!(
            formatted.blocks[2].kind,
            BlockKind::Heading(1),
            "{formatted:?}"
        );
        editor.buffer.undo();
        assert_eq!(editor.rich(), before, "{}", editor.markdown());
        editor.buffer.redo();
        assert_eq!(editor.rich(), formatted, "{}", editor.markdown());
    }

    /// Words typed over, deleted with Backspace or with Delete come back
    /// on Undo with the styles they had.
    fn undo_brings_deleted_words_back_with_their_styles() {
        let (_view, editor) = opened("plain **words** end");
        let before = editor.rich();
        select(&editor, 6, 11);
        editor.buffer.begin_user_action();
        editor.buffer.delete_selection(true, true);
        editor.buffer.insert_interactive_at_cursor("x", true);
        editor.buffer.end_user_action();
        editor.buffer.undo();
        assert_eq!(editor.rich(), before, "{}", editor.markdown());

        let (_view, editor) = opened("plain **words**");
        let before = editor.rich();
        for _ in 0..5 {
            editor.buffer.begin_user_action();
            let mut end = editor.buffer.end_iter();
            editor.buffer.backspace(&mut end, true, true);
            editor.buffer.end_user_action();
        }
        while editor.buffer.can_undo() && editor.source() != "plain words" {
            editor.buffer.undo();
        }
        assert_eq!(editor.rich(), before, "{}", editor.markdown());

        let (_view, editor) = opened("**bold** *it*");
        let before = editor.rich();
        for _ in 0..4 {
            editor.buffer.begin_user_action();
            let (mut from, mut to) = (editor.buffer.start_iter(), editor.buffer.iter_at_offset(1));
            editor.buffer.delete_interactive(&mut from, &mut to, true);
            editor.buffer.end_user_action();
        }
        assert_eq!(editor.source(), " it");
        while editor.buffer.can_undo() && editor.source() != "bold it" {
            editor.buffer.undo();
        }
        assert_eq!(editor.rich(), before, "{}", editor.markdown());
    }

    fn typed_markdown_formats_in_place_as_one_undo_step() {
        let typed = "Hi Ann,\n\n# Plan\n\n- soup\n- salad";
        let (_view, editor) = opened("");
        editor.buffer.insert_at_cursor(typed);
        assert!(editor.markdown_left());
        assert!(editor.format_stray_markdown());
        assert!(!editor.markdown_left());
        assert_eq!(richbuffer::kind_at(&editor.buffer, 2), BlockKind::Heading(1));
        assert_eq!(richbuffer::kind_at(&editor.buffer, 5), BlockKind::Bullet);
        assert_eq!(editor.rich().to_plain(), "Hi Ann,\n\nPlan\n\n- soup\n- salad");
        // The cursor stays at the end, where the writer was typing.
        let cursor = editor.buffer.iter_at_mark(&editor.buffer.get_insert());
        assert!(cursor.is_end(), "{}", cursor.offset());
        editor.buffer.undo();
        assert_eq!(editor.source(), typed);
        // Nothing left to format is no change at all.
        let (_view, editor) = opened("Hi Ann,\n\nSee you Friday - bring soup.");
        assert!(!editor.markdown_left());
        assert!(!editor.format_stray_markdown());
    }

    fn format_keeps_styled_words_and_the_quote() {
        let (_view, editor) = opened("Hi **Ann**.\n\nOn Monday, Ann wrote:\n\n> \\- one\n> \\- two\n> \\# three");
        assert_eq!(richbuffer::kind_at(&editor.buffer, 4), BlockKind::Quote);
        let quoted_before: Vec<Block> = editor.rich().blocks[1..].to_vec();
        cursor_at(&editor, 7);
        editor.buffer.insert_at_cursor("\n- soup\n- salad");
        assert!(editor.format_stray_markdown());
        let after = editor.rich();
        assert!(after.blocks[0].spans[1].style.bold, "{after:?}");
        // A list sets itself apart from the paragraph above it, as it
        // does in any body read from Markdown.
        assert!(after.blocks[1].is_blank(), "{after:?}");
        assert_eq!(after.blocks[2].kind, BlockKind::Bullet, "{after:?}");
        assert_eq!(after.blocks[3].kind, BlockKind::Bullet, "{after:?}");
        assert_eq!(after.blocks[4..].to_vec(), quoted_before);
    }

    fn pasted_markdown_arrives_styled_and_goes_back_to_plain() {
        let markdown = "# Plan\n\n- soup\n- salad";
        let (_view, editor) = opened("Hi");
        cursor_at(&editor, 2);
        editor.buffer.insert_at_cursor("\n");
        let pasted = editor.paste_markdown(markdown);
        assert_eq!(richbuffer::kind_at(&editor.buffer, 1), BlockKind::Heading(1));
        assert_eq!(editor.rich().to_plain(), "Hi\nPlan\n\n- soup\n- salad");
        assert!(!editor.markdown_left());
        assert!(editor.unpaste(pasted));
        assert_eq!(editor.source(), format!("Hi\n{markdown}"));
        assert!(editor.markdown_left());
        // One Undo takes the plain text out, and one more the whole paste.
        editor.buffer.undo();
        editor.buffer.undo();
        assert_eq!(editor.source(), "Hi\n");
        // A paste the writer has since edited stays as it is.
        let (_view, editor) = opened("");
        let pasted = editor.paste_markdown(markdown);
        cursor_at(&editor, 2);
        editor.buffer.insert_at_cursor("x");
        assert!(!editor.unpaste(pasted));
    }

    fn a_markdown_draft_opens_styled() {
        // What the assistant and a template hand over: Markdown, which a
        // rich body shows styled.
        let (_view, editor) = opened("Hi,\n\n## Agenda\n\n1. **Budget**\n2. [Plan](https://e.com)");
        assert!(!editor.markdown_left());
        assert_eq!(richbuffer::kind_at(&editor.buffer, 2), BlockKind::Heading(2));
        assert_eq!(richbuffer::kind_at(&editor.buffer, 4), BlockKind::Numbered);
    }

    fn an_unfolded_quote_lands_where_the_folded_one_would_go() {
        use crate::compose::{unfolded_markdown, unfolded_rich};

        let quoted = "On Monday, Ann wrote:\n> hi\n>\n> there";
        let history = Unfolding {
            markdown: quoted.into(),
            rich: RichBody::from_markdown(quoted),
        };
        for typed in ["Thanks.\n\n-- \nDana\n\n", "\n\n", ""] {
            for format in [ComposeFormat::Rich, ComposeFormat::Markdown] {
                let view = gtk::TextView::new();
                let editor = Editor::new(&view, format);
                editor.fill(typed, None, &[]);
                cursor_at(&editor, 3);
                let cursor = editor
                    .buffer
                    .iter_at_mark(&editor.buffer.get_insert())
                    .offset();
                let before = editor.written();
                editor.append_history(&history);
                let after = editor.written();
                match format {
                    ComposeFormat::Markdown => assert_eq!(
                        after.markdown,
                        unfolded_markdown(&before.markdown, quoted),
                        "{typed:?}"
                    ),
                    ComposeFormat::Rich => assert_eq!(
                        after.rich,
                        Some(unfolded_rich(before.rich.as_ref().unwrap(), &history.rich)),
                        "{typed:?}"
                    ),
                }
                // The cursor stays among the words. In a body of blank lines
                // it sat in what the trim took, and goes to the top.
                let now = editor.buffer.iter_at_mark(&editor.buffer.get_insert());
                let wanted = if typed.trim().is_empty() { 0 } else { cursor };
                assert_eq!(now.offset(), wanted, "{format:?} {typed:?}");
                assert_eq!(editor.history_present(), Some(true), "{format:?} {typed:?}");
                // One Undo takes the whole quote back out, and says so, for
                // the composer to fold it again.
                editor.buffer.undo();
                assert_eq!(editor.written().markdown, before.markdown, "{format:?} {typed:?}");
                assert_eq!(editor.history_present(), Some(false), "{format:?} {typed:?}");
                // Folded again, the reply sends as it did before the unfold.
                let folded = |written: &Written| {
                    let mut draft = crate::compose::Draft::new(
                        1,
                        mailrs_domain::Address {
                            name: None,
                            email: "dana@example.com".into(),
                        },
                    );
                    draft.markdown = written.markdown.clone();
                    draft.rich = written.rich.clone();
                    draft.quoted = Some(quoted.to_string());
                    let raw = crate::compose::build_mime(&draft, 0, "id@example.com").unwrap();
                    let parsed = mail_parser::MessageParser::default().parse(&raw).unwrap();
                    (
                        parsed.body_text(0).map(|t| t.to_string()),
                        parsed.body_html(0).map(|h| h.to_string()),
                    )
                };
                assert_eq!(folded(&editor.written()), folded(&before), "{format:?} {typed:?}");
                // Redo puts it back in the body.
                editor.buffer.redo();
                assert_eq!(editor.written().markdown, after.markdown, "{format:?} {typed:?}");
                assert_eq!(editor.history_present(), Some(true), "{format:?} {typed:?}");
            }
        }
    }

    /// A long quoted reply: a line to write on, and 3,000 quoted lines.
    fn long_reply() -> String {
        let mut body = String::from("Hi Ann,\n\nOn Monday, Ann wrote:\n");
        for n in 0..3000 {
            body.push_str(&format!("> line {n}\n"));
        }
        body
    }

    fn a_keystroke_looks_at_its_own_lines_only() {
        for format in [ComposeFormat::Rich, ComposeFormat::Markdown] {
            let view = gtk::TextView::new();
            let editor = Editor::new(&view, format);
            editor.fill(&long_reply(), None, &[]);
            cursor_at(&editor, 3);
            editor.looked_at.set(0);
            editor.buffer.insert_at_cursor("x");
            assert!(
                editor.looked_at.get() < 5,
                "{format:?} looked at {} lines for one letter",
                editor.looked_at.get()
            );
        }
        // Lines still read as quoted and lists still count after edits.
        let (_view, editor) = opened("1. one\n2. two\n3. three\n\nafter");
        // Past the marker and the first letter.
        cursor_at(&editor, 4);
        editor.enter();
        editor.buffer.insert_at_cursor("half");
        assert_eq!(
            editor.rich().to_plain(),
            "1. o\n2. halfne\n3. two\n4. three\n\nafter"
        );
        let view = gtk::TextView::new();
        let editor = Editor::new(&view, ComposeFormat::Markdown);
        editor.fill("plain\nwords", None, &[]);
        cursor_at(&editor, 6);
        editor.buffer.insert_at_cursor("> ");
        assert_eq!(richbuffer::kind_at(&editor.buffer, 1), BlockKind::Quote);
        assert_eq!(richbuffer::kind_at(&editor.buffer, 0), BlockKind::Paragraph);
    }

    fn the_body_is_read_once_for_a_draft() {
        let (_view, editor) = opened(&long_reply());
        editor.reads.set(0);
        let written = editor.written();
        assert_eq!(editor.reads.get(), 1);
        assert_eq!(written.markdown, written.rich.unwrap().to_markdown());
    }

    fn opened(markdown: &str) -> (gtk::TextView, Rc<Editor>) {
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
        let (_view, editor) = opened("plain words");
        select(&editor, 6, 11);
        editor.toggle("bold");
        assert_eq!(editor.markdown(), "plain **words**");
        assert!(editor.style_here().bold);
        editor.toggle("bold");
        assert_eq!(editor.markdown(), "plain words");
    }

    fn a_style_chosen_at_the_cursor_goes_on_what_is_typed() {
        let (_view, editor) = opened("plain");
        cursor_at(&editor, 5);
        editor.toggle("italic");
        assert!(editor.style_here().italic);
        editor.buffer.insert_at_cursor(" leaning");
        assert_eq!(editor.markdown(), "plain *leaning*");
    }

    fn lines_become_lists_and_headings() {
        let (_view, editor) = opened("one\ntwo\nthree");
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
        let (_view, editor) = opened("- soup");
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
        let (_view, editor) = opened("just words");
        assert!(!editor.enter());
    }

    fn a_link_goes_where_the_held_words_were() {
        let (_view, editor) = opened("see the menu today");
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
        let (_view, editor) = opened("Hi **Ann**\n\n- soup\n- salad\n\n> quoted");
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

    fn clearing_formatting_takes_the_list_markers_too() {
        let (_view, editor) = opened("- **one**\n- two\n\n1. first\n2. second");
        editor.clear();
        assert_eq!(editor.rich().to_plain(), "one\ntwo\n\nfirst\nsecond");
        assert!(
            editor
                .rich()
                .blocks
                .iter()
                .all(|b| b.kind == BlockKind::Paragraph
                    && b.spans.iter().all(|s| s.style == Style::default()))
        );
        // Part of a list: the numbers after it count again from one.
        let (_view, editor) = opened("1. one\n2. two\n3. three");
        select(&editor, 3, 5);
        editor.clear();
        assert_eq!(editor.rich().to_plain(), "one\n1. two\n2. three");
    }

    fn a_new_signature_leaves_the_cursor_and_undo_alone() {
        const BODY: &str = "Hi Ann,\n\n-- \nDana\n\nOn Monday, Ann wrote:\n> hi";
        for format in [ComposeFormat::Rich, ComposeFormat::Markdown] {
            let view = gtk::TextView::new();
            let editor = Editor::new(&view, format);
            editor.fill(BODY, None, &[]);
            cursor_at(&editor, 6);
            editor.buffer.begin_user_action();
            editor.buffer.insert_at_cursor("!");
            editor.buffer.end_user_action();
            let before = editor.markdown();
            let wanted = restyle_signature(&before, "Dana", "Dana Reyes\nSales");

            assert!(editor.swap_signature("Dana", "Dana Reyes\nSales"));
            match format {
                ComposeFormat::Markdown => assert_eq!(editor.markdown(), wanted),
                ComposeFormat::Rich => assert_eq!(
                    editor.rich().to_plain(),
                    RichBody::from_markdown(&wanted).to_plain()
                ),
            }
            let cursor = editor.buffer.iter_at_mark(&editor.buffer.get_insert());
            assert_eq!(cursor.offset(), 7, "{format:?}");
            assert!(editor.buffer.can_undo(), "{format:?}");
            // A signature the writer rewrote stays as they wrote it.
            assert!(!editor.swap_signature("Dana", "Sales"), "{format:?}");
        }
    }
}
