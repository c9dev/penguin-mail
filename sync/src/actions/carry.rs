//! Carrying mail to another account, and the way back.
//!
//! No mail server can move a message it does not hold, so a carry is
//! three steps per message: read it out of the account that has it,
//! land a copy in the one the person dropped it on, and only once that
//! copy is safe take the original away. A carry that cannot land
//! changes nothing; a carry whose last step fails leaves the mail in
//! both places and says so, because a duplicate can be deleted and a
//! lost message cannot. Undo runs the same road backwards, out of the
//! account the copies arrived in and back into the one they started in,
//! dates and marks riding along.

use std::sync::atomic::Ordering;

use mailrs_domain::translate::{fill, gettext};
use mailrs_domain::{MailSet, Role, Target};

use super::{CrossReversal, Failure, History, MailActions, Outcome, Reversal, Undo};
use crate::account::carry::CopyOut;
use crate::mailbox::{Mailbox, Standard};
use crate::services::{Landing, MailBackend};
use crate::{AccountSync, Accounts, MailAction, MovedFrom, SyncError, TriageAction};

/// Runs a move to another account's mailbox. Rows of the destination
/// account's own take the ordinary label change, exactly as a drop
/// inside that account would; the rest are carried, one message at a
/// time. See the module docs for why the order of a carry is what it
/// is.
pub(super) async fn run<A: Accounts>(
    actions: &MailActions<A>,
    targets: &[Target],
    at: &Mailbox,
    history: History,
    from: &MovedFrom,
) -> Outcome {
    let action = MailAction::MoveTo {
        at: Box::new(at.clone()),
    };
    let words = action.describe();
    let refused = |targets: &[Target], reason: String| Outcome {
        done: Vec::new(),
        failed: targets
            .iter()
            .map(|target| Failure {
                target: target.clone(),
                error: reason.clone(),
            })
            .collect(),
        recorded: None,
    };
    let failed = |reason: String| {
        refused(
            targets,
            fill(
                &gettext("{action} failed: {reason}"),
                &[("action", &words), ("reason", &reason)],
            ),
        )
    };
    // The window only offers mailboxes one account owns; anything else
    // never reaches here, and reaches a person as a refusal if it does.
    let Some(account_id) = at.account() else {
        return refused(targets, gettext("Mail cannot go there"));
    };
    let Some(dest) = actions.accounts.account(account_id) else {
        return failed(SyncError::UnknownAccount(account_id).to_string());
    };
    // Where the copies land, in the destination's own terms.
    let place = match at {
        Mailbox::Label { label_id, .. } => label_id.clone(),
        Mailbox::Standard {
            which: Standard::Inbox,
            ..
        } => match actions
            .accounts
            .services(account_id)
            .and_then(|services| services.mail.mailbox_for(Role::Inbox))
        {
            Some(inbox) => inbox,
            None => return failed(gettext("That account has no Inbox.")),
        },
        _ => return refused(targets, gettext("Mail cannot go there")),
    };
    let mut undo = Undo {
        id: actions.next.fetch_add(1, Ordering::Relaxed),
        action,
        relabel: Vec::new(),
        colors: Vec::new(),
        reminders: Vec::new(),
        follow_ups: Vec::new(),
        staged: Vec::new(),
        cross: Vec::new(),
    };
    let mut results: Vec<Option<Result<(), String>>> = vec![None; targets.len()];
    // The rows the destination account already holds need no carrying,
    // only the move a drop inside that account makes.
    let own: Vec<usize> = (0..targets.len())
        .filter(|i| targets[*i].account_id == account_id)
        .collect();
    if !own.is_empty() {
        let batch: Vec<Target> = own.iter().map(|i| targets[*i].clone()).collect();
        let dest_set = match at {
            Mailbox::Label { label_id, .. } => MailSet::Mailbox(label_id.clone()),
            _ => MailSet::Role(Role::Inbox),
        };
        let remove = from
            .of(account_id)
            .filter(|set| **set != dest_set)
            .cloned()
            .into_iter()
            .collect();
        let triage = Ok(Some(TriageAction::Relabel {
            add: vec![dest_set],
            remove,
        }));
        let resolved = vec![triage; batch.len()];
        for (index, changes) in own
            .iter()
            .zip(actions.triage_grouped(&batch, &resolved, from).await)
        {
            match changes {
                Ok(changes) => {
                    undo.relabel.push(Reversal {
                        target: targets[*index].clone(),
                        changes,
                        folder: None,
                    });
                    results[*index] = Some(Ok(()));
                }
                Err(error) => results[*index] = Some(Err(error)),
            }
        }
    }
    for (index, target) in targets.iter().enumerate() {
        if results[index].is_some() {
            continue;
        }
        results[index] = Some(
            actions
                .carry_one(target, &dest, at, &place, &words, &mut undo.cross)
                .await,
        );
    }
    let mut outcome = Outcome::default();
    for (target, result) in targets.iter().zip(results) {
        match result.unwrap_or_else(|| Err(words.clone())) {
            Ok(()) => outcome.done.push(target.clone()),
            Err(error) => outcome.failed.push(Failure {
                target: target.clone(),
                error,
            }),
        }
    }
    // Record what actually happened rather than what fully succeeded:
    // a target whose carry stopped at one message still holds confirmed
    // moves, and Undo is the only road those messages have back.
    if history == History::Record && !(undo.relabel.is_empty() && undo.cross.is_empty()) {
        outcome.recorded = Some(actions.record(undo).await);
    }
    outcome
}

impl<A: Accounts> MailActions<A> {
    /// Carries one foreign target: reads its messages out of their
    /// account, lands them in `place`, and erases the originals only
    /// once copies are safe. The target is done when every message of
    /// it made the whole road. A message whose copy landed but whose
    /// original stayed is reported as staying in both places, and not
    /// recorded for Undo: undoing half a carry would put a third copy
    /// in the world.
    async fn carry_one(
        &self,
        target: &Target,
        dest: &AccountSync,
        at: &Mailbox,
        place: &str,
        words: &str,
        cross: &mut Vec<CrossReversal>,
    ) -> Result<(), String> {
        let failed = |reason: String| {
            fill(
                &gettext("{action} failed: {reason}"),
                &[("action", words), ("reason", &reason)],
            )
        };
        let source = match self.accounts.account(target.account_id) {
            Some(source) => source,
            None => {
                return Err(failed(
                    SyncError::UnknownAccount(target.account_id).to_string(),
                ));
            }
        };
        let copies = match source.copies_to_carry(target).await {
            Ok(copies) => copies,
            Err(err) => return Err(failed(err.to_string())),
        };
        let place = place.to_string();
        let mut carried = Vec::new();
        let mut stuck: Option<String> = None;
        for copy in &copies {
            let raws = match source.fetch_raw(std::slice::from_ref(&copy.id)).await {
                Ok(mut raws) => raws.pop().map(|raw| raw.bytes),
                Err(err) => {
                    stuck.get_or_insert_with(|| failed(err.to_string()));
                    continue;
                }
            };
            let Some(bytes) = raws else {
                stuck
                    .get_or_insert_with(|| failed(gettext("The server did not send the message.")));
                continue;
            };
            let landing = Landing {
                mailboxes: std::slice::from_ref(&place),
                keywords: &copy.keywords,
                at: Some(copy.at),
            };
            let landed = match dest.land(&bytes, &landing).await {
                Ok(name) => name,
                Err(err) => {
                    // This message's copy did not arrive and its
                    // original stays at home. The other messages of
                    // the target each make their own road.
                    stuck.get_or_insert_with(|| failed(err.to_string()));
                    continue;
                }
            };
            match source.erase_carried(target, &copy.id).await {
                Ok(()) => carried.push((landed, copy.clone())),
                Err(err) => {
                    // The copy is safe and the original stayed: both
                    // places hold it now, and that is what the person
                    // has to hear.
                    let mailbox = at.title();
                    stuck.get_or_insert_with(|| {
                        fill(
                            &gettext("Moved to {mailbox}, but a copy stayed behind: {reason}"),
                            &[("mailbox", &mailbox), ("reason", &err.to_string())],
                        )
                    });
                }
            }
        }
        if !carried.is_empty() {
            cross.push(CrossReversal {
                target: target.clone(),
                arrived: dest.account_id(),
                at: at.clone(),
                carried,
            });
            // Ask the destination to look now, so its own sync brings
            // the new copies' rows into the store. A look that fails
            // only delays them: the copies are on the server, and the
            // next pass finds them.
            if let Err(err) = dest.look_for_mail().await {
                tracing::warn!(error = %err, "a look after a carry did not answer");
            }
        }
        match stuck {
            Some(reason) => Err(reason),
            None => Ok(()),
        }
    }

    /// Brings one carried target back: the bytes come out of the
    /// account they arrived in, go back into the one they left, and the
    /// copy over there is taken back once the way home is certain. The
    /// one order Undo may keep, for the same reason a carry may.
    pub(super) async fn reverse_carried(&self, reversal: &CrossReversal, outcome: &mut Outcome) {
        let failure = |reason: String| Failure {
            target: reversal.target.clone(),
            error: fill(
                &gettext("Could not undo: {reason}"),
                &[("reason", &reason)],
            ),
        };
        let (Some(home), Some(dest)) = (
            self.accounts.account(reversal.target.account_id),
            self.accounts.account(reversal.arrived),
        ) else {
            outcome.failed.push(failure(gettext(
                "One of the accounts is gone, so this mail stayed where it is.",
            )));
            return;
        };
        let mut stuck: Option<String> = None;
        for (landed, copy) in &reversal.carried {
            if let Err(reason) = bring_home(&home, &dest, landed, copy, &reversal.at).await {
                // One message's way back can fail while its siblings
                // make theirs, so each is tried and the first
                // complaint is the one the person hears.
                stuck.get_or_insert(reason);
            }
        }
        // Both accounts gain or lose mail on the server, so both are
        // asked to look now: the home account's store gets the rows
        // back, and the other one's lose the copies.
        if let Err(err) = home.look_for_mail().await {
            tracing::warn!(error = %err, "a look after an undone carry did not answer");
        }
        if let Err(err) = dest.look_for_mail().await {
            tracing::warn!(error = %err, "a look after an undone carry did not answer");
        }
        match stuck {
            Some(reason) => outcome.failed.push(failure(reason)),
            None => outcome.done.push(reversal.target.clone()),
        }
    }
}

/// One message's way back: fetched from the account it was carried to,
/// filed again where it started, and taken back once it is home. A
/// failure after the filing leaves the message in both accounts, with
/// the destination named, because the copy over there is the one the
/// person has to delete by hand.
async fn bring_home(
    home: &AccountSync,
    dest: &AccountSync,
    landed: &str,
    copy: &CopyOut,
    at: &Mailbox,
) -> Result<(), String> {
    let bytes = dest
        .landed_bytes(landed)
        .await
        .map_err(|err| err.to_string())?;
    let landing = Landing {
        mailboxes: &copy.mailboxes,
        keywords: &copy.keywords,
        at: Some(copy.at),
    };
    home.land(&bytes, &landing)
        .await
        .map_err(|err| err.to_string())?;
    dest.take_back(landed).await.map_err(|err| {
        let mailbox = at.title();
        let reason = err.to_string();
        fill(
            &gettext("Back where it started, but the copy in {mailbox} stayed behind: {reason}"),
            &[("mailbox", &mailbox), ("reason", &reason)],
        )
    })
}
