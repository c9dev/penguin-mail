//! Putting a file from this computer in the account's Google Drive, so an
//! event can link to it. Sign-in asks for [`crate::DRIVE_FILE_SCOPE`],
//! which reaches only the files Penguin Mail itself puts there.
//!
//! The upload is one `multipart/related` request to Drive v3's upload
//! endpoint: the file's metadata as JSON, then its bytes. The bytes go out
//! as a stream read from the file in chunks, never read whole into
//! memory, and `Content-Length` names the total ahead of time as Drive
//! asks. Drive's guide calls this upload type the one for "a small file
//! (5 MB or less)" without saying it refuses a larger one.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use futures::stream::{self, Stream, StreamExt};
use mailrs_domain::calendar::Attachment;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::client::GmailClient;
use crate::error::GmailError;

pub const DRIVE_UPLOAD_BASE: &str = "https://www.googleapis.com/upload/drive/v3";

/// The fields an upload asks Drive to answer with: what an event's
/// attachment needs.
const FIELDS: &str = "id,name,mimeType,webViewLink,iconLink";

/// How much of the file one read takes.
const CHUNK: usize = 64 * 1024;

impl GmailClient {
    /// Points the Drive upload at another server. Tests use this.
    pub fn with_drive_upload_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.drive_upload_base_url = base_url.into();
        self
    }

    /// Uploads the file at `path` to the account's Drive as `name` and
    /// answers it as an event attachment, linked by Drive's own web link.
    /// `sent` counts the file's bytes as they go out, for a progress bar;
    /// it starts again at zero if the request has to be sent again. A file
    /// that is no longer at `path` answers [`GmailError::FileMissing`]
    /// before anything goes out. Dropping the future stops the upload.
    pub async fn upload_to_drive(
        &self,
        path: &Path,
        name: &str,
        mime_type: &str,
        sent: Arc<AtomicU64>,
    ) -> Result<Attachment, GmailError> {
        let size = tokio::fs::metadata(path).await.map_err(|err| file_error(path, &err))?.len();
        let boundary = format!("penguin-mail-{}", crate::random_token(24));
        let metadata = json!({"name": name, "mimeType": mime_type});
        let head = format!(
            "--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{metadata}\r\n\
             --{boundary}\r\nContent-Type: {mime_type}\r\n\r\n"
        )
        .into_bytes();
        let tail = format!("\r\n--{boundary}--\r\n").into_bytes();
        let length = head.len() as u64 + size + tail.len() as u64;
        let url = format!("{}/files", self.drive_upload_base_url);
        let answer: Value = self
            .call_at(&url, |url| {
                let body = body(head.clone(), path.to_path_buf(), size, tail.clone(), Arc::clone(&sent));
                self.http()
                    .post(url)
                    .query(&[("uploadType", "multipart"), ("fields", FIELDS)])
                    .header(reqwest::header::CONTENT_TYPE, format!("multipart/related; boundary={boundary}"))
                    .header(reqwest::header::CONTENT_LENGTH, length)
                    .body(reqwest::Body::wrap_stream(body))
            })
            .await?;
        let text = |key: &str| answer.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
        let title = Some(text("name")).filter(|n| !n.is_empty()).unwrap_or_else(|| name.to_string());
        let mime = Some(text("mimeType")).filter(|m| !m.is_empty()).unwrap_or_else(|| mime_type.to_string());
        Ok(Attachment {
            title,
            file_url: text("webViewLink"),
            mime_type: mime,
            icon_link: text("iconLink"),
            file_id: text("id"),
            waiting: None,
        })
    }
}

fn file_error(path: &Path, err: &std::io::Error) -> GmailError {
    match err.kind() {
        std::io::ErrorKind::NotFound => GmailError::FileMissing(path.display().to_string()),
        _ => GmailError::File(err.to_string()),
    }
}

/// The request body: `head`, the file's first `size` bytes read a chunk
/// at a time, then `tail`. The file opens when the body is first polled,
/// so a request built again opens it again.
fn body(
    head: Vec<u8>,
    path: PathBuf,
    size: u64,
    tail: Vec<u8>,
    sent: Arc<AtomicU64>,
) -> impl Stream<Item = std::io::Result<Vec<u8>>> + Send + 'static {
    enum Reading {
        Closed(PathBuf),
        Open(Box<dyn AsyncRead + Send + Unpin>),
        Done,
    }
    let file = stream::unfold(Reading::Closed(path), move |state| {
        let sent = Arc::clone(&sent);
        async move {
            let mut reader = match state {
                Reading::Done => return None,
                Reading::Open(reader) => reader,
                Reading::Closed(path) => match tokio::fs::File::open(&path).await {
                    Ok(file) => {
                        sent.store(0, Ordering::SeqCst);
                        Box::new(file.take(size)) as Box<dyn AsyncRead + Send + Unpin>
                    }
                    Err(err) => return Some((Err(err), Reading::Done)),
                },
            };
            let mut chunk = vec![0; CHUNK];
            match reader.read(&mut chunk).await {
                Ok(0) => None,
                Ok(read) => {
                    chunk.truncate(read);
                    sent.fetch_add(read as u64, Ordering::SeqCst);
                    Some((Ok(chunk), Reading::Open(reader)))
                }
                Err(err) => Some((Err(err), Reading::Done)),
            }
        }
    });
    stream::once(async move { Ok(head) }).chain(file).chain(stream::once(async move { Ok(tail) }))
}
