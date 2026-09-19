//! The tools the assistant can call, as JSON Schema. The window runs them;
//! see `ui::window::assistant`.

use mailrs_ai::ToolSpec;
use serde_json::{Value, json};

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: description.into(),
        input_schema: json!({
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        }),
    }
}

fn targets() -> Value {
    json!({
        "type": "array",
        "description": "Conversations to act on, as returned by list_mail or search_mail.",
        "items": {
            "type": "object",
            "properties": {
                "account": {"type": "string", "description": "The account's email address."},
                "thread_id": {"type": "string"},
                "message_id": {"type": "string", "description": "Set when the row has one, to act on that message alone."}
            },
            "required": ["account", "thread_id"],
            "additionalProperties": false
        }
    })
}

fn account(description: &str) -> Value {
    json!({"type": "string", "description": description})
}

fn message_fields() -> Value {
    json!({
        "account": account("Account to send from. Defaults to the one the reply goes to, else the default account."),
        "to": {"type": "array", "items": {"type": "string"}, "description": "Recipients, as addresses or \"Name <address>\"."},
        "cc": {"type": "array", "items": {"type": "string"}},
        "subject": {"type": "string"},
        "body": {"type": "string", "description": "The message in Markdown. Leave out the signature; the app adds it."},
        "reply_to": {
            "type": "object",
            "description": "Set to reply inside an existing conversation.",
            "properties": {
                "account": {"type": "string"},
                "thread_id": {"type": "string"}
            },
            "required": ["account", "thread_id"],
            "additionalProperties": false
        }
    })
}

/// Every tool the assistant may use.
pub fn specs() -> Vec<ToolSpec> {
    vec![
        tool(
            "get_context",
            "The current date and time, the user's accounts and labels, the mailbox on screen, the open conversation, and the selected conversations.",
            json!({}),
            &[],
        ),
        tool(
            "list_mail",
            "Lists conversations in a mailbox, newest first. Inbox, flagged, sent, drafts, VIPs, and labels read the mail kept on this computer (the last few weeks plus everything in the inbox); junk, trash, and all_mail ask Gmail.",
            json!({
                "mailbox": {"type": "string", "enum": ["inbox", "flagged", "sent", "drafts", "vips", "follow_up", "junk", "trash", "all_mail", "label"], "description": "follow_up lists sent mail that has waited 3 to 30 days for a reply."},
                "category": {"type": "string", "enum": ["primary", "updates", "promotions", "social"], "description": "Narrow the inbox to one of Gmail's categories."},
                "label": {"type": "string", "description": "The label's name, when mailbox is \"label\"."},
                "account": account("Limit to one account. All accounts when left out."),
                "unread_only": {"type": "boolean"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 200, "description": "Defaults to 30."}
            }),
            &["mailbox"],
        ),
        tool(
            "search_mail",
            "Searches Gmail with its query syntax across all accounts or one. Reaches all mail, not only recent mail.",
            json!({
                "query": {"type": "string"},
                "account": account("Limit to one account."),
                "limit": {"type": "integer", "minimum": 1, "maximum": 100, "description": "Defaults to 30."}
            }),
            &["query"],
        ),
        tool(
            "read_conversation",
            "Reads every message in a conversation: senders, recipients, dates, text, and attachment names.",
            json!({
                "account": account("The conversation's account."),
                "thread_id": {"type": "string"}
            }),
            &["account", "thread_id"],
        ),
        tool(
            "organize",
            "Archives, trashes, marks, or flags conversations. Reversible; the user can press Ctrl+Z to undo the last change.",
            json!({
                "targets": targets(),
                "action": {"type": "string", "enum": ["archive", "trash", "junk", "not_junk", "move_to_inbox", "mark_read", "mark_unread", "flag", "unflag"]},
                "color": {"type": "string", "enum": ["red", "orange", "yellow", "green", "blue", "purple", "gray"], "description": "Flag color, for action \"flag\". Defaults to red."}
            }),
            &["targets", "action"],
        ),
        tool(
            "label",
            "Adds or removes Gmail labels on conversations, by label name. Missing labels are created when add names them.",
            json!({
                "targets": targets(),
                "add": {"type": "array", "items": {"type": "string"}},
                "remove": {"type": "array", "items": {"type": "string"}}
            }),
            &["targets"],
        ),
        tool(
            "remind_me",
            "Takes conversations out of the inbox now and brings them back, unread, at the given local time.",
            json!({
                "targets": targets(),
                "at": {"type": "string", "description": "Local date and time, as YYYY-MM-DDTHH:MM."}
            }),
            &["targets", "at"],
        ),
        tool(
            "draft_email",
            "Opens a composer window with a message for the user to review and send. Use this unless the user asked you to send.",
            message_fields(),
            &["body"],
        ),
        tool(
            "send_email",
            "Sends a message. The user approves it first, and can undo it for a few seconds after.",
            message_fields(),
            &["to", "subject", "body"],
        ),
        tool(
            "block_sender",
            "Sends all future mail from an address straight to the Trash, with a Gmail filter.",
            json!({
                "account": account("The account to block the sender in."),
                "email": {"type": "string"}
            }),
            &["account", "email"],
        ),
        tool(
            "get_automatic_reply",
            "Reads an account's out-of-office automatic reply.",
            json!({"account": account("The account.")}),
            &["account"],
        ),
        tool(
            "set_automatic_reply",
            "Turns an account's out-of-office automatic reply on or off. Gmail sends it, even with the computer off.",
            json!({
                "account": account("The account."),
                "enabled": {"type": "boolean"},
                "subject": {"type": "string"},
                "message": {"type": "string", "description": "Plain text."},
                "first_day": {"type": "string", "description": "YYYY-MM-DD. Leave out to start now."},
                "last_day": {"type": "string", "description": "YYYY-MM-DD, inclusive. Leave out for no end."},
                "contacts_only": {"type": "boolean"}
            }),
            &["account", "enabled"],
        ),
        tool(
            "list_rules",
            "Lists an account's Gmail filters, described in words, with their ids.",
            json!({"account": account("The account.")}),
            &["account"],
        ),
        tool(
            "create_rule",
            "Creates a Gmail filter. Give at least one condition and one action.",
            json!({
                "account": account("The account."),
                "from": {"type": "string"},
                "to": {"type": "string"},
                "subject": {"type": "string"},
                "has_words": {"type": "string"},
                "not_words": {"type": "string"},
                "has_attachment": {"type": "boolean"},
                "skip_inbox": {"type": "boolean"},
                "mark_read": {"type": "boolean"},
                "star": {"type": "boolean"},
                "label": {"type": "string", "description": "Label name to apply; created when missing."},
                "never_spam": {"type": "boolean"},
                "delete": {"type": "boolean"}
            }),
            &["account"],
        ),
        tool(
            "delete_rule",
            "Deletes a Gmail filter by the id list_rules gave.",
            json!({"account": account("The account."), "id": {"type": "string"}}),
            &["account", "id"],
        ),
        tool(
            "create_label",
            "Creates a Gmail label. Use a slash to nest it: \"Work/Clients\".",
            json!({"account": account("The account."), "name": {"type": "string"}}),
            &["account", "name"],
        ),
        tool(
            "get_settings",
            "The app's settings that change_setting can set, with their current values.",
            json!({}),
            &[],
        ),
        tool(
            "change_setting",
            "Changes one app setting. Use the names and value shapes get_settings returns.",
            json!({
                "name": {"type": "string"},
                "value": {"description": "The new value, in the same shape get_settings shows."}
            }),
            &["name", "value"],
        ),
        tool(
            "set_signature",
            "Sets the Markdown signature added to new mail from an account. Empty removes it.",
            json!({"account": account("The account."), "text": {"type": "string"}}),
            &["account", "text"],
        ),
        tool(
            "vip",
            "Adds a sender to the VIPs or removes one. VIP mail gets its own mailbox.",
            json!({
                "email": {"type": "string"},
                "name": {"type": "string"},
                "add": {"type": "boolean", "description": "True adds, false removes."}
            }),
            &["email", "add"],
        ),
        tool(
            "create_smart_mailbox",
            "Saves a smart mailbox: conditions that list matching mail from Gmail.",
            json!({
                "name": {"type": "string"},
                "account": account("Limit to one account."),
                "match_all": {"type": "boolean", "description": "True: every condition must hold. False: any. Defaults to true."},
                "conditions": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "field": {"type": "string", "enum": ["from", "to", "subject", "words", "label", "newer-than-days", "larger-than-mb", "has-attachment", "unread", "flagged"]},
                            "value": {"type": "string"}
                        },
                        "required": ["field"],
                        "additionalProperties": false
                    }
                }
            }),
            &["name", "conditions"],
        ),
        tool(
            "categorize_sender",
            "Moves a sender's mail into an inbox category and sorts their future mail there with a Gmail filter.",
            json!({
                "account": account("The account."),
                "email": {"type": "string"},
                "name": {"type": "string", "description": "The sender's name, for the confirmation."},
                "category": {"type": "string", "enum": ["primary", "updates", "promotions", "social"]}
            }),
            &["account", "email", "category"],
        ),
        tool(
            "dismiss_follow_up",
            "Stops suggesting a follow-up for a sent conversation.",
            json!({"account": account("The account."), "thread_id": {"type": "string"}}),
            &["account", "thread_id"],
        ),
        tool(
            "list_hidden_addresses",
            "Lists Hide My Email addresses: plus addresses that deliver to an account and can be turned off.",
            json!({}),
            &[],
        ),
        tool(
            "create_hidden_address",
            "Makes a new Hide My Email address for an account, labels its mail, and copies it for the user.",
            json!({
                "account": account("The account it delivers to."),
                "note": {"type": "string", "description": "Where the user will give it out."}
            }),
            &["account", "note"],
        ),
        tool(
            "set_hidden_address",
            "Turns a Hide My Email address off (its mail goes to the Trash) or back on.",
            json!({"address": {"type": "string"}, "active": {"type": "boolean"}}),
            &["address", "active"],
        ),
        tool(
            "open_conversation",
            "Shows a conversation in the mail window.",
            json!({"account": account("The account."), "thread_id": {"type": "string"}}),
            &["account", "thread_id"],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_have_unique_names_and_object_schemas() {
        let all = specs();
        let mut names: Vec<&str> = all.iter().map(|t| t.name.as_str()).collect();
        names.sort();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count);
        for spec in &all {
            assert_eq!(spec.input_schema["type"], "object", "{}", spec.name);
            for required in spec.input_schema["required"].as_array().unwrap() {
                let key = required.as_str().unwrap();
                assert!(
                    spec.input_schema["properties"].get(key).is_some(),
                    "{} requires missing {key}",
                    spec.name
                );
            }
        }
    }
}
