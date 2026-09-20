mod common;

use common::{db, meta, store};
use mailrs_domain::Address;
use mailrs_store::contacts::list_correspondents;

fn addr(name: Option<&str>, email: &str) -> Address {
    Address {
        name: name.map(str::to_string),
        email: email.into(),
    }
}

#[test]
fn people_you_write_to_rank_first() {
    let (conn, id) = db();
    let mut sent = meta(id, "s1", "t1", 300, &["SENT"]);
    sent.from = Some(addr(Some("Me"), "me@example.com"));
    sent.to = vec![addr(Some("Ann Lee"), "ann@example.com")];
    sent.cc = vec![addr(None, "Me@Example.com")];
    let mut first = meta(id, "r1", "t2", 100, &["INBOX"]);
    first.from = Some(addr(Some("Bob"), "bob@example.com"));
    let mut second = meta(id, "r2", "t3", 200, &["INBOX"]);
    second.from = Some(addr(Some("Robert Stone"), "BOB@example.com"));
    let mut robot = meta(id, "r3", "t4", 400, &["INBOX"]);
    robot.from = Some(addr(Some("Shop"), "no-reply@shop.example"));
    store(&conn, &[sent, first, second, robot]);

    let contacts = list_correspondents(&conn).unwrap();
    let summary: Vec<(&str, Option<&str>, i64)> = contacts
        .iter()
        .map(|c| (c.email.as_str(), c.name.as_deref(), c.score))
        .collect();
    assert_eq!(
        summary,
        [
            ("ann@example.com", Some("Ann Lee"), 5),
            ("bob@example.com", Some("Robert Stone"), 2),
        ]
    );
}
