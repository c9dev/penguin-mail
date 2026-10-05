//! Add Account: the provider tiles, then the address and the lookup, the
//! password with Server Settings beside it, the browser sign-in, and the
//! page that says the account is added while its first sync runs. The
//! band at the top shows each step with the tuxedo envelope.
//! `crate::add_account` decides what each step shows; this file draws it
//! and carries the person's answers there and back.
//!
//! The band sits under the navigation view. Each page leaves its top
//! 196 pixels clear, so the band shows through and stays put while the
//! page under it slides, and a change of step crossfades its pose.
//! Server Settings has no band: its page is opaque and covers it.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::{gio, glib, graphene};
use mailrs_discover::Server;
use mailrs_domain::translate::{fill, fill_plural, gettext};
use mailrs_domain::{Account, AccountId, MailSet, Provider, RemoveSetting, Role as MailRole};
use mailrs_store::{accounts, servers};

use crate::add_account::lookup::{Check, Checks, State};
use crate::add_account::post::{self, Advice, AdviceKind, Band, Browser, Stamp, Step, Tile};
use crate::add_account::{
    self, Address, Asking, Continue, Failure, FailureKind, Next, Outcome, Proposal, Protocol,
    Removal, Role, Running, Typed,
};
use crate::core::Core;
use crate::permission::Permission;
use crate::ui::post_band::PostBand;
use crate::ui::tile_grid::TileGrid;

/// The dialog's size, the mockup's.
const WIDTH: i32 = 480;
const HEIGHT: i32 = 700;
/// The band's height in the dialog.
const BAND: i32 = 196;
/// A lookup that answers within this never shows its page, which would
/// only flash.
const LOOKUP_SHOWS_AFTER: Duration = Duration::from_millis(400);

/// Where the dialog opens.
pub enum Opening {
    /// The provider tiles.
    Pick,
    /// The address page for a tile picked in the first-run window.
    Tile(Tile),
    /// A provider's browser sign-in, from the first-run window.
    Browser(Browser),
    /// The password page for an IMAP account that needs to sign in again.
    Again(Account),
    /// A demo stage, for screenshots: see [`preview`].
    Preview(String),
}

/// What the dialog hands the window.
pub enum Done {
    /// An IMAP account signed in, with the name the person gave for their
    /// mail, if any. The dialog stays open on its first sync.
    Added {
        account: Account,
        name: Option<String>,
    },
    /// A Google or Microsoft account signed in through the browser, with
    /// the name Microsoft knows the person by. The dialog stays open on
    /// its first sync, or on Grant Access.
    BrowserAdded {
        account: Account,
        name: Option<String>,
    },
    /// An IMAP account that needed to sign in again did. The dialog is
    /// closed.
    SignedInAgain(Account),
    /// Open Inbox on the last page: the window shows that account's
    /// inbox. The dialog is closed.
    OpenInbox(AccountId),
    /// Grant Access on the last page: the window sends this account
    /// through its provider's consent again. The dialog is closed.
    Grant(String),
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
    let root = match &opening {
        Opening::Pick | Opening::Preview(_) => "pick",
        Opening::Tile(_) => "address",
        Opening::Browser(_) => "browser",
        Opening::Again(_) => "password",
    };
    // Every page is added, the root first, so a page popped off the stack
    // stays in the dialog: a page left without a parent while a screen
    // reader still holds its rows makes GTK abort.
    let pages = this.pages();
    for (tag, page) in &pages {
        if *tag == root {
            this.nav.add(page);
        }
    }
    for (tag, page) in &pages {
        if *tag != root {
            this.nav.add(page);
        }
    }
    this.show_band();
    // The dialog owns the state behind its buttons until it closes, and
    // an answer that arrives after that has nowhere to go.
    let keep = RefCell::new(Some(Rc::clone(&this)));
    this.window.connect_closed(move |_| {
        let this = keep.borrow_mut().take();
        if let Some(this) = this {
            this.asking.close();
            this.stop_browser();
            this.stop_counting();
        }
    });
    this.window.present(Some(parent));
    // The dialog puts its own focus on the first button in the header once
    // it presents, which would leave Enter on Close: the first tile takes
    // it instead. Pages opened below take the focus as they show.
    if root == "pick"
        && let Some((_, first, _)) = this.pick.tiles.first()
    {
        this.window.set_focus(Some(first));
    }
    match opening {
        Opening::Pick => {}
        Opening::Tile(tile) => this.open_address(Some(tile), false),
        Opening::Browser(browser) => this.start_browser(browser, None),
        Opening::Again(account) => this.load_saved(account),
        Opening::Preview(stage) => preview(&this, &stage),
    }
}

/// A banded page: a flat header over the band, the page's own column
/// under it.
struct Banded {
    page: adw::NavigationPage,
    header: adw::HeaderBar,
    body: gtk::Box,
}

fn banded(tag: &str, title: &str) -> Banded {
    let header = adw::HeaderBar::builder()
        .show_title(false)
        .css_classes(["post-band-header"])
        .build();
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .vexpand(true)
        .css_classes(["post-body"])
        .build();
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&body)
        .vexpand(true)
        .build();
    // The clear space the band shows through.
    let clear = gtk::Box::builder()
        .height_request(BAND)
        .can_target(false)
        .build();
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .build();
    column.append(&clear);
    column.append(&scroller);
    let toolbar = adw::ToolbarView::builder()
        .extend_content_to_top_edge(true)
        .top_bar_style(adw::ToolbarStyle::Flat)
        .content(&column)
        .build();
    toolbar.add_top_bar(&header);
    let page = adw::NavigationPage::builder()
        .title(title)
        .tag(tag)
        .child(&toolbar)
        .build();
    Banded { page, header, body }
}

fn label(text: &str, classes: &[&str]) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .xalign(0.0)
        .css_classes(classes.to_vec())
        .build()
}

fn heading(text: &str) -> gtk::Label {
    let heading = label(text, &["post-title"]);
    heading.set_accessible_role(gtk::AccessibleRole::Heading);
    heading
}

fn pill(text: &str, suggested: bool) -> gtk::Button {
    let classes = if suggested {
        vec!["pill", "suggested-action", "post-wide"]
    } else {
        vec!["pill", "post-wide"]
    };
    gtk::Button::builder()
        .label(text)
        .css_classes(classes)
        .build()
}

/// A button with an icon and words, both part of its name.
fn icon_button(icon: &str, text: &str, classes: &[&str]) -> gtk::Button {
    let content = adw::ButtonContent::builder()
        .icon_name(icon)
        .label(text)
        .build();
    let button = gtk::Button::builder()
        .child(&content)
        .css_classes(classes.to_vec())
        .build();
    crate::ui::name(&button, text);
    button
}

/// A provider's mark: its logo, or its initial on a tile of its colour.
fn mark(stamp: Stamp, size: i32) -> gtk::Label {
    let mark = gtk::Label::builder()
        .label(stamp.letter.to_string())
        .width_request(size)
        .height_request(size)
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Start)
        .accessible_role(gtk::AccessibleRole::Presentation)
        .css_classes(["post-mark", &format!("mark-{size}")])
        .build();
    set_mark(&mark, stamp);
    mark
}

fn set_mark(mark: &gtk::Label, stamp: Stamp) {
    for class in mark.css_classes() {
        if class.starts_with("tint-") || class.starts_with("logo-") {
            mark.remove_css_class(&class);
        }
    }
    // The logo is a background in the stylesheet, so the label holds no
    // text for a screen reader to find and the mark stays presentation.
    match stamp.logo {
        Some(logo) => {
            mark.set_label("");
            mark.add_css_class(&format!("logo-{logo}"));
        }
        None => mark.set_label(&stamp.letter.to_string()),
    }
    mark.add_css_class(&format!("tint-{}", stamp.colour.trim_start_matches('#')));
}

/// A card under a form: an icon or a mark, a heading, what to do, and
/// what else the moment needs, with each link a button of its own that a
/// screen reader names by its words.
struct Card {
    area: gtk::Box,
    icon: gtk::Image,
    mark: gtk::Label,
    title: gtk::Label,
    body: gtk::Label,
    /// A dim line under the body: where advice came from.
    source: gtk::Label,
    /// The server's own words, in monospace.
    said: gtk::Label,
    links: gtk::Box,
    buttons: gtk::Box,
}

impl Card {
    fn new() -> Card {
        let icon = gtk::Image::builder()
            .valign(gtk::Align::Start)
            .css_classes(["post-card-icon"])
            .build();
        let mark = mark(post::ANY_SERVER, 34);
        let title = label("", &["post-card-title"]);
        let body = label("", &["post-card-body"]);
        let source = label("", &["post-card-source"]);
        let said = label("", &["post-said", "monospace"]);
        said.set_selectable(true);
        let links = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(["post-links"])
            .build();
        let buttons = gtk::Box::builder()
            .spacing(8)
            .css_classes(["post-card-buttons"])
            .visible(false)
            .build();
        let words = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .hexpand(true)
            .build();
        for part in [&title, &body, &source, &said] {
            words.append(part);
        }
        words.append(&links);
        words.append(&buttons);
        let area = gtk::Box::builder()
            .css_classes(["post-card"])
            .visible(false)
            .build();
        area.append(&icon);
        area.append(&mark);
        area.append(&words);
        Card {
            area,
            icon,
            mark,
            title,
            body,
            source,
            said,
            links,
            buttons,
        }
    }

    /// Fills the card. `tone` is `advice` or `trouble`, the stylesheet's
    /// names for the two tints (libadwaita's own `accent` and `error`
    /// would colour every word); `lead` is an icon name, or a stamp for a provider mark.
    fn show(
        &self,
        tone: &str,
        lead: Result<&str, Stamp>,
        title: &str,
        body: &str,
        links: &[add_account::Link],
    ) {
        for other in ["advice", "trouble"] {
            self.area.remove_css_class(other);
        }
        self.area.add_css_class(tone);
        match lead {
            Ok(icon) => {
                self.icon.set_icon_name(Some(icon));
                self.icon.set_visible(true);
                self.mark.set_visible(false);
            }
            Err(stamp) => {
                set_mark(&self.mark, stamp);
                self.mark.set_visible(true);
                self.icon.set_visible(false);
            }
        }
        self.title.set_text(title);
        self.body.set_text(body);
        self.source.set_visible(false);
        self.area.remove_css_class("with-source");
        self.said.set_visible(false);
        while let Some(child) = self.links.first_child() {
            self.links.remove(&child);
        }
        for link in links {
            self.links.append(&link_button(link));
        }
        self.links.set_visible(!links.is_empty());
        self.buttons.set_visible(false);
        self.area.set_visible(true);
    }

    fn show_source(&self, source: &str) {
        self.source.set_text(source);
        self.source.set_visible(true);
        self.area.add_css_class("with-source");
    }

    fn show_said(&self, said: Option<&str>) {
        self.said.set_text(said.unwrap_or_default());
        self.said.set_visible(said.is_some());
    }

    /// Adds a Copy Command button under the links that puts `text` on the
    /// clipboard. `show` takes it away again with the links.
    fn show_copy(&self, text: Option<&str>) {
        let Some(text) = text else { return };
        let button = icon_button(
            "edit-copy-symbolic",
            &gettext("Copy Command"),
            &["pill", "post-small-pill"],
        );
        button.set_halign(gtk::Align::Start);
        let text = text.to_string();
        button.connect_clicked(move |button| {
            button.clipboard().set_text(&text);
            button.announce(
                &gettext("Command copied"),
                gtk::AccessibleAnnouncementPriority::Medium,
            );
        });
        self.links.append(&button);
        self.links.set_visible(true);
    }

    fn hide(&self) {
        self.area.set_visible(false);
    }
}

fn link_button(link: &add_account::Link) -> gtk::LinkButton {
    let content = adw::ButtonContent::builder()
        .icon_name("adw-external-link-symbolic")
        .label(&link.label)
        .build();
    let button = gtk::LinkButton::builder()
        .uri(&link.url)
        .halign(gtk::Align::Start)
        .build();
    button.set_child(Some(&content));
    crate::ui::name(&button, &link.label);
    button
}

/// A boxed list of rows.
fn boxed_list() -> gtk::ListBox {
    gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list", "post-list"])
        .build()
}

/// A row with an icon before it and, when it leads somewhere, a chevron
/// after it.
fn icon_row(icon: &str, title: &str, subtitle: &str, leads: bool) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(subtitle)
        .activatable(leads)
        .build();
    row.add_prefix(&gtk::Image::from_icon_name(icon));
    if leads {
        row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    }
    row
}

fn filler() -> gtk::Box {
    gtk::Box::builder().vexpand(true).build()
}

// ---- The pages ------------------------------------------------------------

struct PickPage {
    banded: Banded,
    tiles: Vec<(Tile, gtk::Button, gtk::Label)>,
    by_hand: adw::ActionRow,
}

/// The tiles, the line about the browser and the row to type servers:
/// the dialog's first page, and the body of the first-run window.
pub struct Tiles {
    pub grid: TileGrid,
    pub buttons: Vec<(Tile, gtk::Button, gtk::Label)>,
}

/// The provider tiles, three to a row, each row centred, so six sit three
/// over three and five three over two; two to a row in a window too
/// narrow for three (see [`TileGrid`]). Each tile is one button named
/// "Fastmail, Fastmail" or "Google, Gmail, Workspace, signs in through
/// your browser". `microsoft` is whether the build can sign in to
/// Microsoft, whose tile shows only then.
pub fn tiles(width: i32, microsoft: bool) -> Tiles {
    let mut buttons = Vec::new();
    for tile in post::tiles(microsoft) {
        let mark = mark(tile.stamp(), 38);
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        top.append(&mark);
        if tile.in_browser() {
            let globe = gtk::Image::builder()
                .icon_name("penguin-mail-globe-symbolic")
                .hexpand(true)
                .halign(gtk::Align::End)
                .valign(gtk::Align::Start)
                .css_classes(["post-globe"])
                .build();
            top.append(&globe);
        }
        let name = label(&tile.title(), &["post-tile-name"]);
        let sub = label(&tile.subtitle(), &["post-tile-sub"]);
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        content.append(&top);
        content.append(&name);
        content.append(&sub);
        let button = gtk::Button::builder()
            .child(&content)
            .width_request(width)
            .css_classes(["card", "post-tile"])
            .build();
        crate::ui::name(&button, &tile.described());
        buttons.push((tile, button, mark));
    }
    let grid = TileGrid::new(
        &buttons
            .iter()
            .map(|(_, button, _)| button.clone())
            .collect::<Vec<_>>(),
    );
    grid.add_css_class("post-tiles");
    Tiles { grid, buttons }
}

/// The line under the tiles: the globe marks the tiles that sign in
/// through the browser.
pub fn browser_legend() -> gtk::Box {
    let legend = gtk::Box::builder()
        .spacing(8)
        .css_classes(["post-legend"])
        .build();
    legend.append(
        &gtk::Image::builder()
            .icon_name("penguin-mail-globe-symbolic")
            .accessible_role(gtk::AccessibleRole::Presentation)
            .build(),
    );
    legend.append(&label(&gettext("Signs in through your browser"), &[]));
    legend
}

fn pick_page(microsoft: bool) -> PickPage {
    let banded = banded("pick", &gettext("Add Account"));
    let body = &banded.body;
    body.add_css_class("centered");
    let title = heading(&gettext("Add an account"));
    title.set_xalign(0.5);
    let lede = label(&gettext("Choose where your mail lives."), &["post-lede"]);
    lede.set_xalign(0.5);
    let Tiles { grid, buttons } = tiles(136, microsoft);
    let by_hand = icon_row(
        "emblem-system-symbolic",
        &gettext("Enter server settings by hand"),
        "",
        true,
    );
    by_hand.add_css_class("post-by-hand");
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["post-plain-list"])
        .build();
    list.append(&by_hand);
    body.append(&title);
    body.append(&lede);
    body.append(&grid);
    body.append(&browser_legend());
    body.append(&gtk::Separator::builder().css_classes(["post-rule"]).build());
    body.append(&list);
    PickPage {
        banded,
        tiles: buttons,
        by_hand,
    }
}

struct AddressPage {
    banded: Banded,
    address: adw::EntryRow,
    said: gtk::Label,
    /// "Did you mean …?" and the button that takes the correction.
    suggest: gtk::Box,
    suggest_line: gtk::Label,
    take: gtk::Button,
    advice: Card,
    advice_reveal: gtk::Revealer,
    look: gtk::Button,
    servers: gtk::Button,
}

fn address_page() -> AddressPage {
    let banded = banded("address", &gettext("Your Email Address"));
    let address = adw::EntryRow::builder()
        .title(gettext("Email Address"))
        .input_purpose(gtk::InputPurpose::Email)
        .build();
    crate::ui::name(&address, &gettext("Email Address"));
    let field = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list", "post-field"])
        .build();
    field.append(&address);
    let said = label("", &["error", "post-said-line"]);
    said.set_visible(false);
    let suggest_line = label("", &[]);
    suggest_line.set_hexpand(true);
    let take = gtk::Button::builder()
        .label(gettext("Use This Address"))
        .valign(gtk::Align::Center)
        .build();
    let suggest = gtk::Box::builder()
        .spacing(12)
        .css_classes(["post-suggest"])
        .visible(false)
        .build();
    suggest.append(&suggest_line);
    suggest.append(&take);
    let advice = Card::new();
    let advice_reveal = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::Crossfade)
        .transition_duration(150)
        .child(&advice.area)
        .reveal_child(false)
        .build();
    let look = pill(&gettext("Continue"), true);
    let servers = pill(&gettext("Enter Server Settings"), false);
    let body = &banded.body;
    body.append(&heading(&gettext("Your email address")));
    body.append(&label(
        &gettext("Penguin Mail finds the servers for you."),
        &["post-lede-small"],
    ));
    body.append(&field);
    body.append(&said);
    body.append(&suggest);
    body.append(&advice_reveal);
    body.append(&filler());
    body.append(&look);
    body.append(&servers);
    look.add_css_class("post-first-button");
    AddressPage {
        banded,
        address,
        said,
        suggest,
        suggest_line,
        take,
        advice,
        advice_reveal,
        look,
        servers,
    }
}

struct LookupPage {
    banded: Banded,
    title: gtk::Label,
    rows: Vec<(Check, gtk::Label, gtk::Stack, gtk::Label)>,
    servers: gtk::Button,
}

fn lookup_page() -> LookupPage {
    let banded = banded("lookup", &gettext("Looking Up"));
    let title = heading("");
    let list = boxed_list();
    list.add_css_class("post-checks");
    let mut rows = Vec::new();
    for check in Check::ALL {
        let status = gtk::Stack::builder()
            .valign(gtk::Align::Center)
            .css_classes(["post-status"])
            .build();
        for state in ["not-listed", "waiting", "nothing"] {
            let ring = gtk::Box::builder()
                .css_classes(["post-ring", state])
                .valign(gtk::Align::Center)
                .halign(gtk::Align::Center)
                .build();
            status.add_named(&ring, Some(state));
        }
        // A 16-pixel spinner, the size of the rings beside it; an
        // `adw::Spinner` grows to fill the row and makes it taller.
        let spinner = gtk::Spinner::builder()
            .spinning(true)
            .css_classes(["post-spinner"])
            .build();
        status.add_named(&spinner, Some("asking"));
        let done = gtk::Image::builder()
            .icon_name("object-select-symbolic")
            .css_classes(["post-done"])
            .build();
        status.add_named(&done, Some("answered"));
        let title = label("", &["post-check-title"]);
        title.set_hexpand(true);
        title.set_valign(gtk::Align::Center);
        let state = label("", &["post-check-state"]);
        state.set_wrap(false);
        state.set_valign(gtk::Align::Center);
        // A row a person reads, not one they act on: each check says
        // where it has got, and a screen reader reads the pair.
        let line = gtk::Box::builder()
            .spacing(12)
            .css_classes(["post-check"])
            .build();
        line.append(&status);
        line.append(&title);
        line.append(&state);
        let row = gtk::ListBoxRow::builder()
            .child(&line)
            .activatable(false)
            .selectable(false)
            .focusable(false)
            .css_classes(["post-check-row"])
            .build();
        list.append(&row);
        rows.push((check, title, status, state));
    }
    let servers = gtk::Button::builder()
        .label(gettext("Enter Server Settings"))
        .halign(gtk::Align::Start)
        .css_classes(["pill", "post-mid-pill"])
        .build();
    let body = &banded.body;
    body.append(&title);
    body.append(&label(
        &gettext("Only the domain leaves this computer, never your full address."),
        &["post-lede"],
    ));
    body.append(&list);
    body.append(&filler());
    body.append(&servers);
    LookupPage {
        banded,
        title,
        rows,
        servers,
    }
}

struct BrowserPage {
    banded: Banded,
    title: gtk::Label,
    lede: gtk::Label,
    steps: gtk::Box,
    failed: Card,
    waiting: gtk::Box,
    left: gtk::Label,
    progress: gtk::ProgressBar,
    again: gtk::Button,
    copy: gtk::Button,
    retry: gtk::Button,
    cancel: gtk::Button,
}

fn browser_page() -> BrowserPage {
    let banded = banded("browser", &gettext("Sign In in Your Browser"));
    let title = heading("");
    let lede = label("", &["post-lede"]);
    let steps = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["post-steps"])
        .build();
    let failed = Card::new();
    let wait_line = label(&gettext("Waiting for your browser"), &["post-wait-title"]);
    wait_line.set_hexpand(true);
    let left = label("", &["post-left", "numeric"]);
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    top.append(&wait_line);
    top.append(&left);
    let progress = gtk::ProgressBar::builder()
        .css_classes(["post-progress"])
        .build();
    crate::ui::name(&progress, &gettext("Time left"));
    let waiting = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["post-waiting"])
        .build();
    waiting.append(&gtk::Separator::builder().css_classes(["post-rule"]).build());
    waiting.append(&top);
    waiting.append(&progress);
    waiting.append(&label(
        &gettext("Penguin Mail stops waiting after five minutes."),
        &["post-note"],
    ));
    let again = icon_button(
        "view-refresh-symbolic",
        &gettext("Open Page Again"),
        &["pill", "post-small-pill"],
    );
    let copy = icon_button(
        "edit-copy-symbolic",
        &gettext("Copy Link"),
        &["pill", "post-small-pill"],
    );
    let retry = icon_button(
        "view-refresh-symbolic",
        &gettext("Try Again"),
        &["pill", "post-small-pill"],
    );
    retry.set_visible(false);
    let cancel = gtk::Button::builder()
        .label(gettext("Cancel"))
        .hexpand(true)
        .halign(gtk::Align::End)
        .css_classes(["flat", "post-cancel"])
        .build();
    let buttons = gtk::Box::builder()
        .spacing(8)
        .css_classes(["post-bottom"])
        .build();
    for button in [&again, &copy, &retry, &cancel] {
        buttons.append(button);
    }
    let body = &banded.body;
    body.append(&title);
    body.append(&lede);
    body.append(&steps);
    body.append(&failed.area);
    body.append(&filler());
    body.append(&waiting);
    body.append(&buttons);
    BrowserPage {
        banded,
        title,
        lede,
        steps,
        failed,
        waiting,
        left,
        progress,
        again,
        copy,
        retry,
        cancel,
    }
}

/// The browser page's numbered steps, for `browser`'s own pages.
fn fill_steps(steps: &gtk::Box, browser: Browser) {
    while let Some(child) = steps.first_child() {
        steps.remove(&child);
    }
    for (index, step) in browser.steps().iter().enumerate() {
        let number = gtk::Label::builder()
            .label((index + 1).to_string())
            .valign(gtk::Align::Start)
            .css_classes(["post-step-number"])
            .build();
        let line = gtk::Box::builder()
            .spacing(10)
            .css_classes(["post-step"])
            .build();
        line.append(&number);
        line.append(&label(step, &["post-step-text"]));
        steps.append(&line);
    }
}

struct PasswordPage {
    banded: Banded,
    title: gtk::Label,
    lede: gtk::Label,
    /// The fields and cards, in the order the page shows them; an
    /// unreachable server puts its card at the top.
    fields: gtk::Box,
    name_list: gtk::ListBox,
    name: adw::EntryRow,
    password_list: gtk::ListBox,
    password: adw::PasswordEntryRow,
    hint: Card,
    failed: Card,
    failed_servers: gtk::Button,
    failed_retry: gtk::Button,
    incoming: adw::ActionRow,
    /// The server that did not answer, in place of its row, leading to
    /// Server Settings.
    down: adw::ActionRow,
    hosts: gtk::ListBox,
    outgoing: adw::ActionRow,
    /// The POP3 server discovery found beside IMAP, leading to Server
    /// Settings with POP3 picked.
    pop3_offer: adw::ActionRow,
    summary: adw::ActionRow,
    agree: adw::ActionRow,
    confirmed: gtk::CheckButton,
    sign_in: gtk::Button,
}

fn password_page() -> PasswordPage {
    let banded = banded("password", &gettext("Sign In"));
    banded.body.add_css_class("post-found");
    let title = heading("");
    let lede = label("", &["post-lede-small"]);
    let name = adw::EntryRow::builder()
        .title(gettext("Your Name"))
        .input_purpose(gtk::InputPurpose::Name)
        .build();
    crate::ui::name(&name, &gettext("Your Name"));
    let password = adw::PasswordEntryRow::builder()
        .title(gettext("Password"))
        .build();
    crate::ui::name(&password, &gettext("Password"));
    let field = |row: &gtk::Widget| {
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list", "post-field"])
            .build();
        list.append(row);
        list
    };
    let name_list = field(name.upcast_ref());
    let password_list = field(password.upcast_ref());
    let hint = Card::new();
    let failed = Card::new();
    let failed_servers = icon_button(
        "emblem-system-symbolic",
        &gettext("Server Settings"),
        &["pill", "post-small-pill"],
    );
    let failed_retry = icon_button(
        "view-refresh-symbolic",
        &gettext("Try Again"),
        &["pill", "post-small-pill"],
    );
    failed.buttons.append(&failed_servers);
    failed.buttons.append(&failed_retry);
    let hosts = boxed_list();
    hosts.add_css_class("post-servers");
    let incoming = adw::ActionRow::builder()
        .title(gettext("Incoming"))
        .subtitle_selectable(true)
        .build();
    incoming.add_prefix(&gtk::Image::from_icon_name("penguin-mail-lock-symbolic"));
    let down = adw::ActionRow::builder().activatable(true).build();
    down.add_css_class("post-down");
    let down_icon = gtk::Image::from_icon_name("network-wireless-offline-symbolic");
    down_icon.add_css_class("post-down-icon");
    down.add_prefix(&down_icon);
    down.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    down.set_visible(false);
    let outgoing = icon_row("penguin-mail-lock-symbolic", &gettext("Outgoing"), "", true);
    // Shaped like the outgoing row: a choice one level deeper, offered
    // under the servers the password goes to.
    let pop3_offer = adw::ActionRow::builder()
        .activatable(true)
        .subtitle(gettext("Keep this account's mail on this computer alone"))
        .visible(false)
        .build();
    pop3_offer.add_prefix(&gtk::Image::from_icon_name("computer-symbolic"));
    pop3_offer.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    let summary = icon_row("penguin-mail-lock-symbolic", &gettext("Servers"), "", true);
    summary.set_visible(false);
    let confirmed = gtk::CheckButton::builder()
        .valign(gtk::Align::Center)
        .build();
    crate::ui::name(&confirmed, &gettext("Use these servers"));
    let agree = adw::ActionRow::builder()
        .title(gettext("Use these servers"))
        .subtitle(gettext(
            "Penguin Mail guessed these servers. Your password goes to them only after you check the box.",
        ))
        .activatable_widget(&confirmed)
        .visible(false)
        .build();
    agree.add_prefix(&confirmed);
    for row in [&incoming, &outgoing, &pop3_offer, &down, &summary, &agree] {
        hosts.append(row);
    }
    let fields = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["post-fields"])
        .build();
    for part in [
        name_list.upcast_ref::<gtk::Widget>(),
        password_list.upcast_ref(),
        hint.area.upcast_ref(),
        failed.area.upcast_ref(),
        hosts.upcast_ref(),
    ] {
        fields.append(part);
    }
    let sign_in = gtk::Button::builder()
        .label(gettext("Sign In"))
        .sensitive(false)
        .css_classes(["suggested-action", "post-header-action"])
        .build();
    banded.header.pack_end(&sign_in);
    let body = &banded.body;
    body.append(&title);
    body.append(&lede);
    body.append(&fields);
    PasswordPage {
        banded,
        title,
        lede,
        fields,
        name_list,
        name,
        password_list,
        password,
        hint,
        failed,
        failed_servers,
        failed_retry,
        incoming,
        down,
        hosts,
        outgoing,
        pop3_offer,
        summary,
        agree,
        confirmed,
        sign_in,
    }
}

struct ClosedPage {
    banded: Banded,
    title: gtk::Label,
    lede: gtk::Label,
    another_address: adw::ActionRow,
    another_provider: adw::ActionRow,
}

fn closed_page() -> ClosedPage {
    let banded = banded("closed", &gettext("No Other Mail Apps"));
    let title = heading("");
    let lede = label("", &["post-lede"]);
    let list = boxed_list();
    let another_address = icon_row(
        "document-edit-symbolic",
        &gettext("Use another address"),
        &gettext("Change the address and look it up again"),
        true,
    );
    let another_provider = icon_row(
        "go-previous-symbolic",
        &gettext("Choose another provider"),
        &gettext("Back to the list"),
        true,
    );
    list.add_css_class("post-closed-list");
    list.append(&another_address);
    list.append(&another_provider);
    let body = &banded.body;
    body.append(&title);
    body.append(&lede);
    body.append(&list);
    ClosedPage {
        banded,
        title,
        lede,
        another_address,
        another_provider,
    }
}

/// One folder on the last page: its count and its progress.
struct FolderRow {
    role: MailRole,
    row: gtk::Box,
    state: gtk::Label,
    progress: gtk::ProgressBar,
}

struct AddedPage {
    banded: Banded,
    title: gtk::Label,
    lede: gtk::Label,
    folders: Vec<FolderRow>,
    folder_list: gtk::ListBox,
    note: gtk::Label,
    /// The features Google's page left off, for Grant Access.
    withheld: gtk::ListBox,
    grant_note: gtk::Label,
    open_inbox: gtk::Button,
    another: gtk::Button,
    grant_buttons: gtk::Box,
    not_now: gtk::Button,
    grant: gtk::Button,
}

fn added_page() -> AddedPage {
    let banded = banded("added", &gettext("Account Added"));
    // The account is added; going back would offer to add it again.
    banded.page.set_can_pop(false);
    let title = heading("");
    let lede = label("", &["post-lede"]);
    let folder_list = boxed_list();
    folder_list.add_css_class("post-folders");
    let mut folders = Vec::new();
    for (role, icon, name) in [
        // The sidebar's own names, so each folder reads as it does there.
        (
            MailRole::Inbox,
            "penguin-mail-inbox-symbolic",
            crate::ui::Standard::Inbox.name(),
        ),
        (
            MailRole::Sent,
            "mail-send-symbolic",
            crate::ui::Standard::Sent.name(),
        ),
        (
            MailRole::Archive,
            "penguin-mail-archive-symbolic",
            mailrs_domain::translate::pgettext("mailbox", "Archive"),
        ),
    ] {
        let state = label("", &["post-folder-state", "numeric"]);
        state.set_xalign(1.0);
        let top = gtk::Box::builder().spacing(12).build();
        top.append(&gtk::Image::from_icon_name(icon));
        let title = label(&name, &["post-folder-name"]);
        title.set_hexpand(true);
        top.append(&title);
        top.append(&state);
        let progress = gtk::ProgressBar::builder()
            .css_classes(["post-progress"])
            .build();
        crate::ui::name(&progress, &name);
        let row = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(["post-folder"])
            .build();
        row.append(&top);
        row.append(&progress);
        folder_list.append(&row);
        folders.push(FolderRow {
            role,
            row,
            state,
            progress,
        });
    }
    // The words depend on the account, so `show_added` sets them.
    let note = label("", &["post-note"]);
    let withheld = boxed_list();
    let grant_note = label(
        &gettext("Grant Access opens Google's page again. You can also do it later in Preferences."),
        &["post-note"],
    );
    let open_inbox = pill(&gettext("Open Inbox"), true);
    open_inbox.add_css_class("post-first-button");
    let another = pill(&gettext("Add Another Account"), false);
    let not_now = gtk::Button::builder()
        .label(gettext("Not Now"))
        .css_classes(["pill", "post-mid-pill"])
        .build();
    let grant = icon_button(
        "penguin-mail-globe-symbolic",
        &gettext("Grant Access"),
        &["pill", "suggested-action", "post-mid-pill"],
    );
    grant.set_hexpand(true);
    grant.set_halign(gtk::Align::End);
    let grant_buttons = gtk::Box::builder()
        .css_classes(["post-bottom"])
        .build();
    grant_buttons.append(&not_now);
    grant_buttons.append(&grant);
    let body = &banded.body;
    for part in [
        title.upcast_ref::<gtk::Widget>(),
        lede.upcast_ref(),
        folder_list.upcast_ref(),
        note.upcast_ref(),
        withheld.upcast_ref(),
        grant_note.upcast_ref(),
        filler().upcast_ref(),
        open_inbox.upcast_ref(),
        another.upcast_ref(),
        grant_buttons.upcast_ref(),
    ] {
        body.append(part);
    }
    AddedPage {
        banded,
        title,
        lede,
        folders,
        folder_list,
        note,
        withheld,
        grant_note,
        open_inbox,
        another,
        grant_buttons,
        not_now,
        grant,
    }
}

struct ManualStep {
    page: adw::NavigationPage,
    content: adw::PreferencesPage,
    /// What the incoming server speaks: IMAP or POP3.
    protocol: adw::ToggleGroup,
    removal: Removals,
    incoming: ServerRows,
    smtp: ServerRows,
    problem: gtk::Label,
    use_them: gtk::Button,
}

/// The three rows Server Settings gives each server, and its user name,
/// which sits with the other one in the Sign-In group.
struct ServerRows {
    host: adw::EntryRow,
    port: adw::SpinRow,
    security: adw::ToggleGroup,
    user: adw::EntryRow,
}

impl ServerRows {
    fn new(
        role: Role,
        protocol: Option<adw::ToggleGroup>,
        group: &adw::PreferencesGroup,
        user: adw::EntryRow,
    ) -> ServerRows {
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
        // A port still on the other choice's usual number follows the
        // switch; one the person typed stays.
        let follows = port.clone();
        security.connect_active_notify(move |security| {
            let now = add_account::security_at(security.active());
            let protocol = protocol
                .as_ref()
                .map_or(Protocol::Imap, |p| add_account::protocol_at(p.active()));
            let port = follows.value() as u16;
            let moved = add_account::port_after_switch(role, protocol, port, now);
            if moved != port {
                follows.set_value(f64::from(moved));
            }
        });
        ServerRows {
            host,
            port,
            security,
            user,
        }
    }

    fn fill(&self, server: &Server, user: Option<&str>) {
        self.user.set_text(user.unwrap_or_default());
        self.fill_server(server);
    }

    fn fill_server(&self, server: &Server) {
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
            user: self.user.text().to_string(),
        }
    }
}

/// Server Settings' removal choice for a POP3 account: three radio rows
/// and the day count the third one reads.
struct Removals {
    group: adw::PreferencesGroup,
    rows: Vec<(Removal, gtk::CheckButton, adw::ActionRow)>,
    days: adw::SpinRow,
}

impl Removals {
    fn new() -> Removals {
        let group = adw::PreferencesGroup::builder()
            .title(gettext("Mail on the Server"))
            .description(gettext(
                "Penguin Mail keeps every message it downloads on this computer.",
            ))
            .visible(false)
            .build();
        let days = adw::SpinRow::with_range(1.0, 365.0, 1.0);
        days.set_title(&gettext("Days"));
        crate::ui::name(&days, &gettext("Days"));
        days.set_value(f64::from(add_account::DEFAULT_DAYS));
        days.set_visible(false);
        let mut rows: Vec<(Removal, gtk::CheckButton, adw::ActionRow)> = Vec::new();
        for removal in add_account::REMOVALS {
            let check = gtk::CheckButton::builder()
                .valign(gtk::Align::Center)
                .build();
            if let Some((_, first, _)) = rows.first() {
                check.set_group(Some(first));
            }
            let title = add_account::removal_title(removal, add_account::DEFAULT_DAYS);
            crate::ui::name(&check, &title);
            let row = adw::ActionRow::builder()
                .title(&title)
                .activatable_widget(&check)
                .build();
            row.add_prefix(&check);
            group.add(&row);
            rows.push((removal, check, row));
        }
        group.add(&days);
        let removals = Removals { group, rows, days };
        removals.follow_days();
        removals
    }

    /// The day count is read only by the third row, whose title says it,
    /// and shows only while that row is picked: its number is already in
    /// the title above it.
    fn follow_days(&self) {
        let Some((_, third, after)) = self.rows.last() else {
            return;
        };
        let (after, check, days) = (after.clone(), third.clone(), self.days.clone());
        let retitle = move || {
            let title = add_account::removal_title(Removal::AfterDays, days.value() as u32);
            after.set_title(&title);
            crate::ui::name(&check, &title);
        };
        let again = retitle.clone();
        self.days.connect_value_notify(move |_| again());
        let days = self.days.clone();
        third.connect_active_notify(move |third| days.set_visible(third.is_active()));
        retitle();
    }

    fn fill(&self, setting: RemoveSetting) {
        let (removal, days) = add_account::removal_of(setting);
        self.days.set_value(f64::from(days));
        for (choice, check, _) in &self.rows {
            check.set_active(*choice == removal);
        }
        self.days.set_visible(removal == Removal::AfterDays);
    }

    fn chosen(&self) -> RemoveSetting {
        let removal = self
            .rows
            .iter()
            .find(|(_, check, _)| check.is_active())
            .map_or(Removal::Leave, |(removal, _, _)| *removal);
        add_account::removal_setting(removal, self.days.value() as u32)
    }
}

/// An entry row, named for a screen reader by its title.
fn entry(title: &str) -> adw::EntryRow {
    let row = adw::EntryRow::builder().title(title).build();
    crate::ui::name(&row, title);
    row
}

fn manual_step() -> ManualStep {
    let imap_user = entry(&gettext("Incoming User Name"));
    let smtp_user = entry(&gettext("Outgoing User Name"));
    // An entry row has no placeholder of its own: one set on its inner
    // text draws over the title. The rule goes in a tooltip, since a
    // second line under the group would push this row off a 768-pixel
    // screen.
    smtp_user.set_tooltip_text(Some(&gettext(
        "Leave it empty to use the incoming user name.",
    )));
    let incoming = adw::PreferencesGroup::builder()
        .title(gettext("Incoming Mail"))
        .build();
    // Two choices side by side, as Security is.
    let protocol = adw::ToggleGroup::builder()
        .valign(gtk::Align::Center)
        .build();
    for choice in add_account::PROTOCOLS {
        let label = add_account::protocol_label(choice);
        protocol.add(adw::Toggle::builder().label(label).name(label).build());
    }
    crate::ui::name(&protocol, &gettext("Protocol"));
    let protocol_row = adw::ActionRow::builder()
        .title(gettext("Protocol"))
        .build();
    protocol_row.add_suffix(&protocol);
    incoming.add(&protocol_row);
    let incoming_rows = ServerRows::new(
        Role::Incoming,
        Some(protocol.clone()),
        &incoming,
        imap_user,
    );
    let removal = Removals::new();
    // The port follows the protocol as it follows Security, and the
    // removal rows show for POP3 alone.
    let (port, security, shown) = (
        incoming_rows.port.clone(),
        incoming_rows.security.clone(),
        removal.group.clone(),
    );
    protocol.connect_active_notify(move |protocol| {
        let now = add_account::protocol_at(protocol.active());
        let at = port.value() as u16;
        let security = add_account::security_at(security.active());
        let moved = add_account::port_after_protocol(at, security, now);
        if moved != at {
            port.set_value(f64::from(moved));
        }
        shown.set_visible(now == Protocol::Pop3);
    });
    let outgoing = adw::PreferencesGroup::builder()
        .title(gettext("Outgoing Mail"))
        .build();
    let smtp = ServerRows::new(Role::Outgoing, None, &outgoing, smtp_user);
    let problem = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .visible(false)
        .margin_top(12)
        .css_classes(["error"])
        .build();
    let login = adw::PreferencesGroup::builder()
        .title(gettext("Sign-In"))
        .description(gettext("Leave them empty to sign in with your address."))
        .build();
    login.add(&incoming_rows.user);
    login.add(&smtp.user);
    login.add(&problem);
    let content = adw::PreferencesPage::new();
    content.add(&incoming);
    content.add(&removal.group);
    content.add(&outgoing);
    content.add(&login);
    let use_them = gtk::Button::builder()
        .label(gettext("Use These Settings"))
        .css_classes(["suggested-action"])
        .build();
    let header = adw::HeaderBar::new();
    header.pack_end(&use_them);
    let toolbar = adw::ToolbarView::builder()
        .content(&content)
        .css_classes(["post-opaque"])
        .build();
    toolbar.add_top_bar(&header);
    let page = adw::NavigationPage::builder()
        .title(gettext("Server Settings"))
        .tag("servers")
        .child(&toolbar)
        .build();
    ManualStep {
        page,
        content,
        protocol,
        removal,
        incoming: incoming_rows,
        smtp,
        problem,
        use_them,
    }
}

// ---- The dialog ---------------------------------------------------------------

/// What the band shows for one page.
#[derive(Clone)]
struct BandState {
    step: Step,
    stamp: Option<Stamp>,
    provider: Option<String>,
}

struct Dialog {
    core: Rc<Core>,
    window: adw::Dialog,
    nav: adw::NavigationView,
    band: PostBand,
    /// What the band shows on each page, by the page's tag.
    bands: RefCell<HashMap<&'static str, BandState>>,
    asking: Asking,
    /// Whether a sign-in is on its way, so Sign In and Enter cannot start
    /// a second one beside it.
    running: Running,
    done: Box<dyn Fn(Done)>,
    /// The account signing in again, when that is why the dialog opened.
    again: Option<Account>,
    /// The tile picked, whose stamp the address page shows until the
    /// address names a provider.
    tile: Cell<Option<Tile>>,
    /// The address page leads to Server Settings rather than a lookup:
    /// the person chose to type the servers.
    by_hand: Cell<bool>,
    /// The address signing in.
    typed: RefCell<Option<Address>>,
    /// What the password page signs in to.
    proposal: RefCell<Option<Proposal>>,
    /// The address the address page offered a correction for, so a second
    /// Continue with it unchanged looks it up as typed.
    declined: RefCell<Option<Address>>,
    /// The lookup's checks, while it runs.
    checks: RefCell<Checks>,
    /// Dropping this stops the browser sign-in.
    browser_cancel: RefCell<Option<async_channel::Sender<()>>>,
    /// The sign-in page's address, for Open Page Again and Copy Link.
    browser_url: RefCell<Option<String>>,
    /// When the browser sign-in gives up.
    browser_deadline: Cell<Option<Instant>>,
    browser_timer: RefCell<Option<glib::SourceId>>,
    /// Whether the browser sign-in expects an address, for Try Again.
    browser_expected: RefCell<Option<String>>,
    /// The provider the browser page waits for, for Try Again and the
    /// stamp on a failure.
    browser_provider: Cell<Browser>,
    /// The account the last page follows, and the timer that reads its
    /// counts.
    added: Cell<Option<AccountId>>,
    count_timer: RefCell<Option<glib::SourceId>>,
    pick: PickPage,
    address: AddressPage,
    lookup: LookupPage,
    browser: BrowserPage,
    password: PasswordPage,
    closed: ClosedPage,
    finished: AddedPage,
    manual: ManualStep,
}

/// Whether Add Account offers Microsoft: in a build with its client, and
/// in the demo, which signs in nowhere and shows every tile.
pub fn signs_in_to_microsoft(core: &Core) -> bool {
    core.demo || core.built_with_microsoft_sign_in()
}

impl Dialog {
    fn microsoft(&self) -> bool {
        signs_in_to_microsoft(&self.core)
    }

    fn new(core: &Rc<Core>, again: Option<Account>, done: Box<dyn Fn(Done)>) -> Rc<Dialog> {
        let nav = adw::NavigationView::new();
        let band = PostBand::new(BAND, 1.0);
        let under = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        under.append(&band.widget);
        // The space under the band takes what is left, so the band keeps
        // its own height.
        under.append(&filler());
        let stack = gtk::Overlay::builder().child(&under).build();
        stack.add_overlay(&nav);
        let window = adw::Dialog::builder()
            .title(gettext("Add Account"))
            .content_width(WIDTH)
            .content_height(HEIGHT)
            .child(&stack)
            .css_classes(["post-dialog"])
            .build();
        Rc::new(Dialog {
            core: Rc::clone(core),
            window,
            nav,
            band,
            bands: RefCell::new(HashMap::new()),
            asking: Asking::default(),
            running: Running::default(),
            done,
            again,
            tile: Cell::new(None),
            by_hand: Cell::new(false),
            typed: RefCell::new(None),
            proposal: RefCell::new(None),
            declined: RefCell::new(None),
            checks: RefCell::new(Checks::default()),
            browser_cancel: RefCell::new(None),
            browser_url: RefCell::new(None),
            browser_deadline: Cell::new(None),
            browser_timer: RefCell::new(None),
            browser_expected: RefCell::new(None),
            browser_provider: Cell::new(Browser::Google),
            added: Cell::new(None),
            count_timer: RefCell::new(None),
            pick: pick_page(signs_in_to_microsoft(core)),
            address: address_page(),
            lookup: lookup_page(),
            browser: browser_page(),
            password: password_page(),
            closed: closed_page(),
            finished: added_page(),
            manual: manual_step(),
        })
    }

    fn pages(&self) -> [(&'static str, adw::NavigationPage); 8] {
        [
            ("pick", self.pick.banded.page.clone()),
            ("address", self.address.banded.page.clone()),
            ("lookup", self.lookup.banded.page.clone()),
            ("browser", self.browser.banded.page.clone()),
            ("password", self.password.banded.page.clone()),
            ("closed", self.closed.banded.page.clone()),
            ("added", self.finished.banded.page.clone()),
            ("servers", self.manual.page.clone()),
        ]
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
        for (tile, button, mark) in &self.pick.tiles {
            let (weak, tile, mark) = (Rc::downgrade(self), *tile, mark.clone());
            button.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.chose(tile, &mark);
                }
            });
        }
        let by_hand = on(|this| this.open_address(None, true));
        self.pick.by_hand.connect_activated(move |_| by_hand());
        let look = on(Dialog::look);
        self.address.look.connect_clicked(move |_| look());
        let look = on(Dialog::look);
        self.address.address.connect_entry_activated(move |_| look());
        let changed = on(Dialog::address_changed);
        self.address.address.connect_changed(move |_| changed());
        let take = on(Dialog::take_suggestion);
        self.address.take.connect_clicked(move |_| take());
        let manual = on(Dialog::manual_from_address);
        self.address.servers.connect_clicked(move |_| manual());
        let manual = on(Dialog::manual_from_address);
        self.lookup.servers.connect_clicked(move |_| manual());
        let manual = on(Dialog::manual_from_password);
        self.password.outgoing.connect_activated(move |_| manual());
        let manual = on(Dialog::manual_from_password);
        self.password.summary.connect_activated(move |_| manual());
        let manual = on(Dialog::manual_from_password);
        self.password.down.connect_activated(move |_| manual());
        let manual = on(Dialog::manual_from_password);
        self.password.failed_servers.connect_clicked(move |_| manual());
        let as_pop3 = on(Dialog::manual_as_pop3);
        self.password.pop3_offer.connect_activated(move |_| as_pop3());
        let follow = on(Dialog::follow_protocol);
        self.manual.protocol.connect_active_notify(move |_| follow());
        let sign_in = on(Dialog::sign_in);
        self.password.failed_retry.connect_clicked(move |_| sign_in());
        let sign_in = on(Dialog::sign_in);
        self.password.sign_in.connect_clicked(move |_| sign_in());
        let sign_in = on(Dialog::sign_in);
        self.password
            .password
            .connect_entry_activated(move |_| sign_in());
        let ready = on(Dialog::update_sign_in);
        self.password.password.connect_changed(move |_| ready());
        let ready = on(Dialog::update_sign_in);
        self.password.confirmed.connect_toggled(move |_| ready());
        let use_them = on(Dialog::use_manual);
        self.manual.use_them.connect_clicked(move |_| use_them());
        let again = on(|this| this.open_page_again());
        self.browser.again.connect_clicked(move |_| again());
        let copy = on(|this| this.copy_link());
        self.browser.copy.connect_clicked(move |_| copy());
        let retry = on(|this| {
            let expected = this.browser_expected.borrow().clone();
            this.start_browser(this.browser_provider.get(), expected);
        });
        self.browser.retry.connect_clicked(move |_| retry());
        let cancel = on(Dialog::cancel_browser);
        self.browser.cancel.connect_clicked(move |_| cancel());
        let other_address = on(|this| {
            this.back_to("address");
            this.address.address.grab_focus();
        });
        self.closed
            .another_address
            .connect_activated(move |_| other_address());
        let other_provider = on(|this| this.back_to("pick"));
        self.closed
            .another_provider
            .connect_activated(move |_| other_provider());
        let open_inbox = on(|this| {
            if let Some(id) = this.added.get() {
                this.finish(Done::OpenInbox(id));
            }
        });
        self.finished.open_inbox.connect_clicked(move |_| open_inbox());
        let another = on(Dialog::add_another);
        self.finished.another.connect_clicked(move |_| another());
        let not_now = on(|this| {
            this.window.close();
        });
        self.finished.not_now.connect_clicked(move |_| not_now());
        let grant = on(|this| {
            let address = this.typed.borrow().as_ref().map(Address::full);
            if let Some(address) = address {
                this.finish(Done::Grant(address));
            }
        });
        self.finished.grant.connect_clicked(move |_| grant());
        // The band follows the page on screen.
        let shown = on(|this| this.show_band());
        self.nav.connect_visible_page_notify(move |_| shown());
        // Each step opens with the cursor in the field it asks for, so
        // the whole dialog runs from the keyboard.
        let address = self.address.address.clone();
        self.address
            .banded
            .page
            .connect_shown(move |_| _ = address.grab_focus());
        let password = self.password.password.clone();
        self.password
            .banded
            .page
            .connect_shown(move |_| _ = password.grab_focus());
        let host = self.manual.incoming.host.clone();
        self.manual
            .page
            .connect_shown(move |_| _ = host.grab_focus());
        let first = self.pick.tiles.first().map(|(_, button, _)| button.clone());
        self.pick.banded.page.connect_shown(move |_| {
            if let Some(first) = &first {
                first.grab_focus();
            }
        });
        let open_inbox = self.finished.open_inbox.clone();
        let grant = self.finished.grant.clone();
        self.finished.banded.page.connect_shown(move |_| {
            if grant.get_visible() {
                grant.grab_focus();
            } else {
                open_inbox.grab_focus();
            }
        });
    }

    /// Says what the band shows on page `tag`, and shows it if that page
    /// is on screen.
    fn set_band(&self, tag: &'static str, step: Step, stamp: Option<Stamp>, provider: Option<&str>) {
        self.bands.borrow_mut().insert(
            tag,
            BandState {
                step,
                stamp,
                provider: provider.map(str::to_string),
            },
        );
        if self.showing() == Some(tag) {
            self.show_band();
        }
    }

    fn showing(&self) -> Option<&'static str> {
        let tag = self.nav.visible_page().and_then(|page| page.tag())?;
        self.pages()
            .into_iter()
            .map(|(tag, _)| tag)
            .find(|known| *known == tag.as_str())
    }

    fn show_band(&self) {
        let Some(tag) = self.showing() else {
            return;
        };
        let state = self.bands.borrow().get(tag).cloned().unwrap_or(BandState {
            step: if tag == "servers" { Step::Servers } else { Step::Pick },
            stamp: None,
            provider: None,
        });
        // Server Settings covers the band, which keeps the pose it had.
        if let Some(band) = post::band_for(state.step) {
            let stamp = if band == Band::Idle { None } else { state.stamp };
            self.band.show(band, stamp, state.provider.as_deref());
        }
    }

    /// Goes back to page `tag`, which may be the root.
    fn back_to(&self, tag: &str) {
        if self.nav.find_page(tag).is_some() && !self.nav.pop_to_tag(tag) {
            self.nav.replace_with_tags(&[tag]);
        }
    }

    /// Pushes page `tag` unless it is already on screen.
    fn push(&self, tag: &str) {
        if self.showing() == Some(tag) {
            return;
        }
        // A page already under the one on screen is gone back to, so the
        // stack never holds a page twice.
        let stacked = (0..self.nav.navigation_stack().n_items())
            .filter_map(|i| self.nav.navigation_stack().item(i))
            .filter_map(|page| page.downcast::<adw::NavigationPage>().ok())
            .any(|page| page.tag().as_deref() == Some(tag));
        if stacked {
            self.nav.pop_to_tag(tag);
        } else {
            self.nav.push_by_tag(tag);
        }
    }

    /// A tile: the browser for Google and Microsoft, the address page for
    /// the rest. The tile's mark flies up to the envelope's corner.
    fn chose(self: &Rc<Self>, tile: Tile, mark: &gtk::Label) {
        let middle = graphene::Point::new(mark.width() as f32 / 2.0, mark.height() as f32 / 2.0);
        match tile.browser() {
            Some(browser) => self.start_browser(browser, None),
            None => self.open_address(Some(tile), false),
        }
        self.band.fly_from(mark, &middle);
    }

    /// The address page, for `tile` or for any server. `by_hand` sends
    /// Continue to Server Settings instead of a lookup.
    fn open_address(self: &Rc<Self>, tile: Option<Tile>, by_hand: bool) {
        self.tile.set(tile.filter(|t| *t != Tile::Other));
        self.by_hand.set(by_hand);
        self.address.servers.set_visible(!by_hand);
        self.address_changed();
        self.push("address");
    }

    /// Closes the dialog and hands the window what comes next.
    fn finish(&self, done: Done) {
        self.window.close();
        (self.done)(done);
    }

    // ---- The address and the lookup ----------------------------------------

    /// The address page's Continue: looks the address up, or says why it
    /// cannot.
    fn look(self: &Rc<Self>) {
        let typed = self.address.address.text();
        if self.by_hand.get() {
            return self.manual_from_address();
        }
        let declined = self.declined.borrow().clone();
        let address = match add_account::on_continue(&typed, declined.as_ref()) {
            Continue::Look(address) => address,
            Continue::Suggest(better) => return self.suggest(&typed, &better),
            Continue::Say(said) => return self.say(&said),
        };
        let ticket = self.asking.ask();
        self.address.said.set_visible(false);
        self.address.look.set_sensitive(false);
        self.typed.replace(Some(address.clone()));
        self.checks.replace(Checks::default());
        self.lookup.title.set_text(&fill(
            &gettext("Looking up {domain}"),
            &[("domain", &address.domain)],
        ));
        self.show_checks();
        let stamp = self.address_stamp(&address.domain);
        self.set_band("lookup", Step::Lookup, Some(stamp), None);
        self.band.set_lower_asking(false);
        let (heard, hearing) = async_channel::unbounded();
        // The lookup page shows only when the answer takes a moment.
        let this = Rc::clone(self);
        glib::timeout_add_local_once(LOOKUP_SHOWS_AFTER, move || {
            if this.asking.wants(ticket) {
                this.push("lookup");
            }
        });
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            while let Ok(heard) = hearing.recv().await {
                if !this.asking.wants(ticket) {
                    return;
                }
                this.checks.borrow_mut().heard(heard);
                this.show_checks();
            }
        });
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let found = this.core.discover(address.full(), heard).await;
            // The person changed the address or closed the dialog while
            // this ran, and nobody is asking this question now.
            if !this.asking.wants(ticket) {
                return;
            }
            this.asking.forget();
            this.address.look.set_sensitive(true);
            let next = match found {
                Ok(found) => {
                    add_account::after_discovery(found, &address, this.microsoft())
                }
                Err(err) => Next::Say(err.to_string()),
            };
            match next {
                Next::Password(proposal) => this.show_password(proposal),
                Next::Say(said) => {
                    this.back_to("address");
                    this.say(&said);
                }
                Next::Closed(_) => this.show_closed(&address),
                Next::Google => this.start_browser(Browser::Google, Some(address.full())),
                Next::Microsoft => {
                    this.start_browser(Browser::Microsoft, Some(address.full()))
                }
                Next::Manual { proposal, line } => this.show_manual(&proposal, Some(&line)),
            }
        });
    }

    fn show_checks(&self) {
        let checks = self.checks.borrow().clone();
        let domain = self
            .typed
            .borrow()
            .as_ref()
            .map(|a| a.domain.clone())
            .unwrap_or_default();
        for (check, title, status, state_label) in &self.lookup.rows {
            let state = checks.state(*check);
            title.set_text(&check.title(&domain));
            state_label.set_text(&state.text());
            for class in ["asking", "answered"] {
                state_label.remove_css_class(class);
            }
            let name = match state {
                State::NotListed => "not-listed",
                State::Waiting => "waiting",
                State::Asking => "asking",
                State::Answered => "answered",
                State::Nothing => "nothing",
            };
            status.set_visible_child_name(name);
            if name == "asking" {
                state_label.add_css_class("asking");
            }
        }
        // The lower line in the band asks once MX has answered.
        let mx_answered = matches!(checks.state(Check::Mx), State::Answered | State::Nothing);
        self.band.set_lower_asking(mx_answered);
    }

    /// The stamp for an address at `domain`: the provider the list names
    /// for it, else the tile picked, else the stamp for any server.
    fn address_stamp(&self, domain: &str) -> Stamp {
        post::advice(domain, self.microsoft())
            .map(|advice| advice.stamp)
            .or_else(|| self.tile.get().map(Tile::stamp))
            .unwrap_or(post::ANY_SERVER)
    }

    /// Says why the address page cannot go on, with the cursor back in
    /// the address to fix it. Continue lost the focus when it went
    /// insensitive.
    fn say(&self, said: &str) {
        self.address.said.set_text(said);
        self.address.said.set_visible(true);
        self.address.address.grab_focus();
        self.address
            .said
            .announce(said, gtk::AccessibleAnnouncementPriority::High);
    }

    /// Offers `better` in place of the address typed, and looks nothing
    /// up: the typed domain may belong to someone who would then see the
    /// lookups. Continue again with the address unchanged keeps it.
    fn suggest(&self, typed: &str, better: &Address) {
        self.declined.replace(Address::parse(typed));
        self.address
            .suggest_line
            .set_text(&add_account::did_you_mean(better));
        self.address.suggest.set_visible(true);
        self.address.address.grab_focus();
    }

    /// Puts the suggested address in the field, which hides the
    /// suggestion, and leaves the cursor at its end for Continue.
    fn take_suggestion(self: &Rc<Self>) {
        let typed = self.address.address.text();
        let Some(better) = Address::parse(&typed).and_then(|a| add_account::suggestion(&a)) else {
            return;
        };
        self.address.address.set_text(&better.full());
        self.address.address.grab_focus();
        self.address.address.set_position(-1);
    }

    /// Whatever the address page said, or is still looking up, was about
    /// another address. The list's advice for the new one shows at once,
    /// and the stamp follows it.
    fn address_changed(self: &Rc<Self>) {
        self.asking.forget();
        self.proposal.replace(None);
        self.address.said.set_visible(false);
        self.address.suggest.set_visible(false);
        self.address.look.set_sensitive(true);
        let typed = self.address.address.text();
        let domain = typed
            .rsplit_once('@')
            .map(|(_, domain)| domain.trim().trim_end_matches('.').to_lowercase());
        let advice = match &domain {
            Some(domain) if !domain.is_empty() => post::advice(domain, self.microsoft()),
            _ => None,
        };
        let advice = advice.or_else(|| {
            let from_domain = domain.as_ref().is_some_and(|d| d.contains('.'));
            self.tile
                .get()
                .filter(|_| !from_domain)
                .and_then(post::tile_advice)
        });
        let stamp = match (&advice, domain.as_deref()) {
            (Some(advice), _) => advice.stamp,
            (None, _) => self.tile.get().map_or(post::ANY_SERVER, Tile::stamp),
        };
        let provider = advice.as_ref().map(|a| a.provider.clone());
        self.show_advice(advice.as_ref());
        self.set_band("address", Step::Address, Some(stamp), provider.as_deref());
    }

    fn show_advice(&self, advice: Option<&Advice>) {
        let card = &self.address.advice;
        let before = card.title.text();
        match advice {
            Some(advice) => {
                let tone = match advice.kind {
                    AdviceKind::Closed => "trouble",
                    _ => "advice",
                };
                card.show(
                    tone,
                    Err(advice.stamp),
                    &advice.title,
                    &advice.body,
                    advice.link.as_slice(),
                );
                card.show_source(&post::advice_source());
                self.address.advice_reveal.set_reveal_child(true);
                // Once per change of advice, not per keystroke.
                if before != advice.title {
                    self.address.advice_reveal.announce(
                        &advice.title,
                        gtk::AccessibleAnnouncementPriority::Medium,
                    );
                }
            }
            None => {
                card.title.set_text("");
                self.address.advice_reveal.set_reveal_child(false);
            }
        }
    }

    /// A provider with no IMAP: the page that says so and offers another
    /// address or another provider.
    fn show_closed(&self, address: &Address) {
        let Some(advice) = post::advice(&address.domain, self.microsoft()) else {
            return;
        };
        self.closed.title.set_text(&advice.title);
        self.closed.lede.set_text(&advice.body);
        self.closed
            .another_provider
            .set_visible(self.nav.find_page("pick").is_some());
        self.set_band("closed", Step::Closed, Some(advice.stamp), Some(&advice.provider));
        self.push("closed");
    }

    // ---- The password ---------------------------------------------------------

    fn show_password(self: &Rc<Self>, proposal: Proposal) {
        let page = &self.password;
        let provider = post::short_name(&proposal.provider_name);
        let title = add_account::password_title(&proposal);
        page.password.set_title(&title);
        crate::ui::name(&page.password, &title);
        let typed = self.typed.borrow().clone();
        match &self.again {
            Some(account) => {
                page.title.set_text(&account.email);
                page.lede.set_text(&add_account::again_line(account));
                page.name_list.set_visible(false);
            }
            None => {
                let domain = typed.as_ref().map(|a| a.domain.clone()).unwrap_or_default();
                page.title.set_text(&if provider == domain {
                    fill(&gettext("Sign in to {domain}"), &[("domain", &domain)])
                } else {
                    fill(
                        &gettext("{domain} uses {provider}"),
                        &[("domain", &domain), ("provider", &provider)],
                    )
                });
                page.lede.set_text(&add_account::found_line(&proposal));
            }
        }
        match add_account::password_hint(&proposal) {
            Some(_) => {
                let info = proposal.info.as_ref();
                let name = info.map(|i| post::short_name(&i.name)).unwrap_or_default();
                let named = [("provider", name.as_str())];
                let link = info.and_then(|i| i.app_password_url.clone()).map(|url| {
                    add_account::Link {
                        label: fill(&gettext("Open {provider}'s Settings"), &named),
                        url,
                    }
                });
                page.hint.show(
                    "advice",
                    Ok("dialog-password-symbolic"),
                    &fill(&gettext("{provider} needs an app password"), &named),
                    &fill(
                        &gettext(
                            "Not your {provider} website password. Make one with mail access in {provider}'s settings and paste it here.",
                        ),
                        &named,
                    ),
                    link.as_slice(),
                );
            }
            None => page.hint.hide(),
        }
        page.failed.hide();
        self.arrange_password(false);
        self.show_hosts(&proposal);
        let stamp = post::stamp_for(&proposal.provider_name);
        let stamp = if proposal.info.is_some() {
            stamp
        } else {
            post::ANY_SERVER
        };
        self.set_band("password", Step::Found, Some(stamp), Some(&provider));
        self.proposal.replace(Some(proposal));
        self.update_sign_in();
        self.push("password");
    }

    /// Puts the failure card first for an unreachable server, as the
    /// mockup does, or after the fields for everything else.
    fn arrange_password(&self, card_first: bool) {
        let page = &self.password;
        page.failed.area.remove_css_class("post-first-card");
        // The mockup sets the found page's heading two pixels higher
        // than the unreachable one's.
        if card_first {
            page.banded.body.remove_css_class("post-found");
        } else {
            page.banded.body.add_css_class("post-found");
        }
        if card_first {
            page.fields.reorder_child_after(&page.failed.area, None::<&gtk::Widget>);
            page.failed.area.add_css_class("post-first-card");
        } else {
            page.fields
                .reorder_child_after(&page.failed.area, Some(&page.hint.area));
        }
        page.down.set_visible(false);
        page.hosts.remove_css_class("post-tight");
        page.hosts.remove_css_class("post-after-refusal");
        page.incoming.set_visible(true);
        page.outgoing.set_visible(true);
        page.summary.set_visible(false);
        page.password_list.remove_css_class("post-wrong");
    }

    /// Shows both servers the password is about to go to, and asks for a
    /// yes when Penguin Mail guessed them.
    fn show_hosts(&self, proposal: &Proposal) {
        let page = &self.password;
        page.incoming.set_title(&add_account::incoming_title(proposal));
        page.incoming
            .set_subtitle(&add_account::server_row_line(&proposal.incoming));
        // Signing in again keeps the protocol the account has.
        let offer = add_account::pop3_line(proposal).filter(|_| self.again.is_none());
        page.pop3_offer.set_visible(offer.is_some());
        if let Some(line) = offer {
            page.pop3_offer.set_title(&line);
        }
        page.outgoing
            .set_subtitle(&add_account::server_row_line(&proposal.smtp));
        page.summary
            .set_subtitle(&add_account::servers_summary(proposal));
        page.confirmed.set_active(false);
        page.agree.set_visible(proposal.confirm);
    }

    fn update_sign_in(self: &Rc<Self>) {
        let ready = self.proposal.borrow().as_ref().is_some_and(|proposal| {
            add_account::can_sign_in(
                &self.password.password.text(),
                proposal,
                self.password.confirmed.is_active(),
                self.running.is_on(),
            )
        });
        self.password.sign_in.set_sensitive(ready);
        self.password.failed_retry.set_sensitive(ready);
    }

    /// Sign In: tries the servers on show, then discovery's other
    /// candidates while the failure is one that says nothing about the
    /// password. A candidate that needs a yes stops the run and waits for
    /// it.
    fn sign_in(self: &Rc<Self>) {
        let address = self.typed.borrow().clone();
        let proposal = self.proposal.borrow().clone();
        let (Some(address), Some(proposal)) = (address, proposal) else {
            return;
        };
        let password = self.password.password.text().to_string();
        let confirmed = self.password.confirmed.is_active();
        if !add_account::can_sign_in(&password, &proposal, confirmed, self.running.is_on())
            || !self.running.begin()
        {
            return;
        }
        let name = Some(self.password.name.text().trim().to_string())
            .filter(|name| !name.is_empty() && self.again.is_none());
        let ticket = self.asking.ask();
        self.password.sign_in.set_sensitive(false);
        self.password.failed_retry.set_sensitive(false);
        self.password.sign_in.set_label(&gettext("Signing In…"));
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            this.try_candidates(address, proposal, password, name, ticket)
                .await;
            // Every way out of the run ends here, so Sign In comes back
            // whether the run signed in, failed or lost its dialog.
            this.running.end();
            this.password.sign_in.set_label(&gettext("Sign In"));
            this.update_sign_in();
        });
    }

    /// Signs in to `proposal`, then to discovery's other candidates while
    /// the failure says nothing about the password.
    async fn try_candidates(
        self: &Rc<Self>,
        address: Address,
        mut proposal: Proposal,
        password: String,
        name: Option<String>,
        ticket: add_account::Ticket,
    ) {
        loop {
            let attempt = add_account::attempt(&address, &proposal, &password);
            let signed = match attempt.protocol {
                Protocol::Imap => self.core.sign_in_imap(attempt).await,
                Protocol::Pop3 => self.core.sign_in_pop3(attempt).await,
            };
            let err = match signed {
                // The account is kept whether or not the dialog is
                // still open, so the window hears about it either way.
                Ok(account) => {
                    if self.again.is_some() {
                        return self.finish(Done::SignedInAgain(account));
                    }
                    (self.done)(Done::Added {
                        account: account.clone(),
                        name,
                    });
                    if self.asking.wants(ticket) {
                        let stamp = post::stamp_for(&proposal.provider_name);
                        self.show_added(&account, Some(stamp), None);
                    }
                    return;
                }
                Err(err) => err,
            };
            if !self.asking.wants(ticket) {
                tracing::info!(error = %err, "a sign-in failed after its dialog moved on");
                return;
            }
            match add_account::after_failure(&err, &proposal, &address) {
                Outcome::TryNext(next) if !next.confirm => {
                    self.show_hosts(&next);
                    self.proposal.replace(Some((*next).clone()));
                    proposal = *next;
                }
                Outcome::TryNext(next) => {
                    // Say why the first servers failed above the
                    // guessed ones that now wait for a yes.
                    let failure = add_account::failure(&err, &proposal);
                    self.show_password(*next);
                    self.show_failure(&failure);
                    return;
                }
                Outcome::Failed(failure) => {
                    self.show_failure(&failure);
                    return;
                }
            }
        }
    }

    fn show_failure(self: &Rc<Self>, failure: &Failure) {
        let page = &self.password;
        let band = self.bands.borrow().get("password").cloned();
        let (stamp, provider) = band.map_or((None, None), |b| (b.stamp, b.provider));
        match &failure.kind {
            FailureKind::Unreachable {
                host,
                password_sent,
                ..
            } => {
                self.arrange_password(true);
                // Only the server that did not answer stays in the list,
                // marked, and leads to Server Settings.
                let outgoing_failed = *password_sent;
                page.incoming.set_visible(false);
                page.outgoing.set_visible(false);
                let (title, server) = match (outgoing_failed, self.proposal.borrow().as_ref()) {
                    (true, Some(p)) => (gettext("Outgoing"), add_account::server_row_line(&p.smtp)),
                    (false, Some(p)) => (
                        add_account::incoming_title(p),
                        add_account::server_row_line(&p.incoming),
                    ),
                    (_, None) => (String::new(), String::new()),
                };
                page.down.set_title(&title);
                page.down.set_subtitle(&server);
                page.down.set_visible(true);
                page.failed.show(
                    "trouble",
                    Ok("network-wireless-offline-symbolic"),
                    &failure.title,
                    &failure.body,
                    &failure.links,
                );
                page.failed.buttons.set_visible(true);
                page.title
                    .set_text(&fill(&gettext("Could not reach {host}"), &[("host", host)]));
                if !password_sent {
                    page.lede.set_text(&gettext("Your password was not sent."));
                }
                page.hosts.add_css_class("post-tight");
                page.hint.hide();
                self.set_band("password", Step::Unreachable, stamp, provider.as_deref());
            }
            FailureKind::Refused => {
                self.arrange_password(false);
                // A failure with pages to try carries the app password
                // page itself where one is due, and two cards offering it
                // would be one too many.
                page.hint.hide();
                page.failed.show(
                    "trouble",
                    Ok("dialog-warning-symbolic"),
                    &failure.title,
                    &failure.body,
                    &failure.links,
                );
                page.failed.show_said(failure.said.as_deref());
                page.failed.show_copy(failure.copy.as_deref());
                page.password_list.add_css_class("post-wrong");
                page.incoming.set_visible(false);
                page.outgoing.set_visible(false);
                page.summary.set_visible(true);
                page.hosts.add_css_class("post-after-refusal");
                self.set_band("password", Step::Refused, stamp, provider.as_deref());
            }
        }
        self.update_sign_in();
        // Sign In took the focus with it when it went insensitive. A
        // server that did not answer wants another try, not another
        // password.
        if matches!(failure.kind, FailureKind::Unreachable { .. }) {
            page.failed_retry.grab_focus();
        } else {
            page.password.grab_focus();
            page.password.set_position(-1);
        }
        page.failed
            .area
            .announce(&failure.line, gtk::AccessibleAnnouncementPriority::High);
    }

    // ---- Server Settings ---------------------------------------------------------

    /// Server Settings from the address page, filled from what was found
    /// for this address or with a guess.
    fn manual_from_address(self: &Rc<Self>) {
        let Some(address) = Address::parse(&self.address.address.text()) else {
            self.back_to("address");
            return self.say(&add_account::not_an_address());
        };
        // A lookup still running would take the person away from the
        // form they asked for.
        self.asking.forget();
        self.address.look.set_sensitive(true);
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
        self.proposal.replace(Some(proposal.clone()));
        self.fill_manual(proposal, said);
    }

    /// Fills Server Settings from `proposal` and shows it, leaving the
    /// proposal the dialog holds as it is.
    fn fill_manual(self: &Rc<Self>, proposal: &Proposal, said: Option<&str>) {
        let manual = &self.manual;
        manual.content.set_description(said.unwrap_or_default());
        // An outgoing name that only repeats the incoming one stays empty,
        // which the form reads as the same.
        let smtp_user = proposal
            .smtp_user
            .as_deref()
            .filter(|user| Some(*user) != proposal.incoming_user.as_deref());
        // The protocol first, so its handlers move nothing the rows below
        // are about to be filled with.
        manual
            .protocol
            .set_active(add_account::protocol_index(proposal.protocol));
        manual
            .incoming
            .fill(&proposal.incoming, proposal.incoming_user.as_deref());
        manual.smtp.fill(&proposal.smtp, smtp_user);
        manual.removal.fill(proposal.pop3_remove.unwrap_or_default());
        manual
            .removal
            .group
            .set_visible(proposal.protocol == Protocol::Pop3);
        manual.problem.set_visible(false);
        self.push("servers");
    }

    /// Server Settings with POP3 picked and the server discovery found
    /// for it. The proposal on the password page stays IMAP, so going
    /// back without Use These Settings signs in as the page shows.
    fn manual_as_pop3(self: &Rc<Self>) {
        let proposal = self.proposal.borrow().as_ref().and_then(add_account::as_pop3);
        if let Some(proposal) = proposal {
            self.fill_manual(&proposal, None);
        }
    }

    /// Moves the incoming rows to the server discovery found for the
    /// protocol just picked, while they still hold the other one.
    fn follow_protocol(self: &Rc<Self>) {
        let now = add_account::protocol_at(self.manual.protocol.active());
        let host = self.manual.incoming.host.text();
        let found = self
            .proposal
            .borrow()
            .as_ref()
            .and_then(|proposal| add_account::server_after_protocol(proposal, &host, now));
        if let Some(server) = found {
            self.manual.incoming.fill_server(&server);
        }
    }

    fn use_manual(self: &Rc<Self>) {
        let before = self.proposal.borrow().clone();
        let address = self.typed.borrow().clone();
        let (Some(before), Some(address)) = (before, address) else {
            return;
        };
        let protocol = add_account::protocol_at(self.manual.protocol.active());
        let typed = add_account::typed_servers(
            &self.manual.incoming.typed(),
            &self.manual.smtp.typed(),
            &before,
            &address,
        )
        .map(|proposal| {
            add_account::with_protocol(proposal, protocol, self.manual.removal.chosen())
        });
        match typed {
            Ok(proposal) => self.show_password(proposal),
            Err(problem) => {
                self.manual.problem.set_text(&problem);
                self.manual.problem.set_visible(true);
            }
        }
    }

    /// The password page for an account signing in again, from the
    /// servers it kept. An account whose servers are gone starts again
    /// from Server Settings.
    fn load_saved(self: &Rc<Self>, account: Account) {
        self.typed.replace(Address::parse(&account.email));
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let id = account.id;
            let saved = this
                .core
                .read(move |c| {
                    if let Some(servers) = servers::load(c, id)? {
                        return Ok(Some(add_account::Kept::Imap(servers)));
                    }
                    let Some(servers) = servers::load_pop3(c, id)? else {
                        return Ok(None);
                    };
                    let remove = accounts::pop3_remove(c, id)?;
                    Ok(Some(add_account::Kept::Pop3(servers, remove)))
                })
                .await;
            match saved {
                Ok(Some(kept)) => {
                    this.show_password(add_account::saved_proposal(&account, &kept))
                }
                Ok(None) => {
                    let typed = this.typed.borrow().clone();
                    if let Some(address) = typed {
                        this.show_password(add_account::guess(&address));
                        this.show_manual(&add_account::guess(&address), None);
                    }
                }
                Err(err) => this.password.failed.show(
                    "trouble",
                    Ok("dialog-warning-symbolic"),
                    &gettext("Could not sign in"),
                    &err.to_string(),
                    &[],
                ),
            }
        });
    }

    // ---- The browser -------------------------------------------------------------

    /// `browser`'s sign-in in the browser, with the page that waits for
    /// it. `expected` is the address typed, when there was one.
    fn start_browser(self: &Rc<Self>, browser: Browser, expected: Option<String>) {
        self.stop_browser();
        self.browser_provider.set(browser);
        let provider = browser.name();
        let page = &self.browser;
        page.title.set_text(&fill(
            &gettext("Sign in with {provider} in your browser"),
            &[("provider", provider)],
        ));
        page.lede.set_text(&fill(
            &gettext(
                "{provider}'s sign-in page is open in your browser. When you finish there, this window moves on.",
            ),
            &[("provider", provider)],
        ));
        fill_steps(&page.steps, browser);
        page.steps.set_visible(true);
        page.failed.hide();
        page.waiting.set_visible(true);
        page.again.set_visible(true);
        page.copy.set_visible(true);
        page.retry.set_visible(false);
        self.browser_expected.replace(expected.clone());
        let stamp = post::stamp_for(provider);
        self.set_band("browser", Step::Browser, Some(stamp), Some(provider));
        self.push("browser");
        // Without the build's client the browser would open for nothing,
        // so say why at once. The demo goes on to its own message.
        let built = match browser {
            Browser::Google => self.core.built_with_google_sign_in(),
            Browser::Microsoft => self.core.built_with_microsoft_sign_in(),
        };
        if !self.core.demo && !built {
            return self.browser_failed(&match browser {
                Browser::Google => gettext(
                    "This copy of Penguin Mail was built without Google sign-in. Get a release from github.com/c9dev/penguin-mail/releases.",
                ),
                Browser::Microsoft => gettext(
                    "This copy of Penguin Mail was built without Microsoft sign-in. Get a release from github.com/c9dev/penguin-mail/releases.",
                ),
            });
        }
        let ticket = self.asking.ask();
        let (urls, opened) = async_channel::unbounded::<String>();
        let (cancel, canceled) = async_channel::bounded::<()>(1);
        self.browser_cancel.replace(Some(cancel));
        self.count_down(Instant::now() + crate::core::BROWSER_WAIT);
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            while let Ok(url) = opened.recv().await {
                this.browser_url.replace(Some(url));
                this.open_page_again();
            }
        });
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let signed = match browser {
                Browser::Google => this
                    .core
                    .authorize_account(urls, expected, canceled)
                    .await
                    .map(|account| (account, None)),
                Browser::Microsoft => {
                    this.core
                        .authorize_microsoft(urls, expected, false, canceled)
                        .await
                }
            };
            match signed {
                // The account is kept whether or not the dialog is still
                // open, so the window hears about it either way.
                Ok((account, name)) => {
                    (this.done)(Done::BrowserAdded {
                        account: account.clone(),
                        name,
                    });
                    if this.asking.wants(ticket) {
                        this.stop_browser();
                        this.typed.replace(Address::parse(&account.email));
                        // Microsoft's consent is all or nothing, so only
                        // Google's page can leave a feature off.
                        let missing = match browser {
                            Browser::Google => {
                                crate::permission::withheld_permissions(crate::offered::withheld_for(
                                    this.core.account(account.id).as_ref().map(|s| s.services()),
                                ))
                            }
                            Browser::Microsoft => Vec::new(),
                        };
                        this.show_added(&account, Some(stamp), Some(&missing));
                    }
                }
                Err(err) if this.asking.wants(ticket) => {
                    this.stop_browser();
                    match add_account::keyring_unplugged(&err, crate::keyring_plug::current()) {
                        Some(unplugged) => this.browser_failed_with(&unplugged),
                        None => this.browser_failed(&err.to_string()),
                    }
                }
                Err(err) => tracing::info!(error = %err, "a browser sign-in ended after its dialog moved on"),
            }
        });
    }

    fn browser_failed(&self, said: &str) {
        self.browser_failed_with(&Failure {
            line: said.to_string(),
            title: gettext("Could not sign in"),
            body: said.to_string(),
            said: None,
            links: Vec::new(),
            copy: None,
            kind: FailureKind::Refused,
        });
    }

    fn browser_failed_with(&self, failure: &Failure) {
        let page = &self.browser;
        page.steps.set_visible(false);
        page.failed.show(
            "trouble",
            Ok("dialog-warning-symbolic"),
            &failure.title,
            &failure.body,
            &failure.links,
        );
        page.failed.show_said(failure.said.as_deref());
        page.failed.show_copy(failure.copy.as_deref());
        page.waiting.set_visible(false);
        page.again.set_visible(false);
        page.copy.set_visible(false);
        page.retry.set_visible(true);
        let provider = self.browser_provider.get().name();
        self.set_band("browser", Step::Refused, Some(post::stamp_for(provider)), Some(provider));
        page.retry.grab_focus();
        page.failed
            .area
            .announce(&failure.line, gtk::AccessibleAnnouncementPriority::High);
    }

    /// Counts the browser's time down to `deadline` once a second.
    fn count_down(self: &Rc<Self>, deadline: Instant) {
        self.browser_deadline.set(Some(deadline));
        self.show_time_left();
        let weak = Rc::downgrade(self);
        let timer = glib::timeout_add_local(Duration::from_millis(250), move || {
            match weak.upgrade() {
                Some(this) => {
                    this.show_time_left();
                    glib::ControlFlow::Continue
                }
                None => glib::ControlFlow::Break,
            }
        });
        if let Some(old) = self.browser_timer.replace(Some(timer)) {
            old.remove();
        }
    }

    fn show_time_left(&self) {
        let Some(deadline) = self.browser_deadline.get() else {
            return;
        };
        let left = deadline.saturating_duration_since(Instant::now());
        self.browser.left.set_text(&add_account::time_left(left));
        let whole = crate::core::BROWSER_WAIT.as_secs_f64();
        self.browser
            .progress
            .set_fraction((left.as_secs_f64() / whole).clamp(0.0, 1.0));
    }

    /// Stops the browser sign-in and its countdown. Dropping the cancel
    /// sender ends the wait in the core.
    fn stop_browser(&self) {
        let cancel = self.browser_cancel.borrow_mut().take();
        drop(cancel);
        let timer = self.browser_timer.borrow_mut().take();
        if let Some(timer) = timer {
            timer.remove();
        }
    }

    fn cancel_browser(self: &Rc<Self>) {
        self.asking.forget();
        self.stop_browser();
        if self.nav.previous_page(&self.browser.banded.page).is_some() {
            self.nav.pop();
        } else {
            self.window.close();
        }
    }

    fn open_page_again(&self) {
        let url = self.browser_url.borrow().clone();
        if let Some(url) = url {
            gtk::UriLauncher::new(&url).launch(
                self.window.root().and_downcast::<gtk::Window>().as_ref(),
                gio::Cancellable::NONE,
                |_| {},
            );
        }
    }

    fn copy_link(&self) {
        let url = self.browser_url.borrow().clone();
        if let Some(url) = url {
            self.window.clipboard().set_text(&url);
            self.browser.copy.announce(
                &gettext("Link copied"),
                gtk::AccessibleAnnouncementPriority::Medium,
            );
        }
    }

    // ---- Added ------------------------------------------------------------------

    /// The last page: the account is added and its first sync runs. With
    /// `withheld` features from Google's page, it offers Grant Access
    /// instead.
    fn show_added(
        self: &Rc<Self>,
        account: &Account,
        stamp: Option<Stamp>,
        withheld: Option<&[Permission]>,
    ) {
        let page = &self.finished;
        self.added.set(Some(account.id));
        let missing = withheld.unwrap_or_default();
        let grant = !missing.is_empty();
        page.title.set_xalign(if grant { 0.0 } else { 0.5 });
        page.lede.set_xalign(if grant { 0.0 } else { 0.5 });
        if grant {
            page.title.set_text(&fill(
                &gettext("{account} is added"),
                &[("account", &account.email)],
            ));
            let spoken = add_account::small_number(missing.len());
            let count = [("count", spoken.as_str())];
            page.lede.set_text(&if account.provider == mailrs_domain::Provider::Microsoft {
                fill_plural(
                    "Mail is downloading. You left one box unticked on Microsoft's page, so this feature stays off:",
                    "Mail is downloading. You left {count} boxes unticked on Microsoft's page, so these features stay off:",
                    missing.len(),
                    &count,
                )
            } else {
                fill_plural(
                    "Mail is downloading. You left one box unticked on Google's page, so this feature stays off:",
                    "Mail is downloading. You left {count} boxes unticked on Google's page, so these features stay off:",
                    missing.len(),
                    &count,
                )
            });
            while let Some(child) = page.withheld.first_child() {
                page.withheld.remove(&child);
            }
            for permission in missing {
                let (title, covers) = permission.feature(account.provider);
                page.withheld
                    .append(&icon_row(permission.feature_icon(), &title, &covers, false));
            }
        } else {
            page.title.set_text(&fill(
                &gettext("{account} is ready"),
                &[("account", &account.email)],
            ));
            page.lede
                .set_text(&gettext("Mail is downloading. You can close this window."));
        }
        for part in [
            page.folder_list.upcast_ref::<gtk::Widget>(),
            page.note.upcast_ref(),
            page.open_inbox.upcast_ref(),
            page.another.upcast_ref(),
        ] {
            part.set_visible(!grant);
        }
        for part in [
            page.withheld.upcast_ref::<gtk::Widget>(),
            page.grant_note.upcast_ref(),
            page.grant_buttons.upcast_ref(),
        ] {
            part.set_visible(grant);
        }
        for folder in &page.folders {
            let shown = add_account::shows_folder(account.provider, folder.role);
            if let Some(row) = folder.row.parent() {
                row.set_visible(shown);
            }
            folder.state.set_text(&add_account::folder_line(0));
            folder.progress.set_fraction(0.0);
        }
        let provider = account.provider_name().to_string();
        self.set_band("added", Step::Added, stamp, Some(&provider));
        self.band.set_letters(true);
        self.push("added");
        self.finished.note.set_text(
            &add_account::added_page(account.provider, RemoveSetting::Never, false, false).note,
        );
        self.count_folders(account.id, account.provider);
    }

    /// Reads the new account's folder counts every second and a half
    /// until its first download is done: the backfill for an account that
    /// has one, the first check for a POP3 account.
    fn count_folders(self: &Rc<Self>, account_id: AccountId, provider: Provider) {
        self.stop_counting();
        Self::read_counts(self, account_id, provider);
        let weak = Rc::downgrade(self);
        let timer = glib::timeout_add_local(Duration::from_millis(1500), move || {
            let Some(this) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            Self::read_counts(&this, account_id, provider);
            glib::ControlFlow::Continue
        });
        self.count_timer.replace(Some(timer));
    }

    fn read_counts(this: &Rc<Self>, account_id: AccountId, provider: Provider) {
        let this = this.clone();
        glib::spawn_future_local(async move {
            let read = this
                .core
                .read(move |c| {
                    let counts = mailrs_store::threads::mail_counts(c)?;
                    let backfill = mailrs_store::accounts::sync_cursor(c, account_id)?.backfill_done;
                    let (remove, first_check) = if provider == Provider::Pop3 {
                        (
                            mailrs_store::accounts::pop3_remove(c, account_id)?,
                            mailrs_store::pop3::first_check_finished(c, account_id)?,
                        )
                    } else {
                        (RemoveSetting::Never, false)
                    };
                    Ok((counts, add_account::added_page(provider, remove, backfill, first_check)))
                })
                .await;
            if let Ok((counts, page)) = read
                && this.added.get() == Some(account_id)
            {
                this.finished.note.set_text(&page.note);
                this.show_counts(account_id, &counts, page.done);
            }
        });
    }

    fn show_counts(&self, account_id: AccountId, counts: &mailrs_store::threads::MailCounts, done: bool) {
        for folder in &self.finished.folders {
            let count = counts
                .account(account_id, &MailSet::Role(folder.role))
                .threads
                .max(0) as usize;
            folder.state.set_text(&add_account::folder_line(count));
            if done {
                folder.progress.set_fraction(1.0);
            } else if count > 0 {
                folder.progress.pulse();
            }
        }
        if done {
            self.band.set_letters(false);
            self.stop_counting();
        }
    }

    fn stop_counting(&self) {
        let timer = self.count_timer.borrow_mut().take();
        if let Some(timer) = timer {
            timer.remove();
        }
    }

    /// Add Another Account: back to the tiles, with the forms emptied.
    fn add_another(self: &Rc<Self>) {
        self.stop_counting();
        self.added.set(None);
        self.typed.replace(None);
        self.address.address.set_text("");
        self.password.password.set_text("");
        self.password.name.set_text("");
        self.set_band("pick", Step::Pick, None, None);
        self.nav.replace_with_tags(&["pick"]);
    }
}

// ---- Demo stages ------------------------------------------------------------------

/// Puts the dialog on one of its stages with sample data, for screenshots
/// of the demo (`MAILRS_DEMO_ADD_ACCOUNT`). Each stage goes through the
/// same calls the real flow makes; only the answers are made up, since
/// the demo signs in nowhere.
fn preview(this: &Rc<Dialog>, stage: &str) {
    use mailrs_discover::{Candidate, Found, Security, Source, UserName, Verdict};
    use mailrs_imap::{CheckError, ImapError};
    let server = |host: &str, port| Server {
        host: host.into(),
        port,
        security: Security::Tls,
        user_name: UserName::Address,
    };
    let studio = Address::parse("dana@reyes.studio").expect("a sample address");
    let fastmail_candidate = || Candidate {
        source: Source::Mx,
        provider: mailrs_discover::provider_named("Fastmail"),
        imap: Some(server("imap.fastmail.com", 993)),
        smtp: server("smtp.fastmail.com", 465),
        pop3: None,
        confirm: false,
    };
    let fastmail = || {
        let found = Found {
            verdict: Verdict::Servers,
            candidates: vec![fastmail_candidate()],
        };
        match add_account::after_discovery(found, &studio, true) {
            Next::Password(proposal) => proposal,
            _ => add_account::guess(&studio),
        }
    };
    match stage {
        "other" => {
            this.open_address(None, false);
            this.address.address.set_text("dana.reyes@icloud.com");
            // The page takes the focus as it shows, which selects the
            // text; the mockup has the cursor at its end.
            let field = this.address.address.clone();
            glib::timeout_add_local_once(Duration::from_millis(900), move || {
                field.grab_focus();
                field.set_position(-1);
            });
        }
        "browser" | "browser-microsoft" => {
            let browser = if stage == "browser" {
                Browser::Google
            } else {
                Browser::Microsoft
            };
            this.start_browser(browser, None);
            // The demo cannot sign in, so its answer never comes: the
            // page keeps waiting, as it would for a person still on the
            // provider's page, with the mockup's time left.
            this.asking.forget();
            let left = match browser {
                Browser::Google => 252,
                Browser::Microsoft => 220,
            };
            this.count_down(Instant::now() + Duration::from_secs(left));
            this.browser.failed.hide();
            this.browser.steps.set_visible(true);
            this.browser.waiting.set_visible(true);
            this.browser.again.set_visible(true);
            this.browser.copy.set_visible(true);
            this.browser.retry.set_visible(false);
            let provider = browser.name();
            this.set_band("browser", Step::Browser, Some(post::stamp_for(provider)), Some(provider));
        }
        "lookup" => {
            this.typed.replace(Some(studio.clone()));
            this.checks.replace(Checks::default());
            this.lookup.title.set_text(&fill(
                &gettext("Looking up {domain}"),
                &[("domain", &studio.domain)],
            ));
            this.show_checks();
            this.set_band("lookup", Step::Lookup, Some(post::ANY_SERVER), None);
            this.push("address");
            this.push("lookup");
        }
        "found" | "wrong" => {
            this.typed.replace(Some(studio.clone()));
            this.push("address");
            this.show_password(fastmail());
            this.password.name.set_text("Dana Reyes");
            if stage == "wrong" {
                this.password.password.set_text("hunter2hunter");
                let refused = anyhow::Error::new(CheckError::Imap(ImapError::Auth {
                    text: "[AUTHENTICATIONFAILED]".into(),
                }));
                let failure = add_account::failure(&refused, &fastmail());
                this.show_failure(&failure);
            }
        }
        "unreachable" => {
            this.typed.replace(Some(studio.clone()));
            this.push("address");
            let proposal = Proposal {
                incoming: server("mail.reyes.studio", 993),
                smtp: server("mail.reyes.studio", 465),
                ..add_account::guess(&studio)
            };
            this.show_password(proposal.clone());
            this.password.name.set_text("Dana Reyes");
            this.password.password.set_text("correcthorse");
            let gone = anyhow::Error::new(CheckError::Imap(ImapError::Network(
                "connection timed out".into(),
            )));
            this.show_failure(&add_account::failure(&gone, &proposal));
        }
        // POP3: Fastmail offers it beside IMAP, so step 2 has the row
        // that leads to it, and Server Settings opens on it from there.
        "pop3-offer" | "pop3-servers" | "pop3-days" | "pop3-checking" | "pop3-failed" => {
            let found = Found {
                verdict: Verdict::Servers,
                candidates: vec![Candidate {
                    pop3: Some(server("pop.fastmail.com", 995)),
                    ..fastmail_candidate()
                }],
            };
            let Next::Password(imap) = add_account::after_discovery(found, &studio, true) else {
                return;
            };
            this.typed.replace(Some(studio.clone()));
            this.push("address");
            this.show_password(imap.clone());
            this.password.name.set_text("Dana Reyes");
            let Some(pop3) = add_account::as_pop3(&imap) else {
                return;
            };
            match stage {
                "pop3-servers" => this.manual_as_pop3(),
                "pop3-days" => {
                    this.manual_as_pop3();
                    this.manual.removal.fill(RemoveSetting::Days(30));
                }
                "pop3-checking" | "pop3-failed" => {
                    this.show_password(pop3.clone());
                    this.password.name.set_text("Dana Reyes");
                    this.password.password.set_text("correcthorse");
                    if stage == "pop3-checking" {
                        this.password.sign_in.set_sensitive(false);
                        this.password.sign_in.set_label(&gettext("Signing In…"));
                    } else {
                        let no_uidl = ImapError::Refused(gettext(
                            "This server cannot tell its messages apart, so Penguin Mail cannot download from it safely.",
                        ));
                        // That is what `in_imap_words` makes of a server
                        // without UIDL; the app does not link the POP3
                        // crate to build it from one.
                        let failed = anyhow::Error::new(CheckError::Imap(no_uidl));
                        this.show_failure(&add_account::failure(&failed, &pop3));
                    }
                }
                _ => {}
            }
        }
        "closed" => {
            let tuta = Address::parse("dana@tuta.com").expect("a sample address");
            this.typed.replace(Some(tuta.clone()));
            this.push("address");
            this.show_closed(&tuta);
        }
        "servers" => {
            this.typed.replace(Some(studio.clone()));
            this.push("address");
            this.show_password(fastmail());
            this.manual_from_password();
        }
        "added" | "grant" | "pop3-added" => {
            let grant = stage == "grant";
            let pop3 = stage == "pop3-added";
            let this = Rc::clone(this);
            glib::spawn_future_local(async move {
                let accounts = this
                    .core
                    .read(mailrs_store::accounts::list_accounts)
                    .await
                    .unwrap_or_default();
                let Some(first) = accounts.into_iter().next() else {
                    return;
                };
                // The mockup's addresses, on the demo's first account so
                // Open Inbox has somewhere to go.
                let account = |email: &str, provider, name: &str| Account {
                    email: email.into(),
                    provider,
                    provider_name: Some(name.into()),
                    ..first.clone()
                };
                if grant {
                    let account = account(
                        "dana.reyes@gmail.com",
                        mailrs_domain::Provider::Gmail,
                        "Google",
                    );
                    this.typed.replace(Address::parse(&account.email));
                    this.show_added(
                        &account,
                        Some(post::stamp_for("Google")),
                        Some(&[Permission::Settings, Permission::ManageCalendars]),
                    );
                } else {
                    let account = account(
                        "dana@reyes.studio",
                        if pop3 {
                            mailrs_domain::Provider::Pop3
                        } else {
                            mailrs_domain::Provider::Imap
                        },
                        "Fastmail",
                    );
                    this.show_added(&account, Some(post::stamp_for("Fastmail")), None);
                    // The demo's first sync has long finished; the stage
                    // shows one still running.
                    this.stop_counting();
                    this.band.set_letters(true);
                    let counts = [312, 0, 0];
                    for (folder, count) in this.finished.folders.iter().zip(counts) {
                        folder.state.set_text(&add_account::folder_line(count));
                        folder.progress.set_fraction(if count > 0 { 0.26 } else { 0.0 });
                    }
                }
            });
        }
        _ => {}
    }
    this.show_band();
}
