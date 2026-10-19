//! The pages that walk a person through making their own Google project,
//! hosted in Add Account's navigation view.
#![allow(dead_code, reason = "Add Account hosts these pages")]

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use mailrs_domain::translate::gettext;
use mailrs_sync::sign_in::own_client::OwnClient;

use super::add_account::{banded, heading, label};
use crate::core::Core;
use crate::own_project::{self, Step};

/// What the pages ask of the dialog that hosts them.
pub struct Hooks {
    /// Signs in through the client the person gave.
    pub sign_in: Box<dyn Fn(OwnClient)>,
    /// Opens a page in the browser.
    pub open_uri: Box<dyn Fn(&str)>,
}

/// The intro: what the setup gives, and the two ways in.
struct IntroPage {
    page: adw::NavigationPage,
    start: gtk::Button,
    have_key: gtk::Button,
}

/// One of steps 1 to 5: what to do on one Google Cloud page.
struct StepPage {
    step: Step,
    page: adw::NavigationPage,
    open: gtk::Button,
    next: gtk::Button,
    start_over: gtk::Button,
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
        for shown in &self.steps {
            shown.start_over.set_visible(resumed);
        }
        self.go(at);
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
        for shown in &self.steps {
            shown.start_over.set_visible(false);
        }
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

/// A row of centred buttons, as the intro and the warning have.
fn centred_buttons(buttons: &[&gtk::Button]) -> gtk::Box {
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
    banded.body.append(&centred_buttons(&[&start, &have_key]));
    IntroPage {
        page: banded.page,
        start,
        have_key,
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
    plain.body.append(&checklist(&words.checklist));
    for note in &words.notes {
        plain.body.append(&label(note, &["own-note"]));
    }
    let next = button(&gettext("Next"), false);
    next.set_halign(gtk::Align::Center);
    plain.foot.append(&next);
    plain.foot.append(&help_link(step));
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
    }
}
