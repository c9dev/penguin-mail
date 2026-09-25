//! The GTK interface. Widgets are built in code; only the thread row is a
//! GObject subclass, because list rows need a widget type to recycle.

pub mod about;
pub mod add_account;
pub mod assistant;
pub mod assistant_mcp_prefs;
pub mod assistant_prefs;
pub mod assistant_skills_prefs;
pub mod assistant_web_prefs;
pub mod autocomplete;
pub mod calendar;
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
pub mod permission;
pub mod pgp;
pub mod preferences;
pub mod queued;
pub mod roving;
pub mod rules;
pub mod search_suggest;
pub mod sidebar;
pub mod smart_editor;
pub mod templates;
pub mod thread_list;
pub mod thread_row;
pub mod translation;
pub mod unsubscribe;
pub mod vacation;
pub mod welcome;
pub mod when;
pub mod window;

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gio, glib};
use mailrs_domain::translate::gettext;
use mailrs_domain::Folder;
pub use mailrs_sync::Mailbox;
pub use mailrs_sync::mailbox::Standard;
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

/// Names every item of `menu` after the words it shows, each time the
/// menu opens and again whenever its model changes while it is open.
///
/// GTK builds the items of a menu model itself and ties each one to its
/// words through a labelled-by relation whose target never reaches the
/// accessible tree, so a screen reader hears a menu item with no name.
/// GTK builds an item again when the model changes, as when Mute turns
/// into Unmute, and the new item comes without a name.
pub fn name_menu_items(menu: &gtk::PopoverMenu) {
    let watched = Rc::new(RefCell::new(Vec::new()));
    menu.connect_map(move |menu| {
        name_model_buttons(menu.upcast_ref());
        if let Some(model) = menu.menu_model() {
            watch_menu_model(menu, &model, &watched);
        }
    });
}

/// Names the items of `menu` again after `model`, or a section or
/// submenu inside it, changes while the menu is open. Each model is
/// watched once, however often the menu opens.
fn watch_menu_model(
    menu: &gtk::PopoverMenu,
    model: &gio::MenuModel,
    watched: &Rc<RefCell<Vec<glib::WeakRef<gio::MenuModel>>>>,
) {
    let known = {
        let mut list = watched.borrow_mut();
        list.retain(|seen| seen.upgrade().is_some());
        let known = list
            .iter()
            .any(|seen| seen.upgrade().as_ref() == Some(model));
        if !known {
            list.push(model.downgrade());
        }
        known
    };
    if !known {
        let (weak, watched) = (menu.downgrade(), Rc::clone(watched));
        model.connect_items_changed(move |model, _, _, _| {
            let Some(menu) = weak.upgrade().filter(|menu| menu.is_mapped()) else {
                return;
            };
            // GTK rebuilds the changed items in a handler of its own, so
            // the naming waits until that has run.
            let (model, watched) = (model.clone(), Rc::clone(&watched));
            glib::idle_add_local_once(move || {
                name_model_buttons(menu.upcast_ref());
                watch_menu_model(&menu, &model, &watched);
            });
        });
    }
    for index in 0..model.n_items() {
        for link in [gio::MENU_LINK_SECTION, gio::MENU_LINK_SUBMENU] {
            if let Some(inner) = model.item_link(index, link) {
                watch_menu_model(menu, &inner, watched);
            }
        }
    }
}

/// Names the items of the menu a menu button or split button opens,
/// including a menu the button is given later through `set_menu_model`,
/// which builds a new popover each time.
pub fn name_menu_items_of(button: &impl IsA<gtk::Widget>) {
    fn hook(button: &gtk::Widget) {
        let popover = button.property::<Option<gtk::Popover>>("popover");
        if let Some(menu) = popover.and_downcast::<gtk::PopoverMenu>() {
            name_menu_items(&menu);
        }
    }
    let button = button.as_ref();
    hook(button);
    button.connect_notify_local(Some("popover"), |button, _| hook(button));
}

/// Names the items of a menu open under `widget` that another library
/// built, such as the one WebKit shows on a right click in a page.
pub fn name_menu_items_under(widget: &impl IsA<gtk::Widget>) {
    name_model_buttons(widget.as_ref());
}

/// Names every menu item GTK built from a model under `widget`, the
/// pages of submenus included. `GtkModelButton` is private to GTK, so it
/// is found by its type name and read through its `text` property.
fn name_model_buttons(widget: &gtk::Widget) {
    let mut child = widget.first_child();
    while let Some(item) = child {
        if item.type_().name() == "GtkModelButton" {
            let text = item.property::<Option<String>>("text").unwrap_or_default();
            let spoken = without_mnemonic(&text);
            if !spoken.is_empty() {
                // A labelled-by relation outranks a name, so the broken
                // one GTK set has to go before the name is heard.
                item.reset_relation(gtk::AccessibleRelation::LabelledBy);
                name(&item, &spoken);
            }
        }
        name_model_buttons(&item);
        child = item.next_sibling();
    }
}

/// A menu item's words as shown: an underscore marks the key after it
/// and a doubled one stands for itself.
fn without_mnemonic(text: &str) -> String {
    let mut shown = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '_' => shown.extend(chars.next()),
            c => shown.push(c),
        }
    }
    shown
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

#[cfg(test)]
mod tests {
    use super::{split_shortcut, without_mnemonic};

    #[test]
    fn a_menu_item_is_named_without_its_mnemonic_marks() {
        assert_eq!(without_mnemonic("_Reply"), "Reply");
        assert_eq!(without_mnemonic("Move to _Trash"), "Move to Trash");
        assert_eq!(without_mnemonic("work__notes"), "work_notes");
        assert_eq!(without_mnemonic("Export…"), "Export…");
        assert_eq!(without_mnemonic("trailing_"), "trailing");
        assert_eq!(without_mnemonic(""), "");
    }

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
