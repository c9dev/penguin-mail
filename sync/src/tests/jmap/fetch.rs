//! The window, ids and metadata over FakeJmap.

use mailrs_domain::Role;

use super::{fastmail, over, raw};
use crate::fake::FakeJmap;
use crate::{MailBackend, Want};

fn days_ago(days: i64) -> i64 {
    chrono::Utc::now().timestamp_millis() - days * 86_400_000
}

#[tokio::test]
async fn mailbox_pages_pass_the_window_cap_and_keep_old_mail() {
    let h = over(FakeJmap::new().clamping_queries(17)).await;
    let mailbox = h.fake.mailbox("trash");
    let total = crate::services::jmap::MOST_IDS + 3;
    for n in 0..total {
        h.fake
            .deliver("trash", &raw(&format!("old{n}@example.com"), "Old"), 1);
    }
    h.fake
        .deliver("inbox", &raw("kept@example.com", "Inbox"), 1);
    let mail = h.mail();
    assert!(matches!(
        mail.window_ids(crate::ALL_MAIL, Some(&mailbox)).await,
        Err(crate::BackendError::Unsupported)
    ));
    let mut cursor = None;
    let mut ids = std::collections::BTreeSet::new();
    loop {
        let page = mail
            .mailbox_page(&mailbox, cursor.as_deref(), 100)
            .await
            .unwrap();
        for hit in page.refs {
            assert!(ids.insert(hit.id), "a page repeated a message");
        }
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(ids.len(), total);
}

#[tokio::test]
async fn mailbox_pages_refuse_a_changed_query_or_another_mailbox_s_cursor() {
    let h = fastmail().await;
    let mailbox = h.fake.mailbox("trash");
    for n in 0..3 {
        h.fake
            .deliver("trash", &raw(&format!("page{n}@example.com"), "Mail"), 1);
    }
    let mail = h.mail();
    let first = mail.mailbox_page(&mailbox, None, 1).await.unwrap();
    for (folder, cursor) in [
        (h.fake.mailbox("inbox"), first.next.as_deref()),
        (mailbox.clone(), Some("expired")),
    ] {
        assert!(matches!(
            mail.mailbox_page(&folder, cursor, 1).await,
            Err(crate::BackendError::StateLost)
        ));
    }
    h.fake.destroy(&first.refs[0].id);
    assert!(matches!(
        mail.mailbox_page(&mailbox, first.next.as_deref(), 1).await,
        Err(crate::BackendError::StateLost)
    ));
}

#[tokio::test]
async fn the_window_holds_recent_mail_and_the_whole_inbox() {
    let h = fastmail().await;
    let recent = h
        .fake
        .deliver("archive", &raw("w1@x", "Recent"), days_ago(2));
    let old_inbox = h
        .fake
        .deliver("inbox", &raw("w2@x", "Old but in the inbox"), days_ago(400));
    let old_archive = h
        .fake
        .deliver("archive", &raw("w3@x", "Old"), days_ago(400));
    h.bootstrap_all().await;
    assert!(h.stored(&recent).await && h.stored(&old_inbox).await);
    assert!(!h.stored(&old_archive).await);
}

#[tokio::test]
async fn a_server_that_takes_one_mailbox_a_filter_still_lists_the_window() {
    let h = over(FakeJmap::new().one_mailbox_per_filter()).await;
    let recent = h
        .fake
        .deliver("archive", &raw("j1@x", "Recent"), days_ago(2));
    let old_inbox = h
        .fake
        .deliver("inbox", &raw("j2@x", "Old but in the inbox"), days_ago(400));
    let old_archive = h
        .fake
        .deliver("archive", &raw("j3@x", "Old"), days_ago(400));
    let work = h.fake.add_mailbox("Work", None);
    h.fake.with(|s| {
        s.emails
            .get_mut(&old_inbox)
            .unwrap()
            .mailbox_ids
            .insert(work.clone())
    });
    h.bootstrap_all().await;
    assert!(h.stored(&recent).await && h.stored(&old_inbox).await);
    assert!(!h.stored(&old_archive).await);
    let in_work = h.mail().window_ids(30, Some(&work)).await.unwrap();
    assert_eq!(
        in_work.into_iter().map(|r| r.id).collect::<Vec<_>>(),
        [old_inbox]
    );
}

#[tokio::test]
async fn backfill_pages_on_until_the_last_page() {
    let h = fastmail().await;
    for n in 0..205 {
        h.fake
            .deliver("inbox", &raw(&format!("p{n}@x"), "Paged"), days_ago(1));
    }
    let mail = h.mail();
    let first = mail.backfill(30, None).await.unwrap();
    assert_eq!(first.refs.len(), 200);
    let second = mail.backfill(30, first.next.as_deref()).await.unwrap();
    assert_eq!(second.refs.len(), 5);
    assert!(
        second.next.is_none(),
        "a page that reaches the end of every filter is the last"
    );
}

#[tokio::test]
async fn a_page_runs_on_from_recent_mail_into_the_inbox_s_older_mail() {
    let h = fastmail().await;
    for n in 0..150 {
        h.fake
            .deliver("archive", &raw(&format!("r{n}@x"), "Recent"), days_ago(1));
    }
    for n in 0..100 {
        h.fake
            .deliver("inbox", &raw(&format!("o{n}@x"), "Old"), days_ago(400));
    }
    // Recent and in the Inbox: the one kind both filters could name.
    for n in 0..10 {
        h.fake
            .deliver("inbox", &raw(&format!("n{n}@x"), "New"), days_ago(1));
    }
    let mail = h.mail();
    let first = mail.backfill(30, None).await.unwrap();
    assert_eq!(first.refs.len(), 200);
    let second = mail.backfill(30, first.next.as_deref()).await.unwrap();
    assert_eq!(second.refs.len(), 60);
    assert!(second.next.is_none());
    let mut all: Vec<String> = first
        .refs
        .into_iter()
        .chain(second.refs)
        .map(|r| r.id)
        .collect();
    all.sort();
    all.dedup();
    assert_eq!(all.len(), 260, "no message twice and none left out");
}

#[tokio::test]
async fn a_server_that_clamps_its_pages_still_lists_the_whole_window() {
    let h = over(FakeJmap::new().clamping_queries(50)).await;
    for n in 0..120 {
        h.fake
            .deliver("inbox", &raw(&format!("c{n}@x"), "Clamped"), days_ago(1));
    }
    let mail = h.mail();
    let (mut seen, mut cursor) = (0, None::<String>);
    loop {
        let page = mail.backfill(30, cursor.as_deref()).await.unwrap();
        seen += page.refs.len();
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(seen, 120);
    assert_eq!(mail.window_ids(30, None).await.unwrap().len(), 120);
}

#[tokio::test]
async fn the_window_ids_keep_the_whole_inbox_and_nothing_old_elsewhere() {
    let h = fastmail().await;
    let old_inbox = h
        .fake
        .deliver("inbox", &raw("o1@x", "Old inbox"), days_ago(400));
    let old_archive = h
        .fake
        .deliver("archive", &raw("o2@x", "Old archive"), days_ago(400));
    let new_archive = h
        .fake
        .deliver("archive", &raw("o3@x", "New archive"), days_ago(1));
    h.mail().mailboxes().await.unwrap();
    let ids = |refs: Vec<crate::RemoteRef>| {
        refs.into_iter()
            .map(|r| r.id)
            .collect::<std::collections::BTreeSet<_>>()
    };
    let all = ids(h.mail().window_ids(30, None).await.unwrap());
    assert!(all.contains(&old_inbox) && all.contains(&new_archive) && !all.contains(&old_archive));
    let inbox = ids(h
        .mail()
        .window_ids(30, Some(&h.fake.mailbox("inbox")))
        .await
        .unwrap());
    assert_eq!(inbox, [old_inbox].into());
}

#[tokio::test]
async fn an_old_inbox_message_with_another_label_stays_in_that_labels_window() {
    let h = fastmail().await;
    let old_inbox = h
        .fake
        .deliver("inbox", &raw("l1@x", "Old labelled"), days_ago(400));
    let work = h.fake.add_mailbox("Work", None);
    h.fake.with(|s| {
        s.emails
            .get_mut(&old_inbox)
            .unwrap()
            .mailbox_ids
            .insert(work.clone())
    });
    let old_other = h
        .fake
        .deliver("archive", &raw("l2@x", "Old archived"), days_ago(400));
    h.fake.with(|s| {
        s.emails
            .get_mut(&old_other)
            .unwrap()
            .mailbox_ids
            .insert(work.clone())
    });
    h.mail().mailboxes().await.unwrap();
    let found = h.mail().window_ids(30, Some(&work)).await.unwrap();
    assert_eq!(
        found.into_iter().map(|r| r.id).collect::<Vec<_>>(),
        [old_inbox]
    );
}

#[tokio::test]
async fn an_inbox_past_the_cap_is_not_listed() {
    let h = fastmail().await;
    h.mail().mailboxes().await.unwrap();
    h.fake.with(|s| {
        s.total_override = Some(("mb-inbox".into(), 20_001));
    });
    h.mail().mailboxes().await.unwrap();
    assert!(matches!(
        h.mail().inbox_ids().await,
        Err(crate::BackendError::Unsupported)
    ));
}

#[tokio::test]
async fn metadata_names_the_sender_subject_date_and_marks() {
    let h = fastmail().await;
    let id = h
        .fake
        .deliver("inbox", &raw("meta@x", "Hello"), days_ago(0));
    h.mail().mailboxes().await.unwrap();
    let found = h
        .mail()
        .fetch(vec![Want::message(id.clone()), Want::message("gone")])
        .await
        .unwrap();
    let meta = &found.metas[0];
    assert_eq!(meta.subject, "Hello");
    assert_eq!(meta.from.as_ref().unwrap().email, "ada@example.com");
    assert_eq!(meta.rfc822_msgid.as_deref(), Some("meta@x"));
    assert_eq!(meta.roles, [Role::Inbox]);
    assert!(meta.date > 0);
    assert_eq!(found.gone, ["gone"]);
}

#[tokio::test]
async fn a_whole_thread_comes_in_date_order() {
    let h = fastmail().await;
    let first = h.fake.deliver("inbox", &raw("t1@x", "Plans"), days_ago(2));
    let reply = "From: Ada <ada@example.com>\r\nTo: me@example.com\r\nSubject: Re: Plans\r\nMessage-ID: <t2@x>\r\nIn-Reply-To: <t1@x>\r\n\r\nYes\r\n";
    let second = h.fake.deliver("inbox", reply.as_bytes(), days_ago(1));
    let thread = h.fake.with(|s| s.emails[&first].thread_id.clone());
    h.mail().mailboxes().await.unwrap();
    let found = h
        .mail()
        .fetch_whole(vec![thread, "T-none".into()])
        .await
        .unwrap();
    let ids: Vec<&str> = found.whole[0].iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, [first.as_str(), second.as_str()]);
    assert_eq!(found.gone_threads, ["T-none"]);
}

#[tokio::test]
async fn a_sent_message_is_found_by_its_message_id() {
    let h = fastmail().await;
    let id = h.fake.deliver("sent", &raw("sent-1@x", "Out"), days_ago(0));
    h.mail().mailboxes().await.unwrap();
    assert_eq!(h.mail().find_sent("sent-1@x").await.unwrap(), Some(id));
    assert_eq!(h.mail().find_sent("nothing@x").await.unwrap(), None);
}

#[tokio::test]
async fn all_mail_takes_every_message_whatever_its_age() {
    let h = fastmail().await;
    let old = h
        .fake
        .deliver("archive", &raw("all@x", "Old"), days_ago(4000));
    h.mail().mailboxes().await.unwrap();
    let page = h.mail().backfill(crate::ALL_MAIL, None).await.unwrap();
    assert!(page.refs.iter().any(|r| r.id == old));
}

#[tokio::test]
async fn window_ids_stay_inside_their_mailbox() {
    let h = fastmail().await;
    h.fake.deliver("inbox", &raw("i1@x", "In"), days_ago(1));
    let archived = h.fake.deliver("archive", &raw("i2@x", "Out"), days_ago(1));
    h.mail().mailboxes().await.unwrap();
    let ids = h
        .mail()
        .window_ids(30, Some(&h.fake.mailbox("archive")))
        .await
        .unwrap();
    assert_eq!(
        ids.into_iter().map(|r| r.id).collect::<Vec<_>>(),
        [archived]
    );
}

#[tokio::test]
async fn a_message_removed_between_backfill_pages_skips_no_other() {
    let h = fastmail().await;
    for n in 0..205 {
        h.fake
            .deliver("inbox", &raw(&format!("g{n}@x"), "Paged"), days_ago(1));
    }
    let mail = h.mail();
    let first = mail.backfill(30, None).await.unwrap();
    h.fake.destroy(&first.refs[0].id);
    let second = mail.backfill(30, first.next.as_deref()).await.unwrap();
    assert_eq!(
        second.refs.len(),
        5,
        "the five after the first page, though one before them went"
    );
}

#[tokio::test]
async fn a_backfill_whose_last_message_went_starts_again_a_little_before() {
    let h = fastmail().await;
    for n in 0..205 {
        h.fake
            .deliver("inbox", &raw(&format!("h{n}@x"), "Paged"), days_ago(1));
    }
    let mail = h.mail();
    let first = mail.backfill(30, None).await.unwrap();
    h.fake.destroy(&first.refs[150].id);
    h.fake.destroy(&first.refs[199].id);
    let second = mail.backfill(30, first.next.as_deref()).await.unwrap();
    let firsts: std::collections::BTreeSet<String> =
        first.refs.iter().map(|r| r.id.clone()).collect();
    let fresh: Vec<&String> = second
        .refs
        .iter()
        .map(|r| &r.id)
        .filter(|id| !firsts.contains(*id))
        .collect();
    assert_eq!(
        fresh.len(),
        5,
        "repeats may come, but none of the rest is skipped"
    );
}

#[tokio::test]
async fn a_message_removed_during_a_window_listing_skips_no_other() {
    let h = over(FakeJmap::new().clamping_queries(2)).await;
    let ids: Vec<String> = (0..5)
        .map(|n| {
            h.fake
                .deliver("inbox", &raw(&format!("v{n}@x"), "Listed"), days_ago(1) + n)
        })
        .collect();
    h.mail().mailboxes().await.unwrap();
    // The newest goes once the first page has named it.
    h.fake.with(|s| s.vanish_after_query = Some(ids[4].clone()));
    let mut listed: Vec<String> = h
        .mail()
        .window_ids(30, None)
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    listed.sort();
    let mut expected = ids.clone();
    expected.sort();
    assert_eq!(listed, expected);
}

fn calls_of(h: &super::Harness, name: &str) -> usize {
    h.fake
        .with(|s| s.calls.iter().filter(|c| *c == name).count())
}

#[tokio::test]
async fn a_mailbox_s_window_asks_for_its_older_inbox_mail_in_one_filter() {
    let h = fastmail().await;
    let old_inbox = h
        .fake
        .deliver("inbox", &raw("f1@x", "Old labelled"), days_ago(400));
    let work = h.fake.add_mailbox("Work", None);
    h.fake.with(|s| {
        s.emails
            .get_mut(&old_inbox)
            .unwrap()
            .mailbox_ids
            .insert(work.clone())
    });
    for n in 0..30 {
        h.fake.deliver(
            "inbox",
            &raw(&format!("f{n}@y"), "Old in the Inbox only"),
            days_ago(400),
        );
    }
    h.mail().mailboxes().await.unwrap();
    h.fake.with(|s| s.calls.clear());
    let found = h.mail().window_ids(30, Some(&work)).await.unwrap();
    assert_eq!(
        found.into_iter().map(|r| r.id).collect::<Vec<_>>(),
        [old_inbox]
    );
    assert_eq!(
        calls_of(&h, "Email/get"),
        0,
        "no look at the Inbox's other mail"
    );
}

#[tokio::test]
async fn a_server_that_refuses_two_mailboxes_a_filter_is_asked_once_a_session() {
    let h = over(FakeJmap::new().one_mailbox_per_filter()).await;
    let work = h.fake.add_mailbox("Work", None);
    let travel = h.fake.add_mailbox("Travel", None);
    h.mail().mailboxes().await.unwrap();
    h.mail().window_ids(30, Some(&work)).await.unwrap();
    h.fake.with(|s| s.calls.clear());
    h.mail().window_ids(30, Some(&travel)).await.unwrap();
    assert_eq!(
        calls_of(&h, "Email/query"),
        2,
        "recent mail, then the Inbox's older mail, and no filter it refused"
    );
}

#[tokio::test]
async fn a_server_without_anchors_still_pages_the_whole_backfill() {
    let h = over(FakeJmap::new().like_james()).await;
    for n in 0..205 {
        h.fake
            .deliver("inbox", &raw(&format!("q{n}@x"), "Paged"), days_ago(1));
    }
    let mail = h.mail();
    let first = mail.backfill(30, None).await.unwrap();
    h.fake.destroy(&first.refs[3].id);
    let second = mail.backfill(30, first.next.as_deref()).await.unwrap();
    let firsts: std::collections::BTreeSet<String> =
        first.refs.iter().map(|r| r.id.clone()).collect();
    assert_eq!(
        second
            .refs
            .iter()
            .filter(|r| !firsts.contains(&r.id))
            .count(),
        5
    );
    assert!(second.next.is_none());
}

#[tokio::test]
async fn a_server_without_anchors_still_lists_a_window_whole() {
    let h = over(FakeJmap::new().like_james().clamping_queries(2)).await;
    let ids: Vec<String> = (0..5)
        .map(|n| {
            h.fake
                .deliver("inbox", &raw(&format!("z{n}@x"), "Listed"), days_ago(1) + n)
        })
        .collect();
    h.mail().mailboxes().await.unwrap();
    h.fake.with(|s| s.vanish_after_query = Some(ids[4].clone()));
    let mut listed: Vec<String> = h
        .mail()
        .window_ids(30, None)
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    listed.sort();
    let mut expected = ids.clone();
    expected.sort();
    assert_eq!(listed, expected);
}

#[tokio::test]
async fn a_refusal_that_is_not_about_the_anchor_leaves_anchors_on() {
    let h = over(FakeJmap::new().clamping_queries(2)).await;
    for n in 0..5 {
        h.fake
            .deliver("inbox", &raw(&format!("a{n}@x"), "Listed"), days_ago(1) + n);
    }
    h.mail().mailboxes().await.unwrap();
    // The page with the anchor and the page without it are both refused.
    h.fake
        .with(|s| s.refuse_from_anchor = Some((2, "the filter is not valid".into())));
    assert!(h.mail().window_ids(30, None).await.is_err());
    h.fake.with(|s| s.anchored_queries = 0);
    assert_eq!(h.mail().window_ids(30, None).await.unwrap().len(), 5);
    assert!(
        h.fake.with(|s| s.anchored_queries) > 0,
        "anchors were turned off"
    );
}

#[tokio::test]
async fn a_refusal_that_only_the_page_without_an_anchor_gets_past_turns_anchors_off() {
    let h = over(FakeJmap::new().clamping_queries(2)).await;
    for n in 0..5 {
        h.fake
            .deliver("inbox", &raw(&format!("b{n}@x"), "Listed"), days_ago(1) + n);
    }
    h.mail().mailboxes().await.unwrap();
    h.fake
        .with(|s| s.refuse_from_anchor = Some((1, "something was not valid".into())));
    assert_eq!(h.mail().window_ids(30, None).await.unwrap().len(), 5);
    h.fake.with(|s| s.anchored_queries = 0);
    assert_eq!(h.mail().window_ids(30, None).await.unwrap().len(), 5);
    assert_eq!(h.fake.with(|s| s.anchored_queries), 0);
}

#[tokio::test]
async fn a_window_past_the_listing_cap_keeps_every_message_after_the_server_loses_its_place() {
    let h = fastmail().await;
    let count = crate::services::jmap::MOST_IDS + 20;
    let ids: Vec<String> = (0..count)
        .map(|n| {
            h.fake.deliver(
                "inbox",
                &raw(&format!("cap{n}@x"), "Many"),
                days_ago(1) + n as i64,
            )
        })
        .collect();
    h.bootstrap_all().await;
    while h.sync.backfill_step().await.unwrap() {}
    let later = h
        .fake
        .deliver("inbox", &raw("cap-later@x", "Later"), days_ago(0));
    h.fake.forget_changes();
    h.look().await;
    while h.sync.backfill_step().await.unwrap() {}
    assert!(h.stored(&later).await);
    let (account, wanted) = (h.account_id, ids.clone());
    let held =
        h.db.read(move |c| mailrs_store::messages::existing_ids(c, account, &wanted))
            .await
            .unwrap();
    assert_eq!(
        held.len(),
        count,
        "the relist dropped mail it could not list"
    );
}

#[tokio::test]
async fn a_listing_past_the_cap_is_refused_rather_than_cut() {
    let h = fastmail().await;
    for n in 0..crate::services::jmap::MOST_IDS + 1 {
        h.fake
            .deliver("archive", &raw(&format!("cut{n}@x"), "Many"), days_ago(1));
    }
    h.mail().mailboxes().await.unwrap();
    assert!(matches!(
        h.mail().window_ids(30, None).await,
        Err(crate::BackendError::Unsupported)
    ));
    let archive = h.fake.mailbox("archive");
    assert!(matches!(
        h.mail().window_ids(30, Some(&archive)).await,
        Err(crate::BackendError::Unsupported)
    ));
}
