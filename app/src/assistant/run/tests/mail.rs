//! The mail tools that came after the first set, and finding a contact.

use std::sync::Arc;

use chrono::{Duration, Local};
use mailrs_domain::{Attachment, MessageBody};
use mailrs_gmail::labels as gmail;
use mailrs_store::{address_book, templates};
use mailrs_sync::{AccountServices, MailAction, MailCapabilities, TriageAction};
use serde_json::json;

use super::super::Permission;
use super::super::fake::{Harness, ME, NOW, labelled, meta};
use super::super::mail::{Kind, kind_of, pdf_text};
use super::{harness, mail, target};

fn later() -> String {
    (Local::now() + Duration::days(1))
        .format("%Y-%m-%dT%H:%M")
        .to_string()
}

#[tokio::test]
async fn mute_takes_a_thread_out_of_the_inbox_and_back() {
    let h = harness().await;
    let done = h.ok("mute", json!({"targets": [target("t1")]})).await;
    assert_eq!(done["done"], 1);
    let labels = h.labels_of("m1").await;
    assert!(labels.contains(&gmail::MUTE.to_string()));
    assert!(!labels.contains(&gmail::INBOX.to_string()));
    assert_eq!(
        h.asked().mail_changed[0].0,
        MailAction::Mute { muted: true }
    );

    h.ok("mute", json!({"targets": [target("t1")], "mute": false}))
        .await;
    assert!(
        !h.labels_of("m1")
            .await
            .contains(&gmail::MUTE.to_string())
    );
}

/// A message filed under the person's own label, unread and flagged, one
/// trashed and one sent under a category. The read tool names the label
/// and the standard places instead of the Gmail ids that hold them.
#[tokio::test]
async fn reading_mail_names_its_labels_and_marks_not_gmail_ids() {
    let mut mail = mail();
    mail.push(labelled(
        meta("m5", "t5", "kai@example.com", "Flagged kite", NOW),
        &[
            gmail::INBOX,
            gmail::UNREAD,
            gmail::STARRED,
            "Label_kites",
        ],
    ));
    mail.push(labelled(
        meta("m6", "t6", "kai@example.com", "Old kite plans", NOW),
        &[gmail::TRASH],
    ));
    mail.push(labelled(
        meta("m7", "t7", ME, "Kite invite", NOW),
        &[gmail::SENT, gmail::CATEGORY_SOCIAL],
    ));
    let h = Harness::with(mail).await;

    let read = h
        .ok(
            "read_conversation",
            json!({"account": ME, "thread_id": "t5"}),
        )
        .await;
    assert_eq!(read["messages"][0]["labels"], json!(["Kites"]));
    assert_eq!(read["messages"][0]["unread"], json!(true));
    assert_eq!(read["messages"][0]["flagged"], json!(true));
    assert_eq!(read["messages"][0]["muted"], json!(false));
    assert_eq!(read["messages"][0]["in"], json!(["Inbox"]));
    assert_eq!(read["messages"][0]["category"], json!(null));

    let trashed = h
        .ok(
            "read_conversation",
            json!({"account": ME, "thread_id": "t6"}),
        )
        .await;
    assert_eq!(trashed["messages"][0]["in"], json!(["Trash"]));

    let sent = h
        .ok(
            "read_conversation",
            json!({"account": ME, "thread_id": "t7"}),
        )
        .await;
    assert_eq!(sent["messages"][0]["in"], json!(["Sent"]));
    assert_eq!(sent["messages"][0]["category"], json!("Social"));
}

#[tokio::test]
async fn delete_forever_asks_then_erases() {
    let h = harness().await;
    let done = h
        .ok("delete_forever", json!({"targets": [target("t2")]}))
        .await;
    assert_eq!(done["deleted"], 1);
    assert_eq!(
        h.asked().questions,
        ["Delete 1 conversation forever? Gmail cannot bring it back."]
    );
    assert!(
        h.gmail
            .with(|s| s.remote_writes.iter().any(|w| w == "delete m2"))
    );
    assert_eq!(h.asked().relisted, 1, "the list drops the erased rows");
}

#[test]
fn the_delete_forever_question_names_the_accounts_provider() {
    assert_eq!(
        super::super::mail::erase_question(1, "Fastmail"),
        "Delete 1 conversation forever? Fastmail cannot bring it back."
    );
    assert_eq!(
        super::super::mail::erase_question(3, "Fastmail"),
        "Delete 3 conversations forever? Fastmail cannot bring them back."
    );
}

#[tokio::test]
async fn delete_forever_asks_for_the_permission_it_lacks() {
    let h = harness().await;
    h.gmail.withhold(mailrs_gmail::DELETE_SCOPE);
    let answer = h
        .run("delete_forever", json!({"targets": [target("t2")]}))
        .await;
    assert_eq!(
        answer,
        Err(format!(
            "Penguin Mail needs permission to delete mail for good for {ME}. \
             The user was asked to grant it; try again once they have."
        ))
    );
    assert_eq!(
        h.asked().permission_asked,
        [(h.account_id, Permission::Delete)]
    );

    h.effects.asked.borrow_mut().approves = false;
    h.gmail.grant(mailrs_gmail::DELETE_SCOPE);
    assert_eq!(
        h.run("delete_forever", json!({"targets": [target("t2")]}))
            .await,
        Err("The user declined.".into())
    );
    assert!(
        !h.gmail
            .with(|s| s.remote_writes.iter().any(|w| w.starts_with("delete")))
    );
}

#[tokio::test]
async fn delete_forever_on_a_server_that_cannot_says_why_and_asks_nothing() {
    let h = Harness::with_services(|gmail, services| {
        let caps = MailCapabilities {
            delete_forever: false,
            ..services.capabilities()
        };
        *services = AccountServices::fake_with_capabilities(Arc::clone(gmail), caps);
    })
    .await;
    let answer = h.run("delete_forever", json!({"targets": [target("t1")]})).await;
    assert_eq!(
        answer,
        Ok(json!({
            "unavailable": "Gmail cannot delete mail for good. Delete moves it to the Trash."
        }))
    );
    assert!(h.asked().questions.is_empty());
}

/// One account whose server files mail in folders, as IMAP does.
async fn folder_account() -> Harness {
    Harness::with_services(|gmail, services| {
        let caps = MailCapabilities {
            labels: false,
            ..services.capabilities()
        };
        *services = AccountServices::fake_with_capabilities(Arc::clone(gmail), caps);
    })
    .await
}

#[tokio::test]
async fn labelling_on_a_folder_account_moves_the_mail_into_the_folder() {
    let h = folder_account().await;
    // The store holds no thread t1, so the move fails; the action it
    // tried is what counts.
    let _ = h
        .run("label", json!({"targets": [target("t1")], "add": ["kites"]}))
        .await;
    assert_eq!(
        h.asked().mail_changed[0].0,
        MailAction::Triage(TriageAction::MoveTo("Label_kites".into()))
    );
}

#[tokio::test]
async fn a_label_change_a_folder_account_cannot_make_says_why() {
    let h = folder_account().await;
    for input in [
        json!({"targets": [target("t1")], "add": ["Kites", "Boats"]}),
        json!({"targets": [target("t1")], "remove": ["Kites"]}),
        json!({"targets": [target("t1")], "add": ["Boats"]}),
    ] {
        let answer = h.run("label", input.clone()).await;
        assert!(
            answer.as_ref().is_ok_and(|a| a["unavailable"].is_string()),
            "{input}: {answer:?}"
        );
    }
    assert!(h.asked().mail_changed.is_empty());
    assert!(h.asked().questions.is_empty());
}

#[tokio::test]
async fn send_later_schedules_a_new_message_once_the_user_agrees() {
    let h = harness().await;
    let at = later();
    let done = h
        .ok(
            "send_later",
            json!({"to": ["ann@example.com"], "subject": "Kites", "body": "Saturday?", "at": at}),
        )
        .await;
    assert_eq!(done["scheduled"], at);
    {
        let asked = h.asked();
        assert!(
            asked.questions[0].starts_with("Send “Kites” to ann@example.com tomorrow at"),
            "{}",
            asked.questions[0]
        );
        let (draft, when) = &asked.scheduled[0];
        assert_eq!(draft.subject, "Kites");
        assert_eq!(draft.draft_id, None, "a new message gets signed on the way");
        assert!(*when > Local::now().timestamp_millis());
        assert!(asked.sent.is_empty(), "nothing goes out now");
    }

    assert_eq!(
        h.run(
            "send_later",
            json!({"to": ["ann@example.com"], "body": "Hi", "at": "2020-01-01T09:00"})
        )
        .await,
        Err("That time is in the past.".into())
    );
}

#[tokio::test]
async fn send_later_takes_a_saved_draft_as_it_stands() {
    let draft = labelled(
        meta("d1", "t7", ME, "Fern swap", NOW),
        &[gmail::DRAFT],
    );
    let h = Harness::with(vec![draft]).await;
    h.gmail.with(|i| {
        i.bodies.insert(
            "d1".into(),
            MessageBody {
                text: Some("Swap on Sunday?".into()),
                ..MessageBody::default()
            },
        );
        i.drafts.insert("r-1".into(), b"Swap on Sunday?".to_vec());
        i.draft_messages.insert("r-1".into(), "d1".into());
    });

    h.ok(
        "send_later",
        json!({"draft": {"account": ME, "thread_id": "t7"}, "at": later()}),
    )
    .await;
    let asked = h.asked();
    let (draft, _) = &asked.scheduled[0];
    assert_eq!(draft.draft_id.as_deref(), Some("r-1"));
    assert_eq!(draft.subject, "Fern swap");
    assert_eq!(draft.markdown.trim(), "Swap on Sunday?");
    assert_eq!(draft.to[0].email, ME);
}

#[tokio::test]
async fn send_later_keeps_a_saved_drafts_blind_copy_and_files() {
    let draft = labelled(
        meta("d1", "t7", ME, "Fern swap", NOW),
        &[gmail::DRAFT],
    );
    let h = Harness::with(vec![draft]).await;
    let mut written = crate::compose::Draft::new(
        1,
        mailrs_domain::Address {
            name: None,
            email: ME.into(),
        },
    );
    written.to = vec![mailrs_domain::Address {
        name: None,
        email: "ann@example.com".into(),
    }];
    written.bcc = vec![mailrs_domain::Address {
        name: None,
        email: "bo@example.com".into(),
    }];
    written.subject = "Fern swap".into();
    written.markdown = "Swap on Sunday?".into();
    written.in_reply_to = Some("<parent@example.com>".into());
    written.attachments = vec![crate::compose::OutgoingAttachment {
        filename: "ferns.txt".into(),
        mime_type: "text/plain".into(),
        data: b"Three ferns.".to_vec(),
        content_id: None,
    }];
    let raw = crate::compose::build_mime(&written, 0, "<d1@example.com>").expect("a draft");
    h.gmail.with(|i| {
        i.raws.insert("d1".into(), raw.clone());
        i.drafts.insert("r-1".into(), raw);
        i.draft_messages.insert("r-1".into(), "d1".into());
    });

    h.ok(
        "send_later",
        json!({"draft": {"account": ME, "thread_id": "t7"}, "at": later()}),
    )
    .await;
    let asked = h.asked();
    let (draft, _) = &asked.scheduled[0];
    assert_eq!(draft.bcc, written.bcc);
    assert_eq!(draft.in_reply_to, written.in_reply_to);
    assert_eq!(draft.attachments, written.attachments);
}

#[tokio::test]
async fn send_later_leaves_an_encrypted_draft_to_the_composer() {
    let draft = labelled(
        meta("d1", "t7", ME, "Fern swap", NOW),
        &[gmail::DRAFT],
    );
    let h = Harness::with(vec![draft]).await;
    h.gmail.with(|i| {
        i.raws.insert(
            "d1".into(),
            b"Subject: Fern swap\r\nContent-Type: multipart/encrypted; \
              protocol=\"application/pgp-encrypted\"; boundary=\"b\"\r\n\r\n--b--\r\n"
                .to_vec(),
        );
        i.drafts.insert("r-1".into(), b"ciphertext".to_vec());
        i.draft_messages.insert("r-1".into(), "d1".into());
    });

    let refused = h
        .run(
            "send_later",
            json!({"draft": {"account": ME, "thread_id": "t7"}, "at": later()}),
        )
        .await
        .expect_err("the assistant cannot read it");

    assert!(refused.contains("encrypted"), "{refused}");
    assert!(h.asked().scheduled.is_empty());
}

#[tokio::test]
async fn a_template_fills_its_placeholders_in_a_composer() {
    let h = harness().await;
    h.db.write(|c| {
        templates::add(
            c,
            &templates::Template {
                id: 0,
                name: "Thanks".into(),
                subject: "Thanks, {{first_name}}".into(),
                markdown: "Hi {{first_name}},\n\nThank you for **{{subject}}**.".into(),
            },
        )
        .map(|_| ())
    })
    .await
    .expect("the template goes in");

    let listed = h.ok("list_templates", json!({})).await;
    assert_eq!(listed["templates"][0]["name"], "Thanks");

    let opened = h
        .ok(
            "insert_template",
            json!({"template": "thanks", "to": ["Ann Lee <ann@example.com>"]}),
        )
        .await;
    assert_eq!(opened["subject"], "Thanks, Ann");
    assert_eq!(opened["body"], "Hi Ann,\n\nThank you for **Thanks, Ann**.");
    assert_eq!(h.asked().composed[0].subject, "Thanks, Ann");
    assert!(
        h.asked().questions.is_empty(),
        "a composer needs no approval"
    );

    assert_eq!(
        h.run("insert_template", json!({"template": "Sorry"})).await,
        Err("There is no template called Sorry.".into())
    );
}

/// The message `m1`, carrying `files`, each with bytes in the in-memory Gmail.
async fn with_files(files: &[(&str, &str, &[u8])]) -> Harness {
    let h = Harness::with(mail()).await;
    h.gmail.with(|i| {
        let attachments = files
            .iter()
            .enumerate()
            .map(|(n, (name, mime, bytes))| {
                i.attachments
                    .insert(("m1".into(), format!("a{n}")), bytes.to_vec());
                Attachment {
                    part_id: n.to_string(),
                    filename: name.to_string(),
                    mime_type: mime.to_string(),
                    size: bytes.len() as i64,
                    attachment_id: Some(format!("a{n}")),
                    content_id: None,
                }
            })
            .collect();
        i.bodies.insert(
            "m1".into(),
            MessageBody {
                attachments,
                ..MessageBody::default()
            },
        );
    });
    h
}

#[tokio::test]
async fn an_attachment_reads_as_text_where_it_can() {
    let h = with_files(&[
        ("notes.txt", "text/plain", b"Bring the red kite."),
        (
            "invoice.html",
            "application/octet-stream",
            b"<html><body><p>Total: <b>12 EUR</b></p></body></html>",
        ),
        ("photo.jpg", "image/jpeg", &[0xff, 0xd8, 0xff]),
    ])
    .await;
    let read =
        |attachment: &str| json!({"account": ME, "message_id": "m1", "attachment": attachment});

    let notes = h.ok("read_attachment", read("NOTES.txt")).await;
    assert_eq!(notes["text"], "Bring the red kite.");
    assert_eq!(notes["type"], "text/plain");

    let invoice = h.ok("read_attachment", read("2")).await;
    let text = invoice["text"].as_str().expect("the page as text");
    assert!(text.contains("Total: 12 EUR"), "{text}");
    assert!(!text.contains('<'), "{text}");

    let photo = h.ok("read_attachment", read("photo.jpg")).await;
    assert!(photo.get("text").is_none());
    assert!(
        photo["note"]
            .as_str()
            .is_some_and(|n| n.contains("cannot read image/jpeg")),
        "{photo}"
    );

    assert_eq!(
        h.run("read_attachment", read("map.pdf")).await,
        Err("That message has no attachment called map.pdf. It has: notes.txt, invoice.html, photo.jpg.".into())
    );
}

#[test]
fn a_file_is_known_by_its_type_or_its_name() {
    assert_eq!(kind_of("text/plain", "a"), Kind::Text);
    assert_eq!(kind_of("application/octet-stream", "data.CSV"), Kind::Text);
    assert_eq!(kind_of("text/html", "page"), Kind::Html);
    assert_eq!(kind_of("application/pdf", "scan"), Kind::Pdf);
    assert_eq!(kind_of("application/octet-stream", "Scan.PDF"), Kind::Pdf);
    assert_eq!(kind_of("image/png", "logo.png"), Kind::Other);
    assert_eq!(kind_of("application/zip", "kites"), Kind::Other);
}

/// A one-page PDF saying "Kite invoice". The cross-reference table is left
/// out, which poppler rebuilds for itself.
const PDF: &str = "%PDF-1.4
1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj
2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj
3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 300 100]/Contents 4 0 R/Resources<</Font<</F1 5 0 R>>>>>>endobj
4 0 obj<</Length 42>>stream
BT /F1 18 Tf 20 40 Td (Kite invoice) Tj ET
endstream
endobj
5 0 obj<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>endobj
trailer<</Root 1 0 R>>
%%EOF
";

#[tokio::test]
async fn a_pdf_reads_through_pdftotext_when_there_is_one() {
    match pdf_text(PDF.as_bytes().to_vec()).await {
        Ok(Some(text)) => assert!(text.contains("Kite invoice"), "{text}"),
        // A computer without poppler says so rather than failing.
        Ok(None) => {}
        Err(err) => panic!("pdftotext failed: {err}"),
    }
}

#[tokio::test]
async fn find_contact_looks_in_the_address_book_then_in_mail() {
    let h = harness().await;
    let account_id = h.account_id;
    h.db.write(move |c| {
        address_book::save(
            c,
            &[address_book::Contact {
                account_id,
                resource: "people/c1".into(),
                name: Some("Theo Lang".into()),
                emails: vec!["theo@example.com".into()],
                organization: Some("Fernwood Kites".into()),
                phone: Some("+351 912 345 678".into()),
                ..address_book::Contact::default()
            }],
        )
    })
    .await
    .expect("the contact goes in");

    let found = h.ok("find_contact", json!({"query": "fernwood"})).await;
    assert_eq!(found["contacts"][0]["name"], "Theo Lang");
    assert_eq!(found["contacts"][0]["phone"], "+351 912 345 678");
    assert_eq!(found["contacts"][0]["account"], ME);

    let ann = h.ok("find_contact", json!({"query": "ann"})).await;
    assert_eq!(ann["contacts"], json!([]));
    assert_eq!(
        ann["from_mail"][0]["email"], "ann@example.com",
        "someone who only wrote in is found in mail"
    );
}
