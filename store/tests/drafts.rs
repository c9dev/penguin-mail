mod common;

use common::{LabelChange, db, meta, store};
use mailrs_store::messages::Change;
use mailrs_store::{drafts, messages};

#[test]
fn a_remembered_draft_is_found_by_its_message() {
    let (conn, id) = db();
    store(&conn, &[meta(id, "m1", "t1", 100, &["DRAFT"])]);
    drafts::remember(&conn, id, "d1", "m1").unwrap();
    assert_eq!(
        drafts::draft_of(&conn, id, "m1").unwrap().as_deref(),
        Some("d1")
    );
    assert_eq!(drafts::draft_of(&conn, id, "m2").unwrap(), None);
}

#[test]
fn a_sent_or_deleted_draft_is_forgotten() {
    let (conn, id) = db();
    store(&conn, &[meta(id, "m1", "t1", 100, &["DRAFT"])]);
    drafts::remember(&conn, id, "d1", "m1").unwrap();
    drafts::forget(&conn, id, "d1").unwrap();
    assert_eq!(drafts::draft_of(&conn, id, "m1").unwrap(), None);
}

#[test]
fn a_message_that_stopped_being_a_draft_answers_nothing() {
    let (conn, id) = db();
    store(&conn, &[meta(id, "m1", "t1", 100, &["DRAFT"])]);
    drafts::remember(&conn, id, "d1", "m1").unwrap();
    // Sending the draft elsewhere takes the label off during history replay.
    messages::apply(&conn, id, &[Change::label("m1", "DRAFT", false)]).unwrap();
    assert_eq!(drafts::draft_of(&conn, id, "m1").unwrap(), None);

    store(&conn, &[meta(id, "m2", "t2", 200, &["DRAFT"])]);
    drafts::remember(&conn, id, "d2", "m2").unwrap();
    // Deleting it elsewhere takes the message.
    let delete = Change::Delete {
        message_id: "m2".into(),
    };
    messages::apply(&conn, id, &[delete]).unwrap();
    assert_eq!(drafts::draft_of(&conn, id, "m2").unwrap(), None);
}

#[test]
fn a_draft_edited_elsewhere_leaves_only_its_new_message() {
    let (conn, id) = db();
    store(
        &conn,
        &[
            meta(id, "m1", "t1", 100, &["DRAFT"]),
            meta(id, "m2", "t1", 200, &["DRAFT"]),
        ],
    );
    drafts::remember(&conn, id, "d1", "m1").unwrap();
    drafts::remember(&conn, id, "d1", "m2").unwrap();
    assert_eq!(drafts::draft_of(&conn, id, "m1").unwrap(), None);
    assert_eq!(
        drafts::draft_of(&conn, id, "m2").unwrap().as_deref(),
        Some("d1")
    );
}

#[test]
fn a_listing_replaces_what_the_account_held() {
    let (conn, id) = db();
    store(
        &conn,
        &[
            meta(id, "m1", "t1", 100, &["DRAFT"]),
            meta(id, "m2", "t2", 200, &["DRAFT"]),
        ],
    );
    drafts::remember(&conn, id, "d1", "m1").unwrap();
    drafts::replace_all(&conn, id, &[("d2".into(), "m2".into())]).unwrap();
    assert_eq!(drafts::draft_of(&conn, id, "m1").unwrap(), None);
    assert_eq!(
        drafts::draft_of(&conn, id, "m2").unwrap().as_deref(),
        Some("d2")
    );
}

#[test]
fn one_account_does_not_answer_for_another() {
    let (conn, id) = db();
    let other = mailrs_store::accounts::insert_account(&conn, "other@example.com", 0).unwrap();
    store(&conn, &[meta(id, "m1", "t1", 100, &["DRAFT"])]);
    store(&conn, &[meta(other, "m1", "t1", 100, &["DRAFT"])]);
    drafts::remember(&conn, id, "d1", "m1").unwrap();
    assert_eq!(drafts::draft_of(&conn, other, "m1").unwrap(), None);
}
