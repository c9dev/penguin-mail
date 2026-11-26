//! Switch to JMAP: says what the switch changes and what it keeps, asks
//! for what the JMAP sign-in needs, and runs the switch. The account stays
//! where it is, with its name and colour. A server that takes a sign-in
//! on its own page also gets Sign In With Browser, which hands the switch
//! a grant in place of a token. The words come from `crate::offered`; the
//! work happens in `Core::switch_grant` and `Core::switch_to_jmap`.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use mailrs_discover::JmapFound;
use mailrs_domain::Account;
use mailrs_domain::translate::{fill, gettext};
use mailrs_sync::switch::{SwitchSignIn, SwitchTo, Switched};

use crate::add_account::{self, Address, JmapProposal};
use crate::core::Core;
use crate::offered::{self, SwitchAsked, SwitchRow};

/// The page's side margin, and the narrower one under 400 px.
const SIDE: i32 = 20;
const NARROW_SIDE: i32 = 14;
/// The gap between the page's groups.
const SPACING: i32 = 18;
/// The room under the page, before any toast adds its own.
const PAGE_FOOT: i32 = 22;

struct Dialog {
    core: Rc<Core>,
    account: Account,
    proposal: JmapProposal,
    token: bool,
    dialog: adw::Dialog,
    toasts: adw::ToastOverlay,
    secret: adw::PasswordEntryRow,
    switch: gtk::Button,
    /// Sign In With Browser and its line, shown once the server says it
    /// takes a sign-in on its own page.
    browser_offer: gtk::Box,
    browser: gtk::Button,
    /// Closes the dialog; off while the switch runs.
    cancel: gtk::Button,
    progress: gtk::Box,
    /// Open Page Again and Cancel, while the browser has the sign-in.
    waiting: gtk::Box,
    /// The page the browser sign-in opened, for Open Page Again.
    browser_url: RefCell<Option<String>>,
    /// Dropped to end the wait for the browser.
    browser_cancel: RefCell<Option<async_channel::Sender<()>>>,
    header: adw::HeaderBar,
    scrolled: gtk::ScrolledWindow,
    page: gtk::Box,
    /// The failure toast on show, which a new try takes away.
    failure: std::cell::RefCell<Option<adw::Toast>>,
    done: Box<dyn Fn(Switched)>,
}

/// Shows Switch to JMAP for `account` over the session `found`.
/// `calendars` says the account's calendars sign in with the password a
/// token replaces for mail. `done` runs once the switch went through, after
/// the dialog closes.
pub fn present(
    parent: &impl IsA<gtk::Widget>,
    core: Rc<Core>,
    account: Account,
    found: JmapFound,
    calendars: bool,
    done: impl Fn(Switched) + 'static,
) {
    let Some(address) = Address::parse(&account.email) else {
        return;
    };
    let preview = core
        .demo
        .then(|| std::env::var("MAILRS_DEMO_SWITCH").ok())
        .flatten();
    let found = match preview.as_deref() {
        // A server that takes a password, which no demo account has.
        Some("password") => JmapFound {
            provider: None,
            session_url: "https://mail.home.example/jmap/session".into(),
            ..found
        },
        _ => found,
    };
    let proposal = add_account::jmap_proposal(found, None, &address);
    let token = add_account::token_only(&proposal);
    let rows = offered::switch_rows(&account, calendars, token);

    let page = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(SPACING)
        .margin_top(14)
        .margin_bottom(PAGE_FOOT)
        .margin_start(SIDE)
        .margin_end(SIDE)
        .build();
    page.append(&info_group(&gettext("What changes"), &rows.changes));
    page.append(&info_group(&gettext("What stays"), &rows.stays));

    let title = add_account::jmap_secret_title(token);
    let secret = adw::PasswordEntryRow::builder().title(&title).build();
    // Added, not set: a builder's classes replace the row's own `entry`.
    secret.add_css_class("switch-entry");
    crate::ui::name(&secret, &title);
    let sent_to = gtk::Label::builder()
        .label(fill(
            &gettext("Sent to {host} and nowhere else."),
            &[("host", &add_account::host_of(&proposal.session_url))],
        ))
        .wrap(true)
        .xalign(0.0)
        .css_classes(["dim-label", "switch-sent-to"])
        .build();
    let entry = adw::PreferencesGroup::new();
    entry.add(&secret);
    let browser = gtk::Button::builder()
        .child(
            &adw::ButtonContent::builder()
                .icon_name("penguin-mail-globe-symbolic")
                .label(gettext("Sign In With Browser"))
                .build(),
        )
        .css_classes(["pill", "switch-browser"])
        .build();
    crate::ui::name(&browser, &gettext("Sign In With Browser"));
    let browser_line = gtk::Label::builder()
        .label(add_account::browser_line(&proposal))
        .wrap(true)
        .justify(gtk::Justification::Center)
        .css_classes(["dim-label", "switch-browser-line"])
        .build();
    browser.update_property(&[gtk::accessible::Property::Description(
        &browser_line.label(),
    )]);
    let browser_offer = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .visible(false)
        .build();
    browser_offer.append(&browser);
    browser_offer.append(&browser_line);
    page.append(&browser_offer);
    let secret_area = gtk::Box::new(gtk::Orientation::Vertical, 0);
    secret_area.append(&sent_to);
    secret_area.append(&entry);
    page.append(&secret_area);
    if let Some((heading, body, link)) = add_account::jmap_hint(&proposal, token) {
        page.append(&help_card(&heading, &body, link.as_ref()));
    }
    let progress = gtk::Box::builder()
        .spacing(10)
        .halign(gtk::Align::Center)
        .visible(false)
        .css_classes(["switch-progress"])
        .build();
    progress.append(
        &adw::Spinner::builder()
            .width_request(24)
            .height_request(24)
            .build(),
    );
    progress.append(&gtk::Label::new(Some(&gettext("Matching your mail…"))));
    page.append(&progress);
    // The wait for the browser, in the button's place: a spinner and its
    // line, with Open Page Again and Cancel under them.
    let again = gtk::Button::builder()
        .label(gettext("Open Page Again"))
        .css_classes(["pill"])
        .build();
    let stop_wait = gtk::Button::builder()
        .label(gettext("Cancel"))
        .css_classes(["pill"])
        .build();
    let buttons = gtk::Box::builder()
        .spacing(12)
        .halign(gtk::Align::Center)
        .build();
    buttons.append(&again);
    buttons.append(&stop_wait);
    let wait_line = gtk::Box::builder()
        .spacing(10)
        .halign(gtk::Align::Center)
        .css_classes(["switch-progress"])
        .build();
    wait_line.append(
        &adw::Spinner::builder()
            .width_request(24)
            .height_request(24)
            .build(),
    );
    let provider = add_account::post::short_name(&proposal.provider_name);
    wait_line.append(
        &gtk::Label::builder()
            .label(fill(
                &gettext("Finish signing in on {provider}'s page in your browser."),
                &[("provider", &provider)],
            ))
            .wrap(true)
            .justify(gtk::Justification::Center)
            .build(),
    );
    let waiting = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(14)
        .visible(false)
        .css_classes(["switch-waiting"])
        .build();
    waiting.append(&wait_line);
    waiting.append(&buttons);
    page.insert_child_after(&waiting, Some(&browser_offer));

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .child(&page)
        .build();
    let switch = gtk::Button::builder()
        .label(gettext("Switch"))
        .css_classes(["suggested-action"])
        .sensitive(false)
        .build();
    let cancel = gtk::Button::with_label(&gettext("Cancel"));
    let header = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .build();
    header.pack_start(&cancel);
    header.pack_end(&switch);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&scrolled));
    let toasts = adw::ToastOverlay::new();
    toasts.add_css_class("switch-toasts");
    toasts.set_child(Some(&toolbar));
    let dialog = adw::Dialog::builder()
        .title(gettext("Switch to JMAP"))
        .content_width(480)
        .child(&toasts)
        .css_classes(["switch-jmap"])
        .build();
    if let Ok(condition) = adw::BreakpointCondition::parse("max-width: 400px") {
        let narrow = adw::Breakpoint::new(condition);
        narrow.add_setters(&[
            (&page, "margin-start", NARROW_SIDE),
            (&page, "margin-end", NARROW_SIDE),
        ]);
        dialog.add_breakpoint(narrow);
    }

    let this = Rc::new(Dialog {
        core,
        account,
        proposal,
        token,
        dialog: dialog.clone(),
        toasts,
        secret: secret.clone(),
        switch: switch.clone(),
        cancel: cancel.clone(),
        browser_offer,
        browser: browser.clone(),
        progress,
        waiting,
        browser_url: RefCell::default(),
        browser_cancel: RefCell::default(),
        header,
        scrolled,
        page,
        failure: std::cell::RefCell::default(),
        done: Box::new(done),
    });
    // Cancel does what Escape does: it closes the dialog, which the
    // switch keeps open while it runs.
    let closer = dialog.downgrade();
    cancel.connect_clicked(move |_| {
        if let Some(dialog) = closer.upgrade() {
            dialog.close();
        }
    });
    let ready = switch.clone();
    secret.connect_changed(move |row| ready.set_sensitive(!row.text().is_empty()));
    let weak = Rc::downgrade(&this);
    switch.connect_clicked(move |_| {
        if let Some(this) = weak.upgrade() {
            this.start();
        }
    });
    let weak = Rc::downgrade(&this);
    secret.connect_entry_activated(move |_| {
        if let Some(this) = weak.upgrade()
            && this.switch.is_sensitive()
        {
            this.start();
        }
    });
    let weak = Rc::downgrade(&this);
    browser.connect_clicked(move |_| {
        if let Some(this) = weak.upgrade() {
            this.start_browser();
        }
    });
    let weak = Rc::downgrade(&this);
    again.connect_clicked(move |_| {
        if let Some(this) = weak.upgrade() {
            this.open_page();
        }
    });
    // The Cancel under the spinner ends the wait for the browser and
    // leaves the dialog open; the header's Cancel closes it.
    let weak = Rc::downgrade(&this);
    stop_wait.connect_clicked(move |_| {
        if let Some(this) = weak.upgrade() {
            this.browser_cancel.borrow_mut().take();
        }
    });
    // The dialog owns the state behind its controls until it closes, and
    // closing it ends a wait for the browser.
    let keep = std::cell::RefCell::new(Some(Rc::clone(&this)));
    dialog.connect_closed(move |_| {
        if let Some(this) = keep.borrow_mut().take() {
            this.browser_cancel.borrow_mut().take();
        }
    });
    // Sign In With Browser shows once the server says it takes one.
    let (weak, offer) = (
        Rc::downgrade(&this),
        add_account::jmap_browser(&this.proposal),
    );
    let previewed = preview
        .as_deref()
        .is_some_and(|stage| stage.starts_with("browser") || stage == "other-account");
    glib::spawn_future_local(async move {
        let Some(core) = weak.upgrade().map(|this| Rc::clone(&this.core)) else {
            return;
        };
        let ready = previewed || core.jmap_browser_ready(offer).await;
        if let Some(this) = weak.upgrade()
            && ready
        {
            this.browser_offer.set_visible(true);
            this.fit();
        }
    });
    dialog.present(Some(parent));
    // Once the page has its width, so the wrapped rows count.
    let weak = Rc::downgrade(&this);
    let mut tries = 0;
    glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
        tries += 1;
        match weak.upgrade() {
            Some(this) if !this.fit() && tries < 40 => glib::ControlFlow::Continue,
            _ => glib::ControlFlow::Break,
        }
    });
    // Nothing has focus until the person picks the field, as the mockup
    // draws it; Tab reaches the field first.
    let typed = |stage: &str| stage != "offer" && !stage.starts_with("browser");
    if preview.as_deref().is_none_or(|stage| !typed(stage)) {
        dialog.set_focus(None::<&gtk::Widget>);
    }
    if let Some(stage) = preview {
        if typed(&stage) {
            this.secret
                .set_text(&"x".repeat(if this.token { 24 } else { 12 }));
            this.secret.grab_focus();
        }
        // After the dialog has mapped and settled its own focus.
        glib::timeout_add_local_once(std::time::Duration::from_millis(600), move || {
            this.preview(&stage)
        });
    }
}

/// A titled group of icon rows that only inform: nothing in them opens.
fn info_group(title: &str, rows: &[SwitchRow]) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title(title).build();
    for row in rows {
        let shown = crate::ui::plain_row().build();
        shown.add_css_class("switch-info");
        shown.set_title(&row.title);
        shown.set_subtitle(&row.subtitle);
        shown.add_prefix(
            &gtk::Image::builder()
                .icon_name(row.icon)
                .css_classes(["dim-label"])
                .build(),
        );
        group.add(&shown);
    }
    group
}

/// The provider's advice on making a token, in a tinted card, with the
/// page that explains it.
fn help_card(heading: &str, body: &str, link: Option<&add_account::Link>) -> gtk::Box {
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["switch-help"])
        .build();
    let label = |text: &str, classes: &[&str]| {
        gtk::Label::builder()
            .label(text)
            .wrap(true)
            .xalign(0.0)
            .css_classes(classes.to_vec())
            .build()
    };
    card.append(&label(heading, &["switch-help-title"]));
    card.append(&label(body, &["switch-help-body", "dim-label"]));
    if let Some(link) = link {
        let content = adw::ButtonContent::builder()
            .icon_name("penguin-mail-external-link-symbolic")
            .label(&link.label)
            .build();
        let button = gtk::LinkButton::builder()
            .uri(&link.url)
            .halign(gtk::Align::Start)
            .css_classes(["switch-help-link"])
            .build();
        button.set_child(Some(&content));
        crate::ui::name(&button, &link.label);
        card.append(&button);
    }
    card
}

impl Dialog {
    /// Dims the entry and Switch while the switch runs, with the spinner
    /// under them, and lights them again after it.
    fn running(&self, on: bool) {
        self.secret.set_sensitive(!on);
        self.switch
            .set_sensitive(!on && !self.secret.text().is_empty());
        self.browser.set_sensitive(!on);
        self.cancel.set_sensitive(!on);
        self.progress.set_visible(on);
        // Closing now would leave a failure with nowhere to show.
        self.dialog.set_can_close(!on);
        self.fit();
    }

    /// Sizes the dialog to its page at the width it has. A dialog keeps
    /// the height it opened with, and asks its scrolled page for a height
    /// at its natural width: at 360 px the rows wrap to more lines than
    /// that, and the spinner's line comes later, so both would scroll out
    /// of sight.
    /// Answers whether the page had a width to measure at.
    fn fit(&self) -> bool {
        let width = self.scrolled.width();
        if width <= 0 || !self.scrolled.is_mapped() {
            return false;
        }
        // The page's foot margin holds the toast's room (`make_toast_room`),
        // and a measure counts margins.
        let page = self.page.measure(gtk::Orientation::Vertical, width).1;
        self.dialog.set_content_height(self.header.height() + page);
        true
    }

    /// Puts the cursor at the end of the secret, with nothing selected,
    /// so a fix to a pasted token does not start by replacing it.
    fn focus_secret(&self) {
        self.secret.grab_focus();
        // The entry selects its text as it takes focus, once this returns.
        let secret = self.secret.clone();
        glib::idle_add_local_once(move || {
            let end = secret.text().chars().count() as i32;
            secret.select_region(end, end);
        });
    }

    /// Takes away a failure toast on show.
    fn clear_failure(&self) {
        // Taken out before the dismiss, whose signal could reach back here.
        let shown = self.failure.borrow_mut().take();
        if let Some(toast) = shown {
            toast.dismiss();
        }
    }

    /// What a failure's words name.
    fn asked(&self) -> SwitchAsked<'_> {
        SwitchAsked {
            provider: &self.proposal.provider_name,
            address: &self.account.email,
            token: self.token,
            browser: self.browser_offer.is_visible(),
        }
    }

    fn start(self: &Rc<Self>) {
        if self.secret.text().is_empty() {
            return;
        }
        self.clear_failure();
        let to = SwitchTo {
            session_url: self.proposal.session_url.clone(),
            token: self.token,
            user: self.account.email.clone(),
            secret: self.secret.text().to_string(),
        };
        self.switch_with(SwitchSignIn::Typed(to));
    }

    fn switch_with(self: &Rc<Self>, how: SwitchSignIn) {
        self.running(true);
        self.secret.upcast_ref::<gtk::Widget>().announce(
            &gettext("Matching your mail…"),
            gtk::AccessibleAnnouncementPriority::Medium,
        );
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            match this.core.switch_to_jmap(this.account.clone(), how).await {
                Ok(switched) => {
                    this.running(false);
                    this.dialog.close();
                    (this.done)(switched);
                }
                Err(err) => {
                    this.running(false);
                    let failed = offered::switch_failure(&err, &this.asked());
                    this.fail(&failed.line, failed.again);
                }
            }
        });
    }

    /// The wait for the server's own page, in the button's place: the
    /// spinner with its line, and Open Page Again and Cancel under it,
    /// which take the focus the button had. The field and Switch dim; the
    /// dialog can close, which ends the wait.
    fn waiting_for_browser(&self, on: bool) {
        self.secret.set_sensitive(!on);
        self.switch
            .set_sensitive(!on && !self.secret.text().is_empty());
        self.browser.set_sensitive(!on);
        self.browser_offer.set_visible(!on);
        self.waiting.set_visible(on);
        if on {
            self.waiting.child_focus(gtk::DirectionType::TabForward);
        }
        self.fit();
    }

    /// Sign In With Browser: the server's own page, then the switch with
    /// the grant it hands back. The account syncs over IMAP meanwhile.
    fn start_browser(self: &Rc<Self>) {
        if self.browser_cancel.borrow().is_some() {
            return;
        }
        self.clear_failure();
        self.waiting_for_browser(true);
        let provider = add_account::post::short_name(&self.proposal.provider_name);
        let line = fill(
            &gettext("Finish signing in on {provider}'s page in your browser."),
            &[("provider", &provider)],
        );
        self.browser
            .upcast_ref::<gtk::Widget>()
            .announce(&line, gtk::AccessibleAnnouncementPriority::Medium);
        let (urls, opened) = async_channel::unbounded::<String>();
        let (cancel, canceled) = async_channel::bounded::<()>(1);
        self.browser_cancel.replace(Some(cancel));
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            while let Ok(url) = opened.recv().await {
                let Some(this) = weak.upgrade() else { return };
                this.browser_url.replace(Some(url));
                this.open_page();
            }
        });
        let this = Rc::clone(self);
        let offer = add_account::jmap_browser(&self.proposal);
        glib::spawn_future_local(async move {
            let granted = this
                .core
                .switch_grant(offer, this.account.email.clone(), urls, canceled)
                .await;
            this.browser_cancel.borrow_mut().take();
            this.browser_url.replace(None);
            this.waiting_for_browser(false);
            match granted {
                Ok(grant) => {
                    let session_url = this.proposal.session_url.clone();
                    this.switch_with(SwitchSignIn::Browser { session_url, grant });
                }
                Err(err) => match offered::switch_browser_failure(&err, &provider) {
                    Some(failed) => this.fail(&failed.line, failed.again),
                    None => {
                        this.browser.grab_focus();
                    }
                },
            }
        });
    }

    fn open_page(&self) {
        if let Some(url) = self.browser_url.borrow().clone() {
            gtk::UriLauncher::new(&url).launch(
                self.dialog.root().and_downcast::<gtk::Window>().as_ref(),
                gio::Cancellable::NONE,
                |_| {},
            );
        }
    }

    /// Keeps `room` pixels free under the page for the toast, so it covers
    /// nothing, as in the mockup. The dialog
    /// grows by that much where the window lets it; where it cannot, the
    /// page still scrolls far enough that the toast covers none of it.
    fn make_toast_room(&self, room: i32) {
        self.page.set_margin_bottom(PAGE_FOOT + room);
    }

    /// The failure toast. When the server's mail kept changing, it offers
    /// another try with what the person typed.
    fn fail(self: &Rc<Self>, line: &str, again: bool) {
        // A toast's own title keeps to one line and cuts the reason off;
        // this one wraps, as the mockup's three-line toast does. The toast
        // sizes itself to the label's natural width, so that width has to
        // fit the dialog: its margins and the close button take about 100
        // px, Try Again about 150 more, and a character about 7.5.
        let around = if again { 250 } else { 100 };
        let chars = ((self.toasts.width() - around) as f32 / 7.5).clamp(16.0, 60.0) as i32;
        let words = gtk::Label::builder()
            .label(line)
            .wrap(true)
            .max_width_chars(chars)
            .xalign(0.0)
            .css_classes(["switch-toast"])
            .build();
        let toast = adw::Toast::builder()
            .custom_title(&words)
            .timeout(0)
            .build();
        if again {
            toast.set_button_label(Some(&gettext("Try Again")));
            let weak = Rc::downgrade(self);
            toast.connect_button_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.start();
                }
            });
        }
        // The toast is its words, 10 px above and below them, at least 46
        // px, and 8 px from the page and 18 from the foot.
        let words_height = words.measure(gtk::Orientation::Vertical, chars * 15 / 2).1;
        self.make_toast_room((words_height + 20).max(46) + 26);
        let weak = Rc::downgrade(self);
        toast.connect_dismissed(move |_| {
            if let Some(this) = weak.upgrade() {
                this.make_toast_room(0);
                this.fit();
            }
        });
        self.toasts.add_toast(toast.clone());
        self.failure.replace(Some(toast));
        self.fit();
        // Where the dialog could not grow, scroll the room into view, so
        // the token card and its link sit above the toast, not under it.
        let scrolled = self.scrolled.clone();
        glib::idle_add_local_once(move || {
            let adjustment = scrolled.vadjustment();
            adjustment.set_value(adjustment.upper() - adjustment.page_size());
        });
        self.secret
            .upcast_ref::<gtk::Widget>()
            .announce(line, gtk::AccessibleAnnouncementPriority::High);
        self.focus_secret();
    }

    /// The demo's screenshot stages, from `MAILRS_DEMO_SWITCH`: `token`
    /// and `password` with a secret typed, `progress`, `failed` (the
    /// demo refuses every switch), `changed`, `done` or `done-rules`,
    /// which close the dialog as a switch would, `rules-off`, which opens
    /// the Rules with a rule the switch turned off, and `rules-here`, with
    /// a rule kept running here beside a server's script. `browser` shows
    /// Sign In With Browser, `browser-waiting` the wait for the browser,
    /// and `browser-failed` and `other-account` their failure toasts.
    fn preview(self: &Rc<Self>, stage: &str) {
        if stage != "offer" && !stage.starts_with("browser") {
            // The entry selects its text when the window first takes focus.
            let secret = self.secret.clone();
            glib::timeout_add_local_once(std::time::Duration::from_secs(3), move || {
                let end = secret.text().chars().count() as i32;
                secret.select_region(end, end);
            });
        }
        match stage {
            "progress" => self.running(true),
            "failed" => self.start(),
            "changed" => {
                let changed = anyhow::Error::new(mailrs_sync::switch::SwitchError::Sync(
                    mailrs_sync::SyncError::Backend(mailrs_sync::BackendError::Changed),
                ));
                let failed = offered::switch_failure(&changed, &self.asked());
                self.fail(&failed.line, failed.again);
            }
            "browser-waiting" => self.waiting_for_browser(true),
            "browser-failed" => {
                let refused = anyhow::Error::new(mailrs_sync::sign_in::BrowserSignInError::Jmap(
                    mailrs_jmap::JmapError::Unauthorized,
                ));
                let provider = add_account::post::short_name(&self.proposal.provider_name);
                if let Some(failed) = offered::switch_browser_failure(&refused, &provider) {
                    self.fail(&failed.line, failed.again);
                }
            }
            "other-account" => {
                let other = anyhow::Error::new(mailrs_sync::switch::SwitchError::OtherAccount);
                let failed = offered::switch_failure(&other, &self.asked());
                self.fail(&failed.line, failed.again);
            }
            "done" | "done-rules" => {
                self.dialog.close();
                let rules_off = if stage == "done-rules" { 2 } else { 0 };
                (self.done)(Switched {
                    account: self.account.clone(),
                    kept: 0,
                    dropped: 0,
                    rules_off,
                });
            }
            // A rule the switch turned off, or one it left running here
            // beside the server's script, as the Rules dialog lists it.
            "rules-off" | "rules-here" => {
                let off = stage == "rules-off";
                let (this, id) = (Rc::clone(self), self.account.id);
                glib::spawn_future_local(async move {
                    let rule = mailrs_domain::Filter {
                        id: Some("local-demo-allotment".into()),
                        criteria: mailrs_domain::FilterCriteria {
                            subject: Some("allotment".into()),
                            ..Default::default()
                        },
                        action: mailrs_domain::FilterAction {
                            add: vec![mailrs_domain::MailSet::Mailbox("Projects/Allotment".into())],
                            ..Default::default()
                        },
                        off,
                        ..Default::default()
                    };
                    if this
                        .core
                        .db
                        .write(move |c| mailrs_store::local_rules::add(c, id, &rule))
                        .await
                        .is_ok()
                    {
                        let _ = this
                            .dialog
                            .activate_action("win.account-rules", Some(&id.to_variant()));
                        this.dialog.close();
                    }
                });
            }
            _ => {}
        }
    }
}
