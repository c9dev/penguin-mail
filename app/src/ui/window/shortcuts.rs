//! Every keyboard shortcut in one table: the keys, the action each one
//! activates, the windows it works in, and the line the Keyboard Shortcuts
//! dialog shows for it. The main window's controllers, a separate
//! conversation window's, and the dialog all read [`SHORTCUTS`], so a key
//! cannot work without the dialog listing it.
//!
//! The actions a conversation's keys and menus reach live here too, in
//! [`VIEW_ACTIONS`]. The main window installs them for its own
//! conversation and a separate window for the conversation it shows, so
//! the two windows run the same code for the same name.

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use mailrs_domain::Category;
use mailrs_domain::translate::gettext;

use super::MainWindow;
use crate::compose::ReplyKind;
use crate::ui::conversation::{Action, ConversationView};

/// Where a key works.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Place {
    /// The main window, whatever has the focus in it.
    Main,
    /// A conversation in a window of its own.
    Conversation,
    /// Both of the above.
    Both,
    /// The composer, which installs its keys itself. The table lists them
    /// only so the dialog can show them.
    Composer,
}

impl Place {
    /// Whether a key placed here works in `window`.
    fn reaches(self, window: Place) -> bool {
        self == window
            || (self == Place::Both && matches!(window, Place::Main | Place::Conversation))
    }
}

/// What a key hands the action it activates, for the actions that take
/// one, such as the mailbox number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Argument {
    Number(i32),
    Text(&'static str),
}

impl Argument {
    fn variant(self) -> glib::Variant {
        match self {
            Argument::Number(number) => number.to_variant(),
            Argument::Text(text) => text.to_variant(),
        }
    }
}

/// One key and what it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Key {
    /// GTK's accelerator syntax, which the dialog turns into keycaps.
    pub trigger: &'static str,
    /// The detailed action name, such as `win.archive`. Empty for a
    /// composer key, which runs a callback of the composer's own.
    pub action: &'static str,
    pub argument: Option<Argument>,
    pub place: Place,
    /// A single key, pressed without Control, that gives way while you
    /// type in a text field. Only the main window has these, since only it
    /// has a list to move through.
    pub letter: bool,
    /// Whether the dialog lists the key. An alias for a key the same line
    /// already shows stays out of it.
    pub shown: bool,
}

const fn key(trigger: &'static str, action: &'static str, place: Place) -> Key {
    Key {
        trigger,
        action,
        argument: None,
        place,
        letter: false,
        shown: true,
    }
}

/// A chord in the main window.
const fn main(trigger: &'static str, action: &'static str) -> Key {
    key(trigger, action, Place::Main)
}

/// A chord in the main window and in a separate conversation window.
const fn both(trigger: &'static str, action: &'static str) -> Key {
    key(trigger, action, Place::Both)
}

/// A chord in a separate conversation window only.
const fn conversation(trigger: &'static str, action: &'static str) -> Key {
    key(trigger, action, Place::Conversation)
}

/// A single key in the main window, which gives way to typing.
const fn letter(trigger: &'static str, action: &'static str) -> Key {
    Key {
        letter: true,
        ..key(trigger, action, Place::Main)
    }
}

/// A key the composer installs itself.
const fn composer(trigger: &'static str) -> Key {
    key(trigger, "", Place::Composer)
}

impl Key {
    const fn hidden(self) -> Key {
        Key {
            shown: false,
            ..self
        }
    }

    const fn with(self, argument: Argument) -> Key {
        Key {
            argument: Some(argument),
            ..self
        }
    }

    /// Whether a press of `pressed` with `modifiers` is this letter key.
    ///
    /// A letter names the character it types, so `numbersign` is `#` on
    /// any layout and `<Shift>m` is `M`, and Shift alone does not turn `j`
    /// into `J`'s shortcut. Keys that type no character, such as Delete,
    /// match on the key and ignore Shift. Control names the one modifier a
    /// letter may carry, and then no other: Ctrl+Shift+A is not Ctrl+A.
    pub(super) fn pressed(&self, pressed: gdk::Key, modifiers: gdk::ModifierType) -> bool {
        let (control, name) = strip(self.trigger, "<Control>");
        let (shift, name) = strip(name, "<Shift>");
        let Some(wanted) = gdk::Key::from_name(name) else {
            return false;
        };
        if modifiers.intersects(gdk::ModifierType::ALT_MASK | gdk::ModifierType::SUPER_MASK) {
            return false;
        }
        if control {
            return modifiers.contains(gdk::ModifierType::CONTROL_MASK)
                && !modifiers.contains(gdk::ModifierType::SHIFT_MASK)
                && pressed == wanted;
        }
        if modifiers.contains(gdk::ModifierType::CONTROL_MASK) {
            return false;
        }
        match wanted.to_unicode().filter(|c| !c.is_control()) {
            Some(typed) if shift => pressed.to_unicode() == Some(typed.to_ascii_uppercase()),
            Some(typed) => pressed.to_unicode() == Some(typed),
            None => pressed == wanted,
        }
    }
}

/// `text` without `prefix`, and whether it had one.
fn strip<'a>(text: &'a str, prefix: &str) -> (bool, &'a str) {
    match text.strip_prefix(prefix) {
        Some(rest) => (true, rest),
        None => (false, text),
    }
}

/// A heading in the dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Section {
    Reading,
    Organizing,
    Writing,
    General,
}

impl Section {
    const ALL: [Section; 4] = [
        Section::Reading,
        Section::Organizing,
        Section::Writing,
        Section::General,
    ];

    fn title(self) -> String {
        match self {
            Section::Reading => gettext("Reading"),
            Section::Organizing => gettext("Organizing"),
            Section::Writing => gettext("Writing"),
            Section::General => gettext("General"),
        }
    }
}

/// One line of the dialog and the keys behind it.
pub(super) struct Shortcut {
    pub section: Section,
    pub description: fn() -> String,
    pub keys: &'static [Key],
}

impl Shortcut {
    /// The keys the dialog shows, in GTK's accelerator syntax. A run of
    /// one action told apart by its argument, such as the mailbox numbers,
    /// reads as a range, and a key that works in both kinds of window
    /// shows once.
    pub(super) fn accelerators(&self) -> String {
        let shown: Vec<&Key> = self.keys.iter().filter(|k| k.shown).collect();
        if let [first, .., last] = shown.as_slice()
            && shown
                .iter()
                .all(|k| k.argument.is_some() && k.action == first.action)
        {
            return format!("{}...{}", first.trigger, last.trigger);
        }
        let mut triggers: Vec<&str> = Vec::new();
        for key in shown {
            if !triggers.contains(&key.trigger) {
                triggers.push(key.trigger);
            }
        }
        triggers.join(" ")
    }
}

const fn mailbox(number: i32, trigger: &'static str) -> Key {
    main(trigger, "win.go-mailbox").with(Argument::Number(number))
}

const fn flag(color: &'static str, trigger: &'static str) -> Key {
    main(trigger, "win.flag-color").with(Argument::Text(color))
}

/// Apple Mail's shortcuts with Control in place of Command, Gmail's single
/// keys, and the dialog's lines, in the order the dialog shows them.
pub(super) static SHORTCUTS: &[Shortcut] = &[
    Shortcut {
        section: Section::Reading,
        description: || gettext("Next or previous conversation"),
        keys: &[
            letter("j", "win.next-conversation"),
            letter("k", "win.previous-conversation"),
        ],
    },
    Shortcut {
        section: Section::Reading,
        description: || gettext("Open mailbox 1 to 9"),
        keys: &[
            mailbox(1, "<Control>1"),
            mailbox(2, "<Control>2"),
            mailbox(3, "<Control>3"),
            mailbox(4, "<Control>4"),
            mailbox(5, "<Control>5"),
            mailbox(6, "<Control>6"),
            mailbox(7, "<Control>7"),
            mailbox(8, "<Control>8"),
            mailbox(9, "<Control>9"),
        ],
    },
    Shortcut {
        section: Section::Reading,
        description: || gettext("Search"),
        keys: &[
            letter("slash", "win.search"),
            main("<Control><Alt>f", "win.search"),
        ],
    },
    Shortcut {
        section: Section::Reading,
        description: || gettext("Find in the conversation"),
        keys: &[both("<Control>f", "win.find")],
    },
    Shortcut {
        section: Section::Reading,
        description: || gettext("Get new mail"),
        keys: &[
            main("<Control><Shift>n", "win.check"),
            main("F5", "win.check"),
        ],
    },
    Shortcut {
        section: Section::Reading,
        description: || gettext("Select all"),
        keys: &[letter("<Control>a", "win.select-all")],
    },
    Shortcut {
        section: Section::Reading,
        description: || gettext("Clear the selection"),
        keys: &[letter("Escape", "win.clear-selection")],
    },
    Shortcut {
        section: Section::Reading,
        description: || gettext("Bigger or smaller text"),
        keys: &[
            main("<Control>plus", "win.zoom-in"),
            main("<Control>minus", "win.zoom-out"),
            main("<Control>equal", "win.zoom-in").hidden(),
        ],
    },
    Shortcut {
        section: Section::Reading,
        description: || gettext("Open in a new window"),
        keys: &[main("<Control>o", "win.open-window")],
    },
    Shortcut {
        section: Section::Reading,
        description: || gettext("Print"),
        keys: &[both("<Control>p", "win.print")],
    },
    Shortcut {
        section: Section::Reading,
        description: || gettext("View source"),
        keys: &[both("<Control><Alt>u", "win.view-source")],
    },
    Shortcut {
        section: Section::Reading,
        description: || gettext("Normal text size"),
        keys: &[main("<Control>0", "win.zoom-reset")],
    },
    Shortcut {
        section: Section::Organizing,
        description: || gettext("Archive"),
        keys: &[
            both("<Control><Alt>a", "win.archive"),
            letter("e", "win.archive"),
        ],
    },
    Shortcut {
        section: Section::Organizing,
        description: || gettext("Move to trash"),
        keys: &[
            letter("Delete", "win.trash"),
            letter("numbersign", "win.trash"),
            letter("BackSpace", "win.trash").hidden(),
            letter("KP_Delete", "win.trash").hidden(),
            conversation("Delete", "win.trash"),
        ],
    },
    Shortcut {
        section: Section::Organizing,
        description: || gettext("Junk"),
        keys: &[both("<Control><Shift>j", "win.junk")],
    },
    Shortcut {
        section: Section::Organizing,
        description: || gettext("Flag or unflag"),
        keys: &[
            both("<Control><Shift>l", "win.toggle-star"),
            letter("s", "win.toggle-star"),
        ],
    },
    Shortcut {
        section: Section::Organizing,
        description: || gettext("Flag colors"),
        keys: &[
            flag("red", "<Control><Alt>1"),
            flag("orange", "<Control><Alt>2"),
            flag("yellow", "<Control><Alt>3"),
            flag("green", "<Control><Alt>4"),
            flag("blue", "<Control><Alt>5"),
            flag("purple", "<Control><Alt>6"),
            flag("gray", "<Control><Alt>7"),
        ],
    },
    Shortcut {
        section: Section::Organizing,
        description: || gettext("Mark read or unread"),
        keys: &[
            both("<Control><Shift>u", "win.toggle-read"),
            letter("u", "win.toggle-read"),
        ],
    },
    Shortcut {
        section: Section::Organizing,
        description: || gettext("Mute or unmute"),
        keys: &[letter("<Shift>m", "win.mute")],
    },
    Shortcut {
        section: Section::Organizing,
        description: || gettext("Labels"),
        keys: &[
            main("<Control><Alt>m", "win.label"),
            letter("l", "win.label"),
        ],
    },
    Shortcut {
        section: Section::Organizing,
        description: || gettext("Undo"),
        keys: &[letter("<Control>z", "win.undo")],
    },
    Shortcut {
        section: Section::Writing,
        description: || gettext("New message"),
        keys: &[
            main("<Control>n", "win.compose"),
            letter("c", "win.compose"),
        ],
    },
    Shortcut {
        section: Section::Writing,
        description: || gettext("Reply"),
        keys: &[both("<Control>r", "win.reply"), letter("r", "win.reply")],
    },
    Shortcut {
        section: Section::Writing,
        description: || gettext("Reply all"),
        keys: &[
            both("<Control><Shift>r", "win.reply-all"),
            letter("a", "win.reply-all"),
        ],
    },
    Shortcut {
        section: Section::Writing,
        description: || gettext("Forward"),
        keys: &[
            both("<Control><Shift>f", "win.forward"),
            letter("f", "win.forward"),
        ],
    },
    Shortcut {
        section: Section::Writing,
        description: || gettext("Send"),
        keys: &[composer("<Control><Shift>d"), composer("<Control>Return")],
    },
    Shortcut {
        section: Section::Writing,
        description: || gettext("Attach files"),
        keys: &[composer("<Control><Shift>a")],
    },
    Shortcut {
        section: Section::Writing,
        description: || gettext("Bold, italic, link"),
        keys: &[
            composer("<Control>b"),
            composer("<Control>i"),
            composer("<Control>k"),
        ],
    },
    Shortcut {
        section: Section::Writing,
        description: || gettext("Save draft"),
        keys: &[composer("<Control>s")],
    },
    Shortcut {
        section: Section::General,
        description: || gettext("Preferences"),
        keys: &[main("<Control>comma", "win.preferences")],
    },
    Shortcut {
        section: Section::General,
        description: || gettext("Keyboard shortcuts"),
        keys: &[main("<Control>question", "win.shortcuts")],
    },
    Shortcut {
        section: Section::General,
        description: || gettext("Show or hide the assistant"),
        keys: &[main("<Control>j", "win.assistant")],
    },
    Shortcut {
        section: Section::General,
        description: || gettext("Close window"),
        keys: &[
            both("<Control>w", "window.close"),
            conversation("Escape", "window.close").hidden(),
        ],
    },
    Shortcut {
        section: Section::General,
        description: || gettext("Quit"),
        keys: &[main("<Control>q", "win.quit")],
    },
];

/// Every key in the table, with the line it belongs to.
fn keys() -> impl Iterator<Item = &'static Key> {
    SHORTCUTS.iter().flat_map(|shortcut| shortcut.keys)
}

/// The letter key a press in the main window stands for, if any.
pub(super) fn letter_for(pressed: gdk::Key, modifiers: gdk::ModifierType) -> Option<&'static Key> {
    keys().find(|key| key.letter && key.pressed(pressed, modifiers))
}

/// A controller holding every chord that works in `window`.
fn chords(window: Place) -> gtk::ShortcutController {
    let controller = gtk::ShortcutController::new();
    for key in keys().filter(|k| !k.letter && k.place.reaches(window)) {
        let shortcut = gtk::Shortcut::new(
            gtk::ShortcutTrigger::parse_string(key.trigger),
            Some(gtk::NamedAction::new(key.action)),
        );
        if let Some(argument) = key.argument {
            shortcut.set_arguments(Some(&argument.variant()));
        }
        controller.add_shortcut(shortcut);
    }
    controller
}

/// The chords of the main window. They work wherever its focus is.
pub(super) fn main_chords() -> gtk::ShortcutController {
    let controller = chords(Place::Main);
    controller.set_scope(gtk::ShortcutScope::Global);
    controller
}

/// The chords of a conversation in a window of its own.
pub(super) fn conversation_chords() -> gtk::ShortcutController {
    chords(Place::Conversation)
}

/// The Keyboard Shortcuts dialog, one section per heading.
pub(super) fn dialog() -> adw::ShortcutsDialog {
    let dialog = adw::ShortcutsDialog::new();
    for section in Section::ALL {
        let group = adw::ShortcutsSection::new(Some(&section.title()));
        for shortcut in SHORTCUTS.iter().filter(|s| s.section == section) {
            group.add(adw::ShortcutsItem::new(
                &(shortcut.description)(),
                &shortcut.accelerators(),
            ));
        }
        dialog.add(group);
    }
    dialog
}

/// An action that runs on the main window alone.
pub(super) type WindowRun = fn(&Rc<MainWindow>);

/// An action that runs on one conversation view, in whichever window it is.
pub(super) type ViewRun = fn(&Rc<MainWindow>, &Rc<ConversationView>);

/// The main window's own actions without an argument, by name.
pub(super) static MAIN_ACTIONS: &[(&str, WindowRun)] = &[
    ("compose", |win| win.compose_new()),
    ("search", |win| win.list.open_search()),
    ("hide-my-email", |win| win.show_hide_my_email(None)),
    ("check", |win| {
        win.core.poke_all();
        win.reload_folder();
        win.toast(&gettext("Checking for mail"));
    }),
    ("add-account", |win| win.authorize(None)),
    ("shortcuts", |win| win.show_shortcuts()),
    ("mute", |win| win.toggle_mute()),
    ("label", |win| win.conversation.label_button.popup()),
    ("undo", |win| win.undo()),
    ("assistant", |win| win.toggle_assistant()),
    ("remind-custom", |win| win.remind_custom()),
    ("toggle-vip", |win| win.toggle_vip()),
    ("open-window", |win| win.open_current_in_window()),
    ("select-all", |win| win.list.select_all()),
    ("zoom-in", |win| win.change_text_size(1)),
    ("zoom-out", |win| win.change_text_size(-1)),
    ("zoom-reset", |win| win.change_text_size(0)),
    ("next-conversation", |win| win.list.step(1)),
    ("previous-conversation", |win| win.list.step(-1)),
    ("clear-selection", |win| win.clear_selection()),
    ("about", |win| win.show_about()),
    ("preferences", |win| win.show_preferences()),
    ("quit", |win| {
        if let Some(app) = win.app.upgrade() {
            app.quit();
        }
    }),
];

/// What a conversation's keys and menus reach, in the main window and in a
/// window of its own alike.
pub(super) static VIEW_ACTIONS: &[(&str, ViewRun)] = &[
    ("reply", |win, view| win.reply(view, ReplyKind::Reply)),
    ("reply-all", |win, view| {
        win.reply(view, ReplyKind::ReplyAll)
    }),
    ("forward", |win, view| win.reply(view, ReplyKind::Forward)),
    ("archive", |win, view| win.act(view, Action::Archive)),
    ("trash", |win, view| win.act(view, Action::Trash)),
    ("junk", |win, view| win.act(view, Action::Junk)),
    ("toggle-star", |win, view| win.act(view, Action::ToggleStar)),
    ("toggle-read", |win, view| win.act(view, Action::ToggleRead)),
    ("find", |win, view| win.find(view)),
    ("print", |_, view| view.print()),
    ("view-source", |win, view| win.view_source(view)),
    ("export", |win, view| win.export(view)),
    ("unsubscribe", |win, view| win.unsubscribe(Rc::clone(view))),
    ("block-sender", |win, view| {
        win.block_sender(Rc::clone(view))
    }),
    ("always-load-images", |win, view| {
        win.always_load_images(view)
    }),
];

impl MainWindow {
    /// Adds [`VIEW_ACTIONS`] to `group`, each running on `view`, and the
    /// menu's Move Sender To entries, which name a category.
    pub(super) fn install_view_actions(
        self: &Rc<Self>,
        group: &gio::SimpleActionGroup,
        view: &Rc<ConversationView>,
    ) {
        for (name, run) in VIEW_ACTIONS {
            let action = gio::SimpleAction::new(name, None);
            let (win, target) = (Rc::downgrade(self), Rc::downgrade(view));
            action.connect_activate(move |_, _| {
                if let (Some(win), Some(view)) = (win.upgrade(), target.upgrade()) {
                    run(&win, &view);
                }
            });
            group.add_action(&action);
        }
        let categorize = gio::SimpleAction::new("categorize-sender", Some(glib::VariantTy::STRING));
        let (win, target) = (Rc::downgrade(self), Rc::downgrade(view));
        categorize.connect_activate(move |_, parameter| {
            let category = parameter
                .and_then(|p| p.get::<String>())
                .and_then(|k| Category::from_key(&k));
            if let (Some(win), Some(view), Some(category)) =
                (win.upgrade(), target.upgrade(), category)
            {
                win.categorize_sender_from(view, category);
            }
        });
        group.add_action(&categorize);
    }

    /// Adds [`MAIN_ACTIONS`] to the main window.
    pub(super) fn install_main_actions(self: &Rc<Self>) {
        for (name, run) in MAIN_ACTIONS {
            let action = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, _| {
                if let Some(win) = weak.upgrade() {
                    run(&win);
                }
            });
            self.actions.add_action(&action);
        }
        let view = Rc::clone(&self.conversation);
        self.install_view_actions(&self.actions, &view);
    }

    /// Runs the letter key a press stands for, unless you are typing, the
    /// window shows something other than mail, or the key has nothing to
    /// do. Says whether the press was used.
    pub(super) fn letter_pressed(
        self: &Rc<Self>,
        pressed: gdk::Key,
        modifiers: gdk::ModifierType,
    ) -> glib::Propagation {
        if self.typing() || self.stack.visible_child_name().as_deref() != Some("mail") {
            return glib::Propagation::Proceed;
        }
        let Some(key) = letter_for(pressed, modifiers) else {
            return glib::Propagation::Proceed;
        };
        // In the message itself, Control keys select and copy its text.
        if modifiers.contains(gdk::ModifierType::CONTROL_MASK) && self.reading_text() {
            return glib::Propagation::Proceed;
        }
        // Escape belongs to whatever is under it unless there is a
        // selection or a search to close.
        if key.action == "win.clear-selection" && !self.has_selection_to_clear() {
            return glib::Propagation::Proceed;
        }
        let argument = key.argument.map(Argument::variant);
        let _ = WidgetExt::activate_action(&self.window, key.action, argument.as_ref());
        glib::Propagation::Stop
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLACES: [Place; 3] = [Place::Main, Place::Conversation, Place::Composer];

    /// The main window's actions that take an argument, which
    /// `install_actions` adds by hand.
    const MAIN_ACTIONS_WITH_ARGUMENT: [&str; 2] = ["flag-color", "go-mailbox"];

    #[test]
    fn every_line_says_what_its_keys_do() {
        for shortcut in SHORTCUTS {
            assert!(!(shortcut.description)().trim().is_empty());
            assert!(!shortcut.keys.is_empty(), "{}", (shortcut.description)());
            assert!(
                shortcut.keys.iter().any(|k| k.shown),
                "{} shows no key",
                (shortcut.description)()
            );
        }
    }

    #[test]
    fn no_two_actions_share_a_key_in_one_window() {
        for place in PLACES {
            let mut seen: Vec<(&str, &str, Option<Argument>)> = Vec::new();
            for key in keys().filter(|k| k.place.reaches(place)) {
                if let Some((_, action, argument)) =
                    seen.iter().find(|(trigger, ..)| *trigger == key.trigger)
                {
                    assert_eq!(
                        (*action, *argument),
                        (key.action, key.argument),
                        "{} does two things in {place:?}",
                        key.trigger
                    );
                }
                seen.push((key.trigger, key.action, key.argument));
            }
        }
    }

    #[test]
    fn every_key_names_an_action_its_window_has() {
        let view: Vec<String> = VIEW_ACTIONS
            .iter()
            .map(|(name, _)| format!("win.{name}"))
            .collect();
        let main: Vec<String> = MAIN_ACTIONS
            .iter()
            .map(|(name, _)| *name)
            .chain(MAIN_ACTIONS_WITH_ARGUMENT)
            .map(|name| format!("win.{name}"))
            .chain(view.iter().cloned())
            .collect();
        for key in keys() {
            let known = |names: &[String]| {
                key.action == "window.close" || names.iter().any(|n| n == key.action)
            };
            if key.place.reaches(Place::Main) {
                assert!(known(&main), "no main window action for {}", key.trigger);
            }
            if key.place.reaches(Place::Conversation) {
                assert!(known(&view), "no conversation action for {}", key.trigger);
            }
            if key.place == Place::Composer {
                assert!(key.action.is_empty() && !key.letter);
            }
        }
    }

    #[test]
    fn only_the_main_window_has_letters() {
        assert!(keys().filter(|k| k.letter).all(|k| k.place == Place::Main));
    }

    #[test]
    fn the_flag_keys_follow_the_colours_in_order() {
        let flags: Vec<Option<Argument>> = keys()
            .filter(|k| k.action == "win.flag-color")
            .map(|k| k.argument)
            .collect();
        let colours: Vec<Option<Argument>> = mailrs_domain::FlagColor::ALL
            .iter()
            .map(|c| Some(Argument::Text(c.as_str())))
            .collect();
        assert_eq!(flags, colours);
    }

    #[test]
    fn a_line_lists_its_keys_once_and_a_run_of_numbers_as_a_range() {
        let line = |description: &str| {
            SHORTCUTS
                .iter()
                .find(|s| (s.description)() == description)
                .map(Shortcut::accelerators)
                .unwrap()
        };
        assert_eq!(line("Open mailbox 1 to 9"), "<Control>1...<Control>9");
        assert_eq!(line("Move to trash"), "Delete numbersign");
        assert_eq!(
            line("Bigger or smaller text"),
            "<Control>plus <Control>minus"
        );
        assert_eq!(line("Search"), "slash <Control><Alt>f");
    }

    #[test]
    fn a_letter_is_the_character_it_types() {
        let none = gdk::ModifierType::empty();
        let shift = gdk::ModifierType::SHIFT_MASK;
        let control = gdk::ModifierType::CONTROL_MASK;
        let action = |key, modifiers| letter_for(key, modifiers).map(|k| k.action);
        assert_eq!(action(gdk::Key::e, none), Some("win.archive"));
        assert_eq!(action(gdk::Key::J, shift), None);
        assert_eq!(action(gdk::Key::M, shift), Some("win.mute"));
        assert_eq!(action(gdk::Key::numbersign, shift), Some("win.trash"));
        assert_eq!(action(gdk::Key::Delete, shift), Some("win.trash"));
        assert_eq!(action(gdk::Key::KP_Delete, none), Some("win.trash"));
        assert_eq!(action(gdk::Key::a, control), Some("win.select-all"));
        assert_eq!(action(gdk::Key::a, control | shift), None);
        assert_eq!(action(gdk::Key::e, control), None);
        assert_eq!(action(gdk::Key::e, gdk::ModifierType::ALT_MASK), None);
    }
}
