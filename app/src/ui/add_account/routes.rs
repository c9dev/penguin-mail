//! Add Account's ways to sign in to Gmail: the route choice, the Online
//! Accounts page and the app password page.

use super::*;

impl Dialog {
    // ---- The routes --------------------------------------------------------------

    /// Asks how a Google account signs in: the route page, with the own
    /// project first. On Linux the second row names Online Accounts, worded
    /// by whether it holds the address; elsewhere it is the app password.
    /// `expected` is the address typed, when there was one.
    pub(super) fn offer_routes(self: &Rc<Self>, expected: Option<String>) {
        self.goa_after_address.set(false);
        let platform = Platform::this();
        if platform != Platform::Linux {
            return self.show_routes(platform, ready_now(Goa::Absent), expected);
        }
        if !self.running.begin() {
            return;
        }
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let list = this.core.goa_accounts().await;
            this.running.end();
            this.update_buttons();
            let goa = route_choice::goa_state(&list, expected.as_deref());
            this.show_routes(platform, ready_now(goa), expected);
        });
    }

    pub(super) fn show_routes(
        self: &Rc<Self>,
        platform: Platform,
        ready: Ready,
        expected: Option<String>,
    ) {
        self.routes_for(platform, ready, expected);
        self.push(route_page::TAG);
    }

    /// Fills the route page without showing it.
    pub(super) fn routes_for(
        self: &Rc<Self>,
        platform: Platform,
        ready: Ready,
        expected: Option<String>,
    ) {
        let saved = self.saved.borrow().is_some();
        let ready = Ready {
            saved_project: ready.saved_project || saved,
            ..ready
        };
        self.route_expected.replace(expected);
        self.routes_goa.set(ready.goa);
        let weak = Rc::downgrade(self);
        self.routes
            .show(&rows(platform, ready), Some(footer()), move |route| {
                if let Some(this) = weak.upgrade() {
                    this.chose_route(route);
                }
            });
        // The closed envelope with no stamp: no provider is picked yet.
        self.set_band(route_page::TAG, Step::Address, None, None);
    }

    /// Reads the project kept from an earlier setup, and offers it on the
    /// route page once the keyring answers. An entry that does not decode
    /// counts as none, and nothing is said about it.
    pub(super) fn load_saved_project(self: &Rc<Self>) {
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let Some(own) = this.core.saved_project().await else {
                return;
            };
            this.saved.replace(Some(own));
            let expected = this.route_expected.borrow().clone();
            let platform = Platform::this();
            this.routes_for(platform, ready_now(this.routes_goa.get()), expected);
        });
    }

    pub(super) fn chose_route(self: &Rc<Self>, route: Route) {
        self.goa_after_address.set(false);
        match route {
            Route::SavedProject => {
                let saved = self.saved.borrow().clone();
                if let Some(own) = saved {
                    self.open_own(OwnStart::Saved(own));
                }
            }
            // The setup opens unless the environment names a client, which
            // wins over any the setup would make.
            Route::OwnProject if mailrs_sync::Clients::current().own_google.is_none() => {
                self.open_own(OwnStart::Resume);
            }
            Route::OwnProject => {
                let expected = self.route_expected.borrow().clone();
                self.run_browser(Browser::Google, expected, Flow::SignIn, None);
            }
            Route::Goa => self.choose_goa(),
            Route::AppPassword => {
                let typed = self.route_expected.borrow().clone();
                self.open_gmail_password(typed, false);
                self.push(gmail_password::TAG);
            }
        }
    }

    /// The other way to sign in that the setup's pages offer: Online
    /// Accounts on Linux, an app password elsewhere.
    pub(super) fn switch_from_setup(self: &Rc<Self>) {
        match Platform::this() {
            Platform::Linux => self.chose_route(Route::Goa),
            Platform::MacOs => self.chose_route(Route::AppPassword),
        }
    }

    // ---- Online Accounts -------------------------------------------------------

    /// Opens the Online Accounts page for the address typed, or asks for the
    /// address first when there is none: the page is about one address.
    pub(super) fn choose_goa(self: &Rc<Self>) {
        let expected = self.route_expected.borrow().clone();
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let list = this.core.goa_accounts().await;
            match (expected, list) {
                (Some(address), Ok(list)) => {
                    let typed = Some(address.clone());
                    this.open_goa_page(address, &list, typed);
                }
                (None, Ok(_)) => {
                    this.goa_after_address.set(true);
                    this.open_address(Some(Tile::Google), false);
                }
                (address, Err(_)) => this.goa_unavailable(address.unwrap_or_default()).await,
            }
        });
    }

    /// The Online Accounts page for an Online Accounts that does not answer:
    /// the snap needs permission, or the computer has none.
    async fn goa_unavailable(self: &Rc<Self>, address: String) {
        let plug = match crate::packaging::BUILT_FOR {
            crate::packaging::Packaging::Snap => Some(crate::keyring_plug::accounts_plug().await),
            _ => None,
        };
        self.goa_address.replace(Some(address.clone()));
        match plug == Some(crate::keyring_plug::Plug::Disconnected) {
            true => self.goa.show(PageState::SnapPlug { address }),
            false => self.goa.show(PageState::NotInstalled { address }),
        }
        self.set_band(goa_page::TAG, Step::Address, None, None);
        self.push(goa_page::TAG);
    }

    /// The Online Accounts page for `address`, in the state `list` puts it
    /// in: the account is there to use, or has to be added.
    pub(super) fn open_goa_page(
        self: &Rc<Self>,
        address: String,
        list: &[mailrs_sync::goa::GoaAccount],
        expected: Option<String>,
    ) {
        use mailrs_sync::goa;
        self.stop_goa_wait();
        self.goa_address.replace(Some(address.clone()));
        self.goa_before
            .replace(list.iter().map(|a| a.id.clone()).collect());
        self.route_expected.replace(expected);
        match goa::find(list, None, &address) {
            Some(account) => self.goa.show(PageState::Holds {
                account: account.clone(),
            }),
            None => self.goa.show(PageState::Add {
                address,
                others: list.to_vec(),
            }),
        }
        self.set_band(
            goa_page::TAG,
            Step::Address,
            Some(post::stamp_for("Google")),
            Some("Google"),
        );
        self.push(goa_page::TAG);
    }

    /// Wires the Online Accounts page's buttons.
    pub(super) fn connect_goa(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.goa.connect_use(move |account| {
            if let Some(this) = weak.upgrade() {
                this.goa_use(account);
            }
        });
        let weak = Rc::downgrade(self);
        self.goa.connect_open(move || {
            if let Some(this) = weak.upgrade() {
                this.goa_open();
            }
        });
        let weak = Rc::downgrade(self);
        self.goa.connect_wait(move || {
            if let Some(this) = weak.upgrade() {
                let address = this.goa_address.borrow().clone().unwrap_or_default();
                this.goa.show(PageState::Waiting {
                    address,
                    others: Vec::new(),
                });
            }
        });
        let weak = Rc::downgrade(self);
        self.goa.connect_other_ways(move || {
            let Some(this) = weak.upgrade() else { return };
            this.stop_goa_wait();
            let held = this.goa_address.borrow().clone();
            let goa = match held {
                Some(address) => route_choice::goa_state(&Ok(Vec::new()), Some(&address)),
                None => Goa::Absent,
            };
            // The Online Accounts row was this page; the rest are the
            // other ways.
            let saved = this.saved.borrow().is_some();
            let ready = Ready {
                saved_project: saved,
                ..ready_now(goa)
            };
            let weak = Rc::downgrade(&this);
            this.routes.show(&other_ways(ready), None, move |route| {
                if let Some(this) = weak.upgrade() {
                    this.chose_route(route);
                }
            });
            this.set_band(route_page::TAG, Step::Address, None, None);
            this.push(route_page::TAG);
        });
    }

    /// Open Online Accounts, then wait for the address to turn up there.
    pub(super) fn goa_open(self: &Rc<Self>) {
        let address = self.goa_address.borrow().clone().unwrap_or_default();
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let opened = crate::goa::open_online_accounts().await;
            let list = this.core.goa_accounts().await.unwrap_or_default();
            this.goa_before
                .replace(list.iter().map(|a| a.id.clone()).collect());
            if matches!(opened, Ok(crate::goa::Opened::Window)) {
                this.goa.show(PageState::Waiting {
                    address: address.clone(),
                    others: Vec::new(),
                });
            } else {
                this.goa.show(PageState::Missing {
                    address: address.clone(),
                });
            }
            this.goa_wait(address);
        });
    }

    /// Reads Online Accounts every two seconds while the page waits.
    pub(super) fn goa_wait(self: &Rc<Self>, address: String) {
        self.stop_goa_wait();
        let weak = Rc::downgrade(self);
        let timer = glib::timeout_add_local(Duration::from_secs(2), move || {
            let Some(this) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let (this, address) = (Rc::clone(&this), address.clone());
            glib::spawn_future_local(async move { this.goa_look(address).await });
            glib::ControlFlow::Continue
        });
        self.goa_timer.replace(Some(timer));
    }

    pub(super) async fn goa_look(self: &Rc<Self>, address: String) {
        use mailrs_sync::goa;
        if self.goa_timer.borrow().is_none() {
            return;
        }
        let Ok(list) = self.core.goa_accounts().await else {
            return;
        };
        if let Some(found) = goa::find(&list, None, &address) {
            self.stop_goa_wait();
            self.goa.show(PageState::Holds {
                account: found.clone(),
            });
            return;
        }
        let before = self.goa_before.borrow().clone();
        if let Some(added) = list.iter().find(|a| !before.contains(&a.id)) {
            self.stop_goa_wait();
            let others = list.iter().filter(|a| a.id != added.id).cloned().collect();
            self.goa.show(PageState::OtherAdded {
                address,
                added: added.clone(),
                others,
            });
        }
    }

    pub(super) fn stop_goa_wait(&self) {
        let timer = self.goa_timer.borrow_mut().take();
        if let Some(timer) = timer {
            timer.remove();
        }
    }

    /// Signs in through the Online Accounts account `account`.
    pub(super) fn goa_use(self: &Rc<Self>, account: mailrs_sync::goa::GoaAccount) {
        self.stop_goa_wait();
        let ticket = self.asking.ask();
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            match this.core.link_goa(account).await {
                Ok(account) => {
                    // The account is kept whether or not the dialog is still
                    // open, so the window hears about it either way.
                    (this.done)(Done::BrowserAdded {
                        account: account.clone(),
                        name: None,
                    });
                    if this.asking.wants(ticket) {
                        this.typed.replace(Address::parse(&account.email));
                        let missing =
                            crate::permission::withheld_permissions(crate::offered::withheld_for(
                                this.core.account(account.id).as_ref().map(|s| s.services()),
                            ));
                        this.show_added(&account, Some(post::stamp_for("Google")), Some(&missing));
                        this.note_limits();
                    }
                }
                Err(err) if this.asking.wants(ticket) => this.goa.failed(&err.to_string()),
                Err(_) => {}
            }
        });
    }

    /// An account that reads its mail through Online Accounts has no
    /// filters, aliases or automatic reply; the last page says so.
    pub(super) fn note_limits(&self) {
        let lede = self.finished.lede.text();
        self.finished.lede.set_text(&format!(
            "{lede} {}",
            gettext("Filters, aliases and the automatic reply aren't available this way.")
        ));
    }

    // ---- The app password page ---------------------------------------------------

    /// Shows the app password page for a new sign-in, or for `address` to
    /// sign in again, and clears what an earlier try left on it.
    pub(super) fn open_gmail_password(self: &Rc<Self>, address: Option<String>, again: bool) {
        self.gmail.open(address.as_deref(), again);
        self.set_band(gmail_password::TAG, Step::Address, None, None);
        self.update_gmail();
        if again {
            self.push(gmail_password::TAG);
        }
    }

    pub(super) fn update_gmail(self: &Rc<Self>) {
        self.gmail.show_note();
        self.gmail.update_sign_in(self.running.is_on());
    }

    /// Back to the ways of signing in.
    pub(super) fn gmail_another_way(self: &Rc<Self>) {
        match self.nav.find_page(route_page::TAG).is_some() {
            true => self.back_to(route_page::TAG),
            false => self.back_to("pick"),
        }
    }

    /// The page's Sign In: checks the app password with Gmail, then keeps
    /// the account.
    pub(super) fn sign_in_gmail(self: &Rc<Self>) {
        let address = self.gmail.address.text().trim().to_string();
        let pasted = self.gmail.password.text().to_string();
        if !gmail_password::can_sign_in(&address, &pasted, self.running.is_on())
            || !self.running.begin()
        {
            return;
        }
        self.gmail.clear_failure();
        let ticket = self.asking.ask();
        self.update_gmail();
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let signed = this.core.sign_in_with_app_password(address, pasted).await;
            // Every way out of the run ends here, so Sign In comes back
            // whether the run signed in, failed or lost its dialog.
            this.running.end();
            this.update_gmail();
            match signed {
                Ok(account) => this.gmail_signed_in(account, ticket),
                Err(err) if this.asking.wants(ticket) => {
                    let failure = gmail_password::failure(&err);
                    tracing::info!(?failure, "a Gmail sign-in with an app password failed");
                    this.show_gmail_failure(&failure);
                }
                Err(err) => {
                    tracing::info!(%err, "a Gmail sign-in failed after its dialog moved on");
                }
            }
        });
    }

    /// A Gmail account works: the window hears of it, and the page that
    /// offers its calendar and contacts follows.
    pub(super) fn gmail_signed_in(self: &Rc<Self>, account: Account, ticket: add_account::Ticket) {
        if self.again.is_some() {
            return self.finish(Done::SignedInAgain(account));
        }
        (self.done)(Done::BrowserAdded {
            account: account.clone(),
            name: None,
        });
        if !self.asking.wants(ticket) {
            return;
        }
        self.fit_consent_note(&account);
        self.consenting.replace(Some(account));
        self.set_band(route_page::CONSENT_TAG, Step::Address, None, None);
        self.push(route_page::CONSENT_TAG);
        self.window.set_focus(Some(&self.consent.continue_button));
    }

    pub(super) fn show_gmail_failure(self: &Rc<Self>, failure: &gmail_password::GmailFailure) {
        let weak = Rc::downgrade(self);
        // An account that signs in again cannot change how it signs in.
        let again = self.again.is_some();
        self.gmail.show_failure(failure, again, move |action| {
            let Some(this) = weak.upgrade() else {
                return;
            };
            match action {
                GmailAction::OpenAppPasswords => this.open_link(gmail_password::APP_PASSWORDS_URL),
                GmailAction::OpenGmailLabels => this.open_link(gmail_password::LABELS_URL),
                GmailAction::SignInAnotherWay => this.gmail_another_way(),
                // The page the person came from: the route page, or the
                // providers.
                GmailAction::Back => {
                    this.nav.pop();
                }
                GmailAction::TryAgain => this.sign_in_gmail(),
            }
        });
    }

    /// Fits the consent page's note to `account`: hidden until the store
    /// says whether its consent meets Google through the built-in client.
    pub(super) fn fit_consent_note(self: &Rc<Self>, account: &Account) {
        self.consent.fit_note(false);
        let (this, account) = (Rc::clone(self), account.clone());
        glib::spawn_future_local(async move {
            let built_in = this.core.consent_through_built_in(account).await;
            this.consent.fit_note(built_in);
        });
    }

    /// Opens `url` in the browser.
    pub(super) fn open_link(&self, url: &str) {
        gtk::UriLauncher::new(url).launch(gtk::Window::NONE, gio::Cancellable::NONE, |_| {});
    }
}
