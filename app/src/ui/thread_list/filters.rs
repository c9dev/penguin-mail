//! Quick conditions for the current list, kept while moving between mailboxes.

use std::rc::Rc;

use gtk::prelude::*;
use mailrs_domain::ListFilter;
use mailrs_domain::translate::gettext;

pub struct Filters {
    pub button: gtk::MenuButton,
    unread: gtk::CheckButton,
    flagged: gtk::CheckButton,
    attachments: gtk::CheckButton,
}

impl Filters {
    pub fn new() -> Self {
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        let unread = gtk::CheckButton::with_label(&gettext("Unread Only"));
        let flagged = gtk::CheckButton::with_label(&gettext("Flagged Only"));
        let attachments = gtk::CheckButton::with_label(&gettext("With Attachments"));
        for check in [&unread, &flagged, &attachments] {
            super::super::name(check, &check.label().unwrap_or_default());
            content.append(check);
        }
        let hint = gtk::Label::builder()
            .label(gettext("Match all selected filters"))
            .css_classes(["dim-label", "caption"])
            .build();
        content.append(&hint);
        let clear = gtk::Button::with_label(&gettext("Clear Filters"));
        content.append(&clear);
        let checks = [unread.clone(), flagged.clone(), attachments.clone()];
        clear.connect_clicked(move |_| {
            for check in &checks {
                check.set_active(false);
            }
        });
        let popover = gtk::Popover::builder().child(&content).build();
        let button = gtk::MenuButton::builder()
            .icon_name("penguin-mail-filter-symbolic")
            .tooltip_text(gettext("Filter Messages"))
            .popover(&popover)
            .build();
        super::super::name(&button, &gettext("Filter Messages"));
        Self {
            button,
            unread,
            flagged,
            attachments,
        }
    }

    pub fn clear(&self) {
        for check in [&self.unread, &self.flagged, &self.attachments] {
            check.set_active(false);
        }
    }

    pub fn get(&self) -> ListFilter {
        ListFilter {
            unread: self.unread.is_active(),
            flagged: self.flagged.is_active(),
            attachments: self.attachments.is_active(),
        }
    }

    pub fn connect_changed(&self, changed: impl Fn() + 'static) {
        let changed = Rc::new(changed);
        for check in [&self.unread, &self.flagged, &self.attachments] {
            let changed = Rc::clone(&changed);
            let button = self.button.downgrade();
            let checks = [
                self.unread.downgrade(),
                self.flagged.downgrade(),
                self.attachments.downgrade(),
            ];
            check.connect_toggled(move |_| {
                let Some(button) = button.upgrade() else {
                    return;
                };
                let active = checks
                    .iter()
                    .filter_map(|check| check.upgrade())
                    .any(|check| check.is_active());
                if active {
                    button.add_css_class("accent");
                } else {
                    button.remove_css_class("accent");
                }
                let label = if active {
                    gettext("Filter Messages (Active)")
                } else {
                    gettext("Filter Messages")
                };
                button.set_tooltip_text(Some(&label));
                super::super::name(&button, &label);
                changed();
            });
        }
    }
}

#[cfg(test)]
pub(super) fn check() {
    use std::cell::Cell;
    let filters = Filters::new();
    let changes = Rc::new(Cell::new(0));
    let calls = Rc::clone(&changes);
    filters.connect_changed(move || calls.set(calls.get() + 1));
    assert!(!filters.get().active());
    filters.unread.set_active(true);
    filters.flagged.set_active(true);
    assert!(filters.get().unread && filters.get().flagged);
    assert!(!filters.get().attachments);
    assert!(filters.button.has_css_class("accent"));
    filters.clear();
    assert!(!filters.get().active());
    assert!(!filters.button.has_css_class("accent"));
    assert_eq!(changes.get(), 4);
}
