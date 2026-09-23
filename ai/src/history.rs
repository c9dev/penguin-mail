//! The chat a provider sends back to the model each round. Every round
//! carries the whole chat, so a long session costs more and more, and a
//! tool result holding an email body is large. This module owns that chat
//! for the Anthropic and OpenAI providers alike: it opens a turn with the
//! question, keeps what each round adds, and at the end commits the turn
//! or takes it back. Old turns drop off the front whole, so the chat never
//! starts with the answer to a tool call nobody asked for. Providers only
//! render what it holds.

use std::collections::VecDeque;

use serde_json::Value;

/// How much of the chat each round may carry, in characters of JSON. The
/// providers count tokens, so this is a rough stand-in: about four
/// characters per token puts this near 60,000 tokens.
pub(crate) const BUDGET: usize = 240_000;

/// One message and its length as JSON, measured once when it arrives.
struct Entry {
    message: Value,
    size: usize,
}

impl Entry {
    fn new(message: Value) -> Entry {
        let size = message.to_string().len();
        Entry { message, size }
    }
}

/// The turns of one chat, oldest first, and the turn being answered.
pub(crate) struct History {
    turns: VecDeque<Vec<Entry>>,
    /// The turn under way, from its question on. A turn stays open until
    /// the provider commits it or takes it back, and one left open because
    /// its future was dropped, as Stop does, is taken back when the next
    /// turn begins.
    open: Option<Vec<Entry>>,
    budget: usize,
}

impl Default for History {
    fn default() -> History {
        History::new(BUDGET)
    }
}

impl History {
    pub(crate) fn new(budget: usize) -> History {
        History {
            turns: VecDeque::new(),
            open: None,
            budget,
        }
    }

    /// Opens a turn with the user's question, first taking back a turn
    /// that never finished and dropping old turns the budget has no room
    /// for.
    pub(crate) fn begin(&mut self, question: Value) {
        self.rollback();
        self.open = Some(vec![Entry::new(question)]);
        self.trim();
    }

    /// Adds what a round produced to the open turn: the model's reply, or
    /// the results of the tools it called. Old turns go first if the chat
    /// has grown past the budget, so a long tool loop cannot carry the
    /// whole session with it.
    pub(crate) fn push(&mut self, message: Value) {
        let Some(open) = &mut self.open else {
            tracing::warn!("a round answered with no turn open");
            return;
        };
        open.push(Entry::new(message));
        self.trim();
    }

    /// Keeps the open turn, which the model has answered in full.
    pub(crate) fn commit(&mut self) {
        if let Some(open) = self.open.take() {
            self.turns.push_back(open);
        }
    }

    /// Takes the open turn back, question and all, so a failed or stopped
    /// turn leaves the chat as it was before it. A reply that named a tool
    /// and never got its result would make every later request fail.
    pub(crate) fn rollback(&mut self) {
        self.open = None;
    }

    /// Every message to send, oldest first.
    pub(crate) fn messages(&self) -> Vec<Value> {
        self.entries().map(|e| e.message.clone()).collect()
    }

    /// Where the cache breakpoint goes: the last message the user's side
    /// sent, which is the question or the latest tool results. Everything
    /// up to it is what the next round sends again unchanged, so a model
    /// that caches reads it back rather than processing it anew. A reply
    /// the model paused in the middle of comes after it and stays out.
    pub(crate) fn cache_point(&self) -> Option<usize> {
        let messages: Vec<&Value> = self.entries().map(|e| &e.message).collect();
        messages.iter().rposition(|m| m["role"] == "user")
    }

    fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.turns
            .iter()
            .flatten()
            .chain(self.open.iter().flatten())
    }

    fn size(&self) -> usize {
        self.entries().map(|e| e.size).sum()
    }

    /// Drops whole turns from the front until the chat fits. The open turn
    /// always stays, even alone over the budget, because the model cannot
    /// answer a question it never sees.
    fn trim(&mut self) {
        while self.size() > self.budget && !self.turns.is_empty() {
            self.turns.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn question(i: usize) -> Value {
        json!({"role": "user", "content": format!("question {i}")})
    }

    /// One whole turn: a question, a tool call, its result, the answer.
    fn turn(history: &mut History, i: usize, body: &str) {
        history.begin(question(i));
        history.push(json!({"role": "assistant", "content": [
            {"type": "tool_use", "id": format!("t{i}"), "name": "list_mail", "input": {}}
        ]}));
        history.push(json!({"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": format!("t{i}"), "content": body}
        ]}));
        history.push(json!({"role": "assistant", "content": format!("answer {i}")}));
        history.commit();
    }

    fn chat(turns: usize, body: &str) -> History {
        let mut history = History::default();
        for i in 0..turns {
            turn(&mut history, i, body);
        }
        history
    }

    /// Every `tool_result` answers a `tool_use` earlier in the chat.
    fn no_orphans(messages: &[Value]) {
        let mut asked = Vec::new();
        for message in messages {
            for block in message["content"].as_array().into_iter().flatten() {
                match block["type"].as_str() {
                    Some("tool_use") => asked.push(block["id"].clone()),
                    Some("tool_result") => assert!(
                        asked.contains(&block["tool_use_id"]),
                        "an orphan tool result: {block}"
                    ),
                    _ => {}
                }
            }
        }
    }

    fn size(messages: &[Value]) -> usize {
        messages.iter().map(|m| m.to_string().len()).sum()
    }

    #[test]
    fn a_short_chat_stays_whole() {
        let history = chat(2, "a mailbox");
        assert_eq!(history.messages().len(), 8);
    }

    #[test]
    fn a_long_chat_loses_its_oldest_turns() {
        let mut history = chat(20, &"body ".repeat(4000));
        history.begin(question(20));
        let messages = history.messages();
        assert!(size(&messages) <= BUDGET, "{}", size(&messages));
        assert!(messages.len() < 81);
        assert_eq!(messages.last().unwrap(), &question(20));
        assert_eq!(
            messages[0]["content"]
                .as_str()
                .map(|q| q.starts_with("question")),
            Some(true)
        );
        no_orphans(&messages);
    }

    #[test]
    fn one_huge_turn_goes_whole_when_the_next_begins() {
        let mut history = chat(1, &"body ".repeat(100_000));
        history.begin(question(1));
        assert_eq!(history.messages(), vec![question(1)]);
    }

    #[test]
    fn a_huge_turn_in_progress_keeps_every_round() {
        let mut history = chat(3, "a mailbox");
        history.begin(question(3));
        history.push(json!({"role": "assistant", "content": [
            {"type": "tool_use", "id": "big", "name": "read_thread", "input": {}}
        ]}));
        history.push(json!({"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "big", "content": "body ".repeat(100_000)}
        ]}));
        let messages = history.messages();
        // The older turns made room; the open one is whole.
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0], question(3));
        no_orphans(&messages);
    }

    #[test]
    fn trimming_between_rounds_drops_old_turns_whole() {
        let mut history = chat(5, &"body ".repeat(10_000));
        history.begin(question(5));
        for round in 0..6 {
            let id = format!("r{round}");
            history.push(json!({"role": "assistant", "content": [
                {"type": "tool_use", "id": id, "name": "list_mail", "input": {}}
            ]}));
            history.push(json!({"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": id, "content": "body ".repeat(10_000)}
            ]}));
            let messages = history.messages();
            assert!(size(&messages) <= BUDGET || messages[0] == question(5));
            assert!(messages[0]["content"].is_string(), "{}", messages[0]);
            no_orphans(&messages);
        }
    }

    #[test]
    fn a_turn_left_open_is_taken_back_when_the_next_begins() {
        let mut history = chat(1, "a mailbox");
        // Stop drops the future after the model asked for a tool.
        history.begin(question(1));
        history.push(json!({"role": "assistant", "content": [
            {"type": "tool_use", "id": "stopped", "name": "list_mail", "input": {}}
        ]}));
        history.begin(question(2));
        let messages = history.messages();
        assert_eq!(messages.len(), 5);
        assert_eq!(messages[4], question(2));
        no_orphans(&messages);
    }

    #[test]
    fn a_failed_turn_leaves_the_chat_as_it_was() {
        let mut history = chat(1, "a mailbox");
        history.begin(question(1));
        history.push(json!({"role": "assistant", "content": "half"}));
        history.rollback();
        assert_eq!(history.messages().len(), 4);
    }

    #[test]
    fn the_cache_point_is_the_last_message_from_the_user_side() {
        let mut history = chat(1, "a mailbox");
        history.begin(question(1));
        assert_eq!(history.cache_point(), Some(4));
        history.push(json!({"role": "assistant", "content": [
            {"type": "tool_use", "id": "t", "name": "list_mail", "input": {}}
        ]}));
        assert_eq!(history.cache_point(), Some(4));
        history.push(json!({"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "t", "content": "x"}
        ]}));
        assert_eq!(history.cache_point(), Some(6));
        assert_eq!(History::default().cache_point(), None);
    }
}
