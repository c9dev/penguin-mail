//! What a Gmail account's Preferences page says about its sign-in: the
//! features it has, the things only gmail.com does for it, and a Connect
//! row for a calendar it lacks.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use mailrs_domain::translate::{fill, gettext};
use mailrs_domain::{Account, AccountId, GmailSignIn, MailRoute, Provider, SignInClient};
use mailrs_sync::Offers;

use crate::app::App;
use crate::offered::{self, calendar_link};

/// One line of a Gmail account's page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    /// What the account's sign-in gives: every Gmail feature, or mail,
    /// calendar and contacts only.
    Features { every: bool },
    /// An account that reads its mail through this computer's sign-in has
    /// no filters, aliases or automatic reply, and says so.
    Limits { text: String },
    /// The account signs in through the person's own Google project.
    OwnProject,
    /// Something the account does on gmail.com only, with the link.
    Gap {
        text: String,
        link: &'static str,
        label: String,
    },
}

/// The lines under an account's address, from how it signs in.
pub fn lines(sign_in: &GmailSignIn) -> Vec<Line> {
    let mut lines = Vec::new();
    let on_goa = sign_in.on_online_accounts();
    match sign_in.route() {
        MailRoute::GmailApi => lines.push(Line::Features { every: true }),
        MailRoute::Imap => {
            lines.push(Line::Features { every: false });
            if on_goa {
                lines.push(Line::Limits {
                    text: gettext(
                        "Filters, aliases and the automatic reply need Gmail's own connection, which this computer's Google sign-in doesn't offer.",
                    ),
                });
            } else {
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
    }
    if sign_in.client() == SignInClient::Own {
        lines.push(Line::OwnProject);
    }
    lines
}

/// Points an "Open Google Cloud" link at the project of the account with
/// this address, once the keyring says which project that is.
pub type FindProject = Rc<dyn Fn(&str, &gtk::LinkButton)>;

/// How to build an account's Contacts control, and whether the account's
/// page already holds one.
pub struct Contacts {
    pub build: Rc<dyn Fn(Offers) -> gtk::Widget>,
    /// The page was built with the control in it. When it was not, the
    /// control joins the Gmail rows once the calendar connects.
    pub added: bool,
}

/// An account's Gmail rows, kept so they can be filled again in place.
struct RouteRows {
    parent: glib::WeakRef<adw::ExpanderRow>,
    rows: RefCell<Vec<glib::WeakRef<gtk::Widget>>>,
    sign_in: GmailSignIn,
    connect: Rc<dyn Fn(AccountId)>,
    contacts: Contacts,
    find_project: FindProject,
}

/// A Calendar page row and its Connect button.
type CalendarRow = (glib::WeakRef<adw::ActionRow>, glib::WeakRef<gtk::Button>);

thread_local! {
    /// The open Preferences' Gmail rows by account. Only one Preferences
    /// is open; a new one replaces each entry. The rows are held weakly.
    static ROUTE_ROWS: RefCell<HashMap<AccountId, Rc<RouteRows>>> = RefCell::default();
    /// The Calendar page's row for an account that is not connected, with
    /// its Connect button.
    static CALENDAR_ROWS: RefCell<HashMap<AccountId, CalendarRow>> = RefCell::default();
}

/// Adds `account`'s Gmail rows to its expander row: the features, the
/// calendar and contacts that are not connected, and what only gmail.com
/// does. `connect` opens the calendar consent.
pub fn rows(
    app: &Rc<App>,
    account: &Account,
    offers: Offers,
    parent: &adw::ExpanderRow,
    connect: &Rc<dyn Fn(AccountId)>,
    contacts: Contacts,
) {
    if account.provider != Provider::Gmail {
        return;
    }
    if let Some(sign_in) = app.google_sign_in(account.id) {
        let core = Rc::clone(&app.core);
        let find_project: FindProject = Rc::new(move |address, link| {
            let (core, address, link) = (Rc::clone(&core), address.to_string(), link.downgrade());
            glib::spawn_future_local(async move {
                let page = core.own_project_page(address).await;
                if let Some(link) = link.upgrade() {
                    link.set_uri(&page);
                }
            });
        });
        register(
            parent,
            account,
            sign_in,
            offers,
            connect,
            contacts,
            find_project,
        );
    }
}

/// Fills `parent` and keeps the rows for [`refill`].
fn register(
    parent: &adw::ExpanderRow,
    account: &Account,
    sign_in: GmailSignIn,
    offers: Offers,
    connect: &Rc<dyn Fn(AccountId)>,
    contacts: Contacts,
    find_project: FindProject,
) {
    let shown = Rc::new(RouteRows {
        parent: parent.downgrade(),
        rows: RefCell::default(),
        sign_in,
        connect: Rc::clone(connect),
        contacts,
        find_project,
    });
    let added = fill_rows(parent, account, offers, &shown);
    *shown.rows.borrow_mut() = added.iter().map(|w| w.downgrade()).collect();
    ROUTE_ROWS.with(|all| {
        let mut all = all.borrow_mut();
        // Rows of a Preferences that has closed, or of an account it no
        // longer shows, go with their closures.
        all.retain(|_, rows| rows.parent.upgrade().is_some());
        all.insert(account.id, shown);
    });
}

/// Fills `account`'s Gmail rows again from what it offers now, in the
/// Preferences that is open, and touches nothing else in it: focus,
/// scroll and the other accounts stay as they are. False when no open
/// Preferences shows the account.
pub fn refill(id: AccountId, account: &Account, offers: Offers) -> bool {
    let shown = ROUTE_ROWS.with(|all| all.borrow().get(&id).cloned());
    let Some(shown) = shown else {
        return false;
    };
    let Some(parent) = shown.parent.upgrade() else {
        ROUTE_ROWS.with(|all| all.borrow_mut().remove(&id));
        return false;
    };
    let old = std::mem::take(&mut *shown.rows.borrow_mut());
    for row in old.iter().filter_map(|row| row.upgrade()) {
        parent.remove(&row);
    }
    let added = fill_rows(&parent, account, offers, &shown);
    *shown.rows.borrow_mut() = added.iter().map(|w| w.downgrade()).collect();
    let calendar = CALENDAR_ROWS.with(|all| all.borrow().get(&id).cloned());
    if let Some((row, button)) = calendar
        && !calendar_link(account, offers).shows_connect()
    {
        if let Some(row) = row.upgrade() {
            row.set_subtitle(&gettext("Connected"));
        }
        if let Some(button) = button.upgrade() {
            button.set_visible(false);
        }
    }
    true
}

/// Keeps the Calendar page's row for `id`, which says Not connected, so a
/// connect can change it.
pub fn note_calendar_row(id: AccountId, row: &adw::ActionRow, button: &gtk::Button) {
    CALENDAR_ROWS.with(|all| {
        let mut all = all.borrow_mut();
        all.retain(|_, (row, _)| row.upgrade().is_some());
        all.insert(id, (row.downgrade(), button.downgrade()));
    });
}

/// [`rows`] for a sign-in already read, in the order the mockup draws:
/// the features, then calendar and contacts, then what only gmail.com
/// does. Returns the widgets it added.
fn fill_rows(
    parent: &adw::ExpanderRow,
    account: &Account,
    offers: Offers,
    shown: &RouteRows,
) -> Vec<gtk::Widget> {
    let (connect, contacts) = (&shown.connect, &shown.contacts);
    let mut added: Vec<gtk::Widget> = Vec::new();
    let mut put = |widget: &gtk::Widget| {
        parent.add_row(widget);
        added.push(widget.clone());
    };
    let (gaps, own): (Vec<Line>, Vec<Line>) = lines(&shown.sign_in)
        .into_iter()
        .partition(|line| matches!(line, Line::Gap { .. }));
    if !own.is_empty() {
        // Translators: a small heading over the Gmail account's rows; Gmail
        // is a name, keep it as it is.
        put(heading(&gettext("Gmail")).upcast_ref());
    }
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
                put(row.upcast_ref());
            }
            Line::Limits { text } => {
                let row = plain_row(&text, None);
                row.set_title_lines(0);
                put(row.upcast_ref());
            }
            Line::OwnProject => {
                // Translators: Google is a name; keep it as it is.
                let row = plain_row(&gettext("Signed in through your own Google project"), None);
                let label = gettext("Open Google Cloud");
                let inside = crate::ui::external_link_content(&label, 6);
                let link = gtk::LinkButton::builder()
                    .uri(crate::own_project::PROJECT_PAGE)
                    .child(&inside)
                    .valign(gtk::Align::Center)
                    .build();
                link.add_css_class("gmail-link");
                crate::ui::name(&link, &label);
                (shown.find_project)(&account.email, &link);
                row.add_suffix(&link);
                put(row.upcast_ref());
            }
            Line::Gap { .. } => {}
        }
    }
    if calendar_link(account, offers).shows_connect() {
        put(heading(&gettext("Calendar and contacts")).upcast_ref());
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
            put(row.upcast_ref());
        }
    } else if !contacts.added {
        // The page was built without it, while Contacts said Not connected.
        put(&(contacts.build)(offers));
    }
    if !gaps.is_empty() {
        // Translators: a small heading over what only gmail.com does for the
        // account; gmail.com is a name, keep it as it is.
        put(heading(&gettext("On gmail.com")).upcast_ref());
    }
    for gap in gaps {
        if let Line::Gap { text, link, label } = gap {
            put(gap_row(&text, link, &label).upcast_ref());
        }
    }
    added
}

/// A small heading over a group of the account's rows, in capitals as the
/// mockup draws it. An expander's rows have no group titles of their own.
fn heading(text: &str) -> adw::PreferencesRow {
    let words = text.to_uppercase();
    let label = gtk::Label::builder()
        .label(&words)
        .xalign(0.0)
        .css_classes(["gmail-heading"])
        .build();
    label.set_accessible_role(gtk::AccessibleRole::Heading);
    let row = adw::PreferencesRow::builder()
        .title(&words)
        .use_markup(false)
        .activatable(false)
        .focusable(false)
        // Nothing to press, so the pointer passes over it with no hover.
        .can_target(false)
        .child(&label)
        .css_classes(["gmail-heading-row"])
        .build();
    // The row is a list item, and a list item takes no name from the
    // label inside it.
    row.upcast_ref::<gtk::Widget>()
        .update_property(&[gtk::accessible::Property::Label(&words)]);
    row
}

/// A sentence with its link under it, the link bold and followed by the
/// mark that says it leaves the app.
fn gap_row(text: &str, link: &str, label: &str) -> adw::PreferencesRow {
    let words = gtk::Label::builder()
        .label(text)
        .wrap(true)
        .xalign(0.0)
        .build();
    let inside = crate::ui::external_link_content(label, 6);
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
    use mailrs_domain::{GmailSignIn, MailRoute, SignInClient};

    fn sign_in(client: SignInClient, route: MailRoute) -> GmailSignIn {
        match route {
            MailRoute::GmailApi => GmailSignIn::BuiltInApi.with_google_token(client),
            MailRoute::Imap => GmailSignIn::AppPassword { google: client },
        }
    }

    #[test]
    fn an_api_account_has_every_feature() {
        let found = lines(&sign_in(SignInClient::Own, MailRoute::GmailApi));
        assert_eq!(found, [Line::Features { every: true }, Line::OwnProject]);
    }

    #[test]
    fn only_an_account_on_its_own_project_says_so() {
        let built_in = lines(&sign_in(SignInClient::BuiltIn, MailRoute::Imap));
        assert!(!built_in.contains(&Line::OwnProject));
    }

    #[test]
    fn a_built_in_api_account_has_every_feature_and_nothing_more_to_say() {
        let found = lines(&GmailSignIn::BuiltInApi);
        assert_eq!(found, [Line::Features { every: true }]);
    }

    #[test]
    fn an_imap_account_points_filters_and_the_reply_to_gmail() {
        let found = lines(&sign_in(SignInClient::BuiltIn, MailRoute::Imap));
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
}

#[cfg(test)]
pub(crate) mod checks {
    use std::cell::RefCell;

    use super::*;
    use mailrs_domain::{AccountState, SignInClient};

    pub fn run() {
        an_imap_account_reads_in_the_mockups_order();
        connect_asks_for_the_account();
        an_api_account_shows_its_features_and_nothing_to_connect();
        rows_built_again_after_a_connect_read_as_connected();
        refilling_one_accounts_rows_leaves_the_rest_alone();
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
        sign_in: GmailSignIn,
        offers: Offers,
    ) -> (adw::ExpanderRow, Rc<RefCell<Vec<AccountId>>>) {
        let pressed = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&pressed);
        let connect: Rc<dyn Fn(AccountId)> = Rc::new(move |id| sink.borrow_mut().push(id));
        let parent = adw::ExpanderRow::new();
        // The page holds the control unless the account has no calendar.
        let kept = Contacts {
            added: offers.calendar,
            ..contacts()
        };
        register(
            &parent,
            &account(state),
            sign_in,
            offers,
            &connect,
            kept,
            Rc::new(|_, _| {}),
        );
        (parent, pressed)
    }

    /// What the Contacts control would be: a switch row.
    fn contacts() -> Contacts {
        Contacts {
            build: Rc::new(|_| {
                adw::SwitchRow::builder()
                    .title("Contacts")
                    .build()
                    .upcast::<gtk::Widget>()
            }),
            added: false,
        }
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

    fn imap() -> GmailSignIn {
        GmailSignIn::AppPassword {
            google: SignInClient::BuiltIn,
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
                "GMAIL",
                "Gmail features",
                "CALENDAR AND CONTACTS",
                "Calendar",
                "Contacts",
                "ON GMAIL.COM",
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
        let headings: Vec<&adw::PreferencesRow> = rows
            .iter()
            .filter(|row| row.has_css_class("gmail-heading-row"))
            .collect();
        assert_eq!(headings.len(), 3);
        for row in headings {
            assert!(gtk::test_accessible_has_property(
                row.upcast_ref::<gtk::Widget>(),
                gtk::AccessibleProperty::Label
            ));
        }
    }

    fn connect_asks_for_the_account() {
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
    }

    fn an_api_account_shows_its_features_and_nothing_to_connect() {
        let own = GmailSignIn::OwnProject;
        let (parent, _) = filled(AccountState::Ok, own, Offers::EVERYTHING);
        assert_eq!(
            titles(&parent),
            [
                "GMAIL",
                "Gmail features",
                "Signed in through your own Google project"
            ]
        );
        let mut buttons = Vec::new();
        buttons_of(parent.upcast_ref(), &mut buttons);
        let link = buttons
            .into_iter()
            .find_map(|b| b.downcast::<gtk::LinkButton>().ok())
            .expect("the project row links to Google Cloud");
        assert_eq!(link.uri(), crate::own_project::PROJECT_PAGE);
    }

    /// What the window does after the calendar consent: the rows are built
    /// again from what the restarted account offers.
    fn rows_built_again_after_a_connect_read_as_connected() {
        let (before, _) = filled(AccountState::Ok, imap(), no_calendar());
        assert!(titles(&before).contains(&"CALENDAR AND CONTACTS".to_string()));
        let (after, _) = filled(AccountState::Ok, imap(), Offers::EVERYTHING);
        let mut rows = Vec::new();
        rows_of(after.upcast_ref(), &mut rows);
        let said: Vec<String> = rows
            .iter()
            .filter_map(|row| row.downcast_ref::<adw::ActionRow>())
            .filter_map(|row| row.subtitle())
            .map(|subtitle| subtitle.to_string())
            .collect();
        assert!(!said.contains(&"Not connected".to_string()), "{said:?}");
        let found = titles(&after);
        assert!(!found.contains(&"Calendar".to_string()), "{found:?}");
        assert!(
            !found.contains(&"CALENDAR AND CONTACTS".to_string()),
            "{found:?}"
        );
    }

    /// What the window does once the consent goes through: only this
    /// account's Gmail rows change. Another account's expander keeps its
    /// state, and a field being typed in keeps its text and its focus.
    fn refilling_one_accounts_rows_leaves_the_rest_alone() {
        let (mine, _) = filled(AccountState::Ok, imap(), no_calendar());
        let other = adw::ExpanderRow::builder().expanded(true).build();
        let typing = gtk::Entry::new();
        let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        column.append(&mine);
        column.append(&other);
        column.append(&typing);
        let window = gtk::Window::builder().child(&column).build();
        window.present();
        typing.set_text("half a signature");
        typing.grab_focus();
        assert!(
            GtkWindowExt::focus(&window).is_some_and(|f| f.is_ancestor(&typing)),
            "the entry keeps the focus"
        );
        assert!(titles(&mine).contains(&"Contacts".to_string()));
        let before = titles(&mine);
        assert!(before.contains(&"CALENDAR AND CONTACTS".to_string()));
        assert!(refill(3, &account(AccountState::Ok), Offers::EVERYTHING));
        let mut rows = Vec::new();
        rows_of(mine.upcast_ref(), &mut rows);
        let said: Vec<String> = rows
            .iter()
            .filter_map(|row| row.downcast_ref::<adw::ActionRow>())
            .filter_map(|row| row.subtitle())
            .map(|subtitle| subtitle.to_string())
            .collect();
        assert!(!said.contains(&"Not connected".to_string()), "{said:?}");
        let after = titles(&mine);
        assert!(
            !after.contains(&"CALENDAR AND CONTACTS".to_string()),
            "{after:?}"
        );
        // The Contacts control takes the place of the Not connected row.
        assert_eq!(
            after.iter().filter(|t| *t == "Contacts").count(),
            1,
            "{after:?}"
        );
        assert!(after.contains(&"GMAIL".to_string()));
        assert!(other.is_expanded());
        assert_eq!(typing.text(), "half a signature");
        // No window manager under the checks, so ask the window which widget
        // it would focus rather than whether the window is active.
        assert!(
            GtkWindowExt::focus(&window).is_some_and(|f| f.is_ancestor(&typing)),
            "the entry keeps the focus"
        );
        // Asked again for an account the dialog never showed, nothing happens.
        assert!(!refill(99, &account(AccountState::Ok), Offers::EVERYTHING));
        window.close();
    }
}
