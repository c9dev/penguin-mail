use base64::Engine;

use super::{days_ago, message};
use crate::RAW_LIMIT;
use crate::tests::imap_harness;

/// A message with a line of text and a file of `size` bytes, the file in
/// base64 wrapped at 76 characters.
fn with_file(size: usize) -> Vec<u8> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(vec![b'k'; size]);
    let wrapped: Vec<&str> = encoded
        .as_bytes()
        .chunks(76)
        .map(|line| std::str::from_utf8(line).unwrap())
        .collect();
    format!(
        "From: Ann <ann@example.com>\r\nTo: me@example.com\r\nSubject: Plans\r\n\
         Message-ID: <plans@example.com>\r\nMIME-Version: 1.0\r\n\
         Content-Type: multipart/mixed; boundary=\"b\"\r\n\r\n\
         --b\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nThe plans are attached.\r\n\
         --b\r\nContent-Type: application/octet-stream; name=\"plans.bin\"\r\n\
         Content-Disposition: attachment; filename=\"plans.bin\"\r\n\
         Content-Transfer-Encoding: base64\r\n\r\n{}\r\n--b--\r\n",
        wrapped.join("\r\n")
    )
    .into_bytes()
}

#[tokio::test]
async fn a_small_message_is_read_whole_and_kept_for_its_files() {
    let h = imap_harness().await;
    h.imap.deliver_flagged("INBOX", &message("a", "Kites", ""), &[], days_ago(1));
    h.bootstrap().await;

    let body = h.sync.body("INBOX/1001/1").await.unwrap();

    assert_eq!(body.text.as_deref().map(str::trim), Some("Hi."));
    assert!(h.sync.cached_raw("INBOX/1001/1").is_some());
}

#[tokio::test]
async fn a_large_message_is_read_by_its_structure_and_its_file_comes_alone() {
    let h = imap_harness().await;
    let size = RAW_LIMIT as usize;
    h.imap.deliver_flagged("INBOX", &with_file(size), &[], days_ago(1));
    h.bootstrap().await;

    let body = h.sync.body("INBOX/1001/1").await.unwrap();
    let file = h.sync.attachment("INBOX/1001/1", "2").await.unwrap();

    assert_eq!(body.text.as_deref().map(str::trim), Some("The plans are attached."));
    assert_eq!(body.attachments[0].filename, "plans.bin");
    assert_eq!(file, vec![b'k'; size]);
    assert!(
        h.sync.cached_raw("INBOX/1001/1").is_none(),
        "the whole message never came down"
    );
}
