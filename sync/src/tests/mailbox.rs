//! What each mailbox lists, counted and paged, against an in-memory store
//! and `FakeGmail`.

use std::collections::HashMap;
use std::sync::Arc;

use mailrs_domain::smart::{Condition, Field};
use mailrs_domain::{
    Account, AccountState, Category, FlagColor, Folder, SmartMailbox, ThreadSummary,
};
use mailrs_store::outbox::{self, Queued};
use mailrs_store::reminders::Reminder;
use mailrs_store::{flags, reminders};

use super::{Connected, Harness, harness};
use crate::fake::meta;
use crate::mailbox::{Listing, Loaded, Mailbox, Mailboxes, Scope, Standard, Stop, View};
use crate::now_millis;

const DAY: i64 = 24 * 60 * 60 * 1000;

fn lists(h: &Harness) -> Mailboxes<Connected> {
    let connected = HashMap::from([(h.account_id, Arc::clone(&h.sync))]);
    Mailboxes::new(Arc::new(Connected(connected)), h.db.clone())
}

fn scope(h: &Harness) -> Scope {
    Scope::over([Account {
        id: h.account_id,
        email: "me@example.com".into(),
        state: AccountState::Ok,
        provider: mailrs_domain::Provider::Gmail,
        provider_name: None,
    }])
}

fn view() -> View {
    View {
        now: now_millis(),
        ..View::default()
    }
}

/// The thread ids a listing holds, in order.
fn ids(listing: &Listing) -> Vec<String> {
    listing.rows.iter().map(|r| r.id.clone()).collect()
}

async fn list(h: &Harness, mailbox: &Mailbox, view: &View) -> Listing {
    lists(h)
        .list(mailbox, &scope(h), view, Loaded::nothing())
        .await
        .expect("the mailbox lists")
}

/// The page of `mailbox` that follows the rows of `before`.
async fn next(h: &Harness, mailbox: &Mailbox, view: &View, before: &Listing) -> Listing {
    lists(h)
        .list(mailbox, &scope(h), view, Loaded::rows(&before.rows))
        .await
        .expect("the mailbox lists")
}

/// Three inbox threads and one archived, newest first: t3, t2, t1.
async fn seeded() -> Harness {
    let h = harness().await;
    let now = now_millis();
    h.fake
        .seed(meta("a", "t1", now - 3000, &["INBOX", "UNREAD"]));
    h.fake.seed(meta("b", "t2", now - 2000, &["INBOX"]));
    h.fake.seed(meta(
        "c",
        "t3",
        now - 1000,
        &["INBOX", "CATEGORY_PROMOTIONS"],
    ));
    h.fake.seed(meta("d", "t4", now - 4000, &["Label_1"]));
    h.bootstrap_all().await;
    // A Gmail search reads one page, so let a page hold the whole mailbox.
    h.fake.with(|s| s.page_size = 1000);
    h
}

#[tokio::test]
async fn the_inbox_lists_its_threads_with_its_unread_count() {
    let h = seeded().await;

    let listing = list(&h, &Mailbox::Unified(Standard::Inbox), &view()).await;
    assert_eq!(ids(&listing), ["t3", "t2", "t1"]);
    assert_eq!((listing.unread, listing.subtitle.as_str()), (1, "1 unread"));
    assert_eq!(listing.title, "All Inboxes");
    assert_eq!(listing.empty.title, "Inbox Zero");
    assert!(!listing.more);
}

#[tokio::test]
async fn a_label_lists_only_its_own_mail() {
    let h = seeded().await;
    let label = Mailbox::Label {
        account_id: h.account_id,
        label_id: "Label_1".into(),
        name: "Label_1".into(),
    };

    let listing = list(&h, &label, &view()).await;
    assert_eq!(ids(&listing), ["t4"]);
    assert_eq!(listing.title, "Label_1");
}

#[tokio::test]
async fn the_muted_mailbox_lists_muted_threads_and_the_inbox_leaves_them_out() {
    let h = seeded().await;
    h.sync
        .triage_thread("t2", &crate::TriageAction::Mute)
        .await
        .unwrap();

    let listing = list(&h, &Mailbox::Unified(Standard::Muted), &view()).await;
    assert_eq!(ids(&listing), ["t2"]);
    assert_eq!(listing.title, "Muted");
    assert_eq!(listing.empty.title, "No Muted Mail");
    assert!(listing.rows.iter().all(|r| r.muted));

    let inbox = list(&h, &Mailbox::Unified(Standard::Inbox), &view()).await;
    assert_eq!(ids(&inbox), ["t3", "t1"]);
    assert!(inbox.rows.iter().all(|r| !r.muted));
}

#[tokio::test]
async fn a_category_narrows_the_inbox_but_not_a_label() {
    let h = seeded().await;
    let promotions = View {
        category: Some(Category::Promotions),
        ..view()
    };

    let inbox = list(&h, &Mailbox::Unified(Standard::Inbox), &promotions).await;
    assert_eq!(ids(&inbox), ["t3"]);

    let label = Mailbox::Label {
        account_id: h.account_id,
        label_id: "Label_1".into(),
        name: "Label_1".into(),
    };
    let listed = list(&h, &label, &promotions).await;
    assert_eq!(ids(&listed), ["t4"], "categories only narrow an inbox");
}

#[tokio::test]
async fn a_flag_mailbox_lists_that_colour_alone() {
    let h = harness().await;
    let now = now_millis();
    h.fake.seed(meta("a", "t1", now, &["INBOX", "STARRED"]));
    h.fake
        .seed(meta("b", "t2", now - 1000, &["INBOX", "STARRED"]));
    h.bootstrap_all().await;
    let account_id = h.account_id;
    h.db.write(move |c| flags::set_color(c, account_id, "t2", None, Some(FlagColor::Blue)))
        .await
        .unwrap();

    let blue = list(&h, &Mailbox::Flag(FlagColor::Blue), &view()).await;
    assert_eq!(ids(&blue), ["t2"]);
    assert_eq!(blue.title, "Blue Flag");
    let red = list(&h, &Mailbox::Flag(FlagColor::Red), &view()).await;
    assert_eq!(ids(&red), ["t1"], "a star with no colour counts as red");
}

#[tokio::test]
async fn a_vip_mailbox_lists_that_sender() {
    let h = seeded().await;
    let vips = Mailbox::Vips {
        emails: vec!["ann@example.com".into()],
        name: "Ann".into(),
    };

    let listing = list(&h, &vips, &view()).await;
    assert_eq!(listing.rows.len(), 4, "the fake sends everything from Ann");
    assert_eq!(listing.title, "Ann");

    let nobody = Mailbox::Vips {
        emails: vec!["nobody@example.com".into()],
        name: "VIPs".into(),
    };
    let empty = list(&h, &nobody, &view()).await;
    assert!(empty.rows.is_empty());
    assert_eq!(empty.empty.title, "No Mail from VIPs");
}

#[tokio::test]
async fn follow_up_lists_sent_mail_that_waited() {
    let h = harness().await;
    let now = now_millis();
    h.fake.seed(meta("a", "t1", now - 5 * DAY, &["SENT"]));
    h.fake.seed(meta("b", "t2", now, &["INBOX"]));
    h.bootstrap_all().await;

    let listing = list(&h, &Mailbox::FollowUp, &view()).await;
    assert_eq!(ids(&listing), ["t1"]);
    assert_eq!(listing.rows[0].snippet, "Sent 5 days ago, no reply yet");
    assert_eq!(listing.subtitle, "1 conversation");

    let off = View {
        follow_ups: false,
        ..view()
    };
    let hidden = list(&h, &Mailbox::FollowUp, &off).await;
    assert!(hidden.rows.is_empty(), "the setting turns the list off");
}

#[tokio::test]
async fn remind_me_lists_what_comes_back_soonest_first() {
    let h = seeded().await;
    let account_id = h.account_id;
    let at = now_millis() + 2 * DAY;
    h.db.write(move |c| {
        reminders::set(
            c,
            &Reminder {
                account_id,
                thread_id: "t1".into(),
                subject: "Subject a".into(),
                remind_at: at,
            },
        )
    })
    .await
    .unwrap();

    let listing = list(&h, &Mailbox::Reminders, &view()).await;
    assert_eq!(ids(&listing), ["t1"]);
    assert!(listing.rows[0].snippet.starts_with("Returns "));
    assert_eq!(listing.rows[0].last_message_at, at);
    assert_eq!(listing.title, "Remind Me");
}

#[tokio::test]
async fn send_later_lists_scheduled_drafts() {
    let h = harness().await;
    let account_id = h.account_id;
    let at = now_millis() + DAY;
    h.db.write(move |c| {
        outbox::put(
            c,
            &Queued {
                account_id,
                draft_id: Some("r1".into()),
                message_id: Some("m1".into()),
                thread_id: Some("t9".into()),
                subject: "Later".into(),
                recipients: "ann@example.com".into(),
                send_at: at,
                ..Queued::default()
            },
        )
        .map(|_| ())
    })
    .await
    .unwrap();

    let listing = list(&h, &Mailbox::Scheduled, &view()).await;
    assert_eq!(ids(&listing), ["t9"]);
    assert_eq!(listing.rows[0].from, "To ann@example.com");
    assert!(listing.rows[0].snippet.starts_with("Sends "));
    assert_eq!(
        (listing.title.as_str(), listing.subtitle.as_str()),
        ("Send Later", "1 message")
    );
    assert!(
        list(&h, &Mailbox::Outbox, &view()).await.rows.is_empty(),
        "a message waiting for its hour is not stuck"
    );
}

#[tokio::test]
async fn the_outbox_lists_what_is_stuck_and_says_why_and_when() {
    let h = harness().await;
    let account_id = h.account_id;
    let at = now_millis() + 60_000;
    let id =
        h.db.write(move |c| {
            outbox::put(
                c,
                &Queued {
                    account_id,
                    subject: "Report".into(),
                    recipients: "ann@example.com".into(),
                    send_at: at,
                    raw: Some(b"bytes".to_vec()),
                    attempts: 2,
                    problem: Some("network error: offline".into()),
                    ..Queued::default()
                },
            )
        })
        .await
        .unwrap();

    let listing = list(&h, &Mailbox::Outbox, &view()).await;
    assert_eq!(ids(&listing), [crate::outbox_row(id)]);
    assert_eq!(crate::outbox_id(&listing.rows[0].id), Some(id));
    assert_eq!(listing.rows[0].from, "To ann@example.com");
    assert!(
        listing.rows[0]
            .snippet
            .starts_with("network error: offline. Trying again "),
        "the row says why and when: {}",
        listing.rows[0].snippet
    );
    assert_eq!(
        (listing.title.as_str(), listing.subtitle.as_str()),
        ("Outbox", "1 message")
    );
}

#[tokio::test]
async fn a_message_the_outbox_gave_up_on_says_so_instead_of_naming_a_time() {
    let h = harness().await;
    let account_id = h.account_id;
    h.db.write(move |c| {
        outbox::put(
            c,
            &Queued {
                account_id,
                subject: "Too big".into(),
                recipients: "ann@example.com".into(),
                send_at: now_millis(),
                raw: Some(b"bytes".to_vec()),
                attempts: crate::MOST_TRIES,
                problem: Some("Gmail returned HTTP 413".into()),
                ..Queued::default()
            },
        )
        .map(|_| ())
    })
    .await
    .unwrap();

    let listing = list(&h, &Mailbox::Outbox, &view()).await;
    assert_eq!(
        listing.rows[0].snippet,
        "Gmail returned HTTP 413. Penguin Mail stopped trying"
    );
}

#[tokio::test]
async fn a_smart_mailbox_asks_gmail_and_says_when_it_has_no_conditions() {
    let h = seeded().await;
    let smart = |conditions| {
        Mailbox::Smart(SmartMailbox {
            id: "s1".into(),
            name: "From Ann".into(),
            account: None,
            match_all: true,
            conditions,
        })
    };

    let listing = list(
        &h,
        &smart(vec![Condition {
            field: Field::From,
            value: "ann@example.com".into(),
        }]),
        &view(),
    )
    .await;
    assert_eq!(listing.rows.len(), 4);
    assert_eq!(listing.title, "From Ann");
    assert_eq!(listing.empty.title, "No Matching Mail");

    let bare = list(&h, &smart(Vec::new()), &view()).await;
    assert_eq!(bare.notices, ["This smart mailbox has no conditions"]);
    assert!(bare.rows.is_empty());
}

/// Folders and smart mailboxes list through query trees, and Gmail must
/// receive the search text it received before they did.
#[tokio::test]
async fn folders_and_smart_mailboxes_send_gmail_the_text_they_sent_before() {
    let h = seeded().await;
    h.fake.with(|s| s.searched.clear());
    for folder in Folder::ALL {
        let mailbox = Mailbox::Folder {
            account_id: None,
            folder,
        };
        list(&h, &mailbox, &view()).await;
    }
    let smart = Mailbox::Smart(SmartMailbox {
        id: "s1".into(),
        name: "Ann, unread".into(),
        account: None,
        match_all: false,
        conditions: vec![
            Condition {
                field: Field::From,
                value: "Ann Smith".into(),
            },
            Condition {
                field: Field::Unread,
                value: String::new(),
            },
        ],
    });
    list(&h, &smart, &view()).await;

    assert_eq!(
        h.fake.with(|s| s.searched.clone()),
        [
            "-in:inbox -in:sent -in:drafts -in:spam -in:trash",
            "in:spam",
            "in:trash",
            "-in:spam -in:trash",
            "{from:\"Ann Smith\" is:unread}",
        ]
    );
}

#[tokio::test]
async fn a_gmail_folder_and_a_search_come_from_gmail_not_the_store() {
    let h = seeded().await;
    // Seeded after the bootstrap, so Gmail has it and the store does not.
    h.fake
        .seed(meta("only-remote", "t5", now_millis(), &["SPAM"]));

    let junk = list(
        &h,
        &Mailbox::Folder {
            account_id: None,
            folder: Folder::Junk,
        },
        &view(),
    )
    .await;
    assert!(ids(&junk).contains(&"t5".to_string()));
    assert_eq!(
        (junk.title.as_str(), junk.empty.title.as_str()),
        ("Junk", "No Junk")
    );

    let search = list(
        &h,
        &Mailbox::Search {
            query: "in:spam".into(),
            account_id: None,
        },
        &view(),
    )
    .await;
    assert!(ids(&search).contains(&"t5".to_string()));
    assert_eq!(
        (search.title.as_str(), search.subtitle.as_str()),
        ("Search", "in:spam")
    );

    let stored = list(&h, &Mailbox::Unified(Standard::Inbox), &view()).await;
    assert!(!ids(&stored).contains(&"t5".to_string()));
}

#[tokio::test]
async fn a_page_says_when_more_rows_follow() {
    let h = seeded().await;
    let inbox = Mailbox::Unified(Standard::Inbox);
    let paged = View {
        limit: Some(2),
        ..view()
    };

    let first = list(&h, &inbox, &paged).await;
    assert_eq!(ids(&first), ["t3", "t2"]);
    assert!(first.more);

    let second = next(&h, &inbox, &paged, &first).await;
    assert_eq!(ids(&second), ["t1"]);
    assert!(!second.more);
    assert_eq!(second.unread, 0, "only the first page counts the mailbox");
}

#[tokio::test]
async fn a_stored_page_starts_after_the_last_row_held_whatever_the_count() {
    let h = seeded().await;
    let inbox = Mailbox::Unified(Standard::Inbox);
    let first = list(&h, &inbox, &view()).await;
    let t3 = first.rows.iter().find(|r| r.id == "t3").unwrap().clone();
    let held = Loaded {
        count: 99,
        last: Some(t3),
    };
    let after = lists(&h)
        .list(&inbox, &scope(&h), &view(), held)
        .await
        .unwrap();
    assert_eq!(ids(&after), ["t2", "t1"]);

    // With messages instead of conversations, the page starts after the
    // message row, as the window holds it.
    let messages = View {
        threading: false,
        limit: Some(1),
        ..view()
    };
    let first = list(&h, &inbox, &messages).await;
    assert_eq!(first.rows[0].message_id.as_deref(), Some("c"));
    let second = next(&h, &inbox, &messages, &first).await;
    assert_eq!(second.rows[0].message_id.as_deref(), Some("b"));
}

#[tokio::test]
async fn mail_that_arrives_or_leaves_between_pages_neither_repeats_nor_skips_a_row() {
    let inbox = Mailbox::Unified(Standard::Inbox);
    let paged = View {
        limit: Some(2),
        ..view()
    };

    // New mail lands on top after the first page. Counting rows would
    // show t2 again.
    let h = seeded().await;
    let first = list(&h, &inbox, &paged).await;
    assert_eq!(ids(&first), ["t3", "t2"]);
    h.fake.deliver(meta("n", "t9", now_millis(), &["INBOX"]));
    h.sync.incremental().await.unwrap();
    let second = next(&h, &inbox, &paged, &first).await;
    assert_eq!(ids(&second), ["t1"]);

    // A thread on the first page is archived. Counting rows would skip t1.
    let h = seeded().await;
    let first = list(&h, &inbox, &paged).await;
    h.fake.remote_relabel("c", &[], &["INBOX"]);
    h.sync.incremental().await.unwrap();
    let second = next(&h, &inbox, &paged, &first).await;
    assert_eq!(ids(&second), ["t1"]);
}

#[tokio::test]
async fn changed_threads_come_back_only_while_they_belong() {
    let h = seeded().await;
    let inbox = Mailbox::Unified(Standard::Inbox);
    let named = vec![
        (h.account_id, "t1".to_string()),
        (h.account_id, "t2".into()),
    ];

    let fresh = lists(&h)
        .changed(&inbox, &named, &view())
        .await
        .unwrap()
        .expect("the store can answer for an inbox");
    let mut got: Vec<&str> = fresh.rows.iter().map(|r| r.id.as_str()).collect();
    got.sort_unstable();
    assert_eq!(got, ["t1", "t2"]);
    assert_eq!((fresh.unread, fresh.subtitle.as_str()), (1, "1 unread"));

    // t1 was the one unread thread, so archiving it empties that count too.
    h.sync
        .triage_thread("t1", &crate::TriageAction::Archive)
        .await
        .unwrap();
    let after = lists(&h)
        .changed(&inbox, &named, &view())
        .await
        .unwrap()
        .expect("still answerable");
    let ids: Vec<&str> = after.rows.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["t2"], "the archived thread left the inbox");
    assert_eq!((after.unread, after.subtitle.as_str()), (0, ""));
}

#[tokio::test]
async fn a_gmail_mailbox_has_no_row_by_row_refresh() {
    let h = seeded().await;
    let named = vec![(h.account_id, "t1".to_string())];
    let junk = Mailbox::Folder {
        account_id: None,
        folder: Folder::Junk,
    };

    assert!(
        lists(&h)
            .changed(&junk, &named, &view())
            .await
            .unwrap()
            .is_none()
    );
    let search = Mailbox::Search {
        query: "plans".into(),
        account_id: None,
    };
    assert!(
        lists(&h)
            .changed(&search, &named, &view())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        lists(&h)
            .changed(&Mailbox::Unified(Standard::Inbox), &[], &view())
            .await
            .unwrap()
            .is_none(),
        "an event with no thread ids asks for a full reload"
    );
}

/// The window keeps one `Mailboxes` and lists a folder again whenever it
/// comes back on screen. A kept Gmail search answers only while the mail
/// it listed stays as it was: mail trashed since must show in the Trash
/// and leave the folder it came from.
#[tokio::test]
async fn a_folder_lists_again_once_its_mail_changes() {
    let h = seeded().await;
    let lists = lists(&h);
    let folder = |folder| Mailbox::Folder {
        account_id: None,
        folder,
    };
    let (trash, archive) = (folder(Folder::Trash), folder(Folder::Archive));
    let scope = scope(&h);
    let list = async |mailbox: &Mailbox| {
        let listed = lists.list(mailbox, &scope, &view(), Loaded::nothing()).await;
        ids(&listed.expect("the folder lists"))
    };
    assert!(list(&trash).await.is_empty());
    assert_eq!(list(&archive).await, ["t4"]);

    // Nothing changed, so the folder answers from the search it kept.
    let searches = h.fake.with(|s| s.searched.len());
    assert_eq!(list(&archive).await, ["t4"]);
    assert_eq!(h.fake.with(|s| s.searched.len()), searches);

    h.sync
        .triage_thread("t4", &crate::TriageAction::Trash)
        .await
        .unwrap();
    assert!(list(&archive).await.is_empty(), "t4 left the Archive");
    assert_eq!(list(&trash).await, ["t4"], "t4 landed in the Trash");

    // Junked from the Trash, it leaves the Trash for the Junk folder.
    h.sync
        .triage_thread("t4", &crate::TriageAction::Junk)
        .await
        .unwrap();
    assert!(list(&trash).await.is_empty(), "t4 left the Trash");
    assert_eq!(list(&folder(Folder::Junk)).await, ["t4"], "t4 landed in Junk");
}

#[tokio::test]
async fn counts_cover_the_sidebar_and_the_categories() {
    let h = seeded().await;
    let account_id = h.account_id;
    h.db.write(move |c| flags::set_color(c, account_id, "t2", None, Some(FlagColor::Green)))
        .await
        .unwrap();
    let inbox = Mailbox::Unified(Standard::Inbox);
    let label = Mailbox::Label {
        account_id,
        label_id: "Label_1".into(),
        name: "Label_1".into(),
    };
    let sidebar = vec![
        inbox.clone(),
        Mailbox::Unified(Standard::Sent),
        label.clone(),
        Mailbox::Flag(FlagColor::Green),
        Mailbox::Vips {
            emails: vec!["ann@example.com".into()],
            name: "VIPs".into(),
        },
    ];
    let view = View {
        category: Some(Category::Primary),
        ..view()
    };

    let counts = lists(&h)
        .counts(&sidebar, &inbox, &view)
        .await
        .expect("the counts read");
    assert_eq!(counts.mailboxes[&inbox], 1, "the inbox counts unread mail");
    assert_eq!(
        counts.mailboxes[&label], 0,
        "read mail adds no unread badge"
    );
    assert_eq!(counts.mailboxes[&Mailbox::Flag(FlagColor::Green)], 0);
    assert_eq!(counts.mailboxes[&Mailbox::FollowUp], 0);
    assert_eq!(counts.mailboxes[&Mailbox::Scheduled], 0);
    assert_eq!(counts.mailboxes[&Mailbox::Reminders], 0);
    assert_eq!(counts.categories[&Category::Primary], 1);
    assert_eq!(counts.categories[&Category::Promotions], 0);
}

#[tokio::test]
async fn without_threading_a_list_holds_one_row_per_message() {
    let h = harness().await;
    let now = now_millis();
    h.fake.seed(meta("a", "t1", now - 1000, &["INBOX"]));
    h.fake.seed(meta("b", "t1", now, &["INBOX"]));
    h.bootstrap_all().await;
    let flat = View {
        threading: false,
        ..view()
    };

    let listing = list(&h, &Mailbox::Unified(Standard::Inbox), &flat).await;
    let messages: Vec<Option<&str>> = listing
        .rows
        .iter()
        .map(|r: &ThreadSummary| r.message_id.as_deref())
        .collect();
    assert_eq!(messages, [Some("b"), Some("a")]);
}

#[test]
fn each_standard_mailbox_draws_from_its_mail_set() {
    use mailrs_domain::{MailSet, Role};
    assert_eq!(Standard::Inbox.set(), MailSet::Role(Role::Inbox));
    assert_eq!(Standard::Flagged.set(), MailSet::flagged());
    assert_eq!(Standard::Sent.set(), MailSet::Role(Role::Sent));
    assert_eq!(Standard::Drafts.set(), MailSet::Role(Role::Drafts));
    assert_eq!(Standard::Muted.set(), MailSet::muted());
    for which in Standard::ALL {
        assert_eq!(Standard::from_key(which.key()), Some(which));
    }
    assert_eq!(Standard::from_key("INBOX"), Some(Standard::Inbox));
    assert_eq!(Standard::from_key("Label_3"), None);
}

#[test]
fn other_mailboxes_count_unread_but_only_an_inbox_takes_categories() {
    let unified = Mailbox::Unified(Standard::Inbox);
    let mine = Mailbox::Standard { account_id: 1, which: Standard::Inbox };
    let sent = Mailbox::Standard { account_id: 1, which: Standard::Sent };
    assert!(unified.counts_unread() && unified.takes_categories());
    assert!(mine.counts_unread() && mine.takes_categories());
    assert!(sent.counts_unread() && !sent.takes_categories());
    assert_eq!(mine.account(), Some(1));
    assert_eq!(unified.account(), None);
}

/// Each mailbox names the place its mail sits in, where it has one, so a
/// move on a folder account carries only what sits there. A list drawn
/// from anywhere names none, and the roles decide.
#[test]
fn a_mailbox_names_the_place_its_mail_is_moved_from() {
    use crate::MovedFrom;
    use mailrs_domain::{MailSet, Role};
    let inbox = MailSet::Role(Role::Inbox);
    let work = MailSet::Mailbox("Work".into());
    let cases = [
        (Mailbox::Unified(Standard::Inbox), MovedFrom::every(inbox.clone())),
        (
            Mailbox::Standard { account_id: 1, which: Standard::Inbox },
            MovedFrom::one(1, inbox),
        ),
        (
            Mailbox::Standard { account_id: 1, which: Standard::Sent },
            MovedFrom::one(1, MailSet::Role(Role::Sent)),
        ),
        (Mailbox::Unified(Standard::Flagged), MovedFrom::nowhere()),
        (Mailbox::Standard { account_id: 1, which: Standard::Muted }, MovedFrom::nowhere()),
        (
            Mailbox::Label { account_id: 1, label_id: "Work".into(), name: "Work".into() },
            MovedFrom::one(1, work.clone()),
        ),
        (
            Mailbox::Set { account_id: 1, set: work.clone(), name: "Work".into() },
            MovedFrom::one(1, work),
        ),
        (
            Mailbox::Set { account_id: 1, set: MailSet::Unseen, name: "Unread".into() },
            MovedFrom::nowhere(),
        ),
        (
            Mailbox::Folder { account_id: Some(1), folder: Folder::Trash },
            MovedFrom::one(1, MailSet::Role(Role::Trash)),
        ),
        (
            Mailbox::Folder { account_id: None, folder: Folder::Junk },
            MovedFrom::every(MailSet::Role(Role::Junk)),
        ),
        // Archive and All Mail list what sits outside some roles, which
        // on a folder server spans every ordinary folder.
        (Mailbox::Folder { account_id: Some(1), folder: Folder::Archive }, MovedFrom::nowhere()),
        (Mailbox::Folder { account_id: None, folder: Folder::AllMail }, MovedFrom::nowhere()),
        (Mailbox::Search { query: "kites".into(), account_id: Some(1) }, MovedFrom::nowhere()),
        // Remind Me put what this list shows in the Archive.
        (Mailbox::Reminders, MovedFrom::every(MailSet::Role(Role::Archive))),
        (Mailbox::Flag(FlagColor::Red), MovedFrom::nowhere()),
    ];
    for (mailbox, from) in cases {
        assert_eq!(mailbox.moved_from(), from, "{mailbox:?}");
    }
}

/// Two stored threads about kites, one about bread, and a kite thread
/// Gmail holds that the store does not.
async fn kites() -> Harness {
    let h = harness().await;
    let now = now_millis();
    for (id, at, subject) in [
        ("k1", 3000, "Kites for Sunday"),
        ("k2", 2000, "Bread recipe"),
        ("k3", 1000, "Kite repair"),
    ] {
        let mut m = meta(id, id, now - at, &["INBOX"]);
        m.subject = subject.into();
        h.fake.seed(m);
    }
    h.bootstrap_all().await;
    let mut late = meta("k4", "k4", now - 500, &["INBOX"]);
    late.subject = "Kite festival".into();
    h.fake.seed(late);
    h.fake.with(|s| s.page_size = 1000);
    h
}

#[tokio::test]
async fn a_search_answers_from_the_store_first_without_asking_gmail() {
    let h = kites().await;
    let before = h.fake.usage();

    let found = lists(&h)
        .stored_search("kite", None, &scope(&h), &view())
        .await
        .expect("the store answers");

    assert_eq!(ids(&found), ["k3", "k1"]);
    assert_eq!(
        (found.title.as_str(), found.subtitle.as_str()),
        ("Search", "kite")
    );
    assert_eq!(h.fake.usage().calls, before.calls, "Gmail heard nothing");
}

#[tokio::test]
async fn a_stopped_search_asks_gmail_nothing_and_leaves_the_next_one_whole() {
    let h = kites().await;
    let search = Mailbox::Search {
        query: "kite".into(),
        account_id: None,
    };
    let before = h.fake.usage().calls;
    let stop = Stop::default();
    stop.stop();

    lists(&h)
        .list_until(&search, &scope(&h), &view(), Loaded::nothing(), &stop)
        .await
        .expect("a stopped search still answers");
    assert_eq!(h.fake.usage().calls, before, "Gmail heard nothing");

    let found = lists(&h)
        .list_until(&search, &scope(&h), &view(), Loaded::nothing(), &Stop::default())
        .await
        .expect("the search lists");
    assert!(ids(&found).contains(&"k4".to_string()), "{:?}", ids(&found));
}

async fn counted_after_server(
    service: &Mailboxes<Connected>, sidebar: &[Mailbox], shown: &Mailbox,
) -> crate::mailbox::Counts {
    let mut changed = service.counts_changed();
    service.counts(sidebar, shown, &view()).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), changed.changed())
        .await.unwrap().unwrap();
    service.counts(sidebar, shown, &view()).await.unwrap()
}

#[tokio::test]
async fn imap_counts_include_unopened_folders_and_mail_before_the_local_window() {
    let h = super::imap_harness().await;
    h.imap.add_mailbox("INBOX/Lists", None);
    h.imap.add_mailbox("Parent", None);
    h.imap.with(|s| s.mailbox_mut("Parent").no_select = true);
    for folder in ["INBOX/Lists", "Trash", "Sent"] {
        h.imap.deliver(
            folder,
            b"Subject: old unread\r\n\r\nHello".to_vec(),
            now_millis() - 365 * DAY,
        );
        h.imap.deliver_flagged(
            folder,
            b"Subject: read\r\n\r\nHello",
            &["\\Seen"],
            now_millis(),
        );
    }
    h.sync.refresh_labels().await.unwrap();
    let service = Mailboxes::new(
        Arc::new(Connected(HashMap::from([(
            h.account_id,
            Arc::clone(&h.sync),
        )]))),
        h.db.clone(),
    );
    let folder = Mailbox::Label {
        account_id: h.account_id,
        label_id: "INBOX/Lists".into(),
        name: "Lists".into(),
    };
    let trash = Mailbox::Folder {
        account_id: Some(h.account_id),
        folder: Folder::Trash,
    };
    let sent = Mailbox::Standard {
        account_id: h.account_id,
        which: Standard::Sent,
    };
    let all_sent = Mailbox::Unified(Standard::Sent);
    let all_trash = Mailbox::Folder {
        account_id: None,
        folder: Folder::Trash,
    };
    let sidebar = vec![
        folder.clone(),
        trash.clone(),
        sent.clone(),
        all_sent.clone(),
        all_trash.clone(),
    ];
    let counts = counted_after_server(&service, &sidebar, &folder).await;
    for mailbox in &sidebar {
        assert_eq!(counts.mailboxes[mailbox], 1, "{mailbox:?}");
    }
    let calls = h.imap.with(|s| s.calls.clone());
    assert!(
        !calls
            .iter()
            .any(|call| call.starts_with("select ") || call.starts_with("headers "))
    );
    assert!(!calls.contains(&"unread Parent".into()));
    service.counts(&sidebar, &folder, &view()).await.unwrap();
    assert_eq!(
        h.imap.with(|s| s.calls.clone()),
        calls,
        "fresh counts reuse STATUS results"
    );
    service.forget_remote();
    h.imap.set_flags("INBOX/Lists", 1, &["\\Seen"]);
    let counts = counted_after_server(&service, &sidebar, &folder).await;
    assert_eq!(counts.mailboxes[&folder], 0);
    assert_eq!(counts.mailboxes[&trash], 1);
    h.imap.deliver("INBOX", b"Subject: New mail\r\n\r\nHi".to_vec(), now_millis());
    h.bootstrap().await;
    h.imap.set_flags("INBOX/Lists", 1, &[]);
    let counts = counted_after_server(&service, &sidebar, &folder).await;
    assert_eq!(counts.mailboxes[&folder], 1, "a sync change expires cached server counts");
    service.forget_remote();
    h.imap.with(|s| s.aimed.push(("unread".into(), mailrs_imap::ImapError::Network("offline".into()))));
    let counts = counted_after_server(&service, &sidebar, &folder).await;
    assert_eq!(counts.mailboxes[&folder], 0, "offline counts fall back to the stored mail");
}

#[tokio::test]
async fn local_counts_and_other_accounts_do_not_wait_for_a_slow_server() {
    use std::time::Duration;
    use crate::{AccountServices, AccountSync};
    use crate::fake::{FakeImap, FakeSmtp};

    let h = super::imap_harness().await;
    h.imap.deliver("INBOX", b"Subject: Stored\r\n\r\nHi".to_vec(), now_millis());
    h.bootstrap().await;
    h.imap.deliver("INBOX", b"Subject: New\r\n\r\nHi".to_vec(), now_millis());
    let hold = h.imap.hold_next_unread();
    let fast_id = h.db.write(|c| mailrs_store::accounts::insert_account(c, "second@example.com", 0))
        .await.unwrap();
    let fast = Arc::new(FakeImap::new());
    fast.deliver("INBOX", b"Subject: Fast\r\n\r\nHi".to_vec(), now_millis());
    let services = AccountServices::imap(fast, Arc::new(FakeSmtp::default()), super::fake_settings());
    let (events, _) = async_channel::unbounded();
    let second = Arc::new(AccountSync::new(fast_id, services, h.db.clone(), events));
    let service = Mailboxes::new(Arc::new(Connected(HashMap::from([
        (h.account_id, Arc::clone(&h.sync)), (fast_id, second),
    ]))), h.db.clone());
    let slow_box = Mailbox::Standard { account_id: h.account_id, which: Standard::Inbox };
    let fast_box = Mailbox::Standard { account_id: fast_id, which: Standard::Inbox };
    let sidebar = [slow_box.clone(), fast_box.clone()];
    let mut changed = service.counts_changed();
    let local = tokio::time::timeout(Duration::from_secs(2), service.counts(&sidebar, &slow_box, &view()))
        .await.expect("local badges must not wait for STATUS").unwrap();
    assert_eq!(local.mailboxes[&slow_box], 1);
    hold.reached().await;
    tokio::time::timeout(Duration::from_secs(2), changed.changed()).await.unwrap().unwrap();
    let available = service.counts(&sidebar, &slow_box, &view()).await.unwrap();
    assert_eq!(available.mailboxes[&fast_box], 1, "another account finishes while the first is held");
    assert_eq!(available.mailboxes[&slow_box], 1);
    hold.release();
    tokio::time::timeout(Duration::from_secs(2), changed.changed()).await.unwrap().unwrap();
    let remote = service.counts(&sidebar, &slow_box, &view()).await.unwrap();
    assert_eq!(remote.mailboxes[&slow_box], 2);

    // A manual refresh cancels the held generation before starting its replacement.
    service.forget_remote();
    let old = h.imap.hold_next_unread();
    service.counts(&sidebar, &slow_box, &view()).await.unwrap();
    old.reached().await;
    service.forget_remote();
    h.imap.set_flags("INBOX", 2, &["\\Seen"]);
    let updated = counted_after_server(&service, std::slice::from_ref(&slow_box), &slow_box).await;
    assert_eq!(updated.mailboxes[&slow_box], 1);
    old.release();
    assert_eq!(service.counts(std::slice::from_ref(&slow_box), &slow_box, &view()).await.unwrap().mailboxes[&slow_box], 1);
}

#[tokio::test]
async fn a_mail_change_restarts_pending_counts_for_unopened_folders() {
    use std::time::Duration;

    let h = super::imap_harness().await;
    h.imap.add_mailbox("INBOX/Lists", None);
    h.imap.deliver("INBOX/Lists", b"Subject: Unopened\r\n\r\nHi".to_vec(), now_millis());
    h.sync.refresh_labels().await.unwrap();
    let service = Mailboxes::new(
        Arc::new(Connected(HashMap::from([(h.account_id, Arc::clone(&h.sync))]))),
        h.db.clone(),
    );
    let folder = Mailbox::Label {
        account_id: h.account_id,
        label_id: "INBOX/Lists".into(),
        name: "Lists".into(),
    };
    let sidebar = std::slice::from_ref(&folder);
    let old = h.imap.hold_next_unread();
    let local = service.counts(sidebar, &folder, &view()).await.unwrap();
    assert_eq!(local.mailboxes[&folder], 0, "the unopened folder has no local mail");
    tokio::time::timeout(Duration::from_secs(2), old.reached()).await.unwrap();

    let before = h.sync.mail_changes();
    h.imap.deliver("INBOX", b"Subject: New mail\r\n\r\nHi".to_vec(), now_millis());
    h.bootstrap().await;
    assert_ne!(h.sync.mail_changes(), before);
    let replacement = h.imap.hold_next_unread();
    service.counts(sidebar, &folder, &view()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), replacement.reached())
        .await.expect("changed mail starts a new sweep without waiting for the old one");
    tokio::time::timeout(Duration::from_secs(2), async {
        while Arc::strong_count(&old) > 1 {
            tokio::task::yield_now().await;
        }
    }).await.expect("the old STATUS call is canceled, not left running");

    let mut changed = service.counts_changed();
    replacement.release();
    tokio::time::timeout(Duration::from_secs(2), changed.changed()).await.unwrap().unwrap();
    assert_eq!(service.counts(sidebar, &folder, &view()).await.unwrap().mailboxes[&folder], 1);
}
