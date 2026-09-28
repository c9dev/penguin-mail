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

fn client(server: &MockServer) -> GmailClient {
    let oauth = OAuthClient::new("cid", "secret")
        .with_endpoints(format!("{}/auth", server.uri()), format!("{}/token", server.uri()));
    GmailClient::new(oauth, "rt".into()).with_drive_upload_base_url(format!("{}{UPLOAD}", server.uri()))
}

/// A file of `size` bytes in a folder of its own, which goes when the
/// returned guard drops.
fn file_of(size: usize) -> (std::path::PathBuf, TempDir) {
    let dir = TempDir::new();
    let path = dir.0.join("Agenda.pdf");
    let bytes: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
    std::fs::write(&path, bytes).unwrap();
    (path, dir)
}

struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new() -> TempDir {
        let dir = std::env::temp_dir().join(format!("penguin-drive-{}", rand_suffix()));
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
