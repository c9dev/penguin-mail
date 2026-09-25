//! The first-run page: adding the first account.

use adw::prelude::*;
use mailrs_domain::translate::gettext;

/// The first-run page and the two buttons on it, which the window makes
/// insensitive while Google's browser flow runs.
pub struct FirstAccount {
    pub page: gtk::Widget,
    pub google: gtk::Button,
    pub other: gtk::Button,
}

/// Offers to add the first account, from Google or from another provider.
pub fn first_account_page(
    on_google: impl Fn() + 'static,
    on_other: impl Fn() + 'static,
) -> FirstAccount {
    let google = gtk::Button::builder()
        .label(gettext("Sign In with Google"))
        .css_classes(["pill", "suggested-action"])
        .build();
    google.connect_clicked(move |_| on_google());
    // The browser warning is about Google's sign-in alone, so it sits
    // under that button and not above both.
    let warning = gtk::Label::builder()
        .label(gettext(
            "Your browser opens Google's sign-in page. Until Google finishes \
             checking Penguin Mail, it warns that it has not verified the app: \
             choose Advanced, then continue.",
        ))
        .wrap(true)
        .justify(gtk::Justification::Center)
        .css_classes(["caption", "dim-label"])
        .build();
    let with_google = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .build();
    with_google.append(&google);
    with_google.append(&warning);
    let other = gtk::Button::builder()
        .label(gettext("Use Another Provider"))
        .css_classes(["pill"])
        .build();
    other.connect_clicked(move |_| on_other());
    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(24)
        .build();
    buttons.append(&with_google);
    buttons.append(&other);
    // The clamp keeps both buttons one width and wraps the warning to it,
    // where the warning's own width would stretch them across the page.
    let clamp = adw::Clamp::builder()
        .maximum_size(320)
        .tightening_threshold(320)
        .child(&buttons)
        .build();
    let page = adw::StatusPage::builder()
        .icon_name("io.github.c9dev.PenguinMail")
        .title(gettext("Add Your First Account"))
        .child(&clamp)
        .vexpand(true)
        .build();
    FirstAccount {
        page: wrap(&page),
        google,
        other,
    }
}

fn wrap(content: &impl IsA<gtk::Widget>) -> gtk::Widget {
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(
        &adw::HeaderBar::builder()
            .title_widget(&gtk::Label::new(None))
            .build(),
    );
    toolbar.set_content(Some(content));
    toolbar.upcast()
}
