//! Spell check while writing.
//!
//! Penguin Mail checks the words itself rather than handing the text view to
//! a spell-check widget. Two reasons. The composer edits Markdown, so whole
//! stretches of it are not prose: a quoted reply, a fenced code block, the
//! target of a link. And a squiggle has to be a mark on top of the text, not
//! a change to it, because the text is what gets sent.
//!
//! So the squiggle is a [`gtk::TextTag`] with Pango's error underline. Tags
//! live beside the text, never in it: [`Composer::markdown`] reads the buffer
//! with `include_hidden_chars` off and gets the characters alone, so nothing
//! a checker marked can reach the MIME body.
//!
//! [`Composer::markdown`]: super::Composer

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gio, glib, pango};

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

/// The stretches of `markdown` that are prose, as byte ranges.
///
/// Quoted lines, fenced code, inline code, and the targets of links and
/// images are left out. A squiggle under someone else's sentence, or under
/// half a URL, is noise the writer cannot act on.
pub fn prose_ranges(markdown: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut fenced = false;
    let mut at = 0;
    for line in markdown.split_inclusive('\n') {
        let start = at;
        at += line.len();
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced || trimmed.starts_with('>') {
            continue;
        }
        let indent = line.len() - trimmed.len();
        prose_in_line(&line[indent..], start + indent, &mut ranges);
    }
    ranges
}

/// Adds the prose of one line, cutting out code spans and link targets.
fn prose_in_line(line: &str, offset: usize, ranges: &mut Vec<Range<usize>>) {
    let bytes = line.as_bytes();
    let mut at = 0;
    let mut prose = 0;
    let push = |from: usize, to: usize, ranges: &mut Vec<Range<usize>>| {
        if to > from {
            ranges.push(offset + from..offset + to);
        }
    };
    // Where a stretch that started at `at` ends, given its closing byte.
    let closes = |at: usize, close: char| {
        line[at + 1..]
            .find(close)
            .map_or(bytes.len(), |i| at + 1 + i + 1)
    };
    while at < bytes.len() {
        let end = match bytes[at] {
            // A code span runs to the next backtick, or to the end of the line.
            b'`' => closes(at, '`'),
            // `](target)`: the link text stays, the target goes.
            b'(' if at > 0 && bytes[at - 1] == b']' => closes(at, ')'),
            // `<https://…>`, and HTML the writer pasted in. A lone `<` with a
            // space after it is arithmetic, so leave that alone.
            b'<' if !bytes.get(at + 1).is_some_and(u8::is_ascii_whitespace) => {
                match line[at + 1..].find(['>', ' ']) {
                    Some(i) if line.as_bytes()[at + 1 + i] == b'>' => at + 1 + i + 1,
                    _ => at + 1,
                }
            }
            _ => at + 1,
        };
        if end > at + 1 {
            push(prose, at, ranges);
            prose = end;
        }
        at = end;
    }
    push(prose, bytes.len(), ranges);
}

/// Every word of `markdown` a dictionary should judge, as character offsets
/// into the buffer, in the order they appear.
///
/// Character offsets, not bytes, because that is what a [`gtk::TextBuffer`]
/// counts in. Words with a digit, an underscore, or an `@` in them are left
/// out: they are identifiers and addresses, and no dictionary knows them.
pub fn words_to_check(markdown: &str) -> Vec<(Range<usize>, &str)> {
    let mut words = Vec::new();
    // Byte offsets come out of `prose_ranges` in order, so one cursor turns
    // them into character offsets without rescanning the text each time.
    let mut byte = 0;
    let mut character = 0;
    let mut chars_at = |to: usize, text: &str| {
        character += text[byte..to].chars().count();
        byte = to;
        character
    };
    for range in prose_ranges(markdown) {
        let slice = &markdown[range.clone()];
        for (at, word) in split_words(slice) {
            let start = chars_at(range.start + at.start, markdown);
            let end = chars_at(range.start + at.end, markdown);
            words.push((start..end, word));
        }
    }
    words
}

/// Words in one stretch of prose, with their byte offsets inside it.
///
/// Whitespace splits the text into tokens first, and a token carrying a
/// digit, an underscore, an `@`, or a slash is dropped whole: it is an
/// address, a path, or an identifier, and the dictionary would mark every
/// part of it. Only then does a token break into words.
fn split_words(text: &str) -> Vec<(Range<usize>, &str)> {
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
            let inside = matches!(c, '\'' | '’') && from.is_some();
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
    let word = token[from..to].trim_end_matches(['\'', '’']);
    // One letter is "I" or "a"; two is an initial or a unit. Neither is
    // worth a squiggle.
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
        let text = buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .to_string();
        buffer.remove_tag_by_name(TAG, &buffer.start_iter(), &buffer.end_iter());
        for (range, word) in words_to_check(&text) {
            if self.dictionaries.accepts(word) {
                continue;
            }
            let start = buffer.iter_at_offset(range.start as i32);
            let end = buffer.iter_at_offset(range.end as i32);
            buffer.apply_tag_by_name(TAG, &start, &end);
        }
    }

    /// The misspelled word at `offset`, with its bounds. A click just after
    /// the last letter counts, the way it does when you double-click a word.
    fn word_at(&self, offset: i32) -> Option<(i32, i32, String)> {
        let buffer = self.view.buffer();
        let text = buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .to_string();
        words_to_check(&text)
            .into_iter()
            .find(|(range, word)| {
                (range.start..=range.end).contains(&(offset as usize))
                    && !self.dictionaries.accepts(word)
            })
            .map(|(range, word)| (range.start as i32, range.end as i32, word.to_string()))
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

    fn checked(markdown: &str) -> Vec<&str> {
        words_to_check(markdown).into_iter().map(|w| w.1).collect()
    }

    #[test]
    fn prose_is_checked_and_quoted_text_is_not() {
        let text = "Teh plan looks fine.\n> teh plan looks fine\nSee yuo then.";
        assert_eq!(
            checked(text),
            ["Teh", "plan", "looks", "fine", "See", "yuo", "then"]
        );
    }

    #[test]
    fn fenced_code_is_left_alone() {
        let text = "before\n```\nlet mispeled = 1;\n```\nafter";
        assert_eq!(checked(text), ["before", "after"]);
    }

    #[test]
    fn inline_code_and_link_targets_are_left_alone() {
        let text = "Run `cargo bild` and read [the guide](https://exmaple.com/setup).";
        assert_eq!(checked(text), ["Run", "and", "read", "the", "guide"]);
    }

    #[test]
    fn addresses_and_identifiers_are_not_words() {
        let text = "Mail dana@exmaple.com about send_as and h2o today.";
        assert_eq!(checked(text), ["Mail", "about", "and", "today"]);
    }

    #[test]
    fn a_word_keeps_its_apostrophe_but_not_the_quotes_around_it() {
        assert_eq!(checked("'It doesn't' matter"), ["It", "doesn't", "matter"]);
    }

    #[test]
    fn offsets_are_characters_so_accents_do_not_shift_the_squiggle() {
        // "Olá" is three characters and four bytes; the word after it must
        // still line up with what the buffer counts.
        let words = words_to_check("Olá mundo");
        assert_eq!(words, [(0..3, "Olá"), (4..9, "mundo")]);
    }

    #[test]
    fn a_squiggle_points_at_the_text_and_never_joins_it() {
        // The checker hands back ranges, never replacement text, so the
        // markdown that reaches the MIME builder is what the person typed
        // and the tag name appears nowhere in the HTML.
        let markdown = "Teh plan looks fine. Call **yuo** back.";
        for (range, word) in words_to_check(markdown) {
            let at: String = markdown
                .chars()
                .skip(range.start)
                .take(range.end - range.start)
                .collect();
            assert_eq!(at, word);
        }
        let html = crate::compose::markdown_to_html(markdown);
        assert!(!html.contains(TAG) && !html.contains("underline"));
        assert!(html.contains("Teh plan looks fine."));
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
