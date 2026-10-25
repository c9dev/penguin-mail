//! The pages that walk a person through making their own Google project,
//! hosted in Add Account's navigation view.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use mailrs_domain::translate::gettext;
use mailrs_sync::sign_in::own_client::{FileProblem, OwnClient, from_pasted};

use super::add_account::{banded, heading, label};
use crate::core::Core;
use crate::own_project::{self, Step};
use crate::route_choice::Platform;

/// What the pages ask of the dialog that hosts them.
pub struct Hooks {
    /// Signs in through the client the person gave.
    pub sign_in: Box<dyn Fn(OwnClient)>,
    /// Opens a page in the browser.
    pub open_uri: Box<dyn Fn(&str)>,
    /// Leaves the setup for the other way to sign in.
    pub switch: Box<dyn Fn()>,
}

/// What every page of the setup says about the other way to sign in: a
/// button that leads to it, or for an account that signs in again, the
/// line that says how to change the way it signs in.
struct Other {
    row: gtk::Box,
    switch: gtk::Button,
    line: gtk::Label,
}

impl Other {
    fn new() -> Other {
        let words = switch_words();
        let switch = gtk::Button::builder()
            .label(&words)
            .halign(gtk::Align::Center)
            .css_classes(["flat", "own-switch"])
            .build();
        crate::ui::name(&switch, &words);
        let line = label(
            &gettext("To sign in another way, remove this account and add it again."),
            &["own-note"],
        );
        line.set_xalign(0.5);
        line.set_justify(gtk::Justification::Center);
        line.set_visible(false);
        let row = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(["own-other"])
            .build();
        row.append(&switch);
        row.append(&line);
        Other { row, switch, line }
    }

    fn signing_again(&self, again: bool) {
        self.switch.set_visible(!again);
        self.line.set_visible(again);
    }
}

/// The button that leaves the setup: Online Accounts where the system has
/// it, an app password elsewhere.
fn switch_words() -> String {
    match Platform::this() {
        // Translators: "Online Accounts" is the name GNOME and Linux Mint show for this window; use the name your desktop shows in your language.
        Platform::Linux => gettext("Use Online Accounts instead"),
        Platform::MacOs => gettext("Use an app password instead"),
    }
}

/// The intro: what the setup gives, and the two ways in.
struct IntroPage {
    page: adw::NavigationPage,
    start: gtk::Button,
    have_key: gtk::Button,
    other: Other,
}

/// One of steps 1 to 5: what to do on one Google Cloud page.
struct StepPage {
    step: Step,
    page: adw::NavigationPage,
    open: gtk::Button,
    next: gtk::Button,
    start_over: gtk::Button,
    other: Other,
}

/// Step 6: the key file, dropped, chosen or pasted.
struct FilePage {
    page: adw::NavigationPage,
    drop: gtk::Box,
    drop_icon: gtk::Image,
    drop_inside: gtk::Box,
    choose: gtk::Button,
    problem: gtk::Label,
    paste: adw::ExpanderRow,
    id: adw::EntryRow,
    secret: adw::PasswordEntryRow,
    ready: gtk::Box,
    sign_in: gtk::Button,
    start_over: gtk::Button,
    other: Other,
}

/// The page before Google's: what Google is about to say.
struct WarningPage {
    page: adw::NavigationPage,
    sign_in: gtk::Button,
    other: Other,
}

/// The setup's pages, built once.
pub struct OwnProject {
    core: Rc<Core>,
    hooks: Hooks,
    /// The view the pages live in, and the tag of its first page, which
    /// Start Over goes back to.
    nav: RefCell<Option<(adw::NavigationView, &'static str)>>,
    /// Whether the setup opened at a step saved from before, so each step
    /// offers Start Over.
    resumed: Cell<bool>,
    intro: IntroPage,
    steps: Vec<StepPage>,
    file: FilePage,
    warning: WarningPage,
    /// The client step 6 read, which the sign-in goes through.
    found: RefCell<Option<OwnClient>>,
    /// `found` came from the pasted pair, so editing the pair away from a
    /// valid one lets it go.
    found_pasted: Cell<bool>,
}

/// The five steps done on Google Cloud.
const CLOUD_STEPS: [Step; 5] = [
    Step::Project,
    Step::Apis,
    Step::Screen,
    Step::Publish,
    Step::Key,
];

impl OwnProject {
    pub fn new(core: &Rc<Core>, hooks: Hooks) -> Rc<OwnProject> {
        let this = Rc::new(OwnProject {
            core: Rc::clone(core),
            hooks,
            nav: RefCell::new(None),
            resumed: Cell::new(false),
            intro: intro_page(),
            steps: CLOUD_STEPS.into_iter().map(step_page).collect(),
            file: file_page(),
            warning: warning_page(),
            found: RefCell::new(None),
            found_pasted: Cell::new(false),
        });
        this.connect();
        this
    }

    /// Every page with its tag, in the order the setup goes through them.
    /// The host adds each to its view before it opens one.
    pub fn pages(&self) -> Vec<(&'static str, adw::NavigationPage)> {
        let mut pages = vec![(Step::Intro.tag(), self.intro.page.clone())];
        pages.extend(
            self.steps
                .iter()
                .map(|shown| (shown.step.tag(), shown.page.clone())),
        );
        pages.push((Step::File.tag(), self.file.page.clone()));
        pages.push((Step::Warning.tag(), self.warning.page.clone()));
        pages
    }

    /// Shows page `at` in `nav`, whose first page is `root`. With
    /// `resumed`, the step pages offer to start over.
    pub fn open(
        self: &Rc<Self>,
        nav: &adw::NavigationView,
        root: &'static str,
        at: Step,
        resumed: bool,
    ) {
        self.nav.replace(Some((nav.clone(), root)));
        self.resumed.set(resumed);
        self.show_start_over(resumed);
        self.go(at);
    }

    /// Whether the setup is for an account that signs in again, which has
    /// no other way to sign in to switch to.
    pub fn signing_again(&self, again: bool) {
        for other in self.others() {
            other.signing_again(again);
        }
    }

    fn others(&self) -> Vec<&Other> {
        let mut others = vec![&self.intro.other, &self.file.other, &self.warning.other];
        others.extend(self.steps.iter().map(|shown| &shown.other));
        others
    }

    /// Takes `own` as the key the sign-in goes through, as a project kept
    /// from an earlier setup is.
    pub fn use_key(&self, own: OwnClient) {
        self.found_key(own);
    }

    /// Fills the pasted pair and opens its rows, as a person would.
    pub fn paste(&self, id: &str, secret: &str) {
        self.file.paste.set_expanded(true);
        self.file.id.set_text(id);
        self.file.secret.set_text(secret);
    }

    /// The control that takes the focus when page `at` opens first in
    /// the dialog, which would otherwise put it on its close button.
    pub fn first_focus(&self, at: Step) -> gtk::Widget {
        match at {
            Step::Intro => self.intro.start.clone().upcast(),
            Step::File if self.found.borrow().is_some() => self.file.sign_in.clone().upcast(),
            Step::File => self.file.choose.clone().upcast(),
            Step::Warning => self.warning.sign_in.clone().upcast(),
            step => self
                .steps
                .iter()
                .find(|shown| shown.step == step)
                .map_or_else(
                    || self.intro.start.clone().upcast(),
                    |shown| shown.open.clone().upcast(),
                ),
        }
    }

    /// Pushes `step`'s page, or goes back to it when it is already under
    /// the page on screen, so the stack never holds a page twice.
    fn go(&self, step: Step) {
        let Some((nav, _)) = self.nav.borrow().clone() else {
            return;
        };
        let tag = step.tag();
        let stack = nav.navigation_stack();
        let stacked = (0..stack.n_items())
            .filter_map(|i| stack.item(i))
            .filter_map(|page| page.downcast::<adw::NavigationPage>().ok())
            .any(|page| page.tag().as_deref() == Some(tag));
        if stacked {
            nav.pop_to_tag(tag);
        } else {
            nav.push_by_tag(tag);
        }
    }

    /// Forgets the step reached and goes back to the intro.
    fn start_over(&self) {
        if let Err(err) = self.core.set_google_setup_step(None) {
            tracing::warn!(%err, "could not forget the Google setup step");
        }
        self.resumed.set(false);
        self.show_start_over(false);
        let Some((nav, root)) = self.nav.borrow().clone() else {
            return;
        };
        if root == Step::Intro.tag() {
            nav.replace_with_tags(&[root]);
        } else {
            nav.replace_with_tags(&[root, Step::Intro.tag()]);
        }
    }

    fn connect(self: &Rc<Self>) {
        self.connect_file();
        for other in self.others() {
            let switch = weakly(self, |this| (this.hooks.switch)());
            other.switch.connect_clicked(move |_| switch());
        }
        let sign_in = weakly(self, |this| {
            let found = this.found.borrow().clone();
            match found {
                Some(own) => (this.hooks.sign_in)(own),
                None => this.go(Step::File),
            }
        });
        self.warning.sign_in.connect_clicked(move |_| sign_in());
        let button = self.warning.sign_in.downgrade();
        self.warning.page.connect_shown(move |_| {
            if let Some(button) = button.upgrade() {
                button.grab_focus();
            }
        });
        let start = weakly(self, |this| this.go(Step::Project));
        self.intro.start.connect_clicked(move |_| start());
        let have_key = weakly(self, |this| this.go(Step::File));
        self.intro.have_key.connect_clicked(move |_| have_key());
        let first = self.intro.start.downgrade();
        self.intro.page.connect_shown(move |_| {
            if let Some(button) = first.upgrade() {
                button.grab_focus();
            }
        });
        for shown in &self.steps {
            let step = shown.step;
            if let Some(url) = own_project::console_url(step) {
                let open = weakly(self, move |this| (this.hooks.open_uri)(url));
                shown.open.connect_clicked(move |_| open());
            }
            if let Some(next) = step.next() {
                let go = weakly(self, move |this| this.go(next));
                shown.next.connect_clicked(move |_| go());
            }
            let over = weakly(self, |this| this.start_over());
            shown.start_over.connect_clicked(move |_| over());
            let reached = weakly(self, move |this| this.reached(step));
            let open = shown.open.downgrade();
            shown.page.connect_shown(move |_| {
                if let Some(button) = open.upgrade() {
                    button.grab_focus();
                }
                reached();
            });
        }
    }

    fn show_start_over(&self, shown: bool) {
        for page in &self.steps {
            page.start_over.set_visible(shown);
        }
        self.file.start_over.set_visible(shown);
    }

    fn connect_file(self: &Rc<Self>) {
        let page = &self.file;
        let choose = weakly(self, |this| this.choose_file());
        page.choose.connect_clicked(move |_| choose());
        let over = weakly(self, |this| this.start_over());
        page.start_over.connect_clicked(move |_| over());
        let sign_in = weakly(self, |this| this.sign_in_pressed());
        page.sign_in.connect_clicked(move |_| sign_in());
        let typed = weakly(self, |this| this.pasted());
        page.id.connect_changed(move |_| typed());
        let typed = weakly(self, |this| this.pasted());
        page.secret.connect_changed(move |_| typed());
        let shown = weakly(self, |this| this.update_sign_in());
        page.paste.connect_expanded_notify(move |_| shown());
        // A drop carries files, or text such as a link from a browser's
        // download bar, which is answered rather than refused unseen.
        let target = gtk::DropTarget::new(glib::Type::INVALID, gdk::DragAction::COPY);
        target.set_types(&[gdk::FileList::static_type(), glib::Type::STRING]);
        let weak = Rc::downgrade(self);
        target.connect_drop(move |_, value, _, _| {
            let Some(this) = weak.upgrade() else {
                return false;
            };
            let paths: Vec<std::path::PathBuf> = value
                .get::<gdk::FileList>()
                .map(|list| list.files().iter().filter_map(|file| file.path()).collect())
                .unwrap_or_default();
            match own_project::dropped(&paths) {
                Some(path) => this.read(gio::File::for_path(path)),
                None => this.refused(FileProblem::NotTheFile),
            }
            true
        });
        page.drop.add_controller(target);
        let weak = Rc::downgrade(self);
        page.page.connect_shown(move |_| {
            let Some(this) = weak.upgrade() else { return };
            this.reached(Step::File);
            this.focus_main();
        });
    }

    /// Focuses Sign In once a key is found, and Choose File before.
    fn focus_main(&self) {
        let found = self.found.borrow().is_some();
        if found {
            self.file.sign_in.grab_focus();
        } else {
            self.file.choose.grab_focus();
        }
    }

    /// Opens the file chooser in Downloads, where the browser put the key.
    fn choose_file(self: &Rc<Self>) {
        let json = gtk::FileFilter::new();
        json.set_name(Some(&gettext("Key File")));
        json.add_pattern("*.json");
        json.add_mime_type("application/json");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&json);
        let dialog = gtk::FileDialog::builder()
            .modal(true)
            .filters(&filters)
            .default_filter(&json)
            .build();
        if let Some(downloads) = glib::user_special_dir(glib::UserDirectory::Downloads) {
            dialog.set_initial_folder(Some(&gio::File::for_path(downloads)));
        }
        let window = self.file.page.root().and_downcast::<gtk::Window>();
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            // Closing the chooser picks nothing and says nothing.
            if let Ok(file) = dialog.open_future(window.as_ref()).await {
                this.read(file);
            }
        });
    }

    /// Reads the key in `file`, then shows what it found or why not.
    fn read(self: &Rc<Self>, file: gio::File) {
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            match own_project::read_key_file(&file).await {
                Ok(own) => this.found_key(own),
                Err(problem) => this.refused(problem),
            }
        });
    }

    fn found_key(&self, own: OwnClient) {
        self.found_pasted.set(false);
        self.show_found(own);
        self.file.sign_in.grab_focus();
    }

    /// Keeps `own` for the sign-in and says it is ready, with the drop
    /// area folded to one line.
    fn show_found(&self, own: OwnClient) {
        self.found.replace(Some(own));
        let page = &self.file;
        page.problem.set_visible(false);
        page.ready.set_visible(true);
        page.drop.add_css_class("own-drop-found");
        page.drop_icon.set_visible(false);
        page.drop_inside
            .set_orientation(gtk::Orientation::Horizontal);
        page.choose.remove_css_class("suggested-action");
        self.update_sign_in();
    }

    /// Says why the file or the pasted pair made no key, and leaves the
    /// person on Choose File to try another.
    fn refused(&self, problem: FileProblem) {
        let page = &self.file;
        page.problem.set_text(&problem.to_string());
        page.problem.set_visible(true);
        page.choose.grab_focus();
    }

    /// The pasted pair, once both halves have text. Half a pair reports
    /// nothing until Sign In is pressed.
    fn pasted(&self) {
        let (id, secret) = (self.file.id.text(), self.file.secret.text());
        let own = match id.trim().is_empty() || secret.trim().is_empty() {
            true => None,
            false => from_pasted(&id, &secret).ok(),
        };
        match own {
            Some(own) => {
                self.found_pasted.set(true);
                self.show_found(own);
            }
            // A pair edited away from a valid one takes back the key it
            // gave, so Sign In does not go through a key no longer shown.
            None if self.found_pasted.replace(false) => self.forget_found(),
            None => {}
        }
    }

    /// Undoes [`OwnProject::show_found`]: no key, and the drop area open.
    fn forget_found(&self) {
        self.found.replace(None);
        let page = &self.file;
        page.ready.set_visible(false);
        page.drop.remove_css_class("own-drop-found");
        page.drop_icon.set_visible(true);
        page.drop_inside.set_orientation(gtk::Orientation::Vertical);
        self.update_sign_in();
    }

    /// Sign In shows once a key is found or the pasted rows are open.
    fn update_sign_in(&self) {
        let found = self.found.borrow().is_some();
        let shown = found || self.file.paste.is_expanded();
        self.file.sign_in.set_visible(shown);
        if !found {
            self.file.choose.add_css_class("suggested-action");
        }
    }

    fn sign_in_pressed(&self) {
        let found = self.found.borrow().clone();
        if found.is_some() {
            self.go(Step::Warning);
            return;
        }
        let (id, secret) = (self.file.id.text(), self.file.secret.text());
        if id.trim().is_empty() && secret.trim().is_empty() {
            self.file.choose.grab_focus();
            return;
        }
        if let Err(problem) = from_pasted(&id, &secret) {
            self.refused(problem);
        }
    }

    /// Remembers that the person reached `step`, for the next time the
    /// setup opens.
    fn reached(&self, step: Step) {
        if let Err(err) = self.core.set_google_setup_step(step.number()) {
            tracing::warn!(%err, "could not remember the Google setup step");
        }
    }
}

/// `run` on the setup while it lasts. The pages' controls hold this, so
/// they keep the setup from being freed only through the host.
fn weakly(this: &Rc<OwnProject>, run: impl Fn(&Rc<OwnProject>) + 'static) -> impl Fn() + 'static {
    let weak = Rc::downgrade(this);
    move || {
        if let Some(this) = weak.upgrade() {
            run(&this);
        }
    }
}

/// A row of centered buttons, as the intro and the warning have.
fn centered_buttons(buttons: &[&gtk::Button]) -> gtk::Box {
    let row = gtk::Box::builder()
        .spacing(10)
        .halign(gtk::Align::Center)
        .css_classes(["own-buttons"])
        .build();
    for button in buttons {
        row.append(*button);
    }
    row
}

fn button(text: &str, suggested: bool) -> gtk::Button {
    let button = gtk::Button::builder()
        .label(text)
        .css_classes(["own-button"])
        .build();
    if suggested {
        button.add_css_class("suggested-action");
    }
    button
}

fn intro_page() -> IntroPage {
    let words = own_project::words(Step::Intro);
    let banded = banded(Step::Intro.tag(), &words.heading);
    let title = heading(&words.heading);
    title.set_xalign(0.5);
    title.set_justify(gtk::Justification::Center);
    banded.body.append(&title);
    for paragraph in &words.checklist {
        let text = label(paragraph, &["post-lede", "own-intro-text"]);
        text.set_xalign(0.5);
        text.set_justify(gtk::Justification::Center);
        banded.body.append(&text);
    }
    let start = button(&gettext("Start"), true);
    let have_key = button(&gettext("I Already Have a Key"), false);
    banded.body.append(&centered_buttons(&[&start, &have_key]));
    let other = Other::new();
    banded.body.append(&other.row);
    IntroPage {
        page: banded.page,
        start,
        have_key,
        other,
    }
}

fn warning_page() -> WarningPage {
    let words = own_project::words(Step::Warning);
    let banded = banded(Step::Warning.tag(), &words.heading);
    let title = heading(&words.heading);
    title.set_xalign(0.5);
    title.set_justify(gtk::Justification::Center);
    banded.body.append(&title);
    for paragraph in &words.checklist {
        // Markup of our own, translated: Google's words in bold.
        let text = label("", &["post-lede", "own-intro-text"]);
        text.set_markup(paragraph);
        text.set_xalign(0.5);
        text.set_justify(gtk::Justification::Center);
        banded.body.append(&text);
    }
    let sign_in = button(&gettext("Sign in with Google"), true);
    banded.body.append(&centered_buttons(&[&sign_in]));
    let other = Other::new();
    banded.body.append(&other.row);
    WarningPage {
        page: banded.page,
        sign_in,
        other,
    }
}

/// A page with a header bar and no band: the steps' layout.
struct Plain {
    page: adw::NavigationPage,
    header: adw::HeaderBar,
    /// The page's column, which scrolls.
    body: gtk::Box,
    /// The column's foot, under a space that takes what is left.
    foot: gtk::Box,
}

fn plain_page(step: Step, title: &str) -> Plain {
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .vexpand(true)
        .css_classes(["post-body", "own-step"])
        .build();
    if let Some(caption) = own_project::caption(step) {
        body.append(&label(&caption, &["own-caption"]));
    }
    body.append(&heading(title));
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .vexpand(true)
        .build();
    let foot = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["own-foot"])
        .build();
    column.append(&body);
    column.append(&foot);
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&column)
        .vexpand(true)
        .build();
    let header = adw::HeaderBar::builder().show_title(false).build();
    let toolbar = adw::ToolbarView::builder()
        .content(&scroller)
        .css_classes(["post-opaque"])
        .build();
    toolbar.add_top_bar(&header);
    let page = adw::NavigationPage::builder()
        .title(title)
        .tag(step.tag())
        .child(&toolbar)
        .build();
    Plain {
        page,
        header,
        body,
        foot,
    }
}

/// "Steps look different?", the link to the help for `step`.
/// Four buttons, one per API, for the case that the one link above does
/// not turn them all on.
fn api_library_links() -> gtk::Box {
    let links = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .halign(gtk::Align::Start)
        .build();
    links.append(&label(
        // Translators: the services are Google's, switched on one at a time with the four links below, in place of the one button above.
        &gettext("Turn them on one at a time instead"),
        &["own-note"],
    ));
    for (name, url) in own_project::API_LIBRARY {
        let content = adw::ButtonContent::builder()
            .icon_name("adw-external-link-symbolic")
            .label(name)
            .build();
        let link = gtk::LinkButton::builder()
            .uri(url)
            .halign(gtk::Align::Start)
            .build();
        link.set_child(Some(&content));
        crate::ui::name(&link, name);
        links.append(&link);
    }
    links
}

fn help_link(step: Step) -> gtk::LinkButton {
    let text = gettext("Steps look different?");
    let content = adw::ButtonContent::builder()
        .icon_name("adw-external-link-symbolic")
        .label(&text)
        .build();
    let link = gtk::LinkButton::builder()
        .uri(own_project::help_url(step))
        .halign(gtk::Align::Center)
        .css_classes(["own-help"])
        .build();
    link.set_child(Some(&content));
    crate::ui::name(&link, &text);
    link
}

/// The numbered list of what to do. The items are this file's own
/// translated markup, never text from elsewhere.
fn checklist(items: &[String]) -> gtk::ListBox {
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list", "own-list"])
        .build();
    for (index, item) in items.iter().enumerate() {
        let number = gtk::Label::builder()
            .label((index + 1).to_string())
            .valign(gtk::Align::Start)
            .css_classes(["own-number"])
            .build();
        // The number is drawn for the eye; the list already says the order.
        number.update_state(&[gtk::accessible::State::Hidden(true)]);
        let text = gtk::Label::builder()
            .label(item)
            .use_markup(true)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .xalign(0.0)
            .hexpand(true)
            .css_classes(["own-item-text"])
            .build();
        let line = gtk::Box::builder()
            .spacing(11)
            .css_classes(["own-item"])
            .build();
        line.append(&number);
        line.append(&text);
        let row = gtk::ListBoxRow::builder()
            .child(&line)
            .activatable(false)
            .focusable(false)
            .build();
        // A list item takes no name from the label inside it.
        let spoken = gtk::pango::parse_markup(item, '\0')
            .map(|(_, text, _)| text.to_string())
            .unwrap_or_default();
        crate::ui::name(&row, &spoken);
        list.append(&row);
    }
    list
}

fn step_page(step: Step) -> StepPage {
    let words = own_project::words(step);
    let plain = plain_page(step, &words.heading);
    if let Some(lede) = &words.lede {
        plain.body.append(&label(lede, &["own-lede"]));
    }
    let open_text = gettext("Open Google Cloud");
    let open = gtk::Button::builder()
        .child(
            &adw::ButtonContent::builder()
                .icon_name("adw-external-link-symbolic")
                .label(&open_text)
                .build(),
        )
        .halign(gtk::Align::Start)
        .css_classes(["own-button", "own-open", "suggested-action"])
        .build();
    crate::ui::name(&open, &open_text);
    plain.body.append(&open);
    if step == Step::Apis {
        plain.body.append(&api_library_links());
    }
    plain.body.append(&checklist(&words.checklist));
    for note in &words.notes {
        plain.body.append(&label(note, &["own-note"]));
    }
    let next = button(&gettext("Next"), false);
    next.set_halign(gtk::Align::Center);
    plain.foot.append(&next);
    plain.foot.append(&help_link(step));
    let other = Other::new();
    plain.foot.append(&other.row);
    let start_over = gtk::Button::builder()
        .label(gettext("Start over"))
        .visible(false)
        .css_classes(["flat"])
        .build();
    plain.header.pack_start(&start_over);
    StepPage {
        step,
        page: plain.page,
        open,
        next,
        start_over,
        other,
    }
}

fn file_page() -> FilePage {
    let words = own_project::words(Step::File);
    let plain = plain_page(Step::File, &words.heading);
    let icon = gtk::Image::builder()
        .icon_name("document-save-symbolic")
        .pixel_size(44)
        .css_classes(["own-drop-icon"])
        .build();
    let hint = label(
        &gettext("Drop the file you downloaded here"),
        &["own-drop-text"],
    );
    hint.set_xalign(0.5);
    hint.set_justify(gtk::Justification::Center);
    hint.set_hexpand(false);
    let choose = gtk::Button::builder()
        .label(gettext("Choose File…"))
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .css_classes(["own-button", "suggested-action"])
        .build();
    let drop = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .halign(gtk::Align::Fill)
        .css_classes(["own-drop"])
        .build();
    let inside = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .halign(gtk::Align::Center)
        .css_classes(["own-drop-inside"])
        .build();
    inside.append(&hint);
    inside.append(&choose);
    drop.append(&icon);
    drop.append(&inside);
    plain.body.append(&drop);
    let problem = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .visible(false)
        .css_classes(["error", "own-problem"])
        .build();
    plain.body.append(&problem);
    // This is a button on Google's page; use the words Google's page shows in your language.
    let id_title = gettext("Client ID");
    let id = adw::EntryRow::builder().title(&id_title).build();
    crate::ui::name(&id, &id_title);
    // This is a button on Google's page; use the words Google's page shows in your language.
    let secret_title = gettext("Client secret");
    let secret = adw::PasswordEntryRow::builder()
        .title(&secret_title)
        .build();
    crate::ui::name(&secret, &secret_title);
    let paste = adw::ExpanderRow::builder()
        .title(gettext("Copy and paste instead"))
        .use_markup(false)
        .build();
    paste.add_row(&id);
    paste.add_row(&secret);
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list", "own-paste"])
        .build();
    list.append(&paste);
    plain.body.append(&list);
    let ready = gtk::Box::builder()
        .spacing(9)
        .visible(false)
        .css_classes(["own-ready"])
        .build();
    let tick = gtk::Image::builder()
        .icon_name("object-select-symbolic")
        .css_classes(["own-ready-icon"])
        .build();
    // The words are ours; nothing from the file reaches them.
    let found = label(&gettext("Ready: Penguin Mail project"), &["own-ready-text"]);
    ready.append(&tick);
    ready.append(&found);
    plain.body.append(&ready);
    let sign_in = button(&gettext("Sign in with Google"), true);
    sign_in.set_halign(gtk::Align::Center);
    sign_in.set_visible(false);
    plain.foot.append(&sign_in);
    plain.foot.append(&help_link(Step::File));
    let other = Other::new();
    plain.foot.append(&other.row);
    let start_over = gtk::Button::builder()
        .label(gettext("Start over"))
        .visible(false)
        .css_classes(["flat"])
        .build();
    plain.header.pack_start(&start_over);
    FilePage {
        page: plain.page,
        drop,
        drop_icon: icon,
        drop_inside: inside,
        choose,
        problem,
        paste,
        id,
        secret,
        ready,
        sign_in,
        start_over,
        other,
    }
}
