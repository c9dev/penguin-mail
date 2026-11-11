//! Push over EventSource (RFC 8620 section 7.3): one long GET whose body
//! is a stream of events. A `state` event names the types whose state
//! moved in each account; a ping keeps a quiet connection alive. The
//! reader caps one event at a megabyte and gives up on a stream that says
//! nothing for the idle limit the caller sets, twice the ping interval.
//!
//! Cyrus answers no other request of the account while a stream is open.
//! So a new stream is followed by one `Core/echo`, which every server
//! answers; when no answer comes within the probe limit the stream is
//! dropped and push stays off for the client's life. Any API call that
//! stalls before the probe passes counts the same. One that stalls later
//! while a stream is open drops the stream and pauses push for a while
//! (see `JmapClient::sent`).
//!
//! The open stream holds no request permit. A permit lasts one call, and
//! a stream lives for hours: holding one of the four would starve the
//! account's normal calls.

use std::collections::BTreeMap;
use std::time::Duration;

use reqwest::header::HeaderValue;
use serde::Deserialize;

use crate::blob::expand;
use crate::http::{failure, network};
use crate::request::Request;
use crate::session::{CORE, MAIL};
use crate::{JmapClient, JmapError};

/// How often the server is asked to ping, in seconds.
pub const PING_SECONDS: u32 = 60;

/// The most one event, or the bytes waiting for its end, may take.
pub const MOST_EVENT_BYTES: usize = 1 << 20;

const TYPES: &str = "Email,Mailbox,EmailDelivery";

/// The longest event name kept. Real names are `state` and `ping`.
const MOST_NAME_BYTES: usize = 64;

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct StateChange {
    #[serde(default)]
    pub changed: BTreeMap<String, BTreeMap<String, String>>,
}

impl StateChange {
    pub fn touches(&self, account: &str, types: &[&str]) -> bool {
        self.changed
            .get(account)
            .is_some_and(|moved| moved.keys().any(|t| types.contains(&t.as_str())))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pushed {
    /// Something in the mail account changed.
    Changed,
    /// The session names no event source; the account polls.
    NoPush,
}

/// Lines in, state changes out.
#[derive(Debug, Default)]
pub struct Parser {
    event: String,
    data: String,
    /// The event in progress passed the cap; its rest is skipped.
    too_big: bool,
}

impl Parser {
    pub fn new() -> Parser {
        Parser::default()
    }

    /// Takes one line without its line ending, and answers a state change
    /// when the line ends one.
    pub fn line(&mut self, line: &str) -> Option<StateChange> {
        if line.is_empty() {
            let (event, data) = (
                std::mem::take(&mut self.event),
                std::mem::take(&mut self.data),
            );
            if std::mem::take(&mut self.too_big) || event == "ping" || data.is_empty() {
                return None;
            }
            let parsed: serde_json::Value = serde_json::from_str(&data).ok()?;
            let says_state = event == "state"
                || parsed.get("@type").and_then(|t| t.as_str()) == Some("StateChange");
            return says_state
                .then(|| serde_json::from_value(parsed).ok())
                .flatten();
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" if value.len() <= MOST_NAME_BYTES => self.event = value.to_string(),
            "data" if !self.too_big => {
                if self.data.len() + value.len() + 1 > MOST_EVENT_BYTES {
                    self.data = String::new();
                    self.too_big = true;
                } else {
                    if !self.data.is_empty() {
                        self.data.push('\n');
                    }
                    self.data.push_str(value);
                }
            }
            _ => {}
        }
        None
    }
}

/// What the reader task passes to the watch.
enum Read {
    /// The server answered the stream's head.
    Opened,
    Bytes(Vec<u8>),
    Failed(JmapError),
}

/// An open event stream and the bytes read from it. A task of its own
/// holds the connection, so a stalled call can close it through the
/// task's abort handle while the watch sits unpolled holding this.
pub(crate) struct EventStream {
    reads: tokio::sync::mpsc::Receiver<Read>,
    reader: tokio::task::AbortHandle,
    pending: Vec<u8>,
    parser: Parser,
}

impl Drop for EventStream {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

/// Opens the stream and passes on its bytes until the watch lets go of
/// the other end or the task is aborted.
async fn read_stream(request: reqwest::RequestBuilder, to: tokio::sync::mpsc::Sender<Read>) {
    let mut response = match request.send().await {
        Ok(response) if response.status().is_success() => response,
        Ok(response) => {
            let _ = to.send(Read::Failed(failure(response).await)).await;
            return;
        }
        Err(err) => {
            let _ = to.send(Read::Failed(network(err))).await;
            return;
        }
    };
    if to.send(Read::Opened).await.is_err() {
        return;
    }
    loop {
        let read = match response.chunk().await {
            Ok(Some(chunk)) => Read::Bytes(Vec::from(chunk)),
            Ok(None) => Read::Failed(JmapError::Network("the event stream ended".into())),
            Err(err) => Read::Failed(network(err)),
        };
        let last = matches!(read, Read::Failed(_));
        if to.send(read).await.is_err() || last {
            return;
        }
    }
}

impl EventStream {
    /// The next thing the reader passes on, waiting at most `idle`.
    async fn read(&mut self, idle: Duration, quiet: &str) -> Result<Read, JmapError> {
        tokio::time::timeout(idle, self.reads.recv())
            .await
            .map_err(|_| JmapError::Network(quiet.into()))?
            .ok_or_else(|| JmapError::Network("the event stream was closed".into()))
    }

    /// Waits for the head of the answer.
    async fn opened(&mut self, idle: Duration) -> Result<(), JmapError> {
        match self.read(idle, "the event stream did not open").await? {
            Read::Opened => Ok(()),
            Read::Failed(err) => Err(err),
            Read::Bytes(_) => Err(JmapError::Network(
                "the event stream sent bytes before its head".into(),
            )),
        }
    }

    /// The next state change. Any bytes reset the idle clock, a ping too.
    async fn next(&mut self, idle: Duration) -> Result<StateChange, JmapError> {
        loop {
            while let Some(end) = self.pending.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = self.pending.drain(..=end).collect();
                let text = String::from_utf8_lossy(&line);
                if let Some(change) = self.parser.line(text.trim_end_matches(['\r', '\n'])) {
                    return Ok(change);
                }
            }
            // A burst near the cap must not stay allocated for hours.
            if self.pending.is_empty() && self.pending.capacity() > 64 << 10 {
                self.pending.shrink_to(64 << 10);
            }
            // Only a line still waiting for its end counts against the cap.
            if self.pending.len() > MOST_EVENT_BYTES {
                return Err(JmapError::AnswerTooLarge {
                    limit: MOST_EVENT_BYTES,
                });
            }
            match self.read(idle, "the event stream went quiet").await? {
                Read::Bytes(chunk) => self.pending.extend_from_slice(&chunk),
                Read::Failed(err) => return Err(err),
                Read::Opened => {}
            }
        }
    }
}

impl JmapClient {
    /// Starts the task that opens the stream and reads it, and marks the
    /// stream open from now on: a call stalled behind a stream that is
    /// still opening can close it too.
    async fn open_events(
        &self,
        template: &str,
        refused: Option<&HeaderValue>,
    ) -> Result<(EventStream, HeaderValue), JmapError> {
        let ping = PING_SECONDS.to_string();
        let url = match template.contains("{types}") {
            true => {
                let url = expand(
                    template,
                    &[("types", TYPES), ("closeafter", "no"), ("ping", &ping)],
                );
                self.endpoint(&url)?
            }
            false => {
                let mut url = self.endpoint(template)?;
                url.query_pairs_mut()
                    .append_pair("types", TYPES)
                    .append_pair("closeafter", "no")
                    .append_pair("ping", &ping);
                url
            }
        };
        let (request, header) = self.authorized(reqwest::Method::GET, url, refused).await?;
        let request = request.header(reqwest::header::ACCEPT, "text/event-stream");
        // A few chunks ahead at most: the reader waits for the watch.
        let (to, reads) = tokio::sync::mpsc::channel(4);
        let reader = tokio::spawn(read_stream(request, to)).abort_handle();
        self.set_probing(true);
        self.set_streaming(true);
        self.keep_reader(reader.clone());
        Ok((
            EventStream {
                reads,
                reader,
                pending: Vec::new(),
                parser: Parser::new(),
            },
            header,
        ))
    }

    /// Waits until the server says the mail account changed. The stream
    /// stays open between calls; one that fails is dropped and opened
    /// again by the next call.
    ///
    /// One task drives this: a second concurrent caller waits on the
    /// stream's lock. After `Network` (the stream ended or went quiet) the
    /// caller must back off before calling again, or a server that closes
    /// at once makes a tight reconnect loop. While push is paused after a
    /// stall, a call waits out the pause before it opens a stream.
    pub async fn push(&self, idle: Duration) -> Result<Pushed, JmapError> {
        if self.push_off() {
            return Ok(Pushed::NoPush);
        }
        if let Some(until) = self.push_paused() {
            tokio::time::sleep_until(until).await;
        }
        let session = self.session().await?;
        let Some(template) = session.event_source_url.clone() else {
            return Ok(Pushed::NoPush);
        };
        let account = session
            .account_for(MAIL)
            .ok_or_else(|| JmapError::UnknownCapability(MAIL.into()))?
            .to_string();
        let mut held = self.events().lock().await;
        let close = |held: &mut Option<EventStream>, err: JmapError| {
            *held = None;
            self.set_streaming(false);
            self.stopped_by(err)
        };
        // The header a stream was refused with, so the next one opens on a
        // refreshed token. One refresh a watch, as for any other call.
        let mut refused: Option<HeaderValue> = None;
        loop {
            if self.push_off() || self.push_paused().is_some() {
                return close(&mut held, JmapError::Network("push was turned off".into()));
            }
            if held.is_none() {
                let (stream, header) = self.open_events(&template, refused.as_ref()).await?;
                let stream = held.insert(stream);
                // A stall between the check above and the reader's
                // registration could not reach this reader.
                if self.push_off() || self.push_paused().is_some() {
                    continue;
                }
                match stream.opened(idle).await {
                    Err(JmapError::Unauthorized) if refused.is_none() && self.refreshes() => {
                        *held = None;
                        refused = Some(header);
                        continue;
                    }
                    Err(err) => return close(&mut held, err),
                    Ok(()) => {}
                }
                match self.answers_beside_a_stream().await {
                    Ok(true) => self.set_probing(false),
                    Ok(false) => {
                        tracing::info!(
                            "the server holds calls while a push stream is open; polling instead"
                        );
                        self.stop_pushing();
                        return close(&mut held, JmapError::Network("push was turned off".into()));
                    }
                    Err(err) => return close(&mut held, err),
                }
            }
            let Some(stream) = held.as_mut() else {
                continue;
            };
            match stream.next(idle).await {
                Ok(change) if change.touches(&account, &["Email", "Mailbox", "EmailDelivery"]) => {
                    return Ok(Pushed::Changed);
                }
                Ok(_) => {}
                Err(err) => return close(&mut held, err),
            }
        }
    }

    /// What a watch whose stream closed answers: no push once push is off
    /// for good, the stall while it is paused, and `err` otherwise.
    fn stopped_by(&self, err: JmapError) -> Result<Pushed, JmapError> {
        if self.push_off() {
            return Ok(Pushed::NoPush);
        }
        match self.push_paused() {
            Some(_) => Err(JmapError::Network(
                "a call stalled behind the event stream".into(),
            )),
            None => Err(err),
        }
    }

    /// Whether the server answers a `Core/echo` within the probe limit
    /// while the new stream is open.
    async fn answers_beside_a_stream(&self) -> Result<bool, JmapError> {
        let mut request = Request::new();
        let call = request.call(CORE, "Core/echo", serde_json::json!({}));
        match tokio::time::timeout(self.push_limits().probe, self.send(&request)).await {
            Err(_) => Ok(false),
            Ok(answer) => {
                answer?.take::<serde_json::Value>(&call, "Core/echo")?;
                Ok(true)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fed(lines: &[&str]) -> Vec<StateChange> {
        let mut parser = Parser::new();
        lines.iter().filter_map(|l| parser.line(l)).collect()
    }

    #[test]
    fn a_state_event_is_read_at_its_blank_line() {
        let got = fed(&[
            "event: state",
            r#"data: {"@type":"StateChange","changed":{"A1":{"Email":"e2"}}}"#,
            "",
        ]);
        assert_eq!(got.len(), 1);
        assert!(got[0].touches("A1", &["Email", "Mailbox"]));
        assert!(!got[0].touches("A2", &["Email"]));
    }

    #[test]
    fn data_over_several_lines_joins_and_pings_and_comments_are_ignored() {
        let got = fed(&[
            ": a comment",
            "event: ping",
            "data: {\"interval\":60}",
            "",
            "event: state",
            "data: {\"@type\":\"StateChange\",",
            "data: \"changed\":{\"A1\":{\"Mailbox\":\"m2\"}}}",
            "",
        ]);
        assert_eq!(got.len(), 1);
        assert!(got[0].touches("A1", &["Mailbox"]));
        assert!(!got[0].touches("A1", &["Email"]));
    }

    #[test]
    fn an_event_without_a_name_counts_as_state_when_it_says_state_change() {
        let got = fed(&[
            r#"data: {"@type":"StateChange","changed":{"A1":{"EmailDelivery":"d1"}}}"#,
            "",
        ]);
        assert!(got[0].touches("A1", &["EmailDelivery"]));
    }

    #[test]
    fn data_that_is_not_json_is_skipped() {
        assert!(fed(&["event: state", "data: {oops", ""]).is_empty());
    }

    #[test]
    fn an_event_past_the_cap_is_dropped_and_the_next_one_still_reads() {
        let mut parser = Parser::new();
        let filler = "x".repeat(64 << 10);
        for _ in 0..(MOST_EVENT_BYTES / filler.len() + 2) {
            assert!(parser.line(&format!("data: {filler}")).is_none());
        }
        assert!(
            parser.data.len() <= MOST_EVENT_BYTES,
            "the data kept growing"
        );
        assert!(parser.line("").is_none());
        parser.line("event: state");
        parser.line(r#"data: {"@type":"StateChange","changed":{"A1":{"Email":"e3"}}}"#);
        assert!(parser.line("").is_some_and(|c| c.touches("A1", &["Email"])));
    }
}
