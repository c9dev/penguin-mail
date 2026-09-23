//! The Google adapter: every service a Google account offers, over one
//! Gmail client. `G` is that client, the real `AccountClient` or
//! `FakeGmail` for tests and the demo. Gmail's errors become backend
//! errors here, and Gmail's quota and pacing show nowhere else in the
//! services.

mod changes;
mod writes;

use std::sync::Arc;
use std::time::Duration;

use mailrs_domain::invitation::Answer;
use mailrs_domain::{
    EpochMillis, Filter, MailboxKind, MessageBody, MessageMeta, RemoteMailbox, Role, Vacation,
    gmail,
};
use mailrs_gmail::{
    Answered, Busy, ConnectionsPage, ContactFields, Event, EventFields, LabelColor, MessagePage,
    Person, RemoteLabel, SendAs, Series, html_to_text, limiter,
};

use super::{
    AutoReplyService, CalendarService, Changes, ContactsService, IdentityService, MailBackend,
    MailCapabilities, Priority, RulesService, SendAsAddress, SyncState, Unapplied, priority,
};
use crate::api::{DraftRef, GmailApi, SavedDraft};
use crate::{BackendError, MailOp};

/// A Google account's services, all over the one client `G`, which spends
/// one quota bucket for all of them.
pub struct Google<G> {
    gmail: Arc<G>,
}

impl<G> Google<G> {
    pub fn new(gmail: Arc<G>) -> Self {
        Google { gmail }
    }
}

// By hand, since a derive would ask for `G: Clone` and the client is
// shared rather than copied.
impl<G> Clone for Google<G> {
    fn clone(&self) -> Self {
        Google {
            gmail: Arc::clone(&self.gmail),
        }
    }
}

/// Runs one Gmail call at the priority of the work around it. Gmail's
/// quota bucket reads a task-local of its own, so background work is
/// marked there too.
async fn paced<T>(call: impl Future<Output = T>) -> T {
    match priority() {
        Priority::Background => limiter::background(call).await,
        Priority::Foreground => call.await,
    }
}

impl<G: GmailApi> MailBackend for Google<G> {
    fn capabilities(&self) -> MailCapabilities {
        MailCapabilities {
            labels: true,
            server_threads: true,
            files_sent_mail: true,
            categories: true,
            delete_forever: true,
            batch_limit: mailrs_gmail::BATCH_LIMIT,
        }
    }

    fn mailbox_for(&self, role: Role) -> Option<String> {
        gmail::label_of_role(role).map(str::to_string)
    }

    async fn apply(&self, messages: &[String], ops: &[MailOp]) -> Result<(), Unapplied> {
        self.write(messages, ops).await
    }

    fn person_waiting(&self) -> bool {
        self.gmail
            .quota()
            .is_some_and(|quota| quota.foreground_waiting())
    }

    async fn stand_by(&self, wait: Duration) {
        // The guard counts the whole wait as the person's, so backfill
        // stands aside for all of it and not only while the retry asks
        // the bucket.
        let _waiting = self.gmail.quota().map(|quota| quota.waiting());
        tokio::time::sleep(wait).await;
    }

    async fn list_messages(
        &self,
        query: &str,
        page_token: Option<&str>,
        page_size: u32,
    ) -> Result<MessagePage, BackendError> {
        Ok(paced(self.gmail.list_messages(query, page_token, page_size)).await?)
    }

    async fn list_labelled(
        &self,
        label_id: &str,
        query: &str,
        page_token: Option<&str>,
        page_size: u32,
    ) -> Result<MessagePage, BackendError> {
        Ok(paced(
            self.gmail
                .list_labelled(label_id, query, page_token, page_size),
        )
        .await?)
    }

    async fn message_metadata(&self, id: &str) -> Result<MessageMeta, BackendError> {
        Ok(paced(self.gmail.message_metadata(id)).await?)
    }

    async fn thread_metadata(&self, thread_id: &str) -> Result<Vec<MessageMeta>, BackendError> {
        Ok(paced(self.gmail.thread_metadata(thread_id)).await?)
    }

    async fn message_body(&self, id: &str) -> Result<MessageBody, BackendError> {
        Ok(paced(self.gmail.message_body(id)).await?)
    }

    async fn send(&self, raw: &[u8], thread_id: Option<&str>) -> Result<String, BackendError> {
        Ok(paced(self.gmail.send(raw, thread_id)).await?)
    }

    async fn save_draft(
        &self,
        draft_id: Option<&str>,
        raw: &[u8],
        thread_id: Option<&str>,
    ) -> Result<SavedDraft, BackendError> {
        Ok(paced(self.gmail.save_draft(draft_id, raw, thread_id)).await?)
    }

    async fn send_draft(&self, draft_id: &str) -> Result<String, BackendError> {
        Ok(paced(self.gmail.send_draft(draft_id)).await?)
    }

    async fn delete_draft(&self, draft_id: &str) -> Result<(), BackendError> {
        Ok(paced(self.gmail.delete_draft(draft_id)).await?)
    }

    async fn list_drafts(&self) -> Result<Vec<DraftRef>, BackendError> {
        Ok(paced(self.gmail.list_drafts()).await?)
    }

    async fn attachment(
        &self,
        message_id: &str,
        attachment_id: &str,
    ) -> Result<Vec<u8>, BackendError> {
        Ok(paced(self.gmail.attachment(message_id, attachment_id)).await?)
    }

    async fn raw_message(&self, id: &str) -> Result<Vec<u8>, BackendError> {
        Ok(paced(self.gmail.raw_message(id)).await?)
    }

    fn made_by_person(&self, id: &str) -> bool {
        gmail::kind_of(id) == MailboxKind::Label
    }

    async fn mailboxes(&self) -> Result<Vec<RemoteMailbox>, BackendError> {
        Ok(paced(self.gmail.labels())
            .await?
            .into_iter()
            .map(remote_mailbox)
            .collect())
    }

    async fn changes(&self, since: Option<&SyncState>) -> Result<Changes, BackendError> {
        self.history_changes(since).await
    }

    async fn create_mailbox(&self, name: &str) -> Result<RemoteMailbox, BackendError> {
        Ok(remote_mailbox(paced(self.gmail.create_label(name)).await?))
    }

    async fn rename_mailbox(&self, id: &str, name: &str) -> Result<RemoteMailbox, BackendError> {
        Ok(remote_mailbox(
            paced(self.gmail.rename_label(id, name)).await?,
        ))
    }

    async fn delete_mailbox(&self, id: &str) -> Result<(), BackendError> {
        Ok(paced(self.gmail.delete_label(id)).await?)
    }

    async fn set_mailbox_color(
        &self,
        id: &str,
        color: &LabelColor,
    ) -> Result<RemoteMailbox, BackendError> {
        Ok(remote_mailbox(
            paced(self.gmail.set_label_color(id, color)).await?,
        ))
    }

    async fn mailbox_threads(&self, id: &str) -> Result<u64, BackendError> {
        Ok(paced(self.gmail.label_threads(id)).await?)
    }
}

/// A Gmail label as a server mailbox. Gmail lists its keyword and category
/// labels too (`STARRED`, `UNREAD`, `CATEGORY_SOCIAL`); they arrive as
/// system mailboxes without a role, which the store keeps for the label
/// list and never files mail under. Gmail's choice to hide a label from
/// its own list is not read yet, so `hidden` is false.
fn remote_mailbox(label: RemoteLabel) -> RemoteMailbox {
    RemoteMailbox {
        role: gmail::role_of(&label.id),
        kind: match label.kind.as_deref() {
            Some("system") => MailboxKind::System,
            _ => MailboxKind::Label,
        },
        color: label.color.map(|c| c.background_color),
        hidden: false,
        id: label.id,
        name: label.name,
    }
}

impl<G: GmailApi> CalendarService for Google<G> {
    async fn answer_invitation(
        &self,
        ical_uid: &str,
        me: &str,
        answer: Answer,
        occurrence: Option<EpochMillis>,
    ) -> Result<Answered, BackendError> {
        Ok(paced(
            self.gmail
                .answer_invitation(ical_uid, me, answer, occurrence),
        )
        .await?)
    }

    async fn busy_between(
        &self,
        from: EpochMillis,
        to: EpochMillis,
    ) -> Result<Vec<Busy>, BackendError> {
        Ok(paced(self.gmail.busy_between(from, to)).await?)
    }

    async fn series(
        &self,
        ical_uid: &str,
        from: EpochMillis,
    ) -> Result<Option<Series>, BackendError> {
        Ok(paced(self.gmail.series(ical_uid, from)).await?)
    }

    async fn events_between(
        &self,
        from: EpochMillis,
        to: EpochMillis,
    ) -> Result<Vec<Event>, BackendError> {
        Ok(paced(self.gmail.events_between(from, to)).await?)
    }

    async fn create_event(&self, fields: &EventFields) -> Result<Event, BackendError> {
        Ok(paced(self.gmail.create_event(fields)).await?)
    }

    async fn update_event(&self, id: &str, fields: &EventFields) -> Result<Event, BackendError> {
        Ok(paced(self.gmail.update_event(id, fields)).await?)
    }

    async fn delete_event(&self, id: &str) -> Result<(), BackendError> {
        Ok(paced(self.gmail.delete_event(id)).await?)
    }
}

impl<G: GmailApi> ContactsService for Google<G> {
    async fn connections(
        &self,
        page_token: Option<&str>,
        sync_token: Option<&str>,
    ) -> Result<ConnectionsPage, BackendError> {
        Ok(paced(self.gmail.connections(page_token, sync_token)).await?)
    }

    async fn contact_photo(&self, url: &str) -> Result<Vec<u8>, BackendError> {
        Ok(paced(self.gmail.contact_photo(url)).await?)
    }

    async fn create_contact(&self, fields: &ContactFields) -> Result<Person, BackendError> {
        Ok(paced(self.gmail.create_contact(fields)).await?)
    }

    async fn update_contact(
        &self,
        resource: &str,
        fields: &ContactFields,
    ) -> Result<Person, BackendError> {
        Ok(paced(self.gmail.update_contact(resource, fields)).await?)
    }
}

impl<G: GmailApi> RulesService for Google<G> {
    async fn filters(&self) -> Result<Vec<Filter>, BackendError> {
        Ok(paced(self.gmail.filters()).await?)
    }

    async fn create_filter(&self, filter: &Filter) -> Result<Filter, BackendError> {
        Ok(paced(self.gmail.create_filter(filter)).await?)
    }

    async fn delete_filter(&self, id: &str) -> Result<(), BackendError> {
        Ok(paced(self.gmail.delete_filter(id)).await?)
    }
}

impl<G: GmailApi> AutoReplyService for Google<G> {
    async fn vacation(&self) -> Result<Vacation, BackendError> {
        Ok(paced(self.gmail.vacation()).await?)
    }

    async fn set_vacation(&self, vacation: &Vacation) -> Result<(), BackendError> {
        Ok(paced(self.gmail.set_vacation(vacation)).await?)
    }
}

impl<G: GmailApi> IdentityService for Google<G> {
    /// Gmail's send-as list, one `sendAs.list` call. An alias still waiting
    /// on its owner to confirm it is left out, since Gmail would refuse to
    /// send from it. Signatures come as HTML and leave as text.
    async fn identities(&self) -> Result<Vec<SendAsAddress>, BackendError> {
        Ok(paced(self.gmail.send_as())
            .await?
            .into_iter()
            .filter(SendAs::is_verified)
            .map(|identity| SendAsAddress {
                name: Some(identity.display_name).filter(|n| !n.trim().is_empty()),
                email: identity.send_as_email,
                signature: html_to_text(&identity.signature),
                default: identity.is_default,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use mailrs_gmail::{AccountQuota, GmailError, SendAs, limiter};

    use super::{Google, paced};
    use crate::BackendError;
    use crate::fake::FakeGmail;
    use crate::services::{
        AutoReplyService, IdentityService, MailBackend, MailCapabilities, SendAsAddress, background,
    };

    fn google() -> (Arc<FakeGmail>, Google<FakeGmail>) {
        let gmail = Arc::new(FakeGmail::new());
        (Arc::clone(&gmail), Google::new(gmail))
    }

    #[tokio::test]
    async fn a_missing_message_is_only_missing() {
        let (_, google) = google();
        assert!(matches!(
            google.message_metadata("gone").await,
            Err(BackendError::NotFound)
        ));
    }

    #[tokio::test]
    async fn a_missing_permission_is_its_own_kind() {
        let (gmail, google) = google();
        gmail.fail_next(GmailError::MissingScope);
        assert!(matches!(
            google.vacation().await,
            Err(BackendError::NeedsPermission)
        ));
    }

    #[tokio::test]
    async fn identities_are_the_confirmed_addresses_with_their_signatures_as_text() {
        let (gmail, google) = google();
        gmail.with(|s| {
            s.signature = Some("Me\nExample Co".into());
            s.send_as.push(SendAs {
                send_as_email: "old@example.com".into(),
                display_name: "Old".into(),
                is_default: false,
                is_primary: false,
                signature: String::new(),
                verification_status: Some("pending".into()),
            });
        });
        assert_eq!(
            google.identities().await.unwrap(),
            vec![SendAsAddress {
                email: "me@example.com".into(),
                name: Some("Me".into()),
                signature: "Me\nExample Co".into(),
                default: true,
            }]
        );
    }

    #[test]
    fn gmail_has_labels_threads_categories_and_files_its_sent_mail() {
        let (_, google) = google();
        assert_eq!(
            google.capabilities(),
            MailCapabilities {
                labels: true,
                server_threads: true,
                files_sent_mail: true,
                categories: true,
                delete_forever: true,
                batch_limit: 1000,
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn standing_by_counts_as_the_person_waiting() {
        let gmail = FakeGmail::new().under_quota(Arc::new(AccountQuota::standalone()));
        let google = Google::new(Arc::new(gmail));
        assert!(!google.person_waiting());
        let ((), seen) = tokio::join!(google.stand_by(Duration::from_secs(2)), async {
            tokio::task::yield_now().await;
            google.person_waiting()
        });
        assert!(seen, "backfill sees the person waiting for the whole wait");
        assert!(!google.person_waiting());
    }

    #[tokio::test]
    async fn background_work_reaches_gmail_as_background() {
        let asked = || paced(async { limiter::priority() });
        assert_eq!(asked().await, mailrs_gmail::Priority::Foreground);
        assert_eq!(
            background(asked()).await,
            mailrs_gmail::Priority::Background
        );
    }
}
