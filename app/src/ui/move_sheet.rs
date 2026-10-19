//! The sheet that shows once, at the first start after the update, over
//! the window: it lists the Gmail accounts that wait to choose how they
//! sign in.
//!
//! It is an `adw::Dialog` of its own, not an `adw::AlertDialog`: libadwaita
//! holds an alert to about 300 px, and the approved layout is 440 px wide
//! with the heading on one line. Below that width the dialog becomes a
//! bottom sheet on its own.

use adw::prelude::*;
use mailrs_domain::Account;
use mailrs_domain::translate::gettext;

/// The dialog's width, the mockup's.
const WIDTH: i32 = 440;

/// The sheet and the parts a caller or a check reaches.
pub struct Sheet {
    pub dialog: adw::Dialog,
    #[cfg_attr(not(test), expect(dead_code, reason = "only the checks press it"))]
    pub later: gtk::Button,
    pub choose: gtk::Button,
}

fn row(address: &str) -> gtk::ListBoxRow {
    let dot = gtk::Box::builder()
        .valign(gtk::Align::Center)
        .css_classes(["move-sheet-dot"])
        .build();
    let label = gtk::Label::builder()
        .label(address)
        .use_markup(false)
        .xalign(0.0)
        .hexpand(true)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .css_classes(["move-sheet-address"])
        .build();
    let line = gtk::Box::builder()
        .spacing(10)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(16)
        .margin_end(16)
        .build();
    line.append(&dot);
    line.append(&label);
    let row = gtk::ListBoxRow::builder()
        .child(&line)
        .activatable(false)
        .build();
    // A list item takes no name from the label inside it, so a screen
    // reader would say "list item" for each address.
    row.update_property(&[gtk::accessible::Property::Label(address)]);
    row
}

/// The sheet for `accounts`. `choose` runs, after the sheet closes, when
/// the person picks "Choose Now". Escape, "Later" and a swipe down all
/// close it without running `choose`.
pub fn build(accounts: &[Account], choose: impl Fn() + 'static) -> Sheet {
    // Translators: Gmail is a name; keep it as it is.
    let heading_text = gettext("Choose how Penguin Mail signs in to Gmail");
    // Translators: Google and Gmail are names; keep them as they are.
    let body_text = gettext(
        "Penguin Mail has new ways to sign in to Google. Pick one for each Gmail account below to keep your mail arriving. Your mail and settings stay as they are.",
    );
    let heading = gtk::Label::builder()
        .label(heading_text.as_str())
        .wrap(true)
        .justify(gtk::Justification::Center)
        .css_classes(["move-sheet-heading"])
        .build();
    let body = gtk::Label::builder()
        .label(body_text.as_str())
        .wrap(true)
        .justify(gtk::Justification::Center)
        .css_classes(["move-sheet-body"])
        .build();
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    for account in accounts {
        list.append(&row(&account.email));
    }
    let later = gtk::Button::builder()
        .label(gettext("Later"))
        .css_classes(["move-sheet-button"])
        .build();
    let choose_now = gtk::Button::builder()
        .label(gettext("Choose Now"))
        .css_classes(["move-sheet-button", "suggested-action"])
        .build();
    let actions = gtk::Box::builder()
        .spacing(10)
        .halign(gtk::Align::Center)
        .build();
    actions.append(&later);
    actions.append(&choose_now);
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .margin_top(24)
        .margin_bottom(20)
        .margin_start(20)
        .margin_end(20)
        .build();
    content.append(&heading);
    content.append(&body);
    content.append(&list);
    content.append(&actions);
    // The gaps between the parts, which differ from line to line.
    heading.set_margin_bottom(7);
    body.set_margin_bottom(17);
    list.set_margin_bottom(20);
    let dialog = adw::Dialog::builder()
        .title(heading_text.as_str())
        .content_width(WIDTH)
        .child(&content)
        .default_widget(&choose_now)
        .build();
    dialog.add_css_class("move-sheet");
    // In a window too narrow for the full width, the heading drops to a
    // size that still fits one line. Narrower still, it wraps anyway, at
    // the mockup's size.
    if let Ok(condition) = adw::BreakpointCondition::parse("min-width: 400px and max-width: 439px")
    {
        let cramped = adw::Breakpoint::new(condition);
        cramped.add_setter(
            &content,
            "css-classes",
            Some(&["move-sheet-cramped"].to_value()),
        );
        dialog.add_breakpoint(cramped);
    }
    let closing = dialog.downgrade();
    later.connect_clicked(move |_| {
        if let Some(dialog) = closing.upgrade() {
            dialog.close();
        }
    });
    let closing = dialog.downgrade();
    choose_now.connect_clicked(move |_| {
        if let Some(dialog) = closing.upgrade() {
            dialog.close();
        }
        choose();
    });
    Sheet {
        dialog,
        later,
        choose: choose_now,
    }
}

/// Shows the sheet over `parent`, with the focus on "Choose Now".
pub fn present(parent: &impl IsA<gtk::Widget>, accounts: &[Account], choose: impl Fn() + 'static) {
    let sheet = build(accounts, choose);
    sheet.dialog.present(Some(parent));
    // The dialog picks its own focus once it presents, usually the first
    // button.
    if sheet.choose.is_sensitive() {
        sheet.dialog.set_focus(Some(&sheet.choose));
    }
}

#[cfg(test)]
pub(crate) mod checks {
    use super::*;
    use mailrs_domain::{Account, AccountState, Provider};
    use std::cell::Cell;
    use std::rc::Rc;

    pub fn run() {
        the_sheet_lists_each_address_plainly();
        each_address_row_has_a_name();
        choose_now_is_the_default_and_the_dialog_is_440_wide();
        choose_now_walks_and_later_and_escape_do_not();
    }

    fn gmail(id: i64, email: &str) -> Account {
        Account {
            id,
            email: email.into(),
            state: AccountState::Ok,
            provider: Provider::Gmail,
            provider_name: None,
        }
    }

    fn texts(widget: &gtk::Widget, found: &mut Vec<String>) {
        if let Some(label) = widget.downcast_ref::<gtk::Label>() {
            assert!(!label.uses_markup());
            found.push(label.label().to_string());
        }
        let mut child = widget.first_child();
        while let Some(w) = child {
            texts(&w, found);
            child = w.next_sibling();
        }
    }

    fn the_sheet_lists_each_address_plainly() {
        let accounts = [gmail(1, "r&d@example.com"), gmail(2, "b@gmail.com")];
        let sheet = build(&accounts, || {});
        let mut found = Vec::new();
        texts(&sheet.dialog.child().expect("the content"), &mut found);
        assert_eq!(found[0], "Choose how Penguin Mail signs in to Gmail");
        assert_eq!(found[2..4], ["r&d@example.com", "b@gmail.com"]);
        assert_eq!(sheet.later.label().as_deref(), Some("Later"));
        assert_eq!(sheet.choose.label().as_deref(), Some("Choose Now"));
    }

    fn each_address_row_has_a_name() {
        let accounts = [gmail(1, "r&d@example.com"), gmail(2, "b@gmail.com")];
        let sheet = build(&accounts, || {});
        let mut rows = Vec::new();
        list_rows(&sheet.dialog.child().expect("the content"), &mut rows);
        assert_eq!(rows.len(), 2);
        for row in rows {
            assert!(gtk::test_accessible_has_property(
                &row,
                gtk::AccessibleProperty::Label
            ));
        }
    }

    fn list_rows(widget: &gtk::Widget, found: &mut Vec<gtk::ListBoxRow>) {
        if let Some(row) = widget.downcast_ref::<gtk::ListBoxRow>() {
            found.push(row.clone());
        }
        let mut child = widget.first_child();
        while let Some(w) = child {
            list_rows(&w, found);
            child = w.next_sibling();
        }
    }

    fn choose_now_is_the_default_and_the_dialog_is_440_wide() {
        let sheet = build(&[gmail(1, "a@gmail.com")], || {});
        assert_eq!(
            sheet.dialog.default_widget().as_ref(),
            Some(sheet.choose.upcast_ref())
        );
        assert!(sheet.choose.has_css_class("suggested-action"));
        assert!(!sheet.later.has_css_class("suggested-action"));
        assert_eq!(sheet.dialog.content_width(), 440);
    }

    /// Presents `sheet` over a window and returns how many times it closed.
    fn closed_count(sheet: &Sheet) -> (gtk::Window, Rc<Cell<u32>>) {
        let window = gtk::Window::new();
        window.present();
        let closed = Rc::new(Cell::new(0));
        let seen = Rc::clone(&closed);
        sheet
            .dialog
            .connect_closed(move |_| seen.set(seen.get() + 1));
        sheet.dialog.present(Some(&window));
        (window, closed)
    }

    fn choose_now_walks_and_later_and_escape_do_not() {
        let walks = Rc::new(Cell::new(0));
        let counted = Rc::clone(&walks);
        let sheet = build(&[gmail(1, "a@gmail.com")], move || {
            counted.set(counted.get() + 1)
        });
        let (_window, closed) = closed_count(&sheet);
        sheet.later.emit_clicked();
        assert_eq!((walks.get(), closed.get()), (0, 1));

        let counted = Rc::clone(&walks);
        let sheet = build(&[gmail(1, "a@gmail.com")], move || {
            counted.set(counted.get() + 1)
        });
        let (_window, closed) = closed_count(&sheet);
        // Escape closes an adw::Dialog through `close`.
        sheet.dialog.close();
        assert_eq!((walks.get(), closed.get()), (0, 1));

        let counted = Rc::clone(&walks);
        let sheet = build(&[gmail(1, "a@gmail.com")], move || {
            counted.set(counted.get() + 1)
        });
        let (_window, closed) = closed_count(&sheet);
        sheet.choose.emit_clicked();
        assert_eq!((walks.get(), closed.get()), (1, 1));
    }
}
