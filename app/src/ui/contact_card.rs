//! The card behind a sender's name in a conversation: their photo, name,
//! addresses, and where they work, with the three things the app can
//! already do about a person.
//!
//! A sender who is in no address book still gets a card, built from the
//! message header alone, so clicking a name always answers.

use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, pango};

/// Who the card is about.
pub struct Person {
    pub name: String,
    /// The address to write to.
    pub email: String,
    /// Every address, the one to write to first.
    pub addresses: Vec<String>,
    pub organization: Option<String>,
    pub phone: Option<String>,
    /// The contact photo on disk, when there is one.
    pub photo: Option<PathBuf>,
    pub vip: bool,
}

/// What the reader picked. Each one is something the app does elsewhere.
pub enum Choice {
    /// Write to this address.
    Write(String),
    /// Add to the VIPs, or take out again.
    ToggleVip,
    /// Search for everything this person sent.
    AllMail,
}

/// Shows the card over `parent`.
pub fn present(parent: &impl IsA<gtk::Widget>, person: Person, chose: impl Fn(Choice) + 'static) {
    let chose = Rc::new(chose);
    let dialog = adw::Dialog::builder()
        .title("Contact")
        .content_width(380)
        .build();

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(24)
        .margin_end(24)
        .build();

    let avatar = adw::Avatar::builder()
        .size(96)
        .text(&person.name)
        .show_initials(true)
        .halign(gtk::Align::Center)
        .build();
    if let Some(photo) = person
        .photo
        .as_ref()
        .and_then(|path| gdk::Texture::from_filename(path).ok())
    {
        avatar.set_custom_image(Some(&photo));
    }
    content.append(&avatar);

    let name = gtk::Label::builder()
        .label(&person.name)
        .wrap(true)
        .justify(gtk::Justification::Center)
        .css_classes(["title-2"])
        .build();
    content.append(&name);
    if let Some(organization) = person.organization.as_deref() {
        content.append(
            &gtk::Label::builder()
                .label(organization)
                .wrap(true)
                .justify(gtk::Justification::Center)
                .css_classes(["dim-label"])
                .build(),
        );
    }

    let details = adw::PreferencesGroup::new();
    for address in &person.addresses {
        let row = adw::ActionRow::builder()
            .title(address)
            .subtitle(if address == &person.email {
                "Email"
            } else {
                "Other email"
            })
            .activatable(true)
            .build();
        row.add_prefix(&gtk::Image::from_icon_name("mail-unread-symbolic"));
        row.set_title_lines(1);
        if let Some(label) = row.first_child() {
            label.set_tooltip_text(Some(address));
        }
        let (chosen, address) = (Rc::clone(&chose), address.clone());
        let closing = dialog.clone();
        row.connect_activated(move |_| {
            closing.close();
            chosen(Choice::Write(address.clone()));
        });
        details.add(&row);
    }
    if let Some(phone) = person.phone.as_deref() {
        let row = adw::ActionRow::builder()
            .title(phone)
            .subtitle("Phone")
            .build();
        row.add_prefix(&gtk::Image::from_icon_name("call-start-symbolic"));
        details.add(&row);
    }
    content.append(&details);

    let actions = gtk::Box::builder()
        .spacing(6)
        .homogeneous(true)
        .margin_top(6)
        .build();
    let button = |label: &str, icon: &str| {
        let inner = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .build();
        inner.append(&gtk::Image::from_icon_name(icon));
        inner.append(
            &gtk::Label::builder()
                .label(label)
                .ellipsize(pango::EllipsizeMode::End)
                .css_classes(["caption"])
                .build(),
        );
        gtk::Button::builder().child(&inner).build()
    };

    let write = button("New Message", "mail-message-new-symbolic");
    let (chosen, address, closing) = (Rc::clone(&chose), person.email.clone(), dialog.clone());
    write.connect_clicked(move |_| {
        closing.close();
        chosen(Choice::Write(address.clone()));
    });
    actions.append(&write);

    let vip = button(
        if person.vip {
            "Remove from VIPs"
        } else {
            "Add to VIPs"
        },
        "starred-symbolic",
    );
    let (chosen, closing) = (Rc::clone(&chose), dialog.clone());
    vip.connect_clicked(move |_| {
        closing.close();
        chosen(Choice::ToggleVip);
    });
    actions.append(&vip);

    let mail = button("All Their Mail", "system-search-symbolic");
    let (chosen, closing) = (Rc::clone(&chose), dialog.clone());
    mail.connect_clicked(move |_| {
        closing.close();
        chosen(Choice::AllMail);
    });
    actions.append(&mail);
    content.append(&actions);

    let view = adw::ToolbarView::builder().content(&content).build();
    view.add_top_bar(&adw::HeaderBar::builder().show_title(false).build());
    dialog.set_child(Some(&view));
    dialog.present(Some(parent));
}
