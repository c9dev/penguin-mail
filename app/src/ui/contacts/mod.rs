//! Contacts, the third mode of the main window: lists in the sidebar, a
//! list of people beside it, and the person beside that.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::{gio, glib};
use mailrs_domain::translate::{fill, fill_plural, gettext};
use mailrs_domain::{Account, AccountId};
use mailrs_store::address_book::{self, Contact, Link, MailWith};
use mailrs_store::contacts::Correspondent;
use mailrs_store::image_senders::ImageSender;

use crate::app::App;
use crate::format::account_label;
use crate::settings::Change;

mod editor;
mod ops;
mod people;
mod person;
mod photo;
pub mod parts;
mod side;

use people::{Merge, Pair, Person};

/// What the window lends the page.
pub struct Hooks {
    /// Searches mail for an address, in the Mail space.
    pub search_mail: Box<dyn Fn(Vec<String>)>,
    /// Opens a conversation in the Mail space.
    pub open_thread: Box<dyn Fn(AccountId, String)>,
}

/// A list the sidebar offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum List {
    All,
    Vips,
    FromMail,
    SameAddress,
    SamePerson,
    Group(String),
    Account(AccountId),
}

/// What the person pane shows.
#[derive(Clone)]
pub(super) enum Pane {
    /// The row that is selected.
    View,
    /// An edit of one contact of the selected person.
    Edit(Box<Contact>),
    /// A contact nobody has saved yet.
    New,
}

/// A row of the list pane, ready to draw.
#[derive(Clone)]
pub(super) struct Row {
    pub id: String,
    pub name: String,
    pub sub: String,
    pub accounts: Vec<AccountId>,
    pub kind: RowKind,
    pub checked: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum RowKind {
    Person(usize),
    Merge(usize),
    Pair(usize),
    Mail(usize),
}

/// Everything read from the store.
#[derive(Default)]
pub(super) struct Data {
    pub contacts: Vec<Contact>,
    pub links: Vec<Link>,
    pub correspondents: Vec<Correspondent>,
    pub seen: HashMap<String, i64>,
    pub images: Vec<ImageSender>,
    pub accounts: Vec<Account>,
    /// The accounts that hold a contact, which the demo counts as on.
    pub held: HashSet<AccountId>,
}

/// What one Undo does.
pub(super) type UndoRun = Box<dyn FnOnce(&Rc<Contacts>)>;

#[derive(Default)]
pub(super) struct State {
    pub data: Data,
    pub list: Option<List>,
    pub query: String,
    /// Grouped people for All, VIPs and groups; one account's for an
    /// account list.
    pub people: Vec<Person>,
    pub merges: Vec<Merge>,
    pub pairs: Vec<Pair>,
    pub rows: Vec<Row>,
    pub selected: Option<String>,
    pub select: Option<HashSet<String>>,
    pub pane: Option<Pane>,
    pub undo: Option<UndoRun>,
    pub mail: HashMap<String, MailWith>,
    pub keys: HashMap<String, bool>,
    pub generation: u64,
    pub lane_timer: Option<glib::SourceId>,
    /// Which contact each seg of the merge page keeps, by pair row id.
    pub keep: HashMap<String, usize>,
    /// Whose name a link shows.
    pub shown_a: HashMap<String, bool>,
    /// Accounts the demo turned on this session.
    pub turned_on: HashSet<AccountId>,
}

pub struct Contacts {
    pub(super) app: Weak<App>,
    pub(super) hooks: Hooks,
    /// The list and the person, and the Undo lane under them.
    pub page: gtk::Widget,
    /// Lists, groups and accounts, for the window's sidebar.
    pub sidebar: gtk::Widget,
    pub(super) side_box: gtk::Box,
    pub(super) head: gtk::Box,
    pub(super) search_row: gtk::Box,
    pub(super) search: gtk::SearchEntry,
    pub(super) list_stack: gtk::Stack,
    pub(super) status: gtk::Box,
    pub(super) store: gio::ListStore,
    pub(super) selection: gtk::SingleSelection,
    pub(super) view: gtk::ListView,
    pub(super) pane: gtk::Box,
    pub(super) nav: adw::NavigationSplitView,
    pub(super) lane: gtk::Revealer,
    pub(super) lane_text: gtk::Label,
    pub(super) lane_undo: gtk::Button,
    pub(super) state: RefCell<State>,
    pub(super) quiet: Cell<bool>,
    pub(super) back: gtk::Button,
    /// Says the People API is off in the person's Google project.
    pub(super) banner: adw::Banner,
    pub(super) new_message: RefCell<Option<gtk::Button>>,
}

fn item_row(item: &glib::Object) -> Option<Row> {
    item.downcast_ref::<glib::BoxedAnyObject>()
        .map(|o| o.borrow::<Row>().clone())
}

impl Contacts {
    pub fn new(app: &Rc<App>, hooks: Hooks) -> Rc<Self> {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let selection = gtk::SingleSelection::builder()
            .model(&store)
            .autoselect(false)
            .can_unselect(true)
            .build();
        let factory = gtk::SignalListItemFactory::new();
        let view = gtk::ListView::builder()
            .model(&selection)
            .factory(&factory)
            .single_click_activate(true)
            .css_classes(["cts-rows"])
            .build();
        super::name(&view, &gettext("Contacts"));
        let head = gtk::Box::builder()
            .css_classes(["cts-listhead"])
            .spacing(8)
            .build();
        let search = gtk::SearchEntry::builder()
            .hexpand(true)
            .css_classes(["cts-search"])
            .search_delay(120)
            .build();
        let search_row = gtk::Box::builder().css_classes(["cts-searchrow"]).build();
        search_row.append(&search);
        let status = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .valign(gtk::Align::Center)
            .halign(gtk::Align::Center)
            .css_classes(["cts-status"])
            .build();
        let list_stack = gtk::Stack::builder().vexpand(true).build();
        list_stack.add_named(
            &gtk::ScrolledWindow::builder()
                .child(&view)
                .vexpand(true)
                .hscrollbar_policy(gtk::PolicyType::Never)
                .build(),
            Some("rows"),
        );
        list_stack.add_named(&status, Some("status"));
        let list_pane = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(["cts-list"])
            .build();
        list_pane.append(&head);
        list_pane.append(&search_row);
        list_pane.append(&list_stack);
        let pane = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(["cts-person"])
            .hexpand(true)
            .build();
        let back = parts::icon_button("back", &gettext("Back"), &["back"]);
        let nav = adw::NavigationSplitView::builder()
            .sidebar_width_fraction(0.3)
            .min_sidebar_width(320.0)
            .max_sidebar_width(320.0)
            .show_content(false)
            .build();
        nav.set_sidebar(Some(
            &adw::NavigationPage::builder()
                .title(gettext("Contacts"))
                .child(&list_pane)
                .build(),
        ));
        nav.set_content(Some(
            &adw::NavigationPage::builder()
                .title(gettext("Contact"))
                .child(&pane)
                .build(),
        ));
        nav.add_css_class("cts-split");
        back.set_visible(false);
        nav.bind_property("collapsed", &back, "visible")
            .sync_create()
            .build();
        // The window folds to the list alone when the page cannot hold
        // both columns.
        let bin = adw::BreakpointBin::builder()
            .width_request(280)
            .height_request(200)
            .child(&nav)
            .build();
        let fold = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            700.0,
            adw::LengthUnit::Px,
        ));
        fold.add_setter(&nav, "collapsed", Some(&true.to_value()));
        bin.add_breakpoint(fold);
        let lane_text = gtk::Label::builder()
            .wrap(true)
            .xalign(0.0)
            .max_width_chars(60)
            .build();
        let lane_undo = gtk::Button::builder()
            .label(gettext("Undo"))
            .css_classes(["flat"])
            .build();
        let pill = gtk::Box::builder()
            .spacing(12)
            .halign(gtk::Align::Center)
            .css_classes(["cts-toast"])
            .build();
        pill.append(&lane_text);
        pill.append(&lane_undo);
        let lane = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideUp)
            .child(&pill)
            .build();
        let page = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(["cts-page"])
            .build();
        bin.set_vexpand(true);
        let banner = adw::Banner::builder()
            .title(gettext("Turn on Contacts for your Google project"))
            .button_label(gettext("Open Google Cloud"))
            .build();
        page.append(&banner);
        page.append(&bin);
        page.append(&lane);
        let side_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .css_classes(["cts-side"])
            .build();
        let sidebar = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&side_box)
            .build();
        let this = Rc::new(Self {
            app: Rc::downgrade(app),
            hooks,
            page: page.upcast(),
            sidebar: sidebar.upcast(),
            side_box,
            head,
            search_row,
            search,
            list_stack,
            status,
            store,
            selection,
            view,
            pane,
            nav,
            lane,
            lane_text,
            lane_undo,
            state: RefCell::default(),
            quiet: Cell::new(false),
            back,
            banner,
            new_message: RefCell::new(None),
        });
        this.wire(&factory);
        this.reload();
        this
    }

    fn wire(self: &Rc<Self>, factory: &gtk::SignalListItemFactory) {
        let weak = Rc::downgrade(self);
        factory.connect_setup(move |_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().expect("list items");
            let holder = gtk::Box::builder().css_classes(["cts-row-box"]).build();
            // Control-click or Meta-click opens Select with this row.
            let click = gtk::GestureClick::new();
            click.set_propagation_phase(gtk::PropagationPhase::Capture);
            let (weak, item_weak) = (weak.clone(), item.downgrade());
            click.connect_pressed(move |gesture, _, _, _| {
                let held = gesture.current_event_state();
                if held.intersects(gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::META_MASK)
                    && let (Some(this), Some(item)) = (weak.upgrade(), item_weak.upgrade())
                    && let Some(row) = item.item().as_ref().and_then(item_row)
                    && this.state.borrow().select.is_none()
                    && this.picks_here()
                {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    this.start_select(Some(row.id));
                }
            });
            holder.add_controller(click);
            item.set_child(Some(&holder));
        });
        let weak = Rc::downgrade(self);
        factory.connect_bind(move |_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().expect("list items");
            let (Some(this), Some(holder), Some(row)) = (
                weak.upgrade(),
                item.child().and_downcast::<gtk::Box>(),
                item.item().as_ref().and_then(item_row),
            ) else {
                return;
            };
            while let Some(child) = holder.first_child() {
                holder.remove(&child);
            }
            let (picking, query) = {
                let state = this.state.borrow();
                (state.select.is_some(), state.query.clone())
            };
            if picking {
                let tick = gtk::Image::builder()
                    .icon_name("penguin-mail-ct-ok-symbolic")
                    .pixel_size(12)
                    .halign(gtk::Align::Center)
                    .valign(gtk::Align::Center)
                    .width_request(18)
                    .height_request(18)
                    .css_classes(["cts-tick"])
                    .build();
                if row.checked {
                    tick.add_css_class("on");
                } else {
                    tick.set_icon_name(None);
                }
                holder.append(&tick);
            }
            holder.append(&parts::avatar(&row.name, 36, None));
            let text = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .hexpand(true)
                .valign(gtk::Align::Center)
                .build();
            let title = gtk::Label::builder()
                .use_markup(true)
                .label(people::highlight(&row.name, &query))
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes(["cts-name"])
                .build();
            text.append(&title);
            if !row.sub.is_empty() {
                text.append(
                    &gtk::Label::builder()
                        .use_markup(true)
                        .label(people::highlight(&row.sub, &query))
                        .xalign(0.0)
                        .ellipsize(gtk::pango::EllipsizeMode::End)
                        .css_classes(["cts-subline"])
                        .build(),
                );
            }
            if !row.accounts.is_empty() {
                let chips = parts::chips(&row.accounts, 4);
                chips.add_css_class("cts-rowchips");
                text.append(&chips);
            }
            holder.append(&text);
            let account_names: Vec<String> = row
                .accounts
                .iter()
                .map(|a| account_label(*a).short)
                .collect();
            let said = [row.name.clone(), row.sub.clone(), account_names.join(", ")]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(", ");
            item.set_accessible_label(&said);
        });
        let weak = Rc::downgrade(self);
        self.selection.connect_selected_notify(move |selection| {
            let Some(this) = weak.upgrade() else { return };
            if this.quiet.get() {
                return;
            }
            let id = selection
                .selected_item()
                .as_ref()
                .and_then(item_row)
                .map(|r| r.id);
            if this.state.borrow().select.is_some() {
                return;
            }
            this.state.borrow_mut().selected = id;
            this.state.borrow_mut().pane = Some(Pane::View);
            this.show_pane();
        });
        let weak = Rc::downgrade(self);
        self.view.connect_activate(move |_, at| {
            let Some(this) = weak.upgrade() else { return };
            let Some(row) = this.store.item(at).as_ref().and_then(item_row) else {
                return;
            };
            if this.state.borrow().select.is_some() {
                this.toggle_pick(&row.id);
            } else {
                this.nav.set_show_content(true);
            }
        });
        let weak = Rc::downgrade(self);
        self.back.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.nav.set_show_content(false);
            }
        });
        let weak = Rc::downgrade(self);
        self.search.connect_search_changed(move |entry| {
            if let Some(this) = weak.upgrade() {
                this.state.borrow_mut().query = entry.text().trim().to_string();
                this.rebuild();
            }
        });
        let weak = Rc::downgrade(self);
        self.lane_undo.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.undo();
            }
        });
        let weak = Rc::downgrade(self);
        let opened = Rc::new(Cell::new(false));
        self.banner.connect_button_clicked(move |banner| {
            let Some(this) = weak.upgrade() else { return };
            let Some(app) = this.app.upgrade() else { return };
            if opened.replace(true) {
                // The second press asks Google again.
                banner.set_revealed(false);
                opened.set(false);
                banner.set_button_label(Some(&gettext("Open Google Cloud")));
                app.refresh_contacts(true);
                return;
            }
            if let Some((_, url)) = app.contacts_api_off.borrow().clone() {
                gtk::UriLauncher::new(&url).launch(
                    banner.root().and_downcast_ref::<gtk::Window>(),
                    gio::Cancellable::NONE,
                    |_| {},
                );
            }
            banner.set_button_label(Some(&gettext("Try Again")));
        });
        // Keys: / and Ctrl+F search, C goes to New Message. Typing in a
        // field keeps its own keys.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let Some(this) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if this.key(key, modifiers) {
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        self.page.add_controller(keys);
    }

    /// Whether a key in the page was used.
    fn key(self: &Rc<Self>, key: gtk::gdk::Key, modifiers: gtk::gdk::ModifierType) -> bool {
        use gtk::gdk::{Key, ModifierType};
        let typing = self
            .page
            .root()
            .and_then(|r| r.focus())
            .is_some_and(|w| w.is::<gtk::Text>() || w.is::<gtk::TextView>() || w.is::<gtk::Editable>());
        let plain = !modifiers.intersects(
            ModifierType::CONTROL_MASK | ModifierType::ALT_MASK | ModifierType::META_MASK,
        );
        match key {
            Key::f | Key::F if modifiers.contains(ModifierType::CONTROL_MASK) => {
                self.focus_search();
                true
            }
            Key::slash if plain && !typing => {
                self.focus_search();
                true
            }
            Key::c | Key::C if plain && !typing => self.focus_new_message(),
            Key::Escape if self.state.borrow().select.is_some() && !typing => {
                self.stop_select();
                true
            }
            _ => false,
        }
    }

    pub fn focus_search(&self) {
        self.nav.set_show_content(false);
        if self.search_row.is_visible() {
            self.search.grab_focus();
        }
    }

    fn focus_new_message(&self) -> bool {
        match self.new_message.borrow().as_ref() {
            Some(b) if b.is_mapped() => b.grab_focus(),
            _ => false,
        }
    }

    pub(super) fn accounts_with_contacts(&self) -> Vec<Account> {
        let Some(app) = self.app.upgrade() else {
            return Vec::new();
        };
        app.accounts()
            .into_iter()
            .filter(|a| self.reads(a))
            .collect()
    }

    /// Reads the store again and redraws.
    pub fn reload(self: &Rc<Self>) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let generation = {
            let mut state = self.state.borrow_mut();
            state.generation += 1;
            state.generation
        };
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let loaded = app
                .core
                .read(|c| {
                    let contacts = address_book::list(c)?;
                    let links = address_book::links(c)?;
                    let correspondents = mailrs_store::contacts::list_correspondents(c)?;
                    let images = mailrs_store::image_senders::list(c)?;
                    let seen = mailrs_store::address_book::message_counts(
                        c,
                        &correspondents.iter().map(|x| x.email.clone()).collect::<Vec<_>>(),
                    )?;
                    Ok((contacts, links, correspondents, images, seen))
                })
                .await;
            let Some(this) = weak.upgrade() else { return };
            if this.state.borrow().generation != generation {
                return;
            }
            match loaded {
                Ok((mut contacts, links, correspondents, images, seen)) => {
                    let held: HashSet<AccountId> = contacts.iter().map(|c| c.account_id).collect();
                    this.state.borrow_mut().data.held = held.clone();
                    let accounts = this.accounts_with_contacts();
                    contacts.retain(|c| accounts.iter().any(|a| a.id == c.account_id));
                    let mut state = this.state.borrow_mut();
                    state.data = Data {
                        contacts,
                        links,
                        correspondents,
                        seen,
                        images,
                        accounts: app.accounts(),
                        held,
                    };
                    state.mail.clear();
                    state.keys.clear();
                }
                Err(err) => {
                    this.tell(
                        &fill(
                            &gettext("Could not load contacts: {reason}"),
                            &[("reason", &err.to_string())],
                        ),
                        None,
                    );
                    return;
                }
            }
            this.rebuild();
        });
    }

    /// Whether the signed-in accounts keep contacts on this computer.
    pub(super) fn reads(&self, account: &Account) -> bool {
        self.app
            .upgrade()
            .is_some_and(|app| {
                let state = self.state.borrow();
                match app.core.demo {
                    // The demo keeps its contacts in the store, so an
                    // account is on when it holds some, or once turned on.
                    true => {
                        state.data.held.contains(&account.id)
                            || state.turned_on.contains(&account.id)
                    }
                    false => app.settings().reads_contacts(&account.email),
                }
            })
    }

    pub(super) fn is_vip(&self, person: &Person) -> bool {
        let Some(app) = self.app.upgrade() else {
            return false;
        };
        let settings = app.settings();
        let state = self.state.borrow();
        person
            .emails(&state.data.contacts)
            .iter()
            .any(|e| settings.is_vip(e))
    }

    pub(super) fn current_list(&self) -> List {
        self.state.borrow().list.clone().unwrap_or(List::All)
    }

    pub fn show_list(self: &Rc<Self>, list: List) {
        {
            let mut state = self.state.borrow_mut();
            state.list = Some(list);
            state.selected = None;
            state.pane = Some(Pane::View);
            state.select = None;
        }
        self.search.set_text("");
        self.state.borrow_mut().query.clear();
        self.nav.set_show_content(false);
        self.rebuild();
    }

    /// Computes the rows of the current list and draws the sidebar, the
    /// list and the person again.
    pub(super) fn rebuild(self: &Rc<Self>) {
        let Some(app) = self.app.upgrade() else { return };
        {
            let mut state = self.state.borrow_mut();
            if state.list.is_none() {
                state.list = Some(List::All);
            }
            let list = state.list.clone().unwrap_or(List::All);
            let d = &state.data;
            let grouped = people::group(&d.contacts, &d.links);
            let merges = people::same_address(&d.contacts);
            let pairs = people::same_person(&d.contacts, &d.links);
            let settings = app.settings();
            let query = state.query.clone();
            let mut rows: Vec<Row> = Vec::new();
            let mut people_for_rows: Vec<Person> = Vec::new();
            let contacts = &d.contacts;
            let describe = |p: &Person, accounts: bool, idx: usize| Row {
                id: format!("p:{}/{}", contacts[p.members[0]].account_id, contacts[p.members[0]].resource),
                name: p.name.clone(),
                sub: p.email.clone(),
                accounts: if accounts { p.accounts.clone() } else { Vec::new() },
                kind: RowKind::Person(idx),
                checked: false,
            };
            match &list {
                List::All | List::Vips | List::Group(_) => {
                    for p in &grouped {
                        let keep = match &list {
                            List::Vips => p.emails(contacts).iter().any(|e| settings.is_vip(e)),
                            List::Group(g) => p
                                .members
                                .iter()
                                .any(|&i| contacts[i].details.groups.contains(g)),
                            _ => true,
                        };
                        if keep && (query.is_empty() || people::matches(p, contacts, &query)) {
                            people_for_rows.push(p.clone());
                        }
                    }
                    for (idx, p) in people_for_rows.iter().enumerate() {
                        rows.push(describe(p, true, idx));
                    }
                }
                List::Account(id) => {
                    for p in people::of_account(contacts, *id) {
                        if query.is_empty() || people::matches(&p, contacts, &query) {
                            people_for_rows.push(p);
                        }
                    }
                    for (idx, p) in people_for_rows.iter().enumerate() {
                        rows.push(describe(p, false, idx));
                    }
                }
                List::SameAddress => {
                    for (idx, m) in merges.iter().enumerate() {
                        let (keep, drop) = (&contacts[m.keep], &contacts[m.drop]);
                        let name = keep.display().to_string();
                        if !query.is_empty()
                            && !name.to_lowercase().contains(&query.to_lowercase())
                        {
                            continue;
                        }
                        rows.push(Row {
                            id: format!("m:{}/{}|{}", keep.account_id, keep.resource, drop.resource),
                            name,
                            sub: fill(
                                &gettext("Deletes {name} in {account}"),
                                &[
                                    ("name", drop.display()),
                                    ("account", &account_label(m.account).short),
                                ],
                            ),
                            accounts: Vec::new(),
                            kind: RowKind::Merge(idx),
                            checked: false,
                        });
                    }
                }
                List::SamePerson => {
                    for (idx, pr) in pairs.iter().enumerate() {
                        let a = &contacts[pr.a];
                        let name = a.display().to_string();
                        if !query.is_empty()
                            && !name.to_lowercase().contains(&query.to_lowercase())
                        {
                            continue;
                        }
                        rows.push(Row {
                            id: format!("s:{}/{}|{}", a.account_id, a.resource, contacts[pr.b].resource),
                            name,
                            sub: gettext("Keeps both contacts. Nothing is deleted."),
                            accounts: Vec::new(),
                            kind: RowKind::Pair(idx),
                            checked: false,
                        });
                    }
                }
                List::FromMail => {
                    for (idx, c) in from_mail(&state.data).into_iter().enumerate() {
                        let correspondent = &state.data.correspondents[c];
                        let name = correspondent
                            .name
                            .clone()
                            .filter(|n| !n.trim().is_empty())
                            .unwrap_or_else(|| correspondent.email.clone());
                        let seen = state
                            .data
                            .seen
                            .get(&correspondent.email.to_lowercase())
                            .copied()
                            .unwrap_or(1);
                        if !query.is_empty()
                            && !format!("{name} {}", correspondent.email)
                                .to_lowercase()
                                .contains(&query.to_lowercase())
                        {
                            continue;
                        }
                        let _ = idx;
                        rows.push(Row {
                            id: format!("c:{}", correspondent.email.to_lowercase()),
                            name,
                            sub: fill_plural(
                                "Seen in {count} message",
                                "Seen in {count} messages",
                                seen as usize,
                                &[("count", &seen.to_string())],
                            ),
                            accounts: Vec::new(),
                            kind: RowKind::Mail(c),
                            checked: false,
                        });
                    }
                }
            }
            state.people = match &list {
                List::SameAddress | List::SamePerson | List::FromMail => Vec::new(),
                _ => people_for_rows,
            };
            state.merges = merges;
            state.pairs = pairs;
            state.rows = rows;
            // A selection that vanished falls to the first row, as the
            // mockup opens each list on its first row.
            let selected_ok = state
                .selected
                .as_ref()
                .is_some_and(|id| state.rows.iter().any(|r| &r.id == id));
            if !selected_ok {
                state.selected = state.rows.first().map(|r| r.id.clone());
            }
            let ids: HashSet<String> = state.rows.iter().map(|r| r.id.clone()).collect();
            if let Some(pick) = state.select.as_mut() {
                pick.retain(|id| ids.contains(id));
            }
        }
        self.banner
            .set_revealed(app.contacts_api_off.borrow().is_some());
        self.draw_side();
        self.draw_list();
        self.show_pane();
    }

    /// Fills the list store from the rows and keeps the selection.
    pub(super) fn draw_list(self: &Rc<Self>) {
        let (rows, selected, picking, list, query) = {
            let state = self.state.borrow();
            let picked = state.select.clone();
            let rows: Vec<Row> = state
                .rows
                .iter()
                .map(|r| Row {
                    checked: picked.as_ref().is_some_and(|p| p.contains(&r.id)),
                    ..r.clone()
                })
                .collect();
            (
                rows,
                state.selected.clone(),
                picked,
                state.list.clone().unwrap_or(List::All),
                state.query.clone(),
            )
        };
        self.quiet.set(true);
        let items: Vec<glib::BoxedAnyObject> =
            rows.iter().cloned().map(glib::BoxedAnyObject::new).collect();
        self.store.splice(0, self.store.n_items(), &items);
        let at = selected
            .as_ref()
            .and_then(|id| rows.iter().position(|r| &r.id == id));
        self.selection
            .set_selected(at.map_or(gtk::INVALID_LIST_POSITION, |i| i as u32));
        self.quiet.set(false);
        self.draw_head(&list, rows.len(), picking.as_ref().map(HashSet::len), &query);
        self.draw_status(&list, rows.is_empty(), &query);
    }

    fn draw_status(self: &Rc<Self>, list: &List, empty: bool, query: &str) {
        while let Some(child) = self.status.first_child() {
            self.status.remove(&child);
        }
        let Some(app) = self.app.upgrade() else { return };
        let account = match list {
            List::Account(id) => self
                .state
                .borrow()
                .data
                .accounts
                .iter()
                .find(|a| a.id == *id)
                .cloned(),
            _ => None,
        };
        let off = account.as_ref().is_some_and(|a| !self.reads(a));
        let (title, text, button) = if off {
            let name = account.as_ref().map(|a| account_label(a.id).short).unwrap_or_default();
            (
                gettext("Contacts are off"),
                fill(
                    &gettext("{account} can keep an address book. That book is on the server, not on this computer. Turn contacts on to copy it here. Turning contacts off later deletes that copy."),
                    &[("account", &name)],
                ),
                account.clone(),
            )
        } else if !empty {
            self.search_row.set_visible(true);
            self.list_stack.set_visible_child_name("rows");
            return;
        } else if !query.is_empty() {
            (
                gettext("No Matching Contacts"),
                gettext("Nothing matches that search."),
                None,
            )
        } else {
            match list {
                List::SameAddress => (
                    gettext("No shared addresses"),
                    gettext("No two contacts in one account share an address. Nothing is waiting to be deleted."),
                    None,
                ),
                List::SamePerson => (
                    gettext("No close names"),
                    gettext("No two accounts hold a close name that is not linked yet."),
                    None,
                ),
                List::FromMail => (
                    gettext("Everyone is a contact"),
                    gettext("Everyone the mailbox has seen is saved as a contact already."),
                    None,
                ),
                List::Vips => (
                    gettext("No VIPs yet"),
                    gettext("Add a person to VIPs from their page, so you can open them from this list."),
                    None,
                ),
                _ => (
                    gettext("No contacts yet"),
                    gettext("Contacts are on for this account. Nothing has been copied onto this computer."),
                    None,
                ),
            }
        };
        self.search_row
            .set_visible(!off && !matches!(list, List::Account(_)) || !off && !empty || !query.is_empty());
        let heading = gtk::Label::builder()
            .label(&title)
            .css_classes(["cts-status-title"])
            .wrap(true)
            .justify(gtk::Justification::Center)
            .build();
        let body = gtk::Label::builder()
            .label(&text)
            .wrap(true)
            .justify(gtk::Justification::Center)
            .max_width_chars(42)
            .css_classes(["cts-status-body"])
            .build();
        self.status.append(&heading);
        self.status.append(&body);
        if let Some(account) = button {
            let turn_on = parts::button(&gettext("Turn On"), &["yes"]);
            let weak = Rc::downgrade(self);
            turn_on.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.turn_on(account.clone());
                }
            });
            turn_on.set_halign(gtk::Align::Center);
            self.status.append(&turn_on);
        }
        let _ = app;
        self.list_stack.set_visible_child_name("status");
    }

    /// Turns contacts on for an account. Undo turns them off again.
    pub(super) fn turn_on(self: &Rc<Self>, account: Account) {
        let Some(app) = self.app.upgrade() else { return };
        self.state.borrow_mut().turned_on.insert(account.id);
        app.change_settings(Change::AccountContacts {
            email: account.email.clone(),
            on: true,
        });
        let name = account_label(account.id).short;
        let (email, account_id) = (account.email.clone(), account.id);
        self.tell(
            &fill(
                &gettext("Contacts are on for {account}. The book on this computer is empty."),
                &[("account", &name)],
            ),
            Some(Box::new(move |this| {
                this.state.borrow_mut().turned_on.remove(&account_id);
                if let Some(app) = this.app.upgrade() {
                    app.change_settings(Change::AccountContacts { email, on: false });
                }
                this.reload();
            })),
        );
        self.reload();
    }

    fn draw_head(
        self: &Rc<Self>,
        list: &List,
        count: usize,
        picking: Option<usize>,
        _query: &str,
    ) {
        while let Some(child) = self.head.first_child() {
            self.head.remove(&child);
        }
        let (title, sub) = self.list_title(list, count);
        let account_off = match list {
            List::Account(id) => self
                .state
                .borrow()
                .data
                .accounts
                .iter()
                .find(|a| a.id == *id)
                .is_some_and(|a| !self.reads(a)),
            _ => false,
        };
        let weak = Rc::downgrade(self);
        // An account with nothing to list shows its address in the head,
        // as the mockup does for an account turned off or newly on.
        let account_empty = matches!(list, List::Account(_)) && count == 0 && _query.is_empty();
        if account_off || account_empty {
            if let List::Account(id) = list {
                let grey = gtk::Box::builder()
                    .valign(gtk::Align::Start)
                    .margin_top(6)
                    .css_classes(["account-dot", "account-8"])
                    .build();
                let _ = id;
                self.head.append(&grey);
                let heading = gtk::Box::builder()
                    .orientation(gtk::Orientation::Vertical)
                    .build();
                heading.append(&parts::label(&title, &["cts-h2"]));
                heading.append(&parts::label(&sub, &["cts-count"]));
                self.head.append(&heading);
                let _ = sub;
                self.head.add_css_class("accounthead");
            }
            return;
        }
        self.head.remove_css_class("accounthead");
        self.search.set_placeholder_text(Some(&match list {
            List::SameAddress => gettext("Search same address"),
            List::SamePerson => gettext("Search same person"),
            List::FromMail => gettext("Search correspondents"),
            _ => gettext("Search contacts"),
        }));
        super::name(
            &self.search,
            &self.search.placeholder_text().unwrap_or_default(),
        );
        if let Some(n) = picking {
            self.head.append(&parts::label(
                &fill(&gettext("{count} selected"), &[("count", &n.to_string())]),
                &["cts-h2"],
            ));
            let grow = gtk::Box::builder().hexpand(true).build();
            self.head.append(&grow);
            let cancel = parts::button(&gettext("Cancel"), &[]);
            let w = weak.clone();
            cancel.connect_clicked(move |_| {
                if let Some(this) = w.upgrade() {
                    this.stop_select();
                }
            });
            let delete = parts::button(
                &fill(&gettext("Delete {count}"), &[("count", &n.to_string())]),
                &["dangerfill"],
            );
            delete.set_sensitive(n > 0);
            let w = weak.clone();
            delete.connect_clicked(move |_| {
                if let Some(this) = w.upgrade() {
                    this.delete_picked();
                }
            });
            self.head.append(&cancel);
            self.head.append(&delete);
            return;
        }
        self.head.append(&parts::label(&title, &["cts-h2"]));
        if !sub.is_empty() {
            let count = parts::label(&sub, &["cts-count"]);
            count.set_ellipsize(gtk::pango::EllipsizeMode::End);
            count.set_wrap(false);
            self.head.append(&count);
        }
        self.head.append(&gtk::Box::builder().hexpand(true).build());
        if self.picks_here() && count > 0 {
            let select = parts::icon_button("ok", &gettext("Select several contacts"), &[]);
            let w = weak.clone();
            select.connect_clicked(move |_| {
                if let Some(this) = w.upgrade() {
                    this.start_select(None);
                }
            });
            self.head.append(&select);
        }
        if matches!(list, List::All | List::Vips | List::Group(_) | List::Account(_)) {
            let new = parts::icon_button("plus", &gettext("New contact"), &[]);
            let w = weak.clone();
            new.connect_clicked(move |_| {
                if let Some(this) = w.upgrade() {
                    this.begin_new();
                }
            });
            self.head.append(&new);
        }
    }

    /// Select is offered on lists of contacts.
    pub(super) fn picks_here(&self) -> bool {
        matches!(
            self.current_list(),
            List::All | List::Vips | List::Group(_) | List::Account(_)
        )
    }

    fn list_title(&self, list: &List, count: usize) -> (String, String) {
        let query = self.state.borrow().query.clone();
        let total = self.state.borrow().data.contacts.len();
        let people = |n: usize| {
            fill_plural(
                "{count} person",
                "{count} people",
                n,
                &[("count", &n.to_string())],
            )
        };
        let sub_for = |n: usize, of: usize| {
            if query.is_empty() {
                people(n)
            } else {
                fill(
                    &gettext("{count} of {total}"),
                    &[("count", &n.to_string()), ("total", &of.to_string())],
                )
            }
        };
        let _ = total;
        let state = self.state.borrow();
        let all = people::group(&state.data.contacts, &state.data.links).len();
        match list {
            List::All => (gettext("All contacts"), sub_for(count, all)),
            List::Vips => (
                gettext("VIPs"),
                fill_plural("{count} person", "{count} people", count, &[("count", &count.to_string())]),
            ),
            List::FromMail => (gettext("From mail"), gettext("Not contacts yet")),
            List::SameAddress => (
                gettext("Same address"),
                if count == 0 {
                    gettext("None")
                } else {
                    fill_plural("{count} pair", "{count} pairs", count, &[("count", &count.to_string())])
                },
            ),
            List::SamePerson => (
                gettext("Same person"),
                if count == 0 {
                    gettext("None")
                } else {
                    fill_plural("{count} pair", "{count} pairs", count, &[("count", &count.to_string())])
                },
            ),
            List::Group(name) => (name.clone(), people(count)),
            List::Account(id) => {
                let name = account_label(*id);
                let off = state
                    .data
                    .accounts
                    .iter()
                    .find(|a| a.id == *id)
                    .is_some_and(|a| !self.reads(a));
                let sub = if off || query.is_empty() {
                    if off || count == 0 { name.full.clone() } else { people(count) }
                } else {
                    sub_for(count, count)
                };
                (name.short, sub)
            }
        }
    }

    /// Draws the Undo lane with `words`, and runs `undo` if Undo is
    /// pressed before the lane goes.
    pub(super) fn tell(self: &Rc<Self>, words: &str, undo: Option<UndoRun>) {
        self.lane_text.set_label(words);
        self.lane_undo.set_visible(undo.is_some());
        super::name(&self.lane_undo, &gettext("Undo"));
        {
            let mut state = self.state.borrow_mut();
            state.undo = undo;
            if let Some(old) = state.lane_timer.take() {
                old.remove();
            }
        }
        self.lane.set_reveal_child(true);
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(std::time::Duration::from_secs(10), move || {
            if let Some(this) = weak.upgrade() {
                let mut state = this.state.borrow_mut();
                state.lane_timer = None;
                state.undo = None;
                drop(state);
                this.lane.set_reveal_child(false);
            }
        });
        self.state.borrow_mut().lane_timer = Some(id);
    }

    fn undo(self: &Rc<Self>) {
        let run = {
            let mut state = self.state.borrow_mut();
            if let Some(timer) = state.lane_timer.take() {
                timer.remove();
            }
            state.undo.take()
        };
        self.lane.set_reveal_child(false);
        if let Some(run) = run {
            run(self);
        }
    }

    pub(super) fn say_error(self: &Rc<Self>, err: &dyn std::fmt::Display) {
        self.tell(&err.to_string(), None);
    }

    // Select

    fn start_select(self: &Rc<Self>, first: Option<String>) {
        let mut picked = HashSet::new();
        picked.extend(first);
        self.state.borrow_mut().select = Some(picked);
        self.draw_list();
    }

    fn stop_select(self: &Rc<Self>) {
        self.state.borrow_mut().select = None;
        self.draw_list();
    }

    fn toggle_pick(self: &Rc<Self>, id: &str) {
        {
            let mut state = self.state.borrow_mut();
            if let Some(pick) = state.select.as_mut()
                && !pick.remove(id)
            {
                pick.insert(id.to_string());
            }
        }
        self.draw_list();
    }

    /// The contacts the picked rows stand for: the one the row shows, in
    /// its account.
    fn delete_picked(self: &Rc<Self>) {
        let Some(app) = self.app.upgrade() else { return };
        let (targets, _total): (Vec<Contact>, usize) = {
            let state = self.state.borrow();
            let picked = state.select.clone().unwrap_or_default();
            let mut out = Vec::new();
            for row in state.rows.iter().filter(|r| picked.contains(&r.id)) {
                if let RowKind::Person(i) = row.kind {
                    let person = &state.people[i];
                    // The row's own record: the one whose name it shows.
                    let c = &state.data.contacts[person.members[0]];
                    if !c.details.read_only {
                        out.push(c.clone());
                    }
                }
            }
            let n = out.len();
            (out, n)
        };
        if targets.is_empty() {
            self.stop_select();
            return;
        }
        let words = deleted_words(&targets);
        let weak = Rc::downgrade(self);
        self.state.borrow_mut().select = None;
        glib::spawn_future_local(async move {
            let mut gone = Vec::new();
            let mut failure = None;
            for contact in targets {
                let photo = ops::photo_of(&app, &contact);
                match ops::delete(&app, contact.clone()).await {
                    Ok(()) => gone.push((contact, photo)),
                    Err(err) => {
                        failure = Some(err.to_string());
                        break;
                    }
                }
            }
            let Some(this) = weak.upgrade() else { return };
            app.contacts_added();
            this.reload();
            match failure {
                Some(err) => this.say_error(&err),
                None => this.tell(
                    &words,
                    Some(Box::new(move |this| this.restore_all(gone))),
                ),
            }
        });
    }

    pub(super) fn restore_all(self: &Rc<Self>, gone: Vec<(Contact, Option<Vec<u8>>)>) {
        let Some(app) = self.app.upgrade() else { return };
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            for (contact, photo) in gone {
                if let Err(err) = ops::restore(&app, contact, photo).await {
                    if let Some(this) = weak.upgrade() {
                        this.say_error(&err);
                    }
                    return;
                }
            }
            app.contacts_added();
            if let Some(this) = weak.upgrade() {
                this.reload();
            }
        });
    }

    /// Starts a new contact in the person pane.
    pub(super) fn begin_new(self: &Rc<Self>) {
        self.state.borrow_mut().pane = Some(Pane::New);
        self.nav.set_show_content(true);
        self.show_pane();
    }
}

/// The correspondents that are not contacts, and not automated senders.
pub(super) fn from_mail(data: &Data) -> Vec<usize> {
    let known: HashSet<String> = data
        .contacts
        .iter()
        .flat_map(|c| c.emails.iter().map(|e| e.to_lowercase()))
        .collect();
    data.correspondents
        .iter()
        .enumerate()
        .filter(|(_, c)| {
            !known.contains(&c.email.to_lowercase()) && !mailrs_store::contacts::is_automated(&c.email)
        })
        .map(|(i, _)| i)
        .collect()
}

/// "Deleted Harbor Lane Lettings from Work and Mara Okafor from Fernwood."
pub(super) fn deleted_words(contacts: &[Contact]) -> String {
    let parts: Vec<String> = contacts
        .iter()
        .map(|c| {
            fill(
                &gettext("{name} from {account}"),
                &[
                    ("name", c.display()),
                    ("account", &account_label(c.account_id).short),
                ],
            )
        })
        .collect();
    fill(
        &gettext("Deleted {list}."),
        &[("list", &crate::protection::joined(
            &parts.iter().map(String::as_str).collect::<Vec<_>>(),
        ))],
    )
}

/// A website address as a link a browser may open, or `None` for anything
/// but a plain web address.
pub(super) fn website_uri(address: &str) -> Option<String> {
    let address = address.trim();
    let url = url::Url::parse(address)
        .or_else(|_| url::Url::parse(&format!("https://{address}")))
        .ok()?;
    (matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none())
    .then(|| url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contact_websites_open_web_addresses_only() {
        assert_eq!(
            website_uri("https://example.org/a?q=1&b=2").as_deref(),
            Some("https://example.org/a?q=1&b=2")
        );
        assert_eq!(
            website_uri("http://example.org").as_deref(),
            Some("http://example.org/")
        );
        assert_eq!(
            website_uri(" www.example.org ").as_deref(),
            Some("https://www.example.org/")
        );
        for value in [
            "",
            "not a website",
            "file:///tmp/file",
            "javascript:alert(1)",
            "mailto:demo@example.org",
            "https://name:password@example.org",
        ] {
            assert_eq!(website_uri(value), None, "{value}");
        }
    }
}
