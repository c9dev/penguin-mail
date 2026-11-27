//! The small pieces the mockup draws over and over: a section heading, a
//! grouped list of lines, a chip, an avatar, a segmented choice. Each one
//! is a widget with the `cts-` classes of `style.css`.

use adw::prelude::*;
use gtk::glib;
use mailrs_domain::AccountId;

use super::people::{hue, initials};
use crate::format::{account_color_index, account_label};

pub fn icon(name: &str) -> gtk::Image {
    gtk::Image::builder()
        .icon_name(format!("penguin-mail-ct-{name}-symbolic"))
        .pixel_size(16)
        .css_classes(["cts-icon"])
        .build()
}

pub fn label(text: &str, classes: &[&str]) -> gtk::Label {
    // Headings and counts stay on one line; the rest may wrap.
    let one_line = classes.iter().any(|c| matches!(*c, "cts-h2" | "cts-count" | "cts-kicker" | "cts-keep-name" | "cts-who-name"));
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(!one_line)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .css_classes(classes.iter().map(|c| c.to_string()).collect::<Vec<_>>())
        .build()
}

/// The 8 px dot of an account, in the colour its mail rows use.
pub fn dot(account: AccountId) -> gtk::Widget {
    gtk::Box::builder()
        .valign(gtk::Align::Center)
        .css_classes([
            "account-dot".to_string(),
            format!("account-{}", account_color_index(account)),
        ])
        .build()
        .upcast()
}

/// A pill with the account's dot and short name.
pub fn chip(account: AccountId) -> gtk::Widget {
    let chip = gtk::Box::builder()
        .spacing(6)
        .valign(gtk::Align::Center)
        .halign(gtk::Align::Start)
        .css_classes(["cts-chip"])
        .tooltip_text(account_label(account).full)
        .build();
    chip.append(&dot(account));
    chip.append(&gtk::Label::new(Some(&account_label(account).short)));
    chip.upcast()
}

pub fn chips(accounts: &[AccountId]) -> gtk::Box {
    let row = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .orientation(gtk::Orientation::Horizontal)
        .homogeneous(false)
        .column_spacing(6)
        .row_spacing(6)
        .min_children_per_line(1)
        .max_children_per_line(8)
        .halign(gtk::Align::Start)
        .can_target(false)
        .focusable(false)
        .build();
    for account in accounts {
        row.append(&chip(*account));
    }
    let holder = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    holder.append(&row);
    holder
}

/// A round avatar: the photo when there is one, else initials on a flat
/// colour.
pub fn avatar(name: &str, size: i32, photo: Option<&gtk::gdk::Texture>) -> gtk::Widget {
    if let Some(photo) = photo {
        let avatar = adw::Avatar::builder()
            .size(size)
            .text(name)
            .show_initials(true)
            .build();
        avatar.set_custom_image(Some(photo));
        return avatar.upcast();
    }
    gtk::Label::builder()
        .label(initials(name))
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .xalign(0.5)
        .yalign(0.5)
        .justify(gtk::Justification::Center)
        .width_request(size)
        .height_request(size)
        .css_classes([
            "cts-avatar".to_string(),
            (if size > 40 { "lg" } else { "sm" }).to_string(),
            format!("cts-hue-{}", hue(name)),
        ])
        .build()
        .upcast()
}

pub fn sec(title: &str) -> gtk::Label {
    let l = label(title, &["cts-sec"]);
    l.update_property(&[gtk::accessible::Property::Label(title)]);
    l
}

pub fn hint(text: &str) -> gtk::Label {
    label(text, &["cts-hint"])
}

/// A bordered box that holds lines, with a hairline between them.
pub fn group(lines: &[gtk::Widget]) -> gtk::Box {
    let group = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["cts-group"])
        .build();
    group.set_overflow(gtk::Overflow::Hidden);
    for (at, line) in lines.iter().enumerate() {
        if at > 0 {
            line.add_css_class("after");
        }
        group.append(line);
    }
    group
}

/// A line: icon, a title in bold, a dim second line, and anything after.
pub fn line(icon_name: Option<&str>, title: &str, sub: &str, extra: Option<&gtk::Widget>) -> gtk::Box {
    let line = gtk::Box::builder()
        .spacing(12)
        .css_classes(["cts-line"])
        .build();
    if let Some(name) = icon_name {
        let i = icon(name);
        i.set_valign(gtk::Align::Center);
        line.append(&i);
    }
    let text = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .build();
    let title = label(title, &["cts-title"]);
    title.set_selectable(true);
    text.append(&title);
    if !sub.is_empty() {
        text.append(&label(sub, &["cts-sub"]));
    }
    line.append(&text);
    if let Some(extra) = extra {
        line.append(extra);
    }
    line
}

/// A line that opens something, with a chevron.
pub fn go_line(icon_name: &str, title: &str, sub: &str, clicked: impl Fn() + 'static) -> gtk::Button {
    let inner = line(Some(icon_name), title, sub, None);
    inner.append(
        &gtk::Image::builder()
            .icon_name("penguin-mail-chevron-right-symbolic")
            .pixel_size(16)
            .valign(gtk::Align::Center)
            .css_classes(["cts-chev"])
            .build(),
    );
    // The title is selectable text: inside a button it must not take clicks.
    let button = gtk::Button::builder()
        .child(&inner)
        .css_classes(["cts-go"])
        .build();
    mark_unselectable(inner.upcast_ref());
    button.connect_clicked(move |_| clicked());
    super::super::name(&button, &format!("{title}, {sub}"));
    button
}

fn mark_unselectable(widget: &gtk::Widget) {
    if let Some(l) = widget.downcast_ref::<gtk::Label>() {
        l.set_selectable(false);
    }
    let mut child = widget.first_child();
    while let Some(c) = child {
        mark_unselectable(&c);
        child = c.next_sibling();
    }
}

pub fn switch_line(
    icon_name: &str,
    title: &str,
    sub: &str,
    spoken: &str,
    on: bool,
    toggled: impl Fn(bool) + 'static,
) -> gtk::Box {
    let switch = gtk::Switch::builder()
        .active(on)
        .valign(gtk::Align::Center)
        .css_classes(["cts-switch"])
        .build();
    super::super::name(&switch, spoken);
    switch.connect_state_set(move |s, state| {
        toggled(state);
        s.set_state(state);
        glib::Propagation::Stop
    });
    line(Some(icon_name), title, sub, Some(switch.upcast_ref()))
}

/// A pill button as the mockup's `.btn`.
pub fn button(text: &str, classes: &[&str]) -> gtk::Button {
    let b = gtk::Button::builder()
        .label(text)
        .valign(gtk::Align::Center)
        .css_classes(
            std::iter::once("cts-btn")
                .chain(classes.iter().copied())
                .map(String::from)
                .collect::<Vec<_>>(),
        )
        .build();
    b
}

pub fn icon_button(name: &str, spoken: &str, classes: &[&str]) -> gtk::Button {
    let b = gtk::Button::builder()
        .child(&icon(name))
        .tooltip_text(spoken)
        .valign(gtk::Align::Center)
        .css_classes(
            std::iter::once("cts-iconbtn")
                .chain(classes.iter().copied())
                .map(String::from)
                .collect::<Vec<_>>(),
        )
        .build();
    super::super::name(&b, spoken);
    b
}

/// The mockup's segmented choice between a few words. `chosen` runs with
/// the index picked.
pub fn seg(spoken: &str, options: &[String], on: usize, chosen: impl Fn(usize) + 'static) -> gtk::Box {
    let holder = gtk::Box::builder()
        .spacing(0)
        .homogeneous(true)
        .css_classes(["cts-seg"])
        .build();
    holder.update_property(&[gtk::accessible::Property::Label(spoken)]);
    let buttons: std::rc::Rc<Vec<gtk::ToggleButton>> = std::rc::Rc::new(
        options
            .iter()
            .enumerate()
            .map(|(at, text)| {
                let b = gtk::ToggleButton::builder()
                    .label(text)
                    .active(at == on)
                    .hexpand(true)
                    .build();
                holder.append(&b);
                b
            })
            .collect(),
    );
    let chosen = std::rc::Rc::new(chosen);
    for (at, b) in buttons.iter().enumerate() {
        let (all, chosen) = (buttons.clone(), chosen.clone());
        b.connect_toggled(move |me| {
            if !me.is_active() {
                // Pressing the one that is on keeps it on.
                if all.iter().all(|o| !o.is_active()) {
                    me.set_active(true);
                }
                return;
            }
            for (other_at, other) in all.iter().enumerate() {
                if other_at != at && other.is_active() {
                    other.set_active(false);
                }
            }
            chosen(at);
        });
    }
    holder
}

/// A labelled field of the editor: the label above, the entry below.
pub fn entry(label_text: &str, value: &str, placeholder: &str) -> (gtk::Box, gtk::Entry) {
    let holder = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["cts-entry"])
        .build();
    let name = gtk::Label::builder()
        .label(label_text)
        .xalign(0.0)
        .css_classes(["cts-entry-label"])
        .build();
    let field = gtk::Entry::builder()
        .text(value)
        .placeholder_text(placeholder)
        .has_frame(false)
        .css_classes(["cts-entry-field"])
        .build();
    field.update_property(&[gtk::accessible::Property::Label(label_text)]);
    name.set_mnemonic_widget(Some(&field));
    holder.append(&name);
    holder.append(&field);
    // A click anywhere in the field's box lands in the entry.
    let click = gtk::GestureClick::new();
    let weak = field.downgrade();
    click.connect_pressed(move |_, _, _, _| {
        if let Some(f) = weak.upgrade() {
            f.grab_focus();
        }
    });
    holder.add_controller(click);
    (holder, field)
}

/// A radio line with a second line, as the merge and link choices use.
pub fn choice(
    group_head: Option<&gtk::CheckButton>,
    title: &str,
    sub: &str,
    on: bool,
) -> (gtk::Box, gtk::CheckButton) {
    let check = gtk::CheckButton::builder()
        .active(on)
        .css_classes(["cts-radio"])
        .valign(gtk::Align::Center)
        .build();
    if let Some(head) = group_head {
        check.set_group(Some(head));
    }
    let row = gtk::Box::builder()
        .spacing(10)
        .css_classes(["cts-choice"])
        .build();
    row.append(&check);
    let text = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .hexpand(true)
        .build();
    text.append(&label(title, &["cts-title"]));
    if !sub.is_empty() {
        text.append(&label(sub, &["cts-sub"]));
    }
    row.append(&text);
    let click = gtk::GestureClick::new();
    let weak = check.downgrade();
    click.connect_released(move |_, _, _, _| {
        if let Some(c) = weak.upgrade() {
            c.set_active(true);
        }
    });
    row.add_controller(click);
    check.update_property(&[gtk::accessible::Property::Label(&format!("{title}. {sub}"))]);
    (row, check)
}
