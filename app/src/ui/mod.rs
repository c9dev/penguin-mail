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

use mailrs_domain::{Folder, system_label};
pub use mailrs_sync::Mailbox;
pub use mailrs_sync::mailbox::unified_name;
use mailrs_sync::mailbox::{folder_icon, folder_name};

/// How the sidebar and window show a `Folder`.
pub trait FolderLook {
    fn name(self) -> &'static str;
    fn icon(self) -> &'static str;
}

impl FolderLook for Folder {
    fn name(self) -> &'static str {
        folder_name(self)
    }

    fn icon(self) -> &'static str {
        folder_icon(self)
    }
}

/// Label colours from Gmail's palette: name, background, and text.
pub const LABEL_COLORS: [(&str, &str, &str); 9] = [
    ("Red", "#fb4c2f", "#ffffff"),
    ("Orange", "#ffad47", "#ffffff"),
    ("Yellow", "#fad165", "#000000"),
    ("Green", "#16a766", "#ffffff"),
    ("Teal", "#2da2bb", "#ffffff"),
    ("Blue", "#4a86e8", "#ffffff"),
    ("Purple", "#a479e2", "#ffffff"),
    ("Pink", "#f691b3", "#ffffff"),
    ("Gray", "#999999", "#ffffff"),
];

pub const UNIFIED: [&str; 5] = [
    system_label::INBOX,
    system_label::STARRED,
    system_label::SENT,
    system_label::DRAFT,
    system_label::MUTE,
];

pub fn account_label_name(label: &str) -> &'static str {
    match label {
        system_label::INBOX => "Inbox",
        system_label::STARRED => "Flagged",
        system_label::SENT => "Sent",
        system_label::DRAFT => "Drafts",
        system_label::MUTE => "Muted",
        _ => "Mail",
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
