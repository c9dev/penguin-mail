//! Every mail tool, each declared once: the name the model calls, what the
//! pane calls it, the JSON Schema of its input, and its handler. The
//! handler either runs at once or plans a change and names the question to
//! ask before making it.
//!
//! To add a tool, add an entry to [`catalog`] and the method it runs to
//! `Tools`. Nothing else lists the tools: the offer to the model, the pane's
//! labels, and the tests all read this table.

use std::future::ready;

use mailrs_ai::ToolSpec;
use mailrs_domain::smart::Field;
use mailrs_domain::translate::gettext;
use mailrs_domain::{Category, FlagColor};
use serde_json::{Value, json};

use super::{Answer, MailboxName, Organize, ToolResult, Tools};
use crate::core::RunningEngine;
use mailrs_sync::Accounts;

/// A handler that answers the call itself.
type Handler<A> = for<'a> fn(&'a Tools<A>, &'a Value) -> Answer<'a, ToolResult>;

/// A handler that plans a change and leaves the asking to the table.
type Planner<A> = for<'a> fn(&'a Tools<A>, &'a Value) -> Answer<'a, Result<Plan<'a>, String>>;

/// How a tool runs.
pub(super) enum Run<A: Accounts> {
    /// Straight away: a read, or a change Ctrl+Z undoes, or one the person
    /// sees and finishes, such as a draft in a composer.
    Now(Handler<A>),
    /// Plans the change first, asks the plan's question while Ask Before
    /// Acting is on, and makes the change only on Allow.
    AsksFirst(Planner<A>),
}

/// A change a tool has worked out and not yet made.
pub(super) struct Plan<'a> {
    /// What the person approves. It names what changes, such as the
    /// subject and recipients of a message, so the question comes from
    /// the handler and not from the table. `None` when this call changes
    /// too little to ask about.
    question: Option<String>,
    change: Answer<'a, ToolResult>,
}

impl<'a> Plan<'a> {
    pub(super) fn ask(question: String, change: impl Future<Output = ToolResult> + 'a) -> Plan<'a> {
        Plan {
            question: Some(question),
            change: Box::pin(change),
        }
    }

    pub(super) fn without_asking(change: impl Future<Output = ToolResult> + 'a) -> Plan<'a> {
        Plan {
            question: None,
            change: Box::pin(change),
        }
    }
}

/// One mail tool.
pub(super) struct MailTool<A: Accounts> {
    pub name: &'static str,
    /// What the pane shows while the tool runs.
    pub label: fn() -> String,
    /// What the model reads about the tool.
    pub description: &'static str,
    /// The schema's `properties`.
    pub input: fn() -> Value,
    pub required: &'static [&'static str],
    pub run: Run<A>,
}

impl<A: Accounts> MailTool<A> {
    pub fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.into(),
            description: self.description.into(),
            input_schema: json!({
                "type": "object",
                "properties": (self.input)(),
                "required": self.required,
                "additionalProperties": false,
            }),
        }
    }

    /// Runs the call, asking first when the tool plans a change.
    pub(super) async fn run(&self, tools: &Tools<A>, input: &Value) -> ToolResult {
        match self.run {
            Run::Now(handler) => handler(tools, input).await,
            Run::AsksFirst(planner) => {
                let plan = planner(tools, input).await?;
                if let Some(question) = plan.question
                    && tools.desk.settings().ai.confirm_actions
                    && !tools.effects.confirm(question).await
                {
                    return Err("The user declined.".into());
                }
                plan.change.await
            }
        }
    }
}

/// The tool called `name`, if there is one.
pub(super) fn find<A: Accounts>(name: &str) -> Option<MailTool<A>> {
    catalog().into_iter().find(|tool| tool.name == name)
}

/// Every mail tool the model is offered, in the order it reads them.
pub fn specs() -> Vec<ToolSpec> {
    offered().iter().map(MailTool::spec).collect()
}

/// What the pane calls the mail tool `name`, or `None` for a tool from
/// another source.
pub fn label(name: &str) -> Option<String> {
    offered()
        .into_iter()
        .find(|tool| tool.name == name)
        .map(|tool| (tool.label)())
}

/// The table as the window runs it. A name, a label and a spec do not
/// depend on the accounts, so this one serves every caller that wants
/// only those.
fn offered() -> Vec<MailTool<RunningEngine>> {
    catalog()
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

/// The inbox categories a tool may name. The whole inbox needs none, so
/// `all` is left out.
fn categories() -> Vec<&'static str> {
    Category::ALL
        .into_iter()
        .filter(|c| *c != Category::All)
        .map(Category::key)
        .collect()
}

fn colors() -> Vec<&'static str> {
    FlagColor::ALL.map(FlagColor::as_str).to_vec()
}

/// A smart mailbox's condition fields, in the shape its settings store
/// them.
fn fields() -> Vec<Value> {
    Field::ALL
        .iter()
        .filter_map(|f| serde_json::to_value(f).ok())
        .collect()
}

fn answers() -> Vec<&'static str> {
    mailrs_domain::invitation::Answer::ALL
        .map(mailrs_domain::invitation::Answer::as_str)
        .to_vec()
}

/// Every mail tool. The order is the order the model reads them in.
pub(super) fn catalog<A: Accounts>() -> Vec<MailTool<A>> {
    vec![
        MailTool {
            name: "get_context",
            label: || gettext("Looking at the screen"),
            description: "The current date and time, the user's accounts and labels, the mailbox on screen, the open conversation, and the selected conversations.",
            input: || json!({}),
            required: &[],
            run: Run::Now(|t, _| Box::pin(ready(t.context()))),
        },
        MailTool {
            name: "list_mail",
            label: || gettext("Reading a mailbox"),
            description: "Lists conversations in a mailbox, newest first. Inbox, flagged, sent, drafts, VIPs, and labels read the mail kept on this computer (the last few weeks plus everything in the inbox); junk, trash, and all_mail ask Gmail.",
            input: || {
                json!({
                    "mailbox": {"type": "string", "enum": MailboxName::ALL.map(MailboxName::key), "description": "follow_up lists sent mail that has waited 3 to 30 days for a reply."},
                    "category": {"type": "string", "enum": categories(), "description": "Narrow the inbox to one of Gmail's categories."},
                    "label": {"type": "string", "description": "The label's name, when mailbox is \"label\"."},
                    "account": account("Limit to one account. All accounts when left out."),
                    "unread_only": {"type": "boolean"},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 200, "description": "Defaults to 30."}
                })
            },
            required: &["mailbox"],
            run: Run::Now(|t, input| Box::pin(t.list(input))),
        },
        MailTool {
            name: "search_mail",
            label: || gettext("Searching mail"),
            description: "Searches Gmail with its query syntax across all accounts or one. Reaches all mail, not only recent mail.",
            input: || {
                json!({
                    "query": {"type": "string"},
                    "account": account("Limit to one account."),
                    "limit": {"type": "integer", "minimum": 1, "maximum": 100, "description": "Defaults to 30."}
                })
            },
            required: &["query"],
            run: Run::Now(|t, input| Box::pin(t.search(input))),
        },
        MailTool {
            name: "read_conversation",
            label: || gettext("Reading a conversation"),
            description: "Reads every message in a conversation: senders, recipients, dates, text, and attachment names.",
            input: || {
                json!({
                    "account": account("The conversation's account."),
                    "thread_id": {"type": "string"}
                })
            },
            required: &["account", "thread_id"],
            run: Run::Now(|t, input| Box::pin(t.read_thread(input))),
        },
        MailTool {
            name: "organize",
            label: || gettext("Organizing mail"),
            description: "Archives, trashes, marks, or flags conversations. Reversible; the user can press Ctrl+Z to undo the last change.",
            input: || {
                json!({
                    "targets": targets(),
                    "action": {"type": "string", "enum": Organize::ALL.map(Organize::key)},
                    "color": {"type": "string", "enum": colors(), "description": "Flag color, for action \"flag\". Defaults to red."}
                })
            },
            required: &["targets", "action"],
            run: Run::AsksFirst(|t, input| Box::pin(t.organize(input))),
        },
        MailTool {
            name: "label",
            label: || gettext("Changing labels"),
            description: "Adds or removes Gmail labels on conversations, by label name. Missing labels are created when add names them.",
            input: || {
                json!({
                    "targets": targets(),
                    "add": {"type": "array", "items": {"type": "string"}},
                    "remove": {"type": "array", "items": {"type": "string"}}
                })
            },
            required: &["targets"],
            run: Run::Now(|t, input| Box::pin(t.label(input))),
        },
        MailTool {
            name: "remind_me",
            label: || gettext("Setting reminders"),
            description: "Takes conversations out of the inbox now and brings them back, unread, at the given local time.",
            input: || {
                json!({
                    "targets": targets(),
                    "at": {"type": "string", "description": "Local date and time, as YYYY-MM-DDTHH:MM."}
                })
            },
            required: &["targets", "at"],
            run: Run::Now(|t, input| Box::pin(t.remind(input))),
        },
        MailTool {
            name: "draft_email",
            label: || gettext("Writing a draft"),
            description: "Opens a composer window with a message for the user to review and send. Use this unless the user asked you to send.",
            input: message_fields,
            required: &["body"],
            run: Run::Now(|t, input| Box::pin(t.draft(input))),
        },
        MailTool {
            name: "send_email",
            label: || gettext("Sending mail"),
            description: "Sends a message. The user approves it first, and can undo it for a few seconds after.",
            input: message_fields,
            required: &["to", "subject", "body"],
            run: Run::AsksFirst(|t, input| Box::pin(t.send(input))),
        },
        MailTool {
            name: "block_sender",
            label: || gettext("Blocking a sender"),
            description: "Sends all future mail from an address straight to the Trash, with a Gmail filter.",
            input: || {
                json!({
                    "account": account("The account to block the sender in."),
                    "email": {"type": "string"}
                })
            },
            required: &["account", "email"],
            run: Run::AsksFirst(|t, input| Box::pin(t.block(input))),
        },
        MailTool {
            name: "get_automatic_reply",
            label: || gettext("Checking the automatic reply"),
            description: "Reads an account's out-of-office automatic reply.",
            input: || json!({"account": account("The account.")}),
            required: &["account"],
            run: Run::Now(|t, input| Box::pin(t.get_vacation(input))),
        },
        MailTool {
            name: "set_automatic_reply",
            label: || gettext("Setting the automatic reply"),
            description: "Turns an account's out-of-office automatic reply on or off. Gmail sends it, even with the computer off.",
            input: || {
                json!({
                    "account": account("The account."),
                    "enabled": {"type": "boolean"},
                    "subject": {"type": "string"},
                    "message": {"type": "string", "description": "Plain text."},
                    "first_day": {"type": "string", "description": "YYYY-MM-DD. Leave out to start now."},
                    "last_day": {"type": "string", "description": "YYYY-MM-DD, inclusive. Leave out for no end."},
                    "contacts_only": {"type": "boolean"}
                })
            },
            required: &["account", "enabled"],
            run: Run::AsksFirst(|t, input| Box::pin(t.set_vacation(input))),
        },
        MailTool {
            name: "list_rules",
            label: || gettext("Reading rules"),
            description: "Lists an account's Gmail filters, described in words, with their ids.",
            input: || json!({"account": account("The account.")}),
            required: &["account"],
            run: Run::Now(|t, input| Box::pin(t.list_rules(input))),
        },
        MailTool {
            name: "create_rule",
            label: || gettext("Creating a rule"),
            description: "Creates a Gmail filter. Give at least one condition and one action.",
            input: || {
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
                })
            },
            required: &["account"],
            run: Run::AsksFirst(|t, input| Box::pin(t.create_rule(input))),
        },
        MailTool {
            name: "delete_rule",
            label: || gettext("Deleting a rule"),
            description: "Deletes a Gmail filter by the id list_rules gave.",
            input: || json!({"account": account("The account."), "id": {"type": "string"}}),
            required: &["account", "id"],
            run: Run::AsksFirst(|t, input| Box::pin(t.delete_rule(input))),
        },
        MailTool {
            name: "create_label",
            label: || gettext("Creating a label"),
            description: "Creates a Gmail label. Use a slash to nest it: \"Work/Clients\".",
            input: || json!({"account": account("The account."), "name": {"type": "string"}}),
            required: &["account", "name"],
            run: Run::Now(|t, input| Box::pin(t.create_label(input))),
        },
        MailTool {
            name: "get_settings",
            label: || gettext("Reading settings"),
            description: "The app's settings that change_setting can set, with their current values.",
            input: || json!({}),
            required: &[],
            run: Run::Now(|t, _| Box::pin(ready(Ok(t.settings_json())))),
        },
        MailTool {
            name: "change_setting",
            label: || gettext("Changing a setting"),
            description: "Changes one app setting. Use the names and value shapes get_settings returns.",
            input: || {
                json!({
                    "name": {"type": "string"},
                    "value": {"description": "The new value, in the same shape get_settings shows."}
                })
            },
            required: &["name", "value"],
            run: Run::Now(|t, input| Box::pin(ready(t.change_setting(input)))),
        },
        MailTool {
            name: "set_signature",
            label: || gettext("Setting a signature"),
            description: "Sets the Markdown signature added to new mail from an account. Empty removes it.",
            input: || json!({"account": account("The account."), "text": {"type": "string"}}),
            required: &["account", "text"],
            run: Run::Now(|t, input| Box::pin(ready(t.signature(input)))),
        },
        MailTool {
            name: "vip",
            label: || gettext("Updating VIPs"),
            description: "Adds a sender to the VIPs or removes one. VIP mail gets its own mailbox.",
            input: || {
                json!({
                    "email": {"type": "string"},
                    "name": {"type": "string"},
                    "add": {"type": "boolean", "description": "True adds, false removes."}
                })
            },
            required: &["email", "add"],
            run: Run::Now(|t, input| Box::pin(ready(t.vip(input)))),
        },
        MailTool {
            name: "create_smart_mailbox",
            label: || gettext("Creating a smart mailbox"),
            description: "Saves a smart mailbox: conditions that list matching mail from Gmail.",
            input: || {
                json!({
                    "name": {"type": "string"},
                    "account": account("Limit to one account."),
                    "match_all": {"type": "boolean", "description": "True: every condition must hold. False: any. Defaults to true."},
                    "conditions": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "field": {"type": "string", "enum": fields()},
                                "value": {"type": "string"}
                            },
                            "required": ["field"],
                            "additionalProperties": false
                        }
                    }
                })
            },
            required: &["name", "conditions"],
            run: Run::Now(|t, input| Box::pin(ready(t.smart(input)))),
        },
        MailTool {
            name: "categorize_sender",
            label: || gettext("Sorting a sender"),
            description: "Moves a sender's mail into an inbox category and sorts their future mail there with a Gmail filter.",
            input: || {
                json!({
                    "account": account("The account."),
                    "email": {"type": "string"},
                    "name": {"type": "string", "description": "The sender's name, for the confirmation."},
                    "category": {"type": "string", "enum": categories()}
                })
            },
            required: &["account", "email", "category"],
            run: Run::AsksFirst(|t, input| Box::pin(t.categorize(input))),
        },
        MailTool {
            name: "dismiss_follow_up",
            label: || gettext("Dismissing a follow-up"),
            description: "Stops suggesting a follow-up for a sent conversation.",
            input: || json!({"account": account("The account."), "thread_id": {"type": "string"}}),
            required: &["account", "thread_id"],
            run: Run::Now(|t, input| Box::pin(t.dismiss_follow_up(input))),
        },
        MailTool {
            name: "list_hidden_addresses",
            label: || gettext("Reading hidden addresses"),
            description: "Lists Hide My Email addresses: plus addresses that deliver to an account and can be turned off.",
            input: || json!({}),
            required: &[],
            run: Run::Now(|t, _| Box::pin(ready(Ok(t.hidden_list())))),
        },
        MailTool {
            name: "create_hidden_address",
            label: || gettext("Making a hidden address"),
            description: "Makes a new Hide My Email address for an account, labels its mail, and copies it for the user.",
            input: || {
                json!({
                    "account": account("The account it delivers to."),
                    "note": {"type": "string", "description": "Where the user will give it out."}
                })
            },
            required: &["account", "note"],
            run: Run::Now(|t, input| Box::pin(t.hidden_create(input))),
        },
        MailTool {
            name: "set_hidden_address",
            label: || gettext("Changing a hidden address"),
            description: "Turns a Hide My Email address off (its mail goes to the Trash) or back on.",
            input: || json!({"address": {"type": "string"}, "active": {"type": "boolean"}}),
            required: &["address", "active"],
            run: Run::Now(|t, input| Box::pin(t.hidden_set(input))),
        },
        MailTool {
            name: "open_conversation",
            label: || gettext("Opening a conversation"),
            description: "Shows a conversation in the mail window.",
            input: || json!({"account": account("The account."), "thread_id": {"type": "string"}}),
            required: &["account", "thread_id"],
            run: Run::Now(|t, input| Box::pin(ready(t.open(input)))),
        },
        MailTool {
            name: "mute",
            label: || gettext("Muting conversations"),
            description: "Mutes conversations: they leave the inbox, and replies to them skip it too. Set mute to false to unmute and bring them back. Reversible with Ctrl+Z.",
            input: || {
                json!({
                    "targets": targets(),
                    "mute": {"type": "boolean", "description": "False unmutes. Defaults to true."}
                })
            },
            required: &["targets"],
            run: Run::Now(|t, input| Box::pin(t.mute(input))),
        },
        MailTool {
            name: "delete_forever",
            label: || gettext("Deleting mail forever"),
            description: "Erases conversations from Gmail for good. Nothing brings them back, so use organize with trash unless the user asked for this. The user approves it first.",
            input: || json!({"targets": targets()}),
            required: &["targets"],
            run: Run::AsksFirst(|t, input| Box::pin(t.delete_forever(input))),
        },
        MailTool {
            name: "send_later",
            label: || gettext("Scheduling a message"),
            description: "Schedules a message to go out at a local time. Give the message's fields, as for send_email, or name a draft the user already wrote. The user approves it first.",
            input: || {
                let mut fields = message_fields();
                fields["at"] = json!({"type": "string", "description": "When to send it, local time, as YYYY-MM-DDTHH:MM."});
                fields["draft"] = json!({
                    "type": "object",
                    "description": "A saved draft to send as it stands, instead of the other fields: the conversation list_mail with mailbox drafts gave.",
                    "properties": {
                        "account": {"type": "string"},
                        "thread_id": {"type": "string"}
                    },
                    "required": ["account", "thread_id"],
                    "additionalProperties": false
                });
                fields
            },
            required: &["at"],
            run: Run::AsksFirst(|t, input| Box::pin(t.send_later(input))),
        },
        MailTool {
            name: "list_templates",
            label: || gettext("Reading templates"),
            description: "Lists the user's saved templates with their subjects and bodies. Placeholders such as {{first_name}} fill in when a template is used.",
            input: || json!({}),
            required: &[],
            run: Run::Now(|t, _| Box::pin(t.list_templates())),
        },
        MailTool {
            name: "insert_template",
            label: || gettext("Writing from a template"),
            description: "Opens a composer with a saved template, its placeholders filled in from the first recipient, the subject, and today's date. Takes the same fields as draft_email, with the template in place of the body.",
            input: || {
                let mut fields = message_fields();
                if let Some(fields) = fields.as_object_mut() {
                    fields.remove("body");
                }
                fields["template"] = json!({"type": "string", "description": "The template's name, as list_templates gave it."});
                fields
            },
            required: &["template"],
            run: Run::Now(|t, input| Box::pin(t.insert_template(input))),
        },
        MailTool {
            name: "unsubscribe",
            label: || gettext("Unsubscribing"),
            description: "Leaves the mailing list a conversation came from, using its List-Unsubscribe link: a one-click request, an email to the list, or the sender's page opened in the browser. The user approves it first.",
            input: || json!({"account": account("The account."), "thread_id": {"type": "string"}}),
            required: &["account", "thread_id"],
            run: Run::AsksFirst(|t, input| Box::pin(t.unsubscribe(input))),
        },
        MailTool {
            name: "read_attachment",
            label: || gettext("Reading an attachment"),
            description: "Reads an attachment as text: plain text, HTML, and PDF files. Other files come back with a note saying they cannot be read.",
            input: || {
                json!({
                    "account": account("The account."),
                    "message_id": {"type": "string", "description": "The message_id read_conversation gave."},
                    "attachment": {"type": "string", "description": "The file name, or its number in the message's list, starting at 1."}
                })
            },
            required: &["account", "message_id", "attachment"],
            run: Run::Now(|t, input| Box::pin(t.read_attachment(input))),
        },
        MailTool {
            name: "find_contact",
            label: || gettext("Looking up a contact"),
            description: "Looks people up by name, address, or organization: first in the address books Penguin Mail keeps from Google Contacts, then among the people in stored mail.",
            input: || json!({"query": {"type": "string", "description": "Words to find, such as \"priya\" or \"fernwood\"."}}),
            required: &["query"],
            run: Run::Now(|t, input| Box::pin(t.find_contact(input))),
        },
        MailTool {
            name: "list_events",
            label: || gettext("Reading the calendar"),
            description: "Lists the events on an account's Google calendar between two local times, with their ids, times, places, guests, and the user's own answer.",
            input: || {
                json!({
                    "from": {"type": "string", "description": "Local time as YYYY-MM-DDTHH:MM, or a day as YYYY-MM-DD for its start."},
                    "to": {"type": "string", "description": "Local time as YYYY-MM-DDTHH:MM, or a day as YYYY-MM-DD, which counts in full."},
                    "account": account("The calendar's account. Defaults to the default account.")
                })
            },
            required: &["from", "to"],
            run: Run::Now(|t, input| Box::pin(t.list_events(input))),
        },
        MailTool {
            name: "find_free_time",
            label: || gettext("Finding free time"),
            description: "Finds free stretches of at least the given length on an account's calendar, inside working hours on each day between two local times.",
            input: || {
                json!({
                    "from": {"type": "string", "description": "Local time as YYYY-MM-DDTHH:MM, or a day as YYYY-MM-DD."},
                    "to": {"type": "string", "description": "Local time as YYYY-MM-DDTHH:MM, or a day as YYYY-MM-DD, which counts in full."},
                    "minutes": {"type": "integer", "minimum": 5, "maximum": 1440, "description": "How long the free stretch must be."},
                    "day_starts": {"type": "string", "description": "HH:MM. Defaults to 09:00."},
                    "day_ends": {"type": "string", "description": "HH:MM. Defaults to 18:00."},
                    "weekends": {"type": "boolean", "description": "Include Saturdays and Sundays. Defaults to false."},
                    "account": account("The calendar's account. Defaults to the default account.")
                })
            },
            required: &["from", "to", "minutes"],
            run: Run::Now(|t, input| Box::pin(t.find_free_time(input))),
        },
        MailTool {
            name: "create_event",
            label: || gettext("Adding an event"),
            description: "Puts an event on an account's Google calendar and invites its guests. Give start and end as local times, or both as days for an all-day event. The user approves it first.",
            input: || {
                json!({
                    "title": {"type": "string"},
                    "start": {"type": "string", "description": "Local time as YYYY-MM-DDTHH:MM, or the first day as YYYY-MM-DD."},
                    "end": {"type": "string", "description": "Local time as YYYY-MM-DDTHH:MM, or the last day as YYYY-MM-DD."},
                    "attendees": {"type": "array", "items": {"type": "string"}, "description": "Guests' addresses. Google emails each an invitation."},
                    "location": {"type": "string"},
                    "description": {"type": "string"},
                    "account": account("The calendar's account. Defaults to the default account.")
                })
            },
            required: &["title", "start", "end"],
            run: Run::AsksFirst(|t, input| Box::pin(t.create_event(input))),
        },
        MailTool {
            name: "update_event",
            label: || gettext("Changing an event"),
            description: "Changes an event on the calendar by the id list_events gave. Only the fields given change; attendees replaces the guest list. Google tells the guests. The user approves it first.",
            input: || {
                json!({
                    "id": {"type": "string"},
                    "current_title": {"type": "string", "description": "The event's title now, for the confirmation."},
                    "title": {"type": "string", "description": "A new title."},
                    "start": {"type": "string", "description": "Local time as YYYY-MM-DDTHH:MM, or a day as YYYY-MM-DD."},
                    "end": {"type": "string", "description": "Local time as YYYY-MM-DDTHH:MM, or the last day as YYYY-MM-DD."},
                    "attendees": {"type": "array", "items": {"type": "string"}},
                    "location": {"type": "string"},
                    "description": {"type": "string"},
                    "account": account("The calendar's account. Defaults to the default account.")
                })
            },
            required: &["id"],
            run: Run::AsksFirst(|t, input| Box::pin(t.update_event(input))),
        },
        MailTool {
            name: "delete_event",
            label: || gettext("Deleting an event"),
            description: "Deletes an event from the calendar by the id list_events gave. Google tells the guests. The user approves it first.",
            input: || {
                json!({
                    "id": {"type": "string"},
                    "title": {"type": "string", "description": "The event's title, for the confirmation."},
                    "account": account("The calendar's account. Defaults to the default account.")
                })
            },
            required: &["id"],
            run: Run::AsksFirst(|t, input| Box::pin(t.delete_event(input))),
        },
        MailTool {
            name: "answer_invitation",
            label: || gettext("Answering an invitation"),
            description: "Answers the meeting invitation in a message: yes, no, or maybe. Google Calendar records it when it holds the event; otherwise the answer goes to the organizer by email. The user approves it first.",
            input: || {
                json!({
                    "account": account("The message's account."),
                    "message_id": {"type": "string", "description": "The message read_conversation marked as holding an invitation."},
                    "answer": {"type": "string", "enum": answers()}
                })
            },
            required: &["account", "message_id", "answer"],
            run: Run::AsksFirst(|t, input| Box::pin(t.answer_invitation(input))),
        },
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

    /// The pane names every mail tool from the catalog, so none falls back
    /// to the label a tool from nowhere gets.
    #[test]
    fn every_mail_tool_has_a_label_of_its_own() {
        for spec in specs() {
            let name = label(&spec.name).expect("a mail tool has a label");
            assert_ne!(name, gettext("Working"), "{}", spec.name);
        }
        assert_eq!(label("web_search"), None);
    }
}
