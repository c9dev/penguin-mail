//! The account settings of one account: the automatic reply, the filters
//! behind Rules and Block Sender, the labels, the Hide My Email
//! addresses, and the addresses it sends as. The dialogs and the assistant
//! both change settings through this module, so only it knows that
//! Gmail's automatic reply stops before the end it stores, which labels
//! and filters a hidden address needs, and what Gmail answers when the
//! account has not granted the settings permission.

use std::sync::Arc;

use chrono::{Local, TimeZone};
use mailrs_domain::{
    AccountId, EpochMillis, Filter, FilterAction, FilterCriteria, Label, Vacation, system_label,
};
use mailrs_gmail::LabelColor;
use mailrs_store::Db;

use crate::actions::label_id;
use crate::hidden::{self, HiddenAddress};
use crate::{
    AccountServices, AccountSync, Accounts, AnyAutoReply, AnyRules, AutoReplyService, BackendError,
    IdentityService, RulesService, SendAsAddress, SyncError, now_millis,
};

/// The user label that mail to a hidden address gets.
pub const HIDE_MY_EMAIL_LABEL: &str = "Hide My Email";

const DAY: EpochMillis = 24 * 60 * 60 * 1000;

/// What a Gmail call that needs a permission of its own gave back: every
/// settings call, and erasing mail. Gmail refuses those until the account
/// grants the permission, and the caller then asks the user for it, so the
/// refusal is a value to match on rather than an error to take apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Permitted<T> {
    Done(T),
    /// Gmail wants the permission before it answers.
    NeedsPermission,
}

impl<T> Permitted<T> {
    /// The value, or `None` when Gmail wants the permission first.
    pub fn done(self) -> Option<T> {
        match self {
            Permitted::Done(value) => Some(value),
            Permitted::NeedsPermission => None,
        }
    }
}

/// Gmail's automatic reply for one account, in whole days. Gmail stops
/// replying at a moment rather than at the end of a day, so this holds the
/// last day it answers on and the module does the conversion.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AutomaticReply {
    pub enabled: bool,
    pub subject: String,
    /// Plain text. Gmail gets an HTML copy with the same lines.
    pub body: String,
    /// Reply only to people in the account's contacts.
    pub contacts_only: bool,
    /// Reply only to people in the account's Workspace domain. The dialogs
    /// leave this as Gmail has it.
    pub domain_only: bool,
    /// Any time on the first day Gmail replies. Reading one back gives
    /// local midnight.
    pub first_day: Option<EpochMillis>,
    /// Any time on the last day Gmail replies, that day included.
    pub last_day: Option<EpochMillis>,
}

impl AutomaticReply {
    fn from_gmail(vacation: &Vacation) -> AutomaticReply {
        AutomaticReply {
            enabled: vacation.enabled,
            subject: vacation.subject.clone(),
            body: vacation.body.clone(),
            contacts_only: vacation.contacts_only,
            domain_only: vacation.domain_only,
            first_day: vacation.start.map(midnight),
            // Gmail stops at `end`; the last day it answers on is the one
            // that moment falls after.
            last_day: vacation.end.map(|end| midnight(end - 1)),
        }
    }

    fn to_gmail(&self) -> Vacation {
        Vacation {
            enabled: self.enabled,
            subject: self.subject.clone(),
            body: self.body.clone(),
            contacts_only: self.contacts_only,
            domain_only: self.domain_only,
            start: self.first_day.map(midnight),
            end: self.last_day.map(|last| midnight(last) + DAY),
        }
    }
}

pub struct AccountSettings<A: Accounts> {
    accounts: Arc<A>,
    db: Db,
}

/// The value, or an early return when Gmail wants the permission first.
macro_rules! done {
    ($call:expr) => {
        match permitted($call)? {
            Permitted::Done(value) => value,
            Permitted::NeedsPermission => return Ok(Permitted::NeedsPermission),
        }
    };
}

impl<A: Accounts> AccountSettings<A> {
    pub fn new(accounts: Arc<A>, db: Db) -> Self {
        AccountSettings { accounts, db }
    }

    /// The automatic reply the server holds, in whole days.
    pub async fn automatic_reply(
        &self,
        account_id: AccountId,
    ) -> Result<Permitted<AutomaticReply>, SyncError> {
        let vacation = done!(self.auto_reply_service(account_id)?.vacation().await);
        Ok(Permitted::Done(AutomaticReply::from_gmail(&vacation)))
    }

    /// Stores the automatic reply. Gmail keeps answering until the midnight
    /// after `last_day`.
    pub async fn set_automatic_reply(
        &self,
        account_id: AccountId,
        reply: &AutomaticReply,
    ) -> Result<Permitted<()>, SyncError> {
        let service = self.auto_reply_service(account_id)?;
        permitted(service.set_vacation(&reply.to_gmail()).await)
    }

    /// The account's filters, newest last, as the server returns them.
    pub async fn rules(&self, account_id: AccountId) -> Result<Permitted<Vec<Filter>>, SyncError> {
        permitted(self.rules_service(account_id)?.filters().await)
    }

    /// Adds a filter. The server gives the stored one an id.
    pub async fn add_rule(
        &self,
        account_id: AccountId,
        rule: Filter,
    ) -> Result<Permitted<Filter>, SyncError> {
        permitted(self.rules_service(account_id)?.create_filter(&rule).await)
    }

    /// Deletes a filter. A filter the server no longer has counts as
    /// deleted.
    pub async fn delete_rule(
        &self,
        account_id: AccountId,
        id: &str,
    ) -> Result<Permitted<()>, SyncError> {
        permitted(delete_filter(&self.rules_service(account_id)?, id).await)
    }

    /// Sends mail from `email` straight to the Trash from now on.
    pub async fn block_sender(
        &self,
        account_id: AccountId,
        email: &str,
    ) -> Result<Permitted<Filter>, SyncError> {
        self.add_rule(account_id, Filter::block(email)).await
    }

    /// Creates a user label. Slashes nest it, as in Gmail: "Work/Clients".
    pub async fn create_label(
        &self,
        account_id: AccountId,
        name: &str,
    ) -> Result<Permitted<Label>, SyncError> {
        permitted(self.sync(account_id)?.create_label(name).await)
    }

    /// Renames a label, and the labels nested under it along with it.
    pub async fn rename_label(
        &self,
        account_id: AccountId,
        id: &str,
        name: &str,
    ) -> Result<Permitted<()>, SyncError> {
        permitted(self.sync(account_id)?.rename_label(id, name).await)
    }

    /// Gives a label one of Gmail's colours.
    pub async fn recolor_label(
        &self,
        account_id: AccountId,
        id: &str,
        color: LabelColor,
    ) -> Result<Permitted<()>, SyncError> {
        permitted(self.sync(account_id)?.set_label_color(id, color).await)
    }

    /// How many conversations in the whole mailbox carry the label, for
    /// the question before deleting it.
    pub async fn label_threads(
        &self,
        account_id: AccountId,
        id: &str,
    ) -> Result<Permitted<u64>, SyncError> {
        permitted(self.sync(account_id)?.label_threads(id).await)
    }

    /// Deletes a label. Its mail stays, without the label.
    pub async fn delete_label(
        &self,
        account_id: AccountId,
        id: &str,
    ) -> Result<Permitted<()>, SyncError> {
        permitted(self.sync(account_id)?.delete_label(id).await)
    }

    /// Makes a new hidden address of `account_email`, the address of
    /// `account_id`, that none of `taken` uses. Mail to it gets the Hide
    /// My Email label, which this creates when the account lacks it. The
    /// caller keeps what comes back.
    pub async fn create_hidden_address(
        &self,
        account_id: AccountId,
        account_email: &str,
        note: &str,
        taken: &[HiddenAddress],
    ) -> Result<Permitted<HiddenAddress>, SyncError> {
        let address = hidden::fresh(account_email, taken)
            .map_err(|_| SyncError::NotAnAddress(account_email.to_string()))?;
        let rules = self.rules_service(account_id)?;
        let label = done!(
            label_id(
                self.accounts.as_ref(),
                &self.db,
                account_id,
                HIDE_MY_EMAIL_LABEL,
                true,
            )
            .await
        );
        let created = done!(rules.create_filter(&labels(&address, &label)).await);
        Ok(Permitted::Done(HiddenAddress {
            account: account_email.to_string(),
            address,
            note: note.trim().to_string(),
            created: now_millis(),
            active: true,
            label_filter: created.id,
            trash_filter: None,
        }))
    }

    /// Turns a hidden address on or off. Off adds the filter that trashes
    /// its mail; on drops that filter again. The caller keeps the address
    /// that comes back in place of `hidden`.
    pub async fn set_hidden_address_active(
        &self,
        account_id: AccountId,
        hidden: &HiddenAddress,
        active: bool,
    ) -> Result<Permitted<HiddenAddress>, SyncError> {
        let rules = self.rules_service(account_id)?;
        let trash_filter = match (active, hidden.trash_filter.clone()) {
            (true, Some(id)) => {
                done!(delete_filter(&rules, &id).await);
                None
            }
            (false, None) => done!(rules.create_filter(&trashes(&hidden.address)).await).id,
            (_, current) => current,
        };
        Ok(Permitted::Done(HiddenAddress {
            active,
            trash_filter,
            ..hidden.clone()
        }))
    }

    /// Drops a hidden address's filters. Mail to it then arrives like any
    /// other mail, and the caller forgets the address.
    pub async fn delete_hidden_address(
        &self,
        account_id: AccountId,
        hidden: &HiddenAddress,
    ) -> Result<Permitted<()>, SyncError> {
        let rules = self.rules_service(account_id)?;
        for id in [&hidden.label_filter, &hidden.trash_filter]
            .into_iter()
            .flatten()
        {
            done!(delete_filter(&rules, id).await);
        }
        Ok(Permitted::Done(()))
    }

    /// Every address the account may send mail from, its own included,
    /// with the display name and signature the server keeps for each.
    pub async fn send_as(&self, account_id: AccountId) -> Result<Vec<SendAsAddress>, SyncError> {
        Ok(self.services(account_id)?.identities.identities().await?)
    }

    /// The name the account's server puts on its outgoing mail, if one is
    /// set.
    pub async fn display_name(&self, account_id: AccountId) -> Result<Option<String>, SyncError> {
        let addresses = self.send_as(account_id).await?;
        Ok(addresses
            .into_iter()
            .find(|address| address.default)
            .and_then(|address| address.name))
    }

    /// The signature of the default address, as plain text.
    pub async fn signature(&self, account_id: AccountId) -> Result<Option<String>, SyncError> {
        let addresses = self.send_as(account_id).await?;
        Ok(addresses
            .into_iter()
            .find(|address| address.default)
            .map(|address| address.signature)
            .filter(|text| !text.is_empty()))
    }

    fn sync(&self, account_id: AccountId) -> Result<Arc<AccountSync>, SyncError> {
        self.accounts
            .account(account_id)
            .ok_or(SyncError::UnknownAccount(account_id))
    }

    fn services(&self, account_id: AccountId) -> Result<AccountServices, SyncError> {
        self.accounts
            .services(account_id)
            .ok_or(SyncError::UnknownAccount(account_id))
    }

    fn rules_service(&self, account_id: AccountId) -> Result<AnyRules, SyncError> {
        Ok(self.services(account_id)?.rules)
    }

    fn auto_reply_service(&self, account_id: AccountId) -> Result<AnyAutoReply, SyncError> {
        self.services(account_id)?
            .auto_reply
            .ok_or(SyncError::Backend(BackendError::Unsupported))
    }
}

/// A refusal for a missing permission, as a value. Service calls answer a
/// `BackendError` and the label calls a `SyncError`; both come here.
fn permitted<T>(result: Result<T, impl Into<SyncError>>) -> Result<Permitted<T>, SyncError> {
    match result.map_err(Into::<SyncError>::into) {
        Ok(value) => Ok(Permitted::Done(value)),
        Err(SyncError::Backend(BackendError::NeedsPermission)) => Ok(Permitted::NeedsPermission),
        Err(err) => Err(err),
    }
}

/// Deletes filter `id`. A filter the server no longer has counts as
/// deleted, so a hidden address whose filter went elsewhere still turns
/// off and on.
async fn delete_filter(rules: &AnyRules, id: &str) -> Result<(), BackendError> {
    match rules.delete_filter(id).await {
        Ok(()) | Err(BackendError::NotFound) => Ok(()),
        Err(err) => Err(err),
    }
}

/// The filter that gives mail to `address` the label `label_id`.
fn labels(address: &str, label_id: &str) -> Filter {
    Filter {
        id: None,
        criteria: to(address),
        action: FilterAction {
            add_label_ids: vec![label_id.to_string()],
            ..FilterAction::default()
        },
    }
}

/// The filter that sends mail to `address` to the Trash, skipping the Inbox.
fn trashes(address: &str) -> Filter {
    Filter {
        id: None,
        criteria: to(address),
        action: FilterAction {
            add_label_ids: vec![system_label::TRASH.into()],
            remove_label_ids: vec![system_label::INBOX.into()],
            forward: None,
        },
    }
}

fn to(address: &str) -> FilterCriteria {
    FilterCriteria {
        to: Some(address.to_string()),
        ..FilterCriteria::default()
    }
}

/// Local midnight at the start of the day `at` falls in.
fn midnight(at: EpochMillis) -> EpochMillis {
    Local
        .timestamp_millis_opt(at)
        .single()
        .map(|when| when.date_naive())
        .and_then(|day| day.and_hms_opt(0, 0, 0))
        .and_then(|start| Local.from_local_datetime(&start).earliest())
        .map(|start| start.timestamp_millis())
        .unwrap_or(at)
}
