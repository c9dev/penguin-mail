//! `Quick`, the small popover a drag across empty time or N opens: the
//! time, a title field with the cursor in it, the entry's type where the
//! calendar takes more than events, the calendar it goes on, which a
//! menu button changes, and More Details for the editor. Enter saves.
//!
//! One popover serves the whole view, parented to the calendar's card
//! the way the event popover is (`popover.rs`'s own doc comment): a
//! reload or a carousel step can destroy the block or the day cell
//! `show` last pointed at, but never the card. `show` repositions and
//! refills the one popover rather than building a new one each time.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, graphene};
use mailrs_domain::AccountId;
use mailrs_domain::calendar::Calendar;
use mailrs_domain::translate::{fill, gettext};

use super::draft::{self, TypeChoice};
use super::tint;
use crate::ui::name;

/// Runs with the title, the index of the calendar picked among the
/// choices `show` was given, and the type picked.
type OnTitle = dyn Fn(String, usize, TypeChoice);

/// Runs when the type changes, and answers the words the time line
/// shows for it: an hour for an event, two for focus time, whole days for
/// out of office. The view marks the new span on its grid meanwhile.
type When = dyn Fn(TypeChoice) -> String;

/// The name the type switch gives each toggle, and back.
fn toggle_name(choice: TypeChoice) -> &'static str {
    match choice {
        TypeChoice::Event => "event",
        TypeChoice::Focus => "focus",
        TypeChoice::OutOfOffice => "away",
    }
}

fn toggle_choice(name: &str) -> TypeChoice {
    match name {
        "focus" => TypeChoice::Focus,
        "away" => TypeChoice::OutOfOffice,
        _ => TypeChoice::Event,
    }
}

/// The type switch's words for `choice`, a button's, so in title case.
pub fn type_label(choice: TypeChoice) -> String {
    match choice {
        TypeChoice::Event => gettext("Event"),
        TypeChoice::Focus => gettext("Focus Time"),
        TypeChoice::OutOfOffice => gettext("Out of Office"),
    }
}

/// The title the field shows after the type changes from `from` to `to`:
/// the new type's own name when the field is empty or still holds the
/// old type's, else `None`, keeping what the person typed.
pub fn retitled(text: &str, from: TypeChoice, to: TypeChoice) -> Option<String> {
    (text.trim().is_empty() || text == draft::type_title(from)).then(|| draft::type_title(to))
}

/// The lines the calendar button and its list show for each choice: its
/// name, and under it the account's address once the choices span more
/// than one account, where two may share a name such as "Personal".
pub fn choice_lines(choices: &[(AccountId, String, Calendar)]) -> Vec<(String, Option<String>)> {
    let several = choices.iter().any(|(a, _, _)| Some(a) != choices.first().map(|(a, _, _)| a));
    choices
        .iter()
        .map(|(_, address, calendar)| (calendar.name.clone(), several.then(|| address.clone())))
        .collect()
}

/// A choice as a screen reader says it: the name, then the account
/// where the list shows one.
pub fn spoken_choice(name: &str, account: Option<&str>) -> String {
    match account {
        Some(account) => fill(&gettext("{name}, {account}"), &[("name", name), ("account", account)]),
        None => name.to_string(),
    }
}

/// A calendar as the button and the list draw it: the calendar's dot,
/// its name, and the account under it when there is one to tell apart.
struct ChoiceView {
    widget: gtk::Box,
    dot: gtk::Box,
    name: gtk::Label,
    account: gtk::Label,
}

impl ChoiceView {
    fn new() -> ChoiceView {
        let dot = gtk::Box::builder()
            .css_classes(["checked-dot"])
            .valign(gtk::Align::Center)
            .build();
        let name = gtk::Label::builder().xalign(0.0).css_classes(["caption"]).build();
        let account = gtk::Label::builder()
            .xalign(0.0)
            .css_classes(["caption", "quick-account"])
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        let lines = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .valign(gtk::Align::Center)
            .build();
        lines.append(&name);
        lines.append(&account);
        let widget = gtk::Box::builder().spacing(8).build();
        widget.append(&dot);
        widget.append(&lines);
        ChoiceView { widget, dot, name, account }
    }

    fn show(&self, colour: &str, name: &str, account: Option<&str>) {
        self.dot.set_css_classes(&["checked-dot", &tint::css_class(colour)]);
        self.name.set_label(name);
        self.account.set_label(account.unwrap_or_default());
        self.account.set_visible(account.is_some());
    }
}

pub struct Quick {
    pub popover: gtk::Popover,
    /// The stable widget `show`'s bounds are computed against, and the
    /// popover is parented to.
    parent: gtk::Widget,
    time: gtk::Label,
    title: gtk::Entry,
    /// The picked calendar as the button shows it.
    current: ChoiceView,
    /// Opens the list of calendars; `current` is its child.
    calendar_button: gtk::MenuButton,
    /// The calendars on offer, one row each, with the picked one ticked.
    list: gtk::ListBox,
    /// The tick on each row of `list`, in order.
    ticks: RefCell<Vec<gtk::Image>>,
    /// The calendars the list offers, from the last `show`.
    choices: RefCell<Vec<(AccountId, String, Calendar)>>,
    picked: Cell<usize>,
    /// Event, Focus Time and Out of Office, as many as the picked
    /// calendar takes. Hidden on a calendar that takes events alone.
    kinds: adw::ToggleGroup,
    /// The types each calendar among the choices takes, where it takes
    /// more than events (`draft::type_switch`).
    types: RefCell<Vec<Option<Vec<TypeChoice>>>>,
    kind: Cell<TypeChoice>,
    /// Set while the code, not the person, changes the type switch.
    quiet: Cell<bool>,
    when: RefCell<Option<Box<When>>>,
    save: gtk::Button,
    more: gtk::Button,
    on_save: RefCell<Option<Box<OnTitle>>>,
    on_more: RefCell<Option<Box<OnTitle>>>,
}

impl Quick {
    /// Builds the one popover this view will ever open, parented to
    /// `parent`, so it survives a reload; `show` repositions it at
    /// whichever slot a press or N marked.
    pub fn new(parent: &impl IsA<gtk::Widget>) -> Rc<Quick> {
        let time = gtk::Label::builder()
            .xalign(0.0)
            .css_classes(["dim-label", "caption"])
            .build();
        let title = gtk::Entry::builder()
            .placeholder_text(gettext("Add title"))
            .activates_default(false)
            .width_chars(26)
            .build();
        name(&title, &gettext("Title"));
        let kinds = adw::ToggleGroup::builder()
            .css_classes(["quick-kinds"])
            .halign(gtk::Align::Start)
            .visible(false)
            .build();
        name(&kinds, &gettext("Type"));
        // A list rather than a menu: a menu's items cannot carry the
        // calendar's dot or a second line for its account, and three
        // calendars can be called Personal.
        let current = ChoiceView::new();
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["navigation-sidebar", "quick-calendars"])
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .propagate_natural_width(true)
            .max_content_height(360)
            .child(&list)
            .build();
        let list_popover = gtk::Popover::builder().child(&scroller).build();
        let calendar_button = gtk::MenuButton::builder()
            .child(&current.widget)
            .popover(&list_popover)
            .css_classes(["flat", "quick-calendar"])
            .halign(gtk::Align::Start)
            .always_show_arrow(true)
            .build();
        let more = gtk::Button::builder()
            .label(gettext("More Details"))
            .css_classes(["flat"])
            .build();
        let save = gtk::Button::builder()
            .label(gettext("Save"))
            .css_classes(["suggested-action"])
            .sensitive(false)
            .build();
        let buttons = gtk::Box::builder()
            .spacing(6)
            .halign(gtk::Align::End)
            .build();
        buttons.append(&more);
        buttons.append(&save);
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .margin_top(10)
            .margin_bottom(10)
            .margin_start(10)
            .margin_end(10)
            .build();
        for child in [
            time.upcast_ref::<gtk::Widget>(),
            title.upcast_ref(),
            kinds.upcast_ref(),
            calendar_button.upcast_ref(),
            buttons.upcast_ref(),
        ] {
            content.append(child);
        }
        let popover = gtk::Popover::builder()
            .child(&content)
            .has_arrow(true)
            .position(gtk::PositionType::Right)
            .build();
        popover.set_parent(parent);

        let this = Rc::new(Quick {
            popover,
            parent: parent.clone().upcast(),
            time,
            title,
            current,
            calendar_button,
            list,
            ticks: RefCell::new(Vec::new()),
            choices: RefCell::new(Vec::new()),
            picked: Cell::new(0),
            kinds,
            types: RefCell::new(Vec::new()),
            kind: Cell::new(TypeChoice::Event),
            quiet: Cell::new(false),
            when: RefCell::new(None),
            save,
            more,
            on_save: RefCell::new(None),
            on_more: RefCell::new(None),
        });

        let weak = Rc::downgrade(&this);
        this.list.connect_row_activated(move |_, row| {
            let Some(this) = weak.upgrade() else { return };
            let Ok(index) = usize::try_from(row.index()) else { return };
            this.show_calendar(index);
            this.show_kinds();
            if let Some(list) = this.calendar_button.popover() {
                list.popdown();
            }
            this.title.grab_focus();
        });
        // The list opens on the picked calendar, so arrows start from it.
        let weak = Rc::downgrade(&this);
        list_popover.connect_show(move |_| {
            let Some(this) = weak.upgrade() else { return };
            let picked = i32::try_from(this.picked.get()).unwrap_or(0);
            if let Some(row) = this.list.row_at_index(picked) {
                row.grab_focus();
            }
        });
        let weak = Rc::downgrade(&this);
        this.kinds.connect_active_name_notify(move |group| {
            let Some(this) = weak.upgrade() else { return };
            if this.quiet.get() {
                return;
            }
            let choice = group.active_name().map_or(TypeChoice::Event, |n| toggle_choice(&n));
            this.pick_kind(choice);
        });
        let (t, s) = (this.title.clone(), this.save.clone());
        t.connect_changed(move |t| s.set_sensitive(!t.text().trim().is_empty()));
        let weak = Rc::downgrade(&this);
        this.title.connect_activate(move |t| {
            let Some(this) = weak.upgrade() else { return };
            let text = t.text().trim().to_string();
            if !text.is_empty() {
                this.popover.popdown();
                if let Some(f) = this.on_save.borrow().as_ref() {
                    f(text, this.picked.get(), this.kind.get());
                }
            }
        });
        let weak = Rc::downgrade(&this);
        this.save.connect_clicked(move |_| {
            let Some(this) = weak.upgrade() else { return };
            let text = this.title.text().trim().to_string();
            this.popover.popdown();
            if let Some(f) = this.on_save.borrow().as_ref() {
                f(text, this.picked.get(), this.kind.get());
            }
        });
        let weak = Rc::downgrade(&this);
        this.more.connect_clicked(move |_| {
            let Some(this) = weak.upgrade() else { return };
            let text = this.title.text().to_string();
            this.popover.popdown();
            if let Some(f) = this.on_more.borrow().as_ref() {
                f(text, this.picked.get(), this.kind.get());
            }
        });
        this
    }

    /// Shows the popover for `when` to `end`, on `side` of `rect` in
    /// `anchor`'s own coordinates: `anchor` is the time grid or the
    /// month grid, translated into the card's coordinates, which is
    /// where the popover itself is parented. The calendar menu offers
    /// `choices`, starting on the one at `current`. `on_save` runs with
    /// the title and the calendar picked on Enter or Save; `on_more` on
    /// More Details. `types` holds, for each choice, the types its type
    /// switch offers, or `None` for a calendar that takes events alone;
    /// `when` words the time for each type.
    #[expect(clippy::too_many_arguments, reason = "each is a separate part of what the popover shows")]
    pub fn show(
        self: &Rc<Self>,
        anchor: &gtk::Widget,
        rect: &gdk::Rectangle,
        side: gtk::PositionType,
        when: impl Fn(TypeChoice) -> String + 'static,
        choices: Vec<(AccountId, String, Calendar)>,
        types: Vec<Option<Vec<TypeChoice>>>,
        current: usize,
        on_save: impl Fn(String, usize, TypeChoice) + 'static,
        on_more: impl Fn(String, usize, TypeChoice) + 'static,
    ) {
        self.time.set_label(&when(TypeChoice::Event));
        self.when.replace(Some(Box::new(when)));
        self.types.replace(types);
        self.kind.set(TypeChoice::Event);
        self.list.remove_all();
        let mut ticks = Vec::new();
        for ((name, account), (_, _, calendar)) in choice_lines(&choices).iter().zip(&choices) {
            let view = ChoiceView::new();
            view.show(&calendar.color, name, account.as_deref());
            let tick = gtk::Image::builder()
                .icon_name("object-select-symbolic")
                .hexpand(true)
                .halign(gtk::Align::End)
                .accessible_role(gtk::AccessibleRole::Presentation)
                .build();
            view.widget.append(&tick);
            let row = gtk::ListBoxRow::builder().child(&view.widget).build();
            crate::ui::name(&row, &spoken_choice(name, account.as_deref()));
            self.list.append(&row);
            ticks.push(tick);
        }
        self.ticks.replace(ticks);
        // One calendar leaves nothing to pick, so the row reads as the
        // plain line it was.
        let several = choices.len() > 1;
        self.calendar_button.set_sensitive(several);
        self.calendar_button.set_always_show_arrow(several);
        self.choices.replace(choices);
        self.show_calendar(current);
        self.title.set_text("");
        self.save.set_sensitive(false);
        self.show_kinds();
        self.on_save.replace(Some(Box::new(on_save)));
        self.on_more.replace(Some(Box::new(on_more)));

        if let Some(point) = anchor.compute_point(
            &self.parent,
            &graphene::Point::new(rect.x() as f32, rect.y() as f32),
        ) {
            let translated = gdk::Rectangle::new(
                point.x().round() as i32,
                point.y().round() as i32,
                rect.width(),
                rect.height(),
            );
            self.popover.set_pointing_to(Some(&translated));
        }
        self.popover.set_position(side);
        self.popover.popup();
        self.title.grab_focus();
    }

    /// Shows the calendar at `index` among the choices as the one picked.
    fn show_calendar(&self, index: usize) {
        let choices = self.choices.borrow();
        let Some((_, _, calendar)) = choices.get(index) else { return };
        self.picked.set(index);
        let lines = choice_lines(&choices);
        let account = lines.get(index).and_then(|(_, account)| account.as_deref());
        self.current.show(&calendar.color, &calendar.name, account);
        for (at, tick) in self.ticks.borrow().iter().enumerate() {
            tick.set_opacity(if at == index { 1.0 } else { 0.0 });
        }
        name(
            &self.calendar_button,
            &fill(&gettext("Calendar: {name}"), &[("name", &spoken_choice(&calendar.name, account))]),
        );
    }

    /// Fills the type switch with what the picked calendar takes, keeping
    /// the type picked where it still can, and hides it on a calendar
    /// that takes events alone.
    fn show_kinds(&self) {
        let types = self.types.borrow().get(self.picked.get()).cloned().flatten();
        let list = types.clone().unwrap_or_default();
        let kept = draft::kept_type(self.kind.get(), &list);
        self.quiet.set(true);
        self.kinds.remove_all();
        for choice in &list {
            let toggle = adw::Toggle::builder().name(toggle_name(*choice)).label(type_label(*choice)).build();
            self.kinds.add(toggle);
        }
        self.kinds.set_active_name(Some(toggle_name(kept)));
        self.kinds.set_visible(types.is_some());
        self.quiet.set(false);
        self.pick_kind(kept);
    }

    /// Makes the entry `choice`: its time line, and its name in the title
    /// field unless the person typed one.
    fn pick_kind(&self, choice: TypeChoice) {
        let from = self.kind.replace(choice);
        if let Some(when) = self.when.borrow().as_ref() {
            self.time.set_label(&when(choice));
        }
        if let Some(title) = retitled(&self.title.text(), from, choice) {
            self.title.set_text(&title);
            self.title.set_position(-1);
        }
    }

    /// Closes the popover, such as when the view's range changes under
    /// it.
    pub fn hide(&self) {
        self.popover.popdown();
    }
}

/// The scroll that shows a slot from `top` to `bottom` in a view `page`
/// tall scrolled to `value`, over content `upper` tall: `value` itself
/// when the slot already shows, else the slot in the middle of the view,
/// within the content's ends.
pub fn reveal(top: f64, bottom: f64, value: f64, page: f64, upper: f64) -> f64 {
    if top >= value && bottom <= value + page {
        return value;
    }
    let middle = (top + bottom) / 2.0 - page / 2.0;
    middle.min(upper - page).max(0.0)
}

/// The side of its slot quick create opens on: the right, unless the
/// slot's middle `x` sits in the right half of a grid `width` wide,
/// where the popover would run out of the window.
pub fn side(x: f64, width: f64) -> gtk::PositionType {
    if x > width / 2.0 {
        gtk::PositionType::Left
    } else {
        gtk::PositionType::Right
    }
}

/// The part of a span from `y`, `height` tall, that falls inside a view
/// `page` tall, at least one pixel, so the popover's arrow points at
/// what shows.
pub fn clamp_span(y: f64, height: f64, page: f64) -> (f64, f64) {
    let top = y.clamp(0.0, (page - 1.0).max(0.0));
    let bottom = (y + height).min(page).max(top + 1.0);
    (top, bottom - top)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slot_already_on_screen_leaves_the_scroll_alone() {
        assert_eq!(reveal(600.0, 650.0, 400.0, 500.0, 1800.0), 400.0);
    }

    #[test]
    fn a_slot_below_the_view_scrolls_it_to_the_middle() {
        // An evening slot at 21:15 with the grid showing 08:00 to 18:00.
        assert_eq!(reveal(1275.0, 1335.0, 480.0, 600.0, 1440.0), 840.0);
    }

    #[test]
    fn a_reveal_never_scrolls_past_either_end() {
        assert_eq!(reveal(1400.0, 1440.0, 0.0, 600.0, 1440.0), 840.0);
        assert_eq!(reveal(0.0, 60.0, 700.0, 600.0, 1440.0), 0.0);
    }

    #[test]
    fn the_popover_opens_toward_the_wider_side_of_the_grid() {
        assert_eq!(side(200.0, 1000.0), gtk::PositionType::Right);
        assert_eq!(side(800.0, 1000.0), gtk::PositionType::Left);
    }

    #[test]
    fn the_rect_a_popover_points_at_stays_inside_the_view() {
        assert_eq!(clamp_span(-30.0, 60.0, 500.0), (0.0, 30.0));
        assert_eq!(clamp_span(480.0, 60.0, 500.0), (480.0, 20.0));
        assert_eq!(clamp_span(100.0, 60.0, 500.0), (100.0, 60.0));
    }

    fn entry(account: AccountId, address: &str, id: &str, name: &str) -> (AccountId, String, Calendar) {
        (account, address.into(), Calendar { id: id.into(), name: name.into(), ..Calendar::default() })
    }

    #[test]
    fn an_empty_title_takes_the_new_types_name() {
        assert_eq!(retitled("", TypeChoice::Event, TypeChoice::Focus).as_deref(), Some("Focus time"));
        assert_eq!(
            retitled("Focus time", TypeChoice::Focus, TypeChoice::OutOfOffice).as_deref(),
            Some("Out of office")
        );
        assert_eq!(retitled("Out of office", TypeChoice::OutOfOffice, TypeChoice::Event).as_deref(), Some(""));
    }

    #[test]
    fn a_typed_title_stays_when_the_type_changes() {
        assert_eq!(retitled("Deep work", TypeChoice::Focus, TypeChoice::OutOfOffice), None);
    }

    #[test]
    fn each_type_has_its_own_toggle() {
        for choice in draft::TYPE_ORDER {
            assert_eq!(toggle_choice(toggle_name(choice)), choice);
        }
    }

    #[test]
    fn one_accounts_calendars_go_by_their_names() {
        let choices = [entry(1, "me@example.com", "me@example.com", "Personal"), entry(1, "me@example.com", "team", "Team")];
        assert_eq!(choice_lines(&choices), [("Personal".to_string(), None), ("Team".to_string(), None)]);
    }

    #[test]
    fn calendars_of_several_accounts_carry_their_address_under_the_name() {
        let choices = [entry(1, "me@example.com", "me@example.com", "Personal"), entry(2, "me@work.pt", "me@work.pt", "Personal")];
        assert_eq!(
            choice_lines(&choices),
            [
                ("Personal".to_string(), Some("me@example.com".to_string())),
                ("Personal".to_string(), Some("me@work.pt".to_string()))
            ]
        );
    }

    #[test]
    fn a_calendar_with_an_account_says_both_out_loud() {
        assert_eq!(spoken_choice("Personal", Some("me@work.pt")), "Personal, me@work.pt");
        assert_eq!(spoken_choice("Personal", None), "Personal");
    }
}
