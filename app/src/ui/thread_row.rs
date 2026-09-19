use std::cell::OnceCell;

use chrono::Local;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{glib, pango};
use mailrs_domain::{FlagColor, ThreadSummary};

use crate::format::{PALETTE, account_color_index, relative_date};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct ThreadRow {
        pub account: OnceCell<gtk::Box>,
        pub vip: OnceCell<gtk::Image>,
        pub from: OnceCell<gtk::Label>,
        pub clip: OnceCell<gtk::Image>,
        pub star: OnceCell<gtk::Image>,
        pub date: OnceCell<gtk::Label>,
        pub subject: OnceCell<gtk::Label>,
        pub count: OnceCell<gtk::Label>,
        pub snippet: OnceCell<gtk::Label>,
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
            row.set_spacing(8);
            row.add_css_class("thread-row");

            let dot = gtk::Box::builder()
                .valign(gtk::Align::Start)
                .css_classes(["unread-dot"])
                .build();
            let content = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(2)
                .hexpand(true)
                .build();

            let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            let account = gtk::Box::builder()
                .valign(gtk::Align::Center)
                .css_classes(["account-dot"])
                .visible(false)
                .build();
            let vip = marker("starred-symbolic");
            vip.add_css_class("vip");
            vip.set_tooltip_text(Some("VIP"));
            let from = text_label("from");
            from.set_hexpand(true);
            let clip = marker("mail-attachment-symbolic");
            let star = marker("mailrs-flag-symbolic");
            star.add_css_class("starred");
            let date = text_label("date");
            date.set_ellipsize(pango::EllipsizeMode::None);
            for widget in [
                account.upcast_ref::<gtk::Widget>(),
                vip.upcast_ref(),
                from.upcast_ref(),
                clip.upcast_ref(),
                star.upcast_ref(),
                date.upcast_ref(),
            ] {
                top.append(widget);
            }

            let middle = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            let subject = text_label("subject");
            subject.set_hexpand(true);
            let count = gtk::Label::builder()
                .css_classes(["count"])
                .valign(gtk::Align::Center)
                .build();
            middle.append(&subject);
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
            row.append(&content);

            let _ = self.account.set(account);
            let _ = self.vip.set(vip);
            let _ = self.from.set(from);
            let _ = self.clip.set(clip);
            let _ = self.star.set(star);
            let _ = self.date.set(date);
            let _ = self.subject.set(subject);
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

impl ThreadRow {
    pub fn bind(&self, thread: &ThreadSummary, show_account: bool, vip: bool) {
        let imp = self.imp();
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
        let from = get(&imp.from);
        from.set_label(if thread.from.is_empty() {
            "Unknown sender"
        } else {
            &thread.from
        });
        let date = get(&imp.date);
        date.set_label(&relative_date(thread.last_message_at, Local::now()));
        if thread.unread {
            date.add_css_class("unread");
        } else {
            date.remove_css_class("unread");
        }
        get(&imp.subject).set_label(if thread.subject.trim().is_empty() {
            "(no subject)"
        } else {
            &thread.subject
        });
        let count = get(&imp.count);
        count.set_visible(thread.message_count > 1);
        count.set_label(&thread.message_count.to_string());
        get(&imp.snippet).set_label(&thread.snippet);
        let flag = imp.star.get().expect("flag exists");
        flag.set_visible(thread.starred);
        for color in FlagColor::ALL {
            flag.remove_css_class(&format!("flag-{}", color.as_str()));
        }
        flag.add_css_class(&format!(
            "flag-{}",
            thread.flag_color.unwrap_or(FlagColor::Red).as_str()
        ));
        imp.clip
            .get()
            .expect("clip exists")
            .set_visible(thread.has_attachments);
        let account = imp.account.get().expect("account dot exists");
        account.set_visible(show_account);
        for index in 0..PALETTE.len() {
            account.remove_css_class(&format!("account-{index}"));
        }
        account.add_css_class(&format!(
            "account-{}",
            account_color_index(thread.account_id)
        ));
        self.update_property(&[gtk::accessible::Property::Label(&format!(
            "{}{}, {}, {}",
            if thread.unread { "Unread, " } else { "" },
            thread.from,
            thread.subject,
            thread.snippet
        ))]);
    }
}
