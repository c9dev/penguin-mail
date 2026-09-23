use mailrs_store::unsubscribes::{self, How, Left};
use mailrs_store::{accounts, open_in_memory};

#[test]
fn a_list_left_is_remembered_by_its_sender() {
    let conn = open_in_memory().unwrap();
    let a = accounts::insert_account(&conn, "a@example.com", 0).unwrap();
    unsubscribes::record(&conn, a, "News@Shop.example", How::Page, 1_000).unwrap();

    assert_eq!(
        unsubscribes::left(&conn, a, "news@shop.example").unwrap(),
        Some(Left {
            how: How::Page,
            at: 1_000
        }),
        "the address is matched without regard to case"
    );
    assert_eq!(unsubscribes::left(&conn, a, "other@shop.example").unwrap(), None);
}

#[test]
fn leaving_again_keeps_the_latest_way_and_time() {
    let conn = open_in_memory().unwrap();
    let a = accounts::insert_account(&conn, "a@example.com", 0).unwrap();
    unsubscribes::record(&conn, a, "news@shop.example", How::Email, 1_000).unwrap();
    unsubscribes::record(&conn, a, "news@shop.example", How::OneClick, 2_000).unwrap();

    let all = unsubscribes::all(&conn, a).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(
        all.get("news@shop.example"),
        Some(&Left {
            how: How::OneClick,
            at: 2_000
        })
    );
}

#[test]
fn each_account_keeps_its_own() {
    let conn = open_in_memory().unwrap();
    let a = accounts::insert_account(&conn, "a@example.com", 0).unwrap();
    let b = accounts::insert_account(&conn, "b@example.com", 0).unwrap();
    unsubscribes::record(&conn, a, "news@shop.example", How::Page, 1_000).unwrap();

    assert_eq!(unsubscribes::left(&conn, b, "news@shop.example").unwrap(), None);
}
