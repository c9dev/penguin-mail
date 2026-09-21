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
//! anything is awaited, and it is the only way to reach [`Wanted`], which
//! is the only way to reach the calls that await. Then every one of those
//! calls answers through `Wanted`, which gives nothing back once the
//! reader has opened another thread, so an answer nobody is waiting for
//! cannot be written.
//!
//! Nothing here touches GTK. The window is one adapter behind the ports
//! and the tests are another.

use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

use mailrs_domain::{AccountId, MessageBody};

use super::{Engine, Read};

#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;

/// A future the GTK thread waits on. The ports run on that thread, so
/// their answers need no `Send`.
pub type Answer<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

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

/// The protected message of the thread on screen, claimed for one engine
/// run. Whoever holds one has already set that thread's "ask each engine
/// once" flag, so nothing else can put a second pinentry up.
pub struct Claimed {
    pub account_id: AccountId,
    pub thread_id: String,
    pub message_id: String,
    pub opening: Engine,
    pub body: MessageBody,
}

/// What the run reads from the window. Every method answers from what the
/// window already holds, so a test fills one in without a widget.
pub trait Desk {
    fn installed(&self) -> Installed;
    /// The protected message of the thread on screen, claimed for this
    /// run, leaving a message whose engine `installed` lacks unclaimed.
    /// Once per thread: a second call gives nothing back.
    fn claim(&self, installed: Installed) -> Option<Claimed>;
    /// Whether that thread is still the one on screen.
    fn is_showing(&self, account_id: AccountId, thread_id: &str) -> bool;
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
    /// Puts what the engine said above the message.
    fn answered(&self, message_id: String, read: Read);
}

/// The thread an answer belongs to, and the only way to a call that takes
/// time. Each one hands its answer back while that thread is the one on
/// screen and gives nothing back otherwise, because the reader may have
/// opened something else while gpg held a pinentry and an answer they are
/// no longer waiting for has nowhere to go.
pub struct Wanted<'a> {
    desk: &'a dyn Desk,
    effects: &'a dyn Effects,
    account_id: AccountId,
    thread_id: String,
}

impl<'a> Wanted<'a> {
    fn new(desk: &'a dyn Desk, effects: &'a dyn Effects, claimed: &Claimed) -> Wanted<'a> {
        Wanted {
            desk,
            effects,
            account_id: claimed.account_id,
            thread_id: claimed.thread_id.clone(),
        }
    }

    async fn raw_message(&self, message_id: &str) -> Option<Vec<u8>> {
        self.still(
            self.effects
                .raw_message(self.account_id, message_id.to_string()),
            "could not fetch the message to check how it was signed",
        )
        .await
    }

    async fn ask(&self, opening: Engine, raw: Vec<u8>, body: MessageBody) -> Option<Read> {
        self.still(
            self.effects.ask(opening, raw, body),
            "the engine could not be asked about this message",
        )
        .await
    }

    /// What was inside the encryption is what the reader wanted, and it
    /// goes no further than this window: the store keeps the message as
    /// Gmail holds it, ciphertext and all. `read` came through the awaits
    /// above, so the thread was on screen a moment ago.
    fn answered(&self, message_id: String, read: Read) {
        self.effects.answered(message_id, read);
    }

    /// Waits for `answer` and gives it back while the thread is still on
    /// screen. `None` is either the call failing, which `failed` says in
    /// the log, or the reader moving on while it ran.
    async fn still<T>(&self, answer: Answer<'_, Result<T, String>>, failed: &str) -> Option<T> {
        let answer = match answer.await {
            Ok(answer) => answer,
            Err(err) => {
                tracing::info!(error = %err, "{failed}");
                return None;
            }
        };
        self.desk
            .is_showing(self.account_id, &self.thread_id)
            .then_some(answer)
    }
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

    /// Checks or opens the protected message of the thread on screen and
    /// puts what the engine said above it. A thread that has been through
    /// this keeps the answer, so redrawing never asks again.
    pub async fn run(&self) {
        let Some(claimed) = self.desk.claim(self.desk.installed()) else {
            return;
        };
        let wanted = Wanted::new(&*self.desk, &*self.effects, &claimed);
        let Claimed {
            message_id,
            opening,
            body,
            ..
        } = claimed;
        let Some(raw) = wanted.raw_message(&message_id).await else {
            return;
        };
        let Some(read) = wanted.ask(opening, raw, body).await else {
            return;
        };
        wanted.answered(message_id, read);
    }
}
