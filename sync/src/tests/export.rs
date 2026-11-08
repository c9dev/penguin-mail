use crate::export::{Replacing, append, file_name};

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
#[tokio::test]
async fn a_file_written_in_full_takes_the_place_of_the_old_one() {
    use tokio::io::AsyncWriteExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Lunch.mbox");
    std::fs::write(&path, b"old").unwrap();
    let mut file = Replacing::create(&path).await.unwrap();
    file.file().write_all(b"new mail").await.unwrap();
    assert_eq!(file.finish().await.unwrap(), 8);
    assert_eq!(std::fs::read(&path).unwrap(), b"new mail");
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        1,
        "no part file left"
    );
}

#[tokio::test]
async fn a_file_given_up_part_way_leaves_the_old_one() {
    use tokio::io::AsyncWriteExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Lunch.mbox");
    std::fs::write(&path, b"old").unwrap();
    let mut file = Replacing::create(&path).await.unwrap();
    file.file().write_all(b"half").await.unwrap();
    drop(file);
    assert_eq!(std::fs::read(&path).unwrap(), b"old");
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        1,
        "no part file left"
    );
}

/// A link to the file is written through: the file it names takes the new
/// bytes, and the link stays a link.
#[tokio::test]
#[cfg(unix)]
async fn a_link_to_the_file_is_written_through() {
    use tokio::io::AsyncWriteExt;
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("kept.mbox");
    std::fs::write(&real, b"old").unwrap();
    let link = dir.path().join("Lunch.mbox");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let mut file = Replacing::create(&link).await.unwrap();
    file.file().write_all(b"new mail").await.unwrap();
    assert_eq!(file.finish().await.unwrap(), 8);
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read(&real).unwrap(), b"new mail");
}

/// A device or a pipe, such as /dev/stdout, takes the bytes as they come.
#[tokio::test]
#[cfg(unix)]
async fn a_special_file_is_written_straight_to() {
    use tokio::io::AsyncWriteExt;
    let mut file = Replacing::create(std::path::Path::new("/dev/null"))
        .await
        .unwrap();
    file.file().write_all(b"new mail").await.unwrap();
    assert_eq!(file.finish().await.unwrap(), 8);
    assert!(!std::path::Path::new("/dev/null.part").exists());
}

#[tokio::test]
#[cfg(unix)]
async fn the_new_file_keeps_the_old_one_s_mode() {
    use std::os::unix::fs::PermissionsExt;
    use tokio::io::AsyncWriteExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Lunch.mbox");
    std::fs::write(&path, b"old").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut file = Replacing::create(&path).await.unwrap();
    file.file().write_all(b"new mail").await.unwrap();
    file.finish().await.unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

/// A part file already there, even a link someone planted, is no file to
/// write into: it goes, and a new one is made.
#[tokio::test]
#[cfg(unix)]
async fn a_part_file_already_there_is_never_written_through() {
    use tokio::io::AsyncWriteExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Lunch.mbox");
    let victim = dir.path().join("victim");
    std::fs::write(&victim, b"keep me").unwrap();
    std::os::unix::fs::symlink(&victim, dir.path().join("Lunch.mbox.part")).unwrap();
    let mut file = Replacing::create(&path).await.unwrap();
    file.file().write_all(b"new mail").await.unwrap();
    file.finish().await.unwrap();
    assert_eq!(std::fs::read(&victim).unwrap(), b"keep me");
    assert_eq!(std::fs::read(&path).unwrap(), b"new mail");
}

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

    #[tokio::test]
    async fn the_raw_message_is_handed_out_not_copied() {
        let h = harness().await;
        h.fake.seed(meta("one", "t1", DAY, &["INBOX"]));
        let raw = h.sync.raw_message("one").await.unwrap();
        let cached = h
            .sync
            .cached_raw("one")
            .expect("a small message stays cached");
        assert!(std::sync::Arc::ptr_eq(&raw, &cached));
    }

    /// Collects each write the exporter makes, so a test can see how the
    /// file was fed.
    #[derive(Default)]
    struct Writes(Vec<Vec<u8>>);

    impl tokio::io::AsyncWrite for Writes {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            self.0.push(buf.to_vec());
            std::task::Poll::Ready(Ok(buf.len()))
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn a_conversation_streams_to_a_writer_one_message_at_a_time() {
        let h = harness().await;
        h.fake.seed(meta("second", "t1", 2 * DAY, &["INBOX"]));
        h.fake.seed(meta("first", "t1", DAY, &["INBOX"]));
        // Fixed bytes, since the fake's own messages carry a random boundary.
        h.fake.with(|s| {
            for id in ["first", "second"] {
                s.raws.insert(
                    id.into(),
                    format!("From: ann@example.com\r\nDate: Mon, 5 Jan 2026 09:07:03 +0000\r\n\r\nFrom {id}\r\n").into_bytes(),
                );
            }
        });
        let whole = String::from_utf8(h.sync.export_mbox("t1", None).await.unwrap()).unwrap();
        let mut writes = Writes::default();
        h.sync
            .export_mbox_to("t1", None, &mut writes)
            .await
            .unwrap();
        assert_eq!(writes.0.len(), 2, "one write per message");
        assert!(writes.0.iter().all(|w| w.starts_with(b"From ")));
        assert_eq!(String::from_utf8(writes.0.concat()).unwrap(), whole);
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
