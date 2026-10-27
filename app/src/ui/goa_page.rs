//! The page that signs a Gmail account in through the desktop's Online
//! Accounts: the account is already there, it has to be added, the page
//! waits for it, or the snap needs permission first.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;

use super::add_account::{banded, boxed_list, heading, label, pill};
use crate::keyring_plug::ACCOUNTS_COMMAND;
use mailrs_domain::translate::{fill, gettext};
use mailrs_sync::goa::GoaAccount;

/// The page's tag in the dialog's navigation view.
pub const TAG: &str = "goa";

/// What the page shows. `others` are the Google accounts Online Accounts
/// holds besides the one wanted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageState {
    Holds {
        account: GoaAccount,
    },
    Add {
        address: String,
        others: Vec<GoaAccount>,
    },
    Waiting {
        address: String,
        others: Vec<GoaAccount>,
    },
    OtherAdded {
        address: String,
        added: GoaAccount,
        others: Vec<GoaAccount>,
    },
    Missing {
        address: String,
    },
    SnapPlug {
        address: String,
    },
    /// This computer has no Online Accounts to ask.
    NotInstalled {
        address: String,
    },
}

type Pick = Rc<RefCell<Option<Rc<dyn Fn(GoaAccount)>>>>;
type Plain = Rc<RefCell<Option<Rc<dyn Fn()>>>>;

pub struct GoaPage {
    pub page: adw::NavigationPage,
    title: gtk::Label,
    body: gtk::Label,
    /// The main button: Use This Account, Open Online Accounts or Use It.
    main: gtk::Button,
    /// Under the main button: "Wait for {address}" after another account
    /// turned up.
    wait: gtk::Button,
    /// The spinner and its live label.
    waiting: gtk::Box,
    waiting_label: gtk::Label,
    spinner: adw::Spinner,
    others: gtk::ListBox,
    command: gtk::Box,
    command_text: gtk::Label,
    copy: gtk::Button,
    other_ways: gtk::Button,
    problem: gtk::Label,
    state: RefCell<Option<PageState>>,
    on_use: Pick,
    on_open: Plain,
    on_wait: Plain,
    on_other_ways: Plain,
}

impl GoaPage {
    pub fn new() -> Rc<GoaPage> {
        let banded = banded(TAG, &gettext("Add your Google account to this computer"));
        let title = heading("");
        let centered = |label: &gtk::Label| {
            label.set_xalign(0.5);
            label.set_justify(gtk::Justification::Center);
        };
        centered(&title);
        title.add_css_class("post-route-title");
        let body = label("", &["post-lede", "post-consent-text"]);
        body.set_use_markup(true);
        centered(&body);
        let main = pill("", true);
        main.set_halign(gtk::Align::Center);
        main.set_margin_top(20);
        let wait = pill("", false);
        wait.set_halign(gtk::Align::Center);
        wait.set_margin_top(6);
        let spinner = adw::Spinner::new();
        spinner.set_width_request(16);
        spinner.set_height_request(16);
        let waiting_label = label("", &["post-lede-small"]);
        waiting_label.set_use_markup(false);
        waiting_label.set_accessible_role(gtk::AccessibleRole::Status);
        let waiting = gtk::Box::builder()
            .spacing(10)
            .halign(gtk::Align::Center)
            .margin_top(20)
            .build();
        waiting.append(&spinner);
        waiting.append(&waiting_label);
        let others = boxed_list();
        others.add_css_class("post-routes");
        others.set_margin_top(16);
        let command_text = gtk::Label::builder()
            .label(ACCOUNTS_COMMAND)
            .selectable(true)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .xalign(0.0)
            .hexpand(true)
            .css_classes(["monospace"])
            .build();
        let copy = gtk::Button::builder()
            .label(gettext("Copy"))
            .valign(gtk::Align::Center)
            .build();
        let command = gtk::Box::builder()
            .spacing(8)
            .margin_top(16)
            .css_classes(["post-command"])
            .build();
        command.append(&command_text);
        command.append(&copy);
        let other_ways = gtk::Button::builder()
            .label(gettext("Other ways to sign in"))
            .halign(gtk::Align::Center)
            .margin_top(10)
            .css_classes(["flat"])
            .build();
        let problem = label("", &["post-lede-small", "error"]);
        problem.set_use_markup(false);
        centered(&problem);
        problem.set_visible(false);
        for part in [
            title.upcast_ref::<gtk::Widget>(),
            body.upcast_ref(),
            main.upcast_ref(),
            wait.upcast_ref(),
            waiting.upcast_ref(),
            problem.upcast_ref(),
            command.upcast_ref(),
            others.upcast_ref(),
            other_ways.upcast_ref(),
        ] {
            banded.body.append(part);
        }
        let this = Rc::new(GoaPage {
            page: banded.page,
            title,
            body,
            main,
            wait,
            waiting,
            waiting_label,
            spinner,
            others,
            command,
            command_text,
            copy,
            other_ways,
            problem,
            state: RefCell::new(None),
            on_use: Pick::default(),
            on_open: Plain::default(),
            on_wait: Plain::default(),
            on_other_ways: Plain::default(),
        });
        this.connect();
        this
    }

    fn connect(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.main.connect_clicked(move |_| {
            let Some(this) = weak.upgrade() else { return };
            let state = this.state.borrow().clone();
            match state {
                Some(PageState::Holds { account }) => this.use_account(account),
                Some(PageState::OtherAdded { added, .. }) => this.use_account(added),
                Some(PageState::Add { .. }) => call(&this.on_open),
                _ => {}
            }
        });
        let weak = Rc::downgrade(self);
        self.wait.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                call(&this.on_wait);
            }
        });
        let weak = Rc::downgrade(self);
        self.other_ways.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                call(&this.on_other_ways);
            }
        });
        let weak = Rc::downgrade(self);
        self.copy.connect_clicked(move |button| {
            button.clipboard().set_text(ACCOUNTS_COMMAND);
            if let Some(this) = weak.upgrade() {
                this.copy.set_label(&gettext("Copied"));
            }
        });
        let weak = Rc::downgrade(self);
        self.page.connect_shown(move |_| {
            if let Some(this) = weak.upgrade() {
                this.focus_first();
            }
        });
    }

    fn use_account(&self, account: GoaAccount) {
        self.problem.set_visible(false);
        self.busy(true);
        let handler = self.on_use.borrow().clone();
        if let Some(handler) = handler {
            handler(account);
        }
    }

    /// The page, or its main button, while a link runs.
    pub fn busy(&self, busy: bool) {
        self.main.set_sensitive(!busy);
        if busy {
            self.waiting_label.set_label("");
        }
        self.spinner.set_visible(busy);
        self.waiting.set_visible(busy);
        if !busy {
            // The button is back after a failure; keep the keyboard on it.
            self.main.grab_focus();
        }
    }

    /// Says why a link failed, under the button, and gives the button back.
    pub fn failed(&self, said: &str) {
        self.busy(false);
        self.waiting.set_visible(false);
        self.problem.set_label(said);
        self.problem.set_visible(true);
    }

    pub fn connect_use(&self, f: impl Fn(GoaAccount) + 'static) {
        *self.on_use.borrow_mut() = Some(Rc::new(f));
    }

    pub fn connect_open(&self, f: impl Fn() + 'static) {
        *self.on_open.borrow_mut() = Some(Rc::new(f));
    }

    pub fn connect_wait(&self, f: impl Fn() + 'static) {
        *self.on_wait.borrow_mut() = Some(Rc::new(f));
    }

    pub fn connect_other_ways(&self, f: impl Fn() + 'static) {
        *self.on_other_ways.borrow_mut() = Some(Rc::new(f));
    }

    fn focus_first(&self) {
        if self.main.is_visible() && self.main.is_sensitive() {
            self.main.grab_focus();
        } else if self.other_ways.is_visible() {
            self.other_ways.grab_focus();
        }
    }

    pub fn show(self: &Rc<Self>, state: PageState) {
        self.problem.set_visible(false);
        self.spinner.set_visible(false);
        self.waiting.set_visible(false);
        self.command.set_visible(false);
        self.wait.set_visible(false);
        self.main.set_visible(true);
        self.main.set_sensitive(true);
        self.others.remove_all();
        self.others.set_visible(false);
        match &state {
            PageState::Holds { account } => {
                self.title
                    .set_label(&gettext("Use the Google account on this computer"));
                self.set_body(
                    // Translators: {address} is an email address.
                    &gettext(
                        "This computer is already signed in to <b>{address}</b>. Penguin Mail can read your mail through it.",
                    ),
                    &account.identity,
                );
                self.main.set_label(&gettext("Use This Account"));
            }
            PageState::Add { address, others } => {
                self.adding(address);
                self.main.set_label(&open_label());
                self.list_others(others);
            }
            PageState::Waiting { address, others } => {
                self.adding(address);
                self.main.set_visible(false);
                self.waiting.set_visible(true);
                self.spinner.set_visible(true);
                self.waiting_label.set_label(&fill(
                    &gettext("Waiting for {address}…"),
                    &[("address", address)],
                ));
                self.list_others(others);
            }
            PageState::OtherAdded {
                address,
                added,
                others,
            } => {
                self.title.set_label(&fill(
                    &gettext("You added {address}. Use it instead?"),
                    &[("address", &added.identity)],
                ));
                self.body.set_visible(false);
                self.main.set_label(&gettext("Use It"));
                self.wait.set_label(&fill(
                    &gettext("Wait for {address}"),
                    &[("address", address)],
                ));
                self.wait.set_visible(true);
                self.list_others(others);
            }
            PageState::Missing { address } => {
                self.title
                    .set_label(&gettext("Add your Google account to this computer"));
                self.body.set_visible(true);
                // Translators: "Online Accounts" is the name GNOME and Linux Mint show for this window; use the name your desktop shows in your language.
                self.body.set_markup(&gettext(
                    "Open <b>Online Accounts</b> from your applications menu, add your Google account, and come back.",
                ));
                self.main.set_visible(false);
                self.waiting.set_visible(true);
                self.spinner.set_visible(true);
                self.waiting_label.set_label(&fill(
                    &gettext("Waiting for {address}…"),
                    &[("address", address)],
                ));
            }
            PageState::NotInstalled { .. } => {
                self.title
                    .set_label(&gettext("Add your Google account to this computer"));
                self.body.set_visible(true);
                self.body.set_use_markup(false);
                // Translators: "Online Accounts" is the name the software center shows for the gnome-online-accounts-gtk package; use the name it shows in your language.
                self.body.set_label(&gettext(
                    "Install Online Accounts from your software center, add your Google account there, and come back.",
                ));
                self.main.set_visible(false);
            }
            PageState::SnapPlug { .. } => {
                self.title
                    .set_label(&gettext("Add your Google account to this computer"));
                self.body.set_visible(true);
                self.body.set_use_markup(false);
                // Translators: "Online Accounts" is the name GNOME and Linux Mint show for this window; use the name your desktop shows in your language.
                self.body.set_label(&gettext(
                    "Penguin Mail needs permission to use Online Accounts.",
                ));
                self.main.set_visible(false);
                self.command.set_visible(true);
                self.copy.set_label(&gettext("Copy"));
                crate::ui::name(&self.command_text, ACCOUNTS_COMMAND);
            }
        }
        if !matches!(
            state,
            PageState::SnapPlug { .. } | PageState::NotInstalled { .. }
        ) {
            self.body.set_use_markup(true);
        }
        *self.state.borrow_mut() = Some(state);
        self.focus_first();
    }

    fn adding(&self, address: &str) {
        self.title
            .set_label(&gettext("Add your Google account to this computer"));
        self.body.set_visible(true);
        self.set_body(
            // Translators: "Online Accounts" is the name GNOME and Linux Mint show for this window; use the name your desktop shows in your language. {address} is an email address.
            &gettext(
                "Penguin Mail reads Gmail through your computer's Online Accounts. Add <b>{address}</b> there, then come back.",
            ),
            address,
        );
    }

    /// `words` with `address` in bold where it says `{address}`. The
    /// address is escaped, so it shows as typed whatever it holds.
    fn set_body(&self, words: &str, address: &str) {
        self.body.set_visible(true);
        self.body.set_use_markup(true);
        self.body.set_markup(&fill(
            words,
            &[("address", &gtk::glib::markup_escape_text(address))],
        ));
    }

    fn list_others(self: &Rc<Self>, others: &[GoaAccount]) {
        self.others.set_visible(!others.is_empty());
        for other in others {
            let row = adw::ActionRow::builder()
                .use_markup(false)
                .title(fill(
                    &gettext("Use {address} instead"),
                    &[("address", &other.identity)],
                ))
                .activatable(true)
                .build();
            row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            let (weak, other) = (Rc::downgrade(self), other.clone());
            row.connect_activated(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.use_account(other.clone());
                }
            });
            self.others.append(&row);
        }
    }
}

/// The label of the button that opens Online Accounts.
fn open_label() -> String {
    // Translators: "Online Accounts" is the name GNOME and Linux Mint show for this window; use the name your desktop shows in your language.
    gettext("Open Online Accounts")
}

fn call(slot: &Plain) {
    let handler = slot.borrow().clone();
    if let Some(handler) = handler {
        handler();
    }
}

#[cfg(test)]
pub(crate) mod checks {
    use super::*;

    pub fn run() {
        the_holds_page_names_the_address_in_bold();
        an_address_with_markup_characters_shows_as_typed();
        the_waiting_page_gives_way_to_holds();
        another_new_account_asks_whether_to_use_it();
        the_snap_page_shows_the_connect_command();
        without_online_accounts_the_page_says_to_install_it();
        the_main_button_hands_over_the_account_it_names();
    }

    fn account(id: &str, address: &str) -> GoaAccount {
        GoaAccount {
            id: id.into(),
            identity: address.into(),
            presentation: address.into(),
            mail_disabled: false,
            attention_needed: false,
        }
    }

    fn the_holds_page_names_the_address_in_bold() {
        let page = GoaPage::new();
        page.show(PageState::Holds {
            account: account("account_1_0", "dana@example.com"),
        });
        assert_eq!(
            page.title.label(),
            "Use the Google account on this computer"
        );
        assert!(page.body.label().contains("<b>dana@example.com</b>"));
        assert_eq!(page.main.label().as_deref(), Some("Use This Account"));
        assert!(page.main.is_visible() && page.main.is_sensitive());
        assert!(!page.waiting.is_visible());
    }

    fn an_address_with_markup_characters_shows_as_typed() {
        let page = GoaPage::new();
        page.show(PageState::Add {
            address: "r&d@example.com".into(),
            others: vec![account("account_2_0", "x&y@example.com")],
        });
        assert!(page.body.text().contains("r&d@example.com"));
        assert!(page.body.label().contains("<b>r&amp;d@example.com</b>"));
        let row = page
            .others
            .row_at_index(0)
            .and_downcast::<adw::ActionRow>()
            .expect("a row for the other account");
        assert!(!row.uses_markup());
        assert_eq!(row.title(), "Use x&y@example.com instead");
    }

    fn the_waiting_page_gives_way_to_holds() {
        let page = GoaPage::new();
        page.show(PageState::Waiting {
            address: "dana@example.com".into(),
            others: Vec::new(),
        });
        assert_eq!(page.waiting_label.label(), "Waiting for dana@example.com…");
        assert!(page.waiting.is_visible() && !page.main.is_visible());
        page.show(PageState::Holds {
            account: account("account_1_0", "dana@example.com"),
        });
        assert!(!page.waiting.is_visible() && page.main.is_visible());
        assert_eq!(
            page.title.label(),
            "Use the Google account on this computer"
        );
    }

    fn another_new_account_asks_whether_to_use_it() {
        let page = GoaPage::new();
        page.show(PageState::OtherAdded {
            address: "dana@example.com".into(),
            added: account("account_2_0", "other@example.com"),
            others: Vec::new(),
        });
        assert_eq!(
            page.title.label(),
            "You added other@example.com. Use it instead?"
        );
        assert_eq!(page.main.label().as_deref(), Some("Use It"));
        assert_eq!(
            page.wait.label().as_deref(),
            Some("Wait for dana@example.com")
        );
    }

    fn the_snap_page_shows_the_connect_command() {
        let page = GoaPage::new();
        page.show(PageState::SnapPlug {
            address: "dana@example.com".into(),
        });
        assert_eq!(
            page.body.text(),
            "Penguin Mail needs permission to use Online Accounts."
        );
        assert!(page.command.is_visible());
        assert_eq!(page.command_text.label(), ACCOUNTS_COMMAND);
        assert_eq!(page.copy.label().as_deref(), Some("Copy"));
    }

    fn without_online_accounts_the_page_says_to_install_it() {
        let page = GoaPage::new();
        page.show(PageState::NotInstalled {
            address: "dana@example.com".into(),
        });
        assert!(page.body.text().starts_with("Install Online Accounts"));
        assert!(!page.main.is_visible());
        assert!(page.other_ways.is_visible());
    }

    fn the_main_button_hands_over_the_account_it_names() {
        let page = GoaPage::new();
        let used: Rc<RefCell<Vec<String>>> = Rc::default();
        let sink = Rc::clone(&used);
        page.connect_use(move |a| sink.borrow_mut().push(a.id));
        page.show(PageState::OtherAdded {
            address: "dana@example.com".into(),
            added: account("account_2_0", "other@example.com"),
            others: Vec::new(),
        });
        page.main.emit_clicked();
        assert_eq!(*used.borrow(), ["account_2_0"]);
        assert!(!page.main.is_sensitive(), "the button waits for the link");
        page.failed("Online Accounts couldn't sign in to Google. Sign in again there.");
        assert!(page.main.is_sensitive());
        assert!(page.problem.is_visible());
    }
}
