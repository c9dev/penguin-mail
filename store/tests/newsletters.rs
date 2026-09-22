mod common;

use common::{db, meta, store};
use mailrs_domain::{AccountId, Address, MessageMeta};
use mailrs_store::newsletters;

const DAY: i64 = 24 * 60 * 60 * 1000;
const NOW: i64 = 200 * DAY;
/// The 90 days a newsletters list counts a sender's mail over.
const WINDOW: i64 = NOW - 90 * DAY;

const PROMO: &[&str] = &["INBOX", "CATEGORY_PROMOTIONS"];
const FORUMS: &[&str] = &["INBOX", "CATEGORY_FORUMS"];
const PERSONAL: &[&str] = &["INBOX", "CATEGORY_PERSONAL"];

/// One message: who it came from, how old it is, what it is labelled and
/// what its unsubscribe header says. A header pointing at a web page
/// stands for a sender that promised one-click, as most of them do.
fn one(
    account_id: AccountId,
    from: (&str, &str),
    id: &str,
    days_ago: i64,
    labels: &[&str],
    header: Option<&str>,
) -> MessageMeta {
    MessageMeta {
        from: Some(Address {
            name: Some(from.0.into()),
            email: from.1.into(),
        }),
        list_unsubscribe: header.map(str::to_string),
        one_click: header.is_some_and(|h| h.starts_with("<https")),
        ..meta(
            account_id,
            id,
            &format!("t-{id}"),
            NOW - days_ago * DAY,
            labels,
        )
    }
}

#[test]
fn recent_senders_group_with_their_newest_message() {
    let (conn, account) = db();
    // The newest of the shop's four messages shouts the address, to show
    // that the grouping does not care.
    let shop = |id: &str, days: i64, url: &str| {
        let address = match id {
            "s4" => "NEWS@Shop.example",
            _ => "news@shop.example",
        };
        one(account, ("Shop News", address), id, days, PROMO, Some(url))
    };
    let forum = ("Old Forum", "forum@old.example");
    let gone = ("Gone", "gone@past.example");
    store(
        &conn,
        &[
            shop("s1", 40, "<https://shop.example/1>"),
            shop("s2", 30, "<https://shop.example/2>"),
            shop("s3", 10, "<https://shop.example/3>"),
            shop("s4", 3, "<https://shop.example/4>"),
            // Two with no header at all, caught by the category.
            one(account, forum, "f1", 20, FORUMS, None),
            one(account, forum, "f2", 8, FORUMS, None),
            // A sender whose only mail is older than the window.
            one(account, gone, "g1", 120, PROMO, None),
            // Mail that is neither bulk nor carries a header.
            one(account, ("Ann", "ann@example.com"), "a1", 2, PERSONAL, None),
        ],
    );

    let found = newsletters::list(&conn, account, WINDOW).unwrap();
    let names: Vec<&str> = found.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["Shop News", "Old Forum"], "newest sender first");

    let shop = &found[0];
    assert_eq!(
        shop.email, "news@shop.example",
        "the address is lower-cased"
    );
    assert_eq!(shop.messages, 4, "the sender's mail counts as one list");
    assert_eq!(shop.last, NOW - 3 * DAY);
    assert_eq!(shop.message_id, "s4");
    assert_eq!(shop.thread_id, "t-s4");
    assert_eq!(
        shop.header.as_deref(),
        Some("<https://shop.example/4>"),
        "the newest message's header is the one to act on"
    );
    assert!(shop.one_click);

    let forum = &found[1];
    assert_eq!(forum.messages, 2);
    assert_eq!(forum.message_id, "f2");
    assert_eq!(forum.header, None);
    assert!(!forum.one_click);
}

#[test]
fn a_header_alone_makes_a_newsletter_and_another_account_stays_out() {
    let (conn, account) = db();
    let other = mailrs_store::accounts::insert_account(&conn, "other@example.com", 0).unwrap();
    // No category on this one, but the header says it is a list.
    let leave = Some("<mailto:leave@weekly.example>");
    let weekly = ("Weekly", "letter@weekly.example");
    store(&conn, &[one(account, weekly, "w1", 5, &["INBOX"], leave)]);
    let theirs = ("Shop News", "news@shop.example");
    let url = Some("<https://shop.example/u>");
    store(&conn, &[one(other, theirs, "o1", 5, PROMO, url)]);

    let found = newsletters::list(&conn, account, WINDOW).unwrap();
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].email, "letter@weekly.example");
    assert_eq!(found[0].header.as_deref(), leave);
    assert!(!found[0].one_click, "a mailto is no one-click promise");
}
