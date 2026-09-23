//! The thread run, headless. Each test drives `ThreadRun` the way the
//! window does and checks what the ports were asked for.

use mailrs_domain::Target;

use super::fake::{
    ACCOUNT, ELSEWHERE, FakeWindow, Step, THREAD, body, html_body, invited, meta,
    opened_occurrence, portuguese, queued, row, with_picture,
};
use super::{Card, Event, Stale};
use crate::protection::{Mark, Read, Tone};

fn target(message_id: Option<&str>) -> Target {
    Target {
        account_id: ACCOUNT,
        thread_id: THREAD.to_string(),
        message_id: message_id.map(str::to_string),
    }
}

/// What an engine says about a message it opened: the mark, and the body
/// that was inside.
fn opened(inside: mailrs_domain::MessageBody) -> Read {
    Read {
        mark: Mark {
            title: "Encrypted for you".to_string(),
            detail: None,
            tone: Tone::Good,
        },
        body: Some(inside),
        files: Vec::new(),
        sealed: true,
    }
}

#[tokio::test]
async fn opening_shows_the_stored_copy_then_the_bodies_gmail_sent() {
    let window = FakeWindow::new();
    window.run().open(row(THREAD)).await;
    let steps = window.steps();
    let at = |step| steps.iter().position(|s| *s == step).expect("step taken");
    assert!(at(Step::Stored) < at(Step::Show));
    assert!(at(Step::Show) < at(Step::Ensure));
    assert!(at(Step::Ensure) < at(Step::Bodies));
    assert!(at(Step::Bodies) < at(Step::BodiesArrived));
    let text = window
        .open(|open| open.bodies["m1"].clone().ok()?.text)
        .flatten();
    assert_eq!(text.as_deref(), Some("Hello"));
}

/// The store already held every body, so Gmail has nothing to add and the
/// page stays as the stored copy drew it. The pictures and the read mark
/// still come.
#[tokio::test]
async fn a_thread_whose_bodies_the_store_held_fetches_none() {
    let window = FakeWindow::new();
    window.with(|screen| {
        if let Some(stored) = screen.stored.get_mut(THREAD) {
            stored.bodies.insert("m1".to_string(), with_picture());
        }
    });
    window.run().open(row(THREAD)).await;
    assert!(!window.took(Step::Bodies));
    assert!(!window.took(Step::BodiesArrived));
    assert!(window.took(Step::ThumbnailsArrived));
    assert_eq!(window.0.borrow().marked, [target(None)]);
}

#[tokio::test]
async fn two_quick_opens_show_only_the_later_one() {
    let window = FakeWindow::new();
    let (release, held) = futures::channel::oneshot::channel();
    window.with(|screen| {
        screen.holds.insert(THREAD.to_string(), held);
        let later = screen.stored[THREAD].clone();
        screen.stored.insert(ELSEWHERE.to_string(), later);
    });
    let (first, second) = (window.run(), window.run());
    futures::join!(first.open(row(THREAD)), async {
        second.open(row(ELSEWHERE)).await;
        let _ = release.send(());
    });
    assert_eq!(window.0.borrow().shown, [ELSEWHERE]);
}

#[tokio::test]
async fn a_reader_who_moves_on_while_gmail_fetches_the_thread_gets_no_bodies() {
    let window = FakeWindow::new();
    window.with(|screen| screen.moves_on = Some(Step::Ensure));
    window.run().open(row(THREAD)).await;
    assert!(!window.took(Step::Messages));
    assert!(!window.took(Step::BodiesArrived));
}

#[tokio::test]
async fn a_reader_who_moves_on_while_the_bodies_load_gets_none_of_them() {
    let window = FakeWindow::new();
    window.with(|screen| screen.moves_on = Some(Step::Bodies));
    window.run().open(row(THREAD)).await;
    assert!(!window.took(Step::BodiesArrived));
    assert!(!window.took(Step::MarkRead));
}

#[tokio::test]
async fn the_messages_of_a_thread_the_reader_left_stay_out_of_the_next() {
    let window = FakeWindow::new();
    window.with(|screen| {
        screen.moves_on = Some(Step::Messages);
        screen.messages.push(meta("m2", true));
    });
    window.run().open(row(THREAD)).await;
    assert!(!window.took(Step::MessagesArrived));
    assert_eq!(window.open(|open| open.messages.len()), Some(1));
}

#[tokio::test]
async fn pictures_arrive_on_the_attachment_rows() {
    let window = FakeWindow::with_body(with_picture());
    window.run().open(row(THREAD)).await;
    assert!(window.took(Step::ThumbnailsArrived));
    assert_eq!(window.open(|open| open.thumbnails.len()), Some(1));
}

#[tokio::test]
async fn pictures_for_a_thread_the_reader_left_go_nowhere() {
    let window = FakeWindow::with_body(with_picture());
    window.with(|screen| screen.moves_on = Some(Step::Thumbnails));
    window.run().open(row(THREAD)).await;
    assert!(window.took(Step::Thumbnails));
    assert!(!window.took(Step::ThumbnailsArrived));
}

#[tokio::test]
async fn an_unread_thread_is_marked_read_after_the_delay() {
    let window = FakeWindow::new();
    window.run().open(row(THREAD)).await;
    assert_eq!(window.0.borrow().marked, [target(None)]);
}

#[tokio::test]
async fn a_thread_left_during_the_delay_stays_unread() {
    let window = FakeWindow::new();
    window.with(|screen| screen.moves_on = Some(Step::Sleep));
    window.run().open(row(THREAD)).await;
    assert!(window.took(Step::Sleep));
    assert!(window.0.borrow().marked.is_empty());
}

/// The same thread is not the same conversation once the reader goes from
/// one message of it to all of it: only what they were reading is marked.
#[tokio::test]
async fn mark_read_later_marks_only_the_conversation_still_open() {
    let window = FakeWindow::new();
    window.with(|screen| {
        screen.moves_on = Some(Step::Sleep);
        screen.moving = |open| open.only_message = None;
    });
    let mut one = row(THREAD);
    one.message_id = Some("m1".to_string());
    window.run().open(one).await;
    assert!(window.0.borrow().marked.is_empty());
}

#[tokio::test]
async fn a_reader_who_marks_mail_by_hand_is_left_to_it() {
    let window = FakeWindow::new();
    window.with(|screen| screen.delay = None);
    window.run().open(row(THREAD)).await;
    assert!(!window.took(Step::Sleep));
    assert!(window.0.borrow().marked.is_empty());
}

#[tokio::test]
async fn a_thread_already_read_is_not_marked_again() {
    let window = FakeWindow::new();
    window.with(|screen| screen.messages = vec![meta("m1", false)]);
    window.run().open(row(THREAD)).await;
    assert!(!window.took(Step::Sleep));
}

#[tokio::test]
async fn an_invitation_goes_on_the_card_with_what_else_is_on() {
    let window = FakeWindow::with_body(invited());
    window.run().open(row(THREAD)).await;
    let screen = window.0.borrow();
    assert_eq!(
        screen.invitations.last(),
        Some(&Some("kites@example.com".to_string()))
    );
    assert!(screen.steps.contains(&Step::OfferGnome));
    assert!(screen.steps.contains(&Step::Clashes));
}

#[tokio::test]
async fn an_invitation_read_for_a_thread_the_reader_left_goes_nowhere() {
    let window = FakeWindow::with_body(invited());
    window.with(|screen| screen.moves_on = Some(Step::OpenInvitation));
    window.run().open(row(THREAD)).await;
    let screen = window.0.borrow();
    assert!(screen.invitations.iter().all(Option::is_none));
    assert!(!screen.steps.contains(&Step::Busy));
}

#[tokio::test]
async fn an_invitation_to_one_occurrence_says_how_the_series_runs() {
    let window = FakeWindow::with_body(invited());
    window.with(|screen| screen.invitation = Ok(Some(opened_occurrence())));
    window.run().open(row(THREAD)).await;
    assert_eq!(window.0.borrow().series_lines, ["Every Tuesday, 6 left"]);
}

#[tokio::test]
async fn an_invitation_to_a_whole_event_asks_for_no_series() {
    let window = FakeWindow::with_body(invited());
    window.run().open(row(THREAD)).await;
    assert!(!window.took(Step::Series));
}

#[tokio::test]
async fn a_series_the_calendar_cannot_give_leaves_the_card_as_it_was() {
    for series in [Ok(None), Err("offline".to_string())] {
        let window = FakeWindow::with_body(invited());
        window.with(|screen| {
            screen.invitation = Ok(Some(opened_occurrence()));
            screen.series = series;
        });
        window.run().open(row(THREAD)).await;
        assert!(window.took(Step::Series));
        assert!(!window.took(Step::SeriesKnown));
        // The clashes still go on the card.
        assert!(window.took(Step::Clashes));
    }
}

#[tokio::test]
async fn a_series_for_a_thread_the_reader_left_goes_nowhere() {
    let window = FakeWindow::with_body(invited());
    window.with(|screen| {
        screen.invitation = Ok(Some(opened_occurrence()));
        screen.moves_on = Some(Step::Series);
    });
    window.run().open(row(THREAD)).await;
    assert!(window.took(Step::Series));
    assert!(!window.took(Step::SeriesKnown));
}

#[tokio::test]
async fn clashes_for_a_thread_the_reader_left_go_nowhere() {
    let window = FakeWindow::with_body(invited());
    window.with(|screen| screen.moves_on = Some(Step::Busy));
    window.run().open(row(THREAD)).await;
    assert!(window.took(Step::Busy));
    assert!(!window.took(Step::Clashes));
}

#[tokio::test]
async fn a_decrypted_body_reads_the_invitation_and_the_language_again() {
    let window = FakeWindow::new();
    window.run().open(row(THREAD)).await;
    window.with(|screen| screen.steps.clear());
    window
        .run()
        .engine_answered(target(None), "m1".to_string(), opened(invited()))
        .await;
    let steps = window.steps();
    assert!(steps.contains(&Step::Card));
    assert!(steps.contains(&Step::OpenInvitation));
    // The claim is made; asking the engine again would do nothing.
    assert!(!steps.contains(&Step::Engines));
    assert!(window.open(|open| open.card().is_some()).unwrap_or(false));
}

#[tokio::test]
async fn a_signature_alone_changes_nothing_else() {
    let window = FakeWindow::new();
    window.run().open(row(THREAD)).await;
    window.with(|screen| screen.steps.clear());
    let mut read = opened(body("unused"));
    read.body = None;
    window
        .run()
        .engine_answered(target(None), "m1".to_string(), read)
        .await;
    assert_eq!(window.steps(), [Step::EngineAnswered]);
}

#[tokio::test]
async fn an_engine_answer_for_a_thread_the_reader_left_goes_nowhere() {
    let window = FakeWindow::new();
    window.run().open(row(THREAD)).await;
    window.with(|screen| {
        screen.steps.clear();
        if let Some(open) = screen.open.as_mut() {
            open.thread_id = ELSEWHERE.to_string();
        }
    });
    window
        .run()
        .engine_answered(target(None), "m1".to_string(), opened(invited()))
        .await;
    assert!(window.steps().is_empty());
}

#[tokio::test]
async fn a_refresh_with_the_same_messages_redraws_only_the_buttons() {
    let window = FakeWindow::new();
    window.run().open(row(THREAD)).await;
    window.with(|screen| screen.steps.clear());
    window.run().refresh().await;
    assert_eq!(
        window.steps(),
        [Step::Messages, Step::Replace, Step::Buttons]
    );
}

#[tokio::test]
async fn a_refresh_that_finds_a_new_message_fetches_its_body() {
    let window = FakeWindow::new();
    window.run().open(row(THREAD)).await;
    window.with(|screen| {
        screen.steps.clear();
        screen.messages.push(meta("m2", true));
        screen
            .gmail
            .insert("m2".to_string(), body("Tomorrow, then"));
    });
    window.run().refresh().await;
    assert!(window.took(Step::BodiesArrived));
    assert_eq!(window.open(|open| open.bodies.len()), Some(2));
}

#[tokio::test]
async fn a_refresh_that_finds_the_thread_gone_clears_the_view() {
    let window = FakeWindow::new();
    window.run().open(row(THREAD)).await;
    window.with(|screen| screen.messages.clear());
    window.run().refresh().await;
    assert!(window.open(|_| ()).is_none());
}

#[tokio::test]
async fn a_flag_colour_read_for_a_thread_the_reader_left_goes_nowhere() {
    let window = FakeWindow::new();
    window.run().open(row(THREAD)).await;
    window.with(|screen| screen.moves_on = Some(Step::FlagColor));
    window.run().refresh_flag_color().await;
    assert!(!window.took(Step::SetFlag));
}

#[tokio::test]
async fn a_message_in_another_language_gets_the_offer() {
    let window = FakeWindow::with_body(portuguese());
    window.run().open(row(THREAD)).await;
    let last = window.0.borrow().cards.last().cloned();
    assert!(
        matches!(last, Some(Card::Offered { from: Some(from), .. }) if from.code == "pt"),
        "{last:?}"
    );
}

/// Ann writes a paragraph in Portuguese, then a short answer with no
/// words the counting knows.
fn short_answer_after_portuguese(writer: &str) -> std::rc::Rc<FakeWindow> {
    let window = FakeWindow::new();
    let mut second = meta("m2", true);
    if let Some(from) = second.from.as_mut() {
        from.email = writer.to_string();
    }
    window.with(|screen| {
        let messages = vec![meta("m1", false), second];
        screen.messages = messages.clone();
        if let Some(stored) = screen.stored.get_mut(THREAD) {
            stored.messages = messages;
        }
        screen.gmail.insert("m1".to_string(), portuguese());
        screen
            .gmail
            .insert("m2".to_string(), body("Combinado, até lá."));
    });
    window
}

#[tokio::test]
async fn a_short_answer_takes_the_language_its_writer_used_before() {
    let window = short_answer_after_portuguese("ann@example.com");
    window.run().open(row(THREAD)).await;
    let last = window.0.borrow().cards.last().cloned();
    assert!(
        matches!(last, Some(Card::Offered { from: Some(from), .. }) if from.code == "pt"),
        "{last:?}"
    );
}

#[tokio::test]
async fn a_short_answer_from_someone_else_borrows_nothing() {
    let window = short_answer_after_portuguese("bob@example.com");
    window.run().open(row(THREAD)).await;
    assert_eq!(window.0.borrow().cards.last(), Some(&Card::Hidden));
}

#[tokio::test]
async fn a_message_in_the_interface_language_gets_no_card() {
    let window = FakeWindow::new();
    window.run().open(row(THREAD)).await;
    assert_eq!(window.0.borrow().cards.last(), Some(&Card::Hidden));
}

#[tokio::test]
async fn translating_puts_the_translation_on_screen_and_turning_takes_it_off() {
    let window = FakeWindow::with_body(portuguese());
    window.run().open(row(THREAD)).await;
    window.run().translate().await;
    assert!(window.took(Step::Translated));
    let shown = || window.open(|open| open.translations["m1"].shown);
    assert_eq!(shown(), Some(true));
    window.with(|screen| screen.steps.clear());
    window.run().translate().await;
    assert_eq!(shown(), Some(false));
    window.run().translate().await;
    assert_eq!(shown(), Some(true));
    // Turning costs no second request.
    assert_eq!(window.steps(), [Step::Turn, Step::Turn]);
}

#[tokio::test]
async fn a_translation_that_failed_says_so_on_the_card_and_in_a_toast() {
    let window = FakeWindow::with_body(portuguese());
    window.with(|screen| screen.translation = Err("offline".to_string()));
    window.run().open(row(THREAD)).await;
    window.run().translate().await;
    let screen = window.0.borrow();
    assert!(matches!(screen.cards.last(), Some(Card::Problem(_))));
    assert_eq!(screen.toasts.len(), 1);
}

#[tokio::test]
async fn a_translation_for_a_thread_the_reader_left_is_only_toasted_when_it_fails() {
    let window = FakeWindow::with_body(portuguese());
    window.with(|screen| {
        screen.translation = Err("offline".to_string());
        screen.moves_on = Some(Step::Translate);
    });
    window.run().open(row(THREAD)).await;
    window.run().translate().await;
    let screen = window.0.borrow();
    assert_eq!(screen.cards.last(), Some(&Card::Working));
    assert_eq!(screen.toasts.len(), 1);
}

#[tokio::test]
async fn a_translation_for_a_thread_the_reader_left_goes_nowhere() {
    let window = FakeWindow::with_body(portuguese());
    window.with(|screen| screen.moves_on = Some(Step::Translate));
    window.run().open(row(THREAD)).await;
    window.run().translate().await;
    assert!(!window.took(Step::Translated));
}

#[tokio::test]
async fn nowhere_to_send_the_words_is_said_before_anything_goes() {
    let window = FakeWindow::with_body(portuguese());
    window.with(|screen| screen.destination = Err("Pick a model first".to_string()));
    window.run().open(row(THREAD)).await;
    window.run().translate().await;
    assert!(!window.took(Step::Translate));
    assert_eq!(window.0.borrow().toasts, ["Pick a model first"]);
}

#[test]
fn an_opened_body_leaves_the_claim_and_the_pictures_alone() {
    let stale = Stale::after(Event::EngineOpened);
    assert!(stale.translation && stale.invitation);
    assert!(!stale.protection && !stale.thumbnails && !stale.unread);
}

/// The Outbox row for the fixture's queued message.
fn queued_row() -> mailrs_domain::ThreadSummary {
    row(&mailrs_sync::outbox_row(7))
}

#[tokio::test]
async fn a_queued_message_shows_what_the_outbox_kept_without_asking_gmail() {
    let window = FakeWindow::new();
    window.with(|screen| {
        screen.queued.insert(7, queued(Some("No network")));
    });
    window.run().open(queued_row()).await;
    assert!(window.took(Step::Queued));
    for step in [
        Step::Stored,
        Step::Ensure,
        Step::Bodies,
        Step::Engines,
        Step::Card,
    ] {
        assert!(!window.took(step), "{step:?} was taken");
    }
    let stuck = window.open(|open| open.queued.as_ref().map(|q| q.stuck));
    assert_eq!(stuck, Some(Some(true)));
    // Whatever card the thread before left comes down.
    assert_eq!(window.0.borrow().invitations, [None]);
}

#[tokio::test]
async fn a_queued_message_that_has_gone_leaves_the_pane_empty() {
    let window = FakeWindow::new();
    window.run().open(queued_row()).await;
    assert!(window.took(Step::Clear));
    assert!(!window.took(Step::Show));
}

#[tokio::test]
async fn a_refresh_says_what_the_outbox_says_now() {
    let window = FakeWindow::new();
    window.with(|screen| {
        screen.queued.insert(7, queued(Some("No network")));
    });
    window.run().open(queued_row()).await;
    window.with(|screen| {
        screen.queued.insert(7, queued(Some("Gmail is busy")));
    });
    window.run().refresh().await;
    assert!(!window.took(Step::Messages));
    let line = window.open(|open| open.queued.clone().map(|q| q.line));
    assert!(
        line.flatten()
            .is_some_and(|l| l.starts_with("Gmail is busy"))
    );
}

#[tokio::test]
async fn a_queued_message_that_went_out_while_on_screen_leaves_it() {
    let window = FakeWindow::new();
    window.with(|screen| {
        screen.queued.insert(7, queued(None));
    });
    window.run().open(queued_row()).await;
    window.with(|screen| screen.queued.clear());
    window.run().refresh().await;
    assert!(window.took(Step::Clear));
    assert!(window.open(|_| ()).is_none());
}

/// The page on screen, with every patch applied.
fn page(window: &FakeWindow) -> String {
    window.page()
}

/// How many times the page was loaded whole.
fn loads(window: &FakeWindow) -> usize {
    window.0.borrow().loads.len()
}

fn patches(window: &FakeWindow) -> Vec<Vec<String>> {
    window.0.borrow().patches.clone()
}

#[tokio::test]
async fn the_stored_copy_says_a_body_is_loading_until_gmail_sends_it() {
    let window = FakeWindow::new();
    window.run().open(row(THREAD)).await;
    let first = window.0.borrow().loads.first().cloned().unwrap_or_default();
    assert!(first.contains("Loading…"));
    assert!(page(&window).contains("Hello"));
    assert!(!page(&window).contains("Loading…"));
}

/// The body replaces the article that said it was loading; the page is
/// not loaded a second time, so the reader keeps their place.
#[tokio::test]
async fn bodies_from_gmail_patch_the_articles_that_waited_for_them() {
    let window = FakeWindow::new();
    window.with(|screen| {
        let messages = vec![meta("m1", false), meta("m2", true)];
        screen.messages = messages.clone();
        if let Some(stored) = screen.stored.get_mut(THREAD) {
            stored.messages = messages;
            stored.bodies.insert("m1".to_string(), body("Kites at ten"));
        }
        screen
            .gmail
            .insert("m2".to_string(), body("Tomorrow, then"));
    });
    window.run().open(row(THREAD)).await;
    assert_eq!(loads(&window), 1);
    assert_eq!(patches(&window), [["m2"]]);
    let drawn = page(&window);
    assert!(drawn.contains("Kites at ten") && drawn.contains("Tomorrow, then"));
}

#[tokio::test]
async fn pictures_on_the_attachment_rows_patch_their_article() {
    let window = FakeWindow::with_body(with_picture());
    window.run().open(row(THREAD)).await;
    assert_eq!(loads(&window), 1);
    assert_eq!(patches(&window).last().cloned(), Some(vec!["m1".to_string()]));
    assert!(page(&window).contains("<img class=\"thumb\""), "{}", page(&window));
}

#[tokio::test]
async fn an_engine_answer_patches_its_message_alone() {
    let window = FakeWindow::new();
    window.with(|screen| {
        let messages = vec![meta("m1", false), meta("m2", false)];
        screen.messages = messages.clone();
        if let Some(stored) = screen.stored.get_mut(THREAD) {
            stored.messages = messages;
        }
        screen.gmail.insert("m2".to_string(), body("-----BEGIN PGP"));
    });
    window.run().open(row(THREAD)).await;
    let before = patches(&window).len();
    window
        .run()
        .engine_answered(target(None), "m2".to_string(), opened(body("The key is under the mat.")))
        .await;
    assert_eq!(loads(&window), 1);
    assert_eq!(patches(&window)[before..], [["m2"]]);
    assert!(page(&window).contains("The key is under the mat."));
}

#[tokio::test]
async fn a_translation_patches_its_message_and_so_does_turning_it() {
    let window = FakeWindow::with_body(portuguese());
    window.run().open(row(THREAD)).await;
    let before = patches(&window).len();
    window.run().translate().await;
    window.run().translate().await;
    assert_eq!(loads(&window), 1);
    assert_eq!(patches(&window)[before..], [["m1"], ["m1"]]);
}

/// A new message changes the count in the head and the list of articles,
/// which only a whole page can show.
#[tokio::test]
async fn a_message_new_to_the_thread_loads_the_page_again() {
    let window = FakeWindow::new();
    window.run().open(row(THREAD)).await;
    assert_eq!(loads(&window), 1);
    window.with(|screen| {
        screen.messages.push(meta("m2", true));
        screen
            .gmail
            .insert("m2".to_string(), body("Tomorrow, then"));
    });
    window.run().refresh().await;
    assert_eq!(loads(&window), 2);
    assert!(page(&window).contains("2 messages"));
}

#[tokio::test]
async fn a_body_gmail_could_not_send_says_why() {
    let window = FakeWindow::new();
    window.with(|screen| screen.gmail.clear());
    window.run().open(row(THREAD)).await;
    assert!(
        page(&window).contains("This message could not be loaded: gone"),
        "{}",
        page(&window)
    );
}

#[tokio::test]
async fn an_html_body_is_drawn_cleaned() {
    let window = FakeWindow::with_body(html_body(
        "<p>Hi Ann</p><script>steal()</script><img src=\"x\" onerror=\"steal()\">",
    ));
    window.run().open(row(THREAD)).await;
    let drawn = page(&window);
    assert!(drawn.contains("<p>Hi Ann</p>"), "{drawn}");
    assert!(!drawn.contains("steal()"), "{drawn}");
}

#[tokio::test]
async fn a_shown_translation_is_what_the_page_draws() {
    let window = FakeWindow::with_body(portuguese());
    window.run().open(row(THREAD)).await;
    assert!(page(&window).contains("Olá Ana"));
    window.run().translate().await;
    assert!(page(&window).contains("Hello Ana"), "{}", page(&window));
    assert!(!page(&window).contains("Olá Ana"));
    // Turning back draws what arrived.
    window.run().translate().await;
    assert!(page(&window).contains("Olá Ana"));
}

#[tokio::test]
async fn the_words_of_an_html_message_come_from_its_cleaned_body() {
    let window = FakeWindow::with_body(html_body(
        "<p>Olá Ana, a reunião de amanhã fica para as dez horas. Não te esqueças de \
         trazer os documentos que eu te pedi, para podermos ver tudo com calma \
         antes de falar com o banco. Um abraço e até amanhã.</p>",
    ));
    window.run().open(row(THREAD)).await;
    let last = window.0.borrow().cards.last().cloned();
    assert!(
        matches!(last, Some(Card::Offered { from: Some(from), .. }) if from.code == "pt"),
        "{last:?}"
    );
}
