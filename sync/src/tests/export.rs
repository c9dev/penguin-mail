use crate::export::{append, file_name};

/// A message with the headers an mbox separator is built from.
fn message(from: &str, body: &str) -> Vec<u8> {
    format!("From: {from}\nDate: Mon, 5 Jan 2026 09:07:03 +0000\nSubject: Lunch\n\n{body}")
        .into_bytes()
}

fn mbox(raw: &[u8]) -> String {
    let mut out = Vec::new();
    append(&mut out, raw);
    String::from_utf8(out).unwrap()
}

#[test]
fn the_separator_names_the_sender_and_dates_it_the_asctime_way() {
    let out = mbox(&message("Ann <ann@example.com>", "Hello.\n"));
    assert!(
        out.starts_with("From ann@example.com Mon Jan  5 09:07:03 2026\n"),
        "{out}"
    );
}

#[test]
fn a_body_line_that_reads_from_is_quoted() {
    let out = mbox(&message("ann@example.com", "From here on.\nPlain line.\n"));
    assert!(out.contains("\n>From here on.\nPlain line.\n"), "{out}");
}

#[test]
fn a_line_that_is_already_quoted_takes_one_more_mark() {
    let out = mbox(&message("ann@example.com", ">From me.\n>>From me.\n"));
    assert!(out.contains("\n>>From me.\n>>>From me.\n"), "{out}");
}

#[test]
fn a_from_without_its_space_is_left_alone() {
    let out = mbox(&message("ann@example.com", "Fromage.\n>Fromage.\n"));
    assert!(out.contains("\nFromage.\n>Fromage.\n"), "{out}");
}

#[test]
fn crlf_endings_survive_the_quoting() {
    let raw = b"From: ann@example.com\r\nDate: Mon, 5 Jan 2026 09:07:03 +0000\r\n\r\nFrom here.\r\nEnd.\r\n";
    let out = mbox(raw);
    assert!(out.contains("\r\n\r\n>From here.\r\nEnd.\r\n"), "{out:?}");
    assert!(
        out.starts_with("From ann@example.com Mon Jan  5 09:07:03 2026\n"),
        "{out:?}"
    );
}

#[test]
fn a_message_with_no_sender_comes_from_the_mailer_daemon() {
    let raw = b"Date: Mon, 5 Jan 2026 09:07:03 +0000\nSubject: Bounce\n\nUndeliverable.\n";
    let out = mbox(raw);
    assert!(
        out.starts_with("From MAILER-DAEMON Mon Jan  5 09:07:03 2026\n"),
        "{out}"
    );
}

#[test]
fn a_message_with_no_date_falls_back_to_the_epoch() {
    let raw = b"From: ann@example.com\n\nNo date here.\n";
    let out = mbox(raw);
    assert!(
        out.starts_with("From ann@example.com Thu Jan  1 00:00:00 1970\n"),
        "{out}"
    );
}

#[test]
fn a_folded_from_header_still_gives_one_address() {
    let raw =
        b"From: Ann Example\n <ann@example.com>\nDate: Mon, 5 Jan 2026 09:07:03 +0000\n\nHi.\n";
    let out = mbox(raw);
    assert!(out.starts_with("From ann@example.com "), "{out}");
}

#[test]
fn two_messages_are_parted_by_a_blank_line() {
    let mut out = Vec::new();
    append(&mut out, &message("ann@example.com", "One.\n"));
    append(&mut out, &message("bob@example.com", "Two.\n"));
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("One.\n\nFrom bob@example.com "), "{text}");
}

#[test]
fn a_message_that_ends_without_a_newline_gets_one() {
    let out = mbox(&message("ann@example.com", "No trailing newline."));
    assert!(out.ends_with("No trailing newline.\n\n"), "{out:?}");
}

#[test]
fn a_file_name_keeps_the_subject_as_the_sender_wrote_it() {
    assert_eq!(
        file_name("Déjeuner à Paris", 1_767_603_623_000, "mbox"),
        "Déjeuner à Paris 2026-01-05.mbox"
    );
}

#[test]
fn a_file_name_drops_path_separators_and_control_characters() {
    assert_eq!(
        file_name("../etc\\passwd\ttab\nline", 1_767_603_623_000, "mbox"),
        "etc passwd tab line 2026-01-05.mbox"
    );
}

#[test]
fn a_subject_of_nothing_still_names_a_file() {
    assert_eq!(
        file_name("   ", 1_767_603_623_000, "eml"),
        "mail 2026-01-05.eml"
    );
}

/// A whole conversation, and one message of it, as the app exports them.
mod conversations {
    use mailrs_gmail::cost;

    use super::super::harness;
    use crate::fake::meta;

    const DAY: i64 = 24 * 60 * 60 * 1000;

    #[tokio::test]
    async fn a_conversation_exports_every_message_oldest_first() {
        let h = harness().await;
        h.fake.seed(meta("second", "t1", 2 * DAY, &["INBOX"]));
        h.fake.seed(meta("first", "t1", DAY, &["INBOX"]));
        let mbox = String::from_utf8(h.sync.export_mbox("t1", None).await.unwrap()).unwrap();
        let separators = mbox.lines().filter(|l| l.starts_with("From ")).count();
        assert_eq!(separators, 2, "{mbox}");
        let older = mbox.find("Subject first").expect("the older message");
        let newer = mbox.find("Subject second").expect("the newer message");
        assert!(older < newer, "{mbox}");
    }

    #[tokio::test]
    async fn one_message_of_a_conversation_exports_alone() {
        let h = harness().await;
        h.fake.seed(meta("second", "t1", 2 * DAY, &["INBOX"]));
        h.fake.seed(meta("first", "t1", DAY, &["INBOX"]));
        let mbox =
            String::from_utf8(h.sync.export_mbox("t1", Some("second")).await.unwrap()).unwrap();
        assert!(
            mbox.contains("Subject second") && !mbox.contains("Subject first"),
            "{mbox}"
        );
    }

    #[tokio::test]
    async fn a_body_that_reads_from_survives_the_round_trip() {
        let h = harness().await;
        h.fake.seed(meta("one", "t1", DAY, &["INBOX"]));
        h.fake.with(|s| {
            s.raws.insert(
                "one".into(),
                b"From: ann@example.com\r\nDate: Mon, 5 Jan 2026 09:07:03 +0000\r\n\r\nFrom Russia.\r\n".to_vec(),
            );
        });
        let mbox = String::from_utf8(h.sync.export_mbox("t1", None).await.unwrap()).unwrap();
        assert!(mbox.contains("\r\n\r\n>From Russia.\r\n"), "{mbox:?}");
    }

    #[tokio::test]
    async fn exporting_one_message_costs_a_thread_read_and_a_get() {
        let h = harness().await;
        h.fake.seed(meta("one", "t1", DAY, &["INBOX"]));
        h.sync.export_mbox("t1", None).await.unwrap();
        assert_eq!(h.fake.with(|s| s.usage.units), cost::THREAD + cost::GET);
    }

    /// Without a `Date` header every entry would sit at the epoch, which
    /// sorts an imported archive wrong.
    #[tokio::test]
    async fn the_fake_dates_the_mail_it_hands_back() {
        let h = harness().await;
        h.fake.seed(meta("one", "t1", DAY, &["INBOX"]));
        let mbox = String::from_utf8(h.sync.export_mbox("t1", None).await.unwrap()).unwrap();
        assert!(
            mbox.starts_with("From ann@example.com Fri Jan  2 "),
            "{mbox}"
        );
    }
}
