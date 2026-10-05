//! A combo row whose value uses the row's free width.
//!
//! `adw::ComboRow`'s own factory caps its value at 20 characters, so
//! "dana.reyes@example.com" showed as "dana.reyes@example.c…" with half
//! the row empty beside it. [`widen_value`] gives the row factories of its
//! own: the value ellipsizes only once the row runs out of room, and the
//! open list keeps the check mark beside the chosen entry.

use adw::prelude::*;

/// The most characters of the value the row keeps before anything else
/// may take its room; past them it ellipsizes when the row is short.
const MOST_KEPT: usize = 26;

/// Lets `row`'s value, a `gtk::StringList` entry, take the row's free
/// width before it ellipsizes.
pub fn widen_value(row: &adw::ComboRow) {
    let shown = gtk::SignalListItemFactory::new();
    shown.connect_setup(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
        let label = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        item.set_child(Some(&label));
    });
    shown.connect_bind(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
        if let Some(label) = item.child().and_downcast::<gtk::Label>() {
            let text = text_of(item);
            // The row shares its width out by what each side asks for at
            // least, and an ellipsizing label asks for one character, so a
            // long subtitle beside it took the room. Asking for the value's
            // own width, up to an address's usual length, keeps it whole.
            label.set_width_chars(text.chars().count().min(MOST_KEPT) as i32);
            label.set_label(&text);
        }
    });
    row.set_factory(Some(&shown));

    let listed = gtk::SignalListItemFactory::new();
    let weak = row.downgrade();
    listed.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
        let label = gtk::Label::builder().xalign(0.0).hexpand(true).build();
        let check = gtk::Image::builder()
            .icon_name("object-select-symbolic")
            .accessible_role(gtk::AccessibleRole::Presentation)
            .build();
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        line.append(&label);
        line.append(&check);
        item.set_child(Some(&line));
        // The check follows the row's choice while the list is open; the
        // handler holds the widgets weakly and does nothing once they go.
        let Some(row) = weak.upgrade() else { return };
        let (item_ref, check_ref) = (item.downgrade(), check.downgrade());
        row.connect_selected_item_notify(move |row| {
            if let (Some(item), Some(check)) = (item_ref.upgrade(), check_ref.upgrade()) {
                check.set_opacity(chosen(row, &item));
            }
        });
    });
    let weak = row.downgrade();
    listed.connect_bind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
        let Some(line) = item.child().and_downcast::<gtk::Box>() else { return };
        if let Some(label) = line.first_child().and_downcast::<gtk::Label>() {
            label.set_label(&text_of(item));
        }
        if let (Some(check), Some(row)) = (line.last_child(), weak.upgrade()) {
            check.set_opacity(chosen(&row, item));
        }
    });
    row.set_list_factory(Some(&listed));
}

/// The text of the list entry `item` shows.
fn text_of(item: &gtk::ListItem) -> String {
    item.item()
        .and_downcast::<gtk::StringObject>()
        .map(|entry| entry.string().to_string())
        .unwrap_or_default()
}

/// 1 when `item` holds the row's chosen entry, else 0, for the check's
/// opacity, which keeps every line the same width.
fn chosen(row: &adw::ComboRow, item: &gtk::ListItem) -> f64 {
    match item.item().is_some() && row.selected_item() == item.item() {
        true => 1.0,
        false => 0.0,
    }
}
