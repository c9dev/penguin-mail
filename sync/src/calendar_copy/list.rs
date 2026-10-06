//! The calendar list half of the local copy: making, renaming, colouring,
//! hiding and deleting a calendar, and subscribing to one. Each change
//! shows in the copy at once and waits in `calendar_list_changes` until
//! [`CalendarCopy::send`] takes it to the provider, ahead of the event
//! changes, since an event may sit on a calendar made here.
//!
//! What each change needs: the calendars permission to make, rename or
//! delete one (`Withheld::calendars`), and the list permission to colour
//! or hide one on every device and to subscribe (`Withheld::change_calendar_list`).
//! A colour or a hide without the list permission stays on this computer,
//! as both did before the account could change its list.

use mailrs_domain::AccountId;
use mailrs_domain::calendar::list::{self, ListEdit};
use mailrs_domain::calendar::{Access, Calendar};
use mailrs_store::calendar as store;
use mailrs_store::calendar_list as store_list;

use super::{CalendarCopy, TurnedDown, holds_the_queue};
use crate::settings::Permitted;
use crate::{Accounts, AnyCalendar, BackendError, CalendarService, SyncError, Withheld};

/// The colour a subscription shows in until the provider says its own.
const SUBSCRIPTION_COLOR: &str = "#9e69af";

/// An id for a calendar made on this computer, which the provider
/// replaces with its own once the queue sends it.
pub fn new_calendar_id() -> String {
    format!("{}{}", list::LOCAL_ID_PREFIX, super::new_event_id())
}

impl<A: Accounts> CalendarCopy<A> {
    /// Makes a calendar the account owns, named `name` in `color`
    /// (`#rrggbb`), in the primary calendar's zone. Answers the id the
    /// copy files it under until the provider gives its own.
    pub async fn new_calendar(
        &self,
        account_id: AccountId,
        name: &str,
        color: &str,
    ) -> Result<Permitted<String>, SyncError> {
        self.service(account_id)?;
        let withheld = self.withheld_all(account_id)?;
        if withheld.calendars || withheld.change_calendar_list {
            return Ok(Permitted::NeedsPermission);
        }
        let id = new_calendar_id();
        let (name, color) = (name.trim().to_string(), color.to_string());
        let made = id.clone();
        self.db
            .write(move |c| {
                let zone = store::calendars(c, account_id)?
                    .into_iter()
                    .find(|c| c.primary)
                    .map(|c| c.zone)
                    .unwrap_or_default();
                let calendar = Calendar {
                    id: made.clone(),
                    name: name.clone(),
                    color: color.clone(),
                    access: Access::Owner,
                    zone: zone.clone(),
                    shown: true,
                    ..Calendar::default()
                };
                store_list::add_calendar(c, account_id, &calendar)?;
                store_list::enqueue_edit(c, account_id, &made, &ListEdit::Create { name, color, zone }).map(drop)
            })
            .await?;
        Ok(Permitted::Done(id))
    }

    /// Gives a calendar the account owns a new name.
    pub async fn rename_calendar(
        &self,
        account_id: AccountId,
        calendar: &str,
        name: &str,
    ) -> Result<Permitted<()>, SyncError> {
        let held = self.owned(account_id, calendar, |allows| allows.rename).await?;
        if self.withheld_all(account_id)?.calendars {
            return Ok(Permitted::NeedsPermission);
        }
        let (id, name) = (held.id, name.trim().to_string());
        self.db
            .write(move |c| {
                store_list::set_name(c, account_id, &id, &name)?;
                store_list::enqueue_edit(c, account_id, &id, &ListEdit::Rename { name }).map(drop)
            })
            .await?;
        Ok(Permitted::Done(()))
    }

    /// Deletes a calendar the account owns, other than its primary, with
    /// every event on it.
    pub async fn delete_calendar(&self, account_id: AccountId, calendar: &str) -> Result<Permitted<()>, SyncError> {
        let held = self.owned(account_id, calendar, |allows| allows.delete).await?;
        if self.withheld_all(account_id)?.calendars {
            return Ok(Permitted::NeedsPermission);
        }
        let id = held.id;
        self.db
            .write(move |c| {
                store_list::remove_calendar(c, account_id, &id)?;
                store_list::enqueue_edit(c, account_id, &id, &ListEdit::Delete).map(drop)
            })
            .await?;
        Ok(Permitted::Done(()))
    }

    /// Takes a calendar the account does not own off its list, on every
    /// device: a subscription, a holiday calendar or a shared one.
    pub async fn unsubscribe(&self, account_id: AccountId, calendar: &str) -> Result<Permitted<()>, SyncError> {
        let held = self.owned(account_id, calendar, |allows| allows.unsubscribe).await?;
        if self.withheld_all(account_id)?.change_calendar_list {
            return Ok(Permitted::NeedsPermission);
        }
        let id = held.id;
        self.db
            .write(move |c| {
                store_list::remove_calendar(c, account_id, &id)?;
                store_list::enqueue_edit(c, account_id, &id, &ListEdit::Unsubscribe).map(drop)
            })
            .await?;
        Ok(Permitted::Done(()))
    }

    /// Colours a calendar, or with `None` gives it back the provider's
    /// colour. With the list permission the colour goes to the provider
    /// and shows on every device; without it, it stays on this computer.
    pub async fn recolor_calendar(
        &self,
        account_id: AccountId,
        calendar: &str,
        color: Option<String>,
    ) -> Result<Permitted<()>, SyncError> {
        self.service(account_id)?;
        let everywhere = !self.withheld_all(account_id)?.change_calendar_list;
        let id = calendar.to_string();
        self.db
            .write(move |c| match (color, everywhere) {
                (Some(color), true) => {
                    store_list::set_provider_color(c, account_id, &id, &color)?;
                    store_list::enqueue_edit(c, account_id, &id, &ListEdit::Recolor { color }).map(drop)
                }
                // The provider's colour is already in the row; the own one
                // only goes.
                (None, _) => store::set_own_color(c, account_id, &id, None),
                (Some(color), false) => store::set_own_color(c, account_id, &id, Some(&color)),
            })
            .await?;
        Ok(Permitted::Done(()))
    }

    /// Takes a calendar off the sidebar's list, or puts it back. With the
    /// list permission the provider's own list follows, so the person's
    /// other devices do too.
    pub async fn list_calendar(
        &self,
        account_id: AccountId,
        calendar: &str,
        listed: bool,
    ) -> Result<Permitted<()>, SyncError> {
        self.service(account_id)?;
        let everywhere = self.hides_everywhere(account_id)?;
        let id = calendar.to_string();
        self.db
            .write(move |c| {
                store::set_listed(c, account_id, &id, listed)?;
                // A calendar the provider does not have yet goes out hidden
                // or not with the rest of its edits, in order.
                if everywhere {
                    store_list::enqueue_edit(c, account_id, &id, &ListEdit::Hide { hidden: !listed })?;
                }
                Ok(())
            })
            .await?;
        Ok(Permitted::Done(()))
    }

    /// Subscribes the account to the calendar published at `typed`, an
    /// `http`, `https` or `webcal` address. The provider fetches it from
    /// then on, and the copy reads it as a calendar the account only
    /// reads. Answers the id the copy files it under until the provider
    /// gives its own.
    pub async fn subscribe(&self, account_id: AccountId, typed: &str) -> Result<Permitted<String>, SyncError> {
        self.service(account_id)?;
        let url = list::subscription_address(typed)
            .ok_or_else(|| SyncError::Backend(BackendError::Refused(typed.to_string())))?;
        if self.withheld_all(account_id)?.change_calendar_list {
            return Ok(Permitted::NeedsPermission);
        }
        let id = new_calendar_id();
        let calendar = Calendar {
            id: id.clone(),
            name: list::subscription_name(&url),
            color: SUBSCRIPTION_COLOR.into(),
            access: Access::Reader,
            shown: true,
            ..Calendar::default()
        };
        self.db
            .write(move |c| {
                store_list::add_calendar(c, account_id, &calendar)?;
                store_list::enqueue_edit(c, account_id, &calendar.id, &ListEdit::Subscribe { url }).map(drop)
            })
            .await?;
        Ok(Permitted::Done(id))
    }

    /// Puts a calendar the provider publishes, such as a public holiday
    /// calendar, on the account's list by its id. `name` shows until the
    /// provider's own name arrives.
    pub async fn add_public(&self, account_id: AccountId, id: &str, name: &str) -> Result<Permitted<()>, SyncError> {
        self.service(account_id)?;
        if self.withheld_all(account_id)?.change_calendar_list {
            return Ok(Permitted::NeedsPermission);
        }
        let calendar = Calendar {
            id: id.to_string(),
            name: name.to_string(),
            color: SUBSCRIPTION_COLOR.into(),
            access: Access::Reader,
            shown: true,
            ..Calendar::default()
        };
        self.db
            .write(move |c| {
                store_list::add_calendar(c, account_id, &calendar)?;
                store::set_listed(c, account_id, &calendar.id, true)?;
                store_list::enqueue_edit(c, account_id, &calendar.id, &ListEdit::Add).map(drop)
            })
            .await?;
        Ok(Permitted::Done(()))
    }

    /// Sends the account's calendar list edits in order. An edit the
    /// provider turns down leaves the queue and comes back as a
    /// `TurnedDown`, and the next read of the list puts the provider's
    /// version back. A failure that may pass stops the send and keeps the
    /// edit and the rest for next time.
    pub(super) async fn send_list(
        &self,
        calendar: &AnyCalendar,
        account_id: AccountId,
    ) -> Result<Vec<TurnedDown>, SyncError> {
        let mut turned_down = Vec::new();
        while let Some(queued) = self.db.read(move |c| store_list::next_edit(c, account_id)).await? {
            let seq = queued.seq;
            match calendar.edit_list(&queued.calendar, &queued.edit).await {
                Ok(answer) => {
                    let (local, edit) = (queued.calendar.clone(), queued.edit.clone());
                    self.db
                        .write(move |c| {
                            if let Some(listed) = answer.filter(|_| edit.adds()) {
                                store_list::rename_calendar_id(c, account_id, &local, &listed.id)?;
                                store_list::refresh_calendar(c, account_id, &listed)?;
                            }
                            if let ListEdit::Hide { hidden } = edit {
                                store_list::set_provider_hidden(c, account_id, &local, hidden)?;
                            }
                            store_list::finish_edit(c, seq)
                        })
                        .await?;
                }
                // Gone already or not, the calendar is off the list.
                Err(BackendError::NotFound) if queued.edit.removes() => {
                    self.db.write(move |c| store_list::finish_edit(c, seq)).await?;
                }
                Err(err) if holds_the_queue(&err) => return Err(err.into()),
                Err(err) => {
                    let reason = match err {
                        BackendError::Refused(reason) => reason,
                        other => other.to_string(),
                    };
                    let id = queued.calendar.clone();
                    let title = self
                        .db
                        .write(move |c| {
                            store_list::finish_edit(c, seq)?;
                            let name = store::calendars(c, account_id)?.into_iter().find(|c| c.id == id).map(|c| c.name);
                            // A calendar made here that the provider refused
                            // has nothing to come back to.
                            if list::is_local(&id) {
                                store_list::remove_calendar(c, account_id, &id)?;
                            }
                            Ok(name)
                        })
                        .await?;
                    // The next read of the list puts back what the provider
                    // holds: the old name, the calendar a delete took off.
                    self.last_list.lock().expect("copy poisoned").remove(&account_id);
                    turned_down.push(TurnedDown {
                        account_id,
                        calendar: queued.calendar.clone(),
                        event: String::new(),
                        title: title.unwrap_or_else(|| edit_title(&queued.edit)),
                        reason: Some(reason),
                        left_out: None,
                    });
                }
            }
        }
        Ok(turned_down)
    }

    /// The calendar `calendar` when the account's service has one and
    /// `allowed` says a calendar like it takes the change; otherwise
    /// `Unsupported`, which no permission would change.
    async fn owned(
        &self,
        account_id: AccountId,
        calendar: &str,
        allowed: fn(list::Allows) -> bool,
    ) -> Result<Calendar, SyncError> {
        self.service(account_id)?;
        let id = calendar.to_string();
        let held = self
            .db
            .read(move |c| Ok(store::calendars(c, account_id)?.into_iter().find(|c| c.id == id)))
            .await?;
        held.filter(|c| allowed(list::allows(c))).ok_or(SyncError::Backend(BackendError::Unsupported))
    }

    /// The account's calendar service, or `Unsupported` for an account
    /// whose provider offers none.
    fn service(&self, account_id: AccountId) -> Result<AnyCalendar, SyncError> {
        self.calendar(account_id)?.ok_or(SyncError::Backend(BackendError::Unsupported))
    }

    /// Whether a hide goes to the provider's own list: the provider keeps
    /// a hidden flag and the person allowed changes to the list.
    pub(super) fn hides_everywhere(&self, account_id: AccountId) -> Result<bool, SyncError> {
        let services = self.accounts.services(account_id).ok_or(SyncError::UnknownAccount(account_id))?;
        Ok(services.hides_calendars() && !services.withheld().change_calendar_list)
    }

    fn withheld_all(&self, account_id: AccountId) -> Result<Withheld, SyncError> {
        Ok(self.accounts.services(account_id).ok_or(SyncError::UnknownAccount(account_id))?.withheld())
    }
}

/// What to call a calendar in a toast when the copy no longer has it.
fn edit_title(edit: &ListEdit) -> String {
    match edit {
        ListEdit::Create { name, .. } | ListEdit::Rename { name } => name.clone(),
        ListEdit::Subscribe { url } => list::subscription_name(url),
        ListEdit::Delete
        | ListEdit::Unsubscribe
        | ListEdit::Recolor { .. }
        | ListEdit::Hide { .. }
        | ListEdit::Add => String::new(),
    }
}
