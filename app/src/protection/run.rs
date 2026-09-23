//! Running the engine the message on screen needs, OpenPGP or S/MIME,
//! through two ports: [`Desk`] for what the window has open and
//! [`Effects`] for the two calls that take time.
//!
//! The bytes come from Gmail's `format=raw`, because a signature covers
//! the message as it was sent and the parts the API hands back have been
//! decoded since. The engine then runs behind [`Effects::ask`], off the
//! GTK thread, since it may put a pinentry in front of the person and wait
//! as long as they take to type.
//!
//! Two rules decide whether any of that reaches the window, and both are
//! in the types rather than in a comment. The claim comes first:
//! [`Desk::claim`] sets the thread's "ask each engine once" flag before
//! anything is awaited, and the target it names is the only way to a
//! [`Wanted`], which is the only way to reach the calls that await. Then
//! every one of those calls answers through `Wanted`, which gives nothing
//! back once the reader has opened another conversation, so an answer
//! nobody is waiting for cannot be written.
//!
//! Nothing here touches GTK. The window is one adapter behind the ports
//! and the tests are another.

use std::rc::Rc;

use mailrs_domain::{AccountId, MessageBody, Target};

use super::{Engine, Read};
pub use crate::wanted::Answer;
use crate::wanted::{Screen, Wanted};

#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;

/// The engines this computer has. A message names its standard, and the
/// engine that reads it may be the one this computer lacks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Installed {
    pub pgp: bool,
    pub smime: bool,
}

impl Installed {
    /// Whether the call a message needs can run here.
    pub fn runs(&self, engine: Engine) -> bool {
        match engine {
            Engine::Pgp(_) => self.pgp,
            Engine::Smime(_) => self.smime,
        }
    }
}

/// One protected message of the thread on screen, claimed for one engine
/// run. Whoever holds one has already marked that message as asked about,
/// so nothing else can put a second pinentry up for it.
pub struct Claimed {
    /// The conversation on screen when the claim was made.
    pub target: Target,
    pub message_id: String,
    pub opening: Engine,
    pub body: MessageBody,
}

/// What the run reads from the window. Every method answers from what the
/// window already holds, so a test fills one in without a widget. As a
/// [`Screen`] it also says whether the claimed thread is still on screen.
pub trait Desk: Screen {
    fn installed(&self) -> Installed;
    /// The protected messages of the thread on screen, newest first,
    /// claimed for this run, leaving a message whose engine `installed`
    /// lacks unclaimed. Once per message: a second call leaves out what
    /// the first took.
    fn claim(&self, installed: Installed) -> Vec<Claimed>;
}

/// What the run asks the window to do. A test answers with what it likes
/// and records the rest.
pub trait Effects {
    /// The message as it was sent, from Gmail's `format=raw`.
    fn raw_message(
        &self,
        account_id: AccountId,
        message_id: String,
    ) -> Answer<'_, Result<Vec<u8>, String>>;
    /// Runs the call the message needs against the person's gpg or gpgsm,
    /// which may hold a pinentry in front of them for as long as they take
    /// to type.
    fn ask(
        &self,
        opening: Engine,
        raw: Vec<u8>,
        body: MessageBody,
    ) -> Answer<'_, Result<Read, String>>;
    /// Puts what the engine said above the message in `target`, the
    /// conversation the claim was made on.
    fn answered(&self, target: Target, message_id: String, read: Read);
    /// What the engine said about this message earlier in the run, while
    /// the keyring `opening` reads against has not changed since.
    fn remembered(&self, opening: Engine, message_id: &str) -> Option<Read>;
    /// Keeps what the engine said, for a message that arrived in the clear.
    fn remember(&self, opening: Engine, message_id: String, read: &Read);
}

/// The two engines, and the one way to run the one a message needs.
pub struct Engines {
    desk: Rc<dyn Desk>,
    effects: Rc<dyn Effects>,
}

impl Engines {
    pub fn new(desk: Rc<dyn Desk>, effects: Rc<dyn Effects>) -> Engines {
        Engines { desk, effects }
    }

    /// Checks or opens every protected message of the thread on screen,
    /// newest first and one at a time, and puts what the engine said above
    /// each. One engine call at a time keeps to one pinentry at a time. A
    /// thread that has been through this keeps the answers, so redrawing
    /// never asks again.
    pub async fn run(&self) {
        let claimed = self.desk.claim(self.desk.installed());
        let Some(target) = claimed.first().map(|claimed| claimed.target.clone()) else {
            return;
        };
        let wanted = Wanted::new(&*self.desk as &dyn Screen, &*self.effects, target);
        for claimed in claimed {
            // The reader opened something else while an earlier message
            // held the engine; what is left belongs to a thread nobody is
            // looking at.
            if !wanted.is_wanted() {
                return;
            }
            self.one(&wanted, claimed).await;
        }
    }

    /// Checks or opens one claimed message.
    async fn one(&self, wanted: &Wanted<'_, dyn Effects>, claimed: Claimed) {
        let Claimed {
            target,
            message_id,
            opening,
            body,
        } = claimed;
        if let Some(read) = wanted.anyway(|effects| effects.remembered(opening, &message_id)) {
            let target = wanted.target().clone();
            wanted.on_screen(|effects| effects.answered(target, message_id, read));
            return;
        }
        let account_id = target.account_id;
        let fetching = message_id.clone();
        let Some(raw) = wanted
            .ask(
                |effects| effects.raw_message(account_id, fetching),
                "could not fetch the message to check how it was signed",
            )
            .await
        else {
            return;
        };
        let Some(read) = wanted
            .ask(
                |effects| effects.ask(opening, raw, body),
                "the engine could not be asked about this message",
            )
            .await
        else {
            return;
        };
        // What was inside the encryption is what the reader wanted, and it
        // goes no further than this window: the store keeps the message as
        // Gmail holds it, ciphertext and all. A signed message's answer is
        // kept, so opening it again costs neither Gmail nor gpg.
        wanted.anyway(|effects| effects.remember(opening, message_id.clone(), &read));
        let target = wanted.target().clone();
        wanted.on_screen(|effects| effects.answered(target, message_id, read));
    }
}
