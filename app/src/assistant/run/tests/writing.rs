//! The writing tools: blind copies, files, forwards, signing and
//! encryption on the way out, and the drafts Gmail keeps.

use std::path::{Path, PathBuf};

use mail_parser::{MessageParser, MimeHeaders};
use mailrs_domain::{Address, Attachment, MessageBody, Protection, system_label};
use serde_json::json;

use super::super::fake::{Harness, ME, NOW, labelled, meta};
use super::super::writing::{local_file, off_limits};
use super::{harness, mail};
use crate::compose::{self, Draft, OutgoingAttachment};
use crate::protection::{self, Addressees, Standard};

fn address(email: &str) -> Address {
    Address {
        name: None,
        email: email.into(),
    }
}

/// The fixture mailbox, with `m1` carrying `files` and an HTML body that
/// shows the one with the id `logo`.
async fn with_files(files: &[(&str, &str, &[u8], Option<&str>)]) -> Harness {
    let h = harness().await;
    h.gmail.with(|i| {
        let attachments = files
            .iter()
            .enumerate()
            .map(|(n, (name, mime, bytes, cid))| {
                i.attachments
                    .insert(("m1".into(), format!("a{n}")), bytes.to_vec());
                Attachment {
                    part_id: n.to_string(),
                    filename: name.to_string(),
                    mime_type: mime.to_string(),
                    size: bytes.len() as i64,
                    attachment_id: Some(format!("a{n}")),
                    content_id: cid.map(str::to_string),
                }
            })
            .collect();
        i.bodies.insert(
            "m1".into(),
            MessageBody {
                text: Some("The red kite, Saturday.".into()),
                html: Some("<p>The red kite, Saturday.</p><img src=\"cid:logo\">".into()),
                attachments,
                ..MessageBody::default()
            },
        );
    });
    h
}

/// The fixture mailbox, with `m1` arrived encrypted.
async fn with_ciphertext() -> Harness {
    let h = harness().await;
    h.gmail.with(|i| {
        i.bodies.insert(
            "m1".into(),
            MessageBody {
                text: Some("-----BEGIN PGP MESSAGE-----\n...".into()),
                protection: Some(Protection::Encrypted),
                attachments: vec![Attachment {
                    part_id: "1".into(),
                    filename: "encrypted.asc".into(),
                    mime_type: "application/octet-stream".into(),
                    size: 3,
                    attachment_id: Some("a0".into()),
                    content_id: None,
                }],
                ..MessageBody::default()
            },
        );
        i.attachments
            .insert(("m1".into(), "a0".into()), b"abc".to_vec());
    });
    h
}

// ---- Blind copies, files and forwards ------------------------------------

#[tokio::test]
async fn a_blind_copy_goes_out_and_the_question_names_it() {
    let h = harness().await;
    h.ok(
        "send_email",
        json!({"to": ["ann@example.com"], "bcc": ["di@example.com"], "subject": "Kites", "body": "Hi"}),
    )
    .await;
    let asked = h.asked();
    assert_eq!(
        asked.questions,
        ["Send “Kites” to ann@example.com?\n\nBlind copy to di@example.com"]
    );
    assert_eq!(asked.sent[0].bcc, [address("di@example.com")]);
}

#[tokio::test]
async fn a_file_from_a_message_goes_out_with_its_bytes() {
    let h = with_files(&[
        ("notes.txt", "text/plain", b"Bring the red kite.", None),
        ("map.png", "image/png", &[0x89, 0x50], None),
    ])
    .await;
    h.ok(
        "send_email",
        json!({
            "to": ["ann@example.com"],
            "subject": "Notes",
            "body": "Here.",
            "attachments": [{"message_id": "m1", "attachment": "NOTES.txt"}, {"message_id": "m1", "attachment": "2"}],
        }),
    )
    .await;
    {
        let asked = h.asked();
        assert!(
            asked.questions[0].ends_with("Attached: notes.txt, map.png"),
            "{}",
            asked.questions[0]
        );
        let files = &asked.sent[0].attachments;
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].data, b"Bring the red kite.");
        assert_eq!(files[0].mime_type, "text/plain");
        assert_eq!(files[1].filename, "map.png");
    }
    assert_eq!(
        h.run(
            "send_email",
            json!({"to": ["ann@example.com"], "subject": "Notes", "body": "Here.",
                   "attachments": [{"message_id": "m1", "attachment": "plan.pdf"}]}),
        )
        .await,
        Err("That message has no attachment called plan.pdf. It has: notes.txt, map.png.".into())
    );
}

#[tokio::test]
async fn a_forward_carries_the_message_its_header_and_its_files() {
    let h = with_files(&[
        ("notes.txt", "text/plain", b"Bring the red kite.", None),
        ("logo.png", "image/png", &[0x89, 0x50], Some("logo")),
    ])
    .await;
    h.ok(
        "draft_email",
        json!({
            "to": ["ann@example.com"],
            "body": "See below.",
            "forward": {"account": ME, "message_id": "m1"},
        }),
    )
    .await;
    let asked = h.asked();
    assert!(asked.questions.is_empty(), "nothing from this computer");
    let draft = &asked.composed[0];
    assert_eq!(draft.subject, "Fwd: Kite plans");
    assert_eq!(draft.thread_id, None, "a forward starts its own thread");
    let forwarded = draft.forwarded.as_ref().expect("the forwarded message");
    assert_eq!(forwarded.subject, "Kite plans");
    assert!(
        forwarded.from.contains("theo@example.com"),
        "{}",
        forwarded.from
    );
    assert_eq!(forwarded.text, "The red kite, Saturday.");
    assert!(
        forwarded
            .to_plain()
            .contains("---------- Forwarded message ----------")
    );
    let names: Vec<(&str, Option<&str>)> = draft
        .attachments
        .iter()
        .map(|a| (a.filename.as_str(), a.content_id.as_deref()))
        .collect();
    assert_eq!(
        names,
        [("notes.txt", None), ("logo.png", Some("logo"))],
        "the image the HTML shows keeps its id"
    );
    assert_eq!(draft.attachments[0].data, b"Bring the red kite.");
}

#[tokio::test]
async fn a_forward_and_a_reply_do_not_mix() {
    let h = with_files(&[]).await;
    assert_eq!(
        h.run(
            "draft_email",
            json!({
                "body": "Hi",
                "forward": {"account": ME, "message_id": "m1"},
                "reply_to": {"account": ME, "thread_id": "t1"},
            }),
        )
        .await,
        Err("Give reply_to or forward, not both.".into())
    );
}

#[tokio::test]
async fn an_encrypted_message_is_never_sent_on() {
    let h = with_ciphertext().await;
    let forward = h
        .run(
            "send_email",
            json!({"to": ["ann@example.com"], "subject": "Fwd", "body": "Hi",
                   "forward": {"account": ME, "message_id": "m1"}}),
        )
        .await
        .expect_err("ciphertext stays put");
    assert!(forward.contains("arrived encrypted"), "{forward}");
    let file = h
        .run(
            "send_email",
            json!({"to": ["ann@example.com"], "subject": "File", "body": "Hi",
                   "attachments": [{"message_id": "m1", "attachment": "1"}]}),
        )
        .await
        .expect_err("its files stay put too");
    assert!(file.contains("arrived encrypted"), "{file}");
    let asked = h.asked();
    assert!(asked.questions.is_empty() && asked.sent.is_empty());
}

// ---- Files on this computer ------------------------------------------------

/// A folder under /tmp holding one text file, `plan.txt`.
fn a_file() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::Builder::new()
        .prefix("writing")
        .tempdir_in("/tmp")
        .expect("a folder in /tmp");
    let path = dir.path().join("plan.txt");
    std::fs::write(&path, "Meet at the hill.").expect("the file");
    (dir, path)
}

#[tokio::test]
async fn a_file_on_this_computer_is_named_before_it_is_read() {
    let h = harness().await;
    let (_dir, path) = a_file();
    let shown = std::fs::canonicalize(&path).unwrap().display().to_string();
    let input = json!({
        "to": ["ann@example.com"],
        "subject": "Plan",
        "body": "Attached.",
        "attachments": [{"path": path.display().to_string()}],
    });

    h.effects.asked.borrow_mut().approves = false;
    assert_eq!(
        h.run("send_email", input.clone()).await,
        Err("The user declined.".into())
    );
    assert!(h.asked().sent.is_empty());

    h.effects.asked.borrow_mut().approves = true;
    h.ok("send_email", input).await;
    let asked = h.asked();
    assert_eq!(
        asked.questions[1],
        format!("Send “Plan” to ann@example.com?\n\nFrom this computer: {shown}")
    );
    let file = &asked.sent[0].attachments[0];
    assert_eq!(file.filename, "plan.txt");
    assert_eq!(file.data, b"Meet at the hill.");
    assert_eq!(file.mime_type, "text/plain");
}

#[tokio::test]
async fn a_draft_with_a_file_on_this_computer_asks_first() {
    let h = harness().await;
    let (_dir, path) = a_file();
    let shown = std::fs::canonicalize(&path).unwrap().display().to_string();
    h.ok(
        "draft_email",
        json!({"body": "Attached.", "attachments": [{"path": path.display().to_string()}]}),
    )
    .await;
    let asked = h.asked();
    assert_eq!(
        asked.questions,
        [format!(
            "Put a file from this computer in a draft?\n\nFrom this computer: {shown}"
        )]
    );
    assert_eq!(asked.composed[0].attachments[0].data, b"Meet at the hill.");
}

#[tokio::test]
async fn only_the_tools_that_ask_take_a_file_on_this_computer() {
    let h = harness().await;
    let (_dir, path) = a_file();
    let refused = h
        .run(
            "send_later",
            json!({"to": ["ann@example.com"], "body": "Hi", "at": "2099-01-01T09:00",
                   "attachments": [{"path": path.display().to_string()}]}),
        )
        .await
        .expect_err("send_later names no files in its question");
    assert!(refused.contains("draft_email and send_email"), "{refused}");
    assert!(h.asked().scheduled.is_empty());
}

#[test]
fn keys_and_settings_stay_on_this_computer() {
    let home = Path::new("/home/dana");
    let private = [PathBuf::from("/srv/penguin")];
    let refused = |path: &str| off_limits(Path::new(path), home, &private);
    assert!(!refused("/home/dana/Documents/plan.pdf"));
    assert!(!refused("/tmp/scan.pdf"));
    assert!(!refused("/media/dana/USB/photo.jpg"));
    assert!(refused("/home/dana/.ssh/id_ed25519"));
    assert!(refused("/home/dana/.gnupg/private-keys-v1.d/key"));
    assert!(refused("/home/dana/.local/share/penguin-mail/mail.db"));
    assert!(refused("/home/dana/Documents/.secret/notes.txt"));
    assert!(refused("/etc/shadow"));
    assert!(refused("/proc/self/environ"));
    assert!(refused("/home/other/Documents/plan.pdf"));
    assert!(refused("/srv/penguin/mail.db"), "the app's own data");
}

#[test]
fn a_path_is_checked_where_it_leads() {
    let (dir, path) = a_file();
    let home = Path::new("/home/nobody-here");
    assert!(local_file(&path.display().to_string(), home, &[]).is_ok());
    assert!(
        local_file("plan.txt", home, &[]).is_err_and(|e| e.contains("whole path")),
        "a relative path could mean anything"
    );
    assert!(local_file("/tmp/no/such/file", home, &[]).is_err_and(|e| e.contains("no file")));
    assert!(
        local_file(&dir.path().display().to_string(), home, &[])
            .is_err_and(|e| e.contains("not a file"))
    );
    // A link in an open folder that leads somewhere closed is refused for
    // where it leads.
    let link = dir.path().join("hosts");
    std::os::unix::fs::symlink("/etc/hosts", &link).expect("a link");
    assert!(
        local_file(&link.display().to_string(), home, &[])
            .is_err_and(|e| e.contains("keys, passwords or settings"))
    );
    let private = [std::fs::canonicalize(dir.path()).unwrap()];
    assert!(local_file(&path.display().to_string(), home, &private).is_err());
}

// ---- Signing and encrypting ------------------------------------------------

#[tokio::test]
async fn encrypt_goes_out_under_openpgp_to_everyone_with_a_key() {
    let h = harness().await;
    h.effects.asked.borrow_mut().keys = Some(vec!["ann@example.com".into(), ME.into()]);
    let sent = h
        .ok(
            "send_email",
            json!({"to": ["ann@example.com"], "subject": "Keys", "body": "Hi", "encrypt": true, "sign": true}),
        )
        .await;
    assert_eq!(sent["encrypted"], true);
    assert_eq!(sent["signed"], true);
    let asked = h.asked();
    assert_eq!(
        asked.questions,
        ["Send “Keys” to ann@example.com?\n\nSigned and encrypted with OpenPGP"]
    );
    let draft = &asked.sent[0];
    assert!(draft.encrypt && draft.sign);
    assert_eq!(draft.standard, Standard::Pgp);
}

#[tokio::test]
async fn encrypt_names_whoever_has_no_key_and_sends_nothing() {
    let h = harness().await;
    h.effects.asked.borrow_mut().keys = Some(vec!["ann@example.com".into()]);
    assert_eq!(
        h.run(
            "send_email",
            json!({"to": ["ann@example.com", "bo@example.com"], "subject": "Keys", "body": "Hi", "encrypt": true}),
        )
        .await,
        Err("The message cannot be encrypted: gpg holds no key for bo@example.com.".into())
    );
    h.effects.asked.borrow_mut().keys = None;
    assert_eq!(
        h.run(
            "send_email",
            json!({"to": ["ann@example.com"], "subject": "Keys", "body": "Hi", "encrypt": true}),
        )
        .await,
        Err("The message cannot be encrypted: This computer has nothing to encrypt with.".into())
    );
    let asked = h.asked();
    assert!(asked.questions.is_empty() && asked.sent.is_empty());
}

#[tokio::test]
async fn a_blind_copy_stays_hidden_inside_the_encryption() {
    let h = harness().await;
    h.effects.asked.borrow_mut().keys =
        Some(vec!["ann@example.com".into(), "di@example.com".into()]);
    h.ok(
        "send_email",
        json!({"to": ["ann@example.com"], "bcc": ["di@example.com"], "subject": "Keys", "body": "Hi", "encrypt": true}),
    )
    .await;
    let asked = h.asked();
    let readers = Addressees::of(&asked.sent[0]).readers(false);
    assert_eq!(readers.named, ["ann@example.com"]);
    assert_eq!(readers.hidden, ["di@example.com"]);
}

#[tokio::test]
async fn sign_needs_a_key_of_the_senders_own() {
    let h = harness().await;
    h.effects.asked.borrow_mut().keys = Some(vec![]);
    assert_eq!(
        h.run(
            "send_email",
            json!({"to": ["ann@example.com"], "subject": "Signed", "body": "Hi", "sign": true}),
        )
        .await,
        Err(format!(
            "The message cannot be signed: this computer holds no key or certificate for {ME}."
        ))
    );
    h.effects.asked.borrow_mut().keys = Some(vec![ME.into()]);
    h.ok(
        "send_email",
        json!({"to": ["ann@example.com"], "subject": "Signed", "body": "Hi", "sign": true}),
    )
    .await;
    let asked = h.asked();
    assert!(asked.questions[0].ends_with("Signed with OpenPGP"));
    assert!(asked.sent[0].sign && !asked.sent[0].encrypt);
}

/// The settings the composer follows hold for the assistant too, and one
/// it cannot meet leaves the message plain, as the composer's toggle does.
#[tokio::test]
async fn the_users_settings_decide_when_the_call_says_nothing() {
    let h = harness().await;
    {
        let mut screen = h.desk.0.borrow_mut();
        screen.settings.sign_by_default = true;
        screen.settings.encrypt_when_possible = true;
    }
    h.ok(
        "send_email",
        json!({"to": ["ann@example.com"], "subject": "Plain", "body": "Hi"}),
    )
    .await;
    h.effects.asked.borrow_mut().keys = Some(vec!["ann@example.com".into(), ME.into()]);
    h.ok(
        "send_email",
        json!({"to": ["ann@example.com"], "subject": "Sealed", "body": "Hi"}),
    )
    .await;
    let asked = h.asked();
    assert!(!asked.sent[0].sign && !asked.sent[0].encrypt, "no gpg here");
    assert!(asked.sent[1].sign && asked.sent[1].encrypt);
}

#[tokio::test]
async fn a_draft_that_cannot_be_encrypted_still_opens_and_says_why() {
    let h = harness().await;
    h.effects.asked.borrow_mut().keys = Some(vec![]);
    let opened = h
        .ok(
            "draft_email",
            json!({"to": ["bo@example.com"], "body": "Hi", "encrypt": true}),
        )
        .await;
    assert_eq!(
        opened["note"],
        "The message cannot be encrypted: gpg holds no key for bo@example.com."
    );
    assert!(
        h.asked().composed[0].encrypt,
        "the composer shows the toggle and why"
    );
}

// ---- Drafts ------------------------------------------------------------------

/// Gmail holding the draft `written` as `r-1`, backed by the message `d1`
/// in thread `t7`, which the store holds too.
async fn with_draft(written: &Draft) -> Harness {
    let mut all = mail();
    all.push(labelled(
        meta("d1", "t7", ME, &written.subject, NOW),
        &[system_label::DRAFT],
    ));
    let h = Harness::with(all).await;
    let raw = compose::build_mime(written, NOW / 1000, "<d1@example.com>").expect("a draft");
    h.gmail.with(|i| {
        i.raws.insert("d1".into(), raw.clone());
        i.drafts.insert("r-1".into(), raw);
        i.draft_messages.insert("r-1".into(), "d1".into());
    });
    h
}

fn fern_swap() -> Draft {
    let mut draft = Draft::new(1, address(ME));
    draft.to = vec![address("ann@example.com")];
    draft.subject = "Fern swap".into();
    draft.markdown = "Swap on Sunday?".into();
    draft.attachments = vec![OutgoingAttachment {
        filename: "ferns.txt".into(),
        mime_type: "text/plain".into(),
        data: b"Maidenhair, hart's tongue.".to_vec(),
        content_id: None,
    }];
    draft
}

/// The draft Gmail holds as `r-1`, parsed.
fn gmail_draft(h: &Harness) -> Vec<u8> {
    h.gmail
        .with(|i| i.drafts.get("r-1").cloned())
        .expect("Gmail holds the draft")
}

#[tokio::test]
async fn list_drafts_gives_each_draft_with_its_people() {
    let h = with_draft(&fern_swap()).await;
    let listed = h.ok("list_drafts", json!({})).await;
    assert_eq!(listed["count"], 1);
    let draft = &listed["drafts"][0];
    assert_eq!(draft["account"], ME);
    assert_eq!(draft["message_id"], "d1");
    assert_eq!(draft["thread_id"], "t7");
    assert_eq!(draft["subject"], "Fern swap");
    assert_eq!(draft["to"], json!([ME]), "the fixture's metadata");
    assert!(h.asked().questions.is_empty(), "reading asks nothing");
}

#[tokio::test]
async fn edit_draft_changes_what_it_names_and_keeps_the_rest() {
    let h = with_draft(&fern_swap()).await;
    let done = h
        .ok(
            "edit_draft",
            json!({
                "account": ME,
                "message_id": "d1",
                "subject": "Fern swap, Sunday",
                "bcc": ["di@example.com"],
                "remove_attachments": ["FERNS.txt", "moss.txt"],
            }),
        )
        .await;
    assert_eq!(
        h.asked().questions,
        [
            "Change the draft “Fern swap”?\n\nBcc: di@example.com\nSubject: Fern swap, Sunday\nTakes out: FERNS.txt, moss.txt"
        ]
    );
    assert_eq!(done["saved"], true);
    assert_eq!(
        done["not_found"],
        "The draft had no file called moss.txt, so nothing was removed for that name."
    );

    let raw = gmail_draft(&h);
    let parsed = MessageParser::default().parse(&raw).expect("a message");
    assert_eq!(parsed.subject(), Some("Fern swap, Sunday"));
    assert_eq!(
        parsed
            .to()
            .and_then(|a| a.first())
            .and_then(|a| a.address()),
        Some("ann@example.com"),
        "To stays as it was"
    );
    assert_eq!(
        parsed
            .bcc()
            .and_then(|a| a.first())
            .and_then(|a| a.address()),
        Some("di@example.com")
    );
    assert!(
        parsed
            .body_text(0)
            .is_some_and(|t| t.contains("Swap on Sunday?")),
        "the words stay"
    );
    assert_eq!(parsed.attachments().count(), 0, "the file came out");
    assert_eq!(
        h.asked().saved_drafts[0].draft_id.as_deref(),
        Some("r-1"),
        "saved over the same draft"
    );
}

#[tokio::test]
async fn new_words_replace_the_styled_body_too() {
    let h = with_draft(&fern_swap()).await;
    h.ok(
        "edit_draft",
        json!({"account": ME, "message_id": "d1", "body": "Saturday instead?"}),
    )
    .await;
    let raw = gmail_draft(&h);
    let parsed = MessageParser::default().parse(&raw).expect("a message");
    let text = parsed.body_text(0).expect("text");
    assert!(text.contains("Saturday instead?"), "{text}");
    assert!(!text.contains("Sunday"), "{text}");
    let html = parsed.body_html(0).expect("html");
    assert!(html.contains("Saturday instead?"), "{html}");
    assert_eq!(
        parsed
            .attachments()
            .filter_map(|a| a.attachment_name())
            .collect::<Vec<_>>(),
        ["ferns.txt"],
        "the file stays"
    );
}

#[tokio::test]
async fn an_encrypted_draft_is_changed_and_saved_encrypted_again() {
    let mut sealed = fern_swap();
    sealed.encrypt = true;
    sealed.sign = true;
    let h = with_draft(&Draft::new(1, address(ME))).await;
    // Save it through the fake engine, as the composer would have, and
    // point the stored draft message at what Gmail now holds.
    use super::super::Effects;
    sealed.draft_id = Some("r-1".into());
    h.effects
        .save_draft(sealed)
        .await
        .expect("the fake engine seals it");
    let raw = gmail_draft(&h);
    assert_eq!(protection::draft::standard_of(&raw), Some(Standard::Pgp));
    h.gmail.with(|i| {
        i.raws.insert("d1".into(), raw);
        i.draft_messages.insert("r-1".into(), "d1".into());
    });

    h.ok(
        "edit_draft",
        json!({"account": ME, "message_id": "d1", "to": ["bo@example.com"]}),
    )
    .await;
    let raw = gmail_draft(&h);
    assert_eq!(
        protection::draft::standard_of(&raw),
        Some(Standard::Pgp),
        "Gmail never holds it readable"
    );
    let saved = h.asked().saved_drafts.last().cloned().expect("saved");
    assert!(saved.encrypt && saved.sign, "both switches come back on");
    assert_eq!(saved.to, [address("bo@example.com")]);
    assert_eq!(saved.markdown.trim(), "Swap on Sunday?");
    assert_eq!(saved.attachments[0].data, b"Maidenhair, hart's tongue.");
}

#[tokio::test]
async fn edit_draft_needs_a_draft_and_a_change() {
    let h = with_draft(&fern_swap()).await;
    assert_eq!(
        h.run(
            "edit_draft",
            json!({"account": ME, "message_id": "m1", "subject": "x"})
        )
        .await,
        Err(
            "There is no draft with that message_id. list_drafts gives the drafts and their ids."
                .into()
        )
    );
    assert!(
        h.run("edit_draft", json!({"account": ME, "message_id": "d1"}))
            .await
            .is_err_and(|e| e.starts_with("Name at least one change"))
    );
    assert!(h.asked().questions.is_empty());
}

#[tokio::test]
async fn a_declined_edit_or_delete_leaves_the_draft_alone() {
    let h = with_draft(&fern_swap()).await;
    let before = gmail_draft(&h);
    h.effects.asked.borrow_mut().approves = false;
    for (tool, input) in [
        (
            "edit_draft",
            json!({"account": ME, "message_id": "d1", "subject": "Gone"}),
        ),
        ("delete_draft", json!({"account": ME, "message_id": "d1"})),
    ] {
        assert_eq!(h.run(tool, input).await, Err("The user declined.".into()));
    }
    assert_eq!(gmail_draft(&h), before);
    assert!(h.asked().saved_drafts.is_empty());
}

#[tokio::test]
async fn delete_draft_asks_then_removes_it_from_gmail_and_the_list() {
    let h = with_draft(&fern_swap()).await;
    let done = h
        .ok("delete_draft", json!({"account": ME, "message_id": "d1"}))
        .await;
    assert_eq!(done, json!({"deleted": true, "subject": "Fern swap"}));
    assert_eq!(
        h.asked().questions,
        ["Delete the draft “Fern swap”? Gmail cannot bring it back."]
    );
    assert!(h.gmail.with(|i| i.drafts.is_empty()));
    assert_eq!(h.ok("list_drafts", json!({})).await["count"], 0);
    assert_eq!(h.asked().relisted, 1);
}
