//! Push against wiremock: no URL, a change, and a stream gone quiet.

mod common;

use std::time::Duration;

use mailrs_jmap::{JmapError, PushLimits, Pushed};
use serde_json::json;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use common::session_on;

/// The waits push judges a server by. The real ones are 20 s and 10 s;
/// the servers below hold calls on purpose, and these tests would wait
/// the real ones out in full.
const LIMITS: PushLimits = PushLimits {
    stall: Duration::from_secs(3),
    probe: Duration::from_secs(2),
};

fn client(server: &MockServer) -> mailrs_jmap::JmapClient {
    mailrs_jmap::JmapClient::with_push_limits(
        &format!("{}/jmap/session", server.uri()),
        mailrs_jmap::Auth::Bearer("token-1".into()),
        LIMITS,
    )
    .unwrap()
}

/// Answers the `Core/echo` the client sends beside a new stream.
async fn answer_echo(server: &MockServer) {
    let echoed = json!({ "methodResponses": [["Core/echo", {}, "c0"]], "sessionState": "s1" });
    Mock::given(method("POST"))
        .and(path("/api/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(echoed))
        .mount(server)
        .await;
}

#[tokio::test]
async fn a_session_without_event_source_says_so() {
    let server = MockServer::start().await;
    let mut session = session_on(&server.uri(), json!({}));
    session.as_object_mut().unwrap().remove("eventSourceUrl");
    Mock::given(method("GET"))
        .and(path("/jmap/session"))
        .respond_with(ResponseTemplate::new(200).set_body_json(session))
        .mount(&server)
        .await;
    assert_eq!(
        client(&server).push(Duration::from_secs(1)).await.unwrap(),
        Pushed::NoPush
    );
}

#[tokio::test]
async fn a_change_to_the_mail_account_wakes_the_watch() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/jmap/session"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(session_on(&server.uri(), json!({}))),
        )
        .mount(&server)
        .await;
    answer_echo(&server).await;
    let body = "event: state\ndata: {\"@type\":\"StateChange\",\"changed\":{\"OTHER\":{\"Email\":\"x\"}}}\n\n\
                event: state\ndata: {\"@type\":\"StateChange\",\"changed\":{\"A1\":{\"Email\":\"e2\"}}}\n\n";
    Mock::given(method("GET"))
        .and(path("/events/"))
        .and(query_param("types", "Email,Mailbox,EmailDelivery"))
        .and(query_param("closeafter", "no"))
        .and(query_param("ping", "60"))
        .and(header("authorization", "Bearer token-1"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;
    assert_eq!(
        client(&server).push(Duration::from_secs(5)).await.unwrap(),
        Pushed::Changed
    );
}

#[tokio::test]
async fn a_stream_that_closes_ends_the_watch() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/jmap/session"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(session_on(&server.uri(), json!({}))),
        )
        .mount(&server)
        .await;
    answer_echo(&server).await;
    Mock::given(method("GET"))
        .and(path("/events/"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_string(": hello\n\n")
                .set_delay(Duration::from_millis(10)),
        )
        .mount(&server)
        .await;
    let refused = client(&server)
        .push(Duration::from_millis(300))
        .await
        .unwrap_err();
    assert!(
        matches!(&refused, JmapError::Network(said) if said.contains("ended")),
        "{refused:?}"
    );
}

#[tokio::test]
async fn a_line_without_an_end_past_the_cap_is_refused() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/jmap/session"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(session_on(&server.uri(), json!({}))),
        )
        .mount(&server)
        .await;
    answer_echo(&server).await;
    let body = "x".repeat(mailrs_jmap::push::MOST_EVENT_BYTES + 10);
    Mock::given(method("GET"))
        .and(path("/events/"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;
    let refused = client(&server)
        .push(Duration::from_secs(5))
        .await
        .unwrap_err();
    assert!(
        matches!(refused, JmapError::AnswerTooLarge { .. }),
        "{refused:?}"
    );
}

#[tokio::test]
async fn a_failed_open_answers_the_problem_and_leaks_no_address() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/jmap/session"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(session_on(&server.uri(), json!({}))),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/events/"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;
    let refused = client(&server)
        .push(Duration::from_secs(5))
        .await
        .unwrap_err();
    assert!(!refused.to_string().contains("127.0.0.1"), "{refused}");
}

/// Answers the event stream's head and one comment, then holds the socket
/// open and says nothing more. Answers its address.
async fn silent_server() -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut seen = Vec::new();
                let mut buf = [0u8; 1024];
                while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => seen.extend_from_slice(&buf[..n]),
                    }
                }
                let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n";
                let hello = ": hello\n\n";
                let first = format!("{head}{:x}\r\n{hello}\r\n", hello.len());
                if stream.write_all(first.as_bytes()).await.is_err() {
                    return;
                }
                tokio::time::sleep(Duration::from_secs(30)).await;
            });
        }
    });
    format!("http://{address}")
}

#[tokio::test]
async fn a_silent_stream_ends_the_watch_after_the_idle_limit() {
    let events = silent_server().await;
    let server = MockServer::start().await;
    let mut session = session_on(&server.uri(), json!({}));
    session["eventSourceUrl"] = json!(format!("{events}/events/"));
    Mock::given(method("GET"))
        .and(path("/jmap/session"))
        .respond_with(ResponseTemplate::new(200).set_body_json(session))
        .mount(&server)
        .await;
    answer_echo(&server).await;
    let started = std::time::Instant::now();
    let refused = client(&server)
        .push(Duration::from_millis(300))
        .await
        .unwrap_err();
    assert!(
        matches!(&refused, JmapError::Network(said) if said.contains("quiet")),
        "{refused:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(5));
}

/// A server that acts as Cyrus does while an event stream is open: it
/// answers no POST until every stream has closed. The first `free_posts`
/// POSTs are answered at once anyway, standing in for a server that
/// blocks only some of the time. When `state_first` is set the stream
/// sends one state change for A1 before it falls silent. Answers its
/// base URL and the count of streams it has opened.
async fn blocking_server(
    free_posts: usize,
    state_first: bool,
) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::watch;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (open_tx, open_rx) = watch::channel(0usize);
    let open_tx = Arc::new(open_tx);
    let posts = Arc::new(AtomicUsize::new(0));
    let opened = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&opened);
    let session_base = base.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let (open_tx, mut open_rx, posts, base) = (
                Arc::clone(&open_tx),
                open_rx.clone(),
                Arc::clone(&posts),
                session_base.clone(),
            );
            let counted = Arc::clone(&counted);
            tokio::spawn(async move {
                let mut seen = Vec::new();
                let mut buf = [0u8; 4096];
                let head_end = loop {
                    if let Some(at) = seen.windows(4).position(|w| w == b"\r\n\r\n") {
                        break at + 4;
                    }
                    match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => seen.extend_from_slice(&buf[..n]),
                    }
                };
                let head = String::from_utf8_lossy(&seen[..head_end]).to_string();
                let first = head.lines().next().unwrap_or_default().to_string();
                if first.starts_with("GET /events/") {
                    counted.fetch_add(1, Ordering::SeqCst);
                    open_tx.send_modify(|n| *n += 1);
                    let mut out = String::from(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
                    );
                    let hello = match state_first {
                        true => {
                            "event: state\ndata: {\"@type\":\"StateChange\",\"changed\":{\"A1\":{\"Email\":\"e2\"}}}\n\n"
                        }
                        false => ": hello\n\n",
                    };
                    out.push_str(&format!("{:x}\r\n{hello}\r\n", hello.len()));
                    if stream.write_all(out.as_bytes()).await.is_ok() {
                        // Held open until the client drops it.
                        while matches!(stream.read(&mut buf).await, Ok(n) if n > 0) {}
                    }
                    open_tx.send_modify(|n| *n -= 1);
                    return;
                }
                let body = if first.starts_with("POST") {
                    let length: usize = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse().unwrap())
                        })
                        .unwrap_or(0);
                    while seen.len() < head_end + length {
                        match stream.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => seen.extend_from_slice(&buf[..n]),
                        }
                    }
                    if posts.fetch_add(1, Ordering::SeqCst) >= free_posts {
                        let _ = open_rx.wait_for(|n| *n == 0).await;
                    }
                    let asked: serde_json::Value =
                        serde_json::from_slice(&seen[head_end..head_end + length]).unwrap();
                    let answers = asked["methodCalls"].as_array().unwrap().to_vec();
                    json!({ "methodResponses": answers, "sessionState": "s1" }).to_string()
                } else {
                    session_on(&base, json!({})).to_string()
                };
                let out = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(out.as_bytes()).await;
            });
        }
    });
    (base, opened)
}

fn echo() -> mailrs_jmap::Request {
    let mut request = mailrs_jmap::Request::new();
    request.call(mailrs_jmap::CORE, "Core/echo", json!({"hello": true}));
    request
}

fn raw_client(base: &str) -> mailrs_jmap::JmapClient {
    mailrs_jmap::JmapClient::with_push_limits(
        &format!("{base}/jmap/session"),
        mailrs_jmap::Auth::Bearer("token-1".into()),
        LIMITS,
    )
    .unwrap()
}

#[tokio::test]
async fn a_server_that_answers_nothing_while_the_stream_is_open_gets_no_push() {
    let (base, _) = blocking_server(0, false).await;
    let client = raw_client(&base);
    let started = std::time::Instant::now();
    assert_eq!(
        client.push(Duration::from_secs(120)).await.unwrap(),
        Pushed::NoPush
    );
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "took {:?}",
        started.elapsed()
    );
    tokio::time::timeout(Duration::from_secs(5), client.send(&echo()))
        .await
        .expect("a call after the probe waited")
        .unwrap();
    assert_eq!(
        client.push(Duration::from_secs(120)).await.unwrap(),
        Pushed::NoPush,
        "push stays off"
    );
}

#[tokio::test]
async fn a_call_stalled_behind_a_held_stream_drops_the_stream_and_completes() {
    // The probe passes; the stream then reports a change and stays open
    // while the engine looks, and that look is the call Cyrus holds.
    let (base, opened) = blocking_server(1, true).await;
    let client = raw_client(&base);
    assert_eq!(
        client.push(Duration::from_secs(120)).await.unwrap(),
        Pushed::Changed
    );
    let started = std::time::Instant::now();
    tokio::time::timeout(Duration::from_secs(40), client.send(&echo()))
        .await
        .expect("the call never completed")
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "took {:?}",
        started.elapsed()
    );
    assert!(client.push_paused().is_some(), "push was not paused");
    let next = tokio::time::timeout(
        Duration::from_secs(1),
        client.push(Duration::from_secs(120)),
    )
    .await;
    assert!(next.is_err(), "push came back at once: {next:?}");
    assert_eq!(
        opened.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a second stream opened during the pause"
    );
}

#[tokio::test]
async fn a_call_stalled_while_the_watch_waits_ends_the_watch_and_completes() {
    let (base, _) = blocking_server(1, false).await;
    let client = raw_client(&base);
    let watching = client.clone();
    let watch = tokio::spawn(async move { watching.push(Duration::from_secs(120)).await });
    // Let the stream open and the probe pass.
    tokio::time::sleep(Duration::from_millis(500)).await;
    tokio::time::timeout(Duration::from_secs(40), client.send(&echo()))
        .await
        .expect("the call never completed")
        .unwrap();
    let watched = tokio::time::timeout(Duration::from_secs(5), watch)
        .await
        .expect("the watch kept going")
        .unwrap();
    assert!(
        matches!(&watched, Err(JmapError::Network(said)) if said.contains("stalled")),
        "{watched:?}"
    );
}

/// Polls `watch` for `long`, then leaves it pending and unpolled, as the
/// engine leaves its watch while a tick runs.
async fn poll_for<F: std::future::Future + Unpin>(watch: &mut F, long: Duration) {
    tokio::select! {
        _ = watch => panic!("the watch ended before it was left"),
        () = tokio::time::sleep(long) => {}
    }
}

#[tokio::test]
async fn a_call_stalled_while_the_watch_sits_unpolled_drops_the_stream_and_completes() {
    let (base, _) = blocking_server(1, false).await;
    let client = raw_client(&base);
    let mut watch = Box::pin(client.push(Duration::from_secs(120)));
    // The stream opens and the probe passes; then nobody polls the watch.
    poll_for(&mut watch, Duration::from_millis(500)).await;
    let started = std::time::Instant::now();
    tokio::time::timeout(Duration::from_secs(40), client.send(&echo()))
        .await
        .expect("the call never completed")
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "took {:?}",
        started.elapsed()
    );
    let watched = tokio::time::timeout(Duration::from_secs(5), watch)
        .await
        .expect("the watch kept going");
    assert!(
        matches!(&watched, Err(JmapError::Network(said)) if said.contains("stalled")),
        "{watched:?}"
    );
}

#[tokio::test]
async fn a_call_stalled_while_the_watch_sits_unpolled_in_its_probe_completes() {
    let (base, _) = blocking_server(0, false).await;
    let client = raw_client(&base);
    let mut watch = Box::pin(client.push(Duration::from_secs(120)));
    // The stream opens and the probe goes out; then nobody polls the watch.
    poll_for(&mut watch, Duration::from_millis(500)).await;
    let started = std::time::Instant::now();
    tokio::time::timeout(Duration::from_secs(40), client.send(&echo()))
        .await
        .expect("the call never completed")
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "took {:?}",
        started.elapsed()
    );
    let watched = tokio::time::timeout(Duration::from_secs(5), watch)
        .await
        .expect("the watch kept going");
    assert_eq!(
        watched.unwrap(),
        Pushed::NoPush,
        "a stall before the probe passed turns push off for good"
    );
    assert_eq!(client.push_paused(), None);
}

#[tokio::test]
async fn a_slow_download_beside_the_stream_leaves_push_on() {
    let events = silent_server().await;
    let server = MockServer::start().await;
    let mut session = session_on(&server.uri(), json!({}));
    session["eventSourceUrl"] = json!(format!("{events}/events/"));
    Mock::given(method("GET"))
        .and(path("/jmap/session"))
        .respond_with(ResponseTemplate::new(200).set_body_json(session))
        .mount(&server)
        .await;
    answer_echo(&server).await;
    let slow = ResponseTemplate::new(200)
        .set_body_bytes(b"blob".to_vec())
        // Past the stall limit: only the head of an API call is timed,
        // so a slow download is no stall.
        .set_delay(LIMITS.stall + Duration::from_secs(1));
    Mock::given(method("GET"))
        .and(path("/download/A1/b1/x"))
        .respond_with(slow)
        .mount(&server)
        .await;
    let client = client(&server);
    let watching = client.clone();
    let mut watch = tokio::spawn(async move { watching.push(Duration::from_secs(120)).await });
    // Let the stream open and the probe pass.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        client
            .download("A1", "b1", "x", "text/plain", 100)
            .await
            .unwrap(),
        b"blob"
    );
    let watched = tokio::time::timeout(Duration::from_millis(500), &mut watch).await;
    assert!(watched.is_err(), "the watch ended: {watched:?}");
    watch.abort();
}
