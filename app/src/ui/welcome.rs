//! The first-run page: adding the first account.

use adw::prelude::*;
use mailrs_domain::translate::gettext;

/// Offers to add the first account, from Google or from another provider.
/// Returns the page and the Google button, which the window disables
/// while the browser flow runs.
pub fn first_account_page(
    on_google: impl Fn() + 'static,
    on_other: impl Fn() + 'static,
) -> (gtk::Widget, gtk::Button) {
    let google = gtk::Button::builder()
        .label(gettext("Sign In with Google"))
        .css_classes(["pill", "suggested-action"])
        .build();
    google.connect_clicked(move |_| on_google());
    let other = gtk::Button::builder()
        .label(gettext("Use Another Provider"))
        .css_classes(["pill"])
        .build();
    other.connect_clicked(move |_| on_other());
    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .halign(gtk::Align::Center)
        .homogeneous(true)
        .build();
    buttons.append(&google);
    buttons.append(&other);
    let page = adw::StatusPage::builder()
        .icon_name("io.github.c9dev.PenguinMail")
        .title(gettext("Add Your First Account"))
        .description(gettext(
            "Your browser opens Google's sign-in page. Until Google finishes \
             checking Penguin Mail, it warns that it has not verified the app: \
             choose Advanced, then continue.",
        ))
        .child(&buttons)
        .vexpand(true)
        .build();
    (wrap(&page), google)
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
