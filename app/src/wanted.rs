//! The one rule for an answer that arrives late: it reaches the window
//! only while the conversation it was asked for is still on screen.
//!
//! A run that talks to the window holds a [`Wanted`] for the target it
//! started on, and reaches its effect port through nothing else. Every
//! call that takes time answers through [`Wanted::wait`] or
//! [`Wanted::ask`], which give nothing back once the reader has opened
//! another conversation, and every change to the window goes through
//! [`Wanted::on_screen`], which makes it only while that target is shown.
//! So a run cannot forget the question, and an answer nobody is waiting
//! for has nowhere to go.
//!
//! The key is a [`Target`]: the account, the thread, and the one message
//! when the view shows a message rather than the whole thread. Opening
//! one message of the thread on screen is opening something else.

use std::future::Future;
use std::pin::Pin;

use mailrs_domain::Target;

/// A future the GTK thread waits on. The ports run on that thread, so
/// their answers need no `Send`.
pub type Answer<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// What a late answer asks the window: whether the conversation it
/// belongs to is still the one on screen.
pub trait Screen {
    fn is_showing(&self, target: &Target) -> bool;
}

/// The conversation an answer belongs to, and the only way a run reaches
/// its effect port `E`.
pub struct Wanted<'a, E: ?Sized> {
    screen: &'a dyn Screen,
    effects: &'a E,
    target: Target,
}

impl<'a, E: ?Sized> Wanted<'a, E> {
    pub fn new(screen: &'a dyn Screen, effects: &'a E, target: Target) -> Wanted<'a, E> {
        Wanted {
            screen,
            effects,
            target,
        }
    }

    /// The conversation this run started on.
    pub fn target(&self) -> &Target {
        &self.target
    }

    /// Whether that conversation is still the one on screen.
    pub fn is_wanted(&self) -> bool {
        self.screen.is_showing(&self.target)
    }

    /// Runs `change` while the conversation is on screen and gives back
    /// what it said. `None` means the reader has moved on and nothing ran.
    pub fn on_screen<R>(&self, change: impl FnOnce(&'a E) -> R) -> Option<R> {
        self.is_wanted().then(|| change(self.effects))
    }

    /// Runs `call` whatever is on screen. For what belongs to the window
    /// rather than the conversation, such as a toast, and for a call whose
    /// failure the reader hears about wherever they are.
    pub fn anyway<R>(&self, call: impl FnOnce(&'a E) -> R) -> R {
        call(self.effects)
    }

    /// Waits for `call` and gives its answer back while the conversation
    /// is still on screen.
    pub async fn wait<T>(&self, call: impl FnOnce(&'a E) -> Answer<'a, T>) -> Option<T> {
        let answer = call(self.effects).await;
        self.is_wanted().then_some(answer)
    }

    /// Waits for `call`, which can fail, and gives its answer back while
    /// the conversation is still on screen. `None` is either the call
    /// failing, which `failed` says in the log, or the reader moving on
    /// while it ran.
    pub async fn ask<T>(
        &self,
        call: impl FnOnce(&'a E) -> Answer<'a, Result<T, String>>,
        failed: &str,
    ) -> Option<T> {
        match self.wait(call).await? {
            Ok(answer) => Some(answer),
            Err(err) => {
                tracing::info!(error = %err, "{failed}");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    /// A window with one target on screen, and the effect port a run
    /// would change it through.
    struct Window {
        showing: RefCell<Target>,
        changed: RefCell<Vec<&'static str>>,
    }

    impl Screen for Window {
        fn is_showing(&self, target: &Target) -> bool {
            *self.showing.borrow() == *target
        }
    }

    fn target(thread_id: &str, message_id: Option<&str>) -> Target {
        Target {
            account_id: 1,
            thread_id: thread_id.to_string(),
            message_id: message_id.map(str::to_string),
        }
    }

    fn window() -> Window {
        Window {
            showing: RefCell::new(target("t1", None)),
            changed: RefCell::new(Vec::new()),
        }
    }

    #[tokio::test]
    async fn an_answer_for_the_conversation_on_screen_comes_back() {
        let window = window();
        let wanted = Wanted::new(&window, &window, target("t1", None));
        let said = wanted.wait(|_| Box::pin(async { 7 })).await;
        assert_eq!(said, Some(7));
    }

    #[tokio::test]
    async fn an_answer_after_the_reader_moved_on_goes_nowhere() {
        let window = window();
        let wanted = Wanted::new(&window, &window, target("t1", None));
        let said = wanted
            .wait(|window| {
                *window.showing.borrow_mut() = target("t2", None);
                Box::pin(async { 7 })
            })
            .await;
        assert_eq!(said, None);
        assert_eq!(
            wanted.on_screen(|window| window.changed.borrow_mut().push("x")),
            None
        );
        assert!(window.changed.borrow().is_empty());
    }

    #[tokio::test]
    async fn one_message_of_the_thread_is_another_conversation() {
        let window = window();
        let wanted = Wanted::new(&window, &window, target("t1", None));
        *window.showing.borrow_mut() = target("t1", Some("m1"));
        assert!(!wanted.is_wanted());
    }

    #[tokio::test]
    async fn a_call_that_failed_gives_nothing_back() {
        let window = window();
        let wanted = Wanted::new(&window, &window, target("t1", None));
        let said: Option<u8> = wanted
            .ask(|_| Box::pin(async { Err("offline".to_string()) }), "failed")
            .await;
        assert_eq!(said, None);
    }
}
