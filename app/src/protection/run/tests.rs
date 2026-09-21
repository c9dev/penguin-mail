//! The engine run, headless. Each test drives `Engines::run` the way the
//! window does and checks what the ports were asked for, in order.

use mailrs_domain::Protection;

use super::Installed;
use super::fake::{ELSEWHERE, FakeWindow, Step, opened, thread, with_bodies};

#[tokio::test]
async fn a_signed_message_gets_what_the_engine_said() {
    let window = FakeWindow::showing(thread(Some(Protection::Signed)));
    window.engines().run().await;
    assert_eq!(
        window.steps(),
        [Step::Claim, Step::Fetch, Step::Ask, Step::Answered]
    );
    let screen = window.0.borrow();
    let (message_id, read) = screen.answers.first().expect("the engine answered");
    assert_eq!(message_id, "m1");
    assert_eq!(read.mark.title, "Signed by Ann");
}

#[tokio::test]
async fn an_opened_message_brings_back_the_body_that_was_inside() {
    let window = FakeWindow::showing(thread(Some(Protection::Encrypted)));
    window.with(|screen| screen.read = Ok(opened()));
    window.engines().run().await;
    let screen = window.0.borrow();
    let (_, read) = screen.answers.first().expect("the engine answered");
    assert!(read.body.is_some(), "the opened message goes to the window");
}

#[tokio::test]
async fn a_message_in_the_clear_is_never_claimed() {
    let window = FakeWindow::showing(thread(None));
    window.engines().run().await;
    assert_eq!(window.steps(), [Step::Claim]);
    assert!(window.0.borrow().answers.is_empty());
}

#[tokio::test]
async fn a_message_whose_body_has_not_arrived_is_never_claimed() {
    let window = FakeWindow::showing(with_bodies(vec![("m1", Err("still loading".into()))]));
    window.engines().run().await;
    assert_eq!(window.steps(), [Step::Claim]);
}

#[tokio::test]
async fn nothing_is_claimed_with_no_conversation_on_screen() {
    let window = FakeWindow::showing(thread(Some(Protection::Signed)));
    window.with(|screen| screen.open = None);
    window.engines().run().await;
    assert_eq!(window.steps(), [Step::Claim]);
}

#[tokio::test]
async fn an_smime_message_waits_for_a_computer_with_gpgsm() {
    let window = FakeWindow::showing(thread(Some(Protection::SmimeSigned)));
    window.with(|screen| screen.installed.smime = false);
    window.engines().run().await;
    assert_eq!(window.steps(), [Step::Claim]);
    // The claim was left unmade, so the message is still there to read on
    // the day gpgsm turns up.
    window.with(|screen| screen.installed.smime = true);
    window.engines().run().await;
    assert_eq!(window.0.borrow().answers.len(), 1);
}

#[tokio::test]
async fn an_openpgp_message_waits_for_a_computer_with_gpg() {
    let window = FakeWindow::showing(thread(Some(Protection::Signed)));
    window.with(|screen| screen.installed = Installed::default());
    window.engines().run().await;
    assert_eq!(window.steps(), [Step::Claim]);
}

#[tokio::test]
async fn the_engine_is_asked_once_per_thread() {
    let window = FakeWindow::showing(thread(Some(Protection::Signed)));
    window.engines().run().await;
    window.engines().run().await;
    assert_eq!(
        window.steps(),
        [
            Step::Claim,
            Step::Fetch,
            Step::Ask,
            Step::Answered,
            Step::Claim
        ]
    );
}

/// The rule that has already produced bugs: the claim is made before
/// anything is awaited, so a run that starts while the first one holds a
/// pinentry finds nothing to do.
#[tokio::test]
async fn a_run_that_starts_mid_pinentry_puts_up_no_second_one() {
    let window = FakeWindow::showing(thread(Some(Protection::Encrypted)));
    let (release, held) = futures::channel::oneshot::channel();
    window.with(|screen| screen.holds = Some(held));
    let (first, second) = (window.engines(), window.engines());
    tokio::join!(first.run(), async {
        second.run().await;
        let _ = release.send(());
    });
    assert_eq!(
        window.steps(),
        [
            Step::Claim,
            Step::Fetch,
            Step::Ask,
            Step::Claim,
            Step::Answered
        ]
    );
    assert_eq!(window.0.borrow().answers.len(), 1);
}

#[tokio::test]
async fn a_reader_who_moves_on_during_the_fetch_gets_no_card() {
    let window = FakeWindow::showing(thread(Some(Protection::Signed)));
    window.with(|screen| screen.moves_on = Some(Step::Fetch));
    window.engines().run().await;
    assert_eq!(window.steps(), [Step::Claim, Step::Fetch]);
    assert!(window.0.borrow().answers.is_empty());
}

#[tokio::test]
async fn a_reader_who_moves_on_while_the_engine_runs_gets_no_card() {
    let window = FakeWindow::showing(thread(Some(Protection::Encrypted)));
    window.with(|screen| screen.moves_on = Some(Step::Ask));
    window.engines().run().await;
    assert_eq!(window.steps(), [Step::Claim, Step::Fetch, Step::Ask]);
    assert!(window.0.borrow().answers.is_empty());
    assert!(
        window
            .0
            .borrow()
            .open
            .as_ref()
            .is_some_and(|open| open.thread_id == ELSEWHERE)
    );
}

#[tokio::test]
async fn a_message_gmail_will_not_hand_over_leaves_the_card_off() {
    let window = FakeWindow::showing(thread(Some(Protection::Signed)));
    window.with(|screen| screen.raw = Err("the network is down".into()));
    window.engines().run().await;
    assert_eq!(window.steps(), [Step::Claim, Step::Fetch]);
    assert!(window.0.borrow().answers.is_empty());
}

#[tokio::test]
async fn an_engine_that_could_not_be_asked_leaves_the_card_off() {
    let window = FakeWindow::showing(thread(Some(Protection::SmimeEnveloped)));
    window.with(|screen| screen.read = Err("gpgsm died".into()));
    window.engines().run().await;
    assert_eq!(window.steps(), [Step::Claim, Step::Fetch, Step::Ask]);
    assert!(window.0.borrow().answers.is_empty());
}
