//! Asking for a Google permission on screen. `crate::permission` holds the
//! words and the rule for how often to ask; this puts them in a dialog, or
//! on the page a dialog of its own shows in place of its content.

use std::cell::RefCell;

use adw::prelude::*;
use mailrs_domain::AccountId;
use mailrs_domain::translate::gettext;

use crate::permission::{Asked, Occasion, Permission};
use crate::ui::confirm::{Tone, confirm};

thread_local! {
    /// The offers this run has made. It outlives any one window, so
    /// closing to the tray and opening again does not repeat an offer.
    static ASKED: RefCell<Asked> = RefCell::new(Asked::default());
}

/// Asks `account` for `permission` over `parent`. True when the person
/// chose Grant Access; false when they said no, or when this occasion's
/// offer has already been made this run.
pub async fn ask(
    parent: &impl IsA<gtk::Widget>,
    account_id: AccountId,
    account: &str,
    permission: Permission,
    occasion: Occasion,
) -> bool {
    if !ASKED.with(|asked| {
        asked
            .borrow_mut()
            .should_ask(account_id, permission, occasion)
    }) {
        return false;
    }
    let words = permission.wording(occasion, account);
    confirm(
        &words.heading,
        &words.body,
        &gettext("Grant Access"),
        Tone::Suggested,
    )
    .not_now()
    .ask(parent)
    .await
}

/// The page a settings dialog shows when Gmail wants `permission` first:
/// `title`, the permission's words for `account`, and a Grant Access
/// button that runs `grant`.
pub fn page(
    title: &str,
    permission: Permission,
    account: &str,
    grant: impl Fn() + 'static,
) -> adw::StatusPage {
    let page = adw::StatusPage::builder()
        .icon_name("mail-send-symbolic")
        .title(title)
        .description(permission.wording(Occasion::Needed, account).body)
        .build();
    let button = gtk::Button::builder()
        .label(gettext("Grant Access"))
        .halign(gtk::Align::Center)
        .css_classes(["pill", "suggested-action"])
        .build();
    button.connect_clicked(move |_| grant());
    page.set_child(Some(&button));
    page
}
