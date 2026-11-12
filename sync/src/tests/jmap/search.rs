//! Search over FakeJmap: the query tree as an Email/query filter.

use mailrs_domain::query::{Query, Term};
use mailrs_domain::{MailSet, Role};

use super::{fastmail, over, raw};
use crate::fake::FakeJmap;
use crate::{MailBackend, SearchQuery};

fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

async fn ids(h: &super::Harness, query: Query) -> Vec<String> {
    h.mail()
        .search(&SearchQuery::Tree(query), 50)
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect()
}

#[tokio::test]
async fn words_sender_and_subject_find_their_mail() {
    let h = fastmail().await;
    let hello = h.fake.deliver("inbox", &raw("s1@x", "Hello there"), now());
    h.fake
        .deliver("inbox", &raw("s2@x", "Invoice"), now() - 1000);
    h.mail().mailboxes().await.unwrap();
    assert_eq!(
        ids(&h, Query::term(Term::Subject("hello".into()))).await,
        std::slice::from_ref(&hello)
    );
    assert_eq!(
        ids(&h, Query::term(Term::From("ada@example.com".into())))
            .await
            .len(),
        2
    );
    assert_eq!(
        ids(&h, Query::term(Term::Words("there".into()))).await,
        [hello]
    );
}

#[tokio::test]
async fn a_role_and_not_a_role_compile_to_mailboxes() {
    let h = fastmail().await;
    let kept = h.fake.deliver("inbox", &raw("r1@x", "Kept"), now());
    h.fake.deliver("trash", &raw("r2@x", "Binned"), now());
    h.mail().mailboxes().await.unwrap();
    let in_inbox = Query::is_in(MailSet::Role(Role::Inbox));
    assert_eq!(ids(&h, in_inbox).await, std::slice::from_ref(&kept));
    let not_trash = Query::not_in(MailSet::Role(Role::Trash));
    assert_eq!(ids(&h, not_trash).await, [kept]);
}

#[tokio::test]
async fn unread_and_flagged_compile_to_keywords() {
    let h = fastmail().await;
    let read = h.fake.deliver("inbox", &raw("k1@x", "Read"), now());
    let unread = h
        .fake
        .deliver("inbox", &raw("k2@x", "Unread"), now() - 1000);
    h.fake.set_keyword(&read, "$seen", true);
    h.fake.set_keyword(&read, "$flagged", true);
    h.mail().mailboxes().await.unwrap();
    assert_eq!(ids(&h, Query::term(Term::Unread)).await, [unread]);
    assert_eq!(ids(&h, Query::term(Term::Flagged)).await, [read]);
}

#[tokio::test]
async fn a_mailbox_named_by_its_path_is_found() {
    let h = fastmail().await;
    let trips = h.fake.add_mailbox("Trips", None);
    let id = h.fake.deliver("inbox", &raw("p1@x", "Rome"), now());
    h.fake.move_to(&id, &trips);
    h.mail().mailboxes().await.unwrap();
    assert_eq!(
        ids(&h, Query::term(Term::MailboxNamed("trips".into()))).await,
        [id]
    );
}

#[tokio::test]
async fn an_empty_or_finds_nothing_without_asking() {
    let h = fastmail().await;
    h.fake.deliver("inbox", &raw("e1@x", "Any"), now());
    h.mail().mailboxes().await.unwrap();
    let before = h.fake.with(|s| s.calls.len());
    assert!(ids(&h, Query::Or(Vec::new())).await.is_empty());
    assert_eq!(h.fake.with(|s| s.calls.len()), before);
}

#[tokio::test]
async fn typed_syntax_is_not_read_here() {
    let h = fastmail().await;
    let refused = h
        .mail()
        .search(&SearchQuery::Native("from:ada".into()), 10)
        .await;
    assert!(matches!(refused, Err(crate::BackendError::Unsupported)));
}

#[tokio::test]
async fn dates_size_and_attachments_compile_to_filters_the_server_reads() {
    let h = fastmail().await;
    let recent = h.fake.deliver("inbox", &raw("d1@x", "Recent"), now());
    let old = h
        .fake
        .deliver("inbox", &raw("d2@x", "Old"), now() - 10 * 86_400_000);
    h.mail().mailboxes().await.unwrap();
    assert_eq!(
        ids(&h, Query::term(Term::NewerThan(2))).await,
        std::slice::from_ref(&recent)
    );
    let day = chrono::Local::now().date_naive() - chrono::Duration::days(5);
    assert_eq!(ids(&h, Query::term(Term::Before(day))).await, [old]);
    assert_eq!(ids(&h, Query::term(Term::Since(day))).await, [recent]);
    assert!(ids(&h, Query::term(Term::HasAttachment)).await.is_empty());
    assert_eq!(ids(&h, Query::term(Term::Larger(1))).await.len(), 2);
    assert!(
        ids(&h, Query::term(Term::Larger(1_000_000)))
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn an_and_of_terms_needs_all_of_them() {
    let h = fastmail().await;
    let both = h.fake.deliver("inbox", &raw("a1@x", "Hello"), now());
    h.fake.deliver("trash", &raw("a2@x", "Hello"), now() - 1000);
    h.mail().mailboxes().await.unwrap();
    let tree = Query::And(vec![
        Query::term(Term::Subject("hello".into())),
        Query::is_in(MailSet::Role(Role::Inbox)),
    ]);
    assert_eq!(ids(&h, tree).await, [both]);
}

#[tokio::test]
async fn a_name_no_mailbox_has_is_refused_rather_than_matching_everything() {
    let h = fastmail().await;
    h.fake.deliver("inbox", &raw("n1@x", "Any"), now());
    h.mail().mailboxes().await.unwrap();
    let refused = h
        .mail()
        .search(
            &SearchQuery::Tree(Query::term(Term::MailboxNamed("nowhere".into()))),
            10,
        )
        .await;
    assert!(matches!(refused, Err(crate::BackendError::Unsupported)));
}

#[tokio::test]
async fn hits_are_capped_and_a_server_that_clamps_is_paged() {
    let h = over(FakeJmap::new().clamping_queries(3)).await;
    for n in 0..8 {
        h.fake
            .deliver("inbox", &raw(&format!("c{n}@x"), "Many"), now() - n);
    }
    h.mail().mailboxes().await.unwrap();
    let query = SearchQuery::Tree(Query::term(Term::Subject("many".into())));
    assert_eq!(h.mail().search(&query, 5).await.unwrap().len(), 5);
    assert_eq!(h.mail().search(&query, 50).await.unwrap().len(), 8);
}

#[tokio::test]
async fn a_filter_the_server_cannot_take_leaves_the_search_to_the_store() {
    let h = over(FakeJmap::new().one_mailbox_per_filter()).await;
    h.fake.deliver("inbox", &raw("u1@x", "Kept"), now());
    h.mail().mailboxes().await.unwrap();
    let not_trash = SearchQuery::Tree(Query::not_in(MailSet::Role(Role::Trash)));
    let asked = h.mail().search(&not_trash, 50).await;
    assert!(
        matches!(asked, Err(crate::BackendError::Unsupported)),
        "{asked:?}"
    );
}

#[tokio::test]
async fn exact_count_uses_the_total_without_the_search_limit() {
    let h = over(FakeJmap::new().clamping_queries(2)).await;
    for n in 0..503 {
        h.fake.deliver(
            "inbox",
            &raw(&format!("count{n}@example.com"), "Unread"),
            now(),
        );
    }
    h.fake
        .deliver("trash", &raw("trash@example.com", "Trash"), now());
    assert_eq!(
        h.mail()
            .count_query(&Query::term(Term::Unread))
            .await
            .unwrap(),
        503
    );
    let trash = Query::And(vec![
        Query::term(Term::Unread),
        Query::is_in(MailSet::Role(Role::Trash)),
    ]);
    assert_eq!(h.mail().count_query(&trash).await.unwrap(), 1);
}
