//! Putting a file from this computer in the account's Google Drive, so an
//! event can link to it, and letting the event's guests open it. Sign-in
//! asks for [`crate::DRIVE_FILE_SCOPE`], which reaches only the files
//! Penguin Mail itself puts there, and lets it share those.
//!
//! A file of 5 MB or less goes up as one `multipart/related` request: the
//! metadata as JSON, then the bytes. Drive's guide names that upload type
//! for small files, so a larger one goes through a resumable session:
//! one request opens it, then the bytes follow in chunks, each a request
//! of its own. After a dropped connection or a server error the client
//! asks the session how much it holds and sends the rest from there.
//! Either way the bytes are read from the file a piece at a time, never
//! whole into memory, and `Content-Length` names each request's length
//! ahead of time.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures::stream::{self, Stream, StreamExt};
use mailrs_domain::calendar::Attachment;
use reqwest::StatusCode;
use reqwest::header::{CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, LOCATION, RANGE};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::client::{GmailClient, error_from_response};
use crate::error::GmailError;

pub const DRIVE_UPLOAD_BASE: &str = "https://www.googleapis.com/upload/drive/v3";
pub const DRIVE_API_BASE: &str = "https://www.googleapis.com/drive/v3";

/// The fields an upload asks Drive to answer with: what an event's
/// attachment needs.
const FIELDS: &str = "id,name,mimeType,webViewLink,iconLink";

/// How much of the file one read takes.
const PIECE: usize = 64 * 1024;

/// The largest file that goes up in one multipart request.
pub const MULTIPART_MOST: u64 = 5 * 1024 * 1024;

/// The length of one chunk of a resumable upload. Drive wants a multiple
/// of 256 KiB for every chunk but the last.
const CHUNK: u64 = 8 * 1024 * 1024;

/// How many times in a row a resumable upload asks again after a server
/// error or a dropped connection before it gives up.
const MOST_RETRIES: u32 = 5;

impl GmailClient {
    /// Points the Drive upload at another server. Tests use this.
    pub fn with_drive_upload_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.drive_upload_base_url = base_url.into();
        self
    }

    /// Points Drive's own API, which sharing uses, at another server.
    /// Tests use this.
    pub fn with_drive_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.drive_base_url = base_url.into();
        self
    }

    /// Uploads the file at `path` to the account's Drive as `name` and
    /// answers it as an event attachment, linked by Drive's own web link.
    /// `sent` counts the file's bytes as Drive takes them, for a progress
    /// bar. A file that is no longer at `path` answers
    /// [`GmailError::FileMissing`] before anything goes out. Dropping the
    /// future stops the upload, and ends a resumable session on Drive.
    pub async fn upload_to_drive(
        &self,
        path: &Path,
        name: &str,
        mime_type: &str,
        sent: Arc<AtomicU64>,
    ) -> Result<Attachment, GmailError> {
        let size = tokio::fs::metadata(path).await.map_err(|err| file_error(path, &err))?.len();
        let answer = if size <= MULTIPART_MOST {
            self.upload_multipart(path, name, mime_type, size, &sent).await?
        } else {
            self.upload_resumable(path, name, mime_type, size, &sent).await?
        };
        Ok(attachment(&answer, name, mime_type))
    }

    /// Makes `email` a reader of the Drive file `file_id`, one the app
    /// uploaded, without Drive mailing them: the event's own invitation
    /// tells them about it.
    pub async fn share_file(&self, file_id: &str, email: &str) -> Result<(), GmailError> {
        let url = format!("{}/files/{}/permissions", self.drive_base_url, crate::calendar::encode(file_id));
        let body = json!({"role": "reader", "type": "user", "emailAddress": email});
        let _: Value = self
            .call_at(&url, |url| {
                self.http()
                    .post(url)
                    .query(&[("sendNotificationEmail", "false"), ("fields", "id")])
                    .json(&body)
            })
            .await?;
        Ok(())
    }

    async fn upload_multipart(
        &self,
        path: &Path,
        name: &str,
        mime_type: &str,
        size: u64,
        sent: &Arc<AtomicU64>,
    ) -> Result<Value, GmailError> {
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
        self.call_at(&url, |url| {
            let (head, tail) = (head.clone(), tail.clone());
            let file = file_stream(path.to_path_buf(), 0, size, Arc::clone(sent));
            let body = stream::once(async move { Ok(head) }).chain(file).chain(stream::once(async move { Ok(tail) }));
            self.http()
                .post(url)
                .query(&[("uploadType", "multipart"), ("fields", FIELDS)])
                .header(CONTENT_TYPE, format!("multipart/related; boundary={boundary}"))
                .header(CONTENT_LENGTH, length)
                .body(reqwest::Body::wrap_stream(body))
        })
        .await
    }

    async fn upload_resumable(
        &self,
        path: &Path,
        name: &str,
        mime_type: &str,
        size: u64,
        sent: &Arc<AtomicU64>,
    ) -> Result<Value, GmailError> {
        let url = format!("{}/files", self.drive_upload_base_url);
        let metadata = json!({"name": name, "mimeType": mime_type});
        let opened = self
            .send_any(|| {
                self.http()
                    .post(&url)
                    .query(&[("uploadType", "resumable"), ("fields", FIELDS)])
                    .header("X-Upload-Content-Type", mime_type)
                    .header("X-Upload-Content-Length", size)
                    .json(&metadata)
            })
            .await?;
        if !opened.status().is_success() {
            return Err(error_from_response(opened).await);
        }
        let session = opened
            .headers()
            .get(LOCATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| GmailError::Decode("Drive opened an upload session without a Location".into()))?
            .to_string();
        let mut open = OpenSession {
            http: self.http().clone(),
            session: session.clone(),
            token: self.token().await?,
            done: false,
        };
        let mut offset = 0;
        let mut failures = 0;
        let mut lost = false;
        loop {
            let answer = if lost {
                self.send_any(|| {
                    self.http().put(&session).header(CONTENT_RANGE, format!("bytes */{size}")).header(CONTENT_LENGTH, 0)
                })
                .await
            } else {
                let length = CHUNK.min(size - offset);
                sent.store(offset, Ordering::SeqCst);
                self.send_any(|| {
                    let body = file_stream(path.to_path_buf(), offset, length, Arc::clone(sent));
                    self.http()
                        .put(&session)
                        .header(CONTENT_RANGE, format!("bytes {offset}-{}/{size}", offset + length - 1))
                        .header(CONTENT_LENGTH, length)
                        .body(reqwest::Body::wrap_stream(body))
                })
                .await
            };
            let failed = match answer {
                Ok(response) if matches!(response.status(), StatusCode::OK | StatusCode::CREATED) => {
                    open.done = true;
                    sent.store(size, Ordering::SeqCst);
                    return response.json().await.map_err(|e| GmailError::Decode(e.to_string()));
                }
                Ok(response) if response.status() == StatusCode::PERMANENT_REDIRECT => {
                    // Drive's 308 says how much it holds: `Range:
                    // bytes=0-N`, or no header when it holds nothing yet.
                    offset = held(&response);
                    sent.store(offset, Ordering::SeqCst);
                    lost = false;
                    failures = 0;
                    continue;
                }
                Ok(response) if response.status().is_server_error() => error_from_response(response).await,
                Ok(response) => return Err(error_from_response(response).await),
                Err(err @ GmailError::Network(_)) => err,
                Err(err) => return Err(err),
            };
            // A server error or a dropped connection: wait, then ask the
            // session how much it holds.
            if failures >= MOST_RETRIES {
                return Err(failed);
            }
            failures += 1;
            lost = true;
            tokio::time::sleep(Duration::from_millis(100 << failures)).await;
        }
    }
}

/// A resumable session still open on Drive. Dropping it before the upload
/// finished, as Cancel does by dropping the upload, ends the session so
/// Drive keeps no half-sent file.
struct OpenSession {
    http: reqwest::Client,
    session: String,
    token: String,
    done: bool,
}

impl Drop for OpenSession {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else { return };
        let request = self.http.delete(&self.session).bearer_auth(&self.token);
        runtime.spawn(async move {
            // Drive answers 499 to the end of a session; there is nothing
            // more to do either way.
            let _ = request.send().await;
        });
    }
}

/// How many bytes a 308 says the session holds.
fn held(response: &reqwest::Response) -> u64 {
    response
        .headers()
        .get(RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|range| range.rsplit_once('-'))
        .and_then(|(_, last)| last.trim().parse::<u64>().ok())
        .map_or(0, |last| last + 1)
}

fn attachment(answer: &Value, name: &str, mime_type: &str) -> Attachment {
    let text = |key: &str| answer.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
    let title = Some(text("name")).filter(|n| !n.is_empty()).unwrap_or_else(|| name.to_string());
    let mime = Some(text("mimeType")).filter(|m| !m.is_empty()).unwrap_or_else(|| mime_type.to_string());
    Attachment {
        title,
        file_url: text("webViewLink"),
        mime_type: mime,
        icon_link: text("iconLink"),
        file_id: text("id"),
        ..Attachment::default()
    }
}

fn file_error(path: &Path, err: &std::io::Error) -> GmailError {
    match err.kind() {
        std::io::ErrorKind::NotFound => GmailError::FileMissing(path.display().to_string()),
        _ => GmailError::File(err.to_string()),
    }
}

/// `length` bytes of the file at `path` from `offset`, read a piece at a
/// time. The file opens when the stream is first polled, so a request
/// built again opens it again. `sent` goes back to `offset` then and
/// counts each piece as it goes.
fn file_stream(
    path: PathBuf,
    offset: u64,
    length: u64,
    sent: Arc<AtomicU64>,
) -> impl Stream<Item = std::io::Result<Vec<u8>>> + Send + 'static {
    enum Reading {
        Closed(PathBuf),
        Open(tokio::io::Take<tokio::fs::File>),
        Done,
    }
    stream::unfold(Reading::Closed(path), move |state| {
        let sent = Arc::clone(&sent);
        async move {
            let mut reader = match state {
                Reading::Done => return None,
                Reading::Open(reader) => reader,
                Reading::Closed(path) => {
                    let opened = async {
                        let mut file = tokio::fs::File::open(&path).await?;
                        file.seek(std::io::SeekFrom::Start(offset)).await?;
                        Ok::<_, std::io::Error>(file.take(length))
                    };
                    match opened.await {
                        Ok(reader) => {
                            sent.store(offset, Ordering::SeqCst);
                            reader
                        }
                        Err(err) => return Some((Err(err), Reading::Done)),
                    }
                }
            };
            let mut piece = vec![0; PIECE];
            match reader.read(&mut piece).await {
                Ok(0) => None,
                Ok(read) => {
                    piece.truncate(read);
                    sent.fetch_add(read as u64, Ordering::SeqCst);
                    Some((Ok(piece), Reading::Open(reader)))
                }
                Err(err) => Some((Err(err), Reading::Done)),
            }
        }
    })
}
