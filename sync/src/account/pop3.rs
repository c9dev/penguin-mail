//! One POP3 check (spec section 3): sign in, list what the server holds,
//! download what is new a message at a time, ask for the DELEs the
//! account's setting wants, and sign off. Each message's bytes are written
//! before the next is asked for, and the server's list is compared with
//! the store a page at a time.

use std::collections::{BTreeSet, HashMap, HashSet};

use mailrs_domain::translate::gettext;
use mailrs_domain::{ChangeEvent, RemoveSetting};
use mailrs_pop3::{MOST_MESSAGE_BYTES, Pop3Api, Pop3Error, Uidl};
use mailrs_store::{accounts, pop3};

use super::AccountSync;
use crate::services::pop3::{downloaded_id, keep_local, local_meta};
use crate::{AnyMail, BackendError, SyncError, now_millis};

const DAY: i64 = 24 * 60 * 60 * 1000;

/// What a check did before its QUIT.
#[derive(Default)]
struct Done {
    /// Every UIDL the server listed, once the listing came back whole.
    listed: HashSet<String>,
    /// The UIDLs a DELE went out for this session.
    removed: Vec<String>,
    threads: BTreeSet<String>,
    new_mail: Vec<String>,
    /// A message reached its third refused RETR.
    failing_grew: bool,
}

impl AccountSync {
    /// One check of a POP3 account's server, for the engine's tick. Any
    /// other account has nothing to check.
    pub async fn pop3_check(&self) -> Result<(), SyncError> {
        match &self.services.mail {
            AnyMail::Pop3(adapter) => self.check_with(adapter.client().as_ref()).await,
            #[cfg(any(test, feature = "fake"))]
            AnyMail::FakePop3(adapter) => self.check_with(adapter.client().as_ref()).await,
            _ => Ok(()),
        }
    }

    /// The check over `pop3`, one at a time for this account.
    pub(crate) async fn check_with<P: Pop3Api>(&self, pop3: &P) -> Result<(), SyncError> {
        let _one = self.pop3_checking.lock().await;
        let account_id = self.account_id;
        // Read at each check: Server Settings for the account can change
        // it between checks.
        let remove = self
            .db
            .read(move |c| accounts::pop3_remove(c, account_id))
            .await?;
        pop3.connect().await.map_err(BackendError::from)?;
        let mut done = Done::default();
        let checked = self.check_session(pop3, remove, &mut done).await;
        // After a failure, QUIT still lets go of the server's lock. A DELE
        // it carries out was one the account wanted; that row waits, and
        // goes once the server stops listing the message.
        let quit = pop3.quit().await;
        let Done {
            listed,
            removed,
            threads,
            new_mail,
            failing_grew,
        } = done;
        // What was stored stays stored whatever went wrong after it, so
        // the window and the local rules hear of it either way.
        self.emit_threads(threads);
        if !new_mail.is_empty() {
            self.emit(ChangeEvent::NewMail {
                account_id,
                message_ids: new_mail,
            });
        }
        if failing_grew {
            self.emit(ChangeEvent::LabelsChanged { account_id });
        }
        checked?;
        // A QUIT that did not come back carried out no DELE; the rows still
        // want removal, and the next check sends them again.
        quit.map_err(BackendError::from)?;
        self.db
            .write(move |c| {
                pop3::mark_removed(c, account_id, &removed)?;
                // A server listing nothing may have lost its list for a
                // moment, so the rows wait for a listing that names
                // something before any is forgotten.
                if !listed.is_empty() {
                    pop3::forget_gone(c, account_id, &listed)?;
                }
                Ok(())
            })
            .await?;
        Ok(())
    }

    async fn check_session<P: Pop3Api>(
        &self,
        pop3: &P,
        remove: RemoveSetting,
        done: &mut Done,
    ) -> Result<(), SyncError> {
        let account_id = self.account_id;
        let listed = pop3.uidl().await.map_err(BackendError::from)?;
        let sizes: HashMap<u32, u64> = pop3
            .list()
            .await
            .map_err(BackendError::from)?
            .into_iter()
            .map(|item| (item.id, item.octets))
            .collect();
        // Decided by the marker, not by whether anything is downloaded: a
        // first download that failed partway leaves mail here that is still
        // old mail.
        let first = !self
            .db
            .read(move |c| pop3::first_check_finished(c, account_id))
            .await?;
        for page in listed.chunks(pop3::PAGE) {
            let names: Vec<String> = page.iter().map(|u| u.uidl.clone()).collect();
            let new: HashSet<String> = self
                .db
                .read(move |c| pop3::unseen(c, account_id, &names))
                .await?
                .into_iter()
                .collect();
            for Uidl { id, uidl } in page.iter().filter(|u| new.contains(&u.uidl)) {
                // A message over the cap is never asked for.
                if sizes
                    .get(id)
                    .is_some_and(|octets| *octets > MOST_MESSAGE_BYTES)
                {
                    done.failing_grew |= self.count_failure(uidl, &Pop3Error::TooLarge).await?;
                    continue;
                }
                match pop3.retr(*id).await {
                    Ok(raw) => self.keep_download(uidl, raw, first, remove, done).await?,
                    // One message the server will not hand over holds up
                    // nothing else; the next check asks for it again.
                    Err(err @ Pop3Error::Refused(_)) => {
                        tracing::warn!(account = account_id, uidl, %err, "the server would not hand over a message");
                        done.failing_grew |= self.count_failure(uidl, &err).await?;
                    }
                    // Anything else leaves the session unusable.
                    Err(err) => return Err(BackendError::from(err).into()),
                }
            }
        }
        // Every listed message is downloaded or recorded as failed. A
        // check that ended early never reaches this line, so the next one
        // is still a first check and takes the old mail as old.
        if first {
            self.db
                .write(move |c| pop3::finish_first_check(c, account_id))
                .await?;
        }
        if let RemoveSetting::Days(days) = remove {
            let cutoff = now_millis() - i64::from(days) * DAY;
            self.db
                .write(move |c| pop3::want_removed_before(c, account_id, cutoff))
                .await?;
        }
        // Leave on Server sends no DELE, even for a row an earlier setting
        // marked.
        if remove != RemoveSetting::Never {
            self.send_deles(pop3, &listed, done).await?;
        }
        done.listed = listed.into_iter().map(|u| u.uidl).collect();
        Ok(())
    }

    /// Stores one download. The bytes, the row and the UIDL go in one
    /// write, so a crash between them leaves nothing half kept and the
    /// next check fetches the message again. `raw` is dropped once the
    /// write is done, before the next RETR.
    async fn keep_download(
        &self,
        uidl: &str,
        raw: Vec<u8>,
        first: bool,
        remove: RemoveSetting,
        done: &mut Done,
    ) -> Result<(), SyncError> {
        let account_id = self.account_id;
        let received = now_millis();
        let id = downloaded_id(uidl);
        // A first download takes each message's own date, which stands
        // for the INTERNALDATE a server would keep. Later mail is dated
        // when it arrived here, so a message whose Date header is older
        // than the rules' watermark still runs through them.
        let (meta, links) = local_meta(account_id, &id, &raw, "inbox", &[], received, first);
        let uidl = uidl.to_string();
        let threads = self
            .db
            .write(move |c| {
                let threads = keep_local(c, account_id, meta, links, &raw)?;
                pop3::mark_downloaded(c, account_id, &uidl, received)?;
                if remove == RemoveSetting::Downloaded {
                    pop3::want_removed(c, account_id, std::slice::from_ref(&uidl))?;
                }
                Ok(threads)
            })
            .await?;
        done.threads.extend(threads);
        // A first download would raise a notification for every message
        // the server ever held.
        if !first {
            done.new_mail.push(id);
        }
        Ok(())
    }

    /// Counts a refused RETR with the server's words. True when the
    /// message just reached the account's menu.
    async fn count_failure(&self, uidl: &str, err: &Pop3Error) -> Result<bool, SyncError> {
        let words = match err {
            Pop3Error::Refused(text) => text.clone(),
            Pop3Error::TooLarge => gettext("The message is larger than Penguin Mail downloads."),
            other => other.to_string(),
        };
        let (account_id, uidl) = (self.account_id, uidl.to_string());
        let failures = self
            .db
            .write(move |c| pop3::record_failure(c, account_id, &uidl, &words))
            .await?;
        Ok(failures == pop3::SHOWN_AFTER)
    }

    /// A DELE for each message the account wants off the server that the
    /// server still lists, a page of rows at a time, noting each in
    /// `done.removed` for a clean QUIT to confirm. A DELE the server
    /// refuses goes again at the next check.
    async fn send_deles<P: Pop3Api>(
        &self,
        pop3: &P,
        listed: &[Uidl],
        done: &mut Done,
    ) -> Result<(), SyncError> {
        let numbers: HashMap<&str, u32> = listed.iter().map(|u| (u.uidl.as_str(), u.id)).collect();
        let account_id = self.account_id;
        let mut after: Option<String> = None;
        loop {
            let from = after.take();
            let page = self
                .db
                .read(move |c| pop3::pending_removal(c, account_id, from.as_deref(), pop3::PAGE))
                .await?;
            let Some(last) = page.last().cloned() else {
                return Ok(());
            };
            for uidl in page {
                let Some(id) = numbers.get(uidl.as_str()) else {
                    continue;
                };
                match pop3.dele(*id).await {
                    Ok(()) => done.removed.push(uidl),
                    Err(Pop3Error::Refused(_)) => {}
                    Err(err) => return Err(BackendError::from(err).into()),
                }
            }
            after = Some(last);
        }
    }
}
