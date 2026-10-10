//! Mailboxes: the unified views, then one section per account.

mod keys;
mod sections;
pub(crate) mod tree;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib, pango};
use mailrs_domain::translate::{fill, fill_plural, gettext};
use mailrs_domain::{
    Account, AccountId, AccountState, FlagColor, Folder, Label, MailSet, Provider, Role,
};
use mailrs_sync::Offers;

use super::{FolderLook, LABEL_COLORS, Mailbox, Standard, describe, label_color_name};
use crate::format::{PALETTE, account_color_index, palette_name};
use crate::offered::Filing;
use crate::settings::Space;
use keys::ListKey;
use sections::{Place, Section};
use tree::label_rows;

struct Row {
    row: gtk::ListBoxRow,
    mailbox: Mailbox,
    /// The mailbox's name, kept so the spoken name can be built again
    /// whenever the count beside it changes.
    name: String,
    count: gtk::Label,
}

struct Heading {
    row: gtk::ListBoxRow,
    account_id: AccountId,
    /// What the heading calls the account: its name, or its address.
    name: String,
    /// What a screen reader adds after the row's name.
    description: String,
    count: gtk::Label,
    /// The row's own Rules, Hide My Email and Automatic Reply actions,
    /// gated again by [`Sidebar::regate`] when the account starts.
    actions: gio::SimpleActionGroup,
}

/// Undo Send at the foot of the sidebar, while a message waits out its
/// delay. It slides up into place and back down, over 200 ms.
pub struct UndoPill {
    pub revealer: gtk::Revealer,
    pub button: gtk::Button,
    left: gtk::Label,
}

impl UndoPill {
    fn new() -> UndoPill {
        let left = gtk::Label::builder().css_classes(["undo-left"]).build();
        let content = gtk::Box::builder().spacing(8).build();
        content.append(&gtk::Image::from_icon_name("edit-undo-symbolic"));
        content.append(
            &gtk::Label::builder()
                .label(gettext("Undo Send"))
                .xalign(0.0)
                .hexpand(true)
                .build(),
        );
        content.append(&left);
        let button = gtk::Button::builder()
            .child(&content)
            .css_classes(["undo-pill"])
            .tooltip_text(gettext("Stop the message from going out"))
            .build();
        // The seconds change every second and the name stays put, so a
        // screen reader is not told each one.
        super::name(&button, &gettext("Undo Send"));
        let revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideUp)
            .transition_duration(200)
            .child(&button)
            .build();
        UndoPill { revealer, button, left }
    }

    /// Sets `left` ("0:07") at the pill's end. Private: go through
    /// `Sidebar::show_undo`, which opens it and keeps the shared foot in
    /// step.
    fn fill(&self, left: &str) {
        if self.left.label() != left {
            self.left.set_label(left);
        }
    }
}

/// The next event, at the foot of the mail sidebar above Undo Send. A
/// click opens it in the calendar.
pub struct NextEvent {
    pub revealer: gtk::Revealer,
    pub button: gtk::Button,
    bar: gtk::Box,
    when: gtk::Label,
    what: gtk::Label,
    /// The tint rule for the event's colour, which gives the bar its
    /// colour through `--cal-colour`.
    css: gtk::CssProvider,
    /// The colour `css` holds, so an unchanged one is not loaded again.
    colour: RefCell<String>,
}

impl NextEvent {
    fn new() -> NextEvent {
        let bar = gtk::Box::builder().css_classes(["bar"]).build();
        let when = gtk::Label::builder().xalign(0.0).css_classes(["when"]).build();
        let what = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(pango::EllipsizeMode::End)
            .css_classes(["what"])
            .build();
        let lines = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .hexpand(true)
            .build();
        lines.append(&when);
        lines.append(&what);
        let content = gtk::Box::builder().spacing(8).build();
        content.append(&bar);
        content.append(&lines);
        content.append(&gtk::Image::from_icon_name("x-office-calendar-symbolic"));
        let button = gtk::Button::builder()
            .child(&content)
            .css_classes(["next-event"])
            .tooltip_text(gettext("Show in Calendar"))
            .build();
        let revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::Crossfade)
            .transition_duration(200)
            .child(&button)
            .build();
        let css = gtk::CssProvider::new();
        if let Some(display) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        }
        NextEvent {
            revealer,
            button,
            bar,
            when,
            what,
            css,
            colour: RefCell::default(),
        }
    }

    /// Sets the card's two lines and the event's colour. Private: go
    /// through `Sidebar::show_next`, which opens it and keeps the shared
    /// foot in step.
    fn fill(&self, words: &crate::ui::calendar::next::Words, colour: &str) {
        use crate::ui::calendar::{next, tint};
        if self.when.label() != words.when {
            self.when.set_label(&words.when);
        }
        if self.what.label() != words.what {
            self.what.set_label(&words.what);
        }
        // Loading a provider on the display restyles every widget in every
        // window, and this runs once a minute, so it loads only a new colour.
        if *self.colour.borrow() != colour {
            self.bar.set_css_classes(&["bar", tint::css_class(colour).as_str()]);
            self.css.load_from_string(&tint::stylesheet(&[colour.to_string()]));
            colour.clone_into(&mut self.colour.borrow_mut());
        }
        super::name(&self.button, &next::spoken(words));
    }
}

/// What one revealer in the foot is doing: `reveals` is where it is
/// headed, `revealed` whether its child is still on screen.
#[derive(Clone, Copy, Debug)]
struct Slot {
    reveals: bool,
    revealed: bool,
}

impl Slot {
    fn of(revealer: &gtk::Revealer) -> Slot {
        Slot {
            reveals: revealer.reveals_child(),
            revealed: revealer.is_child_revealed(),
        }
    }

    /// A closed revealer has no height but still takes the box's
    /// spacing, so it stays hidden unless it opens, shows or closes.
    fn takes_room(self) -> bool {
        self.reveals || self.revealed
    }
}

/// Which parts of the sidebar's foot are visible. Free of GTK state, so
/// a plain test can check the rule; `Sidebar::sync_foot` applies it.
#[derive(Debug)]
struct Foot {
    next: bool,
    undo: bool,
    /// The bar itself, with its padding: hidden while it holds neither.
    shown: bool,
}

impl Foot {
    /// `mail` is whether the Mail space shows: the next-event card
    /// belongs to it alone and goes at once when the calendar opens.
    fn of(mail: bool, next: Slot, undo: Slot) -> Foot {
        let next = mail && next.takes_room();
        let undo = undo.takes_room();
        Foot {
            next,
            undo,
            shown: next || undo,
        }
    }
}

#[cfg(test)]
mod foot_tests {
    use super::{Foot, Slot};

    const CLOSED: Slot = Slot { reveals: false, revealed: false };
    const OPENING: Slot = Slot { reveals: true, revealed: false };
    const OPEN: Slot = Slot { reveals: true, revealed: true };
    const CLOSING: Slot = Slot { reveals: false, revealed: true };

    #[test]
    fn a_revealer_takes_room_while_it_opens_shows_or_closes() {
        assert!(!CLOSED.takes_room());
        assert!(OPENING.takes_room());
        assert!(OPEN.takes_room());
        assert!(CLOSING.takes_room());
    }

    #[test]
    fn a_closed_revealer_is_hidden_so_the_foot_spacing_goes() {
        let foot = Foot::of(true, OPEN, CLOSED);
        assert_eq!((foot.next, foot.undo, foot.shown), (true, false, true));
        let foot = Foot::of(true, CLOSED, OPENING);
        assert_eq!((foot.next, foot.undo, foot.shown), (false, true, true));
    }

    #[test]
    fn the_foot_stays_until_a_closing_slide_ends() {
        assert!(Foot::of(true, CLOSING, CLOSED).shown);
        assert!(Foot::of(true, CLOSED, CLOSING).shown);
        assert!(!Foot::of(true, CLOSED, CLOSED).shown);
    }

    #[test]
    fn the_calendar_space_hides_the_next_event_at_once() {
        let foot = Foot::of(false, OPEN, CLOSED);
        assert_eq!((foot.next, foot.undo, foot.shown), (false, false, false));
        let foot = Foot::of(false, OPEN, OPEN);
        assert_eq!((foot.next, foot.undo, foot.shown), (false, true, true));
    }
}

pub struct Sidebar {
    pub page: adw::ToolbarView,
    pub header: adw::HeaderBar,
    /// The next event, above Undo Send at the foot of the card. Belongs
    /// to the Mail space only; hidden while the calendar shows.
    pub next: NextEvent,
    /// Undo Send, below the next-event card, at the foot of the card.
    /// Ruling R9 leaves Add Account out of it; the main menu and the
    /// welcome page still open the same picker.
    pub undo: UndoPill,
    /// The bottom bar `next` and `undo` sit in, shared by both spaces.
    /// `sync_foot` hides it whole while it holds neither.
    foot: gtk::Box,
    /// Whether the Mail space shows, which the next-event card needs.
    mail_showing: Cell<bool>,
    /// Switches between the mail and the calendar. Its toggles are named
    /// `mail` and `calendar`; it hides while no account offers a
    /// calendar, and "Mailboxes" shows in its place, since a switch to a
    /// calendar that cannot show anything would only mislead.
    pub switch: adw::ToggleGroup,
    /// The badge on the Mail toggle and on the Calendar one.
    badges: [(Space, gtk::Label); 2],
    /// Unread mail in the unified inbox, for the Mail toggle's badge.
    unread: Cell<i64>,
    /// Invitations waiting for an answer, for the Calendar toggle's.
    waiting: Cell<i64>,
    title: adw::WindowTitle,
    /// The mailbox list, or the calendar's own sidebar.
    content: gtk::Stack,
    list: gtk::ListBox,
    scroller: gtk::ScrolledWindow,
    /// Colours for label icons, rewritten on each rebuild.
    label_css: gtk::CssProvider,
    /// The row the arrow keys last moved the focus to (`mark_keyed`).
    keyed: RefCell<Option<gtk::ListBoxRow>>,
    rows: RefCell<Vec<Row>>,
    headings: RefCell<Vec<Heading>>,
    /// Accounts whose sections the user expanded or collapsed.
    expanded: RefCell<HashMap<AccountId, bool>>,
    /// Whether sections start open. Unset means open only with one account.
    pub start_expanded: Cell<Option<bool>>,
    muted: Cell<bool>,
    /// Handles mail dropped on a mailbox; true when it was taken.
    on_drop: Rc<dyn Fn(Mailbox) -> bool>,
    /// Handles a label dropped on another label's row.
    on_label_drop: Rc<dyn Fn(LabelDrop)>,
    /// The label being dragged, while one is, so the rows it passes over
    /// can tell it from dragged mail.
    dragging: Rc<RefCell<Option<Dragged>>>,
}

/// A label on the move, by account and id.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Dragged {
    account_id: AccountId,
    label_id: String,
}

/// A label dropped on a row of its own account's labels: which one, onto
/// which row, and on which part of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelDrop {
    pub account_id: AccountId,
    pub dragged: String,
    pub target: String,
    pub zone: tree::Zone,
}

impl Sidebar {
    pub fn new(
        on_select: impl Fn(Mailbox) + 'static,
        on_drop: impl Fn(Mailbox) -> bool + 'static,
        on_label_drop: impl Fn(LabelDrop) + 'static,
    ) -> Rc<Sidebar> {
        let list = gtk::ListBox::builder()
            .css_classes(["navigation-sidebar", "mailboxes"])
            .selection_mode(gtk::SelectionMode::Single)
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&list)
            .build();
        let title = adw::WindowTitle::new(&gettext("Mailboxes"), "");
        let switch = adw::ToggleGroup::builder()
            .css_classes(["round", "space-switch"])
            .valign(gtk::Align::Center)
            .visible(false)
            .build();
        let badges = [
            (Space::Mail, "mail", gettext("Mail"), "mail-unread-symbolic"),
            (Space::Calendar, "calendar", gettext("Calendar"), "x-office-calendar-symbolic"),
        ]
        .map(|(space, name, label, icon)| {
            let content = adw::ButtonContent::builder()
                .icon_name(icon)
                .label(&label)
                .build();
            // Each toggle counts what waits in the other space, in a small
            // accent pill after its name, while that space is away.
            let badge = gtk::Label::builder()
                .css_classes(["space-badge"])
                .valign(gtk::Align::Center)
                .visible(false)
                .build();
            let child = gtk::Box::builder().spacing(6).build();
            child.append(&content);
            child.append(&badge);
            let toggle = adw::Toggle::builder()
                .name(name)
                .label(&label)
                .child(&child)
                .build();
            switch.add(toggle);
            (space, badge)
        });
        switch.set_active_name(Some("mail"));
        // 8 px past the header's own padding puts the switch 14 px into
        // the card, as mockups.py's `sidebar_shell` draws it.
        let titles = gtk::Box::builder().margin_start(8).build();
        titles.append(&title);
        titles.append(&switch);
        // At the start rather than as the title: a title widget is centred
        // in the room beside the menu button, 8 px right of where the
        // mockup puts the switch.
        let header = adw::HeaderBar::builder()
            .show_end_title_buttons(false)
            .show_title(false)
            .build();
        header.pack_start(&titles);
        let content = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(150)
            .build();
        content.add_named(&scroller, Some("mail"));
        // The sidebar sits inside the window, 8 px from its top, bottom,
        // start and end edges, so the window's background shows round it.
        let page = adw::ToolbarView::builder()
            .css_classes(["sidebar-card"])
            .margin_top(8)
            .margin_bottom(8)
            .margin_start(8)
            .margin_end(8)
            .build();
        page.add_top_bar(&header);
        page.set_content(Some(&content));
        let next = NextEvent::new();
        let undo = UndoPill::new();
        let foot = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(14)
            .css_classes(["sidebar-foot"])
            .build();
        foot.append(&next.revealer);
        foot.append(&undo.revealer);
        page.add_bottom_bar(&foot);

        let sidebar = Rc::new(Sidebar {
            page,
            header,
            next,
            undo,
            foot,
            mail_showing: Cell::new(true),
            switch,
            badges,
            unread: Cell::new(0),
            waiting: Cell::new(0),
            title,
            content,
            list,
            scroller: scroller.clone(),
            keyed: RefCell::new(None),
            label_css: {
                let css = gtk::CssProvider::new();
                if let Some(display) = gdk::Display::default() {
                    gtk::style_context_add_provider_for_display(
                        &display,
                        &css,
                        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
                    );
                }
                css
            },
            rows: RefCell::new(Vec::new()),
            headings: RefCell::new(Vec::new()),
            expanded: RefCell::new(HashMap::new()),
            start_expanded: Cell::new(None),
            muted: Cell::new(false),
            on_drop: Rc::new(on_drop),
            on_label_drop: Rc::new(on_label_drop),
            dragging: Rc::default(),
        });
        let weak = Rc::downgrade(&sidebar);
        sidebar.list.connect_row_selected(move |_, row| {
            let (Some(sidebar), Some(row)) = (weak.upgrade(), row) else {
                return;
            };
            if sidebar.muted.get() {
                return;
            }
            let chosen = sidebar
                .rows
                .borrow()
                .iter()
                .find(|r| &r.row == row)
                .map(|r| r.mailbox.clone());
            if let Some(mailbox) = chosen {
                on_select(mailbox);
            }
        });
        let weak = Rc::downgrade(&sidebar);
        sidebar.list.connect_row_activated(move |_, row| {
            let Some(sidebar) = weak.upgrade() else {
                return;
            };
            let account = sidebar
                .headings
                .borrow()
                .iter()
                .find(|h| &h.row == row)
                .map(|h| h.account_id);
            if let Some(account_id) = account {
                let open = !sidebar.is_expanded(account_id);
                sidebar.expanded.borrow_mut().insert(account_id, open);
                sidebar.apply_expansion();
            }
        });
        // The list is one Tab stop and the arrows move inside it without
        // opening each mailbox they pass (keys.rs). Capture runs before
        // the list's own key bindings, which would select every row.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(&sidebar);
        keys.connect_key_pressed(move |_, key, _, state| {
            let Some(sidebar) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let focus = focus_at(sidebar.list.upcast_ref());
            let handled = match keys::route(key, state, focus) {
                Some(ListKey::Leave { forward }) => sidebar.leave_list(forward),
                Some(ListKey::Step(step)) => sidebar.step_focus(step),
                Some(ListKey::Open) => sidebar.open_focused(),
                Some(ListKey::Menu) => sidebar.open_options(),
                None => false,
            };
            if handled { glib::Propagation::Stop } else { glib::Propagation::Proceed }
        });
        sidebar.list.add_controller(keys);
        // A press in the list puts the pointer in charge, so the keyboard's
        // ring goes.
        let press = gtk::GestureClick::new();
        press.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(&sidebar);
        press.connect_pressed(move |_, _, _, _| {
            if let Some(sidebar) = weak.upgrade() {
                sidebar.mark_keyed(None);
            }
        });
        sidebar.list.add_controller(press);
        // A slide that closes a revealer ends here, and only then may the
        // revealer and the foot go without cutting it short.
        for revealer in [&sidebar.next.revealer, &sidebar.undo.revealer] {
            let weak = Rc::downgrade(&sidebar);
            revealer.connect_child_revealed_notify(move |_| {
                if let Some(sidebar) = weak.upgrade() {
                    sidebar.sync_foot();
                }
            });
        }
        // Neither the next event nor Undo Send has anything to show yet.
        sidebar.sync_foot();
        sidebar
    }

    /// The list's rows in order.
    fn list_rows(&self) -> Vec<gtk::ListBoxRow> {
        let mut rows = Vec::new();
        let mut child = self.list.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            if let Ok(row) = widget.downcast::<gtk::ListBoxRow>() {
                rows.push(row);
            }
        }
        rows
    }

    /// The row the keyboard focus is on itself, not on a button inside it.
    fn focused_row(&self) -> Option<gtk::ListBoxRow> {
        let focus = self.list.root().and_then(|root| root.focus())?;
        focus.downcast::<gtk::ListBoxRow>().ok().filter(|row| row.parent().as_ref() == Some(self.list.upcast_ref()))
    }

    /// Moves the focus out of the list, to the next control after it or
    /// the one before. The list stops taking the focus while the window
    /// looks for that control, so the search passes over its rows.
    fn leave_list(&self, forward: bool) -> bool {
        let Some(root) = self.list.root() else { return false };
        let direction = if forward { gtk::DirectionType::TabForward } else { gtk::DirectionType::TabBackward };
        self.list.set_can_focus(false);
        if !root.child_focus(direction) {
            // Past the window's last control: start again from its first,
            // as GTK's own Tab does.
            root.set_focus(None::<&gtk::Widget>);
            root.child_focus(direction);
        }
        self.list.set_can_focus(true);
        self.mark_keyed(None);
        true
    }

    /// Moves the focus `step` reachable rows down or up, without
    /// selecting, so nothing loads until Enter or Space.
    fn step_focus(&self, step: i32) -> bool {
        let rows = self.list_rows();
        let reachable: Vec<bool> = rows.iter().map(|row| keys::reachable(row_keys(row))).collect();
        let from = self
            .focused_row()
            .or_else(|| self.list.selected_row())
            .and_then(|row| rows.iter().position(|r| *r == row));
        let Some(from) = from else { return false };
        if let Some(to) = keys::step_to(&reachable, from, step) {
            self.mark_keyed(Some(&rows[to]));
            rows[to].grab_focus();
        }
        true
    }

    /// Puts the `keyed` class on the row the arrows moved the focus to,
    /// and takes it off the one before. A row focused by `grab_focus`
    /// carries GTK's focus-visible state, but GTK 4.22 drew libadwaita's
    /// ring only for a row Tab reached (checked 2026-10-05), so the
    /// stylesheet draws the same ring on `.keyed:focus`. A click clears
    /// it, and the ring needs the focus too, so it shows only where the
    /// keyboard is.
    fn mark_keyed(&self, row: Option<&gtk::ListBoxRow>) {
        if let Some(old) = self.keyed.replace(row.cloned()) {
            old.remove_css_class("keyed");
        }
        if let Some(row) = row {
            row.add_css_class("keyed");
        }
    }

    /// Opens the menu of the focused row's options button, the one an
    /// account heading or a label shows on hover. A row without one keeps
    /// the key for its own right-click menu (`context_menu`).
    fn open_options(&self) -> bool {
        let Some(row) = self.focused_row() else { return false };
        let mut stack: Vec<gtk::Widget> = row.first_child().into_iter().collect();
        while let Some(widget) = stack.pop() {
            if let Some(button) = widget.downcast_ref::<gtk::MenuButton>() {
                button.popup();
                return true;
            }
            stack.extend(widget.next_sibling());
            stack.extend(widget.first_child());
        }
        false
    }

    /// Opens the mailbox under the focus, or opens or closes the account
    /// whose heading has it.
    fn open_focused(&self) -> bool {
        let Some(row) = self.focused_row() else { return false };
        if keys::on_enter(row_keys(&row)) == Some(ListKey::Menu) {
            return self.open_options();
        }
        if row.is_selectable() {
            self.list.select_row(Some(&row));
        } else if row.is_activatable() {
            row.activate();
        }
        true
    }

    /// Shows the switch, or "Mailboxes" in its place.
    pub fn set_switch_visible(&self, visible: bool) {
        self.switch.set_visible(visible);
        self.title.set_visible(!visible);
    }

    /// Puts the calendar's own sidebar, `content`, in place of the
    /// mailbox list. The foot, shared by both spaces, stays below it, but
    /// the next-event card belongs to Mail alone: the calendar shows the
    /// same event on its own page.
    pub fn show_calendar(&self, content: &gtk::Widget) {
        if self.content.child_by_name("calendar").is_none() {
            self.content.add_named(content, Some("calendar"));
        }
        self.content.set_visible_child_name("calendar");
        self.mail_showing.set(false);
        self.sync_foot();
        self.sync_badges();
    }

    /// Puts the mailbox list back.
    pub fn show_mail(&self) {
        self.content.set_visible_child_name("mail");
        self.mail_showing.set(true);
        self.sync_foot();
        self.sync_badges();
    }

    /// Unread mail in the unified inbox changed.
    pub fn set_unread(&self, count: i64) {
        self.unread.set(count);
        self.sync_badges();
    }

    /// The number of invitations waiting for an answer changed.
    pub fn set_waiting(&self, count: i64) {
        self.waiting.set(count);
        self.sync_badges();
    }

    /// Puts each toggle's count on its badge, and in its name, so a
    /// screen reader hears "Mail, 12 unread" where the eye sees the pill.
    fn sync_badges(&self) {
        let on_screen = if self.mail_showing.get() { Space::Mail } else { Space::Calendar };
        let badges = space_badges(on_screen, self.unread.get(), self.waiting.get());
        // An `adw::Toggle` is not a widget, and the group points each of
        // its buttons' `LabelledBy` at the toggle's content, which wins
        // over a label set later. The buttons are the group's radio
        // children, in the toggles' own order.
        let buttons = std::iter::successors(self.switch.first_child(), |child| child.next_sibling())
            .filter(|child| child.accessible_role() == gtk::AccessibleRole::Radio);
        for ((space, badge), button) in self.badges.iter().zip(buttons) {
            let count = match space {
                Space::Mail => badges.mail,
                Space::Calendar => badges.calendar,
            };
            let text = badge_text(count);
            badge.set_label(text.as_deref().unwrap_or_default());
            badge.set_visible(text.is_some());
            button.reset_relation(gtk::AccessibleRelation::LabelledBy);
            super::name(&button, &toggle_name(*space, count));
        }
    }

    /// Shows the next-event card and keeps the shared foot in step.
    pub fn show_next(&self, words: &crate::ui::calendar::next::Words, colour: &str) {
        self.next.fill(words, colour);
        self.open(&self.next.revealer);
    }

    /// Takes the next-event card away. The foot follows once the card
    /// has faded out.
    pub fn hide_next(&self) {
        self.next.revealer.set_reveal_child(false);
        self.sync_foot();
    }

    /// Shows the Undo Send pill and keeps the shared foot in step.
    pub fn show_undo(&self, left: &str) {
        self.undo.fill(left);
        self.open(&self.undo.revealer);
    }

    /// Takes the Undo Send pill away. The foot follows once the pill has
    /// slid down.
    pub fn hide_undo(&self) {
        self.undo.revealer.set_reveal_child(false);
        self.sync_foot();
    }

    /// Makes room for `revealer` before telling it to open: a revealer
    /// that is not on screen jumps to open with no transition.
    fn open(&self, revealer: &gtk::Revealer) {
        if revealer.reveals_child() {
            return;
        }
        let opening = Slot {
            reveals: true,
            revealed: revealer.is_child_revealed(),
        };
        let slot = |r: &gtk::Revealer| if r == revealer { opening } else { Slot::of(r) };
        self.apply(Foot::of(
            self.mail_showing.get(),
            slot(&self.next.revealer),
            slot(&self.undo.revealer),
        ));
        revealer.set_reveal_child(true);
    }

    /// Shows the foot (the border and the padding round it, `.sidebar-foot`)
    /// and each revealer in it only while they have something on screen,
    /// so the box's spacing falls only between two that both show.
    fn sync_foot(&self) {
        self.apply(Foot::of(
            self.mail_showing.get(),
            Slot::of(&self.next.revealer),
            Slot::of(&self.undo.revealer),
        ));
    }

    fn apply(&self, foot: Foot) {
        self.next.revealer.set_visible(foot.next);
        self.undo.revealer.set_visible(foot.undo);
        self.foot.set_visible(foot.shown);
    }

    fn is_expanded(&self, account_id: AccountId) -> bool {
        let default = self
            .start_expanded
            .get()
            .unwrap_or(self.headings.borrow().len() <= 1);
        self.expanded
            .borrow()
            .get(&account_id)
            .copied()
            .unwrap_or(default)
    }

    /// Shows or hides each account's rows. The selected row always stays visible.
    fn apply_expansion(&self) {
        let selected = self.list.selected_row();
        // An open account's mailboxes run straight into the next heading,
        // so that heading gets space above it. Closed accounts stay flush,
        // on the same pitch as the mailboxes.
        let mut after_open = false;
        for heading in self.headings.borrow().iter() {
            let open = self.is_expanded(heading.account_id);
            if after_open {
                heading.row.add_css_class("after-open");
            } else {
                heading.row.remove_css_class("after-open");
            }
            after_open = open;
            heading
                .row
                .update_state(&[gtk::accessible::State::Expanded(Some(open))]);
            heading
                .count
                .set_visible(!open && heading.count.label() != "0");
            for row in self.rows.borrow().iter() {
                if row.mailbox.account() == Some(heading.account_id) {
                    row.row
                        .set_visible(open || selected.as_ref() == Some(&row.row));
                }
            }
        }
    }

    /// Sets each account heading's own Rules, Hide My Email and Automatic
    /// Reply actions from what `offers` says now, without rebuilding
    /// anything else. `read_accounts` calls this every time, even while a
    /// search on screen skips the rest of a rebuild, so an account that
    /// starts mid-search does not leave its menu gated at "everything".
    pub fn regate(&self, offers: impl Fn(AccountId) -> Offers) {
        for heading in self.headings.borrow().iter() {
            for (name, enabled) in crate::offered::account_menu_actions(offers(heading.account_id))
            {
                if let Some(action) = heading
                    .actions
                    .lookup_action(name)
                    .and_downcast::<gio::SimpleAction>()
                {
                    action.set_enabled(enabled);
                }
            }
        }
    }

    /// Rebuilds every row. `selected` is kept selected when it still exists.
    /// Rebuilds every row. `vips` lists VIPs by address and name. `offers`
    /// says what each account offers, which words its menu.
    pub fn rebuild(
        &self,
        accounts: &[(Account, Vec<Label>)],
        extras: &Extras,
        selected: &Mailbox,
        offers: impl Fn(AccountId) -> Offers,
    ) {
        let vips = &extras.vips;
        let mut label_rules = String::new();
        // Keep the scroll position; label changes rebuild every row.
        let scrolled = self.scroller.vadjustment().value();
        self.muted.set(true);
        self.list.remove_all();
        self.rows.borrow_mut().clear();
        self.headings.borrow_mut().clear();
        for (section, places) in sections::LAYOUT {
            self.list.append(&section_title(&section.title()));
            for &place in places {
                self.add_place(place, vips);
            }
        }
        if !extras.smart.is_empty() {
            self.list.append(&section_title(&Section::Smart.title()));
            for smart in &extras.smart {
                let mailbox = Mailbox::Smart(smart.clone());
                let row = self.add_mailbox(mailbox, &smart.name, "folder-saved-search-symbolic", 0);
                smart_menu(&row, &smart.id);
            }
        }
        if !accounts.is_empty() {
            // The rows above list every account at once. This says what
            // the ones below are, rather than leaving a reader to work it
            // out from the addresses.
            self.list.append(&section_title(&Section::Accounts.title()));
        }
        for (account, labels) in accounts {
            let shown = extras.names.get(&account.id);
            let account_offers = offers(account.id);
            let (row, count, actions) = heading(
                account,
                shown,
                account_offers,
                extras.not_downloading.contains(&account.id),
            );
            self.list.append(&row);
            self.headings.borrow_mut().push(Heading {
                row,
                account_id: account.id,
                name: shown.unwrap_or(&account.email).clone(),
                description: heading_description(account),
                count,
                actions,
            });
            let order = extras
                .label_order
                .get(&account.id)
                .cloned()
                .unwrap_or_default();
            let rows = label_rows(labels, &order);
            let mut add_labels = |under| {
                for entry in rows.iter().filter(|row| row.under == under) {
                    let label = entry.label;
                    let mailbox = Mailbox::Label {
                        account_id: account.id,
                        label_id: label.id.clone(),
                        name: label.name.replace('/', " › "),
                    };
                    let dest = mailbox.clone();
                    let row = match entry.opens {
                        true => self.add_mailbox(
                            mailbox,
                            entry.leaf,
                            label_icon(account_offers),
                            entry.depth,
                        ),
                        false => self.add_group(mailbox, entry.leaf, entry.depth),
                    };
                    if let Some(color) = label.color.as_deref().and_then(css_hex)
                        && let Some(icon) = row.child().and_then(|c| c.first_child())
                    {
                        let class = format!("label-color-{color}");
                        label_rules.push_str(&format!(".{class} {{ color: #{color}; }}\n"));
                        icon.add_css_class(&class);
                    }
                    let moves = tree::moves(&rows, &label.id);
                    label_menu(&row, account.id, label, Filing::of([account_offers]), moves);
                    self.label_drag(&row, account.id, &label.id, entry.opens.then_some(dest));
                }
            };
            for which in Standard::ALL {
                let mailbox = Mailbox::Standard {
                    account_id: account.id,
                    which,
                };
                self.add_mailbox(mailbox, &which.name(), which.icon(), 1);
                if let MailSet::Role(role) = which.set() {
                    add_labels(Some(role));
                }
            }
            for folder in Folder::ALL {
                let mailbox = Mailbox::Folder {
                    account_id: Some(account.id),
                    folder,
                };
                self.add_mailbox(mailbox, &folder.name(), folder.icon(), 1);
                let role = match folder {
                    Folder::Archive => Role::Archive,
                    Folder::Junk => Role::Junk,
                    Folder::Trash => Role::Trash,
                    Folder::AllMail => Role::All,
                };
                add_labels(Some(role));
            }
            add_labels(None);
            // Tags follow the folders, flat, with no menu and no drag: they
            // cannot nest or move, and the person makes and renames them in
            // Outlook.
            for tag in tree::tag_rows(labels) {
                let mailbox = Mailbox::Label {
                    account_id: account.id,
                    label_id: tag.id.clone(),
                    name: tag.name.clone(),
                };
                let row = self.add_mailbox(mailbox, &tag.name, "penguin-mail-tag-symbolic", 1);
                if let Some(color) = tag.color.as_deref().and_then(css_hex)
                    && let Some(icon) = row.child().and_then(|c| c.first_child())
                {
                    let class = format!("label-color-{color}");
                    label_rules.push_str(&format!(".{class} {{ color: #{color}; }}\n"));
                    icon.add_css_class(&class);
                }
            }
        }
        self.label_css.load_from_string(&label_rules);
        self.select(selected);
        self.apply_expansion();
        self.muted.set(false);
        // The new rows get their height, and selecting a row scrolls to it,
        // a moment later; put the view back once that has happened.
        let adjustment = self.scroller.vadjustment();
        glib::timeout_add_local_once(std::time::Duration::from_millis(120), move || {
            adjustment.set_value(scrolled);
        });
    }

    /// Adds the row, or the run of rows, that one place in the layout
    /// stands for.
    fn add_place(&self, place: Place, vips: &[(String, String)]) {
        match place {
            Place::Unified(which) => {
                let row =
                    self.add_mailbox(Mailbox::Unified(which), &which.unified_name(), which.icon(), 0);
                if which == Standard::Flagged
                    && let Some(icon) = row.child().and_then(|c| c.first_child())
                {
                    // The Flagged row wears the same orange as each flag
                    // colour under it, so the two read as one idea.
                    icon.add_css_class("flag-orange");
                }
            }
            Place::Vips => {
                if vips.is_empty() {
                    return;
                }
                let everyone = Mailbox::Vips {
                    emails: vips.iter().map(|(e, _)| e.clone()).collect(),
                    name: gettext("VIPs"),
                };
                let row = self.add_mailbox(everyone, &gettext("VIPs"), "starred-symbolic", 0);
                if let Some(icon) = row.child().and_then(|c| c.first_child()) {
                    icon.add_css_class("sidebar-vip");
                }
                for (email, name) in vips {
                    let person = Mailbox::Vips {
                        emails: vec![email.clone()],
                        name: name.clone(),
                    };
                    let row = self.add_mailbox(person, name, "avatar-default-symbolic", 1);
                    let menu = gio::Menu::new();
                    let item = gio::MenuItem::new(Some(&gettext("Remove from VIPs")), None);
                    item.set_action_and_target_value(Some("win.vip-remove"), Some(&email.to_variant()));
                    menu.append_item(&item);
                    context_menu(&row, &menu);
                }
            }
            Place::FlagColors => {
                // One row per flag colour in use, as Apple Mail shows them.
                for color in FlagColor::ALL {
                    let row = self.add_mailbox(
                        Mailbox::Flag(color),
                        &color.name(),
                        "penguin-mail-flag-symbolic",
                        1,
                    );
                    if let Some(icon) = row.child().and_then(|c| c.first_child()) {
                        icon.add_css_class(&format!("flag-{}", color.as_str()));
                    }
                }
            }
            Place::Outbox => {
                // Send Later has the tray with an arrow, which the mockup
                // pictures; the Outbox has a tray holding a letter, and
                // `set_counts` swaps in a warning while a send has failed.
                self.add_mailbox(Mailbox::Outbox, &gettext("Outbox"), sections::outbox_icon(0), 0);
            }
            Place::Scheduled => {
                self.add_mailbox(
                    Mailbox::Scheduled,
                    &gettext("Send Later"),
                    "penguin-mail-outbox-symbolic",
                    0,
                );
            }
            Place::Reminders => {
                self.add_mailbox(Mailbox::Reminders, &gettext("Remind Me"), "alarm-symbolic", 0);
            }
            Place::FollowUp => {
                self.add_mailbox(
                    Mailbox::FollowUp,
                    &gettext("Follow Up"),
                    "appointment-soon-symbolic",
                    0,
                );
            }
            Place::Folder(folder) => {
                let mailbox = Mailbox::Folder {
                    account_id: None,
                    folder,
                };
                self.add_mailbox(mailbox, &folder.name(), folder.icon(), 0);
            }
        }
    }

    /// Adds a mailbox row. `depth` indents it: 0 for the unified views, 1
    /// for an account's mailboxes, and one more per level of label nesting.
    fn add_mailbox(&self, mailbox: Mailbox, name: &str, icon: &str, depth: u32) -> gtk::ListBoxRow {
        self.add_row(mailbox, name, icon, depth, true)
    }

    /// Adds a row for a group, a server folder that holds only other
    /// folders. It indents and closes with its account like a mailbox, so
    /// the folders under it nest, but nothing selects or opens it and it
    /// takes no dropped mail.
    fn add_group(&self, mailbox: Mailbox, name: &str, depth: u32) -> gtk::ListBoxRow {
        let row = self.add_row(mailbox, name, "folder-symbolic", depth, false);
        row.set_tooltip_text(Some(&gettext("Holds folders, not mail")));
        // The keys stop on it and Enter opens its menu (`row_keys`).
        row.add_css_class(GROUP_CLASS);
        row
    }

    /// Lets a label row be dragged within its account's labels, and take
    /// a dragged label before it, after it, or inside it. With `mail`, the
    /// mailbox the row opens, it takes dragged mail too, as every mailbox
    /// row does; a group opens none and takes none.
    fn label_drag(
        &self,
        row: &gtk::ListBoxRow,
        account_id: AccountId,
        label_id: &str,
        mail: Option<Mailbox>,
    ) {
        let here = Dragged {
            account_id,
            label_id: label_id.to_string(),
        };
        let source = gtk::DragSource::new();
        source.set_actions(gdk::DragAction::MOVE);
        source.connect_prepare(|_, _, _| {
            Some(gdk::ContentProvider::for_value(&DRAG_LABEL.to_value()))
        });
        let (dragging, dragged, weak) = (Rc::clone(&self.dragging), here.clone(), row.downgrade());
        source.connect_drag_begin(move |source, _| {
            *dragging.borrow_mut() = Some(dragged.clone());
            if let Some(row) = weak.upgrade() {
                source.set_icon(Some(&gtk::WidgetPaintable::new(Some(&row))), 0, 0);
            }
        });
        let dragging = Rc::clone(&self.dragging);
        source.connect_drag_end(move |_, _, _| {
            dragging.borrow_mut().take();
        });
        row.add_controller(source);

        // Some(true) for another label of this account, Some(false) for a
        // label that cannot land here, None for mail.
        let dragging = Rc::clone(&self.dragging);
        let label_over = move || -> Option<bool> {
            let dragging = dragging.borrow();
            let dragged = dragging.as_ref()?;
            Some(dragged.account_id == here.account_id && dragged.label_id != here.label_id)
        };
        let target = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::MOVE);
        let (over, takes_mail) = (label_over.clone(), mail.is_some());
        target.connect_enter(move |target, _, _| match over().unwrap_or(takes_mail) {
            true => gdk::DragAction::MOVE,
            false => {
                target.reject();
                gdk::DragAction::empty()
            }
        });
        let weak = row.downgrade();
        target.connect_motion(move |_, _, y| {
            if let (Some(true), Some(row)) = (label_over(), weak.upgrade()) {
                show_zone(&row, Some(tree::zone(y, f64::from(row.height()))));
            }
            gdk::DragAction::MOVE
        });
        let weak = row.downgrade();
        target.connect_leave(move |_| {
            if let Some(row) = weak.upgrade() {
                show_zone(&row, None);
            }
        });
        let (dragging, weak) = (Rc::clone(&self.dragging), row.downgrade());
        let (on_label_drop, on_drop) = (Rc::clone(&self.on_label_drop), Rc::clone(&self.on_drop));
        let target_id = label_id.to_string();
        target.connect_drop(move |_, value, _, y| {
            let Some(row) = weak.upgrade() else {
                return false;
            };
            show_zone(&row, None);
            // Cloned out, so the callbacks below may start another drag.
            let dragged = dragging.borrow().clone();
            match (dragged, value.get::<String>().ok().as_deref()) {
                (Some(dragged), Some(DRAG_LABEL)) if dragged.account_id == account_id => {
                    on_label_drop(LabelDrop {
                        account_id,
                        dragged: dragged.label_id,
                        target: target_id.clone(),
                        zone: tree::zone(y, f64::from(row.height())),
                    });
                    true
                }
                (None, Some(DRAG_MAIL)) => mail.clone().is_some_and(|mailbox| on_drop(mailbox)),
                _ => false,
            }
        });
        row.add_controller(target);
    }

    /// Adds a row. `opens` is false for a row that only groups others.
    fn add_row(
        &self,
        mailbox: Mailbox,
        name: &str,
        icon: &str,
        depth: u32,
        opens: bool,
    ) -> gtk::ListBoxRow {
        // 12 px puts the icon inside the row's own left edge, which
        // `.mailboxes > row`'s 8 px margin already sets 8 px in from the
        // card; 10 px of spacing then lands the name 38 px into the row,
        // as the mockup draws both.
        let content = gtk::Box::builder()
            .spacing(10)
            .margin_start(12 + 18 * depth as i32)
            .css_classes(["mailbox-row"])
            .build();
        content.append(&gtk::Image::from_icon_name(icon));
        content.append(
            &gtk::Label::builder()
                .label(name)
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(pango::EllipsizeMode::End)
                .build(),
        );
        let count = gtk::Label::builder()
            .css_classes(["count"])
            .visible(false)
            .build();
        content.append(&count);
        let row = gtk::ListBoxRow::builder()
            .child(&content)
            .visible(!sections::hidden_until_used(&mailbox))
            .selectable(opens)
            .activatable(opens)
            .build();
        // A label row takes mail through `label_drag`, which also takes
        // dragged labels. Every other row turns a dragged label away.
        let label = matches!(mailbox, Mailbox::Label { .. });
        if opens && takes_mail(&mailbox) && !label {
            let target = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::MOVE);
            let dragging = Rc::clone(&self.dragging);
            target.connect_enter(move |target, _, _| match dragging.borrow().is_some() {
                true => {
                    target.reject();
                    gdk::DragAction::empty()
                }
                false => gdk::DragAction::MOVE,
            });
            let (on_drop, dest) = (Rc::clone(&self.on_drop), mailbox.clone());
            target.connect_drop(move |_, value, _, _| {
                value.get::<String>().is_ok_and(|v| v == DRAG_MAIL) && on_drop(dest.clone())
            });
            row.add_controller(target);
        }
        super::name(&row, name);
        self.list.append(&row);
        self.rows.borrow_mut().push(Row {
            row: row.clone(),
            mailbox,
            name: name.to_string(),
            count,
        });
        row
    }

    /// Selects `mailbox` without reporting it as a user choice.
    pub fn select(&self, mailbox: &Mailbox) {
        let was_muted = self.muted.replace(true);
        let rows = self.rows.borrow();
        let row = rows.iter().find(|r| &r.mailbox == mailbox).or(rows.first());
        self.list.select_row(row.map(|r| &r.row));
        self.muted.set(was_muted);
    }

    /// Deselects everything, as when showing search results.
    pub fn clear_selection(&self) {
        let was_muted = self.muted.replace(true);
        self.list.unselect_all();
        self.muted.set(was_muted);
    }

    pub fn mailboxes(&self) -> Vec<Mailbox> {
        self.rows
            .borrow()
            .iter()
            .map(|r| r.mailbox.clone())
            .collect()
    }

    /// Unread counts for mailboxes, totals for drafts and queued mail.
    pub fn set_counts(&self, counts: &HashMap<Mailbox, i64>) {
        let selected = self.list.selected_row();
        for row in self.rows.borrow().iter() {
            let count = counts.get(&row.mailbox).copied().unwrap_or(0);
            if sections::hidden_until_used(&row.mailbox) {
                // Each flag colour shows only while in use.
                row.row
                    .set_visible(count > 0 || selected.as_ref() == Some(&row.row));
            }
            let is_drafts = row.mailbox.standard() == Some(Standard::Drafts)
                || matches!(
                    &row.mailbox,
                    Mailbox::Scheduled
                        | Mailbox::Outbox
                        | Mailbox::Reminders
                        | Mailbox::FollowUp
                        | Mailbox::Flag(_)
                );
            let shown = count > 0 && (row.mailbox.counts_unread() || is_drafts);
            if row.mailbox == Mailbox::Outbox
                && let Some(icon) = row.row.child().and_then(|c| c.first_child()).and_downcast::<gtk::Image>()
            {
                icon.set_icon_name(Some(sections::outbox_icon(count)));
            }
            row.count.set_visible(shown);
            row.count.set_label(&count.to_string());
            super::name(
                &row.row,
                &mailbox_row_name(
                    &row.name,
                    if shown { count } else { 0 },
                    row.mailbox.counts_unread(),
                ),
            );
            if row.mailbox.counts_unread() {
                row.count.add_css_class("unread");
            } else {
                row.count.remove_css_class("unread");
            }
        }
        for heading in self.headings.borrow().iter() {
            let inbox = Mailbox::Standard {
                account_id: heading.account_id,
                which: Standard::Inbox,
            };
            let unread = counts.get(&inbox).copied().unwrap_or(0);
            heading.count.set_label(&unread.to_string());
            describe(
                &heading.row,
                &heading_row_name(&heading.name, unread),
                &heading.description,
            );
        }
        self.apply_expansion();
    }
}

/// What a dragged set of list rows carries. The rows themselves stay with
/// the thread list.
pub const DRAG_MAIL: &str = "mailrs-mail";

/// What a dragged label row carries. Which label it is waits in
/// `Sidebar::dragging`, since only the sidebar drags labels.
const DRAG_LABEL: &str = "mailrs-label";

/// Marks `row` with where a dragged label would land: a line above or
/// below it, or a tint for inside it. None clears the mark.
fn show_zone(row: &gtk::ListBoxRow, zone: Option<tree::Zone>) {
    for (class, shown) in [
        ("drop-before", tree::Zone::Before),
        ("drop-inside", tree::Zone::Inside),
        ("drop-after", tree::Zone::After),
    ] {
        match zone == Some(shown) {
            true => row.add_css_class(class),
            false => row.remove_css_class(class),
        }
    }
}

/// What a mailbox row says out loud. The badge at its end is a bare
/// number on screen, so the name takes it in and says what it counts. A
/// row whose badge is hidden says only its name.
fn mailbox_row_name(mailbox: &str, count: i64, unread: bool) -> String {
    if count <= 0 {
        return mailbox.to_string();
    }
    let number = count.to_string();
    let values = [("mailbox", mailbox), ("count", number.as_str())];
    match unread {
        true => fill_plural(
            "{mailbox}, {count} unread message",
            "{mailbox}, {count} unread messages",
            count as usize,
            &values,
        ),
        false => fill_plural(
            "{mailbox}, {count} message",
            "{mailbox}, {count} messages",
            count as usize,
            &values,
        ),
    }
}

/// The counts the Mail and Calendar toggles carry. Each counts only
/// while the other space shows, since the space on screen already shows
/// its own numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpaceBadges {
    /// Unread mail in the unified inbox.
    pub mail: i64,
    /// Invitations waiting for the person's answer.
    pub calendar: i64,
}

pub fn space_badges(on_screen: Space, unread: i64, waiting: i64) -> SpaceBadges {
    SpaceBadges {
        mail: if on_screen == Space::Calendar { unread } else { 0 },
        calendar: if on_screen == Space::Mail { waiting } else { 0 },
    }
}

/// A toggle's badge: its count, "99+" past 99, and none at zero.
pub fn badge_text(count: i64) -> Option<String> {
    match count {
        ..=0 => None,
        1..=99 => Some(count.to_string()),
        _ => Some(gettext("99+")),
    }
}

/// What a toggle says out loud, with the count its badge shows.
pub fn toggle_name(space: Space, count: i64) -> String {
    let number = count.to_string();
    let values = [("count", number.as_str())];
    match (space, count > 0) {
        (Space::Mail, false) => gettext("Mail"),
        (Space::Calendar, false) => gettext("Calendar"),
        (Space::Mail, true) => fill_plural("Mail, {count} unread", "Mail, {count} unread", count as usize, &values),
        (Space::Calendar, true) => fill_plural(
            "Calendar, {count} waiting for your answer",
            "Calendar, {count} waiting for your answer",
            count as usize,
            &values,
        ),
    }
}

/// What an account heading says out loud: the account, and the unread
/// mail behind it while the section is closed.
fn heading_row_name(account: &str, unread: i64) -> String {
    if unread <= 0 {
        return account.to_string();
    }
    let number = unread.to_string();
    fill_plural(
        "{account}, {count} unread message",
        "{account}, {count} unread messages",
        unread as usize,
        &[("account", account), ("count", number.as_str())],
    )
}

/// What the sidebar shows besides accounts and their labels.
#[derive(Debug, Clone, Default)]
pub struct Extras {
    /// VIPs as address and name.
    pub vips: Vec<(String, String)>,
    /// Smart mailboxes in the user's order.
    pub smart: Vec<mailrs_domain::SmartMailbox>,
    /// Names shown instead of account addresses.
    pub names: HashMap<AccountId, String>,
    /// Where the person put each account's labels among their siblings,
    /// by label id.
    pub label_order: HashMap<AccountId, HashMap<String, i64>>,
    /// Accounts with a message their POP3 server refused three times.
    pub not_downloading: HashSet<AccountId>,
}

/// A small heading between sections. It cannot be selected.
fn section_title(text: &str) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::builder()
        .child(
            &gtk::Label::builder()
                .label(text)
                .xalign(0.0)
                .css_classes(["sidebar-section", "dim-label", "caption-heading"])
                .build(),
        )
        .selectable(false)
        .activatable(false)
        .build();
    // The list puts the label inside a row of its own, and the row is
    // what a screen reader reaches, so the words have to be on it too or
    // the section announces as nothing.
    row.set_accessible_role(gtk::AccessibleRole::RowHeader);
    crate::ui::name(&row, text);
    row
}

/// Edit, move, and delete on a right click or long press of a smart mailbox.
fn smart_menu(row: &gtk::ListBoxRow, id: &str) {
    let menu = gio::Menu::new();
    let target = id.to_variant();
    let item = |text: &str, action: &str| {
        let item = gio::MenuItem::new(Some(text), None);
        item.set_action_and_target_value(Some(action), Some(&target));
        item
    };
    menu.append_item(&item(&gettext("Edit…"), "win.smart-edit"));
    let order = gio::Menu::new();
    order.append_item(&item(&gettext("Move Up"), "win.smart-up"));
    order.append_item(&item(&gettext("Move Down"), "win.smart-down"));
    menu.append_section(None, &order);
    let danger = gio::Menu::new();
    danger.append_item(&item(&gettext("Delete…"), "win.smart-delete"));
    menu.append_section(None, &danger);
    context_menu(row, &menu);
}

/// `#rrggbb` as six lower-case hex digits, safe inside a CSS class name.
fn css_hex(color: &str) -> Option<String> {
    let hex = color.trim().trim_start_matches('#').to_lowercase();
    (hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit())).then_some(hex)
}

/// A 10 px dot in a tag's colour, the one its icon wears in the sidebar,
/// for a list of tags such as the Tags popover. A tag with no colour
/// gets a dim dot in the text colour, so every name starts at the same
/// place.
pub(super) fn tag_dot(color: Option<&str>) -> gtk::DrawingArea {
    let fill = color
        .and_then(css_hex)
        .and_then(|hex| gdk::RGBA::parse(format!("#{hex}")).ok());
    let dot = gtk::DrawingArea::builder()
        .content_width(10)
        .content_height(10)
        .valign(gtk::Align::Center)
        .accessible_role(gtk::AccessibleRole::Presentation)
        .build();
    dot.set_draw_func(move |area, cr, width, height| {
        let paint = fill.unwrap_or_else(|| {
            let mut dim = area.color();
            dim.set_alpha(dim.alpha() * 0.35);
            dim
        });
        let (width, height) = (f64::from(width), f64::from(height));
        cr.set_source_rgba(
            f64::from(paint.red()),
            f64::from(paint.green()),
            f64::from(paint.blue()),
            f64::from(paint.alpha()),
        );
        cr.arc(width / 2.0, height / 2.0, width.min(height) / 2.0, 0.0, std::f64::consts::TAU);
        // A failed fill leaves the dot out, which costs nothing but its
        // colour.
        let _ = cr.fill();
    });
    dot
}

/// The icon for a folder row a person can open: the tag Gmail's labels
/// wear, since mail there can carry several at once, or the plain folder
/// icon the sidebar gives a group once the account keeps mail in one
/// place at a time.
fn label_icon(offers: Offers) -> &'static str {
    if offers.labels {
        "penguin-mail-tag-symbolic"
    } else {
        "folder-symbolic"
    }
}

/// Mailboxes mail can be moved into.
fn takes_mail(mailbox: &Mailbox) -> bool {
    match mailbox {
        Mailbox::Unified(which) | Mailbox::Standard { which, .. } => {
            matches!(which, Standard::Inbox | Standard::Flagged | Standard::Muted)
        }
        Mailbox::Label { .. } => true,
        Mailbox::Folder { .. } => true,
        Mailbox::Flag(_) => true,
        Mailbox::Search { .. }
        | Mailbox::Scheduled
        | Mailbox::Outbox
        | Mailbox::Reminders
        | Mailbox::FollowUp
        | Mailbox::Vips { .. }
        | Mailbox::Set { .. }
        | Mailbox::Smart(_) => false,
    }
}

/// A label row's menu, on a right click or long press and on the button
/// at the row's end, which shows while the pointer or the keyboard is on
/// the row.
fn label_menu(row: &gtk::ListBoxRow, account_id: AccountId, label: &Label, filing: Filing, moves: tree::Moves) {
    let menu = gio::Menu::new();
    let target = (account_id, label.id.clone()).to_variant();
    let item = |text: &str, action: &str| {
        let item = gio::MenuItem::new(Some(text), None);
        item.set_action_and_target_value(Some(action), Some(&target));
        item
    };
    let edit = gio::Menu::new();
    edit.append_item(&item(&gettext("Rename…"), "win.label-rename"));
    edit.append_item(&item(&filing.new_inside_item(), "win.label-new-inside"));
    let colors = gio::Menu::new();
    for index in 0..LABEL_COLORS.len() {
        let entry = gio::MenuItem::new(Some(&label_color_name(index)), None);
        entry.set_action_and_target_value(
            Some("win.label-color"),
            Some(&(account_id, label.id.clone(), index as i32).to_variant()),
        );
        colors.append_item(&entry);
    }
    edit.append_submenu(Some(&gettext("Color")), &colors);
    menu.append_section(None, &edit);
    // Move Up and Move Down show only where they would move the label:
    // not up for the first among its siblings, not down for the last.
    let order = gio::Menu::new();
    if moves.up {
        order.append_item(&item(&gettext("Move Up"), "win.label-up"));
    }
    if moves.down {
        order.append_item(&item(&gettext("Move Down"), "win.label-down"));
    }
    if order.n_items() > 0 {
        menu.append_section(None, &order);
    }
    let danger = gio::Menu::new();
    danger.append_item(&item(&gettext("Delete…"), "win.label-delete"));
    menu.append_section(None, &danger);
    context_menu(row, &menu);

    let Some(content) = row.child().and_downcast::<gtk::Box>() else {
        return;
    };
    let options = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .menu_model(&menu)
        .css_classes(["flat", "circular", "label-options"])
        .valign(gtk::Align::Center)
        .halign(gtk::Align::End)
        .tooltip_text(filing.options_tooltip())
        .build();
    super::name(&options, &filing.options_name(&label.name.replace('/', " › ")));
    super::name_menu_items_of(&options);
    // `.options-open` keeps the count hidden while the menu is open and
    // the pointer has left the row.
    let weak = row.downgrade();
    options.connect_active_notify(move |options| {
        if let Some(row) = weak.upgrade() {
            match options.is_active() {
                true => row.add_css_class("options-open"),
                false => row.remove_css_class("options-open"),
            }
        }
    });
    // The sidebar has no room for the count and the button side by side,
    // so the button lies over the count and takes its place on hover or
    // focus, as in Gmail.
    let end = gtk::Overlay::builder().css_classes(["label-end"]).build();
    if let Some(count) = content.last_child() {
        content.remove(&count);
        count.set_halign(gtk::Align::End);
        end.set_child(Some(&count));
    }
    end.add_overlay(&options);
    end.set_measure_overlay(&options, true);
    content.append(&end);
}

/// Opens `menu` at the pointer on a right click or long press of `row`.
fn context_menu(row: &gtk::ListBoxRow, menu: &gio::Menu) {
    let popover = gtk::PopoverMenu::from_model(Some(menu));
    super::name_menu_items(&popover);
    popover.set_has_arrow(false);
    popover.set_halign(gtk::Align::Start);
    popover.set_parent(row);
    let show = {
        let popover = popover.clone();
        move |x: f64, y: f64| {
            popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
            popover.popup();
        }
    };
    let click = gtk::GestureClick::builder()
        .button(gdk::BUTTON_SECONDARY)
        .build();
    let open = show.clone();
    click.connect_pressed(move |_, _, x, y| open(x, y));
    row.add_controller(click);
    // Touch only: a mouse has the right click, and a held mouse button is
    // how a label drag starts.
    let press = gtk::GestureLongPress::builder().touch_only(true).build();
    let held = show.clone();
    press.connect_pressed(move |_, x, y| held(x, y));
    row.add_controller(press);
    // The keyboard's way in: the Menu key or Shift+F10 on the focused
    // row, which the list leaves to the row (`Sidebar::open_options`).
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(move |controller, key, _, state| {
        let focus = controller.widget().map_or(keys::Focus::InList, |row| focus_at(&row));
        if keys::route(key, state, focus) != Some(ListKey::Menu) {
            return glib::Propagation::Proceed;
        }
        let Some(row) = controller.widget() else { return glib::Propagation::Proceed };
        show(f64::from(row.width()) / 2.0, f64::from(row.height()) / 2.0);
        glib::Propagation::Stop
    });
    row.add_controller(keys);
    row.connect_destroy(move |_| popover.unparent());
}

/// The class on a group row, a server folder that holds only folders.
const GROUP_CLASS: &str = "mailbox-group";

/// What the keys need to know of `row`.
fn row_keys(row: &gtk::ListBoxRow) -> keys::Row {
    keys::Row {
        shown: row.is_visible(),
        sensitive: row.is_sensitive(),
        opens: row.is_selectable() || row.is_activatable(),
        group: row.has_css_class(GROUP_CLASS),
    }
}

/// Where the keyboard focus sits relative to `within`, a widget that
/// holds menus: inside one of its popovers, or anywhere else. The walk
/// goes from the focused widget up its parents and stops at `within`.
fn focus_at(within: &gtk::Widget) -> keys::Focus {
    let mut at = within.root().and_then(|root| root.focus());
    while let Some(widget) = at {
        if &widget == within {
            break;
        }
        if widget.is::<gtk::Popover>() {
            return keys::Focus::InPopover;
        }
        at = widget.parent();
    }
    keys::Focus::InList
}

/// The settings section of an account's menu, as words and actions. Every
/// setting is listed; the ones a server may lack are the account's own
/// actions under the `account` prefix, which `heading` turns off from
/// what the account `offers`, and their items hide while they are off.
fn account_settings(offers: Offers) -> Vec<(String, &'static str)> {
    vec![
        (gettext("Automatic Reply…"), "account.vacation"),
        (gettext("Signature…"), "win.account-signature"),
        (gettext("Rules…"), "account.rules"),
        (gettext("Hide My Email…"), "account.hide-my-email"),
        (Filing::of([offers]).new_item(), "win.account-new-label"),
    ]
}

/// The account row's tooltip: the address, and for an account whose mail
/// lives only here, a second line that says so.
fn heading_tooltip(account: &Account) -> String {
    match account.provider {
        Provider::Pop3 => format!("{}\n{}", account.email, gettext("On this computer")),
        _ => account.email.clone(),
    }
}

/// What a screen reader adds after the account row's name.
fn heading_description(account: &Account) -> String {
    match account.provider {
        Provider::Pop3 => gettext("On this computer. Show or hide this account's mailboxes"),
        _ => gettext("Show or hide this account's mailboxes"),
    }
}

fn heading(
    account: &Account,
    name: Option<&String>,
    offers: Offers,
    not_downloading: bool,
) -> (gtk::ListBoxRow, gtk::Label, gio::SimpleActionGroup) {
    let content = gtk::Box::builder()
        .spacing(6)
        .css_classes(["sidebar-heading"])
        .build();
    // Ruling R3: the mockup's account row has no chevron, only a colour
    // dot and the address. `apply_expansion` tells assistive technology
    // whether the account is open.
    let dot = gtk::Box::builder()
        .valign(gtk::Align::Center)
        .css_classes([
            "account-dot",
            &format!("account-{}", account_color_index(account.id)),
        ])
        .build();
    content.append(&dot);
    content.append(
        &gtk::Label::builder()
            .label(name.map_or(account.email.as_str(), String::as_str))
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(pango::EllipsizeMode::Middle)
            .css_classes(["email"])
            .tooltip_text(heading_tooltip(account))
            .build(),
    );
    let status = status_of(account);
    let count = gtk::Label::builder()
        .css_classes(["count", "unread"])
        .visible(false)
        .build();
    content.append(&count);
    if let Some((icon, tip)) = status {
        let image = gtk::Image::builder()
            .icon_name(icon)
            .tooltip_text(&tip)
            .build();
        super::name(&image, &tip);
        if matches!(
            account.state,
            AccountState::NeedsReauth | AccountState::Stopped
        ) {
            image.add_css_class("warning");
        } else {
            image.add_css_class("dim-label");
        }
        content.append(&image);
    }
    let menu = gio::Menu::new();
    let item = |label: &str, action: &str| {
        let item = gio::MenuItem::new(Some(label), None);
        if action.starts_with("account.") {
            // The row's own action knows its account, so it takes no
            // target, and its item hides while the account lacks it.
            item.set_action_and_target_value(Some(action), None);
            item.set_attribute_value("hidden-when", Some(&"action-disabled".to_variant()));
        } else {
            item.set_action_and_target_value(Some(action), Some(&account.id.to_variant()));
        }
        item
    };
    menu.append_item(&item(&gettext("Check for Mail"), "win.account-check"));
    let settings = gio::Menu::new();
    for (label, action) in account_settings(offers) {
        settings.append_item(&item(&label, action));
    }
    menu.append_section(None, &settings);
    let look = gio::Menu::new();
    look.append_item(&item(&gettext("Rename…"), "win.account-rename"));
    let colors = gio::Menu::new();
    for index in 0..PALETTE.len() {
        let entry = gio::MenuItem::new(Some(&palette_name(index)), None);
        entry.set_action_and_target_value(
            Some("win.account-color"),
            Some(&(account.id, index as i32).to_variant()),
        );
        colors.append_item(&entry);
    }
    look.append_submenu(Some(&gettext("Color")), &colors);
    look.append_item(&item(&gettext("Move Up"), "win.account-up"));
    look.append_item(&item(&gettext("Move Down"), "win.account-down"));
    menu.append_section(None, &look);
    let access = gio::Menu::new();
    access.append_item(&item(&gettext("Sign In Again…"), "win.account-reconnect"));
    access.append_item(&item(
        &gettext("Messages That Will Not Download…"),
        "account.not-downloading",
    ));
    menu.append_section(None, &access);
    let danger = gio::Menu::new();
    danger.append_item(&item(&gettext("Remove Account…"), "win.account-remove"));
    menu.append_section(None, &danger);
    let options = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .menu_model(&menu)
        .css_classes(["flat", "circular", "account-options"])
        .valign(gtk::Align::Center)
        .tooltip_text(gettext("Account options"))
        .build();
    super::name(
        &options,
        &fill(
            &gettext("Options for {account}"),
            &[("account", name.unwrap_or(&account.email))],
        ),
    );
    super::name_menu_items_of(&options);
    content.append(&options);
    let row = gtk::ListBoxRow::builder()
        .child(&content)
        .css_classes(["account-heading"])
        .selectable(false)
        .activatable(true)
        .build();
    // One `win.` action serves every account's menu, so it cannot be off
    // for one account. The row holds this account's own Rules, Hide My
    // Email and Automatic Reply, each off when the account lacks it;
    // `Sidebar::regate` sets them again once the account's offers change,
    // whether or not the row itself gets rebuilt.
    let own = gio::SimpleActionGroup::new();
    let add_own = |name: &'static str, enabled: bool| {
        let action = gio::SimpleAction::new(name, None);
        action.set_enabled(enabled);
        let (weak, account_id) = (row.downgrade(), account.id);
        action.connect_activate(move |_, _| {
            let Some(row) = weak.upgrade() else {
                return;
            };
            let target = format!("win.account-{name}");
            if let Err(err) = row.activate_action(&target, Some(&account_id.to_variant())) {
                tracing::warn!(error = %err, action = %target, "could not open an account setting");
            }
        });
        own.add_action(&action);
    };
    for (name, enabled) in crate::offered::account_menu_actions(offers) {
        add_own(name, enabled);
    }
    // On while the store holds a message the POP3 server refused three
    // times; the window rebuilds the row when one reaches three.
    add_own("not-downloading", not_downloading);
    row.insert_action_group("account", Some(&own));
    describe(
        &row,
        &heading_row_name(name.unwrap_or(&account.email), 0),
        &heading_description(account),
    );
    (row, count, own)
}

/// The icon and the words beside an account's name for its state, or
/// nothing while it syncs as it should.
fn status_of(account: &Account) -> Option<(&'static str, String)> {
    match account.state {
        AccountState::NeedsReauth => Some((
            "dialog-warning-symbolic",
            gettext("Sign in again to keep syncing"),
        )),
        AccountState::Offline => Some(("network-offline-symbolic", gettext("Offline"))),
        AccountState::BackingOff => Some((
            "network-offline-symbolic",
            fill(
                &gettext("{provider} is not responding; retrying"),
                &[("provider", &mailrs_discover::resolved_provider_name(account.provider_name()))],
            ),
        )),
        // A keyring read that never answers, often an unlock prompt
        // nobody can see, is no fault of the provider's.
        AccountState::WaitingForKeyring => Some((
            "dialog-password-symbolic",
            gettext("The keyring is not responding; retrying"),
        )),
        AccountState::Bootstrapping => {
            Some(("mail-send-receive-symbolic", gettext("Downloading mail")))
        }
        AccountState::Stopped => Some((
            "dialog-warning-symbolic",
            gettext("Syncing stopped after an error; restart Penguin Mail to try again"),
        )),
        AccountState::Ok => None,
    }
}

#[cfg(test)]
mod tests {
    use mailrs_domain::{Account, AccountState, Provider};

    use super::status_of;
    use super::{
        Mailbox, Standard, heading_row_name, label_icon, mailbox_row_name, takes_mail,
    };

    use super::{Offers, account_settings};

    #[test]
    fn a_pop3_account_row_says_its_mail_is_on_this_computer() {
        let pop3 = Account {
            id: 3,
            email: "dana@example.org".into(),
            state: AccountState::Ok,
            provider: Provider::Pop3,
            provider_name: Some("example.org".into()),
        };
        assert_eq!(super::heading_tooltip(&pop3), "dana@example.org\nOn this computer");
        assert!(super::heading_description(&pop3).starts_with("On this computer."));
        let imap = Account {
            provider: Provider::Imap,
            ..pop3
        };
        assert_eq!(super::heading_tooltip(&imap), "dana@example.org");
        assert_eq!(
            super::heading_description(&imap),
            "Show or hide this account's mailboxes"
        );
    }

    #[test]
    fn a_label_account_opens_a_folder_row_under_a_tag() {
        assert_eq!(
            label_icon(Offers { labels: true, ..Offers::EVERYTHING }),
            "penguin-mail-tag-symbolic"
        );
    }

    #[test]
    fn a_folder_account_opens_a_folder_row_under_a_folder() {
        assert_eq!(
            label_icon(Offers { labels: false, ..Offers::EVERYTHING }),
            "folder-symbolic"
        );
    }

    #[test]
    fn an_account_that_backs_off_names_who_is_not_answering() {
        let gmail = Account {
            id: 1,
            email: "me@gmail.com".into(),
            state: AccountState::BackingOff,
            provider: Provider::Gmail,
            provider_name: None,
        };
        let fastmail = Account {
            provider: Provider::Imap,
            provider_name: Some("Fastmail".into()),
            ..gmail.clone()
        };
        let by_domain = Account {
            provider_name: Some("fastmail.com".into()),
            ..fastmail.clone()
        };
        let said = |account: &Account| status_of(account).map(|(_, said)| said);
        assert_eq!(said(&gmail).as_deref(), Some("Gmail is not responding; retrying"));
        assert_eq!(said(&fastmail).as_deref(), Some("Fastmail is not responding; retrying"));
        assert_eq!(
            said(&by_domain).as_deref(),
            Some("Fastmail is not responding; retrying"),
            "an account saved under its domain still shows its real provider"
        );
    }

    #[test]
    fn an_account_waiting_for_the_keyring_names_the_keyring() {
        let waiting = Account {
            id: 1,
            email: "dana@reyes-home.example".into(),
            state: AccountState::WaitingForKeyring,
            provider: Provider::Pop3,
            provider_name: Some("reyes-home.example".into()),
        };
        assert_eq!(
            status_of(&waiting),
            Some(("dialog-password-symbolic", "The keyring is not responding; retrying".to_string()))
        );
    }

    #[test]
    fn an_account_that_syncs_shows_no_state() {
        let fine = Account {
            id: 1,
            email: "me@gmail.com".into(),
            state: AccountState::Ok,
            provider: Provider::Gmail,
            provider_name: None,
        };
        assert_eq!(status_of(&fine), None);
    }

    fn actions(offers: Offers) -> Vec<&'static str> {
        account_settings(offers).into_iter().map(|(_, action)| action).collect()
    }

    #[test]
    fn a_gmail_account_menu_keeps_every_setting() {
        assert_eq!(
            actions(Offers::EVERYTHING),
            [
                "account.vacation",
                "win.account-signature",
                "account.rules",
                "account.hide-my-email",
                "win.account-new-label",
            ]
        );
    }

    #[test]
    fn an_account_menu_lists_every_setting_and_its_own_actions_hide_what_it_lacks() {
        let bare = Offers {
            rules: false,
            auto_reply: false,
            auto_reply_subject: false,
            auto_reply_contacts_only: false,
            ..Offers::EVERYTHING
        };
        // The items stay in the model; the account's own actions are off,
        // and an item bound to an action that is off hides.
        assert_eq!(actions(bare), actions(Offers::EVERYTHING));
    }

    #[test]
    fn a_mailbox_row_reads_its_badge_as_part_of_the_row() {
        assert_eq!(
            mailbox_row_name("Inbox", 12, true),
            "Inbox, 12 unread messages"
        );
        assert_eq!(
            mailbox_row_name("Inbox", 1, true),
            "Inbox, 1 unread message"
        );
        assert_eq!(mailbox_row_name("Drafts", 3, false), "Drafts, 3 messages");
        assert_eq!(mailbox_row_name("Drafts", 0, false), "Drafts");
    }

    #[test]
    fn an_account_heading_reads_the_mail_behind_a_closed_section() {
        assert_eq!(heading_row_name("ann@example.com", 0), "ann@example.com");
        assert_eq!(heading_row_name("Work", 1), "Work, 1 unread message");
        assert_eq!(heading_row_name("Work", 4), "Work, 4 unread messages");
    }

    #[test]
    fn inbox_flagged_and_muted_take_dropped_mail() {
        for which in [Standard::Inbox, Standard::Flagged, Standard::Muted] {
            assert!(takes_mail(&Mailbox::Unified(which)), "{which:?}");
            assert!(takes_mail(&Mailbox::Standard { account_id: 1, which }), "{which:?}");
        }
        for which in [Standard::Sent, Standard::Drafts] {
            assert!(!takes_mail(&Mailbox::Unified(which)), "{which:?}");
            assert!(!takes_mail(&Mailbox::Standard { account_id: 1, which }), "{which:?}");
        }
    }
}

#[cfg(test)]
mod badge_tests {
    use super::{badge_text, space_badges, toggle_name};
    use crate::settings::Space;

    #[test]
    fn no_badge_shows_at_zero() {
        assert_eq!(badge_text(0), None);
    }

    #[test]
    fn a_count_shows_as_its_number() {
        assert_eq!(badge_text(12).as_deref(), Some("12"));
    }

    #[test]
    fn a_count_past_ninety_nine_shows_as_99_plus() {
        assert_eq!(badge_text(99).as_deref(), Some("99"));
        assert_eq!(badge_text(100).as_deref(), Some("99+"));
    }

    #[test]
    fn mail_shows_its_unread_only_while_the_calendar_shows() {
        let badges = space_badges(Space::Calendar, 12, 3);
        assert_eq!(badges.mail, 12);
        assert_eq!(badges.calendar, 0);
    }

    #[test]
    fn the_calendar_shows_what_waits_only_while_mail_shows() {
        let badges = space_badges(Space::Mail, 12, 3);
        assert_eq!(badges.mail, 0);
        assert_eq!(badges.calendar, 3);
    }

    #[test]
    fn a_toggle_says_its_count_out_loud() {
        assert_eq!(toggle_name(Space::Mail, 12), "Mail, 12 unread");
        assert_eq!(toggle_name(Space::Calendar, 1), "Calendar, 1 waiting for your answer");
        assert_eq!(toggle_name(Space::Mail, 0), "Mail");
    }
}
