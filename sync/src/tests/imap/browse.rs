//! Explicit server mailbox listings are not the background sync window.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use mailrs_domain::ThreadSummary;
use mailrs_store::accounts;

use super::days_ago;
use crate::fake::FakeImap;
use crate::mailbox::{Loaded, Mailbox, Mailboxes, Scope, View};
use crate::tests::{Connected, ImapHarness, fake_settings, imap_harness_on};

const FOLDER: &str = "Work/Clients";
const OLD: i64 = 1_704_067_200_000; // January 1, 2024.

fn message(id: &str, subject: &str, extra: &str) -> Vec<u8> {
    format!(
        "From: Ann <ann@example.org>\r\nTo: me@example.org\r\nSubject: {subject}\r\n\
             Message-ID: <{id}@example.org>\r\nDate: Mon, 1 Jan 2024 09:00:00 +0000\r\n\
             {extra}\r\nHi.\r\n"
    )
    .into_bytes()
}

async fn harness() -> ImapHarness {
    let mut settings = fake_settings();
    settings.window_days = 183;
    let mut h = imap_harness_on(FakeImap::new(), settings).await;
    h.sync = Arc::new(
        Arc::try_unwrap(h.sync)
            .ok()
            .unwrap()
            .with_limits(183, 5 * 1024 * 1024 * 1024),
    );
    h.imap.add_mailbox(FOLDER, None);
    h
}

async fn listings(h: &ImapHarness) -> (Mailboxes<Connected>, Scope) {
    let scope = Scope::of_mail_accounts(&h.db.read(accounts::list_accounts).await.unwrap());
    let lists = Mailboxes::new(
        Arc::new(Connected(HashMap::from([(
            h.account_id,
            Arc::clone(&h.sync),
        )]))),
        h.db.clone(),
    );
    (lists, scope)
}

fn folder(h: &ImapHarness, id: &str) -> Mailbox {
    Mailbox::Label {
        account_id: h.account_id,
        label_id: id.into(),
        name: id.into(),
    }
}

fn view(grouped: bool) -> View {
    View {
        threading: grouped,
        now: crate::now_millis(),
        ..View::default()
    }
}

fn deliver(h: &ImapHarness, n: usize, date: i64) {
    h.imap.deliver(
        FOLDER,
        message(&format!("m{n}"), &format!("Note {n}"), ""),
        date,
    );
}

#[tokio::test]
async fn a_window_keeps_its_cursor_when_the_assistant_lists_the_same_mailbox() {
    let h = harness().await;
    for n in 0..120 {
        deliver(&h, n, OLD + n as i64);
    }
    h.bootstrap().await;
    let (assistant, scope) = listings(&h).await;
    let window = assistant.reader();
    let mailbox = folder(&h, FOLDER);
    let mut rows = Vec::new();
    for expected in [25, 50, 75] {
        let page = window
            .list(&mailbox, &scope, &view(false), Loaded::rows(&rows))
            .await
            .unwrap();
        rows = page.rows;
        assert_eq!(rows.len(), expected);
    }
    let page = assistant
        .list(&mailbox, &scope, &view(false), Loaded::nothing())
        .await
        .unwrap();
    assert_eq!(page.rows.len(), 25);
    let page = window
        .list(&mailbox, &scope, &view(false), Loaded::rows(&rows))
        .await
        .unwrap();
    assert_eq!(
        page.rows.len(),
        100,
        "another caller must not shrink the window's prefix"
    );
}

#[tokio::test]
async fn a_waiting_account_does_not_lock_another_account_or_overwrite_its_cursor() {
    use crate::fake::FakeSmtp;
    use crate::{AccountServices, AccountSync};
    use futures::poll;
    use std::sync::mpsc;
    use std::time::Duration;

    let h = harness().await;
    deliver(&h, 1, OLD);
    h.bootstrap().await;
    let second_id =
        h.db.write(|c| accounts::insert_account(c, "second@example.org", 0))
            .await
            .unwrap();
    let other = Arc::new(FakeImap::new());
    other.add_mailbox(FOLDER, None);
    for n in 0..60 {
        other.deliver(
            FOLDER,
            message(&format!("other{n}"), "Other account", ""),
            OLD + n,
        );
    }
    let second = Arc::new(AccountSync::new(
        second_id,
        AccountServices::imap(other, Arc::new(FakeSmtp::default()), fake_settings()),
        h.db.clone(),
        async_channel::unbounded().0,
    ));
    second.browse_mailbox(FOLDER, None, 25).await.unwrap();
    let scope = Scope::of_mail_accounts(&h.db.read(accounts::list_accounts).await.unwrap());
    let lists = Mailboxes::new(
        Arc::new(Connected(HashMap::from([
            (h.account_id, Arc::clone(&h.sync)),
            (second_id, second),
        ]))),
        h.db.clone(),
    );
    let entered = Arc::new(tokio::sync::Notify::new());
    let notified = entered.notified();
    let writer_entered = Arc::clone(&entered);
    let (release, held) = mpsc::channel();
    let db = h.db.clone();
    let writer = tokio::spawn(async move {
        db.write(move |_| {
            writer_entered.notify_one();
            held.recv().unwrap();
            Ok(())
        })
        .await
        .unwrap();
    });
    notified.await;
    let mailbox = folder(&h, FOLDER);
    let ungrouped = view(false);
    let mut waiting = Box::pin(lists.list(&mailbox, &scope, &ungrouped, Loaded::nothing()));
    let headers = h.imap.calls_to("headers");
    let reached = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            assert!(poll!(waiting.as_mut()).is_pending());
            if h.imap.calls_to("headers") > headers {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await;
    let other_box = Mailbox::Label {
        account_id: second_id,
        label_id: FOLDER.into(),
        name: FOLDER.into(),
    };
    let result = match reached {
        Ok(()) => {
            tokio::time::timeout(
                Duration::from_secs(3),
                lists.list(&other_box, &scope, &ungrouped, Loaded::nothing()),
            )
            .await
        }
        Err(error) => Err(error),
    };
    // Always release the SQLite writer, including a failing regression,
    // before unwrapping results or dropping the database.
    release.send(()).unwrap();
    writer.await.unwrap();
    let page = result
        .expect("another account must not wait for this listing")
        .unwrap();
    assert_eq!(page.rows.len(), 25);
    waiting.await.unwrap();
    let next = lists
        .list(&other_box, &scope, &ungrouped, Loaded::rows(&page.rows))
        .await
        .unwrap();
    assert_eq!(
        next.rows.len(),
        50,
        "late completion must not replace the newer cursor"
    );
    assert!(next.rows.iter().all(|r| r.account_id == second_id));
}

#[tokio::test]
async fn server_rows_keep_cached_flag_colors_and_invitation_marks() {
    use mailrs_domain::FlagColor;
    use mailrs_store::flags;

    let h = harness().await;
    h.imap.deliver(FOLDER, message("root", "Plans", ""), OLD);
    h.imap.deliver(
        FOLDER,
        message(
            "reply",
            "Re: Plans",
            "In-Reply-To: <root@example.org>\r\nReferences: <root@example.org>\r\n",
        ),
        OLD + 1,
    );
    h.imap.set_flags(FOLDER, 1, &["\\Flagged"]);
    h.imap.set_flags(FOLDER, 2, &["\\Flagged"]);
    h.bootstrap().await;
    let (lists, scope) = listings(&h).await;
    let mailbox = folder(&h, FOLDER);
    let first = lists
        .list(&mailbox, &scope, &view(false), Loaded::nothing())
        .await
        .unwrap();
    let root = first
        .rows
        .iter()
        .find(|r| r.subject == "Plans")
        .unwrap()
        .clone();
    let reply = first
        .rows
        .iter()
        .find(|r| r.subject == "Re: Plans")
        .unwrap()
        .clone();
    let account_id = h.account_id;
    let (a, b) = (root.clone(), reply.clone());
    h.db.write(move |c| {
        flags::set_color(c, account_id, &a.id, a.message_id.as_deref(), Some(FlagColor::Blue))?;
        flags::set_color(c, account_id, &b.id, b.message_id.as_deref(), Some(FlagColor::Green))?;
        c.execute("INSERT INTO invitations(account_id, uid, sequence, message_id, seen_at) VALUES (?1, 'fixture', 0, ?2, 0)",
            rusqlite::params![account_id, a.message_id.unwrap()])?;
        Ok(())
    }).await.unwrap();
    let messages = lists
        .list(&mailbox, &scope, &view(false), Loaded::nothing())
        .await
        .unwrap();
    let a = messages
        .rows
        .iter()
        .find(|r| r.message_id == root.message_id)
        .unwrap();
    let b = messages
        .rows
        .iter()
        .find(|r| r.message_id == reply.message_id)
        .unwrap();
    assert_eq!(a.flag_color, Some(FlagColor::Blue));
    assert!(a.invitation);
    assert_eq!(b.flag_color, Some(FlagColor::Green));
    assert!(!b.invitation);
    let grouped = lists
        .list(&mailbox, &scope, &view(true), Loaded::nothing())
        .await
        .unwrap();
    assert_eq!(grouped.rows.len(), 1);
    assert_eq!(grouped.rows[0].flag_color, Some(FlagColor::Green));
    assert!(grouped.rows[0].invitation);
}

#[tokio::test]
async fn an_old_only_explicit_folder_lists_past_183_days_without_following() {
    let h = harness().await;
    deliver(&h, 1, OLD);
    h.bootstrap().await;
    assert!(h.ids().await.is_empty());
    let (lists, scope) = listings(&h).await;
    let page = lists
        .list(&folder(&h, FOLDER), &scope, &view(true), Loaded::nothing())
        .await
        .unwrap();
    assert_eq!(
        page.rows.len(),
        1,
        "the server holds old mail even though the window is empty"
    );
    assert_eq!(page.rows[0].last_message_at, OLD);
    assert!(!h.is_followed(FOLDER), "browsing is not background Follow");
}

#[tokio::test]
async fn mixed_old_and_new_folder_mail_both_list() {
    let h = harness().await;
    deliver(&h, 1, OLD);
    deliver(&h, 2, days_ago(1));
    h.bootstrap().await;
    h.sync.follow_mailbox(FOLDER).await.unwrap();
    assert_eq!(h.ids().await.len(), 1, "Follow keeps the finite window");
    let (lists, scope) = listings(&h).await;
    let page = lists
        .list(&folder(&h, FOLDER), &scope, &view(false), Loaded::nothing())
        .await
        .unwrap();
    assert_eq!(page.rows.len(), 2);
    assert_eq!(page.rows[0].subject, "Note 2");
    assert_eq!(page.rows[1].subject, "Note 1");
}

#[tokio::test]
async fn more_than_a_hundred_messages_cross_many_pages_without_truncation() {
    let h = harness().await;
    for n in 0..637 {
        deliver(&h, n, OLD + n as i64);
    }
    h.bootstrap().await;
    let (lists, scope) = listings(&h).await;
    let mut held: BTreeMap<String, ThreadSummary> = BTreeMap::new();
    let mut pages = 0;
    loop {
        let rows: Vec<_> = held.values().cloned().collect();
        let page = lists
            .list(
                &folder(&h, FOLDER),
                &scope,
                &view(false),
                Loaded::rows(&rows),
            )
            .await
            .unwrap();
        assert!(!page.rows.is_empty(), "a page must make progress");
        if pages == 0 {
            assert_eq!(h.ids().await.len(), 25);
            assert!(
                h.imap.with(|s| s.answered) <= 104,
                "the first scroll does not enumerate the whole dense mailbox"
            );
        }
        for row in page.rows {
            held.insert(row.message_id.clone().unwrap(), row);
        }
        pages += 1;
        assert!(pages < 40);
        if !page.more {
            break;
        }
    }
    assert_eq!(held.len(), 637);
    assert_eq!(
        pages, 26,
        "metadata arrives in bounded pages, not one history download"
    );
    h.imap
        .fail_on("select", mailrs_imap::ImapError::Network("offline".into()));
    let cached = lists
        .list(&folder(&h, FOLDER), &scope, &view(false), Loaded::nothing())
        .await
        .unwrap();
    assert_eq!(cached.rows.len(), crate::PAGE);
    assert!(cached.more && !cached.notices.is_empty());
    h.imap
        .fail_on("select", mailrs_imap::ImapError::Network("offline".into()));
    let older_cached = lists
        .list(
            &folder(&h, FOLDER),
            &scope,
            &view(false),
            Loaded::rows(&cached.rows),
        )
        .await
        .unwrap();
    assert_eq!(
        older_cached.rows.len(),
        637,
        "the local cache's first 500 rows are not its end either"
    );
    assert!(older_cached.more && !older_cached.notices.is_empty());
}

#[tokio::test]
async fn online_folder_failure_is_not_reported_as_empty() {
    let h = harness().await;
    h.bootstrap().await;
    h.imap
        .fail_on("select", mailrs_imap::ImapError::Network("offline".into()));
    let (lists, scope) = listings(&h).await;
    let answer = lists
        .list(&folder(&h, FOLDER), &scope, &view(false), Loaded::nothing())
        .await;
    assert!(
        answer.is_err() || !answer.unwrap().notices.is_empty(),
        "a failed server listing is not an empty folder"
    );
}

#[tokio::test]
async fn grouping_across_page_boundaries_keeps_every_message_and_updates_existing_rows() {
    let h = harness().await;
    // Parents, copies with the same Message-ID, then replies. A parent
    // arrives in the cache after the reply that names it, in another page.
    for n in 0..137 {
        deliver(&h, n, OLD + n as i64);
    }
    for n in 0..137 {
        deliver(&h, n, OLD + 200 + n as i64);
    }
    for n in 0..137 {
        h.imap.deliver(
            FOLDER,
            message(
                &format!("r{n}"),
                &format!("Re: Note {n}"),
                &format!("References: <m{n}@example.org>\r\n"),
            ),
            OLD + 400 + n as i64,
        );
    }
    h.bootstrap().await;
    let (lists, scope) = listings(&h).await;
    let mut rows = Vec::new();
    let mut pages = 0;
    loop {
        let page = lists
            .list(
                &folder(&h, FOLDER),
                &scope,
                &view(true),
                Loaded::rows(&rows),
            )
            .await
            .unwrap();
        assert!(
            page.replace,
            "a prefix can revise an earlier conversation row"
        );
        rows = page.rows;
        pages += 1;
        assert!(pages < 30);
        if !page.more {
            break;
        }
    }
    assert_eq!(pages, 17);
    assert_eq!(
        rows.len(),
        137,
        "more than the old total search cap, after grouping"
    );
    assert!(rows.iter().all(|r| r.message_count == 3));
    assert_eq!(
        rows.iter()
            .map(|r| &r.id)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        137
    );
}

#[tokio::test]
async fn pages_of_one_conversation_do_not_mean_end_of_mailbox() {
    let h = harness().await;
    for n in 0..81 {
        h.imap
            .deliver(FOLDER, message("same", "One conversation", ""), OLD + n);
    }
    h.bootstrap().await;
    let (lists, scope) = listings(&h).await;
    let mut rows = Vec::new();
    for expected in [25, 50, 75, 81] {
        let page = lists
            .list(
                &folder(&h, FOLDER),
                &scope,
                &view(true),
                Loaded::rows(&rows),
            )
            .await
            .unwrap();
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0].message_count, expected);
        assert_eq!(page.more, expected < 81);
        rows = page.rows;
    }
}

#[tokio::test]
async fn a_failed_later_metadata_page_retries_without_losing_valid_rows_or_cursor() {
    let h = harness().await;
    for n in 0..81 {
        deliver(&h, n, OLD + n as i64);
    }
    h.bootstrap().await;
    let (lists, scope) = listings(&h).await;
    let first = lists
        .list(&folder(&h, FOLDER), &scope, &view(false), Loaded::nothing())
        .await
        .unwrap();
    assert_eq!(first.rows.len(), 25);
    assert_eq!(
        h.ids().await.len(),
        25,
        "only one bounded metadata page is stored"
    );
    h.imap
        .fail_on("headers", mailrs_imap::ImapError::Network("reset".into()));
    let failed = lists
        .list(
            &folder(&h, FOLDER),
            &scope,
            &view(false),
            Loaded::rows(&first.rows),
        )
        .await;
    assert!(failed.is_err());
    assert_eq!(h.ids().await.len(), 25);
    let retried = lists
        .list(
            &folder(&h, FOLDER),
            &scope,
            &view(false),
            Loaded::rows(&first.rows),
        )
        .await
        .unwrap();
    assert_eq!(retried.rows.len(), 50);
    assert!(retried.more);
    assert_eq!(&retried.rows[..25], first.rows);
    assert_eq!(h.ids().await.len(), 50);
}

#[tokio::test]
async fn offline_browsing_uses_cached_rows_and_does_not_claim_server_eof() {
    let h = harness().await;
    for n in 0..81 {
        deliver(&h, n, OLD + n as i64);
    }
    h.bootstrap().await;
    let (lists, scope) = listings(&h).await;
    let first = lists
        .list(&folder(&h, FOLDER), &scope, &view(false), Loaded::nothing())
        .await
        .unwrap();
    h.imap
        .fail_on("select", mailrs_imap::ImapError::Network("offline".into()));
    let fallback = lists
        .list(&folder(&h, FOLDER), &scope, &view(false), Loaded::nothing())
        .await
        .unwrap();
    assert_eq!(fallback.rows, first.rows);
    assert!(!fallback.notices.is_empty());
    assert!(fallback.more, "a partial cache is not the server's end");
    let recovered = lists
        .list(
            &folder(&h, FOLDER),
            &scope,
            &view(false),
            Loaded::rows(&fallback.rows),
        )
        .await
        .unwrap();
    assert!(recovered.more);
    assert_eq!(
        recovered.rows.len(),
        25,
        "recovery starts a fresh server prefix"
    );
    let recovered = lists
        .list(
            &folder(&h, FOLDER),
            &scope,
            &view(false),
            Loaded::rows(&recovered.rows),
        )
        .await
        .unwrap();
    assert_eq!(recovered.rows.len(), 50);
    // An offline cache is paginated too, not cut at its first local page.
    let small = View {
        limit: Some(7),
        ..view(false)
    };
    let mut cached_rows = Vec::new();
    for expected in [7, 14, 21, 28, 35, 42, 49, 50] {
        h.imap
            .fail_on("select", mailrs_imap::ImapError::Network("offline".into()));
        let cached = lists
            .list(
                &folder(&h, FOLDER),
                &scope,
                &small,
                Loaded::rows(&cached_rows),
            )
            .await
            .unwrap();
        assert_eq!(cached.rows.len(), expected);
        assert!(cached.more && cached.replace && !cached.notices.is_empty());
        cached_rows = cached.rows;
    }
    // No connected service at all, as after an offline restart.
    let offline = Mailboxes::new(Arc::new(Connected(HashMap::new())), h.db.clone());
    let cached = offline
        .list(&folder(&h, FOLDER), &scope, &view(false), Loaded::nothing())
        .await
        .unwrap();
    assert_eq!(cached.rows.len(), 50);
}

#[tokio::test]
async fn uidvalidity_reset_replaces_the_old_prefix_before_loading_more() {
    let h = harness().await;
    for n in 0..61 {
        deliver(&h, n, OLD + n as i64);
    }
    h.bootstrap().await;
    let (lists, scope) = listings(&h).await;
    let first = lists
        .list(&folder(&h, FOLDER), &scope, &view(false), Loaded::nothing())
        .await
        .unwrap();
    h.imap.reset_uidvalidity(FOLDER);
    let validity = h.imap.with(|s| s.mailboxes[FOLDER].uidvalidity);
    let reset = lists
        .list(
            &folder(&h, FOLDER),
            &scope,
            &view(false),
            Loaded::rows(&first.rows),
        )
        .await
        .unwrap();
    assert!(reset.replace && reset.more);
    assert_eq!(
        reset.rows.len(),
        25,
        "the stale prefix was discarded, not appended"
    );
    assert_eq!(
        h.ids().await.len(),
        25,
        "renumbered cached mail keeps its stored id rather than becoming a duplicate"
    );
    for row in &reset.rows {
        let location = h.location(row.message_id.as_ref().unwrap()).await.unwrap();
        assert_eq!(
            mailrs_domain::Location::parse(&location)
                .unwrap()
                .uidvalidity,
            validity
        );
    }
}

#[tokio::test]
async fn reload_drops_server_moves_and_deletions_and_finds_mail_moved_in() {
    let h = harness().await;
    for n in 0..3 {
        deliver(&h, n, OLD + n as i64);
    }
    h.bootstrap().await;
    let (lists, scope) = listings(&h).await;
    let first = lists
        .list(&folder(&h, FOLDER), &scope, &view(false), Loaded::nothing())
        .await
        .unwrap();
    assert_eq!(first.rows.len(), 3);
    h.imap.remote_expunge(FOLDER, 1);
    // Another client moves one out and one in. No feed or Follow runs.
    h.imap.deliver("Work", message("m1", "Note 1", ""), OLD + 1);
    h.imap.remote_expunge(FOLDER, 2);
    deliver(&h, 9, OLD + 9);
    lists.forget_remote();
    let fresh = lists
        .list(&folder(&h, FOLDER), &scope, &view(false), Loaded::nothing())
        .await
        .unwrap();
    assert_eq!(
        fresh
            .rows
            .iter()
            .map(|r| r.subject.as_str())
            .collect::<Vec<_>>(),
        ["Note 9", "Note 2"]
    );
    assert!(
        lists
            .changed(
                &folder(&h, FOLDER),
                &[(h.account_id, first.rows[0].id.clone())],
                &view(false)
            )
            .await
            .unwrap()
            .is_none(),
        "a stale local splice must reload from the server"
    );
}

#[tokio::test]
async fn listing_fetches_headers_only_keeps_unread_and_leaves_window_and_cache_limits_alone() {
    let h = harness().await;
    deliver(&h, 1, OLD);
    h.bootstrap().await;
    let before =
        h.db.read(move |c| accounts::sync_cursor(c, 1))
            .await
            .unwrap();
    let changes = h.sync.mail_changes();
    h.imap.with(|s| s.calls.clear());
    let (lists, scope) = listings(&h).await;
    let page = lists
        .list(&folder(&h, FOLDER), &scope, &view(false), Loaded::nothing())
        .await
        .unwrap();
    assert_eq!(page.rows.len(), 1);
    assert!(page.rows[0].unread);
    assert!(!h.imap.message(FOLDER, 1).unwrap().flags.contains("\\Seen"));
    let calls = h.imap.calls();
    assert!(calls.iter().any(|c| c.starts_with("headers ")));
    for call in &calls {
        assert!(
            ["select ", "search ", "headers "]
                .iter()
                .any(|prefix| call.starts_with(prefix)),
            "listing must not fetch raw/body/structure or write: {call}"
        );
        assert!(
            !call.contains("SINCE"),
            "explicit browsing has no date cutoff"
        );
    }
    assert_eq!(
        changes,
        h.sync.mail_changes(),
        "listing metadata is not new-mail delivery"
    );
    assert!(h.drain().is_empty());
    let after =
        h.db.read(move |c| accounts::sync_cursor(c, 1))
            .await
            .unwrap();
    assert_eq!(before.state, after.state);
    assert_eq!(before.sync_gen, after.sync_gen);
    assert_eq!(before.backfill_done, after.backfill_done);
    assert_eq!(
        h.db.read(|c| accounts::window_days(c, 1)).await.unwrap(),
        Some(183)
    );
    assert_eq!(
        h.db.read(|c| Ok(c.query_row("SELECT count(*) FROM bodies", [], |r| r.get::<_, i64>(0))?))
            .await
            .unwrap(),
        0
    );
    h.sync.prune(crate::now_millis()).await.unwrap();
    assert!(
        h.ids().await.is_empty(),
        "normal finite-window metadata pruning still applies"
    );
    assert_eq!(
        h.imap.messages_in(FOLDER).len(),
        1,
        "local pruning never deletes server mail"
    );
}

#[tokio::test]
async fn empty_noselect_and_dot_delimited_mailboxes_are_scoped_by_exact_server_id() {
    let h = harness().await;
    h.imap.add_mailbox("Work", None);
    h.imap.add_mailbox("Work.Clients", None);
    h.imap.with(|s| {
        s.mailbox_mut("Work").no_select = true;
        s.delimiter = '.';
    });
    h.imap
        .deliver("Work.Clients", message("dot", "Dot folder", ""), OLD);
    h.bootstrap().await;
    let (lists, scope) = listings(&h).await;
    for empty in ["Work", FOLDER] {
        let page = lists
            .list(&folder(&h, empty), &scope, &view(false), Loaded::nothing())
            .await
            .unwrap();
        assert!(page.rows.is_empty() && !page.more && page.notices.is_empty());
    }
    let page = lists
        .list(
            &folder(&h, "Work.Clients"),
            &scope,
            &view(false),
            Loaded::nothing(),
        )
        .await
        .unwrap();
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows[0].subject, "Dot folder");
    assert!(
        !h.imap.calls().contains(&"select Work".to_string()),
        "a NoSelect group is never selected"
    );
}

#[tokio::test]
async fn browser_never_crosses_accounts_or_folders_or_an_excluded_scope() {
    use crate::fake::FakeSmtp;
    use crate::{AccountServices, AccountSync};

    let h = harness().await;
    deliver(&h, 1, OLD);
    h.imap.add_mailbox("Work.Clients", None);
    h.imap
        .deliver("Work.Clients", message("m1", "Other folder", ""), OLD);
    h.bootstrap().await;
    let second_id =
        h.db.write(|c| accounts::insert_account(c, "second@example.org", 0))
            .await
            .unwrap();
    let other = Arc::new(FakeImap::new());
    other.add_mailbox(FOLDER, None);
    other.deliver(FOLDER, message("m1", "Other account", ""), OLD);
    let second = Arc::new(AccountSync::new(
        second_id,
        AccountServices::imap(other, Arc::new(FakeSmtp::default()), fake_settings()),
        h.db.clone(),
        async_channel::unbounded().0,
    ));
    let scope = Scope::of_mail_accounts(&h.db.read(accounts::list_accounts).await.unwrap());
    let lists = Mailboxes::new(
        Arc::new(Connected(HashMap::from([
            (h.account_id, Arc::clone(&h.sync)),
            (second_id, second),
        ]))),
        h.db.clone(),
    );
    let aside = lists
        .list(
            &folder(&h, "Work.Clients"),
            &scope,
            &view(true),
            Loaded::nothing(),
        )
        .await
        .unwrap();
    assert_eq!(aside.rows.len(), 1);
    assert_eq!(aside.rows[0].subject, "Other folder");
    let a = lists
        .list(&folder(&h, FOLDER), &scope, &view(true), Loaded::nothing())
        .await
        .unwrap();
    assert_eq!(a.rows.len(), 1);
    assert_eq!(a.rows[0].subject, "Note 1");
    assert_eq!(a.rows[0].message_count, 1);
    let other_box = Mailbox::Label {
        account_id: second_id,
        label_id: FOLDER.into(),
        name: FOLDER.into(),
    };
    let b = lists
        .list(&other_box, &scope, &view(true), Loaded::nothing())
        .await
        .unwrap();
    assert_eq!(b.rows.len(), 1);
    assert_eq!(b.rows[0].subject, "Other account");
    assert_eq!(b.rows[0].account_id, second_id);
    let excluded = lists
        .list(
            &folder(&h, FOLDER),
            &Scope::of_mail_accounts(&[]),
            &view(false),
            Loaded::nothing(),
        )
        .await
        .unwrap();
    assert!(excluded.rows.is_empty());
}

#[tokio::test]
async fn a_pop3_custom_mailbox_stays_local_and_keeps_its_old_mail() {
    use crate::fake::FakePop3;
    use crate::tests::pop3::pop3_harness;
    let h = pop3_harness(FakePop3::default(), mailrs_domain::RemoveSetting::Never).await;
    let made = h.sync.create_label(FOLDER).await.unwrap();
    h.keep("one", &message("one", "Old local mail", ""), &made.id, OLD)
        .await;
    let lists = Mailboxes::new(
        Arc::new(Connected(HashMap::from([(
            h.account_id,
            Arc::clone(&h.sync),
        )]))),
        h.db.clone(),
    );
    let mailbox = Mailbox::Label {
        account_id: h.account_id,
        label_id: made.id,
        name: FOLDER.into(),
    };
    assert!(!lists.is_remote(&mailbox));
    let scope = Scope::of_mail_accounts(&h.db.read(accounts::list_accounts).await.unwrap());
    let page = lists
        .list(&mailbox, &scope, &view(false), Loaded::nothing())
        .await
        .unwrap();
    assert_eq!(page.rows.len(), 1);
    assert!(!page.replace);
    h.sync.prune(crate::now_millis()).await.unwrap();
    let page = lists
        .list(&mailbox, &scope, &view(false), Loaded::nothing())
        .await
        .unwrap();
    assert_eq!(page.rows.len(), 1);
    assert_eq!(
        h.fake.connects(),
        0,
        "local folders never contact POP3 for a listing"
    );
}

#[tokio::test]
async fn small_page_limits_do_not_cap_the_mailbox_and_sparse_uids_do_not_skip_mail() {
    use crate::MailBackend;
    let h = harness().await;
    for n in 0..121 {
        if n == 60 {
            h.imap.with(|s| s.mailbox_mut(FOLDER).uidnext = 100_001);
        }
        deliver(&h, n, OLD + n as i64);
    }
    h.bootstrap().await;
    let (lists, scope) = listings(&h).await;
    let small = View {
        limit: Some(7),
        ..view(false)
    };
    let mut rows = Vec::new();
    let mut pages = 0;
    loop {
        let page = lists
            .list(&folder(&h, FOLDER), &scope, &small, Loaded::rows(&rows))
            .await
            .unwrap();
        assert!(page.rows.len() > rows.len());
        assert!(page.rows.len() - rows.len() <= 7);
        rows = page.rows;
        pages += 1;
        if !page.more {
            break;
        }
        assert!(pages < 20);
    }
    assert_eq!(rows.len(), 121);
    assert_eq!(pages, 18);
    let mail = &h.sync.services().mail;
    let cursor = mail
        .mailbox_page(FOLDER, None, 7)
        .await
        .unwrap()
        .next
        .unwrap();
    assert!(matches!(
        mail.mailbox_page("Work.Clients", Some(&cursor), 7).await,
        Err(crate::BackendError::NotFound | crate::BackendError::StateLost)
    ));
    assert!(matches!(
        mail.mailbox_page(FOLDER, Some("bad cursor"), 7).await,
        Err(crate::BackendError::StateLost)
    ));
}

#[tokio::test]
async fn sent_and_drafts_browse_server_mail_outside_the_background_window() {
    use crate::mailbox::Standard;

    for (name, which) in [("Sent", Standard::Sent), ("Drafts", Standard::Drafts)] {
        let h = harness().await;
        h.imap.deliver(name, message("old", "Older mail", ""), OLD);
        h.bootstrap().await;
        assert!(h.ids().await.is_empty());
        let (lists, scope) = listings(&h).await;
        let mailbox = Mailbox::Standard {
            account_id: h.account_id,
            which,
        };
        let found = lists
            .list(&mailbox, &scope, &view(false), Loaded::nothing())
            .await
            .unwrap();
        assert_eq!(
            found.rows.len(),
            1,
            "{name} must show the server's older mail"
        );
        assert_eq!(found.rows[0].subject, "Older mail");
    }
}

#[tokio::test]
async fn unified_sent_pages_through_each_imap_account() {
    use crate::fake::FakeSmtp;
    use crate::mailbox::Standard;
    use crate::{AccountServices, AccountSync};

    let h = harness().await;
    for n in 0..31 {
        h.imap.deliver(
            "Sent",
            message(&format!("a{n}"), "First account", ""),
            OLD + n,
        );
    }
    h.bootstrap().await;
    let second_id =
        h.db.write(|c| accounts::insert_account(c, "second@example.org", 0))
            .await
            .unwrap();
    let other = Arc::new(FakeImap::new());
    for n in 0..41 {
        other.deliver(
            "Sent",
            message(&format!("b{n}"), "Second account", ""),
            OLD + n,
        );
    }
    let second = Arc::new(AccountSync::new(
        second_id,
        AccountServices::imap(other, Arc::new(FakeSmtp::default()), fake_settings()),
        h.db.clone(),
        async_channel::unbounded().0,
    ));
    second.bootstrap().await.unwrap();
    let scope = Scope::of_mail_accounts(&h.db.read(accounts::list_accounts).await.unwrap());
    let lists = Mailboxes::new(
        Arc::new(Connected(HashMap::from([
            (h.account_id, Arc::clone(&h.sync)),
            (second_id, second),
        ]))),
        h.db.clone(),
    );
    let mut rows = Vec::new();
    loop {
        let page = lists
            .list(
                &Mailbox::Unified(Standard::Sent),
                &scope,
                &view(false),
                Loaded::rows(&rows),
            )
            .await
            .unwrap();
        assert!(page.rows.len() > rows.len());
        rows = page.rows;
        if !page.more {
            break;
        }
        assert!(rows.len() < 72);
    }
    assert_eq!(
        rows.iter().filter(|r| r.account_id == h.account_id).count(),
        31
    );
    assert_eq!(
        rows.iter().filter(|r| r.account_id == second_id).count(),
        41
    );
}

#[tokio::test]
async fn unified_sent_keeps_cached_mail_from_other_providers() {
    use crate::fake::{FakeGmail, meta};
    use crate::mailbox::Standard;
    use crate::{AccountServices, AccountSync};

    let h = harness().await;
    h.imap
        .deliver("Sent", message("imap", "Older IMAP mail", ""), OLD);
    h.bootstrap().await;
    let google_id =
        h.db.write(|c| accounts::insert_account(c, "google@example.org", 0))
            .await
            .unwrap();
    let google = Arc::new(FakeGmail::default());
    google.seed(meta("sent", "google-sent", crate::now_millis(), &["SENT"]));
    let sync = Arc::new(AccountSync::new(
        google_id,
        AccountServices::google(google),
        h.db.clone(),
        async_channel::unbounded().0,
    ));
    sync.bootstrap().await.unwrap();
    let scope = Scope::of_mail_accounts(&h.db.read(accounts::list_accounts).await.unwrap());
    let lists = Mailboxes::new(
        Arc::new(Connected(HashMap::from([
            (h.account_id, Arc::clone(&h.sync)),
            (google_id, sync),
        ]))),
        h.db.clone(),
    );
    let found = lists
        .list(
            &Mailbox::Unified(Standard::Sent),
            &scope,
            &view(false),
            Loaded::nothing(),
        )
        .await
        .unwrap();
    assert_eq!(found.rows.len(), 2);
    assert_eq!(
        found
            .rows
            .iter()
            .filter(|r| r.account_id == google_id)
            .count(),
        1
    );
    assert_eq!(
        found
            .rows
            .iter()
            .filter(|r| r.account_id == h.account_id)
            .count(),
        1
    );
}

#[tokio::test]
async fn a_late_unified_listing_keeps_a_newer_folders_cursor() {
    use crate::fake::FakeSmtp;
    use crate::mailbox::Standard;
    use crate::{AccountServices, AccountSync};
    use std::time::Duration;

    let h = harness().await;
    h.imap
        .deliver("Sent", message("first", "First account", ""), OLD);
    h.bootstrap().await;
    let other_id =
        h.db.write(|c| accounts::insert_account(c, "second@example.org", 0))
            .await
            .unwrap();
    let other = Arc::new(FakeImap::new());
    other.add_mailbox(FOLDER, None);
    other.deliver("Sent", message("second", "Second account", ""), OLD);
    for n in 0..60 {
        other.deliver(
            FOLDER,
            message(&format!("folder{n}"), "Client", ""),
            OLD + n,
        );
    }
    let second = Arc::new(AccountSync::new(
        other_id,
        AccountServices::imap(other, Arc::new(FakeSmtp::default()), fake_settings()),
        h.db.clone(),
        async_channel::unbounded().0,
    ));
    second.bootstrap().await.unwrap();
    second.browse_mailbox(FOLDER, None, 25).await.unwrap();
    let scope = Scope::of_mail_accounts(&h.db.read(accounts::list_accounts).await.unwrap());
    let lists = Mailboxes::new(
        Arc::new(Connected(HashMap::from([
            (h.account_id, Arc::clone(&h.sync)),
            (other_id, second),
        ]))),
        h.db.clone(),
    );
    let (release, held) = std::sync::mpsc::channel::<()>();
    let (entered, ready) = tokio::sync::oneshot::channel();
    let db = h.db.clone();
    let writer = tokio::spawn(async move {
        db.write(move |_| {
            let _ = entered.send(());
            let _ = held.recv();
            Ok(())
        })
        .await
        .unwrap();
    });
    ready.await.unwrap();
    let unified = Mailbox::Unified(Standard::Sent);
    let ungrouped = view(false);
    let mut old = Box::pin(lists.list(&unified, &scope, &ungrouped, Loaded::nothing()));
    let headers = h.imap.calls_to("headers");
    let reached = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            assert!(futures::poll!(old.as_mut()).is_pending());
            if h.imap.calls_to("headers") > headers {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    let newer = Mailbox::Label {
        account_id: other_id,
        label_id: FOLDER.into(),
        name: FOLDER.into(),
    };
    let loaded = match reached {
        Ok(()) => {
            tokio::time::timeout(
                Duration::from_secs(3),
                lists.list(&newer, &scope, &ungrouped, Loaded::nothing()),
            )
            .await
        }
        Err(error) => Err(error),
    };
    drop(release);
    writer.await.unwrap();
    let first = loaded.unwrap().unwrap();
    assert_eq!(first.rows.len(), 25);
    old.await.unwrap();
    let next = lists
        .list(&newer, &scope, &ungrouped, Loaded::rows(&first.rows))
        .await
        .unwrap();
    assert_eq!(
        next.rows.len(),
        50,
        "the old unified request must not reset this prefix"
    );
}

#[tokio::test]
async fn quick_filters_keep_the_cursor_past_an_empty_server_page() {
    let h = harness().await;
    for n in 0..30 {
        deliver(&h, n, OLD + n as i64);
        if n > 0 {
            h.imap.set_flags(FOLDER, n as u32 + 1, &["\\Seen"]);
        }
    }
    h.bootstrap().await;
    let (lists, scope) = listings(&h).await;
    let mailbox = folder(&h, FOLDER);
    let mut view = view(false);
    view.filter.unread = true;
    let first = lists
        .list(&mailbox, &scope, &view, Loaded::nothing())
        .await
        .unwrap();
    assert!(first.rows.is_empty());
    assert!(first.more && first.replace);
    let next = first.next.unwrap();
    assert_eq!(next.count, 25);
    let second = lists.list(&mailbox, &scope, &view, next).await.unwrap();
    assert_eq!(second.rows.len(), 1);
    assert_eq!(second.rows[0].subject, "Note 0");
    assert!(!second.more);
    assert!(!h.imap.message(FOLDER, 1).unwrap().flags.contains("\\Seen"));
}
