//! Putting a file in the account's Google Drive, against a stand-in
//! upload endpoint. `wiremock` checks the multipart body and its headers.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use mailrs_gmail::{GmailClient, GmailError, OAuthClient};
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const UPLOAD: &str = "/upload/drive/v3";

async fn mount_token(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"access_token": "at-1", "expires_in": 3600})))
        .mount(server)
        .await;
}

const DRIVE: &str = "/drive/v3";

fn client(server: &MockServer) -> GmailClient {
    let oauth = OAuthClient::new("cid", "secret")
        .with_endpoints(format!("{}/auth", server.uri()), format!("{}/token", server.uri()));
    GmailClient::new(oauth, "rt".into())
        .with_drive_upload_base_url(format!("{}{UPLOAD}", server.uri()))
        .with_drive_base_url(format!("{}{DRIVE}", server.uri()))
}

/// A file of `size` bytes in a folder of its own, which goes when the
/// returned guard drops.
fn file_of(size: usize) -> (std::path::PathBuf, TempDir) {
    file_named(size, "one")
}

fn file_named(size: usize, tag: &str) -> (std::path::PathBuf, TempDir) {
    let dir = TempDir::new_tagged(tag);
    let path = dir.0.join("Agenda.pdf");
    let bytes: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
    std::fs::write(&path, bytes).unwrap();
    (path, dir)
}

struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new() -> TempDir {
        TempDir::new_tagged("one")
    }

    fn new_tagged(tag: &str) -> TempDir {
        let dir = std::env::temp_dir().join(format!("penguin-drive-{}-{tag}", rand_suffix()));
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn rand_suffix() -> String {
    format!("{}-{:?}", std::process::id(), std::thread::current().id()).replace(['(', ')'], "")
}

/// The two parts of a `multipart/related` body, split on `boundary`.
fn parts(body: &[u8], boundary: &str) -> Vec<Vec<u8>> {
    let marker = format!("--{boundary}");
    let text = body;
    let mut found = Vec::new();
    let mut at = 0;
    let positions: Vec<usize> = (0..text.len())
        .filter(|&i| text[i..].starts_with(marker.as_bytes()))
        .collect();
    for pair in positions.windows(2) {
        at = pair[1];
        found.push(text[pair[0] + marker.len()..pair[1]].to_vec());
    }
    assert!(text[at..].starts_with(format!("{marker}--").as_bytes()), "the body ends with the closing boundary");
    found
}

#[tokio::test]
async fn an_upload_sends_the_metadata_then_the_file_in_one_multipart_body() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    let size = 200_000;
    let (file, _dir) = file_of(size);
    Mock::given(method("POST"))
        .and(path(format!("{UPLOAD}/files")))
        .and(query_param("uploadType", "multipart"))
        .and(header("authorization", "Bearer at-1"))
        .respond_with(move |request: &Request| {
            let kind = request.headers.get("content-type").unwrap().to_str().unwrap().to_string();
            let boundary = kind.strip_prefix("multipart/related; boundary=").expect("multipart/related").to_string();
            let length: usize = request.headers.get("content-length").unwrap().to_str().unwrap().parse().unwrap();
            assert_eq!(length, request.body.len(), "Content-Length names the whole body");
            let found = parts(&request.body, &boundary);
            assert_eq!(found.len(), 2);
            let (head, json) = split_head(&found[0]);
            assert!(head.contains("Content-Type: application/json; charset=UTF-8"), "{head}");
            let metadata: Value = serde_json::from_slice(json.strip_suffix(b"\r\n").unwrap()).unwrap();
            assert_eq!(metadata, json!({"name": "Agenda.pdf", "mimeType": "application/pdf"}));
            let (head, bytes) = split_head(&found[1]);
            assert!(head.contains("Content-Type: application/pdf"), "{head}");
            let bytes = bytes.strip_suffix(b"\r\n").unwrap();
            assert_eq!(bytes.len(), size);
            assert!(bytes.iter().enumerate().all(|(i, b)| *b == (i % 251) as u8));
            ResponseTemplate::new(200).set_body_json(json!({
                "id": "1abc",
                "name": "Agenda.pdf",
                "mimeType": "application/pdf",
                "webViewLink": "https://drive.google.com/file/d/1abc/view?usp=drivesdk",
                "iconLink": "https://drive-thirdparty.googleusercontent.com/16/type/application/pdf"
            }))
        })
        .expect(1)
        .mount(&server)
        .await;
    let sent = Arc::new(AtomicU64::new(0));
    let file = client(&server)
        .upload_to_drive(&file, "Agenda.pdf", "application/pdf", Arc::clone(&sent))
        .await
        .unwrap();
    assert_eq!(file.file_id, "1abc");
    assert_eq!(file.title, "Agenda.pdf");
    assert_eq!(file.mime_type, "application/pdf");
    assert_eq!(file.file_url, "https://drive.google.com/file/d/1abc/view?usp=drivesdk");
    assert_eq!(file.waiting, None);
    assert_eq!(sent.load(Ordering::SeqCst), size as u64, "progress counts every byte of the file");
}

#[tokio::test]
async fn an_upload_asks_drive_for_the_fields_it_links_with() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    let (file, _dir) = file_of(10);
    Mock::given(method("POST"))
        .and(path(format!("{UPLOAD}/files")))
        .and(query_param("fields", "id,name,mimeType,webViewLink,iconLink"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "1", "webViewLink": "https://x.example/1"})))
        .expect(1)
        .mount(&server)
        .await;
    client(&server).upload_to_drive(&file, "a.bin", "application/octet-stream", Arc::default()).await.unwrap();
}

#[tokio::test]
async fn a_missing_file_says_so_before_anything_goes_out() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    let dir = TempDir::new();
    let err = client(&server)
        .upload_to_drive(&dir.0.join("gone.pdf"), "gone.pdf", "application/pdf", Arc::default())
        .await
        .unwrap_err();
    assert!(matches!(err, GmailError::FileMissing(_)), "{err:?}");
    let posted = server.received_requests().await.unwrap().iter().filter(|r| r.url.path().contains("files")).count();
    assert_eq!(posted, 0);
}

#[tokio::test]
async fn an_account_without_drive_access_gets_missing_scope() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    let (file, _dir) = file_of(10);
    Mock::given(method("POST"))
        .and(path(format!("{UPLOAD}/files")))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({"error": {
            "code": 403, "message": "Request had insufficient authentication scopes.",
            "errors": [{"reason": "insufficientPermissions"}]
        }})))
        .mount(&server)
        .await;
    let err = client(&server).upload_to_drive(&file, "a.pdf", "application/pdf", Arc::default()).await.unwrap_err();
    assert!(matches!(err, GmailError::MissingScope), "{err:?}");
}

/// A part's headers and its content, split at the blank line.
fn split_head(part: &[u8]) -> (String, &[u8]) {
    let at = part.windows(4).position(|w| w == b"\r\n\r\n").expect("a blank line after the headers");
    (String::from_utf8_lossy(&part[..at]).into_owned(), &part[at + 4..])
}

const MIB: usize = 1024 * 1024;

fn byte_at(i: usize) -> u8 {
    (i % 251) as u8
}

/// The `bytes a-b/total` or `bytes */total` of a request.
fn range_of(request: &Request) -> String {
    request.headers.get("content-range").map(|v| v.to_str().unwrap().to_string()).unwrap_or_default()
}

async fn mount_session(server: &MockServer, size: usize) {
    let location = format!("{}{UPLOAD}/files?uploadType=resumable&upload_id=session1", server.uri());
    Mock::given(method("POST"))
        .and(path(format!("{UPLOAD}/files")))
        .and(query_param("uploadType", "resumable"))
        .and(header("x-upload-content-type", "application/pdf"))
        .and(header("x-upload-content-length", size.to_string().as_str()))
        .respond_with(move |request: &Request| {
            let metadata: Value = serde_json::from_slice(&request.body).unwrap();
            assert_eq!(metadata, json!({"name": "Big.pdf", "mimeType": "application/pdf"}));
            ResponseTemplate::new(200).insert_header("Location", location.as_str())
        })
        .expect(1)
        .mount(server)
        .await;
}

fn done_file() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "id": "big1", "name": "Big.pdf", "mimeType": "application/pdf",
        "webViewLink": "https://drive.google.com/file/d/big1/view"
    }))
}

/// A file above 5 MB goes up in chunks through a resumable session. The
/// second chunk meets a 503; the client asks the session how much it
/// holds and sends the rest from there.
#[tokio::test]
async fn a_large_file_resumes_after_a_server_error_midway() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    let size = 9 * MIB;
    let (file, _dir) = file_named(size, "big");
    mount_session(&server, size).await;
    let held_back = 8 * MIB + 256 * 1024;
    let log = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen = Arc::clone(&log);
    let failed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    Mock::given(method("PUT"))
        .and(path(format!("{UPLOAD}/files")))
        .and(query_param("upload_id", "session1"))
        .respond_with(move |request: &Request| {
            let range = range_of(request);
            seen.lock().unwrap().push(range.clone());
            if range == format!("bytes */{size}") {
                assert!(request.body.is_empty(), "a status query carries no bytes");
                return ResponseTemplate::new(308).insert_header("Range", format!("bytes=0-{}", held_back - 1).as_str());
            }
            let (span, total) = range.strip_prefix("bytes ").unwrap().split_once('/').unwrap();
            assert_eq!(total, size.to_string());
            let (from, to) = span.split_once('-').unwrap();
            let (from, to): (usize, usize) = (from.parse().unwrap(), to.parse().unwrap());
            assert_eq!(request.body.len(), to - from + 1, "the chunk is as long as its range");
            assert!(request.body.iter().enumerate().all(|(i, b)| *b == byte_at(from + i)), "the chunk holds the file's own bytes");
            if to + 1 < size {
                return ResponseTemplate::new(308).insert_header("Range", format!("bytes=0-{to}").as_str());
            }
            if !failed.swap(true, std::sync::atomic::Ordering::SeqCst) {
                return ResponseTemplate::new(503);
            }
            done_file()
        })
        .mount(&server)
        .await;
    let sent = Arc::new(AtomicU64::new(0));
    let file = client(&server).upload_to_drive(&file, "Big.pdf", "application/pdf", Arc::clone(&sent)).await.unwrap();
    assert_eq!(file.file_id, "big1");
    assert_eq!(file.file_url, "https://drive.google.com/file/d/big1/view");
    assert_eq!(sent.load(Ordering::SeqCst), size as u64);
    let chunk = 8 * MIB;
    assert_eq!(
        *log.lock().unwrap(),
        vec![
            format!("bytes 0-{}/{size}", chunk - 1),
            format!("bytes {chunk}-{}/{size}", size - 1),
            format!("bytes */{size}"),
            format!("bytes {held_back}-{}/{size}", size - 1),
        ]
    );
    let posted_multipart = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .any(|r| r.url.query().is_some_and(|q| q.contains("uploadType=multipart")));
    assert!(!posted_multipart, "a large file never goes up in one multipart body");
}

/// Dropping the upload, as Cancel does, ends the session on Drive.
#[tokio::test]
async fn cancelling_a_large_upload_ends_its_session() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    let size = 6 * MIB;
    let (file, _dir) = file_named(size, "cancel");
    mount_session(&server, size).await;
    Mock::given(method("PUT"))
        .and(path(format!("{UPLOAD}/files")))
        .respond_with(ResponseTemplate::new(308).set_delay(std::time::Duration::from_secs(10)))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!("{UPLOAD}/files")))
        .and(query_param("upload_id", "session1"))
        .respond_with(ResponseTemplate::new(499))
        .expect(1)
        .mount(&server)
        .await;
    let gmail = client(&server);
    let upload = gmail.upload_to_drive(&file, "Big.pdf", "application/pdf", Arc::default());
    assert!(tokio::time::timeout(std::time::Duration::from_millis(500), upload).await.is_err());
    for _ in 0..40 {
        let deleted = server.received_requests().await.unwrap().iter().any(|r| r.method.as_str() == "DELETE");
        if deleted {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("the session was never ended");
}

#[tokio::test]
async fn sharing_a_file_makes_the_guest_a_reader_without_mailing_them() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    Mock::given(method("POST"))
        .and(path(format!("{DRIVE}/files/1abc/permissions")))
        .and(query_param("sendNotificationEmail", "false"))
        .and(wiremock::matchers::body_json(json!({"role": "reader", "type": "user", "emailAddress": "ana@example.com"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "perm1"})))
        .expect(1)
        .mount(&server)
        .await;
    client(&server).share_file("1abc", "ana@example.com").await.unwrap();
}
