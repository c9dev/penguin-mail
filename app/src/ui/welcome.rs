//! The first-run page: adding the first account.

use adw::prelude::*;
use mailrs_domain::translate::gettext;

/// Offers to add the first account. Returns the page and its button, which
/// the window disables while the browser flow runs.
pub fn first_account_page(on_add: impl Fn() + 'static) -> (gtk::Widget, gtk::Button) {
    let add = gtk::Button::builder()
        .label(gettext("Sign In with Google"))
        .css_classes(["pill", "suggested-action"])
        .halign(gtk::Align::Center)
        .build();
    add.connect_clicked(move |_| on_add());
    let page = adw::StatusPage::builder()
        .icon_name("io.github.c9dev.PenguinMail")
        .title(gettext("Add Your First Account"))
        .description(gettext(
            "Your browser opens Google's sign-in page. Until Google finishes \
             checking Penguin Mail, it warns that it has not verified the app: \
             choose Advanced, then continue.",
        ))
        .child(&add)
        .vexpand(true)
        .build();
    (wrap(&page), add)
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
