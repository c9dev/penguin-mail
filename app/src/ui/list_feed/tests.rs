use mailrs_sync::Listing;

use super::{Coalesce, Landed, ListFeed, Refresh, Splice, Ticket};

type Feed = ListFeed<&'static str>;

fn page(more: bool) -> Result<Listing, ()> {
    Ok(Listing {
        more,
        ..Listing::default()
    })
}

fn named(id: &str) -> (i64, String) {
    (1, id.to_owned())
}

/// A feed with its first page on screen and more rows behind it.
fn listed() -> (Feed, Ticket) {
    let mut feed = Feed::default();
    let ticket = feed.shown();
    assert!(feed.first_page(ticket, &page(true)).is_some());
    (feed, ticket)
}

#[test]
fn several_store_changes_make_one_splice() {
    let (mut feed, ticket) = listed();
    assert_eq!(feed.changed(vec![named("a")]), Coalesce::Arm);
    assert_eq!(feed.changed(vec![named("b")]), Coalesce::Joined);
    assert_eq!(feed.changed(vec![named("c")]), Coalesce::Joined);
    assert_eq!(
        feed.fire(),
        Refresh::Splice(ticket, vec![named("a"), named("b"), named("c")])
    );
    // The next change starts a timer of its own.
    assert_eq!(feed.changed(vec![named("d")]), Coalesce::Arm);
}

#[test]
fn a_request_for_everything_wins_over_a_splice() {
    let (mut feed, _) = listed();
    feed.changed(vec![named("a")]);
    assert_eq!(feed.everything(), Coalesce::Joined);
    feed.changed(vec![named("b")]);
    assert_eq!(feed.fire(), Refresh::Reload);
    // The threads went with that reload and do not come back later.
    feed.changed(vec![named("c")]);
    assert_eq!(feed.fire(), Refresh::Splice(Ticket(1), vec![named("c")]));
}

#[test]
fn a_change_that_names_no_threads_reloads() {
    let (mut feed, _) = listed();
    feed.changed(vec![named("a")]);
    feed.changed(Vec::new());
    assert_eq!(feed.fire(), Refresh::Reload);
}

#[test]
fn a_listing_for_the_mailbox_before_is_dropped() {
    let mut feed = Feed::default();
    let inbox = feed.shown();
    let sent = feed.shown();
    assert_eq!(feed.first_page(inbox, &page(true)), None);
    assert_eq!(
        feed.first_page(sent, &page(false)),
        Some(Landed { reveal: None })
    );
    // The inbox said more rows follow; the sent mail, on screen, did not.
    assert_eq!(feed.scrolled_to_end(), None);
}

#[test]
fn an_error_for_the_mailbox_on_screen_still_counts() {
    let mut feed = Feed::default();
    let ticket = feed.shown();
    assert!(
        feed.first_page(ticket, &Err::<Listing, _>("offline"))
            .is_some()
    );
}

#[test]
fn a_splice_for_the_mailbox_before_is_dropped() {
    let (mut feed, _) = listed();
    feed.changed(vec![named("a")]);
    let Refresh::Splice(ticket, _) = feed.fire() else {
        panic!("expected a splice");
    };
    feed.shown();
    assert_eq!(feed.spliced(ticket, Some("rows"), false), Splice::Stale);
}

#[test]
fn a_remote_folder_prunes_instead_of_splicing() {
    let (feed, ticket) = listed();
    assert_eq!(feed.spliced::<&str>(ticket, None, true), Splice::Prune);
    assert_eq!(feed.spliced::<&str>(ticket, None, false), Splice::Reload);
    assert_eq!(
        feed.spliced(ticket, Some("rows"), true),
        Splice::Put("rows")
    );
}

#[test]
fn loading_more_does_not_fire_twice_while_a_page_is_on_its_way() {
    let (mut feed, ticket) = listed();
    assert_eq!(feed.scrolled_to_end(), Some(ticket));
    assert_eq!(feed.scrolled_to_end(), None);
    assert!(feed.next_page(ticket, &page(true)));
    assert_eq!(feed.scrolled_to_end(), Some(ticket));
}

#[test]
fn a_failed_page_can_be_asked_for_again() {
    let (mut feed, ticket) = listed();
    feed.scrolled_to_end();
    assert!(feed.next_page(ticket, &Err::<Listing, _>("offline")));
    assert_eq!(feed.scrolled_to_end(), Some(ticket));
}

#[test]
fn no_page_follows_the_last() {
    let (mut feed, ticket) = listed();
    feed.scrolled_to_end();
    assert!(feed.next_page(ticket, &page(false)));
    assert_eq!(feed.scrolled_to_end(), None);
}

#[test]
fn no_page_follows_before_the_first_lands() {
    let (mut feed, _) = listed();
    let ticket = feed.reload();
    assert_eq!(feed.scrolled_to_end(), None);
    feed.first_page(ticket, &page(true));
    assert_eq!(feed.scrolled_to_end(), Some(ticket));
}

#[test]
fn a_page_for_the_list_before_is_dropped() {
    let (mut feed, old) = listed();
    feed.scrolled_to_end();
    let new = feed.reload();
    assert!(!feed.next_page(old, &page(true)));
    feed.first_page(new, &page(true));
    // The reload cleared the old page's claim, so scrolling works again.
    assert_eq!(feed.scrolled_to_end(), Some(new));
}

#[test]
fn reveal_selects_after_the_first_page() {
    let mut feed = Feed::default();
    let ticket = feed.shown();
    assert_eq!(feed.reveal("thread"), None);
    assert_eq!(
        feed.first_page(ticket, &page(false)),
        Some(Landed {
            reveal: Some("thread")
        })
    );
    // It selects once, not on every page after.
    let again = feed.reload();
    assert_eq!(
        feed.first_page(again, &page(false)),
        Some(Landed { reveal: None })
    );
}

#[test]
fn reveal_waits_through_a_stale_page() {
    let mut feed = Feed::default();
    let first = feed.shown();
    feed.reveal("thread");
    let second = feed.reload();
    assert_eq!(feed.first_page(first, &page(false)), None);
    assert_eq!(
        feed.first_page(second, &page(false)),
        Some(Landed {
            reveal: Some("thread")
        })
    );
}

#[test]
fn reveal_selects_now_when_the_rows_are_there() {
    let (mut feed, _) = listed();
    assert_eq!(feed.reveal("thread"), Some("thread"));
}

#[test]
fn showing_another_mailbox_drops_a_waiting_reveal() {
    let mut feed = Feed::default();
    feed.shown();
    feed.reveal("thread");
    let other = feed.shown();
    assert_eq!(
        feed.first_page(other, &page(false)),
        Some(Landed { reveal: None })
    );
}

#[test]
fn reveal_waits_for_the_first_mailbox_the_window_lists() {
    let mut feed = Feed::default();
    assert_eq!(feed.reveal("thread"), None);
    let ticket = feed.reload();
    assert_eq!(
        feed.first_page(ticket, &page(false)),
        Some(Landed {
            reveal: Some("thread")
        })
    );
}
