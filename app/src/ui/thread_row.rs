use std::cell::{OnceCell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use chrono::Local;
use gtk::gdk;
use gtk::subclass::prelude::*;
use gtk::{glib, pango};
use mailrs_domain::{FlagColor, ThreadSummary};

use crate::format::{PALETTE, account_color_index, relative_date};
use mailrs_domain::translate::{fill, gettext};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct ThreadRow {
        pub avatar: OnceCell<adw::Avatar>,
        pub account: OnceCell<gtk::Box>,
        pub vip: OnceCell<gtk::Image>,
        /// The sender's name, and after it the address (ruling R4), so a
        /// look-alike display name does not hide the real address.
        pub from: OnceCell<gtk::Label>,
        pub clip: OnceCell<gtk::Image>,
        pub mute: OnceCell<gtk::Image>,
        pub star: OnceCell<gtk::Image>,
        pub date: OnceCell<gtk::Label>,
        pub subject: OnceCell<gtk::Label>,
        pub count: OnceCell<gtk::Label>,
        pub snippet: OnceCell<gtk::Label>,
        /// The calendar mark beside the subject, on while an open message
        /// of the thread carried a live invitation.
        pub invite: OnceCell<gtk::Image>,
        /// Opens the row's menu: at a point for a click, over the whole
        /// row for a key.
        pub menu: RefCell<Option<super::OpenMenu>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ThreadRow {
        const NAME: &'static str = "MailrsThreadRow";
        type Type = super::ThreadRow;
        type ParentType = gtk::Box;
    }

    impl ObjectImpl for ThreadRow {
        fn constructed(&self) {
            self.parent_constructed();
            let row = self.obj();
            row.set_orientation(gtk::Orientation::Horizontal);
            // The dot, the avatar and the text sit at exact offsets from
            // the mockup (22, 52, 80 px), each carried on its own CSS
            // margin, so the row sets no spacing of its own.
            row.set_spacing(0);
            row.add_css_class("thread-row");
            // A box says nothing out loud whatever name it is given, so
            // the row takes the role its place in the list calls for.
            row.set_accessible_role(gtk::AccessibleRole::ListItem);

            let dot = gtk::Box::builder()
                .valign(gtk::Align::Start)
                .css_classes(["unread-dot"])
                .build();
            let avatar = adw::Avatar::builder()
                .size(34)
                .show_initials(true)
                .valign(gtk::Align::Start)
                .visible(false)
                .css_classes(["thread-avatar"])
                .build();
            let content = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(2)
                .hexpand(true)
                .css_classes(["thread-text"])
                .build();

            let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            let account = gtk::Box::builder()
                .valign(gtk::Align::Center)
                .css_classes(["account-dot"])
                .visible(false)
                .build();
            let vip = marker("starred-symbolic");
            vip.add_css_class("vip");
            vip.set_tooltip_text(Some(&gettext("VIP")));
            // The name and the address share one label that takes the row's
            // spare width; see `sender_markup`.
            let from = text_label("from");
            from.set_hexpand(true);
            let clip = marker("mail-attachment-symbolic");
            let mute = marker("audio-volume-muted-symbolic");
            mute.set_tooltip_text(Some(&gettext("Muted")));
            let star = marker("penguin-mail-flag-symbolic");
            star.add_css_class("starred");
            let date = text_label("date");
            date.set_ellipsize(pango::EllipsizeMode::None);
            for widget in [
                account.upcast_ref::<gtk::Widget>(),
                vip.upcast_ref(),
                from.upcast_ref(),
                clip.upcast_ref(),
                mute.upcast_ref(),
                star.upcast_ref(),
                date.upcast_ref(),
            ] {
                top.append(widget);
            }

            let middle = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            let subject = text_label("subject");
            subject.set_hexpand(true);
            let invite = marker("x-office-calendar-symbolic");
            invite.add_css_class("invite");
            invite.set_tooltip_text(Some(&gettext("Invitation")));
            let count = gtk::Label::builder()
                .css_classes(["count"])
                .valign(gtk::Align::Center)
                .build();
            middle.append(&subject);
            middle.append(&invite);
            middle.append(&count);

            let snippet = text_label("snippet");
            snippet.set_wrap(true);
            snippet.set_wrap_mode(pango::WrapMode::WordChar);
            snippet.set_lines(2);
            snippet.set_yalign(0.0);

            content.append(&top);
            content.append(&middle);
            content.append(&snippet);
            row.append(&dot);
            row.append(&avatar);
            row.append(&content);

            let _ = self.avatar.set(avatar);
            let _ = self.account.set(account);
            let _ = self.vip.set(vip);
            let _ = self.from.set(from);
            let _ = self.clip.set(clip);
            let _ = self.mute.set(mute);
            let _ = self.star.set(star);
            let _ = self.date.set(date);
            let _ = self.subject.set(subject);
            let _ = self.invite.set(invite);
            let _ = self.count.set(count);
            let _ = self.snippet.set(snippet);
        }
    }

    impl WidgetImpl for ThreadRow {}
    impl BoxImpl for ThreadRow {}

    fn text_label(class: &str) -> gtk::Label {
        gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(pango::EllipsizeMode::End)
            .width_chars(1)
            .css_classes([class])
            .build()
    }

    fn marker(icon: &str) -> gtk::Image {
        gtk::Image::builder()
            .icon_name(icon)
            .pixel_size(14)
            .css_classes(["marker"])
            .valign(gtk::Align::Center)
            .build()
    }
}

glib::wrapper! {
    pub struct ThreadRow(ObjectSubclass<imp::ThreadRow>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl Default for ThreadRow {
    fn default() -> Self {
        glib::Object::new()
    }
}

/// The CSS class of each flag colour, so binding a row builds no strings.
const FLAG_CLASSES: [&str; 7] = [
    "flag-red",
    "flag-orange",
    "flag-yellow",
    "flag-green",
    "flag-blue",
    "flag-purple",
    "flag-gray",
];

/// The CSS class of each account colour, one per entry of [`PALETTE`].
const ACCOUNT_CLASSES: [&str; 9] = [
    "account-0",
    "account-1",
    "account-2",
    "account-3",
    "account-4",
    "account-5",
    "account-6",
    "account-7",
    "account-8",
];

const _: () = assert!(ACCOUNT_CLASSES.len() == PALETTE.len());
const _: () = assert!(FLAG_CLASSES.len() == FlagColor::ALL.len());

/// The CSS class of each avatar hue, one per entry of [`PALETTE`], which
/// the stylesheet paints in that entry's colour.
const HUE_CLASSES: [&str; 9] = [
    "avatar-hue-0",
    "avatar-hue-1",
    "avatar-hue-2",
    "avatar-hue-3",
    "avatar-hue-4",
    "avatar-hue-5",
    "avatar-hue-6",
    "avatar-hue-7",
    "avatar-hue-8",
];
const _: () = assert!(HUE_CLASSES.len() == PALETTE.len());

/// Paints `avatar` in the hue the conversation gives the same person
/// ([`crate::format::avatar_hue`]).
pub fn set_avatar_hue(avatar: &adw::Avatar, name: &str, address: &str) {
    let hue = crate::format::avatar_hue(name, address);
    for (index, class) in HUE_CLASSES.iter().enumerate() {
        match index == hue {
            true => avatar.add_css_class(class),
            false => avatar.remove_css_class(class),
        }
    }
}

/// The name an avatar draws its initials from.
fn display_name(thread: &ThreadSummary) -> String {
    if thread.from.trim().is_empty() {
        thread.from_email.clone()
    } else {
        thread.from.clone()
    }
}

fn flag_class(color: FlagColor) -> &'static str {
    FLAG_CLASSES[FlagColor::ALL.iter().position(|c| *c == color).unwrap_or(0)]
}

/// What the row says out loud. The dot, the bold and the calendar mark
/// are on screen, so the words say them too.
fn spoken(unread: bool, invitation: bool, sender: &str, subject: &str, snippet: &str) -> String {
    let pattern = match (unread, invitation) {
        (true, true) => gettext("Unread invitation, {sender}, {subject}, {snippet}"),
        (true, false) => gettext("Unread, {sender}, {subject}, {snippet}"),
        (false, true) => gettext("Invitation, {sender}, {subject}, {snippet}"),
        (false, false) => gettext("{sender}, {subject}, {snippet}"),
    };
    fill(&pattern, &[("sender", sender), ("subject", subject), ("snippet", snippet)])
}

/// What opens a row's menu, at the point a click landed on, or over the
/// whole row for `None`.
pub type OpenMenu = Rc<dyn Fn(Option<(f64, f64)>)>;

/// What a row shows where a face goes: nothing while contacts are off,
/// the contact's photo, or their initials.
pub enum Avatar<'a> {
    Hidden,
    Photo(&'a gdk::Texture),
    Initials,
}

impl ThreadRow {
    /// Gives the row its menu, which the list opens from the keyboard.
    pub fn set_menu(&self, open: OpenMenu) {
        self.imp().menu.replace(Some(open));
    }

    /// Opens the row's menu over the row, and says whether it has one.
    pub fn open_menu(&self) -> bool {
        let open = self.imp().menu.borrow().clone();
        open.map(|open| open(None)).is_some()
    }

    pub fn bind(&self, thread: &ThreadSummary, show_account: bool, vip: bool, avatar: Avatar) {
        let imp = self.imp();
        let face = imp.avatar.get().expect("avatar exists");
        match avatar {
            Avatar::Hidden => face.set_visible(false),
            Avatar::Photo(photo) => {
                face.set_visible(true);
                face.set_text(Some(&display_name(thread)));
                face.set_custom_image(Some(photo));
            }
            Avatar::Initials => {
                face.set_visible(true);
                face.set_text(Some(&display_name(thread)));
                face.set_custom_image(gdk::Paintable::NONE);
            }
        }
        set_avatar_hue(face, &thread.from, &thread.from_email);
        imp.vip.get().expect("vip star exists").set_visible(vip);
        let get = |cell: &OnceCell<gtk::Label>| {
            cell.get()
                .expect("row children exist after construction")
                .clone()
        };
        if thread.unread {
            self.add_css_class("unread");
        } else {
            self.remove_css_class("unread");
        }
        let name = if thread.from.is_empty() {
            gettext("Unknown sender")
        } else {
            thread.from.clone()
        };
        get(&imp.from).set_markup(&sender_markup(&name, &thread.from_email));
        let date = get(&imp.date);
        date.set_label(&relative_date(thread.last_message_at, Local::now()));
        if thread.unread {
            date.add_css_class("unread");
        } else {
            date.remove_css_class("unread");
        }
        get(&imp.subject).set_label(&if thread.subject.trim().is_empty() {
            gettext("(no subject)")
        } else {
            thread.subject.clone()
        });
        let count = get(&imp.count);
        count.set_visible(thread.message_count > 1);
        count.set_label(&thread.message_count.to_string());
        get(&imp.snippet).set_label(&one_line(&thread.snippet));
        let flag = imp.star.get().expect("flag exists");
        flag.set_visible(thread.starred);
        let wanted = flag_class(thread.flag_color.unwrap_or(FlagColor::Red));
        if !flag.has_css_class(wanted) {
            for class in FLAG_CLASSES {
                flag.remove_css_class(class);
            }
            flag.add_css_class(wanted);
        }
        imp.clip
            .get()
            .expect("clip exists")
            .set_visible(thread.has_attachments);
        imp.mute
            .get()
            .expect("mute mark exists")
            .set_visible(thread.muted);
        imp.invite
            .get()
            .expect("invitation mark exists")
            .set_visible(thread.invitation);
        let account = imp.account.get().expect("account dot exists");
        account.set_visible(show_account);
        let wanted =
            ACCOUNT_CLASSES[account_color_index(thread.account_id) % ACCOUNT_CLASSES.len()];
        if !account.has_css_class(wanted) {
            for class in ACCOUNT_CLASSES {
                account.remove_css_class(class);
            }
            account.add_css_class(wanted);
        }
        let described = spoken(
            thread.unread,
            thread.invitation,
            &thread.from,
            &thread.subject,
            &thread.snippet,
        );
        self.update_property(&[gtk::accessible::Property::Label(&described)]);
    }
}

/// The sender's name and, dimmed after it, the address (ruling R4), as
/// one line of Pango markup. One label ellipsizes at its end, so the
/// address gives way first and the name only once the address is gone.
/// Two labels in a box would shrink both at once.
fn sender_markup(name: &str, address: &str) -> String {
    let name_part = glib::markup_escape_text(name);
    if address.is_empty() || address == name {
        return name_part.to_string();
    }
    format!(
        "{name_part}\u{2002}<span weight=\"normal\" alpha=\"64%\" size=\"89%\">{}</span>",
        glib::markup_escape_text(address)
    )
}

/// `text` as one paragraph. A label clamps its lines within each
/// paragraph, so a preview that kept the body's line breaks would show
/// every line of it.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::{one_line, sender_markup, spoken};

    #[test]
    fn a_preview_shows_as_one_paragraph() {
        // A label clamps its lines within each paragraph, so a preview
        // stored with line breaks would grow the row.
        assert_eq!(one_line("Hi\r\n\r\nSee you\n  soon"), "Hi See you soon");
        assert_eq!(one_line("  plain "), "plain");
    }

    #[test]
    fn the_address_follows_the_name_in_one_line() {
        assert_eq!(
            sender_markup("Kemi Adeyemi", "k@uni.example"),
            "Kemi Adeyemi\u{2002}<span weight=\"normal\" alpha=\"64%\" size=\"89%\">k@uni.example</span>"
        );
    }

    #[test]
    fn a_name_that_is_the_address_shows_once() {
        assert_eq!(sender_markup("k@uni.example", "k@uni.example"), "k@uni.example");
        assert_eq!(sender_markup("Kemi", ""), "Kemi");
    }

    #[test]
    fn the_sender_line_escapes_markup() {
        assert_eq!(sender_markup("A & <B>", ""), "A &amp; &lt;B&gt;");
    }

    #[test]
    fn a_row_says_what_its_marks_show() {
        assert_eq!(spoken(false, false, "Ann", "Rent", "Due Friday"), "Ann, Rent, Due Friday");
        assert_eq!(spoken(true, false, "Ann", "Rent", "Due Friday"), "Unread, Ann, Rent, Due Friday");
        assert_eq!(
            spoken(false, true, "Priya", "Design review", "Wednesday"),
            "Invitation, Priya, Design review, Wednesday"
        );
        assert_eq!(
            spoken(true, true, "Priya", "Design review", "Wednesday"),
            "Unread invitation, Priya, Design review, Wednesday"
        );
    }
}
