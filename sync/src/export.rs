//! Mail written out as the files other programs read: one message as the
//! RFC 822 bytes Gmail holds, a conversation as an mbox.
//!
//! The mbox is mboxrd, the form that survives a round trip. Every line of
//! the message that already reads `From `, however many `>` marks stand in
//! front of it, takes one more, so a reader strips one back off and gets
//! the message it started with. Nothing else about the message changes:
//! its own line endings, CRLF and all, go into the file as they arrived,
//! and only the lines mbox itself adds end in a bare newline.

use std::path::{Path, PathBuf};

use mailrs_domain::{EpochMillis, Target};
use mailrs_mime::address::parse_address_list;
use tokio::io::AsyncWriteExt;

use crate::{Accounts, MailActions, SyncError};

/// The sender an mbox separator names when the message has no usable
/// `From` header. Mail systems have written this since Unix mail began.
const UNKNOWN_SENDER: &str = "MAILER-DAEMON";

/// How many bytes of subject a file name may carry. Linux takes 255 bytes
/// for one name; the rest leaves room for the date and the extension.
const SUBJECT_BYTES: usize = 180;

impl<A: Accounts> MailActions<A> {
    /// The mail `targets` name as one mbox file, in the order given: each
    /// conversation oldest message first, or the one message a target
    /// names. The window's Export and the assistant both write this.
    pub async fn export_mbox(&self, targets: &[Target]) -> Result<Vec<u8>, SyncError> {
        let mut mbox = Vec::new();
        self.export_mbox_to(targets, &mut mbox).await?;
        Ok(mbox)
    }

    /// `export_mbox`, written to `out` one message at a time, so the file
    /// is fed as the messages arrive and the mail is never held whole.
    pub async fn export_mbox_to<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        targets: &[Target],
        out: &mut W,
    ) -> Result<(), SyncError> {
        for target in targets {
            let sync = self
                .accounts
                .account(target.account_id)
                .ok_or(SyncError::UnknownAccount(target.account_id))?;
            sync.export_mbox_to(&target.thread_id, target.message_id.as_deref(), out)
                .await?;
        }
        Ok(())
    }

    /// `export_mbox` into the file at `path`, written beside it under a
    /// temporary name and renamed over it when every message is in, so a
    /// failed export leaves what was there. Answers the file's length.
    pub async fn export_mbox_file(
        &self,
        targets: &[Target],
        path: &Path,
    ) -> Result<u64, SyncError> {
        let mut file = Replacing::create(path).await.map_err(SyncError::io)?;
        self.export_mbox_to(targets, file.file()).await?;
        file.finish().await.map_err(SyncError::io)
    }

    /// One message as the RFC 822 bytes Gmail holds, the whole of an
    /// `.eml` file.
    pub async fn export_message(
        &self,
        account_id: mailrs_domain::AccountId,
        message_id: &str,
    ) -> Result<std::sync::Arc<Vec<u8>>, SyncError> {
        let sync = self
            .accounts
            .account(account_id)
            .ok_or(SyncError::UnknownAccount(account_id))?;
        sync.raw_message(message_id).await
    }
}

/// A file written beside `path` under a temporary name and renamed over
/// it once whole, so a failed or interrupted write leaves what was there.
/// Dropped before [`Replacing::finish`], it takes the temporary file away.
///
/// A link at `path` is written through, so the file it names takes the
/// bytes and the link stays. A path that names no regular file, such as
/// `/dev/stdout` or a pipe, is written to as it is: there is nothing to
/// rename over.
pub struct Replacing {
    file: Option<Counted>,
    /// The temporary file, or `None` when the bytes go straight to `path`.
    partial: Option<PathBuf>,
    path: PathBuf,
}

impl Replacing {
    pub async fn create(path: &Path) -> std::io::Result<Replacing> {
        let path = through_links(path).await;
        let found = tokio::fs::metadata(&path).await;
        if let Ok(meta) = &found
            && !meta.is_file()
        {
            let file = tokio::fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .await?;
            return Ok(Replacing {
                file: Some(Counted::new(file)),
                partial: None,
                path,
            });
        }
        let mut partial = path.as_os_str().to_owned();
        partial.push(".part");
        let partial = PathBuf::from(partial);
        let file = create_new(&partial).await?;
        // The new file reads and writes as the old one did.
        if let Ok(meta) = found {
            file.set_permissions(meta.permissions()).await?;
        }
        Ok(Replacing {
            file: Some(Counted::new(file)),
            partial: Some(partial),
            path,
        })
    }

    /// Where to write.
    pub fn file(&mut self) -> &mut (impl tokio::io::AsyncWrite + Unpin + use<>) {
        self.file.as_mut().expect("a file until finish")
    }

    /// Puts the written file in the place of the old one, once its bytes
    /// are on the disk: a rename that reached the disk before them would
    /// leave an empty file after a crash. Answers how many bytes went.
    pub async fn finish(mut self) -> std::io::Result<u64> {
        let mut out = self.file.take().expect("a file until finish");
        let Some(partial) = self.partial.take() else {
            out.flush().await?;
            return Ok(out.written);
        };
        let finished = async {
            out.flush().await?;
            out.file.sync_all().await?;
            tokio::fs::rename(&partial, &self.path).await?;
            Ok(out.written)
        }
        .await;
        if finished.is_err() {
            let _ = std::fs::remove_file(&partial);
        }
        finished
    }
}

impl Drop for Replacing {
    fn drop(&mut self) {
        if self.file.take().is_some()
            && let Some(partial) = &self.partial
        {
            let _ = std::fs::remove_file(partial);
        }
    }
}

/// The file a chain of links at `path` ends at, or `path` itself when it is
/// no link. A link that leads nowhere yet names the file to make.
async fn through_links(path: &Path) -> PathBuf {
    let mut path = path.to_path_buf();
    // Past this many, a loop of links answers the last one read.
    for _ in 0..32 {
        let Ok(meta) = tokio::fs::symlink_metadata(&path).await else {
            break;
        };
        if !meta.file_type().is_symlink() {
            break;
        }
        let Ok(next) = tokio::fs::read_link(&path).await else {
            break;
        };
        path = match path.parent() {
            Some(parent) if next.is_relative() => parent.join(next),
            _ => next,
        };
    }
    path
}

/// A new file at `path`, made by this call and by no link: a part file a
/// crash left, or a link planted under its name, goes first.
async fn create_new(path: &Path) -> std::io::Result<tokio::fs::File> {
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    match options.open(path).await {
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            tokio::fs::remove_file(path).await?;
            options.open(path).await
        }
        opened => opened,
    }
}

/// A file that counts the bytes written to it, which a device or a pipe
/// cannot say afterwards.
struct Counted {
    file: tokio::fs::File,
    written: u64,
}

impl Counted {
    fn new(file: tokio::fs::File) -> Self {
        Counted { file, written: 0 }
    }
}

impl tokio::io::AsyncWrite for Counted {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let polled = std::pin::Pin::new(&mut self.file).poll_write(cx, buf);
        if let std::task::Poll::Ready(Ok(n)) = &polled {
            self.written += *n as u64;
        }
        polled
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.file).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.file).poll_shutdown(cx)
    }
}

/// Adds `raw` to `out` as one mbox entry: a `From ` separator built from
/// the message's own headers, the message with its `From ` lines quoted,
/// and the blank line that parts it from whatever comes next.
pub fn append(out: &mut Vec<u8>, raw: &[u8]) {
    out.extend_from_slice(separator(raw).as_bytes());
    for line in lines(raw) {
        if reads_from(line) {
            out.push(b'>');
        }
        out.extend_from_slice(line);
    }
    if !out.ends_with(b"\n") {
        out.push(b'\n');
    }
    out.push(b'\n');
}

/// A name for the file this mail is saved to, such as
/// `Lunch plans 2026-01-05.mbox`. The subject is the sender's text, so
/// path separators and control characters come out as spaces before it
/// reaches a save dialog. The date is UTC, which names one message one
/// way wherever it is exported.
pub fn file_name(subject: &str, date: EpochMillis, extension: &str) -> String {
    let mut name = String::new();
    let mut space = true;
    for c in subject.chars() {
        let plain = if c == '/' || c == '\\' || c.is_control() {
            ' '
        } else {
            c
        };
        if plain.is_whitespace() {
            space = true;
            continue;
        }
        if space && !name.is_empty() {
            name.push(' ');
        }
        if name.len() + plain.len_utf8() > SUBJECT_BYTES {
            break;
        }
        name.push(plain);
        space = false;
    }
    // A leading dot hides the file, and a pair of them climbs a directory.
    let name = name.trim_start_matches('.').trim();
    let name = if name.is_empty() { "mail" } else { name };
    // A message with no Date header carries 0, which is no date to stamp.
    match chrono::DateTime::from_timestamp_millis(date).filter(|_| date != 0) {
        Some(day) => format!("{name} {}.{extension}", day.format("%Y-%m-%d")),
        None => format!("{name}.{extension}"),
    }
}

/// The `From ` line that opens an entry: the sender's address, then the
/// date in the form `ctime` prints, kept in the offset the message was
/// written in. A message that names neither comes from the mailer daemon
/// at the epoch, which reads as the placeholder it is.
fn separator(raw: &[u8]) -> String {
    let sender = header(raw, "From")
        .and_then(|from| parse_address_list(&from).into_iter().next())
        .map(|address| address.email)
        .filter(|email| !email.is_empty() && !email.contains(char::is_whitespace))
        .unwrap_or_else(|| UNKNOWN_SENDER.to_string());
    let date = header(raw, "Date")
        .and_then(|date| chrono::DateTime::parse_from_rfc2822(&date).ok())
        .map(|date| date.format("%a %b %e %H:%M:%S %Y").to_string())
        .unwrap_or_else(|| "Thu Jan  1 00:00:00 1970".to_string());
    format!("From {sender} {date}\n")
}

/// Whether `line` needs another quote mark: it reads `From `, behind any
/// number of marks it already carries.
fn reads_from(line: &[u8]) -> bool {
    match line.iter().position(|b| *b != b'>') {
        Some(at) => line[at..].starts_with(b"From "),
        None => false,
    }
}

/// The value of header `name`, folded lines joined. A message may repeat
/// a header; this takes the first, as a reader does.
fn header(raw: &[u8], name: &str) -> Option<String> {
    let mut value: Option<String> = None;
    for line in lines(raw) {
        let line = match line.strip_suffix(b"\n") {
            Some(line) => line.strip_suffix(b"\r").unwrap_or(line),
            None => line,
        };
        if line.is_empty() {
            break;
        }
        if line[0] == b' ' || line[0] == b'\t' {
            if let Some(held) = value.as_mut() {
                held.push(' ');
                held.push_str(String::from_utf8_lossy(line).trim());
            }
            continue;
        }
        if value.is_some() {
            break;
        }
        if let Some(colon) = line.iter().position(|b| *b == b':')
            && line[..colon].eq_ignore_ascii_case(name.as_bytes())
        {
            value = Some(
                String::from_utf8_lossy(&line[colon + 1..])
                    .trim()
                    .to_string(),
            );
        }
    }
    value
}

/// The message's lines, each keeping the terminator it came with. The last
/// one comes as it is when the message ends without a newline.
fn lines(raw: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = raw;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let end = rest
            .iter()
            .position(|b| *b == b'\n')
            .map_or(rest.len(), |at| at + 1);
        let (line, tail) = rest.split_at(end);
        rest = tail;
        Some(line)
    })
}

#[cfg(test)]
mod file_name_tests {
    use super::file_name;

    /// A message with no Date carries 0, which is no date, so the file is
    /// named by its subject alone rather than stamped with 1970.
    #[test]
    fn a_file_name_leaves_out_a_date_the_message_lacks() {
        assert_eq!(file_name("Kite plans", 0, "eml"), "Kite plans.eml");
        assert_eq!(file_name("", 0, "eml"), "mail.eml");
        assert_eq!(
            file_name("Kite plans", 1_773_000_000_000, "eml"),
            "Kite plans 2026-03-08.eml"
        );
    }
}
