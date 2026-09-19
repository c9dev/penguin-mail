//! What each mailbox lists, counted and paged, against an in-memory store
//! and `FakeGmail`.

use std::collections::HashMap;
use std::sync::Arc;

use mailrs_domain::smart::{Condition, Field};
use mailrs_domain::{
    Account, AccountState, Category, FlagColor, Folder, SmartMailbox, ThreadSummary, system_label,
};
use mailrs_store::reminders::Reminder;
use mailrs_store::scheduled::Scheduled;
use mailrs_store::{flags, reminders, scheduled};

use super::{Connected, Harness, harness};
use crate::fake::meta;
use crate::mailbox::{Listing, Mailbox, Mailboxes, Scope, View};
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
        .list(mailbox, &scope(h), view, 0)
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

    let listing = list(&h, &Mailbox::Unified(system_label::INBOX), &view()).await;
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
async fn a_category_narrows_the_inbox_but_not_a_label() {
    let h = seeded().await;
    let promotions = View {
        category: Some(Category::Promotions),
        ..view()
    };

    let inbox = list(&h, &Mailbox::Unified(system_label::INBOX), &promotions).await;
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
        scheduled::schedule(
            c,
            &Scheduled {
                account_id,
                draft_id: "r1".into(),
                message_id: "m1".into(),
                thread_id: "t9".into(),
                subject: "Later".into(),
                recipients: "ann@example.com".into(),
                send_at: at,
            },
        )
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
    assert_eq!((junk.title.as_str(), junk.empty.title), ("Junk", "No Junk"));

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

    let stored = list(&h, &Mailbox::Unified(system_label::INBOX), &view()).await;
    assert!(!ids(&stored).contains(&"t5".to_string()));
}

#[tokio::test]
async fn a_page_says_when_more_rows_follow() {
    let h = seeded().await;
    let inbox = Mailbox::Unified(system_label::INBOX);
    let paged = View {
        limit: Some(2),
        ..view()
    };

    let first = lists(&h).list(&inbox, &scope(&h), &paged, 0).await.unwrap();
    assert_eq!(ids(&first), ["t3", "t2"]);
    assert!(first.more);

    let second = lists(&h).list(&inbox, &scope(&h), &paged, 2).await.unwrap();
    assert_eq!(ids(&second), ["t1"]);
    assert!(!second.more);
    assert_eq!(second.unread, 0, "only the first page counts the mailbox");
}

#[tokio::test]
async fn changed_threads_come_back_only_while_they_belong() {
    let h = seeded().await;
    let inbox = Mailbox::Unified(system_label::INBOX);
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
            .changed(&Mailbox::Unified(system_label::INBOX), &[], &view())
            .await
            .unwrap()
            .is_none(),
        "an event with no thread ids asks for a full reload"
    );
}

#[tokio::test]
async fn counts_cover_the_sidebar_and_the_categories() {
    let h = seeded().await;
    let account_id = h.account_id;
    h.db.write(move |c| flags::set_color(c, account_id, "t2", None, Some(FlagColor::Green)))
        .await
        .unwrap();
    let inbox = Mailbox::Unified(system_label::INBOX);
    let label = Mailbox::Label {
        account_id,
        label_id: "Label_1".into(),
        name: "Label_1".into(),
    };
    let sidebar = vec![
        inbox.clone(),
        Mailbox::Unified(system_label::SENT),
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
    assert_eq!(counts.mailboxes[&label], 1, "a label counts every thread");
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

    let listing = list(&h, &Mailbox::Unified(system_label::INBOX), &flat).await;
    let messages: Vec<Option<&str>> = listing
        .rows
        .iter()
        .map(|r: &ThreadSummary| r.message_id.as_deref())
        .collect();
    assert_eq!(messages, [Some("b"), Some("a")]);
}
