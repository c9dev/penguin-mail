//! Conversation context that another provider can read without replaying tools.

use serde_json::{Value, json};

pub(super) fn without_thinking(mut message: Value) -> Option<Value> {
    if let Some(blocks) = message["content"].as_array_mut() {
        blocks.retain(|block| {
            !matches!(
                block["type"].as_str(),
                Some("thinking" | "redacted_thinking")
            )
        });
        if blocks.is_empty() {
            return None;
        }
    }
    Some(message)
}

pub(super) fn as_text(message: Value) -> Option<Value> {
    let role = match message["role"].as_str()? {
        "assistant" => "assistant",
        _ => "user",
    };
    let mut parts = Vec::new();
    if message["role"] == "tool" {
        parts.push(format!(
            "Past tool result {}: {}",
            message["tool_call_id"], message["content"]
        ));
    } else if let Some(text) = message["content"].as_str() {
        parts.push(text.to_string());
    } else if let Some(blocks) = message["content"].as_array() {
        for block in blocks {
            match block["type"].as_str() {
                Some("text") => parts.push(block["text"].as_str().unwrap_or_default().to_string()),
                Some("tool_use" | "server_tool_use") => parts.push(format!(
                    "Past tool call {} ({}): {}",
                    block["name"], block["id"], block["input"]
                )),
                Some("thinking" | "redacted_thinking") => {}
                _ => parts.push(format!("Past tool result: {block}")),
            }
        }
    }
    if let Some(calls) = message["tool_calls"].as_array() {
        for call in calls {
            parts.push(format!(
                "Past tool call {} ({}): {}",
                call["function"]["name"], call["id"], call["function"]["arguments"]
            ));
        }
    }
    let content = parts.join("\n");
    if content.is_empty() {
        return None;
    }
    Some(json!({"role":role,"content":content}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProviderConfig;
    use crate::history::History;
    use crate::providers::State;

    fn config(kind: usize) -> ProviderConfig {
        match kind {
            0 => ProviderConfig::OpenAiCompatible {
                base_url: "http://localhost".into(),
                api_key: None,
                model: "other".into(),
            },
            1 => ProviderConfig::Anthropic {
                api_key: "key".into(),
                model: "other".into(),
            },
            _ => ProviderConfig::ClaudeCode {
                command: "claude".into(),
                model: Some("other".into()),
            },
        }
    }

    fn history(state: &mut State) -> &mut History {
        match state {
            State::OpenAi(chat) => &mut chat.history,
            State::Anthropic(chat) => &mut chat.history,
            State::ClaudeCode(chat) => &mut chat.history,
        }
    }

    #[test]
    fn provider_handoffs_keep_questions_replies_and_tool_evidence() {
        for from in 0..3 {
            for to in 0..3 {
                if from == to {
                    continue;
                }
                let mut state = State::new(config(from), "Mail instructions".into());
                let h = history(&mut state);
                h.begin(json!({"role":"user","content":"Find invoices"}));
                if from == 0 {
                    h.push(json!({"role":"assistant","tool_calls":[{"id":"one","function":{"name":"search_mail","arguments":"{}"}}]}));
                    h.push(json!({"role":"tool","tool_call_id":"one","content":"Invoice 2291"}));
                } else {
                    h.push(json!({"role":"assistant","content":[
                        {"type":"thinking","thinking":"private reasoning","signature":"old signature"},
                        {"type":"tool_use","id":"one","name":"search_mail","input":{}}
                    ]}));
                    h.push(json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"one","content":"Invoice 2291"}]}));
                }
                h.push(json!({"role":"assistant","content":"Found your invoice."}));
                h.commit();
                h.begin(json!({"role":"user","content":"Canceled question"}));
                state.set_provider(config(to));
                let messages = history(&mut state).messages();
                assert_eq!(messages[0]["content"], "Find invoices");
                assert_eq!(messages.last().unwrap()["content"], "Found your invoice.");
                let text = serde_json::to_string(&messages).unwrap();
                assert!(text.contains("search_mail") && text.contains("Invoice 2291"));
                assert!(!text.contains("private reasoning") && !text.contains("old signature"));
                assert!(!text.contains("Canceled question"));
                assert!(
                    messages
                        .iter()
                        .all(|m| m["content"].is_string() && m.get("tool_calls").is_none())
                );
            }
        }
    }

    #[test]
    fn anthropic_model_changes_drop_signed_thinking_and_keep_tool_pairs() {
        let mut state = State::new(config(1), String::new());
        let h = history(&mut state);
        h.begin(json!({"role":"user","content":"Find invoices"}));
        h.push(json!({"role":"assistant","content":[
            {"type":"thinking","thinking":"reason","signature":"signed"},
            {"type":"tool_use","id":"one","name":"search_mail","input":{}}
        ]}));
        h.push(json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"one","content":"2291"}]}));
        h.commit();
        state.set_provider(ProviderConfig::Anthropic {
            api_key: "new-key".into(),
            model: "new-model".into(),
        });
        let messages = history(&mut state).messages();
        assert_eq!(messages[1]["content"].as_array().unwrap().len(), 1);
        assert_eq!(
            messages[1]["content"][0]["id"],
            messages[2]["content"][0]["tool_use_id"]
        );
    }
    #[test]
    fn provider_changes_keep_conversation_options() {
        let schema = json!({"type":"object"});
        let mut chat = crate::Conversation::new(config(0), String::new())
            .with_thinking()
            .with_format(schema.clone());
        chat.set_web(true);
        chat.set_rounds(7);
        for kind in [1, 2, 0, 1] {
            chat.set_provider(config(kind));
            match &chat.inner {
                State::Anthropic(state) => {
                    assert!(state.think && state.web);
                    assert_eq!(state.format, Some(schema.clone()));
                    assert_eq!(state.rounds, 7);
                }
                State::ClaudeCode(state) => assert!(state.web),
                State::OpenAi(state) => assert_eq!(state.rounds, 7),
            }
        }
    }

    #[test]
    fn a_different_openai_model_can_use_tools_after_the_old_one_rejected_them() {
        let mut state = State::new(config(0), String::new());
        if let State::OpenAi(chat) = &mut state {
            chat.tools_off = true;
            chat.history.begin(json!({"role":"user","content":"Hello"}));
            chat.history
                .push(json!({"role":"assistant","content":"Hi"}));
            chat.history.commit();
        }
        state.set_provider(ProviderConfig::OpenAiCompatible {
            base_url: "http://localhost".into(),
            api_key: None,
            model: "with-tools".into(),
        });
        if let State::OpenAi(chat) = &mut state {
            assert!(!chat.tools_off);
            assert_eq!(chat.history.messages().len(), 2);
        } else {
            panic!("Wrong provider");
        }
    }
}
