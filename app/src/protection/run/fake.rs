//! The engine run with no window: one thread in memory behind both ports,
//! an answer waiting for each of the two calls that take time, and a log
//! of what the run asked for, in order.
//!
//! The claim goes through the same [`OpenThread::take_protected`] the
//! window uses, so the once-per-thread rule under test is the one that
//! ships. Nothing here starts a widget, and nothing here runs gpg.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use mailrs_domain::{AccountId, MessageBody, MessageMeta, Protection, Target};

use super::{Answer, Claimed, Desk, Effects, Engines, Installed};
use crate::open_thread::OpenThread;
use crate::protection::remembered::Verdicts;
use crate::protection::{Engine, Mark, Read, Tone};
use crate::wanted::Screen as OnScreen;

/// The account and thread every fixture belongs to.
pub const ACCOUNT: AccountId = 1;
pub const THREAD: &str = "t1";
/// The thread the reader opens instead, mid-run.
pub const ELSEWHERE: &str = "t2";

/// One thing the run asked the window for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Claim,
    Fetch,
    Ask,
    Answered,
}

/// The window the run reads and writes.
pub struct Screen {
    pub installed: Installed,
    /// The thread on screen, or `None` once the reader closed it.
    pub open: Option<OpenThread>,
    /// What Gmail hands back for the raw message.
    pub raw: Result<Vec<u8>, String>,
    /// What the engine makes of each message it is asked about.
    pub read: Result<Read, String>,
    /// The step the reader opens another thread during, which is how a
    /// test makes an answer arrive for a thread nobody is looking at.
    pub moves_on: Option<Step>,
    /// Holds the engine where a pinentry would hold it, until the test
    /// lets go.
    pub holds: Option<futures::channel::oneshot::Receiver<()>>,
    /// What the run asked for, oldest first.
    pub steps: Vec<Step>,
    /// What the window was told the engine said.
    pub answers: Vec<(String, Read)>,
    /// When the keyring last changed.
    pub keyring: Option<std::time::SystemTime>,
    /// The answers the window keeps between runs.
    pub verdicts: Verdicts,
}

pub struct FakeWindow(pub RefCell<Screen>);

/// A message of the fixture thread.
pub fn meta(id: &str) -> MessageMeta {
    MessageMeta {
        account_id: ACCOUNT,
        id: id.to_string(),
        thread_id: THREAD.to_string(),
        rfc822_msgid: None,
        from: None,
        to: Vec::new(),
        cc: Vec::new(),
        subject: "Kite plans".to_string(),
        date: 0,
        snippet: String::new(),
        size: 0,
        has_attachments: false,
        label_ids: Vec::new(),
        list_unsubscribe: None,
        one_click: false,
    }
}

/// A body as `extract_body` reports one: the wrapper the message arrived
/// in, and none of the parts, since a signature covers the bytes as they
/// were sent.
pub fn body(protection: Option<Protection>) -> MessageBody {
    MessageBody {
        text: Some("Hello".to_string()),
        protection,
        ..MessageBody::default()
    }
}

/// A thread of one message with that body on screen.
pub fn thread(protection: Option<Protection>) -> OpenThread {
    with_bodies(vec![("m1", Ok(body(protection)))])
}

/// A thread whose messages have those bodies, oldest first.
pub fn with_bodies(messages: Vec<(&str, Result<MessageBody, String>)>) -> OpenThread {
    OpenThread {
        account_id: ACCOUNT,
        thread_id: THREAD.to_string(),
        subject: "Kite plans".to_string(),
        messages: messages.iter().map(|(id, _)| meta(id)).collect(),
        bodies: messages
            .into_iter()
            .map(|(id, body)| (id.to_string(), body))
            .collect(),
        expanded: HashSet::new(),
        images_allowed: false,
        only_message: None,
        me: Vec::new(),
        inline_images: HashMap::new(),
        thumbnails: HashMap::new(),
        opened_files: HashMap::new(),
        sealed: HashSet::new(),
        photos: HashMap::new(),
        unsubscribed: false,
        marks: HashMap::new(),
        asked: HashSet::new(),
        flag_color: None,
        translations: HashMap::new(),
        queued: None,
    }
}

/// What a good signature looks like coming back from an engine: the mark,
/// and the body cut from the part it covers.
pub fn signed() -> Read {
    Read {
        mark: Mark {
            title: "Signed by Ann".to_string(),
            detail: None,
            tone: Tone::Good,
        },
        body: Some(body(None)),
        files: Vec::new(),
        sealed: false,
    }
}

/// What opening a message looks like: the mark, plus the message that was
/// inside the ciphertext.
pub fn opened() -> Read {
    Read {
        sealed: true,
        ..signed()
    }
}

impl FakeWindow {
    /// A window with `open` on screen, both engines on this computer, and
    /// an answer waiting for each call.
    pub fn showing(open: OpenThread) -> Rc<FakeWindow> {
        Rc::new(FakeWindow(RefCell::new(Screen {
            installed: Installed {
                pgp: true,
                smime: true,
            },
            open: Some(open),
            raw: Ok(b"From: ann@example.com".to_vec()),
            read: Ok(signed()),
            moves_on: None,
            holds: None,
            steps: Vec::new(),
            answers: Vec::new(),
            keyring: Some(std::time::SystemTime::UNIX_EPOCH),
            verdicts: Verdicts::default(),
        })))
    }

    pub fn with<R>(&self, change: impl FnOnce(&mut Screen) -> R) -> R {
        change(&mut self.0.borrow_mut())
    }

    /// The run, with this window behind both ports.
    pub fn engines(self: &Rc<Self>) -> Engines {
        Engines::new(
            Rc::clone(self) as Rc<dyn Desk>,
            Rc::clone(self) as Rc<dyn Effects>,
        )
    }

    /// The steps the run took, oldest first.
    pub fn steps(&self) -> Vec<Step> {
        self.0.borrow().steps.clone()
    }

    /// Notes a step, and moves the reader on to another thread when the
    /// test asked for that to happen during this one.
    fn reached(&self, step: Step) {
        self.with(|screen| {
            screen.steps.push(step);
            if screen.moves_on == Some(step)
                && let Some(open) = screen.open.as_mut()
            {
                open.thread_id = ELSEWHERE.to_string();
            }
        });
    }
}

impl Desk for FakeWindow {
    fn installed(&self) -> Installed {
        self.with(|screen| screen.installed)
    }

    fn claim(&self, installed: Installed) -> Vec<Claimed> {
        self.reached(Step::Claim);
        self.with(|screen| {
            screen
                .open
                .as_mut()
                .map(|open| open.take_protected(installed))
                .unwrap_or_default()
        })
    }
}

impl OnScreen for FakeWindow {
    fn is_showing(&self, target: &Target) -> bool {
        self.with(|screen| {
            screen
                .open
                .as_ref()
                .is_some_and(|open| open.target() == *target)
        })
    }
}

impl Effects for FakeWindow {
    fn raw_message(
        &self,
        _account_id: AccountId,
        _message_id: String,
    ) -> Answer<'_, Result<Vec<u8>, String>> {
        self.reached(Step::Fetch);
        let raw = self.with(|screen| screen.raw.clone());
        Box::pin(async move { raw })
    }

    fn ask(
        &self,
        _opening: Engine,
        _raw: Vec<u8>,
        _body: MessageBody,
    ) -> Answer<'_, Result<Read, String>> {
        self.reached(Step::Ask);
        let (held, read) = self.with(|screen| (screen.holds.take(), screen.read.clone()));
        Box::pin(async move {
            if let Some(held) = held {
                let _ = held.await;
            }
            read
        })
    }

    fn answered(&self, _target: Target, message_id: String, read: Read) {
        self.reached(Step::Answered);
        self.with(|screen| screen.answers.push((message_id, read)));
    }

    fn remembered(&self, _opening: Engine, message_id: &str) -> Option<Read> {
        self.with(|screen| screen.verdicts.get(message_id, screen.keyring))
    }

    fn remember(&self, _opening: Engine, message_id: String, read: &Read) {
        self.with(|screen| {
            let keyring = screen.keyring;
            screen.verdicts.keep(message_id, keyring, read);
        });
    }
}
