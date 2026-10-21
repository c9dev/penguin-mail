//! The listing run, headless. Each test drives `ListingRun` the way the
//! window does and checks what the list shows and what was asked for.

use std::collections::HashSet;
use std::sync::Arc;

use futures::{pin_mut, poll};
use mailrs_domain::{Folder, Target};
use mailrs_sync::{BackendError, Listing, PAGE, SyncError};

use super::fake::{Call, Fake, row};
use crate::ui::window::Reveal;
use crate::ui::window::on_screen::OnScreen;
use crate::ui::{Mailbox, Standard};

fn inbox() -> Mailbox {
    Mailbox::Unified(Standard::Inbox)
}

fn label(id: &str) -> Mailbox {
    Mailbox::Label {
        account_id: 1,
        label_id: id.into(),
        name: id.into(),
    }
}

fn trash() -> Mailbox {
    Mailbox::Folder {
        account_id: Some(1),
        folder: Folder::Trash,
    }
}

fn show(mailbox: Mailbox) -> impl FnOnce(&mut OnScreen) -> Option<super::Redraw> {
    move |screen| Some(screen.show(mailbox, false))
}

/// A server search the reader typed over is stopped, and what it had
/// found by then never reaches the list.
#[tokio::test]
async fn a_search_stopped_mid_answer_never_lands() {
    let (fake, run) = Fake::new();
    run.change(show(inbox()));
    fake.settle().await;
    fake.during(Call::ServerSearch, |run| run.typed("invoices"));
    run.typed("invoice");
    fake.settle().await;
    let stops = fake.with(|s| s.stops.clone());
    assert_eq!(stops.len(), 2, "each search asked the servers");
    assert!(stops[0].stopped(), "the key stopped the first search");
    assert!(!stops[1].stopped());
    let pages = fake.with(|s| s.pages.clone());
    assert!(
        !pages.iter().flatten().any(|id| id == "invoice-server"),
        "the stopped answer stayed off the list: {pages:?}"
    );
    assert_eq!(fake.ids(), ["invoices-server"]);
}

/// A notification opens a thread while another mailbox is on screen: the
/// inbox comes on screen and the thread is selected once its rows land.
#[tokio::test]
async fn a_reveal_selects_after_the_first_page_lands() {
    let (fake, run) = Fake::new();
    fake.with(|s| s.store = vec![row("t1", 1)]);
    run.change(show(trash()));
    fake.settle().await;
    run.reveal((1, "t1".into(), Reveal::Read));
    assert_eq!(
        fake.calls(Call::Select),
        0,
        "the inbox's rows are not there yet"
    );
    fake.settle().await;
    assert_eq!(run.mailbox(), inbox());
    let calls = fake.with(|s| s.calls.clone());
    let last_rows = calls.iter().rposition(|c| *c == Call::SetRows).unwrap();
    let select = calls.iter().position(|c| *c == Call::Select).unwrap();
    assert!(last_rows < select, "selected after the rows landed");
    assert_eq!(
        fake.with(|s| s.selected.clone()),
        [(1, "t1".into(), Reveal::Read)]
    );
}

#[tokio::test]
async fn a_reveal_in_the_inbox_on_screen_selects_at_once() {
    let (fake, run) = Fake::new();
    run.change(show(inbox()));
    fake.settle().await;
    run.reveal((1, "t1".into(), Reveal::Reply));
    assert_eq!(fake.calls(Call::Select), 1);
    assert_eq!(fake.calls(Call::List), 1, "nothing listed again");
}

/// A change event in a folder the servers list: the rows that left it go,
/// and the rest stay.
#[tokio::test]
async fn a_folder_prunes_the_threads_that_left_it() {
    let (fake, run) = Fake::new();
    fake.with(|s| {
        s.store = vec![row("t1", 1), row("t2", 2)];
        s.gone = vec![Target::thread(1, "t2")];
    });
    run.change(show(trash()));
    fake.settle().await;
    assert_eq!(fake.ids(), ["t1", "t2"]);
    run.changed(vec![(1, "t1".into()), (1, "t2".into())]);
    fake.settle().await;
    assert_eq!(fake.calls(Call::GoneFrom), 1);
    assert_eq!(fake.ids(), ["t1"]);
    assert_eq!(fake.calls(Call::List), 1, "the folder was not listed again");
}

/// An answer for a mailbox the reader left while it was out goes nowhere.
#[tokio::test]
async fn a_late_answer_for_a_mailbox_no_longer_shown_is_dropped() {
    let (fake, run) = Fake::new();
    fake.during(Call::List, |run| run.change(show(label("B"))));
    run.change(show(label("A")));
    fake.settle().await;
    assert_eq!(run.mailbox(), label("B"));
    assert_eq!(fake.with(|s| s.pages.clone()), [["B-1"]]);
}

/// The answer to a prune that comes back after the folder left the screen
/// takes no rows from the mailbox shown now.
#[tokio::test]
async fn a_late_prune_leaves_the_next_mailbox_alone() {
    let (fake, run) = Fake::new();
    fake.with(|s| {
        s.store = vec![row("t1", 1)];
        s.gone = vec![Target::thread(1, "t1")];
    });
    run.change(show(trash()));
    fake.settle().await;
    fake.during(Call::GoneFrom, |run| run.change(show(inbox())));
    run.prune(vec![Target::thread(1, "t1")]);
    fake.settle().await;
    assert_eq!(fake.calls(Call::DropRows), 0);
    assert_eq!(fake.ids(), ["t1"]);
}

/// The list was first listed while an account's first sync had stored
/// only a few threads. Older mail that arrives afterwards still reaches
/// it, a page at a time as the reader scrolls.
#[tokio::test]
async fn mail_that_arrives_after_a_short_first_page_loads_as_the_reader_scrolls() {
    let (fake, run) = Fake::new();
    let newest: Vec<_> = (0..20).map(|n| row(&format!("n{n}"), n)).collect();
    fake.with(|s| s.store = newest.clone());
    run.change(show(inbox()));
    fake.settle().await;
    assert_eq!(fake.ids().len(), 20);
    run.scrolled_to_end();
    fake.settle().await;
    assert_eq!(fake.calls(Call::Append), 0, "the store held no more yet");

    let older: Vec<_> = (0..600).map(|n| row(&format!("o{n}"), 100 + n)).collect();
    fake.with(|s| s.store.extend(older.iter().cloned()));
    run.changed(older.iter().map(|r| (1, r.id.clone())).collect());
    fake.settle().await;
    assert_eq!(fake.ids().len(), PAGE, "a page's worth went in");

    run.scrolled_to_end();
    fake.settle().await;
    assert_eq!(fake.with(|s| s.held.clone()), [PAGE]);
    assert_eq!(fake.ids().len(), 620, "the rest came with the next page");
}

#[tokio::test]
async fn a_burst_of_change_events_reads_the_store_once() {
    let (fake, run) = Fake::new();
    fake.with(|s| s.store = vec![row("t1", 1), row("t2", 2)]);
    run.change(show(inbox()));
    fake.settle().await;
    run.changed(vec![(1, "t1".into())]);
    run.changed(vec![(1, "t2".into())]);
    fake.settle().await;
    assert_eq!(fake.calls(Call::Changed), 1);
    assert_eq!(fake.calls(Call::Refreshing), 1);
}

/// A folder put on screen shows the spinner while the servers answer; the
/// same folder listed again after a change keeps its rows up.
#[tokio::test]
async fn a_reload_keeps_the_rows_where_a_new_mailbox_spins() {
    let (fake, run) = Fake::new();
    run.change(show(trash()));
    assert_eq!(fake.calls(Call::Loading), 1);
    fake.settle().await;
    run.reload_remote();
    fake.settle().await;
    assert_eq!(fake.calls(Call::Loading), 1);
    assert_eq!(fake.calls(Call::List), 2);
}

/// A successful follow supersedes its queued first read. Later reloads
/// and visits reuse the completed follow.
#[tokio::test]
async fn a_label_is_followed_once() {
    let (fake, run) = Fake::new();
    run.change(show(label("Work")));
    fake.settle().await;
    assert_eq!(
        fake.calls(Call::List),
        1,
        "the superseded first read never starts"
    );
    for _ in 0..3 {
        run.reload();
        fake.settle().await;
    }
    run.change(show(inbox()));
    fake.settle().await;
    run.change(show(label("Work")));
    fake.settle().await;
    assert_eq!(fake.with(|s| s.followed.clone()), [(1, "Work".to_string())]);
    assert_eq!(fake.calls(Call::List), 6);
}

#[tokio::test]
async fn a_removed_folder_name_can_be_followed_again_in_the_same_account() {
    let (fake, run) = Fake::new();
    run.change(show(label("Work")));
    fake.settle().await;
    // A refreshed label list observes deletion or rename before that
    // server name is created again under the same running account.
    run.mailboxes_live(&HashSet::from([(1, "Renamed".to_string())]));
    run.change(show(inbox()));
    fake.settle().await;
    run.change(show(label("Work")));
    fake.settle().await;
    assert_eq!(
        fake.with(|s| s.followed.clone()),
        [(1, "Work".to_string()), (1, "Work".to_string())]
    );
}

#[tokio::test]
async fn follows_are_separate_for_each_mailbox_and_account() {
    let (fake, run) = Fake::new();
    for mailbox in [
        label("Work"),
        label("Travel"),
        Mailbox::Label {
            account_id: 2,
            label_id: "Work".into(),
            name: "Work".into(),
        },
    ] {
        run.change(show(mailbox));
        fake.settle().await;
    }
    assert_eq!(
        fake.with(|s| s.followed.clone()),
        [(1, "Work".into()), (1, "Travel".into()), (2, "Work".into()),]
    );
}

#[tokio::test]
async fn reloads_and_revisits_join_a_pending_follow() {
    let (fake, run) = Fake::new();
    let followed = fake.hold(Call::Follow);
    run.change(show(label("Work")));
    let settled = fake.settle();
    pin_mut!(settled);
    assert!(poll!(&mut settled).is_pending());
    run.reload();
    run.change(show(inbox()));
    run.change(show(label("Work")));
    assert!(poll!(&mut settled).is_pending());
    assert_eq!(fake.calls(Call::Follow), 1);
    followed.send(()).unwrap();
    settled.await;
    assert_eq!(fake.ids(), ["Work-1"]);
    assert_eq!(fake.calls(Call::Follow), 1);
    assert_eq!(fake.calls(Call::List), 3, "queued stale pages never start");
}

#[tokio::test]
async fn an_offline_follow_reports_failure_and_a_local_reload_retries() {
    let (fake, run) = Fake::new();
    fake.with(|s| {
        s.labels.insert("Work".into(), Vec::new());
        s.follow_answers
            .push_back(Err(SyncError::Backend(BackendError::Offline(
                "test offline".into(),
            ))
            .into()));
    });
    let followed = fake.hold(Call::Follow);
    run.change(show(label("Work")));
    let settled = fake.settle();
    pin_mut!(settled);
    assert!(poll!(&mut settled).is_pending());
    assert!(fake.ids().is_empty(), "the first local page was empty");
    followed.send(()).unwrap();
    settled.await;
    assert_eq!(fake.calls(Call::Failed), 1, "not silently No Mail");
    assert!(fake.with(|s| s.failures[0].contains("test offline")));
    assert_eq!(fake.calls(Call::Follow), 1, "failure does not loop");
    assert_eq!(fake.calls(Call::List), 1, "failure does not reload itself");

    fake.with(|s| {
        s.labels.insert("Work".into(), vec![row("arrived", 0)]);
    });
    // Check for Mail now uses this same reload for local mailboxes too.
    assert!(!run.mailbox().is_remote());
    run.reload();
    fake.settle().await;
    assert_eq!(fake.calls(Call::Follow), 2);
    assert_eq!(fake.ids(), ["arrived"]);
    run.reload();
    fake.settle().await;
    assert_eq!(fake.calls(Call::Follow), 2, "success is remembered");
}

#[tokio::test]
async fn a_failed_follow_can_retry_on_a_later_visit() {
    let (fake, run) = Fake::new();
    fake.with(|s| {
        s.follow_answers
            .push_back(Err(anyhow::anyhow!("test failure")))
    });
    run.change(show(label("Work")));
    fake.settle().await;
    run.change(show(inbox()));
    fake.settle().await;
    run.change(show(label("Work")));
    fake.settle().await;
    assert_eq!(fake.calls(Call::Failed), 1);
    assert_eq!(fake.calls(Call::Follow), 2);
}

#[tokio::test]
async fn a_late_successful_follow_does_not_reload_another_mailbox() {
    let (fake, run) = Fake::new();
    fake.with(|s| s.store = vec![row("inbox", 0)]);
    let followed = fake.hold(Call::Follow);
    run.change(show(label("Work")));
    let settled = fake.settle();
    pin_mut!(settled);
    assert!(poll!(&mut settled).is_pending());
    run.change(show(inbox()));
    assert!(poll!(&mut settled).is_pending());
    let listed = fake.calls(Call::List);
    followed.send(()).unwrap();
    settled.await;
    assert_eq!(
        fake.calls(Call::List),
        listed,
        "no reload of the new mailbox"
    );
    assert_eq!(fake.ids(), ["inbox"]);
    run.change(show(label("Work")));
    fake.settle().await;
    assert_eq!(
        fake.calls(Call::Follow),
        1,
        "off-screen success is remembered"
    );
}

#[tokio::test]
async fn a_late_failed_follow_does_not_report_against_another_mailbox() {
    let (fake, run) = Fake::new();
    fake.with(|s| {
        s.follow_answers
            .push_back(Err(anyhow::anyhow!("test failure")))
    });
    let followed = fake.hold(Call::Follow);
    run.change(show(label("Work")));
    let settled = fake.settle();
    pin_mut!(settled);
    assert!(poll!(&mut settled).is_pending());
    run.change(show(inbox()));
    assert!(poll!(&mut settled).is_pending());
    followed.send(()).unwrap();
    settled.await;
    assert_eq!(fake.calls(Call::Failed), 0);
    assert_eq!(fake.calls(Call::List), 2);
    run.change(show(label("Work")));
    fake.settle().await;
    assert_eq!(
        fake.calls(Call::Follow),
        2,
        "off-screen failure permits retry"
    );
}

/// The first page captured an empty store. A change event splices the
/// fetched row before that page returns; follow success invalidates it.
#[tokio::test]
async fn follow_success_keeps_a_late_empty_first_page_from_wiping_fetched_rows() {
    let (fake, run) = Fake::new();
    fake.with(|s| {
        s.labels.insert("Work".into(), Vec::new());
    });
    let first = fake.hold(Call::List);
    let followed = fake.hold(Call::Follow);
    run.change(show(label("Work")));
    let settled = fake.settle();
    pin_mut!(settled);
    assert!(poll!(&mut settled).is_pending());
    fake.with(|s| {
        let arrived = row("arrived", 0);
        s.store = vec![arrived.clone()];
        s.labels.insert("Work".into(), vec![arrived]);
    });
    run.changed(vec![(1, "arrived".into())]);
    assert!(poll!(&mut settled).is_pending());
    assert_eq!(fake.calls(Call::Replace), 1);
    assert_eq!(fake.ids(), ["arrived"]);
    followed.send(()).unwrap();
    assert!(poll!(&mut settled).is_pending());
    assert_eq!(fake.with(|s| s.pages.clone()), [["arrived"]]);
    first.send(()).unwrap();
    settled.await;
    assert_eq!(fake.ids(), ["arrived"]);
    assert_eq!(
        fake.calls(Call::SetRows),
        1,
        "the late empty page was stale"
    );
    assert_eq!(fake.calls(Call::Follow), 1);
}

#[tokio::test]
async fn a_missing_account_fails_and_can_retry_after_it_connects() {
    let (fake, run) = Fake::new();
    fake.with(|s| {
        s.accounts.remove(&1);
    });
    run.change(show(label("Work")));
    fake.settle().await;
    assert_eq!(fake.calls(Call::Failed), 1);
    assert_eq!(fake.calls(Call::List), 1, "missing is not success");
    fake.with(|s| {
        s.accounts.insert(1, Arc::new(()));
    });
    run.reload();
    fake.settle().await;
    assert_eq!(fake.calls(Call::Follow), 2);
    assert_eq!(fake.calls(Call::List), 2);
    assert_eq!(fake.ids(), ["Work-1"]);
}

#[tokio::test]
async fn removing_an_account_discards_its_pending_follow() {
    let (fake, run) = Fake::new();
    let followed = fake.hold(Call::Follow);
    run.change(show(label("Work")));
    let settled = fake.settle();
    pin_mut!(settled);
    assert!(poll!(&mut settled).is_pending());
    fake.with(|s| {
        s.accounts.remove(&1);
    });
    run.accounts_live(&HashSet::new());
    run.change(|screen| screen.accounts_read(false));
    assert!(poll!(&mut settled).is_pending());
    followed.send(()).unwrap();
    settled.await;
    assert_eq!(run.mailbox(), inbox());
    assert_eq!(fake.calls(Call::List), 2);
    assert_eq!(fake.calls(Call::Failed), 0);
    fake.with(|s| {
        s.accounts.insert(1, Arc::new(()));
    });
    run.change(show(label("Work")));
    fake.settle().await;
    assert_eq!(fake.calls(Call::Follow), 2);
}

#[tokio::test]
async fn a_reconnected_account_does_not_inherit_a_successful_follow() {
    let (fake, run) = Fake::new();
    run.change(show(label("Work")));
    fake.settle().await;
    fake.with(|s| {
        s.accounts.insert(1, Arc::new(()));
    });
    run.accounts_live(&HashSet::from([1]));
    run.reload();
    fake.settle().await;
    assert_eq!(fake.calls(Call::Follow), 2);
    run.reload();
    fake.settle().await;
    assert_eq!(fake.calls(Call::Follow), 2);
}

#[tokio::test]
async fn reconnecting_while_a_follow_is_pending_discards_its_success() {
    let (fake, run) = Fake::new();
    let followed = fake.hold(Call::Follow);
    run.change(show(label("Work")));
    let settled = fake.settle();
    pin_mut!(settled);
    assert!(poll!(&mut settled).is_pending());
    fake.with(|s| {
        s.accounts.insert(1, Arc::new(()));
    });
    followed.send(()).unwrap();
    settled.await;
    assert_eq!(
        fake.calls(Call::List),
        1,
        "old account cannot reload the screen"
    );
    assert_eq!(fake.calls(Call::Failed), 0);
    run.reload();
    fake.settle().await;
    assert_eq!(
        fake.calls(Call::Follow),
        2,
        "old success was not remembered"
    );
}

#[tokio::test]
async fn an_old_follow_cannot_clear_a_reconnected_accounts_pending_follow() {
    let (fake, run) = Fake::new();
    let old = fake.hold(Call::Follow);
    run.change(show(label("Work")));
    let settled = fake.settle();
    pin_mut!(settled);
    assert!(poll!(&mut settled).is_pending());
    fake.with(|s| {
        s.accounts.insert(1, Arc::new(()));
    });
    let new = fake.hold(Call::Follow);
    run.reload();
    assert!(poll!(&mut settled).is_pending());
    old.send(()).unwrap();
    assert!(poll!(&mut settled).is_pending());
    run.reload();
    assert!(poll!(&mut settled).is_pending());
    assert_eq!(
        fake.calls(Call::Follow),
        2,
        "the new pending follow still coalesces"
    );
    new.send(()).unwrap();
    settled.await;
    assert_eq!(fake.calls(Call::Follow), 2);
    assert_eq!(fake.calls(Call::List), 4);
}

#[tokio::test]
async fn a_queued_follow_does_not_start_on_a_replacement_account() {
    let (fake, run) = Fake::new();
    run.change(show(label("Work")));
    fake.with(|s| {
        s.accounts.insert(1, Arc::new(()));
    });
    fake.settle().await;
    assert_eq!(fake.calls(Call::Follow), 0);
    assert_eq!(fake.calls(Call::List), 1);
    assert_eq!(fake.calls(Call::Failed), 0);
    run.reload();
    fake.settle().await;
    assert_eq!(fake.calls(Call::Follow), 1);
}

fn prefix(rows: Vec<mailrs_domain::ThreadSummary>, more: bool) -> anyhow::Result<Listing> {
    Ok(Listing {
        rows,
        more,
        replace: true,
        ..Listing::default()
    })
}

#[tokio::test]
async fn a_server_page_updates_earlier_conversations_instead_of_appending_duplicates() {
    let (fake, run) = Fake::new();
    fake.with(|s| s.remote_labels = true);
    let mut joined = row("thread", 1);
    joined.message_count = 2;
    fake.with(|s| {
        s.remote_labels = true;
        s.answers.extend([
            prefix(vec![row("thread", 1)], true),
            prefix(vec![joined.clone(), row("older", 2)], false),
        ]);
    });
    run.change(show(label("Work/Clients")));
    fake.settle().await;
    run.scrolled_to_end();
    fake.settle().await;
    assert_eq!(fake.ids(), ["thread", "older"]);
    assert_eq!(fake.with(|s| s.rows[0].message_count), 2);
    assert_eq!(fake.calls(Call::Append), 0);
    assert_eq!(fake.calls(Call::SetRows), 2);
    run.scrolled_to_end();
    fake.settle().await;
    assert_eq!(fake.calls(Call::List), 2, "the server cursor ended");
}

#[tokio::test]
async fn a_late_server_prefix_cannot_replace_another_mailbox() {
    let (fake, run) = Fake::new();
    fake.with(|s| s.remote_labels = true);
    fake.with(|s| {
        s.answers.extend([
            prefix(vec![row("first", 1)], true),
            prefix(vec![row("stale", 2)], false),
        ])
    });
    run.change(show(label("Work/Clients")));
    fake.settle().await;
    fake.during(Call::List, |run| run.change(show(label("Work"))));
    run.scrolled_to_end();
    fake.settle().await;
    assert_eq!(fake.ids(), ["Work-1"]);
    assert!(!fake.with(|s| s.pages.iter().flatten().any(|id| id == "stale")));
}

#[tokio::test]
async fn a_failed_server_page_keeps_rows_and_allows_the_same_scroll_to_retry() {
    let (fake, run) = Fake::new();
    fake.with(|s| s.remote_labels = true);
    fake.with(|s| {
        s.answers.extend([
            prefix(vec![row("first", 1)], true),
            Err(anyhow::anyhow!("offline")),
            prefix(vec![row("first", 1), row("older", 2)], false),
        ])
    });
    run.change(show(label("Work/Clients")));
    fake.settle().await;
    run.scrolled_to_end();
    fake.settle().await;
    assert_eq!(fake.ids(), ["first"]);
    assert_eq!(fake.calls(Call::Failed), 1);
    run.scrolled_to_end();
    fake.settle().await;
    assert_eq!(fake.ids(), ["first", "older"]);
    assert_eq!(fake.with(|s| s.held.clone()), [1, 1]);
}

#[tokio::test]
async fn server_mailbox_changes_reload_while_other_labels_still_splice_locally() {
    for remote in [true, false] {
        let (fake, run) = Fake::new();
        fake.with(|s| s.remote_labels = true);
        fake.with(|s| s.remote_labels = remote);
        run.change(show(label("Work/Clients")));
        fake.settle().await;
        let before = fake.calls(Call::List);
        run.changed(vec![(1, "thread".into())]);
        fake.settle().await;
        assert_eq!(fake.calls(Call::List), before + usize::from(remote));
        assert_eq!(fake.calls(Call::Replace), usize::from(!remote));
        run.reload_remote();
        fake.settle().await;
        assert_eq!(fake.calls(Call::List), before + 2 * usize::from(remote));
    }
}

#[tokio::test]
async fn a_queued_first_page_for_a_previous_mailbox_never_starts() {
    let (fake, run) = Fake::new();
    run.change(show(Mailbox::Folder {
        account_id: Some(1),
        folder: Folder::Archive,
    }));
    run.change(show(Mailbox::Folder {
        account_id: Some(1),
        folder: Folder::Junk,
    }));
    fake.settle().await;
    assert_eq!(fake.calls(Call::List), 1);
}

#[tokio::test]
async fn a_queued_append_for_a_previous_mailbox_never_starts() {
    let (fake, run) = Fake::new();
    fake.with(|s| {
        s.store = (0..PAGE + 2)
            .map(|n| row(&format!("row{n}"), n as i64))
            .collect()
    });
    run.change(show(Mailbox::Folder {
        account_id: Some(1),
        folder: Folder::Archive,
    }));
    fake.settle().await;
    run.scrolled_to_end();
    run.change(show(Mailbox::Folder {
        account_id: Some(1),
        folder: Folder::Junk,
    }));
    fake.settle().await;
    assert_eq!(fake.calls(Call::List), 2);
    assert_eq!(fake.calls(Call::Append), 0);
}

#[tokio::test]
async fn previews_fill_existing_rows_without_relisting_or_selecting() {
    let (fake, run) = Fake::new();
    fake.with(|s| s.store = vec![row("first", 1), row("second", 2)]);
    run.change(show(inbox()));
    fake.settle().await;
    run.previews(vec![Target::thread(1, "first")]);
    fake.settle().await;
    assert_eq!(fake.ids(), ["first", "second"]);
    assert_eq!(fake.with(|s| s.rows[0].snippet.clone()), "Preview");
    assert!(fake.with(|s| s.rows[1].snippet.is_empty()));
    assert_eq!(fake.calls(Call::List), 1);
    assert_eq!(fake.calls(Call::SetRows), 1);
    assert_eq!(fake.calls(Call::Select), 0);
}

#[tokio::test]
async fn previews_replace_the_queue_and_never_fetch_two_rows_together() {
    let (fake, run) = Fake::new();
    run.change(show(inbox()));
    fake.settle().await;
    let first = Target::thread(1, "first");
    let old = Target::thread(1, "old");
    let new = Target::thread(1, "new");
    let answer = fake.hold(Call::Preview);
    run.previews(vec![first.clone(), old]);
    let settling = fake.settle();
    pin_mut!(settling);
    assert!(poll!(settling.as_mut()).is_pending());
    run.previews(vec![first.clone(), new.clone(), new.clone()]);
    assert!(poll!(settling.as_mut()).is_pending());
    assert_eq!(
        fake.with(|s| s.previews.clone()),
        std::slice::from_ref(&first)
    );
    answer.send(()).unwrap();
    settling.await;
    assert_eq!(
        fake.with(|s| s.previews.clone()),
        [first.clone(), new.clone()]
    );
    run.previews(vec![first, new]);
    fake.settle().await;
    assert_eq!(fake.calls(Call::Preview), 2);
}

#[tokio::test]
async fn disabling_previews_drops_the_answer_and_queued_work() {
    let (fake, run) = Fake::new();
    run.change(show(inbox()));
    fake.settle().await;
    fake.during(Call::Preview, |run| run.previews(Vec::new()));
    run.previews(vec![
        Target::thread(1, "first"),
        Target::thread(1, "second"),
    ]);
    fake.settle().await;
    assert_eq!(fake.calls(Call::Preview), 1);
    assert_eq!(fake.calls(Call::PreviewArrived), 0);
    assert!(run.state.borrow().previews.attempted.is_empty());
}

#[tokio::test]
async fn a_preview_from_the_previous_listing_never_lands() {
    for reload in [false, true] {
        let (fake, run) = Fake::new();
        run.change(show(inbox()));
        fake.settle().await;
        fake.during(Call::Preview, move |run| {
            if reload {
                run.reload();
            } else {
                run.change(show(trash()));
            }
        });
        run.previews(vec![Target::thread(1, "same"), Target::thread(1, "queued")]);
        fake.settle().await;
        assert_eq!(fake.calls(Call::Preview), 1);
        assert_eq!(fake.calls(Call::PreviewArrived), 0);
        run.previews(vec![Target::thread(1, "same")]);
        fake.settle().await;
        assert_eq!(fake.calls(Call::Preview), 2);
        assert_eq!(fake.calls(Call::PreviewArrived), 1);
    }
}

#[tokio::test]
async fn a_preview_from_the_stored_search_cannot_reach_the_server_page() {
    let (fake, run) = Fake::new();
    let server = fake.hold(Call::ServerSearch);
    run.search("invoice".into());
    let settling = fake.settle();
    pin_mut!(settling);
    assert!(poll!(settling.as_mut()).is_pending());
    let preview = fake.hold(Call::Preview);
    run.previews(vec![Target::thread(1, "invoice-stored")]);
    assert!(poll!(settling.as_mut()).is_pending());
    server.send(()).unwrap();
    assert!(poll!(settling.as_mut()).is_pending());
    preview.send(()).unwrap();
    settling.await;
    assert_eq!(fake.ids(), ["invoice-server"]);
    assert_eq!(fake.calls(Call::PreviewArrived), 0);
}

#[tokio::test]
async fn unavailable_previews_do_not_loop_and_attempts_only_keep_the_viewport() {
    let (fake, run) = Fake::new();
    run.change(show(inbox()));
    fake.settle().await;
    fake.with(|s| {
        s.preview_answers.extend([
            Ok(None),
            Ok(Some(String::new())),
            Err(anyhow::anyhow!("offline")),
        ])
    });
    let targets: Vec<_> = (0..3).map(|n| Target::thread(1, n.to_string())).collect();
    run.previews(targets.clone());
    fake.settle().await;
    run.previews(targets);
    fake.settle().await;
    assert_eq!(fake.calls(Call::Preview), 3);
    assert_eq!(fake.calls(Call::Failed), 0);
    for n in 3..100 {
        run.previews(vec![Target::thread(1, n.to_string())]);
        fake.settle().await;
        assert_eq!(run.state.borrow().previews.attempted.len(), 1);
    }
}

#[tokio::test]
async fn a_new_message_in_a_visible_thread_gets_its_own_preview() {
    let (fake, run) = Fake::new();
    fake.with(|s| {
        s.store = vec![row("same", 1)];
        s.preview_answers.extend([
            Ok(Some("Old message".into())),
            Ok(Some("New message".into())),
        ]);
    });
    run.change(show(inbox()));
    fake.settle().await;
    let answer = fake.hold(Call::Preview);
    let target = Target::thread(1, "same");
    run.previews(vec![target.clone()]);
    let settling = fake.settle();
    pin_mut!(settling);
    assert!(poll!(settling.as_mut()).is_pending());
    fake.with(|s| s.store = vec![row("same", 2)]);
    let ticket = run.state.borrow().previews.ticket.unwrap();
    run.splice(ticket, vec![(1, "same".into())]).await;
    run.previews(vec![target]);
    answer.send(()).unwrap();
    settling.await;
    assert_eq!(fake.calls(Call::Preview), 2);
    assert_eq!(fake.calls(Call::PreviewArrived), 1);
    assert_eq!(fake.with(|s| s.rows[0].snippet.clone()), "New message");
}
