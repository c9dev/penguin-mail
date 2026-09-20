//! Spell check while writing.
//!
//! Penguin Mail checks the words itself rather than handing the text view to
//! a spell-check widget. Two reasons. Whole stretches of a draft are not the
//! writer's prose: a quoted reply, a code block, a list marker GTK drew, a
//! run of inline code. And a squiggle has to be a mark on top of the text,
//! not a change to it, because the text is what gets sent.
//!
//! So the squiggle is a [`gtk::TextTag`] named `misspelled` with Pango's
//! error underline, and it lies beside the characters rather than in them.
//! [`richbuffer::read`] builds the outgoing body from the block kinds, the
//! four style tags and the link tags, and asks about nothing else, so a tag
//! this module applies cannot reach the HTML or the plain text part.
//!
//! What counts as prose comes from the buffer, not from reading the text:
//! [`richbuffer::kind_at`] gives every line its kind in rich text and in
//! Markdown alike, and the tags on a character say whether it is a marker,
//! inline code, or a picture.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gio, glib, pango};

use super::richbuffer;
use crate::richtext::BlockKind;

/// The tag that draws the squiggle.
const TAG: &str = "misspelled";

/// How long the typing has to stop before the buffer is checked again.
const SETTLE_MS: u32 = 180;

/// How many corrections the menu offers.
const SUGGESTIONS: usize = 6;

/// Hunspell dictionaries, plus the words this person told Penguin Mail to
/// accept. Every composer shares one of these, because reading a dictionary
/// costs long enough that doing it twice would show.
pub struct Dictionaries {
    loaded: Vec<spellbook::Dictionary>,
    /// Words from Add to Dictionary, which Preferences keeps.
    known: RefCell<HashSet<String>>,
    /// Words from Ignore, which last until the app closes.
    ignored: RefCell<HashSet<String>>,
}

impl Dictionaries {
    /// Reads a Hunspell dictionary for each of `languages`, skipping any it
    /// cannot find or parse. Slow enough to belong on a worker thread.
    pub fn load(languages: &[String], known: &[String]) -> Dictionaries {
        let mut loaded = Vec::new();
        for language in languages {
            let Some((aff, dic)) = dictionary_files(language) else {
                tracing::info!(
                    language,
                    "no dictionary installed; not checking this language"
                );
                continue;
            };
            match (std::fs::read_to_string(&aff), std::fs::read_to_string(&dic)) {
                (Ok(aff), Ok(dic)) => match spellbook::Dictionary::new(&aff, &dic) {
                    Ok(dictionary) => loaded.push(dictionary),
                    Err(err) => tracing::warn!(language, error = %err, "unreadable dictionary"),
                },
                _ => tracing::warn!(language, "could not read the dictionary files"),
            }
        }
        Dictionaries {
            loaded,
            known: RefCell::new(known.iter().map(|w| w.to_lowercase()).collect()),
            ignored: RefCell::new(HashSet::new()),
        }
    }

    /// With no dictionary installed the composer draws nothing at all.
    pub fn is_empty(&self) -> bool {
        self.loaded.is_empty()
    }

    /// Whether any dictionary, or the person, accepts `word`.
    pub fn accepts(&self, word: &str) -> bool {
        let lower = word.to_lowercase();
        self.known.borrow().contains(&lower)
            || self.ignored.borrow().contains(&lower)
            || self.loaded.iter().any(|d| d.check(word))
    }

    /// Corrections to offer for `word`, best first, across every language.
    pub fn suggest(&self, word: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for dictionary in &self.loaded {
            let mut from_this = Vec::new();
            dictionary.suggest(word, &mut from_this);
            for suggestion in from_this {
                if !out.iter().any(|s| s == &suggestion) {
                    out.push(suggestion);
                }
            }
        }
        out.truncate(SUGGESTIONS);
        out
    }

    /// Accepts `word` from now on. Preferences keeps the list.
    pub fn remember(&self, word: &str) {
        self.known.borrow_mut().insert(word.to_lowercase());
    }

    /// Accepts `word` until the app closes.
    pub fn ignore(&self, word: &str) {
        self.ignored.borrow_mut().insert(word.to_lowercase());
    }
}

/// Where dictionaries live: `$DICPATH` first, then the user's own, then the
/// two directories Debian and Fedora put Hunspell files in.
fn dictionary_paths() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::env::var_os("DICPATH")
        .iter()
        .flat_map(std::env::split_paths)
        .collect();
    if let Some(home) = std::env::var_os("HOME") {
        paths.push(Path::new(&home).join(".local/share/hunspell"));
    }
    paths.push(PathBuf::from("/usr/share/hunspell"));
    paths.push(PathBuf::from("/usr/share/myspell/dicts"));
    paths
}

/// The `.aff` and `.dic` pair for `language`, if both are there.
fn dictionary_files(language: &str) -> Option<(PathBuf, PathBuf)> {
    dictionary_paths().into_iter().find_map(|dir| {
        let aff = dir.join(format!("{language}.aff"));
        let dic = dir.join(format!("{language}.dic"));
        (aff.is_file() && dic.is_file()).then_some((aff, dic))
    })
}

/// Every language a dictionary is installed for, such as `en_US` and
/// `pt_BR`, sorted and without repeats.
pub fn installed_languages() -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for dir in dictionary_paths() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if path.extension().is_some_and(|e| e == "dic")
                && dir.join(format!("{stem}.aff")).is_file()
                && !names.iter().any(|n| n == stem)
            {
                names.push(stem.to_string());
            }
        }
    }
    names.sort();
    names
}

/// The dictionary tag the app's locale asks for, such as `pt_BR` from
/// `pt_BR.UTF-8`. `en_US` when the environment says nothing.
pub fn locale_language() -> String {
    for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        let Ok(value) = std::env::var(key) else {
            continue;
        };
        let tag = value
            .split(['.', '@'])
            .next()
            .unwrap_or("")
            .replace('-', "_");
        if !tag.is_empty() && tag != "C" && tag != "POSIX" {
            return tag;
        }
    }
    "en_US".to_string()
}

/// Which dictionaries to read, given what the account asks for and what is
/// installed. `wanted` is the account's override, empty to follow `locale`.
///
/// A request for `pt_BR` settles for `pt_PT` when only that is installed,
/// because a Portuguese dictionary is far better than none. A request for a
/// language with nothing installed drops out, and an empty answer means the
/// composer shows no squiggles at all.
pub fn languages_to_load(wanted: &[String], locale: &str, installed: &[String]) -> Vec<String> {
    let asked: Vec<&str> = match wanted.is_empty() {
        true => vec![locale],
        false => wanted.iter().map(String::as_str).collect(),
    };
    let mut out: Vec<String> = Vec::new();
    for tag in asked {
        let base = tag.split('_').next().unwrap_or(tag);
        let best = installed
            .iter()
            .find(|i| i.eq_ignore_ascii_case(tag))
            .or_else(|| installed.iter().find(|i| *i == base))
            .or_else(|| installed.iter().find(|i| i.split('_').next() == Some(base)));
        if let Some(found) = best
            && !out.contains(found)
        {
            out.push(found.clone());
        }
    }
    out
}

/// Whether a line of this kind holds the writer's own prose. A quoted
/// reply is someone else's words, and a code block is not prose at all, so
/// neither gets a squiggle. The buffer carries the kind on every line, in
/// rich text and in Markdown alike, so nothing here has to read the text to
/// find out.
pub fn checks_prose(kind: BlockKind) -> bool {
    !matches!(kind, BlockKind::Quote | BlockKind::Code)
}

/// Whether a line opens or closes a fenced code block. Rich text gives
/// fenced code its own kind, but Markdown keeps the fences as text, so the
/// lines between them have to be counted out.
fn is_fence(line: &str) -> bool {
    let line = line.trim_start();
    line.starts_with("```") || line.starts_with("~~~")
}

/// Whether the character at `iter` is prose: not a list marker the writer
/// cannot edit, not a run of inline code, and not a picture.
fn is_prose(iter: &gtk::TextIter) -> bool {
    if iter.child_anchor().is_some() {
        return false;
    }
    !iter.tags().iter().any(|tag| {
        matches!(
            tag.name().as_deref(),
            Some(richbuffer::MARKER) | Some("code")
        )
    })
}

/// Every stretch of prose in `buffer`, as the pair of iters around it.
///
/// One walk covers both ways of writing: the line kinds rule out quotes and
/// code blocks, the tags rule out list markers, inline code and pictures,
/// and what is left is what a dictionary should see.
fn prose_runs(buffer: &gtk::TextBuffer) -> Vec<(gtk::TextIter, gtk::TextIter)> {
    let mut runs = Vec::new();
    let mut fenced = false;
    for line in 0..buffer.line_count() {
        let Some(start) = buffer.iter_at_line(line) else {
            continue;
        };
        let mut last = start;
        if !last.ends_line() {
            last.forward_to_line_end();
        }
        if is_fence(&buffer.text(&start, &last, false)) {
            fenced = !fenced;
            continue;
        }
        if fenced || !checks_prose(richbuffer::kind_at(buffer, line)) {
            continue;
        }
        let mut iter = start;
        let mut run: Option<gtk::TextIter> = None;
        while iter < last {
            match is_prose(&iter) {
                true => {
                    run.get_or_insert(iter);
                }
                false => {
                    if let Some(from) = run.take() {
                        runs.push((from, iter));
                    }
                }
            }
            iter.forward_char();
        }
        if let Some(from) = run {
            runs.push((from, last));
        }
    }
    runs
}

/// The words in `text` a dictionary should judge, with their byte offsets.
///
/// Whitespace splits the text into tokens first, and a token carrying a
/// digit, an underscore, an `@`, or a slash is dropped whole: it is an
/// address, a path, or an identifier, and the dictionary would mark every
/// part of it. Only then does a token break into words.
pub fn words_in(text: &str) -> Vec<(Range<usize>, &str)> {
    let mut words = Vec::new();
    let mut at = 0;
    for token in text.split_inclusive(char::is_whitespace) {
        let start = at;
        at += token.len();
        if token.contains(['@', '_', '/', '\\']) || token.chars().any(char::is_numeric) {
            continue;
        }
        let mut from: Option<usize> = None;
        for (offset, c) in token.char_indices() {
            // An apostrophe holds a word together, as in "doesn't", but only
            // between letters: the ones around a quotation are punctuation.
            let inside = matches!(c, '\'' | '\u{2019}') && from.is_some();
            if c.is_alphabetic() || inside {
                from.get_or_insert(offset);
            } else if let Some(word_at) = from.take() {
                push_word(token, word_at, offset, start, &mut words);
            }
        }
        if let Some(word_at) = from {
            push_word(token, word_at, token.len(), start, &mut words);
        }
    }
    words
}

/// Keeps `token[from..to]` as a word, minus any trailing apostrophe.
fn push_word<'a>(
    token: &'a str,
    from: usize,
    to: usize,
    offset: usize,
    out: &mut Vec<(Range<usize>, &'a str)>,
) {
    let word = token[from..to].trim_end_matches(['\'', '\u{2019}']);
    // One letter is "I" or "a". Neither is worth a squiggle.
    if word.chars().count() >= 2 {
        out.push((offset + from..offset + from + word.len(), word));
    }
}

/// Underlines the misspellings in a composer's text view and offers
/// corrections on right-click.
pub struct SpellCheck {
    view: gtk::TextView,
    dictionaries: Rc<Dictionaries>,
    /// The word the context menu was opened on, as character offsets.
    at: Cell<(i32, i32)>,
    /// Rises with each keystroke, so only the last pause checks the buffer.
    generation: Cell<u64>,
    /// Told about every word Add to Dictionary keeps, so Preferences can
    /// save it.
    on_learn: Box<dyn Fn(&str)>,
}

impl SpellCheck {
    /// Starts checking `view`. Does nothing at all when no dictionary was
    /// found, so a machine without one shows a plain text view and no error.
    pub fn attach(
        view: &gtk::TextView,
        dictionaries: Rc<Dictionaries>,
        on_learn: impl Fn(&str) + 'static,
    ) -> Option<Rc<SpellCheck>> {
        if dictionaries.is_empty() {
            return None;
        }
        let buffer = view.buffer();
        if buffer.tag_table().lookup(TAG).is_none() {
            // Added after the composer's own tags, so the squiggle draws over
            // them: bold, quoted, and misspelled all show at once.
            buffer.tag_table().add(
                &gtk::TextTag::builder()
                    .name(TAG)
                    .underline(pango::Underline::Error)
                    .build(),
            );
        }
        let spell = Rc::new(SpellCheck {
            view: view.clone(),
            dictionaries,
            at: Cell::new((0, 0)),
            generation: Cell::new(0),
            on_learn: Box::new(on_learn),
        });
        let weak = Rc::downgrade(&spell);
        buffer.connect_changed(move |_| {
            if let Some(spell) = weak.upgrade() {
                spell.recheck_soon();
            }
        });
        spell.wire_menu();
        spell.recheck();
        Some(spell)
    }

    /// Checks the buffer once the typing settles, so a fast typist is not
    /// racing a dictionary on every keystroke.
    fn recheck_soon(self: &Rc<Self>) {
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(
            std::time::Duration::from_millis(SETTLE_MS as u64),
            move || {
                if let Some(spell) = weak.upgrade()
                    && spell.generation.get() == generation
                {
                    spell.recheck();
                }
            },
        );
    }

    /// Marks every misspelling in the buffer and clears the rest.
    pub fn recheck(&self) {
        let buffer = self.view.buffer();
        buffer.remove_tag_by_name(TAG, &buffer.start_iter(), &buffer.end_iter());
        for (start, end, _) in self.misspellings() {
            let (from, to) = (buffer.iter_at_offset(start), buffer.iter_at_offset(end));
            buffer.apply_tag_by_name(TAG, &from, &to);
        }
    }

    /// Every misspelling in the buffer, as character offsets and the word.
    ///
    /// Character offsets, not bytes, because that is what a
    /// [`gtk::TextBuffer`] counts in, and an accented letter would otherwise
    /// shift every squiggle after it.
    fn misspellings(&self) -> Vec<(i32, i32, String)> {
        let buffer = self.view.buffer();
        let mut found = Vec::new();
        for (from, to) in prose_runs(&buffer) {
            let text = buffer.text(&from, &to, false).to_string();
            for (at, word) in words_in(&text) {
                if self.dictionaries.accepts(word) {
                    continue;
                }
                let start = from.offset() + text[..at.start].chars().count() as i32;
                found.push((start, start + word.chars().count() as i32, word.to_string()));
            }
        }
        found
    }

    /// The misspelled word at `offset`. A click just after the last letter
    /// counts, the way it does when you double-click a word.
    fn word_at(&self, offset: i32) -> Option<(i32, i32, String)> {
        self.misspellings()
            .into_iter()
            .find(|(start, end, _)| (*start..=*end).contains(&offset))
    }

    /// Puts corrections on the text view's own context menu when the
    /// pointer is over a misspelling, and takes them off when it is not.
    fn wire_menu(self: &Rc<Self>) {
        let actions = gio::SimpleActionGroup::new();
        let correct = gio::SimpleAction::new("correct", Some(glib::VariantTy::STRING));
        let weak = Rc::downgrade(self);
        correct.connect_activate(move |_, word| {
            if let (Some(spell), Some(word)) = (weak.upgrade(), word.and_then(|w| w.str())) {
                spell.replace_word(word);
            }
        });
        actions.add_action(&correct);
        for (name, learn) in [("learn", true), ("ignore", false)] {
            let action = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, _| {
                if let Some(spell) = weak.upgrade() {
                    spell.accept_word(learn);
                }
            });
            actions.add_action(&action);
        }
        self.view.insert_action_group("spell", Some(&actions));

        // Capture phase, so the menu is ready before GTK builds and shows it.
        let gesture = gtk::GestureClick::new();
        gesture.set_button(gtk::gdk::BUTTON_SECONDARY);
        gesture.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        gesture.connect_pressed(move |_, _, x, y| {
            if let Some(spell) = weak.upgrade() {
                spell.offer_corrections(x, y);
            }
        });
        self.view.add_controller(gesture);
    }

    /// Builds the extra menu for the word under the pointer.
    fn offer_corrections(&self, x: f64, y: f64) {
        let (bx, by) =
            self.view
                .window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
        let Some(iter) = self.view.iter_at_location(bx, by) else {
            return self.view.set_extra_menu(gio::MenuModel::NONE);
        };
        let Some((start, end, word)) = self.word_at(iter.offset()) else {
            return self.view.set_extra_menu(gio::MenuModel::NONE);
        };
        self.at.set((start, end));
        let menu = gio::Menu::new();
        let suggestions = self.dictionaries.suggest(&word);
        let corrections = gio::Menu::new();
        if suggestions.is_empty() {
            let item = gio::MenuItem::new(Some("No Suggestions"), None);
            // Nothing to activate: the row is there to answer the question.
            item.set_action_and_target_value(Some("spell.none"), None);
            corrections.append_item(&item);
        }
        for suggestion in suggestions {
            let item = gio::MenuItem::new(Some(&suggestion), None);
            item.set_action_and_target_value(Some("spell.correct"), Some(&suggestion.to_variant()));
            corrections.append_item(&item);
        }
        menu.append_section(None, &corrections);
        let keep = gio::Menu::new();
        keep.append(Some("Add to Dictionary"), Some("spell.learn"));
        keep.append(Some("Ignore"), Some("spell.ignore"));
        menu.append_section(None, &keep);
        self.view.set_extra_menu(Some(&menu));
    }

    /// Puts `word` in place of the one the menu was opened on.
    fn replace_word(&self, word: &str) {
        let (start, end) = self.at.get();
        let buffer = self.view.buffer();
        let (mut from, mut to) = (buffer.iter_at_offset(start), buffer.iter_at_offset(end));
        buffer.begin_user_action();
        buffer.delete(&mut from, &mut to);
        buffer.insert(&mut from, word);
        buffer.end_user_action();
    }

    /// Stops marking the word the menu was opened on. With `learn`, keeps it
    /// in Preferences; otherwise it comes back next time the app starts.
    fn accept_word(&self, learn: bool) {
        let (start, end) = self.at.get();
        let buffer = self.view.buffer();
        let word = buffer
            .text(
                &buffer.iter_at_offset(start),
                &buffer.iter_at_offset(end),
                false,
            )
            .to_string();
        if learn {
            self.dictionaries.remember(&word);
            (self.on_learn)(&word);
        } else {
            self.dictionaries.ignore(&word);
        }
        self.recheck();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checked(text: &str) -> Vec<&str> {
        words_in(text).into_iter().map(|w| w.1).collect()
    }

    #[test]
    fn a_quote_and_a_code_block_are_not_prose() {
        // These two kinds are the whole rule, so a new kind has to be
        // decided here rather than slipping through as prose.
        assert!(!checks_prose(BlockKind::Quote));
        assert!(!checks_prose(BlockKind::Code));
        for kind in [
            BlockKind::Paragraph,
            BlockKind::Heading(1),
            BlockKind::Bullet,
            BlockKind::Numbered,
        ] {
            assert!(checks_prose(kind), "{kind:?}");
        }
    }

    #[test]
    fn a_markdown_fence_opens_and_closes() {
        assert!(is_fence("```"));
        assert!(is_fence("   ~~~rust"));
        assert!(!is_fence("almost ```"));
    }

    #[test]
    fn words_come_out_of_a_line_of_prose() {
        assert_eq!(
            checked("Teh plan looks fine."),
            ["Teh", "plan", "looks", "fine"]
        );
    }

    #[test]
    fn addresses_and_identifiers_are_not_words() {
        let text = "Mail dana@exmaple.com about send_as and h2o today.";
        assert_eq!(checked(text), ["Mail", "about", "and", "today"]);
    }

    #[test]
    fn a_url_is_dropped_whole_rather_than_marked_in_pieces() {
        assert_eq!(checked("See https://exmaple.com/setup now"), ["See", "now"]);
    }

    #[test]
    fn a_word_keeps_its_apostrophe_but_not_the_quotes_around_it() {
        assert_eq!(checked("'It doesn't' matter"), ["It", "doesn't", "matter"]);
    }

    #[test]
    fn a_squiggle_is_a_tag_the_serializer_never_asks_about() {
        // `richbuffer::read` builds the outgoing body from the block kinds,
        // the four style tags and the link tags. The squiggle is none of
        // them, so there is no path from a mark to the message.
        assert!(!richbuffer::STYLES.contains(&TAG));
        for kind in [
            BlockKind::Paragraph,
            BlockKind::Quote,
            BlockKind::Code,
            BlockKind::Bullet,
        ] {
            assert_ne!(richbuffer::block_tag(kind), TAG);
        }
    }

    #[test]
    fn offsets_are_bytes_inside_the_line_and_the_word_is_the_text_there() {
        // The buffer counts characters, so the caller converts; what this
        // hands back has to address the real bytes of what it was given.
        let text = "Olá mundo";
        for (range, word) in words_in(text) {
            assert_eq!(&text[range], word);
        }
    }

    #[test]
    fn a_missing_dictionary_checks_nothing_and_says_nothing() {
        let dictionaries = Dictionaries::load(&["zz_ZZ".to_string()], &[]);
        assert!(dictionaries.is_empty());
        // Nothing is marked, and nothing is said about it either.
        assert!(!dictionaries.accepts("wibble"));
    }

    #[test]
    fn the_locale_picks_a_dictionary_and_settles_for_a_neighbour() {
        let installed = ["en_US".to_string(), "pt_PT".to_string()];
        assert_eq!(languages_to_load(&[], "en_US", &installed), ["en_US"]);
        // Brazilian is not installed, so European Portuguese stands in.
        assert_eq!(languages_to_load(&[], "pt_BR", &installed), ["pt_PT"]);
        assert!(languages_to_load(&[], "ja_JP", &installed).is_empty());
    }

    #[test]
    fn an_account_can_ask_for_two_languages_at_once() {
        let installed = ["en_GB".to_string(), "pt_PT".to_string()];
        let wanted = ["en_GB".to_string(), "pt_PT".to_string()];
        assert_eq!(
            languages_to_load(&wanted, "fr_FR", &installed),
            ["en_GB", "pt_PT"]
        );
    }

    #[test]
    fn added_and_ignored_words_stop_being_marked() {
        let dictionaries = Dictionaries::load(&[], &["Penguin".to_string()]);
        assert!(dictionaries.accepts("penguin"));
        assert!(!dictionaries.accepts("wibble"));
        dictionaries.ignore("wibble");
        assert!(dictionaries.accepts("Wibble"));
    }
}
