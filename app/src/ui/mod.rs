//! The GTK interface. Widgets are built in code; only the thread row is a
//! GObject subclass, because list rows need a widget type to recycle.

pub mod about;
pub mod assistant;
pub mod assistant_mcp_prefs;
pub mod assistant_prefs;
pub mod assistant_skills_prefs;
pub mod assistant_web_prefs;
pub mod autocomplete;
pub mod composer;
pub mod confirm;
pub mod contact_card;
pub mod contacts_prefs;
pub mod conversation;
pub mod find;
pub mod hide_my_email;
pub mod invitation;
pub mod list_feed;
pub mod moving;
pub mod pgp;
pub mod preferences;
pub mod rules;
pub mod search_suggest;
pub mod sidebar;
pub mod smart_editor;
pub mod templates;
pub mod thread_list;
pub mod thread_row;
pub mod translation;
pub mod vacation;
pub mod welcome;
pub mod when;
pub mod window;

use gtk::prelude::*;
use mailrs_domain::translate::gettext;
use mailrs_domain::{Folder, system_label};
pub use mailrs_sync::Mailbox;
pub use mailrs_sync::mailbox::unified_name;
use mailrs_sync::mailbox::{folder_icon, folder_name};

/// Gives `widget` the name a screen reader says for it.
///
/// A button carrying only an icon has no name of its own, and GTK never
/// reads a tooltip out, so every such control is named here. The name says
/// what the control does in as few words as carry it, and it is a
/// translated string like any other word a person reads.
pub fn name(widget: &impl IsA<gtk::Widget>, spoken: &str) {
    widget
        .as_ref()
        .update_property(&[gtk::accessible::Property::Label(spoken)]);
}

/// Names `widget` after a tooltip whose last words are its keyboard
/// shortcut, such as "Archive (E or Ctrl+Alt+A)".
///
/// The name is what stands before the bracket and the keys go in a
/// property of their own, since a screen reader says the two at
/// different moments. A tooltip that names no shortcut is used whole.
pub fn name_with_shortcut(widget: &impl IsA<gtk::Widget>, tip: &str) {
    let (said, keys) = split_shortcut(tip);
    widget.as_ref().update_property(&[
        gtk::accessible::Property::Label(said),
        gtk::accessible::Property::KeyShortcuts(keys),
    ]);
}

/// A tooltip split into what the control does and the keys that do it.
fn split_shortcut(tip: &str) -> (&str, &str) {
    match tip.strip_suffix(')').and_then(|rest| rest.rsplit_once('(')) {
        Some((said, keys)) => (said.trim_end(), keys),
        None => (tip, ""),
    }
}

/// Names `widget` and adds the line read after the name, for a control
/// whose name alone leaves out what pressing it would do.
pub fn describe(widget: &impl IsA<gtk::Widget>, spoken: &str, detail: &str) {
    widget.as_ref().update_property(&[
        gtk::accessible::Property::Label(spoken),
        gtk::accessible::Property::Description(detail),
    ]);
}

/// Points `widget` at the label standing beside it, so a field reads out
/// under that word rather than under nothing.
pub fn labelled_by(widget: &impl IsA<gtk::Widget>, label: &impl IsA<gtk::Widget>) {
    widget
        .as_ref()
        .update_relation(&[gtk::accessible::Relation::LabelledBy(&[label
            .as_ref()
            .upcast_ref()])]);
}

/// How the sidebar and window show a `Folder`.
pub trait FolderLook {
    fn name(self) -> String;
    fn icon(self) -> &'static str;
}

impl FolderLook for Folder {
    fn name(self) -> String {
        folder_name(self)
    }

    fn icon(self) -> &'static str {
        folder_icon(self)
    }
}

/// Label colours from Gmail's palette: background and text.
/// [`label_color_name`] names them in the reader's language.
pub const LABEL_COLORS: [(&str, &str); 9] = [
    ("#fb4c2f", "#ffffff"),
    ("#ffad47", "#ffffff"),
    ("#fad165", "#000000"),
    ("#16a766", "#ffffff"),
    ("#2da2bb", "#ffffff"),
    ("#4a86e8", "#ffffff"),
    ("#a479e2", "#ffffff"),
    ("#f691b3", "#ffffff"),
    ("#999999", "#ffffff"),
];

/// What the label colour menu calls the colour at `index`.
pub fn label_color_name(index: usize) -> String {
    match index {
        0 => gettext("Red"),
        1 => gettext("Orange"),
        2 => gettext("Yellow"),
        3 => gettext("Green"),
        4 => gettext("Teal"),
        5 => gettext("Blue"),
        6 => gettext("Purple"),
        7 => gettext("Pink"),
        _ => gettext("Gray"),
    }
}

pub const UNIFIED: [&str; 5] = [
    system_label::INBOX,
    system_label::STARRED,
    system_label::SENT,
    system_label::DRAFT,
    system_label::MUTE,
];

pub fn account_label_name(label: &str) -> String {
    match label {
        system_label::INBOX => gettext("Inbox"),
        system_label::STARRED => gettext("Flagged"),
        system_label::SENT => gettext("Sent"),
        system_label::DRAFT => gettext("Drafts"),
        system_label::MUTE => gettext("Muted"),
        _ => gettext("Mail"),
    }
}

pub fn mailbox_icon(label: &str) -> &'static str {
    match label {
        system_label::INBOX => "penguin-mail-inbox-symbolic",
        system_label::STARRED => "penguin-mail-flag-symbolic",
        system_label::SENT => "mail-send-symbolic",
        system_label::DRAFT => "document-edit-symbolic",
        system_label::MUTE => "audio-volume-muted-symbolic",
        _ => "penguin-mail-tag-symbolic",
    }
}

#[cfg(test)]
mod tests {
    use super::split_shortcut;

    #[test]
    fn a_tooltip_gives_up_the_keys_at_the_end_of_it() {
        assert_eq!(
            split_shortcut("Archive (E or Ctrl+Alt+A)"),
            ("Archive", "E or Ctrl+Alt+A")
        );
        assert_eq!(split_shortcut("Search (/)"), ("Search", "/"));
        assert_eq!(split_shortcut("More Actions"), ("More Actions", ""));
        assert_eq!(split_shortcut(""), ("", ""));
    }
}
