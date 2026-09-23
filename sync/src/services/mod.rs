//! What an account can be asked to do, split by kind of work, since
//! providers split it the same way: Fastmail reads mail over IMAP and its
//! calendar over CalDAV. Each kind is a trait here. A provider's adapter
//! implements the kinds it offers; [`Google`] is the only adapter so far.
//!
//! The mail trait still speaks Gmail's shapes (label ids, history pages,
//! drafts by Gmail's draft id), because mailboxes and keywords only exist
//! once the store holds them. Its errors and its pacing are neutral
//! already.

mod any;
mod google;
mod pacing;

pub use any::{AnyAutoReply, AnyCalendar, AnyContacts, AnyIdentities, AnyMail, AnyRules};
pub use google::Google;
pub use pacing::{Priority, background, priority};

use std::sync::Arc;
use std::time::Duration;

use mailrs_domain::invitation::Answer;
use mailrs_domain::{EpochMillis, Filter, MessageBody, MessageMeta, Vacation};
use mailrs_gmail::{
    Answered, Busy, ConnectionsPage, ContactFields, Event, EventFields, HistoryPage, LabelColor,
    MessagePage, Person, Profile, RemoteLabel, Series,
};

use crate::BackendError;
use crate::api::{AccountClient, DraftRef, SavedDraft};
#[cfg(any(test, feature = "fake"))]
use crate::fake::FakeGmail;

/// One address an account may send mail as: its own, or an alias whose
/// owner has confirmed it. The server keeps a display name and a signature
/// per address, so all three travel together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendAsAddress {
    pub email: String,
    pub name: Option<String>,
    /// The signature the server holds for this address, as plain text.
    pub signature: String,
    /// The address the server sends from when the writer picks none.
    pub default: bool,
}

/// What an account's mail service can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MailCapabilities {
    /// A message can sit in several mailboxes at once, as with labels.
    pub labels: bool,
    /// The server groups messages into threads itself.
    pub server_threads: bool,
    /// The server files a copy of what the account sends.
    pub files_sent_mail: bool,
    /// The server sorts the inbox into categories.
    pub categories: bool,
    /// Mail can be erased for good, not only moved to the Trash.
    pub delete_forever: bool,
}

/// The services one account is served by. A provider that lacks one leaves
/// its field `None`, and the module that needs it answers
/// `BackendError::Unsupported`.
#[derive(Clone)]
pub struct AccountServices {
    pub mail: AnyMail,
    pub calendar: Option<AnyCalendar>,
    pub contacts: Option<AnyContacts>,
    pub rules: AnyRules,
    pub auto_reply: Option<AnyAutoReply>,
    pub identities: AnyIdentities,
}

impl AccountServices {
    /// A Google account: every service, all over the one client, which
    /// spends one quota bucket for all of them.
    pub fn google(client: AccountClient) -> Self {
        let google = Google::new(Arc::new(client));
        AccountServices {
            mail: AnyMail::Google(google.clone()),
            calendar: Some(AnyCalendar::Google(google.clone())),
            contacts: Some(AnyContacts::Google(google.clone())),
            rules: AnyRules::Google(google.clone()),
            auto_reply: Some(AnyAutoReply::Google(google.clone())),
            identities: AnyIdentities::Google(google),
        }
    }

    /// The in-memory Gmail, for tests and the demo, through the same
    /// adapter a real account uses.
    #[cfg(any(test, feature = "fake"))]
    pub fn fake(gmail: Arc<FakeGmail>) -> Self {
        let google = Google::new(gmail);
        AccountServices {
            mail: AnyMail::Fake(google.clone()),
            calendar: Some(AnyCalendar::Fake(google.clone())),
            contacts: Some(AnyContacts::Fake(google.clone())),
            rules: AnyRules::Fake(google.clone()),
            auto_reply: Some(AnyAutoReply::Fake(google.clone())),
            identities: AnyIdentities::Fake(google),
        }
    }

    pub fn capabilities(&self) -> MailCapabilities {
        self.mail.capabilities()
    }
}

/// Listing, reading, changing and sending mail.
pub trait MailBackend: Send + Sync + 'static {
    fn capabilities(&self) -> MailCapabilities;

    /// Whether the person is waiting on this account's server now.
    /// Backfill reads it between pages and gives way.
    fn person_waiting(&self) -> bool;

    /// Sits out `wait` on the person's behalf, as a mail action waiting
    /// out a rate limit does. The account's background work stands aside
    /// for the whole wait.
    fn stand_by(&self, wait: Duration) -> impl Future<Output = ()> + Send;

    fn profile(&self) -> impl Future<Output = Result<Profile, BackendError>> + Send;

    fn labels(&self) -> impl Future<Output = Result<Vec<RemoteLabel>, BackendError>> + Send;

    /// One page of a search, at most `page_size` ids long.
    fn list_messages(
        &self,
        query: &str,
        page_token: Option<&str>,
        page_size: u32,
    ) -> impl Future<Output = Result<MessagePage, BackendError>> + Send;

    /// One page of the messages `query` matches that carry `label_id`.
    /// Compares label membership with the store without fetching metadata.
    fn list_labelled(
        &self,
        label_id: &str,
        query: &str,
        page_token: Option<&str>,
        page_size: u32,
    ) -> impl Future<Output = Result<MessagePage, BackendError>> + Send;

    fn message_metadata(
        &self,
        id: &str,
    ) -> impl Future<Output = Result<MessageMeta, BackendError>> + Send;

    /// Every message in the thread, oldest first.
    fn thread_metadata(
        &self,
        thread_id: &str,
    ) -> impl Future<Output = Result<Vec<MessageMeta>, BackendError>> + Send;

    fn message_body(
        &self,
        id: &str,
    ) -> impl Future<Output = Result<MessageBody, BackendError>> + Send;

    /// One page of the changes since `start_history_id`. Answers
    /// `BackendError::StateLost` once the server no longer keeps changes
    /// that old, and the caller lists the mail again.
    fn history(
        &self,
        start_history_id: u64,
        page_token: Option<&str>,
    ) -> impl Future<Output = Result<HistoryPage, BackendError>> + Send;

    fn modify_labels(
        &self,
        id: &str,
        add: &[String],
        remove: &[String],
    ) -> impl Future<Output = Result<(), BackendError>> + Send;

    /// One label change over many messages in a single call, at most
    /// [`mailrs_gmail::BATCH_LIMIT`] ids; the caller splits longer lists.
    fn batch_modify(
        &self,
        ids: &[String],
        add: &[String],
        remove: &[String],
    ) -> impl Future<Output = Result<(), BackendError>> + Send;

    /// Erases messages for good. Answers `BackendError::NeedsPermission`
    /// until the account grants the delete permission.
    fn delete_messages(
        &self,
        ids: &[String],
    ) -> impl Future<Output = Result<(), BackendError>> + Send;

    /// Sends raw RFC 822 bytes. Returns the new message id.
    fn send(
        &self,
        raw: &[u8],
        thread_id: Option<&str>,
    ) -> impl Future<Output = Result<String, BackendError>> + Send;

    /// Creates a draft, or replaces draft `draft_id`.
    fn save_draft(
        &self,
        draft_id: Option<&str>,
        raw: &[u8],
        thread_id: Option<&str>,
    ) -> impl Future<Output = Result<SavedDraft, BackendError>> + Send;

    /// Sends a draft as the server holds it. Returns the sent message's id.
    fn send_draft(
        &self,
        draft_id: &str,
    ) -> impl Future<Output = Result<String, BackendError>> + Send;

    fn delete_draft(&self, draft_id: &str)
    -> impl Future<Output = Result<(), BackendError>> + Send;

    /// Every draft in the account, each with the message inside it.
    fn list_drafts(&self) -> impl Future<Output = Result<Vec<DraftRef>, BackendError>> + Send;

    fn attachment(
        &self,
        message_id: &str,
        attachment_id: &str,
    ) -> impl Future<Output = Result<Vec<u8>, BackendError>> + Send;

    /// The message as it arrived, in RFC 822 form.
    fn raw_message(&self, id: &str) -> impl Future<Output = Result<Vec<u8>, BackendError>> + Send;

    fn create_label(
        &self,
        name: &str,
    ) -> impl Future<Output = Result<RemoteLabel, BackendError>> + Send;

    fn rename_label(
        &self,
        id: &str,
        name: &str,
    ) -> impl Future<Output = Result<RemoteLabel, BackendError>> + Send;

    fn delete_label(&self, id: &str) -> impl Future<Output = Result<(), BackendError>> + Send;

    fn set_label_color(
        &self,
        id: &str,
        color: &LabelColor,
    ) -> impl Future<Output = Result<RemoteLabel, BackendError>> + Send;

    /// How many conversations carry the label in the whole mailbox, not
    /// only in the part this computer keeps.
    fn label_threads(&self, id: &str) -> impl Future<Output = Result<u64, BackendError>> + Send;
}

/// The account's calendar. Every call answers
/// `BackendError::NeedsPermission` until the account grants the calendar
/// permission.
pub trait CalendarService: Send + Sync + 'static {
    /// Answers the event `ical_uid` names as `me`, and lets the server
    /// tell the organizer. `occurrence` is the start of the one occurrence
    /// to answer; `None` answers the series.
    fn answer_invitation(
        &self,
        ical_uid: &str,
        me: &str,
        answer: Answer,
        occurrence: Option<EpochMillis>,
    ) -> impl Future<Output = Result<Answered, BackendError>> + Send;

    /// What the calendar already holds between `from` and `to`.
    fn busy_between(
        &self,
        from: EpochMillis,
        to: EpochMillis,
    ) -> impl Future<Output = Result<Vec<Busy>, BackendError>> + Send;

    /// How the repeating event `ical_uid` names repeats, with what is left
    /// of it from `from`. `None` when there is no such event or it does
    /// not repeat.
    fn series(
        &self,
        ical_uid: &str,
        from: EpochMillis,
    ) -> impl Future<Output = Result<Option<Series>, BackendError>> + Send;

    /// Every event on the primary calendar that overlaps `from` to `to`,
    /// in the order they start.
    fn events_between(
        &self,
        from: EpochMillis,
        to: EpochMillis,
    ) -> impl Future<Output = Result<Vec<Event>, BackendError>> + Send;

    /// Puts a new event on the primary calendar and invites its guests.
    fn create_event(
        &self,
        fields: &EventFields,
    ) -> impl Future<Output = Result<Event, BackendError>> + Send;

    /// Changes what `fields` sets on event `id` and tells its guests.
    fn update_event(
        &self,
        id: &str,
        fields: &EventFields,
    ) -> impl Future<Output = Result<Event, BackendError>> + Send;

    /// Takes event `id` off the primary calendar and tells its guests.
    fn delete_event(&self, id: &str) -> impl Future<Output = Result<(), BackendError>> + Send;
}

/// The account's address book.
pub trait ContactsService: Send + Sync + 'static {
    /// One page of the contacts. `sync_token` from the last read asks for
    /// changes alone. Answers `BackendError::NeedsPermission` until the
    /// account grants the contacts permission.
    fn connections(
        &self,
        page_token: Option<&str>,
        sync_token: Option<&str>,
    ) -> impl Future<Output = Result<ConnectionsPage, BackendError>> + Send;

    /// The bytes of one contact photo.
    fn contact_photo(
        &self,
        url: &str,
    ) -> impl Future<Output = Result<Vec<u8>, BackendError>> + Send;

    /// Adds a contact. Answers `BackendError::NeedsPermission` until the
    /// account grants the permission to change contacts.
    fn create_contact(
        &self,
        fields: &ContactFields,
    ) -> impl Future<Output = Result<Person, BackendError>> + Send;

    /// Changes the fields `fields` names on the contact `resource`.
    fn update_contact(
        &self,
        resource: &str,
        fields: &ContactFields,
    ) -> impl Future<Output = Result<Person, BackendError>> + Send;
}

/// The rules the server runs on arriving mail.
pub trait RulesService: Send + Sync + 'static {
    fn filters(&self) -> impl Future<Output = Result<Vec<Filter>, BackendError>> + Send;

    fn create_filter(
        &self,
        filter: &Filter,
    ) -> impl Future<Output = Result<Filter, BackendError>> + Send;

    fn delete_filter(&self, id: &str) -> impl Future<Output = Result<(), BackendError>> + Send;
}

/// The automatic reply the server sends while the person is away.
pub trait AutoReplyService: Send + Sync + 'static {
    fn vacation(&self) -> impl Future<Output = Result<Vacation, BackendError>> + Send;

    fn set_vacation(
        &self,
        vacation: &Vacation,
    ) -> impl Future<Output = Result<(), BackendError>> + Send;
}

/// The addresses the account sends as.
pub trait IdentityService: Send + Sync + 'static {
    /// Every address the account may send from, its own included, each
    /// with its display name and signature. An address the server would
    /// refuse to send from is left out.
    fn identities(&self) -> impl Future<Output = Result<Vec<SendAsAddress>, BackendError>> + Send;
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use mailrs_domain::Filter;

    use super::*;
    use crate::fake::FakeGmail;

    #[test]
    fn a_google_account_has_every_service() {
        let services = AccountServices::fake(Arc::new(FakeGmail::new()));
        assert!(services.calendar.is_some());
        assert!(services.contacts.is_some());
        assert!(services.auto_reply.is_some());
        assert_eq!(
            services.capabilities(),
            MailCapabilities {
                labels: true,
                server_threads: true,
                files_sent_mail: true,
                categories: true,
                delete_forever: true,
            }
        );
    }

    #[tokio::test]
    async fn every_service_reaches_the_one_mailbox() {
        let gmail = Arc::new(FakeGmail::new());
        let services = AccountServices::fake(Arc::clone(&gmail));
        services
            .rules
            .create_filter(&Filter::block("pest@example.com"))
            .await
            .unwrap();
        assert_eq!(gmail.with(|s| s.filters.len()), 1);
        let labels = services.mail.labels().await.unwrap();
        assert!(labels.iter().any(|l| l.id == "INBOX"));
        let identities = services.identities.identities().await.unwrap();
        assert_eq!(identities[0].email, "me@example.com");
        let calendar = services.calendar.as_ref().expect("Gmail has a calendar");
        assert!(calendar.events_between(0, 1).await.unwrap().is_empty());
    }
}
