//! Keeping a chat short enough to send. Every turn sends the whole chat
//! back, so a long session costs more and more, and a tool result holding
//! an email body is large. Old turns drop off the front.

use serde_json::Value;

/// How much of the chat each turn may carry, in characters of JSON. The
/// providers count tokens, so this is a rough stand-in: about four
/// characters per token puts this near 60,000 tokens.
pub(crate) const BUDGET: usize = 240_000;

/// Drops the oldest turns until the chat fits in `budget`, then lines it
/// up so the first message is a plain question from the user. A reply that
/// answers a tool call cannot lead a chat, and neither can an assistant
/// turn, so both go with it.
pub(crate) fn trim(history: &mut Vec<Value>, budget: usize) {
    while size(history) > budget && history.len() > 2 {
        history.remove(0);
        while history.len() > 2 && !starts_a_turn(&history[0]) {
            history.remove(0);
        }
    }
}

fn size(history: &[Value]) -> usize {
    history.iter().map(|m| m.to_string().len()).sum()
}

/// Whether the chat can start here: a message the user wrote, rather than
/// a reply or the answer to a tool call.
fn starts_a_turn(message: &Value) -> bool {
    if message["role"] != "user" {
        return false;
    }
    match &message["content"] {
        Value::Array(blocks) => !blocks.iter().any(|b| b["type"] == "tool_result"),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn chat(turns: usize, body: &str) -> Vec<Value> {
        let mut history = Vec::new();
        for i in 0..turns {
            history.push(json!({"role": "user", "content": format!("question {i}")}));
            history.push(json!({"role": "assistant", "content": [
                {"type": "tool_use", "id": format!("t{i}"), "name": "list_mail", "input": {}}
            ]}));
            history.push(json!({"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": format!("t{i}"), "content": body}
            ]}));
            history.push(json!({"role": "assistant", "content": format!("answer {i}")}));
        }
        history
    }

    #[test]
    fn a_short_chat_stays_whole() {
        let mut history = chat(2, "a mailbox");
        let before = history.clone();
        trim(&mut history, BUDGET);
        assert_eq!(history, before);
    }

    #[test]
    fn a_long_chat_loses_its_oldest_turns() {
        let mut history = chat(20, &"body ".repeat(4000));
        trim(&mut history, BUDGET);
        assert!(size(&history) <= BUDGET, "{}", size(&history));
        assert!(history.len() < 80);
        assert_eq!(history.last().unwrap()["content"], json!("answer 19"));
    }

    #[test]
    fn the_chat_starts_with_a_question() {
        let mut history = chat(20, &"body ".repeat(4000));
        trim(&mut history, BUDGET);
        assert_eq!(history[0]["role"], "user");
        assert!(history[0]["content"].is_string());
    }

    #[test]
    fn one_huge_turn_survives() {
        let mut history = chat(1, &"body ".repeat(100_000));
        trim(&mut history, BUDGET);
        assert_eq!(history.len(), 2);
    }
}
