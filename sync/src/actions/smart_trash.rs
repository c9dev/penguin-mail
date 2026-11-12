//! Sent messages that would keep a trashed conversation in a smart mailbox.
use super::*;
use mailrs_domain::SmartMailbox;
use mailrs_domain::query::{Query, Term};

impl<A: Accounts> MailActions<A> {
    /// Reads only stored mail. Folder-account smart listings use the same store query.
    /// The returned message targets bound consent to the messages seen before asking.
    pub async fn sent_in_smart(
        &self,
        targets: &[Target],
        smart: &SmartMailbox,
    ) -> Result<Vec<Target>, SyncError> {
        let targets: Vec<Target> = targets
            .iter()
            .filter(|t| {
                t.message_id.is_none()
                    && self
                        .accounts
                        .services(t.account_id)
                        .is_some_and(|s| !s.capabilities().labels)
            })
            .cloned()
            .collect();
        let smart = smart.clone();
        Ok(self
            .db
            .read(move |c| {
                let accounts = mailrs_store::accounts::list_accounts(c)?;
                let now = chrono::Local::now();
                let mut sent = Vec::new();
                for target in targets {
                    let Some(account) = accounts.iter().find(|a| a.id == target.account_id) else {
                        continue;
                    };
                    let Some(query) = smart.query_for(&account.email) else {
                        continue;
                    };
                    let query = Query::And(vec![
                        query,
                        Query::Term(Term::In(MailSet::Role(Role::Sent))),
                    ]);
                    for hit in mailrs_store::query::matching_thread(
                        c,
                        target.account_id,
                        &target.thread_id,
                        &query,
                        &now,
                    )? {
                        let message = Target {
                            account_id: target.account_id,
                            thread_id: hit.thread_id,
                            message_id: Some(hit.message_id),
                        };
                        if !sent.contains(&message) {
                            sent.push(message);
                        }
                    }
                }
                Ok(sent)
            })
            .await?)
    }
}
