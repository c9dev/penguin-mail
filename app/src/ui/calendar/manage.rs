//! What the calendar list's menus do: hiding and colouring a calendar,
//! renaming and deleting one the account owns, and adding one (a new
//! calendar, a subscription, a holiday calendar). The work itself is
//! `mailrs_sync::calendar_copy`'s; this module asks the person, runs it,
//! and reloads.
//!
//! A change that needs a permission the account withheld opens the
//! Grant Access question instead of its dialog, so the item says why it
//! cannot go ahead rather than going missing from the menu.

use std::future::Future;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use mailrs_domain::AccountId;
use mailrs_domain::calendar::Calendar;
use mailrs_domain::calendar::list;
use mailrs_domain::translate::{fill, fill_plural, gettext, with_reason};
use mailrs_sync::{Permitted, SyncError, Withheld};

use super::CalendarView;
use super::holidays::{self, Region};
use super::sidebar::{AddKind, ListChange};
use super::tint;
use crate::permission::Permission;
use crate::ui::confirm::{Tone, confirm};

/// The colour a new calendar starts in: Blue, the fifth label colour.
const FIRST_COLOR: usize = 5;

/// The line under "Delete “Work”?", which says what goes with it.
pub fn delete_body(events: usize) -> String {
    match events {
        0 => gettext("The calendar is deleted from Google Calendar on every device. This cannot be undone."),
        n => fill_plural(
            "The calendar and its {count} event are deleted from Google Calendar on every device. \
             This cannot be undone.",
            "The calendar and its {count} events are deleted from Google Calendar on every device. \
             This cannot be undone.",
            n,
            &[("count", &n.to_string())],
        ),
    }
}

/// The account a change to the calendar list would write to, or `None`
/// for a change that stays on this computer (showing a calendar, folding
/// an account).
fn list_edit_of(change: &ListChange) -> Option<AccountId> {
    match change {
        ListChange::Color { account, .. }
        | ListChange::Rename { account, .. }
        | ListChange::Delete { account, .. }
        | ListChange::Unsubscribe { account, .. }
        | ListChange::Add { account, .. } => Some(*account),
        ListChange::Listed { .. } | ListChange::Shown { .. } | ListChange::Folded { .. } => None,
    }
}

/// The account a change subscribes or unsubscribes, which only a provider
/// with `Offers::subscriptions` takes.
fn subscription_of(change: &ListChange) -> Option<AccountId> {
    match change {
        ListChange::Unsubscribe { account, .. }
        | ListChange::Add { account, kind: AddKind::Subscribe | AddKind::Holidays } => Some(*account),
        _ => None,
    }
}

/// Shows `dialog` over `parent` with the keyboard focus in `field`, and
/// answers the response the person chose. An alert dialog puts its own
/// focus on its first button once it shows, so the field takes it back
/// right after (the libadwaita-dialog-traps skill).
async fn ask_in(dialog: &adw::AlertDialog, parent: &impl IsA<gtk::Widget>, field: &impl IsA<gtk::Widget>) -> String {
    let (sender, answer) = async_channel::bounded(1);
    dialog.connect_response(None, move |_, response| {
        let _ = sender.try_send(response.to_string());
    });
    dialog.present(Some(parent));
    if field.is_sensitive() {
        dialog.set_focus(Some(field));
    }
    answer.recv().await.unwrap_or_default()
}

impl CalendarView {
    /// Does what the person chose in a calendar row's menu or in Add
    /// Calendar. Showing a calendar and folding an account stay with the
    /// view itself.
    pub(super) fn manage(self: &Rc<Self>, change: ListChange) {
        // The menus leave these out for an account that cannot change its
        // list; a stale menu or a script that reaches here is turned away
        // before any adapter is asked.
        if let Some(account) = list_edit_of(&change)
            && !self.changeable(account)
        {
            return;
        }
        if let Some(account) = subscription_of(&change)
            && !self.subscribes(account)
        {
            return;
        }
        let copy = self.core.calendar_copy();
        let next_event = super::sidebar::redraws_next_event(&change);
        match change {
            ListChange::Listed { account, calendar, listed } => {
                self.run_list_edit(account, next_event, gettext("Could not change the calendar list: {reason}"), async move {
                    copy.list_calendar(account, &calendar, listed).await
                })
            }
            ListChange::Color { account, calendar, color } => {
                self.run_list_edit(account, next_event, gettext("Could not change the color: {reason}"), async move {
                    copy.recolor_calendar(account, &calendar, color).await
                })
            }
            ListChange::Rename { account, calendar } => self.ask_rename(account, &calendar),
            ListChange::Delete { account, calendar } => self.ask_delete(account, &calendar),
            ListChange::Unsubscribe { account, calendar } => self.ask_unsubscribe(account, &calendar),
            ListChange::Add { account, kind } => match kind {
                AddKind::New => self.ask_new(account),
                AddKind::Subscribe => self.ask_subscribe(account),
                AddKind::Holidays => self.show_holidays(account),
            },
            ListChange::Shown { .. } | ListChange::Folded { .. } => {}
        }
    }

    /// Runs one change to the calendar list, then reads the list again
    /// and sends the queue, so the change reaches Google now when the
    /// network allows. A refusal for want of a permission asks for it.
    fn run_list_edit<T: Send + 'static>(
        self: &Rc<Self>,
        account: AccountId,
        next_event: bool,
        failed: String,
        edit: impl Future<Output = Result<Permitted<T>, SyncError>> + Send + 'static,
    ) {
        let weak = Rc::downgrade(self);
        let core = Rc::clone(&self.core);
        glib::spawn_future_local(async move {
            let done = core.call(edit).await;
            let Some(view) = weak.upgrade() else { return };
            match done {
                Ok(Permitted::Done(_)) => {
                    view.reload();
                    if next_event {
                        (view.hooks.next_event)();
                    }
                    (view.hooks.push)(account);
                }
                Ok(Permitted::NeedsPermission) => view.needs(Permission::ManageCalendars, account),
                Err(err) => view.say(&with_reason(&failed, &err, &[])),
            }
        });
    }

    /// Whether the account's provider lets its calendar list change.
    /// An account not yet known reads as able, as the sidebar assumes.
    fn changeable(&self, account: AccountId) -> bool {
        self.accounts
            .borrow()
            .iter()
            .find(|(a, _, _)| a.id == account)
            .is_none_or(|(_, offers, _)| offers.calendar_list)
    }

    /// Whether the account subscribes to calendars and adds holiday ones
    /// (`Offers::subscriptions`); an account not yet started does.
    fn subscribes(&self, account: AccountId) -> bool {
        self.accounts
            .borrow()
            .iter()
            .find(|(a, _, _)| a.id == account)
            .is_none_or(|(_, offers, _)| offers.subscriptions)
    }

    fn withheld_of(&self, account: AccountId) -> Withheld {
        self.accounts
            .borrow()
            .iter()
            .find(|(a, _, _)| a.id == account)
            .map_or(Withheld::NONE, |(_, _, withheld)| *withheld)
    }

    fn address_of(&self, account: AccountId) -> String {
        self.accounts
            .borrow()
            .iter()
            .find(|(a, _, _)| a.id == account)
            .map(|(a, _, _)| a.email.clone())
            .unwrap_or_default()
    }

    fn calendar_of(&self, account: AccountId, calendar: &str) -> Option<Calendar> {
        self.calendars.borrow().get(&(account, calendar.to_string())).cloned()
    }

    fn ask_rename(self: &Rc<Self>, account: AccountId, calendar: &str) {
        let Some(held) = self.calendar_of(account, calendar) else { return };
        if self.withheld_of(account).calendars {
            return self.needs(Permission::ManageCalendars, account);
        }
        let dialog = adw::AlertDialog::new(Some(&gettext("Rename Calendar")), None);
        let name = gtk::Entry::builder().text(&held.name).activates_default(true).build();
        crate::ui::name(&name, &gettext("Calendar name"));
        dialog.set_extra_child(Some(&name));
        dialog.add_responses(&[("cancel", &gettext("Cancel")), ("rename", &gettext("Rename"))]);
        dialog.set_response_appearance("rename", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("rename"));
        dialog.set_close_response("cancel");
        let (answer, was) = (dialog.clone(), held.name.clone());
        name.connect_changed(move |name| {
            let typed = name.text();
            answer.set_response_enabled("rename", !typed.trim().is_empty() && typed.trim() != was);
        });
        dialog.set_response_enabled("rename", false);
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            if ask_in(&dialog, &this.page, &name).await != "rename" {
                return;
            }
            let (copy, new_name) = (this.core.calendar_copy(), name.text().trim().to_string());
            let id = held.id.clone();
            this.run_list_edit(account, true, gettext("Could not rename the calendar: {reason}"), async move {
                copy.rename_calendar(account, &id, &new_name).await
            });
        });
    }

    fn ask_delete(self: &Rc<Self>, account: AccountId, calendar: &str) {
        let Some(held) = self.calendar_of(account, calendar) else { return };
        if self.withheld_of(account).calendars {
            return self.needs(Permission::ManageCalendars, account);
        }
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let id = held.id.clone();
            let copy = this.core.calendar_copy();
            let events = this
                .core
                .call(async move { copy.event_count(account, &id).await })
                .await
                .unwrap_or(0);
            let question = confirm(
                &fill(&gettext("Delete “{calendar}”?"), &[("calendar", &held.name)]),
                &delete_body(events),
                &gettext("Delete"),
                Tone::Destructive,
            );
            if !question.ask(&this.page).await {
                return;
            }
            let (copy, id) = (this.core.calendar_copy(), held.id.clone());
            this.run_list_edit(account, true, gettext("Could not delete the calendar: {reason}"), async move {
                copy.delete_calendar(account, &id).await
            });
            this.say(&fill(&gettext("Deleted “{calendar}”"), &[("calendar", &held.name)]));
        });
    }

    fn ask_unsubscribe(self: &Rc<Self>, account: AccountId, calendar: &str) {
        let Some(held) = self.calendar_of(account, calendar) else { return };
        if self.withheld_of(account).change_calendar_list {
            return self.needs(Permission::ManageCalendars, account);
        }
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let question = confirm(
                &fill(&gettext("Unsubscribe from “{calendar}”?"), &[("calendar", &held.name)]),
                &gettext(
                    "It leaves your calendar list in Google Calendar on every device. You can \
                     subscribe to it again later.",
                ),
                &gettext("Unsubscribe"),
                Tone::Destructive,
            );
            if !question.ask(&this.page).await {
                return;
            }
            let (copy, id) = (this.core.calendar_copy(), held.id.clone());
            this.run_list_edit(account, true, gettext("Could not unsubscribe: {reason}"), async move {
                copy.unsubscribe(account, &id).await
            });
        });
    }

    fn ask_new(self: &Rc<Self>, account: AccountId) {
        let withheld = self.withheld_of(account);
        if withheld.calendars || withheld.change_calendar_list {
            return self.needs(Permission::ManageCalendars, account);
        }
        super::ensure_tints(crate::ui::LABEL_COLORS.iter().map(|(hex, _)| *hex));
        let dialog = adw::AlertDialog::new(
            Some(&gettext("New Calendar")),
            Some(&fill(
                &gettext("A calendar of your own on {account}, in Google Calendar on every device."),
                &[("account", &self.address_of(account))],
            )),
        );
        let name = gtk::Entry::builder()
            .placeholder_text(gettext("Name"))
            .activates_default(true)
            .build();
        crate::ui::name(&name, &gettext("Calendar name"));
        let swatches = gtk::Box::builder().spacing(8).halign(gtk::Align::Center).build();
        crate::ui::name(&swatches, &gettext("Color"));
        let mut first: Option<gtk::ToggleButton> = None;
        let mut buttons = Vec::new();
        for (index, (hex, _)) in crate::ui::LABEL_COLORS.iter().enumerate() {
            let swatch = gtk::ToggleButton::builder()
                .css_classes(["calendar-swatch", &tint::css_class(hex)])
                .tooltip_text(crate::ui::label_color_name(index))
                .active(index == FIRST_COLOR)
                .build();
            crate::ui::name(&swatch, &crate::ui::label_color_name(index));
            match &first {
                Some(first) => swatch.set_group(Some(first)),
                None => first = Some(swatch.clone()),
            }
            swatches.append(&swatch);
            buttons.push((swatch, *hex));
        }
        let fields = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(18).build();
        fields.append(&name);
        fields.append(&swatches);
        dialog.set_extra_child(Some(&fields));
        dialog.add_responses(&[("cancel", &gettext("Cancel")), ("create", &gettext("Create"))]);
        dialog.set_response_appearance("create", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("create"));
        dialog.set_close_response("cancel");
        dialog.set_response_enabled("create", false);
        let answer = dialog.clone();
        name.connect_changed(move |name| answer.set_response_enabled("create", !name.text().trim().is_empty()));
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            if ask_in(&dialog, &this.page, &name).await != "create" {
                return;
            }
            let color = buttons
                .iter()
                .find(|(swatch, _)| swatch.is_active())
                .map_or(crate::ui::LABEL_COLORS[FIRST_COLOR].0, |(_, hex)| *hex)
                .to_string();
            let (copy, typed) = (this.core.calendar_copy(), name.text().trim().to_string());
            this.run_list_edit(account, true, gettext("Could not make the calendar: {reason}"), async move {
                copy.new_calendar(account, &typed, &color).await
            });
        });
    }

    fn ask_subscribe(self: &Rc<Self>, account: AccountId) {
        if self.withheld_of(account).change_calendar_list {
            return self.needs(Permission::ManageCalendars, account);
        }
        let dialog = adw::AlertDialog::new(
            Some(&gettext("Subscribe to Calendar")),
            Some(&gettext(
                "Enter the address of a published calendar, starting with https or webcal. \
                 Google Calendar fetches it from then on and shows it on every device. You \
                 can read it but not change it.",
            )),
        );
        let address = gtk::Entry::builder()
            .placeholder_text("https://example.com/calendar.ics")
            .input_purpose(gtk::InputPurpose::Url)
            .activates_default(true)
            .build();
        crate::ui::name(&address, &gettext("Calendar address"));
        dialog.set_extra_child(Some(&address));
        dialog.add_responses(&[("cancel", &gettext("Cancel")), ("subscribe", &gettext("Subscribe"))]);
        dialog.set_response_appearance("subscribe", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("subscribe"));
        dialog.set_close_response("cancel");
        dialog.set_response_enabled("subscribe", false);
        let answer = dialog.clone();
        address.connect_changed(move |address| {
            answer.set_response_enabled("subscribe", list::subscription_address(&address.text()).is_some());
        });
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            if ask_in(&dialog, &this.page, &address).await != "subscribe" {
                return;
            }
            let (copy, typed) = (this.core.calendar_copy(), address.text().to_string());
            this.run_list_edit(account, false, gettext("Could not subscribe: {reason}"), async move {
                copy.subscribe(account, &typed).await
            });
            this.say(&gettext("Subscribed. The events arrive once Google Calendar reads the address."));
        });
    }

    fn show_holidays(self: &Rc<Self>, account: AccountId) {
        if self.withheld_of(account).change_calendar_list {
            return self.needs(Permission::ManageCalendars, account);
        }
        let group = adw::PreferencesGroup::builder()
            .description(fill(
                &gettext("Public holidays from Google Calendar, added to {account}."),
                &[("account", &self.address_of(account))],
            ))
            .build();
        for region in holidays::regions() {
            group.add(&self.holiday_row(account, region));
        }
        let page = adw::PreferencesPage::new();
        page.add(&group);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        toolbar.set_content(Some(&page));
        let dialog = adw::Dialog::builder()
            .title(gettext("Holiday Calendars"))
            .content_width(420)
            .content_height(600)
            .child(&toolbar)
            .build();
        dialog.present(Some(&self.page));
    }

    /// One region's row: its name, and Add, or "Added" once the account
    /// has the region's calendar.
    fn holiday_row(self: &Rc<Self>, account: AccountId, region: Region) -> adw::ActionRow {
        let row = crate::ui::plain_row().title(&region.name).build();
        let added = || gtk::Label::builder().label(gettext("Added")).css_classes(["dim-label"]).build();
        if self.calendar_of(account, &region.calendar_id()).is_some() {
            row.add_suffix(&added());
            return row;
        }
        // A child label, since GTK names a button after its own label
        // over the name set below, and each row's Add must say which
        // region it adds.
        let add = gtk::Button::builder()
            .child(&gtk::Label::new(Some(&gettext("Add"))))
            .valign(gtk::Align::Center)
            .build();
        crate::ui::name(&add, &fill(&gettext("Add holidays in {region}"), &[("region", &region.name)]));
        row.add_suffix(&add);
        let (weak, held_row) = (Rc::downgrade(self), row.downgrade());
        add.connect_clicked(move |add| {
            let Some(view) = weak.upgrade() else { return };
            let copy = view.core.calendar_copy();
            let (id, name) = (region.calendar_id(), region.calendar_name());
            view.run_list_edit(account, false, gettext("Could not add the holidays: {reason}"), async move {
                copy.add_public(account, &id, &name).await
            });
            add.set_visible(false);
            if let Some(row) = held_row.upgrade() {
                row.add_suffix(&added());
            }
        });
        row
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deleting_an_empty_calendar_names_no_events() {
        assert!(!delete_body(0).contains('{'));
        assert!(delete_body(0).starts_with("The calendar is deleted"));
    }

    #[test]
    fn deleting_a_calendar_says_how_many_events_go() {
        assert!(delete_body(1).contains("its 1 event are"), "{}", delete_body(1));
        assert!(delete_body(12).contains("its 12 events"));
    }

    #[test]
    fn only_changes_that_write_the_list_name_an_account_to_check() {
        let on = |c: &str| (7, c.to_string());
        let (account, calendar) = on("work");
        assert_eq!(list_edit_of(&ListChange::Rename { account, calendar: calendar.clone() }), Some(7));
        assert_eq!(list_edit_of(&ListChange::Color { account, calendar: calendar.clone(), color: None }), Some(7));
        assert_eq!(list_edit_of(&ListChange::Add { account, kind: AddKind::Subscribe }), Some(7));
        assert_eq!(list_edit_of(&ListChange::Shown { account, calendar, shown: true }), None);
    }

    #[test]
    fn subscribing_unsubscribing_and_holidays_need_an_account_that_takes_subscriptions() {
        let (account, calendar) = (7, "fixtures".to_string());
        assert_eq!(subscription_of(&ListChange::Add { account, kind: AddKind::Subscribe }), Some(7));
        assert_eq!(subscription_of(&ListChange::Add { account, kind: AddKind::Holidays }), Some(7));
        assert_eq!(subscription_of(&ListChange::Unsubscribe { account, calendar: calendar.clone() }), Some(7));
        assert_eq!(subscription_of(&ListChange::Add { account, kind: AddKind::New }), None);
        assert_eq!(subscription_of(&ListChange::Delete { account, calendar }), None);
    }
}
