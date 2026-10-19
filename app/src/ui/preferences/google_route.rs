//! What a Gmail account's Preferences page says about its sign-in: the
//! features it has, the move it still has to make, the things only
//! gmail.com does for it, and a Connect row for a calendar it lacks.

use std::rc::Rc;

use adw::prelude::*;
use mailrs_domain::translate::{fill, gettext};
use mailrs_domain::{Account, AccountId, MailRoute, Provider};
use mailrs_store::accounts::GoogleSignIn;
use mailrs_sync::Offers;

use crate::app::App;
use crate::move_banner::{Waiting, banner};
use crate::offered::{self, calendar_link};
use crate::route_choice::Platform;

/// The window action that opens the route choice for one account, by id.
pub const CHOOSE_ROUTE: &str = "win.account-choose-route";

/// One line of a Gmail account's page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    /// What the account's sign-in gives: every Gmail feature, or mail,
    /// calendar and contacts only.
    Features { every: bool },
    /// The account still has to choose how it signs in: the banner's words
    /// for one account.
    Move { title: String },
    /// Something the account does on gmail.com only, with the link.
    Gap {
        text: String,
        link: &'static str,
        label: String,
    },
}

/// The lines under `account`'s address. `platform` is for the GNOME Online
/// Accounts plan, whose route changes what the Move line offers.
pub fn lines(sign_in: GoogleSignIn, _platform: Platform, account: &Account) -> Vec<Line> {
    let mut lines = Vec::new();
    match sign_in.route {
        MailRoute::GmailApi => lines.push(Line::Features { every: true }),
        MailRoute::Imap => {
            lines.push(Line::Features { every: false });
            lines.push(Line::Gap {
                text: offered::filters_gap(),
                link: offered::FILTERS_LINK,
                label: offered::filters_link_label(),
            });
            lines.push(Line::Gap {
                text: offered::reply_gap(),
                link: offered::SETTINGS_LINK,
                label: offered::settings_link_label(),
            });
        }
    }
    if sign_in.needs_move(account.provider) {
        let waiting = Waiting {
            account: account.clone(),
            paused: account.state == mailrs_domain::AccountState::MailPaused,
        };
        if let Some(banner) = banner(&[waiting], |_| false) {
            lines.push(Line::Move {
                title: banner.title,
            });
        }
    }
    lines
}

/// Adds `account`'s Gmail rows to its expander row: the features, the
/// move, the calendar and contacts that are not connected, and what only
/// gmail.com does. `connect` opens the calendar consent, or the route
/// choice for an account that has to move first.
pub fn rows(
    app: &Rc<App>,
    account: &Account,
    offers: Offers,
    parent: &adw::ExpanderRow,
    connect: &Rc<dyn Fn(AccountId)>,
) {
    if account.provider != Provider::Gmail {
        return;
    }
    if let Some(sign_in) = app.google_sign_in(account.id) {
        fill_rows(parent, account, sign_in, offers, connect);
    }
}

/// [`rows`] for a sign-in already read, in the order the mockup draws:
/// the features and the move, then calendar and contacts, then what only
/// gmail.com does.
fn fill_rows(
    parent: &adw::ExpanderRow,
    account: &Account,
    sign_in: GoogleSignIn,
    offers: Offers,
    connect: &Rc<dyn Fn(AccountId)>,
) {
    let (gaps, own): (Vec<Line>, Vec<Line>) = lines(sign_in, Platform::this(), account)
        .into_iter()
        .partition(|line| matches!(line, Line::Gap { .. }));
    for line in own {
        match line {
            Line::Features { every } => {
                // Translators: Gmail is a name; keep it as it is.
                let row = plain_row(&gettext("Gmail features"), None);
                let value = if every {
                    gettext("Every feature")
                } else {
                    gettext("Mail, calendar and contacts")
                };
                row.add_suffix(&dim(&value));
                parent.add_row(&row);
                // The button sits in a row of its own under the line, as
                // the mockup draws it: an expander's row has no room for
                // both the value and a button in its suffix.
                if !every {
                    let holder = plain_row("", None);
                    holder.add_suffix(&route_button(&gettext("Get Every Feature"), account.id));
                    parent.add_row(&holder);
                }
            }
            Line::Move { title } => {
                let row = plain_row(&title, None);
                // The GNOME Online Accounts plan replaces this with its
                // one-click move for an address that GNOME already holds.
                row.add_suffix(&route_button(&gettext("Choose…"), account.id));
                parent.add_row(&row);
            }
            Line::Gap { .. } => {}
        }
    }
    if calendar_link(account, offers).shows_connect() {
        for (title, name) in [
            (
                gettext("Calendar"),
                gettext("Connect the calendar for {account}"),
            ),
            (
                gettext("Contacts"),
                gettext("Connect the contacts for {account}"),
            ),
        ] {
            let row = plain_row(&title, Some(&gettext("Not connected")));
            row.add_suffix(&connect_button(
                &fill(&name, &[("account", &account.email)]),
                account.id,
                connect,
            ));
            parent.add_row(&row);
        }
    }
    for gap in gaps {
        if let Line::Gap { text, link, label } = gap {
            parent.add_row(&gap_row(&text, link, &label));
        }
    }
}

/// A sentence with its link under it, the link bold and followed by the
/// mark that says it leaves the app.
fn gap_row(text: &str, link: &str, label: &str) -> adw::PreferencesRow {
    let words = gtk::Label::builder()
        .label(text)
        .wrap(true)
        .xalign(0.0)
        .build();
    let inside = gtk::Box::builder().spacing(6).build();
    inside.append(&gtk::Label::new(Some(label)));
    inside.append(&gtk::Image::from_icon_name("adw-external-link-symbolic"));
    let button = gtk::LinkButton::builder()
        .uri(link)
        .child(&inside)
        .halign(gtk::Align::Start)
        .build();
    // Not through the builder, which would drop the `link` class.
    button.add_css_class("gmail-link");
    crate::ui::name(&button, label);
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .margin_top(12)
        .margin_bottom(8)
        .margin_start(12)
        .margin_end(12)
        .build();
    column.append(&words);
    column.append(&button);
    adw::PreferencesRow::builder()
        .title(text)
        .use_markup(false)
        .activatable(false)
        .child(&column)
        .build()
}

/// A row of words, as plain text.
fn plain_row(title: &str, subtitle: Option<&str>) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .use_markup(false)
        .title(title)
        .build();
    if let Some(subtitle) = subtitle {
        row.set_subtitle(subtitle);
    }
    row
}

fn dim(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .valign(gtk::Align::Center)
        .css_classes(["dim-label"])
        .build()
}

/// A button that opens the route choice for `account`.
fn route_button(label: &str, account: AccountId) -> gtk::Button {
    let button = gtk::Button::builder()
        .label(label)
        .valign(gtk::Align::Center)
        .build();
    button.connect_clicked(move |button| {
        if let Err(err) = button.activate_action(CHOOSE_ROUTE, Some(&account.to_variant())) {
            tracing::warn!(account, %err, "the route choice did not open");
        }
    });
    button
}

fn connect_button(name: &str, account: AccountId, connect: &Rc<dyn Fn(AccountId)>) -> gtk::Button {
    let button = gtk::Button::builder()
        .label(gettext("Connect"))
        .valign(gtk::Align::Center)
        .build();
    crate::ui::name(&button, name);
    let connect = Rc::clone(connect);
    button.connect_clicked(move |_| connect(account));
    button
}

#[cfg(test)]
mod tests {
    use super::{Line, lines};
    use crate::route_choice::Platform;
    use mailrs_domain::{Account, AccountState, MailRoute, Provider, SignInClient};
    use mailrs_store::accounts::GoogleSignIn;

    fn account(state: AccountState) -> Account {
        Account {
            id: 3,
            email: "maria.costa@gmail.com".into(),
            state,
            provider: Provider::Gmail,
            provider_name: Some("Gmail".into()),
        }
    }

    fn sign_in(client: SignInClient, route: MailRoute) -> GoogleSignIn {
        GoogleSignIn { client, route }
    }

    #[test]
    fn an_api_account_has_every_feature() {
        let found = lines(
            sign_in(SignInClient::Own, MailRoute::GmailApi),
            Platform::Linux,
            &account(AccountState::Ok),
        );
        assert_eq!(found, [Line::Features { every: true }]);
    }

    #[test]
    fn an_imap_account_points_filters_and_the_reply_to_gmail() {
        let found = lines(
            sign_in(SignInClient::BuiltIn, MailRoute::Imap),
            Platform::Linux,
            &account(AccountState::Ok),
        );
        assert_eq!(found[0], Line::Features { every: false });
        assert!(found.contains(&Line::Gap {
            text: "Your Gmail filters still run. Change them on gmail.com.".into(),
            link: "https://mail.google.com/mail/#settings/filters",
            label: "Open Gmail Filters".into(),
        }));
        assert!(found.contains(&Line::Gap {
            text: "Set your automatic reply on gmail.com.".into(),
            link: "https://mail.google.com/mail/#settings/general",
            label: "Open Gmail Settings".into(),
        }));
        assert_eq!(found.len(), 3);
    }

    #[test]
    fn a_built_in_api_account_repeats_the_banner() {
        let found = lines(
            sign_in(SignInClient::BuiltIn, MailRoute::GmailApi),
            Platform::Linux,
            &account(AccountState::Ok),
        );
        assert_eq!(
            found,
            [
                Line::Features { every: true },
                Line::Move {
                    title: "Choose how maria.costa@gmail.com signs in to keep your mail arriving."
                        .into()
                }
            ]
        );
    }

    #[test]
    fn a_paused_account_repeats_the_paused_wording() {
        let found = lines(
            sign_in(SignInClient::BuiltIn, MailRoute::GmailApi),
            Platform::Linux,
            &account(AccountState::MailPaused),
        );
        assert!(found.contains(&Line::Move {
            title: "Penguin Mail can't get new mail for maria.costa@gmail.com. Choose how it signs in to get your mail back. Nothing on this computer was lost.".into()
        }));
    }
}

#[cfg(test)]
pub(crate) mod checks {
    use std::cell::RefCell;

    use super::*;
    use mailrs_domain::{AccountState, SignInClient};

    pub fn run() {
        an_imap_account_reads_in_the_mockups_order();
        connect_asks_for_the_account_and_a_paused_one_gets_it_too();
        an_api_account_shows_its_features_and_nothing_to_connect();
    }

    fn account(state: AccountState) -> Account {
        Account {
            id: 3,
            email: "maria.costa@gmail.com".into(),
            state,
            provider: Provider::Gmail,
            provider_name: Some("Gmail".into()),
        }
    }

    fn rows_of(widget: &gtk::Widget, found: &mut Vec<adw::PreferencesRow>) {
        let mut child = widget.first_child();
        while let Some(w) = child {
            if let Ok(row) = w.clone().downcast::<adw::PreferencesRow>() {
                found.push(row);
            }
            rows_of(&w, found);
            child = w.next_sibling();
        }
    }

    fn buttons_of(widget: &gtk::Widget, found: &mut Vec<gtk::Button>) {
        let mut child = widget.first_child();
        while let Some(w) = child {
            if let Ok(button) = w.clone().downcast::<gtk::Button>() {
                found.push(button);
            }
            buttons_of(&w, found);
            child = w.next_sibling();
        }
    }

    /// The expander row `fill_rows` filled, and the ids Connect was pressed for.
    fn filled(
        state: AccountState,
        sign_in: GoogleSignIn,
        offers: Offers,
    ) -> (adw::ExpanderRow, Rc<RefCell<Vec<AccountId>>>) {
        let pressed = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&pressed);
        let connect: Rc<dyn Fn(AccountId)> = Rc::new(move |id| sink.borrow_mut().push(id));
        let parent = adw::ExpanderRow::new();
        fill_rows(&parent, &account(state), sign_in, offers, &connect);
        (parent, pressed)
    }

    fn titles(parent: &adw::ExpanderRow) -> Vec<String> {
        let mut rows = Vec::new();
        rows_of(parent.upcast_ref(), &mut rows);
        // The first is the expander's own header row.
        rows.iter()
            .skip(1)
            .map(|row| row.title().to_string())
            .collect()
    }

    fn imap() -> GoogleSignIn {
        GoogleSignIn {
            client: SignInClient::BuiltIn,
            route: MailRoute::Imap,
        }
    }

    fn no_calendar() -> Offers {
        Offers {
            calendar: false,
            contacts: false,
            ..Offers::EVERYTHING
        }
    }

    fn an_imap_account_reads_in_the_mockups_order() {
        let (parent, _) = filled(AccountState::Ok, imap(), no_calendar());
        let found = titles(&parent);
        assert_eq!(
            found,
            [
                "Gmail features",
                "",
                "Calendar",
                "Contacts",
                "Your Gmail filters still run. Change them on gmail.com.",
                "Set your automatic reply on gmail.com.",
            ]
        );
        let mut rows = Vec::new();
        rows_of(parent.upcast_ref(), &mut rows);
        let subtitles: Vec<String> = rows
            .iter()
            .filter_map(|row| row.downcast_ref::<adw::ActionRow>())
            .filter_map(|row| row.subtitle())
            .filter(|subtitle| !subtitle.is_empty())
            .map(|subtitle| subtitle.to_string())
            .collect();
        assert_eq!(subtitles, ["Not connected", "Not connected"]);
        let mut buttons = Vec::new();
        buttons_of(parent.upcast_ref(), &mut buttons);
        let labels: Vec<String> = buttons
            .iter()
            .filter_map(|b| b.label())
            .map(|l| l.to_string())
            .collect();
        assert!(
            labels.contains(&"Get Every Feature".to_string()),
            "{labels:?}"
        );
    }

    fn connect_asks_for_the_account_and_a_paused_one_gets_it_too() {
        let (parent, pressed) = filled(AccountState::Ok, imap(), no_calendar());
        let mut buttons = Vec::new();
        buttons_of(parent.upcast_ref(), &mut buttons);
        for button in buttons
            .iter()
            .filter(|b| b.label().as_deref() == Some("Connect"))
        {
            button.emit_clicked();
        }
        assert_eq!(*pressed.borrow(), [3, 3]);
        // A paused account is not running, so it offers everything; its
        // state is what shows Connect, and the move line repeats the banner.
        let built_in_api = GoogleSignIn {
            client: SignInClient::BuiltIn,
            route: MailRoute::GmailApi,
        };
        let (paused, _) = filled(AccountState::MailPaused, built_in_api, Offers::EVERYTHING);
        let found = titles(&paused);
        assert!(found.contains(&"Calendar".to_string()), "{found:?}");
        assert!(found.contains(&"Contacts".to_string()), "{found:?}");
        assert!(
            found
                .iter()
                .any(|t| t.starts_with("Penguin Mail can't get new mail for maria.costa@gmail.com")),
            "{found:?}"
        );
    }

    fn an_api_account_shows_its_features_and_nothing_to_connect() {
        let own = GoogleSignIn {
            client: SignInClient::Own,
            route: MailRoute::GmailApi,
        };
        let (parent, _) = filled(AccountState::Ok, own, Offers::EVERYTHING);
        assert_eq!(titles(&parent), ["Gmail features"]);
    }
}
