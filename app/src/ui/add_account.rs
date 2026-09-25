//! Add Account: the picker, then Another Provider's steps, the address
//! and the password, with Server Settings beside them. Done closes the
//! dialog and the window takes over. `crate::add_account` decides what
//! each step shows; this file draws it and carries the person's answers
//! there and back.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use mailrs_discover::Server;
use mailrs_domain::Account;
use mailrs_domain::translate::gettext;
use mailrs_store::servers;

use crate::add_account::{self, Address, Asking, Choice, Next, Outcome, Proposal, Typed};
use crate::core::Core;

/// Where the dialog opens.
pub enum Opening {
    /// The picker: Google or another provider.
    Pick,
    /// Another Provider's first step.
    Other,
    /// Step 2 for an IMAP account that needs to sign in again.
    Again(Account),
}

/// How the dialog ended, for the window to carry on from.
pub enum Done {
    /// The Google sign-in serves this, expecting the address typed, if any.
    Google(Option<String>),
    /// An IMAP account signed in, with the name the person gave for their
    /// mail, if any.
    Added {
        account: Account,
        name: Option<String>,
    },
    /// An IMAP account that needed to sign in again did.
    SignedInAgain(Account),
}

pub fn present(
    core: &Rc<Core>,
    parent: &impl IsA<gtk::Widget>,
    opening: Opening,
    done: impl Fn(Done) + 'static,
) {
    let again = match &opening {
        Opening::Again(account) => Some(account.clone()),
        _ => None,
    };
    let this = Dialog::new(core, again, Box::new(done));
    this.connect();
    // The first page added is where the dialog opens. The rest are added
    // too, so a page popped off the stack stays in the dialog: a page
    // left without a parent while a screen reader still holds its rows
    // makes GTK abort.
    let (again, root) = match opening {
        Opening::Pick => (None, this.picker()),
        Opening::Other => (None, this.first.page.clone()),
        Opening::Again(account) => (Some(account), this.second.page.clone()),
    };
    this.nav.add(&root);
    for page in [&this.first.page, &this.second.page, &this.manual.page] {
        if page != &root {
            this.nav.add(page);
        }
    }
    if let Some(account) = again {
        this.load_saved(account);
    }
    // The dialog owns the state behind its buttons until it closes, and
    // an answer that arrives after that has nowhere to go.
    let keep = RefCell::new(Some(Rc::clone(&this)));
    this.window.connect_closed(move |_| {
        if let Some(this) = keep.borrow_mut().take() {
            this.asking.close();
        }
    });
    this.window.present(Some(parent));
}

struct Dialog {
    core: Rc<Core>,
    window: adw::Dialog,
    nav: adw::NavigationView,
    asking: Asking,
    done: Box<dyn Fn(Done)>,
    /// The account signing in again, when that is why the dialog opened.
    again: Option<Account>,
    /// The address step 2 signs in as.
    typed: RefCell<Option<Address>>,
    /// What step 2 signs in to.
    proposal: RefCell<Option<Proposal>>,
    first: FirstStep,
    second: SecondStep,
    manual: ManualStep,
}

struct FirstStep {
    page: adw::NavigationPage,
    address: adw::EntryRow,
    look: gtk::Button,
    looking: gtk::Box,
    said: gtk::Label,
    servers: adw::ActionRow,
}

struct SecondStep {
    page: adw::NavigationPage,
    who: adw::PreferencesGroup,
    name: adw::EntryRow,
    password: adw::PasswordEntryRow,
    hint: Note,
    failed: Note,
    /// The two servers the password goes to, always on show, and the
    /// yes that servers Penguin Mail guessed wait for.
    hosts: adw::PreferencesGroup,
    incoming: adw::ActionRow,
    outgoing: adw::ActionRow,
    agree: adw::ActionRow,
    confirmed: gtk::CheckButton,
    sign_in: gtk::Button,
    servers: adw::ActionRow,
}

struct ManualStep {
    page: adw::NavigationPage,
    content: adw::PreferencesPage,
    imap: ServerRows,
    smtp: ServerRows,
    user: adw::EntryRow,
    problem: gtk::Label,
    use_them: gtk::Button,
}

/// The three rows Server Settings gives each server.
struct ServerRows {
    host: adw::EntryRow,
    port: adw::SpinRow,
    security: adw::ToggleGroup,
}

impl ServerRows {
    fn new(group: &adw::PreferencesGroup) -> ServerRows {
        let host = entry(&gettext("Server"));
        host.set_input_purpose(gtk::InputPurpose::Url);
        let port = adw::SpinRow::with_range(1.0, 65535.0, 1.0);
        port.set_title(&gettext("Port"));
        crate::ui::name(&port, &gettext("Port"));
        // Two choices side by side read at a glance where a drop-down
        // would hide one of them.
        let security = adw::ToggleGroup::builder()
            .valign(gtk::Align::Center)
            .build();
        for choice in add_account::SECURITIES {
            let label = add_account::security_label(choice);
            security.add(adw::Toggle::builder().label(label).name(label).build());
        }
        crate::ui::name(&security, &gettext("Security"));
        let security_row = adw::ActionRow::builder()
            .title(gettext("Security"))
            .build();
        security_row.add_suffix(&security);
        group.add(&host);
        group.add(&port);
        group.add(&security_row);
        ServerRows {
            host,
            port,
            security,
        }
    }

    fn fill(&self, server: &Server) {
        self.host.set_text(&server.host);
        self.port.set_value(f64::from(server.port));
        self.security
            .set_active(add_account::security_index(server.security));
    }

    fn typed(&self) -> Typed {
        Typed {
            host: self.host.text().to_string(),
            port: self.port.value() as u16,
            security: add_account::security_at(self.security.active()),
        }
    }
}

/// A line of text under a form, with the pages that help as links of
/// their own below it. Each link is a button a screen reader names by its
/// words, which a link inside a label's markup never gets.
struct Note {
    area: gtk::Box,
    line: gtk::Label,
    links: gtk::Box,
}

impl Note {
    fn new(class: Option<&str>) -> Note {
        let line = gtk::Label::builder().wrap(true).xalign(0.0).build();
        if let Some(class) = class {
            line.add_css_class(class);
        }
        let links = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        let area = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .margin_top(12)
            .visible(false)
            .css_classes(["account-note"])
            .build();
        area.append(&line);
        area.append(&links);
        Note { area, line, links }
    }

    fn show(&self, line: &str, links: &[add_account::Link]) {
        self.line.set_text(line);
        while let Some(child) = self.links.first_child() {
            self.links.remove(&child);
        }
        for link in links {
            let button = gtk::LinkButton::builder()
                .uri(&link.url)
                .label(&link.label)
                .halign(gtk::Align::Start)
                .build();
            self.links.append(&button);
        }
        self.area.set_visible(true);
    }

    fn hide(&self) {
        self.area.set_visible(false);
    }
}

/// An entry row, named for a screen reader by its title.
fn entry(title: &str) -> adw::EntryRow {
    let row = adw::EntryRow::builder().title(title).build();
    crate::ui::name(&row, title);
    row
}

/// A wrapping line of text, hidden until something fills it.
fn line(class: Option<&str>) -> gtk::Label {
    let label = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .visible(false)
        .margin_top(12)
        .build();
    if let Some(class) = class {
        label.add_css_class(class);
    }
    label
}

/// A page of the dialog: a header bar with `action` at its end, and
/// `content` below.
fn page(
    title: &str,
    tag: &str,
    content: &impl IsA<gtk::Widget>,
    action: Option<&gtk::Button>,
) -> adw::NavigationPage {
    let header = adw::HeaderBar::new();
    if let Some(action) = action {
        header.pack_end(action);
    }
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(content));
    adw::NavigationPage::builder()
        .title(title)
        .tag(tag)
        .child(&toolbar)
        .build()
}

fn suggested(label: &str) -> gtk::Button {
    gtk::Button::builder()
        .label(label)
        .css_classes(["suggested-action"])
        .build()
}

/// A row that opens Server Settings, one level deeper, the way the
/// picker's rows open their step.
fn server_settings_row() -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(gettext("Server Settings"))
        .activatable(true)
        .build();
    row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    row
}

impl Dialog {
    fn new(core: &Rc<Core>, again: Option<Account>, done: Box<dyn Fn(Done)>) -> Rc<Dialog> {
        let nav = adw::NavigationView::new();
        let window = adw::Dialog::builder()
            .title(gettext("Add Account"))
            .content_width(460)
            .content_height(560)
            .child(&nav)
            .build();
        Rc::new(Dialog {
            core: Rc::clone(core),
            window,
            nav,
            asking: Asking::default(),
            done,
            again,
            typed: RefCell::new(None),
            proposal: RefCell::new(None),
            first: first_step(),
            second: second_step(),
            manual: manual_step(),
        })
    }

    /// Wires each control to what it does. Every closure holds a weak
    /// reference to the dialog; `present` holds the strong one until the
    /// dialog closes.
    fn connect(self: &Rc<Self>) {
        let on = |run: fn(&Rc<Dialog>)| {
            let weak = Rc::downgrade(self);
            move || {
                if let Some(this) = weak.upgrade() {
                    run(&this);
                }
            }
        };
        let look = on(Dialog::look);
        self.first.look.connect_clicked(move |_| look());
        let look = on(Dialog::look);
        self.first.address.connect_entry_activated(move |_| look());
        let changed = on(Dialog::address_changed);
        self.first.address.connect_changed(move |_| changed());
        let manual = on(Dialog::manual_from_address);
        self.first.servers.connect_activated(move |_| manual());
        let manual = on(Dialog::manual_from_password);
        self.second.servers.connect_activated(move |_| manual());
        let sign_in = on(Dialog::sign_in);
        self.second.sign_in.connect_clicked(move |_| sign_in());
        let sign_in = on(Dialog::sign_in);
        self.second
            .password
            .connect_entry_activated(move |_| sign_in());
        let ready = on(Dialog::update_sign_in);
        self.second.password.connect_changed(move |_| ready());
        let ready = on(Dialog::update_sign_in);
        self.second.confirmed.connect_toggled(move |_| ready());
        let use_them = on(Dialog::use_manual);
        self.manual.use_them.connect_clicked(move |_| use_them());
        // Each step opens with the cursor in the field it asks for, so
        // the whole dialog runs from the keyboard.
        let address = self.first.address.clone();
        self.first
            .page
            .connect_shown(move |_| _ = address.grab_focus());
        let password = self.second.password.clone();
        self.second
            .page
            .connect_shown(move |_| _ = password.grab_focus());
        let host = self.manual.imap.host.clone();
        self.manual
            .page
            .connect_shown(move |_| _ = host.grab_focus());
    }

    fn picker(self: &Rc<Self>) -> adw::NavigationPage {
        let group = adw::PreferencesGroup::new();
        for choice in Choice::ALL {
            let row = adw::ActionRow::builder()
                .title(choice.title())
                .subtitle(choice.subtitle())
                .activatable(true)
                .build();
            row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            let weak = Rc::downgrade(self);
            row.connect_activated(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.chose(choice);
                }
            });
            group.add(&row);
        }
        let content = adw::PreferencesPage::new();
        content.add(&group);
        page(&gettext("Add Account"), "pick", &content, None)
    }

    fn chose(&self, choice: Choice) {
        match choice {
            Choice::Google => self.finish(Done::Google(None)),
            Choice::Other => self.nav.push(&self.first.page),
        }
    }

    /// Closes the dialog and hands the window what comes next.
    fn finish(&self, done: Done) {
        self.window.close();
        (self.done)(done);
    }

    /// Step 1's Continue: looks the address up, or says why it cannot.
    fn look(self: &Rc<Self>) {
        let Some(address) = Address::parse(&self.first.address.text()) else {
            return self.say(&add_account::not_an_address());
        };
        let ticket = self.asking.ask();
        self.first.said.set_visible(false);
        self.first.looking.set_visible(true);
        self.first.look.set_sensitive(false);
        self.typed.replace(Some(address.clone()));
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let found = this.core.discover(address.full()).await;
            // The person changed the address or closed the dialog while
            // this ran, and nobody is asking this question now.
            if !this.asking.wants(ticket) {
                return;
            }
            this.first.looking.set_visible(false);
            this.first.look.set_sensitive(true);
            let next = match found {
                Ok(found) => add_account::after_discovery(found, &address),
                Err(err) => Next::Say(err.to_string()),
            };
            match next {
                Next::Password(proposal) => this.show_password(proposal),
                Next::Say(said) => this.say(&said),
                Next::Google => this.finish(Done::Google(Some(address.full()))),
                Next::Manual { proposal, line } => this.show_manual(&proposal, Some(&line)),
            }
        });
    }

    /// Says why step 1 cannot go on, with the cursor back in the address
    /// to fix it. Continue lost the focus when it went insensitive.
    fn say(&self, said: &str) {
        self.first.said.set_text(said);
        self.first.said.set_visible(true);
        self.first.address.grab_focus();
    }

    /// Whatever step 1 said, or is still looking up, was about another
    /// address.
    fn address_changed(self: &Rc<Self>) {
        self.asking.forget();
        self.proposal.replace(None);
        self.first.said.set_visible(false);
        self.first.looking.set_visible(false);
        self.first.look.set_sensitive(true);
    }

    fn show_password(self: &Rc<Self>, proposal: Proposal) {
        let second = &self.second;
        second.page.set_title(&proposal.provider_name);
        let title = add_account::password_title(&proposal);
        second.password.set_title(&title);
        crate::ui::name(&second.password, &title);
        match add_account::password_hint(&proposal) {
            Some(hint) => second.hint.show(&hint.line, hint.link.as_slice()),
            None => second.hint.hide(),
        }
        second.failed.hide();
        self.show_hosts(&proposal);
        match &self.again {
            Some(account) => {
                second
                    .who
                    .set_description(Some(&add_account::again_line(account)));
                second.name.set_visible(false);
            }
            None => {
                let address = self.typed.borrow().as_ref().map(Address::full);
                second.who.set_description(address.as_deref());
            }
        }
        self.proposal.replace(Some(proposal));
        self.update_sign_in();
        let showing = self.nav.visible_page().and_then(|p| p.tag());
        if showing.as_deref() != Some("password") {
            self.nav.push(&second.page);
        }
    }

    /// Shows both servers the password is about to go to, and asks for a
    /// yes when Penguin Mail guessed them.
    fn show_hosts(&self, proposal: &Proposal) {
        let second = &self.second;
        second
            .incoming
            .set_subtitle(&add_account::server_line(&proposal.imap));
        second
            .outgoing
            .set_subtitle(&add_account::server_line(&proposal.smtp));
        second.confirmed.set_active(false);
        second.agree.set_visible(proposal.confirm);
        if proposal.confirm {
            second.hosts.set_title(&gettext("Use These Servers?"));
            second.hosts.set_description(Some(&gettext(
                "Penguin Mail guessed these servers. Your password goes to them only after you check the box.",
            )));
        } else {
            second.hosts.set_title(&gettext("Servers"));
            second.hosts.set_description(None);
        }
    }

    fn update_sign_in(self: &Rc<Self>) {
        let ready = self.proposal.borrow().as_ref().is_some_and(|proposal| {
            add_account::can_sign_in(
                &self.second.password.text(),
                proposal,
                self.second.confirmed.is_active(),
            )
        });
        self.second.sign_in.set_sensitive(ready);
    }

    /// Sign In: tries the servers on show, then discovery's other
    /// candidates while the failure is one that says nothing about the
    /// password. A candidate that needs a yes stops the run and waits for
    /// it.
    fn sign_in(self: &Rc<Self>) {
        let address = self.typed.borrow().clone();
        let proposal = self.proposal.borrow().clone();
        let (Some(address), Some(mut proposal)) = (address, proposal) else {
            return;
        };
        let password = self.second.password.text().to_string();
        if !add_account::can_sign_in(&password, &proposal, self.second.confirmed.is_active()) {
            return;
        }
        let name = Some(self.second.name.text().trim().to_string())
            .filter(|name| !name.is_empty() && self.again.is_none());
        let ticket = self.asking.ask();
        self.second.failed.hide();
        self.second.sign_in.set_sensitive(false);
        self.second.sign_in.set_label(&gettext("Signing In…"));
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            loop {
                let attempt = add_account::attempt(&address, &proposal, &password);
                let signed = this.core.sign_in_imap(attempt).await;
                let err = match signed {
                    // The account is kept whether or not the dialog is
                    // still open, so the window hears about it either way.
                    Ok(account) => {
                        this.second.sign_in.set_label(&gettext("Sign In"));
                        return this.finish(match this.again {
                            Some(_) => Done::SignedInAgain(account),
                            None => Done::Added { account, name },
                        });
                    }
                    Err(err) => err,
                };
                if !this.asking.wants(ticket) {
                    tracing::info!(error = %err, "a sign-in failed after its dialog moved on");
                    return;
                }
                match add_account::after_failure(&err, &proposal, &address) {
                    Outcome::TryNext(next) if !next.confirm => {
                        this.show_hosts(&next);
                        this.proposal.replace(Some(next.clone()));
                        proposal = next;
                    }
                    Outcome::TryNext(next) => {
                        // Say why the first servers failed above the
                        // guessed ones that now wait for a yes.
                        let failure = add_account::failure(&err, &proposal);
                        this.show_password(next);
                        this.show_failure(&failure);
                        return;
                    }
                    Outcome::Failed(failure) => {
                        this.show_failure(&failure);
                        return;
                    }
                }
            }
        });
    }

    fn show_failure(self: &Rc<Self>, failure: &add_account::Failure) {
        let second = &self.second;
        second.sign_in.set_label(&gettext("Sign In"));
        // A failure with pages to try carries the app password page
        // itself where one is due, and two lines offering it would be one
        // too many.
        if !failure.links.is_empty() {
            second.hint.hide();
        }
        second.failed.show(&failure.line, &failure.links);
        // Sign In took the focus with it when it went insensitive.
        second.password.grab_focus();
        self.update_sign_in();
    }

    /// Server Settings from step 1, filled from what was found for this
    /// address or with a guess.
    fn manual_from_address(self: &Rc<Self>) {
        let Some(address) = Address::parse(&self.first.address.text()) else {
            return self.say(&add_account::not_an_address());
        };
        // A lookup still running would take the person away from the
        // form they asked for.
        self.asking.forget();
        self.first.looking.set_visible(false);
        self.first.look.set_sensitive(true);
        let same = self.typed.borrow().as_ref() == Some(&address);
        let found = self.proposal.borrow().clone().filter(|_| same);
        let proposal = found.unwrap_or_else(|| add_account::guess(&address));
        self.typed.replace(Some(address));
        self.show_manual(&proposal, None);
    }

    fn manual_from_password(self: &Rc<Self>) {
        let proposal = self.proposal.borrow().clone();
        if let Some(proposal) = proposal {
            self.show_manual(&proposal, None);
        }
    }

    fn show_manual(self: &Rc<Self>, proposal: &Proposal, said: Option<&str>) {
        let manual = &self.manual;
        manual.content.set_description(said.unwrap_or_default());
        manual.imap.fill(&proposal.imap);
        manual.smtp.fill(&proposal.smtp);
        manual
            .user
            .set_text(proposal.user.as_deref().unwrap_or_default());
        manual.problem.set_visible(false);
        self.proposal.replace(Some(proposal.clone()));
        self.nav.push(&manual.page);
    }

    fn use_manual(self: &Rc<Self>) {
        let before = self.proposal.borrow().clone();
        let Some(before) = before else { return };
        let typed = add_account::typed_servers(
            &self.manual.imap.typed(),
            &self.manual.smtp.typed(),
            &self.manual.user.text(),
            &before,
        );
        match typed {
            Ok(proposal) => {
                if !self.nav.pop_to_tag("password") {
                    self.nav.pop();
                }
                self.show_password(proposal);
            }
            Err(problem) => {
                self.manual.problem.set_text(&problem);
                self.manual.problem.set_visible(true);
            }
        }
    }

    /// Step 2 for an account signing in again, from the servers it kept.
    /// An account whose servers are gone starts again from Server
    /// Settings.
    fn load_saved(self: &Rc<Self>, account: Account) {
        self.typed.replace(Address::parse(&account.email));
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let id = account.id;
            let saved = this.core.read(move |c| servers::load(c, id)).await;
            match saved {
                Ok(Some(saved)) => {
                    this.show_password(add_account::saved_proposal(&account, &saved))
                }
                Ok(None) => {
                    let typed = this.typed.borrow().clone();
                    if let Some(address) = typed {
                        this.show_password(add_account::guess(&address));
                        this.show_manual(&add_account::guess(&address), None);
                    }
                }
                Err(err) => this.second.failed.show(&err.to_string(), &[]),
            }
        });
    }
}

fn first_step() -> FirstStep {
    let address = entry(&gettext("Email Address"));
    address.set_input_purpose(gtk::InputPurpose::Email);
    let looking = gtk::Box::builder()
        .spacing(8)
        .margin_top(12)
        .visible(false)
        .build();
    looking.append(&adw::Spinner::new());
    let looking_line = gtk::Label::builder()
        .label(gettext("Looking for your mail servers…"))
        .css_classes(["dim-label"])
        .build();
    looking.append(&looking_line);
    let said = line(None);
    let group = adw::PreferencesGroup::new();
    group.add(&address);
    group.add(&looking);
    group.add(&said);
    let servers = server_settings_row();
    let more = adw::PreferencesGroup::new();
    more.add(&servers);
    let content = adw::PreferencesPage::new();
    content.add(&group);
    content.add(&more);
    let look = suggested(&gettext("Continue"));
    FirstStep {
        page: page(
            &gettext("Another Provider"),
            "address",
            &content,
            Some(&look),
        ),
        address,
        look,
        looking,
        said,
        servers,
    }
}

fn second_step() -> SecondStep {
    let name = entry(&gettext("Your Name"));
    name.set_input_purpose(gtk::InputPurpose::Name);
    let password = adw::PasswordEntryRow::builder()
        .title(gettext("Password"))
        .build();
    crate::ui::name(&password, &gettext("Password"));
    let hint = Note::new(Some("dim-label"));
    let failed = Note::new(Some("error"));
    let who = adw::PreferencesGroup::new();
    who.add(&name);
    who.add(&password);
    who.add(&hint.area);
    who.add(&failed.area);
    let incoming = adw::ActionRow::builder()
        .title(gettext("Incoming Mail"))
        .subtitle_selectable(true)
        .build();
    let outgoing = adw::ActionRow::builder()
        .title(gettext("Outgoing Mail"))
        .subtitle_selectable(true)
        .build();
    let confirmed = gtk::CheckButton::builder()
        .valign(gtk::Align::Center)
        .build();
    crate::ui::name(&confirmed, &gettext("Use these servers"));
    let agree = adw::ActionRow::builder()
        .title(gettext("Use these servers"))
        .activatable_widget(&confirmed)
        .visible(false)
        .build();
    agree.add_prefix(&confirmed);
    let servers = server_settings_row();
    let hosts = adw::PreferencesGroup::builder()
        .title(gettext("Servers"))
        .build();
    hosts.add(&incoming);
    hosts.add(&outgoing);
    hosts.add(&agree);
    hosts.add(&servers);
    let content = adw::PreferencesPage::new();
    content.add(&who);
    content.add(&hosts);
    let sign_in = suggested(&gettext("Sign In"));
    sign_in.set_sensitive(false);
    SecondStep {
        page: page("", "password", &content, Some(&sign_in)),
        who,
        name,
        password,
        hint,
        failed,
        hosts,
        incoming,
        outgoing,
        agree,
        confirmed,
        sign_in,
        servers,
    }
}

fn manual_step() -> ManualStep {
    let incoming = adw::PreferencesGroup::builder()
        .title(gettext("Incoming Mail"))
        .build();
    let imap = ServerRows::new(&incoming);
    let outgoing = adw::PreferencesGroup::builder()
        .title(gettext("Outgoing Mail"))
        .build();
    let smtp = ServerRows::new(&outgoing);
    let user = entry(&gettext("User Name"));
    let problem = line(Some("error"));
    let login = adw::PreferencesGroup::builder()
        .description(gettext("Leave it empty to sign in with your address."))
        .build();
    login.add(&user);
    login.add(&problem);
    let content = adw::PreferencesPage::new();
    content.add(&incoming);
    content.add(&outgoing);
    content.add(&login);
    let use_them = suggested(&gettext("Use These Settings"));
    ManualStep {
        page: page(
            &gettext("Server Settings"),
            "servers",
            &content,
            Some(&use_them),
        ),
        content,
        imap,
        smtp,
        user,
        problem,
        use_them,
    }
}
