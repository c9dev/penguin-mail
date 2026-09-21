//! Splits `<think>…</think>` spans out of streamed reply text. Some local
//! models (Qwen, DeepSeek R1 and others) write their reasoning into the
//! reply between these tags when the server does not move it into a field
//! of its own. The tags can arrive cut across chunks, so the end of a chunk
//! that might be the start of a tag waits for the next one.

const OPEN: &str = "<think>";
const CLOSE: &str = "</think>";

/// One run of reply text or reasoning, in the order the model wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Piece {
    Text(String),
    Thinking(String),
}

#[derive(Debug, Default)]
pub(crate) struct ThinkTags {
    inside: bool,
    /// Text not handed out yet: the tail that may be part of a tag.
    held: String,
    /// Set after a closing tag. The model puts blank lines between its
    /// reasoning and the reply, and those should not open the reply.
    trim_next: bool,
}

impl ThinkTags {
    /// Takes the next chunk and returns what it completes.
    pub(crate) fn push(&mut self, chunk: &str) -> Vec<Piece> {
        self.held.push_str(chunk);
        let mut out = Vec::new();
        loop {
            let tag = if self.inside { CLOSE } else { OPEN };
            if let Some(at) = self.held.find(tag) {
                let before: String = self.held.drain(..at + tag.len()).collect();
                self.give(&mut out, &before[..at]);
                self.trim_next = self.inside;
                self.inside = !self.inside;
                continue;
            }
            let cut = self.held.len() - partial_tag(&self.held, tag);
            let text: String = self.held.drain(..cut).collect();
            self.give(&mut out, &text);
            return out;
        }
    }

    /// Hands out whatever is still held, once the stream has ended.
    pub(crate) fn finish(&mut self) -> Vec<Piece> {
        let mut out = Vec::new();
        let rest = std::mem::take(&mut self.held);
        self.give(&mut out, &rest);
        out
    }

    fn give(&mut self, out: &mut Vec<Piece>, text: &str) {
        let text = if self.trim_next && !self.inside {
            text.trim_start()
        } else {
            text
        };
        if text.is_empty() {
            return;
        }
        if !self.inside {
            self.trim_next = false;
        }
        match (out.last_mut(), self.inside) {
            (Some(Piece::Thinking(so_far)), true) | (Some(Piece::Text(so_far)), false) => {
                so_far.push_str(text)
            }
            (_, true) => out.push(Piece::Thinking(text.to_string())),
            (_, false) => out.push(Piece::Text(text.to_string())),
        }
    }
}

/// How many bytes at the end of `text` could begin `tag`. The tags are
/// ASCII, so the cut always falls between characters.
fn partial_tag(text: &str, tag: &str) -> usize {
    (1..tag.len())
        .rev()
        .find(|&n| text.ends_with(&tag[..n]))
        .unwrap_or(0)
}
