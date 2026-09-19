//! Reads a server-sent events stream from an HTTP response, one event at a time.

use crate::AiError;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SseEvent {
    /// The `event:` field, empty when the server sent none.
    pub event: String,
    /// The `data:` lines, joined with newlines.
    pub data: String,
}

pub(crate) struct SseReader {
    response: reqwest::Response,
    buf: Vec<u8>,
    event: String,
    data: Vec<String>,
    finished: bool,
}

impl SseReader {
    pub(crate) fn new(response: reqwest::Response) -> SseReader {
        SseReader {
            response,
            buf: Vec::new(),
            event: String::new(),
            data: Vec::new(),
            finished: false,
        }
    }

    /// The next complete event, or `None` once the stream ends.
    pub(crate) async fn next(&mut self) -> Result<Option<SseEvent>, AiError> {
        loop {
            if let Some(end) = self.buf.iter().position(|&b| b == b'\n') {
                let mut line: Vec<u8> = self.buf.drain(..=end).collect();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if let Some(event) = self.take_line(&String::from_utf8_lossy(&line)) {
                    return Ok(Some(event));
                }
                continue;
            }
            if self.finished {
                return Ok(self.dispatch());
            }
            match self.response.chunk().await {
                Ok(Some(bytes)) => self.buf.extend_from_slice(&bytes),
                Ok(None) => {
                    self.finished = true;
                    if !self.buf.is_empty() {
                        self.buf.push(b'\n');
                    }
                }
                Err(e) => return Err(AiError::Network(e.to_string())),
            }
        }
    }

    /// Applies one line; a blank line completes the pending event.
    fn take_line(&mut self, line: &str) -> Option<SseEvent> {
        if line.is_empty() {
            return self.dispatch();
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        match field {
            "event" => self.event = value.to_string(),
            "data" => self.data.push(value.to_string()),
            _ => {}
        }
        None
    }

    fn dispatch(&mut self) -> Option<SseEvent> {
        if self.data.is_empty() {
            self.event.clear();
            return None;
        }
        let event = SseEvent {
            event: std::mem::take(&mut self.event),
            data: self.data.join("\n"),
        };
        self.data.clear();
        Some(event)
    }
}
