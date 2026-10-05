//! One POP3 check (spec section 3): sign in, list what the server holds,
//! download what is new a message at a time, ask for the DELEs the
//! account's setting wants, and sign off. Each message's bytes are written
//! before the next is asked for, and the server's list is compared with
//! the store a page at a time.

use std::collections::{BTreeSet, HashMap, HashSet};

use mailrs_domain::{ChangeEvent, RemoveSetting};
use mailrs_pop3::{MOST_MESSAGE_BYTES, Pop3Api, Pop3Error, Uidl, UidlListing};
use mailrs_store::pop3::FailReason;
use mailrs_store::{accounts, pop3};

use super::AccountSync;
use crate::services::pop3::{keep_local, local_meta};
use crate::{AnyMail, BackendError, SyncError, now_millis};

const DAY: i64 = 24 * 60 * 60 * 1000;

/// What a check did before its QUIT.
#[derive(Default)]
struct Done {
    /// Every UIDL the server listed, once the listing came back whole and
    /// every line of it read. Empty otherwise, and nothing is forgotten.
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
        let menu_changed = self
            .db
            .write(move |c| {
                pop3::mark_removed(c, account_id, &removed)?;
                // A server listing nothing may have lost its list for a
                // moment, so the rows wait for a listing that names
                // something before any is forgotten.
                if listed.is_empty() {
                    return Ok(false);
                }
                pop3::forget_gone(c, account_id, &listed)?;
                pop3::forget_gone_failures(c, account_id, &listed)
            })
            .await?;
        if menu_changed {
            self.emit(ChangeEvent::LabelsChanged { account_id });
        }
        Ok(())
    }

    async fn check_session<P: Pop3Api>(
        &self,
        pop3: &P,
        remove: RemoveSetting,
        done: &mut Done,
    ) -> Result<(), SyncError> {
        let account_id = self.account_id;
        // Decided by the marker, not by whether anything is downloaded: a
        // first download that failed partway leaves mail here that is still
        // old mail.
        let first = !self
            .db
            .read(move |c| pop3::first_check_finished(c, account_id))
            .await?;
        // The UIDLs that failed during this check, so a session opened
        // after a broken answer neither counts nor asks for them twice. A
        // download needs no entry: the store already has it. Each new
        // session follows a failure added here, so the loop ends.
        let mut handled: HashSet<String> = HashSet::new();
        let listing = loop {
            if let Some(listing) = self
                .download_pass(pop3, first, remove, &mut handled, done)
                .await?
            {
                break listing;
            }
            pop3.connect().await.map_err(BackendError::from)?;
        };
        let UidlListing { messages: listed, unreadable } = listing;
        if let RemoveSetting::Days(days) = remove {
            let cutoff = now_millis() - i64::from(days) * DAY;
            self.db
                .write(move |c| pop3::want_removed_before(c, account_id, cutoff))
                .await?;
        }
        // Leave on Server sends no DELE, even for a row an earlier setting
        // marked.
        if remove != RemoveSetting::Never {
            // The store commits without syncing, and a DELE lets the server
            // drop the other copy at QUIT. A power cut after that QUIT and
            // before SQLite's own checkpoint would lose the message from
            // both, so the downloads go to disk first. When a reader holds
            // the log back, the DELEs wait for the next check.
            if self.db.checkpoint().await? {
                self.send_deles(pop3, &listed, done).await?;
            } else {
                tracing::warn!(account = account_id, "the downloads are not on disk yet; removal from the server waits for the next check");
            }
        }
        // A line that did not read left its message out of the listing,
        // and forgetting what the listing lacks would forget that one too.
        if unreadable == 0 {
            done.listed = listed.into_iter().map(|u| u.uidl).collect();
        } else {
            tracing::warn!(account = account_id, unreadable, "the UIDL answer had lines that do not read; nothing is forgotten this check");
        }
        Ok(())
    }

    /// One pass over what the open session lists: every message not yet
    /// here and not handled this check, those that never failed first.
    /// Answers the listing, or `None` when a broken answer ended the
    /// session and another must carry on.
    async fn download_pass<P: Pop3Api>(
        &self,
        pop3: &P,
        first: bool,
        remove: RemoveSetting,
        handled: &mut HashSet<String>,
        done: &mut Done,
    ) -> Result<Option<UidlListing>, SyncError> {
        let account_id = self.account_id;
        let listing = pop3.uidl().await.map_err(BackendError::from)?;
        let listed = &listing.messages;
        let sizes: HashMap<u32, u64> = pop3
            .list()
            .await
            .map_err(BackendError::from)?
            .into_iter()
            .map(|item| (item.id, item.octets))
            .collect();
        // A server should never list one UIDL twice in a session, but a
        // buggy one can. The first listing downloads and the rest are
        // skipped, so neither body replaces the other.
        let mut taken: HashSet<&str> = HashSet::new();
        // A message that failed before waits until the rest are down, so
        // one that fails at every check never holds up the mail listed
        // after it.
        let mut retry: Vec<(&Uidl, FailReason)> = Vec::new();
        for page in listed.chunks(pop3::PAGE) {
            let names: Vec<String> = page
                .iter()
                .filter(|u| !handled.contains(&u.uidl))
                .map(|u| u.uidl.clone())
                .collect();
            let (new, failed) = self
                .db
                .read(move |c| {
                    let new = pop3::unseen(c, account_id, &names)?;
                    let failed = pop3::failure_reasons(c, account_id, &new)?;
                    Ok((new, failed))
                })
                .await?;
            let new: HashSet<String> = new.into_iter().collect();
            let failed: HashMap<String, FailReason> = failed.into_iter().collect();
            for listing in page.iter().filter(|u| new.contains(&u.uidl)) {
                if !taken.insert(&listing.uidl) {
                    tracing::warn!(account = account_id, uidl = listing.uidl, message = listing.id, "the server listed this UIDL twice; only its first message downloads");
                    continue;
                }
                if let Some(reason) = failed.get(&listing.uidl) {
                    retry.push((listing, *reason));
                    continue;
                }
                let size = sizes.get(&listing.id).copied();
                if !self
                    .download_one(pop3, listing, size, None, first, remove, handled, done)
                    .await?
                {
                    return Ok(None);
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
        for (listing, reason) in retry {
            let size = sizes.get(&listing.id).copied();
            if !self
                .download_one(pop3, listing, size, Some(reason), first, remove, handled, done)
                .await?
            {
                return Ok(None);
            }
        }
        Ok(Some(listing))
    }

    /// Downloads one listed message of `size` octets, or counts why it did
    /// not come down. `earlier` is why it failed last time. False when the
    /// answer left the session unreadable and the check must open another.
    #[expect(clippy::too_many_arguments, reason = "one step of the pass, which holds all of these")]
    async fn download_one<P: Pop3Api>(
        &self,
        pop3: &P,
        listing: &Uidl,
        size: Option<u64>,
        earlier: Option<FailReason>,
        first: bool,
        remove: RemoveSetting,
        handled: &mut HashSet<String>,
        done: &mut Done,
    ) -> Result<bool, SyncError> {
        let Uidl { id, uidl } = listing;
        let account_id = self.account_id;
        // A message over the cap is never asked for, and neither is one
        // whose answer ran past the cap before: reading it again would
        // fetch up to the cap and fail at the same place.
        if size.is_some_and(|octets| octets > MOST_MESSAGE_BYTES)
            || earlier == Some(FailReason::TooLarge)
        {
            handled.insert(uidl.clone());
            done.failing_grew |= self
                .count_failure(pop3, *id, uidl, &Pop3Error::TooLarge, true)
                .await?;
            return Ok(true);
        }
        let answer = pop3.retr(*id, size.unwrap_or(0)).await;
        if answer.is_err() {
            handled.insert(uidl.clone());
        }
        match answer {
            Ok(raw) => self.keep_download(uidl, raw, first, remove, done).await?,
            // One message the server will not hand over holds up nothing
            // else; the next check asks for it again.
            Err(err @ Pop3Error::Refused(_)) => {
                tracing::warn!(account = account_id, uidl, %err, "the server would not hand over a message");
                done.failing_grew |= self.count_failure(pop3, *id, uidl, &err, true).await?;
            }
            // An answer past the cap that LIST put under it, or one that is
            // not POP3, leaves the rest of it unread, and the client has
            // dropped the session. The message is counted and another
            // session carries on with the rest.
            Err(err @ (Pop3Error::TooLarge | Pop3Error::Protocol(_))) => {
                tracing::warn!(account = account_id, uidl, %err, "could not read a message's answer");
                done.failing_grew |= self.count_failure(pop3, *id, uidl, &err, false).await?;
                return Ok(false);
            }
            // A connection that drops ends the check. Counting the message
            // in flight puts it after the rest at the next check, in case
            // it is what the server drops on.
            Err(err @ Pop3Error::Network(_)) => {
                done.failing_grew |= self.count_failure(pop3, *id, uidl, &err, false).await?;
                return Err(BackendError::from(err).into());
            }
            Err(err) => return Err(BackendError::from(err).into()),
        }
        Ok(true)
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
        // A first download takes each message's own date, which stands
        // for the INTERNALDATE a server would keep. Later mail is dated
        // when it arrived here, so a message whose Date header is older
        // than the rules' watermark still runs through them. The store id
        // is chosen in the write below, so the row gets it there.
        let (mut meta, links) = local_meta(account_id, "", &raw, "inbox", &[], received, first);
        let uidl = uidl.to_string();
        let (id, threads) = self
            .db
            .write(move |c| {
                // Never `pop3/<uidl>` blindly: that id may belong to an
                // older message the server once gave the same UIDL.
                let id = pop3::download_id(c, account_id, &uidl)?;
                meta.id.clone_from(&id);
                meta.thread_id.clone_from(&id);
                let threads = keep_local(c, account_id, meta, links, &raw)?;
                pop3::mark_downloaded(c, account_id, &uidl, &id, received)?;
                if remove == RemoveSetting::Downloaded {
                    pop3::want_removed(c, account_id, std::slice::from_ref(&uidl))?;
                }
                Ok((id, threads))
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

    /// Counts a failed RETR of message `id`, called `uidl`, and why. True
    /// when the message just reached the account's menu. One the server
    /// stopped listing and lists again counts from nothing and reaches the
    /// menu again (`pop3::forget_gone_failures`). With the session
    /// still open, that third failure reads the message's headers with
    /// `TOP n 0`, so the menu can say who sent it and what it is about.
    async fn count_failure<P: Pop3Api>(
        &self,
        pop3: &P,
        id: u32,
        uidl: &str,
        err: &Pop3Error,
        session_open: bool,
    ) -> Result<bool, SyncError> {
        let (reason, words) = match err {
            Pop3Error::Refused(text) => (FailReason::Refused, text.clone()),
            Pop3Error::TooLarge => (FailReason::TooLarge, String::new()),
            Pop3Error::Network(_) => (FailReason::Dropped, String::new()),
            _ => (FailReason::Unreadable, String::new()),
        };
        let (account_id, uidl) = (self.account_id, uidl.to_string());
        let named = uidl.clone();
        let failures = self
            .db
            .write(move |c| pop3::record_failure(c, account_id, &uidl, reason, &words))
            .await?;
        let shown = failures == pop3::SHOWN_AFTER;
        if shown && session_open {
            // The headers only name the message; a server that cannot
            // answer TOP leaves the menu to number it.
            match pop3.top(id, 0).await {
                Ok(head) => {
                    let summary = mailrs_mime::summary(&head);
                    let sender = summary.from.map(|from| from.display().to_string());
                    let subject = Some(summary.subject).filter(|s| !s.is_empty());
                    self.db
                        .write(move |c| {
                            pop3::name_failure(c, account_id, &named, sender.as_deref(), subject.as_deref())
                        })
                        .await?;
                }
                Err(err) => tracing::info!(account = account_id, uidl = named, %err, "could not read the failing message's headers"),
            }
        }
        Ok(shown)
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
        // A UIDL listed twice maps to its first number, the message that
        // came down; the skipped one stays on the server.
        let mut numbers: HashMap<&str, u32> = HashMap::with_capacity(listed.len());
        for Uidl { id, uidl } in listed {
            numbers.entry(uidl.as_str()).or_insert(*id);
        }
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
