use mailrs_store::image_senders::{allow, forget, list};
use mailrs_store::open_in_memory;

#[test]
fn a_sender_goes_on_the_list_and_comes_off_it() {
    let conn = open_in_memory().unwrap();
    assert!(list(&conn).unwrap().is_empty());
    allow(&conn, "Ann@Example.com", false, 100).unwrap();
    allow(&conn, "example.net", true, 200).unwrap();
    let rows = list(&conn).unwrap();
    // Newest first, and the address arrives lower case.
    assert_eq!(rows[0].sender, "example.net");
    assert!(rows[0].whole_domain);
    assert_eq!(rows[1].sender, "ann@example.com");
    assert!(!rows[1].whole_domain);

    forget(&conn, "ANN@example.com").unwrap();
    let rows = list(&conn).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].sender, "example.net");
}

#[test]
fn allowing_the_same_sender_twice_moves_its_date() {
    let conn = open_in_memory().unwrap();
    allow(&conn, "ann@example.com", false, 100).unwrap();
    allow(&conn, "ann@example.com", true, 300).unwrap();
    let rows = list(&conn).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].allowed_at, 300);
    assert!(rows[0].whole_domain);
}

#[test]
fn an_empty_sender_is_not_stored() {
    let conn = open_in_memory().unwrap();
    allow(&conn, "   ", false, 100).unwrap();
    assert!(list(&conn).unwrap().is_empty());
}

#[test]
fn forgetting_someone_who_was_never_listed_does_nothing() {
    let conn = open_in_memory().unwrap();
    allow(&conn, "ann@example.com", false, 100).unwrap();
    forget(&conn, "bob@example.com").unwrap();
    assert_eq!(list(&conn).unwrap().len(), 1);
}
