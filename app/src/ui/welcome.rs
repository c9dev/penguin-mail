//! First-run pages: OAuth client setup, then the first account.

use adw::prelude::*;
use mailrs_domain::translate::gettext;

/// The steps for making a Google OAuth client, for the Help dialog. The
/// console's own names stay as Google writes them, since that is what the
/// reader has to find on screen.
fn how_to() -> String {
    gettext(
        "1. At console.cloud.google.com, create a project and enable the Gmail API.\n\n\
         2. Under Google Auth Platform, choose External, add the scope gmail.modify, \
         and click Publish app. Leaving it in Testing signs you out every 7 days.\n\n\
         3. Create an OAuth client of type Desktop app and paste its ID and secret \
         here.\n\n\
         The full walkthrough is in docs/setup.md.",
    )
}

/// Asks for the OAuth client ID and secret.
pub fn setup_page(on_save: impl Fn(String, String) + 'static) -> gtk::Widget {
    let id = adw::EntryRow::builder().title(gettext("Client ID")).build();
    let secret = adw::PasswordEntryRow::builder()
        .title(gettext("Client Secret"))
        .build();
    let group = adw::PreferencesGroup::new();
    group.add(&id);
    group.add(&secret);
    let save = gtk::Button::builder()
        .label(gettext("Continue"))
        .css_classes(["pill", "suggested-action"])
        .halign(gtk::Align::Center)
        .sensitive(false)
        .build();
    let help = gtk::Button::builder()
        .label(gettext("How do I get these?"))
        .css_classes(["flat"])
        .halign(gtk::Align::Center)
        .build();
    let valid = {
        let (id, secret, save) = (id.clone(), secret.clone(), save.clone());
        move || {
            save.set_sensitive(
                id.text().trim().ends_with(".apps.googleusercontent.com")
                    && !secret.text().trim().is_empty(),
            )
        }
    };
    let check = valid.clone();
    id.connect_changed(move |_| check());
    secret.connect_changed(move |_| valid());
    {
        let (id, secret) = (id.clone(), secret.clone());
        save.connect_clicked(move |_| {
            on_save(
                id.text().trim().to_string(),
                secret.text().trim().to_string(),
            )
        });
    }
    help.connect_clicked(|button| {
        let dialog =
            adw::AlertDialog::new(Some(&gettext("Create Your OAuth Client")), Some(&how_to()));
        dialog.add_response("ok", &gettext("Got It"));
        dialog.present(Some(button));
    });
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .css_classes(["welcome-card"])
        .build();
    column.append(&group);
    column.append(&save);
    column.append(&help);
    let page = adw::StatusPage::builder()
        .icon_name("io.github.c9dev.PenguinMail")
        .title(gettext("Welcome to Penguin Mail"))
        .description(gettext(
            "Penguin Mail reads Gmail through your own Google Cloud OAuth client, so \
             nobody else can reach your mail. Paste the client's ID and secret to begin.",
        ))
        .child(
            &adw::Clamp::builder()
                .maximum_size(440)
                .child(&column)
                .build(),
        )
        .vexpand(true)
        .build();
    wrap(&page)
}

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
            "Your browser opens Google's sign-in page. Google warns that it hasn't \
             verified Penguin Mail, because the app is yours alone: choose Advanced, \
             then continue.",
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
