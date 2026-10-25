//! The thread run, headless. Each test drives `ThreadRun` the way the
//! window does and checks what the ports were asked for.

use mailrs_domain::Target;

use super::fake::{
    ACCOUNT, ELSEWHERE, FakeWindow, Step, THREAD, body, html_body, ics, invited, loose, loose_with,
    meta, opened_cancellation, opened_invitation, opened_occurrence, opened_publish, personal,
    portuguese, queued, row, spot, strip, with_inline_picture, with_picture,
};
use super::{Card, Event, ReceiptCard, Stale};
use crate::open_thread::Served;
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
        revocation_unchecked: false,
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
        .open(|open| open.bodies["m1"].clone().ok()?.text.clone())
        .flatten();
    assert_eq!(text.as_deref(), Some("Hello"));
}

/// The person left this sender's list before, so the thread opens with no
/// Unsubscribe banner to offer.
#[tokio::test]
async fn a_thread_from_a_list_already_left_opens_unsubscribed() {
    let window = FakeWindow::new();
    window.with(|screen| {
        if let Some(stored) = screen.stored.get_mut(THREAD) {
            stored.left = true;
        }
    });
    window.run().open(row(THREAD)).await;
    assert_eq!(window.open(|open| open.unsubscribed), Some(true));
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
    assert!(screen.steps.contains(&Step::OfferCalendarAccess));
    assert!(screen.steps.contains(&Step::Clashes));
}

/// The card sits inside the message that carries the invitation, between
/// its header and its body, even when a later reply is the newest message.
#[tokio::test]
async fn an_invitation_s_card_sits_in_the_message_that_carries_it() {
    let window = FakeWindow::with_body(invited());
    window.with(|screen| {
        let thread = vec![meta("m1", true), meta("m2", true)];
        if let Some(stored) = screen.stored.get_mut(THREAD) {
            stored.messages = thread.clone();
        }
        screen.messages = thread;
        screen.gmail.insert("m2".to_string(), body("Count me in."));
    });
    window.run().open(row(THREAD)).await;
    let screen = window.0.borrow();
    let page = screen.document.as_ref().expect("a page").html("");
    assert_eq!(page.matches("class=\"event-slot\"").count(), 1);
    let slot = page.find("class=\"event-slot\"").expect("the card's place");
    let first = page.find("id=\"m-m1\"").expect("the invitation");
    let second = page.find("id=\"m-m2\"").expect("the reply");
    assert!(
        first < slot && slot < second,
        "the place is in the message with the invitation"
    );
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
async fn a_published_file_offers_the_calendars_to_add_it_to() {
    let window = FakeWindow::with_body(invited());
    window.with(|screen| screen.invitation = Ok(Some(opened_publish())));
    window.run().open(row(THREAD)).await;
    assert_eq!(
        window.0.borrow().targets_shown,
        [("kites@example.com".to_string(), vec![personal()])]
    );
}

#[tokio::test]
async fn a_request_asks_for_no_calendars_to_add_to() {
    let window = FakeWindow::with_body(invited());
    window.run().open(row(THREAD)).await;
    assert!(!window.took(Step::AddTargets));
}

#[tokio::test]
async fn an_account_with_no_calendar_leaves_the_card_to_hand_the_file_to_the_desktop() {
    for targets in [Ok(Vec::new()), Err("offline".to_string())] {
        let window = FakeWindow::with_body(invited());
        window.with(|screen| {
            screen.invitation = Ok(Some(opened_publish()));
            screen.add_targets = targets;
        });
        window.run().open(row(THREAD)).await;
        assert!(window.took(Step::AddTargets));
        assert!(!window.took(Step::AddTargetsKnown));
    }
}

#[tokio::test]
async fn calendars_for_a_thread_the_reader_left_go_nowhere() {
    let window = FakeWindow::with_body(invited());
    window.with(|screen| {
        screen.invitation = Ok(Some(opened_publish()));
        screen.moves_on = Some(Step::AddTargets);
    });
    window.run().open(row(THREAD)).await;
    assert!(!window.took(Step::AddTargetsKnown));
}

#[tokio::test]
async fn an_invitation_on_the_calendar_offers_to_show_it() {
    let window = FakeWindow::with_body(invited());
    window.with(|screen| screen.on_calendar = Ok(Some(spot())));
    window.run().open(row(THREAD)).await;
    assert_eq!(
        window.0.borrow().spots,
        [("kites@example.com".to_string(), spot())]
    );
}

#[tokio::test]
async fn an_invitation_the_calendar_lacks_offers_nothing() {
    for found in [Ok(None), Err("the store is busy".to_string())] {
        let window = FakeWindow::with_body(invited());
        window.with(|screen| screen.on_calendar = found);
        window.run().open(row(THREAD)).await;
        assert!(window.took(Step::OnCalendar));
        assert!(!window.took(Step::OnCalendarKnown));
        assert!(
            window.took(Step::Clashes),
            "the clashes still go on the card"
        );
    }
}

#[tokio::test]
async fn a_spot_for_a_thread_the_reader_left_goes_nowhere() {
    let window = FakeWindow::with_body(invited());
    window.with(|screen| {
        screen.on_calendar = Ok(Some(spot()));
        screen.moves_on = Some(Step::OnCalendar);
    });
    window.run().open(row(THREAD)).await;
    assert!(window.took(Step::OnCalendar));
    assert!(!window.took(Step::OnCalendarKnown));
}

#[tokio::test]
async fn an_answered_invitation_still_offers_to_show_it() {
    let window = FakeWindow::with_body(invited());
    window.with(|screen| {
        screen.invitation = Ok(Some(mailrs_sync::Opened {
            answer: Some(mailrs_domain::invitation::Answer::Yes),
            ..opened_invitation()
        }));
        screen.on_calendar = Ok(Some(spot()));
    });
    window.run().open(row(THREAD)).await;
    assert!(window.took(Step::OnCalendarKnown));
    assert!(
        !window.took(Step::Busy),
        "an answered invitation asks about no clashes"
    );
}

#[tokio::test]
async fn an_answered_invitation_still_offers_calendar_access() {
    // Grant Access lets the event show in the Calendar space, which is
    // worth as much after an answer as before it.
    let window = FakeWindow::with_body(invited());
    window.with(|screen| {
        screen.invitation = Ok(Some(mailrs_sync::Opened {
            answer: Some(mailrs_domain::invitation::Answer::Yes),
            ..opened_invitation()
        }));
    });
    window.run().open(row(THREAD)).await;
    assert!(window.took(Step::OfferCalendarAccess));
}

#[tokio::test]
async fn a_cancellation_offers_no_calendar_access() {
    let window = FakeWindow::with_body(invited());
    window.with(|screen| screen.invitation = Ok(Some(opened_cancellation())));
    window.run().open(row(THREAD)).await;
    assert!(!window.took(Step::OfferCalendarAccess));
}

#[tokio::test]
async fn a_card_the_calendar_lacked_looks_again_once_the_calendar_is_read() {
    // Opened before the copy's first read: the event was not there yet.
    let window = FakeWindow::with_body(invited());
    window.with(|screen| screen.on_calendar = Ok(None));
    window.run().open(row(THREAD)).await;
    assert!(window.0.borrow().spots.is_empty());
    window.with(|screen| screen.on_calendar = Ok(Some(spot())));
    window.run().calendar_read().await;
    assert_eq!(
        window.0.borrow().spots,
        [("kites@example.com".to_string(), spot())]
    );
}

#[tokio::test]
async fn a_card_that_shows_in_calendar_asks_nothing_when_the_calendar_is_read() {
    let window = FakeWindow::with_body(invited());
    window.with(|screen| screen.on_calendar = Ok(Some(spot())));
    window.run().open(row(THREAD)).await;
    window.with(|screen| screen.steps.clear());
    window.run().calendar_read().await;
    assert!(!window.took(Step::OnCalendar));
}

#[tokio::test]
async fn a_thread_without_an_invitation_asks_nothing_when_the_calendar_is_read() {
    let window = FakeWindow::new();
    window.run().open(row(THREAD)).await;
    window.run().calendar_read().await;
    assert!(!window.took(Step::OnCalendar));
}

#[tokio::test]
async fn a_cancellation_looks_for_nothing_on_the_calendar() {
    let window = FakeWindow::with_body(invited());
    window.with(|screen| screen.invitation = Ok(Some(opened_cancellation())));
    window.run().open(row(THREAD)).await;
    assert!(!window.took(Step::OnCalendar));
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
async fn a_language_the_reader_never_translates_gets_no_card() {
    let window = FakeWindow::with_body(portuguese());
    window.with(|screen| screen.never = vec!["es".to_string(), "pt".to_string()]);
    window.run().open(row(THREAD)).await;
    assert_eq!(window.0.borrow().cards.last(), Some(&Card::Hidden));
}

#[tokio::test]
async fn russian_and_czech_can_be_excluded_from_translation() {
    for (code, text) in [
        (
            "ru",
            "Спасибо за ваше письмо. Мы уже получили все документы и будем рады ответить на ваши вопросы. Если вы хотите, мы можем обсудить это завтра.",
        ),
        (
            "cs",
            "Děkuji za váš e-mail. Jsme rádi, že jste nám napsali. Prosím, pošlete nám dokumenty, které máme pro vás připravit, a můžeme se na to podívat.",
        ),
    ] {
        let window = FakeWindow::with_body(body(text));
        window.with(|screen| screen.never = vec![code.to_string()]);
        window.run().open(row(THREAD)).await;
        assert_eq!(
            window.0.borrow().cards.last(),
            Some(&Card::Hidden),
            "{code}"
        );
        window.with(|screen| screen.never.clear());
        window.run().offer_translation_again();
        let last = window.0.borrow().cards.last().cloned();
        assert!(
            matches!(last, Some(Card::Offered { from: Some(from), .. }) if from.code == code),
            "{code}: {last:?}"
        );
    }
}

#[tokio::test]
async fn taking_a_language_off_the_list_offers_its_card_again() {
    let window = FakeWindow::with_body(portuguese());
    window.with(|screen| screen.never = vec!["pt".to_string()]);
    window.run().open(row(THREAD)).await;
    window.with(|screen| screen.never.clear());
    window.run().offer_translation_again();
    let last = window.0.borrow().cards.last().cloned();
    assert!(
        matches!(last, Some(Card::Offered { from: Some(from), .. }) if from.code == "pt"),
        "{last:?}"
    );
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
    assert_eq!(
        patches(&window).last().cloned(),
        Some(vec!["m1".to_string()])
    );
    assert!(
        page(&window).contains("<img class=\"thumb\""),
        "{}",
        page(&window)
    );
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
        screen
            .gmail
            .insert("m2".to_string(), body("-----BEGIN PGP"));
    });
    window.run().open(row(THREAD)).await;
    let before = patches(&window).len();
    window
        .run()
        .engine_answered(
            target(None),
            "m2".to_string(),
            opened(body("The key is under the mat.")),
        )
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

/// The text goes on screen first. The pictures it names by `cid:` come
/// after, served to the page from the thread itself, so their arrival
/// changes no article and loads nothing again.
#[tokio::test]
async fn inline_pictures_arrive_after_the_text_and_change_no_article() {
    let window = FakeWindow::with_body(with_inline_picture());
    window.run().open(row(THREAD)).await;
    let steps = window.steps();
    let at = |step| steps.iter().position(|s| *s == step).expect("step taken");
    assert!(at(Step::BodiesArrived) < at(Step::Images));
    assert!(at(Step::Images) < at(Step::ImagesArrived));
    assert_eq!(loads(&window), 1);
    assert_eq!(
        patches(&window),
        [["m1"]],
        "the body, and nothing for the picture"
    );
    assert!(
        page(&window).contains("src=\"mailrs-cid:1/m1/0/logo@kites\""),
        "{}",
        page(&window)
    );
    let served = window.open(|open| open.picture("m1", 0, "logo@kites"));
    assert!(matches!(served, Some(Served::Ready(picture)) if *picture.bytes == [1, 2, 3]));
}

/// A thread read before comes out of the store whole, and its pictures
/// were never part of it.
#[tokio::test]
async fn a_stored_thread_fetches_its_inline_pictures_too() {
    let window = FakeWindow::new();
    window.with(|screen| {
        if let Some(stored) = screen.stored.get_mut(THREAD) {
            stored
                .bodies
                .insert("m1".to_string(), with_inline_picture());
        }
    });
    window.run().open(row(THREAD)).await;
    assert!(!window.took(Step::Bodies));
    assert!(window.took(Step::ImagesArrived));
}

#[tokio::test]
async fn inline_pictures_for_a_thread_the_reader_left_go_nowhere() {
    let window = FakeWindow::with_body(with_inline_picture());
    window.with(|screen| screen.moves_on = Some(Step::Images));
    window.run().open(row(THREAD)).await;
    assert!(window.took(Step::Images));
    assert!(!window.took(Step::ImagesArrived));
}

/// A body with no picture to name asks for none.
#[tokio::test]
async fn a_body_that_names_no_picture_fetches_none() {
    let window = FakeWindow::new();
    window.run().open(row(THREAD)).await;
    assert!(!window.took(Step::Images));
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

/// Every cleaning goes through the port, which does it on another thread
/// than the one that draws.
fn cleaned_away(window: &FakeWindow) -> bool {
    let here = std::thread::current().id();
    let cleaners = window.0.borrow().cleaners.clone();
    !cleaners.is_empty() && cleaners.iter().all(|worker| *worker != here)
}

#[tokio::test]
async fn gmail_s_html_is_cleaned_through_the_port_before_it_is_drawn() {
    let window = FakeWindow::with_body(html_body("<p>Hi Ann</p>"));
    window.run().open(row(THREAD)).await;
    let steps = window.steps();
    let at = |step| steps.iter().position(|s| *s == step).expect("step taken");
    assert!(at(Step::Bodies) < at(Step::Clean) && at(Step::Clean) < at(Step::BodiesArrived));
    assert!(cleaned_away(&window));
    assert_eq!(
        patches(&window),
        [["m1"]],
        "loading, then the body in one patch"
    );
    assert!(page(&window).contains("<p>Hi Ann</p>"));
}

/// The stored copy goes on screen whole, its HTML already cleaned.
#[tokio::test]
async fn the_stored_copy_is_cleaned_before_it_goes_on_screen() {
    let window = FakeWindow::new();
    window.with(|screen| {
        screen.messages = vec![meta("m1", false)];
        if let Some(stored) = screen.stored.get_mut(THREAD) {
            stored.messages = vec![meta("m1", false)];
            stored
                .bodies
                .insert("m1".to_string(), html_body("<p>Kites at ten</p>"));
        }
    });
    window.run().open(row(THREAD)).await;
    let steps = window.steps();
    let at = |step| steps.iter().position(|s| *s == step).expect("step taken");
    assert!(at(Step::Clean) < at(Step::Show));
    assert!(cleaned_away(&window));
    let first = window.0.borrow().loads.first().cloned().unwrap_or_default();
    assert!(first.contains("<p>Kites at ten</p>"), "{first}");
}

/// A body an engine opened says it is loading until the port has cleaned
/// it, and nothing cleans it on the way to the page.
#[tokio::test]
async fn an_opened_html_body_is_cleaned_through_the_port() {
    let window = FakeWindow::new();
    window.run().open(row(THREAD)).await;
    let before = window.0.borrow().cleaners.len();
    let before_patches = patches(&window).len();
    window
        .run()
        .engine_answered(
            target(None),
            "m1".to_string(),
            opened(html_body("<p>Under the mat</p>")),
        )
        .await;
    assert_eq!(window.0.borrow().cleaners.len(), before + 1);
    assert!(window.took(Step::Cleaned));
    assert!(cleaned_away(&window));
    assert_eq!(patches(&window)[before_patches..], [["m1"], ["m1"]]);
    assert!(page(&window).contains("<p>Under the mat</p>"));
}

/// A body the cleaner fails on ends in a fallback line, not in "Loading…"
/// for good, and is not sent out to be cleaned again.
#[tokio::test]
async fn a_body_that_fails_to_clean_ends_in_the_fallback_not_in_loading() {
    let window = FakeWindow::with_body(html_body("<p>Hi Ann</p>"));
    window.with(|screen| screen.clean_fails = true);
    window.run().open(row(THREAD)).await;
    let drawn = page(&window);
    assert!(
        drawn.contains("This message could not be displayed"),
        "{drawn}"
    );
    assert!(!drawn.contains("Loading…"), "{drawn}");
    let asked = window
        .steps()
        .iter()
        .filter(|step| **step == Step::Clean)
        .count();
    assert_eq!(asked, 1, "cleaned once");
}

/// A model's HTML is mail like any other: cleaned through the port.
#[tokio::test]
async fn a_translated_html_message_is_cleaned_through_the_port() {
    let window = FakeWindow::with_body(html_body(
        "<p>Olá Ana, a reunião de amanhã fica para as dez horas. Não te esqueças de \
         trazer os documentos que eu te pedi, para podermos ver tudo com calma \
         antes de falar com o banco. Um abraço e até amanhã.</p>",
    ));
    window.run().open(row(THREAD)).await;
    let before = window.0.borrow().cleaners.len();
    window.run().translate().await;
    assert_eq!(window.0.borrow().cleaners.len(), before + 1);
    assert!(cleaned_away(&window));
    assert!(page(&window).contains("Hello Ana"), "{}", page(&window));
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

#[tokio::test]
async fn a_synced_calendar_draws_the_hours_around_the_invitation() {
    let window = FakeWindow::with_body(invited());
    window.with(|screen| screen.strip = Ok(Some(strip())));
    window.run().open(row(THREAD)).await;
    assert_eq!(
        window.0.borrow().strips,
        [("kites@example.com".to_string(), strip())]
    );
    assert!(
        !window.took(Step::Busy),
        "the strip already says what else is on"
    );
}

#[tokio::test]
async fn without_a_synced_calendar_the_clash_line_stays() {
    for found in [Ok(None), Err("the store is busy".to_string())] {
        let window = FakeWindow::with_body(invited());
        window.with(|screen| screen.strip = found);
        window.run().open(row(THREAD)).await;
        assert!(window.took(Step::Strip));
        assert!(!window.took(Step::StripKnown));
        assert!(window.took(Step::Busy));
    }
}

#[tokio::test]
async fn an_answered_invitation_still_draws_its_hours() {
    let window = FakeWindow::with_body(invited());
    window.with(|screen| {
        screen.invitation = Ok(Some(mailrs_sync::Opened {
            answer: Some(mailrs_domain::invitation::Answer::Yes),
            ..opened_invitation()
        }));
        screen.strip = Ok(Some(strip()));
    });
    window.run().open(row(THREAD)).await;
    assert!(window.took(Step::StripKnown));
    assert!(!window.took(Step::Busy));
}

#[tokio::test]
async fn a_cancellation_draws_no_hours() {
    let window = FakeWindow::with_body(invited());
    window.with(|screen| screen.invitation = Ok(Some(opened_cancellation())));
    window.run().open(row(THREAD)).await;
    assert!(!window.took(Step::Strip));
}

#[tokio::test]
async fn the_hours_follow_the_occurrence_show_in_calendar_opens() {
    let window = FakeWindow::with_body(invited());
    window.with(|screen| screen.on_calendar = Ok(Some(spot())));
    window.run().open(row(THREAD)).await;
    assert_eq!(window.0.borrow().strip_asked, [Some(spot().start)]);

    let window = FakeWindow::with_body(invited());
    window.run().open(row(THREAD)).await;
    assert_eq!(
        window.0.borrow().strip_asked,
        [None],
        "without a spot, the invitation's own start"
    );
}

#[tokio::test]
async fn hours_for_a_thread_the_reader_left_go_nowhere() {
    let window = FakeWindow::with_body(invited());
    window.with(|screen| {
        screen.strip = Ok(Some(strip()));
        screen.moves_on = Some(Step::Strip);
    });
    window.run().open(row(THREAD)).await;
    assert!(window.took(Step::Strip));
    assert!(!window.took(Step::StripKnown));
}

#[tokio::test]
async fn ai_language_changes_translation_without_changing_the_interface() {
    for language in ["Russian", "Czech", "Slovak"] {
        let window = FakeWindow::with_body(body(
            "Please read the attached document and let us know if you have any questions. We will send you the details tomorrow. Thank you for your help.",
        ));
        window.with(|screen| screen.language = Some(language.into()));
        window.run().open(row(THREAD)).await;
        assert!(matches!(
            window.0.borrow().cards.last(),
            Some(Card::Offered { .. })
        ));
        window.run().translate().await;
        assert_eq!(window.0.borrow().requested_languages, [language]);
        assert_eq!(
            window.open(|open| open.translations["m1"].into.clone()),
            Some(language.into())
        );
    }
}

#[tokio::test]
async fn changing_the_ai_language_discards_only_old_translations() {
    let window = FakeWindow::with_body(portuguese());
    window.run().open(row(THREAD)).await;
    window.run().translate().await;
    window.run().offer_translation_again();
    assert_eq!(window.open(|open| open.translations.len()), Some(1));
    window.with(|screen| screen.language = Some("Czech".into()));
    window.run().offer_translation_again();
    assert_eq!(window.open(|open| open.translations.len()), Some(0));
    assert!(page(&window).contains("Olá Ana"));
    assert!(!page(&window).contains("Hello Ana"));
    assert!(matches!(
        window.0.borrow().cards.last(),
        Some(Card::Offered { .. })
    ));
    window.run().translate().await;
    assert_eq!(window.0.borrow().requested_languages, ["English", "Czech"]);
}

#[tokio::test]
async fn changing_language_during_translation_discards_the_old_answer_and_error() {
    for reply in [
        Ok(vec![Some("Old translation".into())]),
        Err("offline".into()),
    ] {
        let window = FakeWindow::with_body(portuguese());
        window.with(|screen| {
            screen.languages_during_translation = vec!["Russian".into()];
            screen.translation = reply;
        });
        window.run().open(row(THREAD)).await;
        window.run().translate().await;
        assert!(!window.took(Step::Translated));
        assert!(window.0.borrow().toasts.is_empty());
        assert_eq!(window.open(|open| open.translations.len()), Some(0));
    }
}

#[tokio::test]
async fn returning_to_the_original_language_drops_the_in_flight_answer_and_error() {
    for reply in [
        Ok(vec![Some("Old translation".into())]),
        Err("offline".into()),
    ] {
        let window = FakeWindow::with_body(portuguese());
        window.with(|screen| {
            screen.languages_during_translation = vec!["Czech".into(), "English".into()];
            screen.translation = reply;
        });
        window.run().open(row(THREAD)).await;
        window.run().translate().await;
        assert!(!window.took(Step::Translated));
        assert!(window.0.borrow().toasts.is_empty());
        assert_eq!(window.open(|open| open.translations.len()), Some(0));
    }
}

/// Ann's message, asking for a read receipt at her own address.
fn asking() -> mailrs_domain::MessageBody {
    mailrs_domain::MessageBody {
        receipt_to: Some("ann@example.com".into()),
        ..body("The rota is up.")
    }
}

#[tokio::test]
async fn a_message_that_asks_puts_the_card_up() {
    let window = FakeWindow::with_body(asking());
    window.run().open(row(THREAD)).await;
    assert_eq!(
        window.with(|s| s.receipt_cards.clone()),
        [ReceiptCard::Asked {
            message_id: "m1".into(),
            to: "ann@example.com".into()
        }]
    );
    assert!(
        window.page().contains("mailrs:receipt/send"),
        "{}",
        window.page()
    );
}

#[tokio::test]
async fn an_answered_receipt_is_not_asked_again() {
    let window = FakeWindow::with_body(asking());
    window.with(|s| s.receipt_answered = Ok(true));
    window.run().open(row(THREAD)).await;
    assert!(window.with(|s| s.receipt_cards.is_empty()));
}

#[tokio::test]
async fn a_message_marked_mdnsent_elsewhere_asks_nothing() {
    let window = FakeWindow::with_body(asking());
    window.with(|s| {
        for meta in s
            .messages
            .iter_mut()
            .chain(s.stored.get_mut(THREAD).unwrap().messages.iter_mut())
        {
            meta.held
                .keywords
                .push(mailrs_domain::mailbox::keyword::MDN_SENT.to_string());
        }
    });
    window.run().open(row(THREAD)).await;
    assert!(!window.took(Step::ReceiptAnswered));
}

#[tokio::test]
async fn never_ask_puts_nothing_up() {
    let window = FakeWindow::with_body(asking());
    window.with(|s| s.offers_receipts = false);
    window.run().open(row(THREAD)).await;
    assert!(
        !window.took(Step::ReceiptAnswered),
        "the store is not even asked"
    );
    assert!(
        window.page().contains("Read receipt requested"),
        "the message still says it asked"
    );
}

#[tokio::test]
async fn a_message_from_me_asks_nothing() {
    let window = FakeWindow::with_body(mailrs_domain::MessageBody {
        receipt_to: Some("me@example.com".into()),
        ..body("Note to self")
    });
    window.with(|s| {
        for meta in s
            .messages
            .iter_mut()
            .chain(s.stored.get_mut(THREAD).unwrap().messages.iter_mut())
        {
            meta.from = Some(mailrs_domain::Address {
                name: None,
                email: "Me@Example.com".into(),
            });
        }
    });
    window.run().open(row(THREAD)).await;
    assert!(window.with(|s| s.receipt_cards.is_empty()));
    assert!(!window.page().contains("Read receipt requested"));
}

/// RFC 8098: a receipt to someone other than the sender tells a stranger
/// the mail was opened, so the app does not offer one.
#[tokio::test]
async fn a_receipt_asked_for_another_address_is_not_offered() {
    let window = FakeWindow::with_body(mailrs_domain::MessageBody {
        receipt_to: Some("tracker@elsewhere.example".into()),
        ..body("The rota is up.")
    });
    window.run().open(row(THREAD)).await;
    assert!(window.with(|s| s.receipt_cards.is_empty()));
    assert!(!window.took(Step::ReceiptAnswered));
    assert!(!window.page().contains("Read receipt requested"));
}

#[tokio::test]
async fn the_sender_s_address_matches_in_any_case() {
    let window = FakeWindow::with_body(mailrs_domain::MessageBody {
        receipt_to: Some("ANN@example.com".into()),
        ..body("The rota is up.")
    });
    window.run().open(row(THREAD)).await;
    assert_eq!(window.with(|s| s.receipt_cards.len()), 1);
}

#[tokio::test]
async fn sending_takes_the_card_down_and_says_so() {
    let window = FakeWindow::with_body(asking());
    let run = window.run();
    run.open(row(THREAD)).await;
    run.answer_receipt("m1".into(), true).await;
    assert_eq!(
        window.with(|s| s.receipt_answers.clone()),
        [("m1".to_string(), true)]
    );
    assert_eq!(
        window.with(|s| s.receipt_cards.last().cloned()),
        Some(ReceiptCard::Hidden)
    );
    assert!(window.with(|s| s.toasts.iter().any(|t| t == "Read receipt sent")));
    assert!(!window.page().contains("mailrs:receipt/send"));
    assert!(window.page().contains("Read receipt requested"));
}

#[tokio::test]
async fn declining_takes_the_card_down_quietly() {
    let window = FakeWindow::with_body(asking());
    let run = window.run();
    run.open(row(THREAD)).await;
    run.answer_receipt("m1".into(), false).await;
    assert_eq!(
        window.with(|s| s.receipt_answers.clone()),
        [("m1".to_string(), false)]
    );
    assert_eq!(
        window.with(|s| s.receipt_cards.last().cloned()),
        Some(ReceiptCard::Hidden)
    );
    assert!(window.with(|s| s.toasts.is_empty()));
}

#[tokio::test]
async fn the_buttons_wait_while_the_answer_goes_out() {
    let window = FakeWindow::with_body(asking());
    let run = window.run();
    run.open(row(THREAD)).await;
    run.answer_receipt("m1".into(), true).await;
    let cards = window.with(|s| s.receipt_cards.clone());
    assert_eq!(cards[cards.len() - 2], ReceiptCard::Working);
}

#[tokio::test]
async fn a_failed_send_puts_the_question_back() {
    let window = FakeWindow::with_body(asking());
    window.with(|s| s.receipt_answer = Err("offline".into()));
    let run = window.run();
    run.open(row(THREAD)).await;
    run.answer_receipt("m1".into(), true).await;
    assert!(matches!(
        window.with(|s| s.receipt_cards.last().cloned()),
        Some(ReceiptCard::Asked { .. })
    ));
    assert!(window.with(|s| {
        s.toasts
            .iter()
            .any(|t| t == "The receipt was not sent, and you can try again: offline")
    }));
}

/// A send that may have gone keeps its answer, so it is not tried again,
/// and the card does not offer it.
#[tokio::test]
async fn a_failed_send_already_recorded_takes_the_card_down() {
    let window = FakeWindow::with_body(asking());
    window.with(|s| s.receipt_answer = Err("offline".into()));
    let run = window.run();
    run.open(row(THREAD)).await;
    window.with(|s| s.receipt_answered = Ok(true));
    run.answer_receipt("m1".into(), true).await;
    assert_eq!(
        window.with(|s| s.receipt_cards.last().cloned()),
        Some(ReceiptCard::Hidden)
    );
    assert!(
        window.with(|s| s.toasts.iter().any(|t| t
            == "The receipt may have gone out, so Penguin Mail will not send it again: offline")),
        "{:?}",
        window.with(|s| s.toasts.clone())
    );
}

/// A read report for mail the person sent: Original-Message-ID names a
/// message in their Sent mail.
fn report(original: &str) -> mailrs_domain::MessageBody {
    mailrs_domain::MessageBody {
        read_report: Some(mailrs_domain::ReadReport {
            by: "ann@example.com".into(),
            displayed: true,
            original: Some(original.into()),
        }),
        ..body("Your message was shown.")
    }
}

#[tokio::test]
async fn a_report_for_mail_i_sent_shows_as_one_line() {
    let window = FakeWindow::with_body(report("kites@example.com"));
    window.with(|s| {
        s.sent
            .push(("kites@example.com".into(), vec!["Ann@Example.com".into()]))
    });
    window.run().open(row(THREAD)).await;
    let page = window.page();
    assert!(page.contains("ann@example.com</strong>"), "{page}");
    assert!(!page.contains("Your message was shown."), "{page}");
}

/// Anyone can send a report naming anyone. One that names no message the
/// person sent says nothing about who read what.
#[tokio::test]
async fn a_forged_report_shows_no_read_line() {
    let window = FakeWindow::with_body(report("made-up@example.com"));
    window.run().open(row(THREAD)).await;
    let page = window.page();
    assert!(window.took(Step::SentCopy));
    assert!(!page.contains("Read by"), "{page}");
    assert!(page.contains("Your message was shown."), "{page}");
}

/// A real Message-ID from Sent, in a report from someone the message never
/// went to: the report says nothing about who read it.
#[tokio::test]
async fn a_report_from_someone_the_message_did_not_go_to_shows_no_line() {
    let window = FakeWindow::with_body(report("kites@example.com"));
    window.with(|s| {
        s.sent
            .push(("kites@example.com".into(), vec!["bob@example.com".into()]))
    });
    window.run().open(row(THREAD)).await;
    let page = window.page();
    assert!(!page.contains("Read by"), "{page}");
    assert!(page.contains("Your message was shown."), "{page}");
}

#[tokio::test]
async fn a_press_while_the_answer_goes_out_does_nothing() {
    use super::Effects;
    let window = FakeWindow::with_body(asking());
    let run = window.run();
    run.open(row(THREAD)).await;
    window.receipt_card(ReceiptCard::Working);
    run.answer_receipt("m1".into(), true).await;
    assert!(!window.took(Step::AnswerReceipt));
}

/// The engine opening a body redraws the receipt card. One whose answer is
/// on its way keeps its buttons waiting.
#[tokio::test]
async fn a_redraw_while_the_answer_goes_out_keeps_the_buttons_waiting() {
    use super::Effects;
    let window = FakeWindow::with_body(asking());
    let run = window.run();
    run.open(row(THREAD)).await;
    window.receipt_card(ReceiptCard::Working);
    run.engine_answered(target(None), "m1".into(), opened(asking()))
        .await;
    assert_eq!(
        window.with(|s| s.receipt_cards.last().cloned()),
        Some(ReceiptCard::Working)
    );
    assert!(
        !window.page().contains("mailrs:receipt/"),
        "{}",
        window.page()
    );
}

/// A redraw that asked the store before the answer was kept hears "not
/// answered" after the receipt went. The card stays down, so a second
/// press cannot say a receipt was sent when none was.
#[tokio::test]
async fn a_stale_redraw_does_not_bring_the_card_back_after_sending() {
    let window = FakeWindow::with_body(asking());
    let run = window.run();
    run.open(row(THREAD)).await;
    let (release, held) = futures::channel::oneshot::channel();
    window.with(|s| s.receipt_hold = Some(held));
    futures::join!(run.account_ready(ACCOUNT), async {
        run.answer_receipt("m1".into(), true).await;
        let _ = release.send(());
    });
    assert_eq!(
        window.with(|s| s.receipt_cards.last().cloned()),
        Some(ReceiptCard::Hidden)
    );
    assert!(
        !window.page().contains("mailrs:receipt/send"),
        "{}",
        window.page()
    );
}

/// An account still connecting cannot say whether the receipt was
/// answered, so the card waits for it and comes up once it can.
#[tokio::test]
async fn the_card_comes_up_once_the_account_connects() {
    let window = FakeWindow::with_body(asking());
    window.with(|s| s.receipt_answered = Ok(true));
    let run = window.run();
    run.open(row(THREAD)).await;
    assert!(window.with(|s| s.receipt_cards.is_empty()));
    window.with(|s| s.receipt_answered = Ok(false));
    run.account_ready(ACCOUNT + 1).await;
    assert!(
        window.with(|s| s.receipt_cards.is_empty()),
        "another account changes nothing"
    );
    run.account_ready(ACCOUNT).await;
    assert_eq!(window.with(|s| s.receipt_cards.len()), 1);
}

#[tokio::test]
async fn a_loose_message_shows_from_its_bytes_without_asking_the_store_or_gmail() {
    let window = FakeWindow::new();
    window.run().open_loose(loose()).await;
    // Cleaning happens before Show, as `open` does it, so there is no
    // Cleaned step.
    for step in [Step::Clean, Step::Show, Step::Card, Step::Engines] {
        assert!(window.took(step), "{step:?} not taken");
    }
    for step in [
        Step::Stored,
        Step::Ensure,
        Step::Messages,
        Step::Bodies,
        Step::Images,
        Step::Thumbnails,
        Step::Sleep,
        Step::MarkRead,
        Step::ReceiptAnswered,
    ] {
        assert!(!window.took(step), "{step:?} was taken");
    }
    assert_eq!(window.open(|open| open.is_loose()), Some(true));
    // The picture came from the bytes, so the page does not wait for it.
    let id = window.open(|open| open.messages[0].id.clone()).unwrap();
    assert_eq!(
        window.open(|open| open.inline_images.contains_key(&id)),
        Some(true)
    );
}

#[tokio::test]
async fn a_loose_message_naming_a_picture_it_lacks_settles_without_a_fetch() {
    let raw = b"From: a@example.com\r\nSubject: x\r\nContent-Type: text/html\r\n\r\n<img src=\"cid:nowhere\">\r\n".to_vec();
    let window = FakeWindow::new();
    window.run().open_loose(loose_with(raw)).await;
    assert!(!window.took(Step::Images));
    let id = window.open(|open| open.messages[0].id.clone()).unwrap();
    assert_eq!(
        window.open(|open| open.inline_images.get(&id).map(|p| p.len())),
        Some(Some(0))
    );
}

#[tokio::test]
async fn a_refresh_leaves_a_loose_message_as_it_is() {
    let window = FakeWindow::new();
    window.run().open_loose(loose()).await;
    window.run().refresh().await;
    window.run().refresh_flag_color().await;
    assert!(!window.took(Step::Messages));
    assert!(!window.took(Step::Clear));
    assert!(!window.took(Step::FlagColor));
    assert_eq!(window.open(|open| open.messages.len()), Some(1));
}

#[tokio::test]
async fn an_invitation_in_a_loose_message_shows_as_an_event_to_keep() {
    let raw = format!(
        "From: ann@example.com\r\nSubject: Kites\r\nContent-Type: text/calendar; method=REQUEST\r\n\r\n{}",
        ics()
    )
    .into_bytes();
    let window = FakeWindow::new();
    window.run().open_loose(loose_with(raw)).await;
    assert!(
        !window.took(Step::OpenInvitation),
        "nothing is remembered in the store"
    );
    let shown = window
        .0
        .borrow()
        .event_cards
        .last()
        .cloned()
        .expect("a card");
    assert_eq!(
        shown.invitation.method,
        mailrs_domain::invitation::Method::Publish
    );
    assert!(shown.answer.is_none());
}

/// A loose message whose sender asked for a receipt, as a file.
fn loose_asking() -> crate::open_thread::Loose {
    loose_with(
        b"From: Ann <ann@example.com>\r\nTo: me@example.com\r\nSubject: Kites\r\nDisposition-Notification-To: ann@example.com\r\n\r\nhi\r\n"
            .to_vec(),
    )
}

/// A receipt would mail about a message in no mailbox and remember the
/// answer under an id the store never saw, so the card stays down even
/// when the account connects after the window opened.
#[tokio::test]
async fn a_loose_message_gets_no_receipt_card_when_its_account_connects() {
    let window = FakeWindow::new();
    window.with(|s| s.receipt_answered = Ok(false));
    let run = window.run();
    run.open_loose(loose_asking()).await;
    run.account_ready(ACCOUNT).await;
    assert!(!window.took(Step::ReceiptAnswered));
    assert!(window.with(|s| s.receipt_cards.is_empty()));
}

#[tokio::test]
async fn the_engine_opening_a_loose_message_brings_no_receipt_card() {
    let window = FakeWindow::new();
    window.with(|s| s.receipt_answered = Ok(false));
    let run = window.run();
    run.open_loose(loose_asking()).await;
    let (target, id) = window
        .open(|open| (open.target(), open.messages[0].id.clone()))
        .unwrap();
    let mut inside = body("hi");
    inside.receipt_to = Some("ann@example.com".into());
    run.engine_answered(target, id, opened(inside)).await;
    assert!(!window.took(Step::ReceiptAnswered));
    assert!(window.with(|s| s.receipt_cards.is_empty()));
}

/// Parsing a file near the size ceiling takes most of a second, so it
/// goes through the effects, away from the window, before the thread
/// is built; a later open still wins over it.
#[tokio::test]
async fn a_loose_message_is_parsed_away_from_the_window_before_it_shows() {
    let window = FakeWindow::new();
    window.run().open_loose(loose()).await;
    let steps = window.steps();
    let at = |step| steps.iter().position(|s| *s == step).expect("step taken");
    assert!(at(Step::Parse) < at(Step::Show));
}
