//! The GTK interface. Widgets are built in code; only the thread row is a
//! GObject subclass, because list rows need a widget type to recycle.

pub mod assistant;
pub mod assistant_prefs;
pub mod autocomplete;
pub mod composer;
pub mod contact_card;
pub mod conversation;
pub mod hide_my_email;
pub mod invitation;
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
pub mod vacation;
pub mod welcome;
pub mod when;
pub mod window;

use mailrs_domain::translate::gettext;
use mailrs_domain::{Folder, system_label};
pub use mailrs_sync::Mailbox;
pub use mailrs_sync::mailbox::unified_name;
use mailrs_sync::mailbox::{folder_icon, folder_name};

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
