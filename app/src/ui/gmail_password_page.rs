//! The page that signs in to Gmail with an app password: three steps that
//! walk a person through making one on Google's own page, the address and
//! the password, and what to try when Google says no.

use adw::prelude::*;

use super::add_account::{banded, boxed_list, heading, label};
use crate::gmail_password::{self, Action, GmailFailure};
use mailrs_domain::translate::gettext;

pub use crate::gmail_password::TAG;

/// The page and the widgets the dialog reads and listens to.
pub struct GmailPasswordPage {
    pub page: adw::NavigationPage,
    pub address: adw::EntryRow,
    pub password: adw::PasswordEntryRow,
    pub note: gtk::Label,
    pub again_line: gtk::Label,
    pub sign_in: gtk::Button,
    pub spinner: adw::Spinner,
    pub help: adw::ExpanderRow,
    pub other_way: gtk::Button,
    /// The help row that holds `other_way`, which an account signing in
    /// again does not get.
    other_row: gtk::ListBoxRow,
    pub error: gtk::Label,
    pub error_buttons: gtk::Box,
}

impl GmailPasswordPage {
    pub fn new() -> GmailPasswordPage {
        // Translators: App password is the name of Google's page; use the words Google's own
        // page shows in your language.
        let title_text = gettext("Sign in with an app password");
        let banded = banded(TAG, &title_text);
        banded.body.add_css_class("post-found");
        banded.body.add_css_class("app-password");
        let title = heading(&title_text);
        title.set_xalign(0.5);
        title.set_justify(gtk::Justification::Center);
        title.add_css_class("post-route-title");
        let intro = label(
            // Translators: Google is a name; keep it as it is.
            &gettext(
                "Google gives each app its own password. You make one in your Google account and paste it here. Your Google password stays with Google.",
            ),
            &["post-lede", "post-consent-text"],
        );
        intro.set_xalign(0.5);
        intro.set_justify(gtk::Justification::Center);

        let steps = boxed_list();
        steps.add_css_class("post-steps-list");
        steps.append(&step(
            1,
            // Translators: 2-Step Verification is Google's name for it; use the words Google's
            // own page shows in your language.
            &gettext("Turn on 2-Step Verification, if it isn't on yet."),
            Some((
                // Translators: 2-Step Verification is Google's name for it; use the words Google's
                // own page shows in your language.
                gettext("Open 2-Step Verification"),
                gmail_password::TWO_STEP_URL,
            )),
        ));
        steps.append(&step(
            2,
            // Translators: App password and Penguin Mail are names; use the words Google's own
            // page shows in your language for App passwords.
            &gettext("Make an app password. Name it Penguin Mail."),
            Some((
                // Translators: App passwords is the name of Google's page; use the words Google's
                // own page shows in your language.
                gettext("Open App passwords"),
                gmail_password::APP_PASSWORDS_URL,
            )),
        ));
        steps.append(&step(
            3,
            // Translators: Google is a name; keep it as it is.
            &gettext(
                "Copy the 16 letters Google shows and paste them below. Google shows them only once.",
            ),
            None,
        ));

        let again_line = label(
            // Translators: Google is a name; keep it as it is. App passwords is the name of
            // Google's page; use the words Google's own page shows in your language.
            &gettext(
                "Changing your Google password removes your app passwords. Make a new one to keep getting mail.",
            ),
            &["post-lede-small", "app-password-again"],
        );
        again_line.set_visible(false);

        let address = adw::EntryRow::builder()
            .title(gettext("Email Address"))
            .input_purpose(gtk::InputPurpose::Email)
            .build();
        crate::ui::name(&address, &gettext("Email Address"));
        // Translators: App password is the name of Google's page; use the words Google's own
        // page shows in your language.
        let password_title = gettext("App password");
        let password = adw::PasswordEntryRow::builder()
            .title(&password_title)
            .build();
        crate::ui::name(&password, &password_title);
        let fields = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list", "post-field"])
            .build();
        fields.append(&address);
        fields.append(&password);

        let note = label("", &["post-lede-small"]);
        note.set_visible(false);
        let error = label("", &["post-lede-small", "error"]);
        error.set_visible(false);
        let error_buttons = gtk::Box::builder()
            .spacing(8)
            .halign(gtk::Align::Center)
            .visible(false)
            .build();

        let sign_in = gtk::Button::builder()
            .label(gettext("Sign In"))
            .sensitive(false)
            .halign(gtk::Align::Center)
            .css_classes(["pill", "suggested-action", "post-wide"])
            .build();
        crate::ui::name(&sign_in, &gettext("Sign In"));
        let spinner = adw::Spinner::builder().visible(false).build();
        let action = gtk::Box::builder()
            .spacing(10)
            .halign(gtk::Align::Center)
            .build();
        action.append(&spinner);
        action.append(&sign_in);

        let help = adw::ExpanderRow::builder()
            // Translators: App passwords is the name of Google's page; use the words Google's own
            // page shows in your language.
            .title(gettext(
                "App passwords page says the setting isn't available?",
            ))
            .title_lines(0)
            .build();
        for line in [
            // Translators: 2-Step Verification is Google's name for it; use the words Google's
            // own page shows in your language.
            gettext("2-Step Verification is off for this account."),
            // Translators: Advanced Protection is Google's name for it; use the words Google's
            // own page shows in your language.
            gettext("The account uses Advanced Protection."),
            // Translators: App passwords is the name of Google's page; use the words Google's own
            // page shows in your language.
            gettext("A work or school admin turned app passwords off."),
        ] {
            let row = adw::ActionRow::builder()
                .title(&line)
                .title_lines(0)
                .build();
            row.add_prefix(&gtk::Label::new(Some("•")));
            help.add_row(&row);
        }
        let other_way = gtk::Button::builder()
            .label(gettext("Use another way to sign in"))
            .halign(gtk::Align::Start)
            .css_classes(["pill", "post-small-pill"])
            .margin_top(6)
            .margin_bottom(12)
            .margin_start(12)
            .build();
        crate::ui::name(&other_way, &gettext("Use another way to sign in"));
        let other_row = gtk::ListBoxRow::builder()
            .activatable(false)
            .selectable(false)
            .child(&other_way)
            .build();
        crate::ui::name(&other_row, &gettext("Use another way to sign in"));
        help.add_row(&other_row);
        let help_list = boxed_list();
        help_list.append(&help);

        let body = &banded.body;
        body.append(&title);
        body.append(&intro);
        body.append(&again_line);
        body.append(&steps);
        body.append(&fields);
        body.append(&note);
        body.append(&error);
        body.append(&error_buttons);
        body.append(&help_list);
        // Sign In and its spinner stay in view under the scroller.
        banded.bottom_bar(&action).add_css_class("app-password");

        // A page pushed on top of a focused button leaves the focus nowhere
        // in the view, so the entry that still needs words takes it.
        let (address_entry, password_entry) = (address.downgrade(), password.downgrade());
        banded.page.connect_shown(move |_| {
            let filled = address_entry
                .upgrade()
                .is_some_and(|a| !a.text().trim().is_empty());
            let focus: Option<gtk::Widget> = match filled {
                true => password_entry.upgrade().map(|p| p.upcast()),
                false => address_entry.upgrade().map(|a| a.upcast()),
            };
            if let Some(widget) = focus {
                widget.grab_focus();
            }
        });
        GmailPasswordPage {
            page: banded.page,
            address,
            password,
            note,
            again_line,
            sign_in,
            spinner,
            help,
            other_way,
            other_row,
            error,
            error_buttons,
        }
    }

    /// Shows a page for a new sign-in, or one again for `address`, which
    /// cannot change.
    pub fn open(&self, address: Option<&str>, again: bool) {
        self.address.set_text(address.unwrap_or_default());
        self.address.set_editable(!again);
        match again {
            true => self.address.add_css_class("app-password-locked"),
            false => self.address.remove_css_class("app-password-locked"),
        }
        self.again_line.set_visible(again);
        self.other_row.set_visible(!again);
        self.password.set_text("");
        self.clear_failure();
        self.note.set_visible(false);
    }

    /// The note under the entry, for what is pasted now.
    pub fn show_note(&self) {
        let note = gmail_password::note(&self.password.text());
        self.note.set_label(note.as_deref().unwrap_or_default());
        self.note.set_visible(note.is_some());
    }

    /// Whether Sign In can run now.
    pub fn update_sign_in(&self, running: bool) {
        let ready =
            gmail_password::can_sign_in(&self.address.text(), &self.password.text(), running);
        self.sign_in.set_sensitive(ready);
        self.spinner.set_visible(running);
        let words = match running {
            true => gettext("Signing In…"),
            false => gettext("Sign In"),
        };
        self.sign_in.set_label(&words);
    }

    /// Says why the sign-in failed, with the buttons that help, each
    /// handed to `pressed`. An account that signs in `again` is offered no
    /// other way to sign in.
    pub fn show_failure(
        &self,
        failure: &GmailFailure,
        again: bool,
        pressed: impl Fn(Action) + Clone + 'static,
    ) {
        self.clear_failure();
        self.error.set_label(&failure.words());
        self.error.set_visible(true);
        for action in failure
            .actions()
            .into_iter()
            .filter(|action| !(again && *action == Action::SignInAnotherWay))
        {
            let button = gtk::Button::builder()
                .label(action.label())
                .css_classes(["pill", "post-small-pill"])
                .build();
            crate::ui::name(&button, &action.label());
            let pressed = pressed.clone();
            button.connect_clicked(move |_| pressed(action));
            self.error_buttons.append(&button);
        }
        self.error_buttons
            .set_visible(self.error_buttons.first_child().is_some());
        // The entry takes the focus again, so a new paste is one key away.
        self.password.grab_focus();
    }

    pub fn clear_failure(&self) {
        self.error.set_visible(false);
        self.error.set_label("");
        while let Some(child) = self.error_buttons.first_child() {
            self.error_buttons.remove(&child);
        }
        self.error_buttons.set_visible(false);
    }
}

/// One numbered step, with the link under its words when it has one.
fn step(number: u32, text: &str, link: Option<(String, &str)>) -> gtk::ListBoxRow {
    let badge = gtk::Label::builder()
        .label(number.to_string())
        .valign(gtk::Align::Start)
        .halign(gtk::Align::Center)
        .accessible_role(gtk::AccessibleRole::Presentation)
        .css_classes(["post-step-number"])
        .build();
    let words = label(text, &["post-step-text"]);
    words.set_hexpand(true);
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .hexpand(true)
        .build();
    column.append(&words);
    if let Some((label_text, url)) = link {
        // The words come first and the arrow after them, as the mockup has it.
        let content = crate::ui::external_link_content(&label_text, 5);
        let button = gtk::LinkButton::builder()
            .uri(url)
            .halign(gtk::Align::Start)
            .css_classes(["app-password-link"])
            .build();
        button.set_child(Some(&content));
        crate::ui::name(&button, &label_text);
        column.append(&button);
    }
    let row = gtk::Box::builder()
        .spacing(12)
        .margin_top(8)
        .margin_bottom(6)
        .margin_start(12)
        .margin_end(12)
        .build();
    row.append(&badge);
    row.append(&column);
    let item = gtk::ListBoxRow::builder()
        .activatable(false)
        .selectable(false)
        .child(&row)
        .build();
    crate::ui::name(&item, text);
    item
}

#[cfg(test)]
pub(crate) mod checks {
    use super::*;

    pub fn run() {
        the_page_names_its_links_and_fields();
        sign_in_stays_named_while_insensitive();
        the_help_opens_and_names_its_state();
    }

    fn names(widget: &gtk::Widget, found: &mut Vec<(String, String)>) {
        let mut child = widget.first_child();
        while let Some(w) = child {
            if let Some(link) = w.downcast_ref::<gtk::LinkButton>() {
                found.push((link.uri().to_string(), w.accessible_label_text()));
            }
            names(&w, found);
            child = w.next_sibling();
        }
    }

    trait Spoken {
        fn accessible_label_text(&self) -> String;
    }

    impl Spoken for gtk::Widget {
        fn accessible_label_text(&self) -> String {
            self.downcast_ref::<gtk::LinkButton>()
                .and_then(|l| l.child())
                .and_then(|c| c.first_child())
                .and_downcast::<gtk::Label>()
                .map(|l| l.label().to_string())
                .unwrap_or_default()
        }
    }

    fn the_page_names_its_links_and_fields() {
        let page = GmailPasswordPage::new();
        let mut links = Vec::new();
        names(page.page.upcast_ref(), &mut links);
        assert_eq!(
            links,
            [
                (
                    gmail_password::TWO_STEP_URL.to_string(),
                    "Open 2-Step Verification".to_string()
                ),
                (
                    gmail_password::APP_PASSWORDS_URL.to_string(),
                    "Open App passwords".to_string()
                ),
            ]
        );
        assert_eq!(page.address.title(), "Email Address");
        assert_eq!(page.password.title(), "App password");
    }

    fn sign_in_stays_named_while_insensitive() {
        let page = GmailPasswordPage::new();
        page.address.set_text("me@gmail.com");
        page.password.set_text("abcdefghijklmnop");
        page.update_sign_in(false);
        assert!(page.sign_in.is_sensitive());
        page.update_sign_in(true);
        assert!(!page.sign_in.is_sensitive());
        assert!(page.spinner.is_visible());
        assert_eq!(page.sign_in.label().as_deref(), Some("Signing In…"));
        page.update_sign_in(false);
        assert_eq!(page.sign_in.label().as_deref(), Some("Sign In"));
        assert!(!page.spinner.is_visible());
    }

    fn the_help_opens_and_names_its_state() {
        let page = GmailPasswordPage::new();
        assert!(!page.help.is_expanded());
        page.help.set_expanded(true);
        assert!(page.help.is_expanded());
        assert_eq!(
            page.help.title(),
            "App passwords page says the setting isn't available?"
        );
        assert_eq!(
            page.other_way.label().as_deref(),
            Some("Use another way to sign in")
        );
    }
}
