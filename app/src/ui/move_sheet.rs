//! The sheet that shows once, at the first start after the update, over
//! the window: it lists the Gmail accounts that wait to choose how they
//! sign in.

use adw::prelude::*;
use mailrs_domain::Account;
use mailrs_domain::translate::gettext;

/// The sheet's two answers. Closing the dialog or pressing Escape means
/// "later".
const LATER: &str = "later";
const CHOOSE: &str = "choose";

/// The sheet for `accounts`. `choose` runs when the person picks
/// "Choose Now".
pub fn build(accounts: &[Account], choose: impl Fn() + 'static) -> adw::AlertDialog {
    // Translators: Gmail is a name; keep it as it is.
    let heading = gettext("Choose how Penguin Mail signs in to Gmail");
    // Translators: Google and Gmail are names; keep them as they are.
    let body = gettext(
        "Penguin Mail has new ways to sign in to Google. Pick one for each Gmail account below to keep your mail arriving. Your mail and settings stay as they are, and nothing downloads again.",
    );
    let dialog = adw::AlertDialog::new(Some(&heading), Some(&body));
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list", "move-sheet-list"])
        .build();
    for account in accounts {
        let dot = gtk::Box::builder()
            .valign(gtk::Align::Center)
            .css_classes(["move-sheet-dot"])
            .build();
        let address = gtk::Label::builder()
            .label(account.email.as_str())
            .use_markup(false)
            .xalign(0.0)
            .hexpand(true)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .css_classes(["move-sheet-address"])
            .build();
        let row = gtk::Box::builder()
            .spacing(10)
            .margin_top(11)
            .margin_bottom(11)
            .margin_start(14)
            .margin_end(14)
            .build();
        row.append(&dot);
        row.append(&address);
        list.append(
            &gtk::ListBoxRow::builder()
                .child(&row)
                .activatable(false)
                .build(),
        );
    }
    dialog.set_extra_child(Some(&list));
    dialog.add_responses(&[(LATER, &gettext("Later")), (CHOOSE, &gettext("Choose Now"))]);
    dialog.set_response_appearance(CHOOSE, adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some(CHOOSE));
    dialog.set_close_response(LATER);
    dialog.connect_response(None, move |_, response| {
        if response == CHOOSE {
            choose();
        }
    });
    dialog
}

/// Shows the sheet over `parent`.
pub fn present(parent: &impl IsA<gtk::Widget>, accounts: &[Account], choose: impl Fn() + 'static) {
    build(accounts, choose).present(Some(parent));
}

#[cfg(test)]
pub(crate) mod checks {
    use super::*;
    use mailrs_domain::{Account, AccountState, Provider};
    use std::cell::Cell;
    use std::rc::Rc;

    pub fn run() {
        the_sheet_lists_each_address_plainly();
        choose_now_is_the_default();
        choose_now_runs_the_walk_and_later_does_not();
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

    fn addresses(widget: &gtk::Widget, found: &mut Vec<String>) {
        if let Some(label) = widget.downcast_ref::<gtk::Label>() {
            assert!(!label.uses_markup());
            found.push(label.label().to_string());
        }
        let mut child = widget.first_child();
        while let Some(w) = child {
            addresses(&w, found);
            child = w.next_sibling();
        }
    }

    fn the_sheet_lists_each_address_plainly() {
        let accounts = [gmail(1, "r&d@example.com"), gmail(2, "b@gmail.com")];
        let dialog = build(&accounts, || {});
        let list = dialog.extra_child().expect("the list of addresses");
        let mut found = Vec::new();
        addresses(&list, &mut found);
        assert_eq!(found, ["r&d@example.com", "b@gmail.com"]);
        assert_eq!(
            dialog.heading().unwrap(),
            "Choose how Penguin Mail signs in to Gmail"
        );
    }

    fn choose_now_is_the_default() {
        let dialog = build(&[gmail(1, "a@gmail.com")], || {});
        assert_eq!(dialog.default_response().as_deref(), Some("choose"));
        assert_eq!(dialog.close_response(), "later");
        assert_eq!(
            dialog.response_appearance("choose"),
            adw::ResponseAppearance::Suggested
        );
        assert_eq!(dialog.response_label("choose"), "Choose Now");
        assert_eq!(dialog.response_label("later"), "Later");
    }

    fn choose_now_runs_the_walk_and_later_does_not() {
        let walks = Rc::new(Cell::new(0));
        let counted = Rc::clone(&walks);
        let dialog = build(&[gmail(1, "a@gmail.com")], move || {
            counted.set(counted.get() + 1)
        });
        dialog.emit_by_name::<()>("response", &[&"later"]);
        assert_eq!(walks.get(), 0);
        dialog.emit_by_name::<()>("response", &[&"choose"]);
        assert_eq!(walks.get(), 1);
    }
}
