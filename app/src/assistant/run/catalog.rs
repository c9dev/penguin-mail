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

use super::manage::COLOR_KEYS;
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
        "bcc": {"type": "array", "items": {"type": "string"}, "description": "Blind copies: the other recipients do not see these."},
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
        },
        "forward": {
            "type": "object",
            "description": "Set to forward a message: it goes below the body under a forwarded-message header, with its files. The subject defaults to \"Fwd: \" and the original's. Not with reply_to.",
            "properties": {
                "account": {"type": "string"},
                "message_id": {"type": "string", "description": "The message_id read_conversation gave."}
            },
            "required": ["account", "message_id"],
            "additionalProperties": false
        },
        "attachments": attachments(),
        "sign": {"type": "boolean", "description": "Sign with the sender's OpenPGP key or S/MIME certificate. Defaults to the user's setting."},
        "encrypt": {"type": "boolean", "description": "Encrypt to every recipient's key or certificate. Fails, naming who, when a recipient has none. Defaults to the user's setting, which encrypts only when it can."}
    })
}

/// Files a message carries: out of messages in the mail, or off this
/// computer by path.
fn attachments() -> Value {
    json!({
        "type": "array",
        "description": "Files to attach. Give message_id and attachment for a file in a message, or path for a file on this computer the user named. The user sees every path before anything is read; hidden folders and system folders are refused.",
        "items": {
            "type": "object",
            "properties": {
                "message_id": {"type": "string", "description": "The message_id read_conversation gave."},
                "attachment": {"type": "string", "description": "The file name, or its number in the message's list, starting at 1."},
                "account": {"type": "string", "description": "The message's account, when it is not the one the message goes from."},
                "path": {"type": "string", "description": "A file on this computer, as a full path or starting with ~/."}
            },
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
            description: "Lists conversations in a mailbox, newest first. Inbox, flagged, sent, drafts, VIPs, muted, and labels read the mail kept on this computer (the last few weeks plus everything in the inbox); archive, junk, trash, all_mail, and smart mailboxes ask Gmail. archive is received mail taken out of the inbox. send_later, outbox, and reminders list what waits, soonest first: each row says why it waits and when it goes or returns, and its account, thread_id, and message_id are the target send_now, cancel_send, delete_queued, reschedule, cancel_reminder, and change_reminder take.",
            input: || {
                json!({
                    "mailbox": {"type": "string", "enum": MailboxName::ALL.map(MailboxName::key), "description": "follow_up lists sent mail that has waited 3 to 30 days for a reply."},
                    "category": {"type": "string", "enum": categories(), "description": "Narrow the inbox to one of Gmail's categories."},
                    "label": {"type": "string", "description": "The label's name, when mailbox is \"label\"."},
                    "name": {"type": "string", "description": "The smart mailbox's name, when mailbox is \"smart\"."},
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
            description: "Archives, trashes, marks, or flags conversations, and takes those back: unflag takes off the flag and its star, move_to_inbox brings mail back out of the Trash, and not_junk takes mail out of Junk into the inbox. Reversible; the user can press Ctrl+Z, or you can call undo, to take back the last change.",
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
            description: "Adds or removes Gmail labels on conversations, by label name. When add names a label an account lacks, the user decides whether to create it; if they decline, only mail in accounts that have the label gets it.",
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
            description: "Opens a composer window with a message for the user to review and send. Use this unless the user asked you to send. The user approves files from this computer first.",
            input: message_fields,
            required: &["body"],
            run: Run::AsksFirst(|t, input| Box::pin(t.draft(input))),
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
            name: "list_drafts",
            label: || gettext("Reading drafts"),
            description: "Lists the drafts waiting in Gmail, newest first, with their subjects, recipients, dates and message ids. Read one with read_conversation.",
            input: || json!({"account": account("Limit to one account. All accounts when left out.")}),
            required: &[],
            run: Run::Now(|t, input| Box::pin(t.list_drafts(input))),
        },
        MailTool {
            name: "edit_draft",
            label: || gettext("Changing a draft"),
            description: "Changes a draft Gmail keeps and saves it back. Only the fields given change: to, cc and bcc replace the lists, body replaces the whole text, attachments adds files, remove_attachments takes files out by name. An encrypted draft stays encrypted unless encrypt is false. The user approves it first.",
            input: || {
                json!({
                    "account": account("The draft's account."),
                    "message_id": {"type": "string", "description": "The draft's message_id, from list_drafts."},
                    "to": {"type": "array", "items": {"type": "string"}},
                    "cc": {"type": "array", "items": {"type": "string"}},
                    "bcc": {"type": "array", "items": {"type": "string"}},
                    "subject": {"type": "string"},
                    "body": {"type": "string", "description": "The new text in Markdown, signature included. Read the draft first to keep what should stay."},
                    "attachments": attachments(),
                    "remove_attachments": {"type": "array", "items": {"type": "string"}, "description": "File names to take out."},
                    "sign": {"type": "boolean"},
                    "encrypt": {"type": "boolean"}
                })
            },
            required: &["account", "message_id"],
            run: Run::AsksFirst(|t, input| Box::pin(t.edit_draft(input))),
        },
        MailTool {
            name: "delete_draft",
            label: || gettext("Deleting a draft"),
            description: "Deletes a draft from Gmail for good. The user approves it first.",
            input: || {
                json!({
                    "account": account("The draft's account."),
                    "message_id": {"type": "string", "description": "The draft's message_id, from list_drafts."}
                })
            },
            required: &["account", "message_id"],
            run: Run::AsksFirst(|t, input| Box::pin(t.delete_draft(input))),
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
            name: "rename_label",
            label: || gettext("Renaming a label"),
            description: "Renames a Gmail label. Labels nested under it move along. The user approves it first.",
            input: || {
                json!({
                    "account": account("The account."),
                    "label": {"type": "string", "description": "The label's name now."},
                    "new_name": {"type": "string", "description": "Use a slash to nest it: \"Work/Clients\"."}
                })
            },
            required: &["account", "label", "new_name"],
            run: Run::AsksFirst(|t, input| Box::pin(t.rename_label(input))),
        },
        MailTool {
            name: "recolor_label",
            label: || gettext("Coloring a label"),
            description: "Gives a Gmail label one of the colours of Gmail's palette. The user approves it first.",
            input: || {
                json!({
                    "account": account("The account."),
                    "label": {"type": "string", "description": "The label's name."},
                    "color": {"type": "string", "enum": COLOR_KEYS}
                })
            },
            required: &["account", "label", "color"],
            run: Run::AsksFirst(|t, input| Box::pin(t.recolor_label(input))),
        },
        MailTool {
            name: "delete_label",
            label: || gettext("Deleting a label"),
            description: "Deletes a Gmail label. Its mail stays in Gmail without it, and labels nested under it stay. The user sees how many conversations carry it and approves it first.",
            input: || json!({"account": account("The account."), "label": {"type": "string", "description": "The label's name."}}),
            required: &["account", "label"],
            run: Run::AsksFirst(|t, input| Box::pin(t.delete_label(input))),
        },
        MailTool {
            name: "list_smart_mailboxes",
            label: || gettext("Reading smart mailboxes"),
            description: "Lists the smart mailboxes with their ids, conditions, and the Gmail search each one runs.",
            input: || json!({}),
            required: &[],
            run: Run::Now(|t, _| Box::pin(ready(Ok(t.list_smart_mailboxes())))),
        },
        MailTool {
            name: "update_smart_mailbox",
            label: || gettext("Changing a smart mailbox"),
            description: "Changes a smart mailbox by its name or id. Only the fields given change; conditions replaces the whole list.",
            input: || {
                json!({
                    "mailbox": {"type": "string", "description": "The smart mailbox's name or id, as list_smart_mailboxes gave it."},
                    "name": {"type": "string", "description": "A new name."},
                    "account": account("Limit it to this account."),
                    "all_accounts": {"type": "boolean", "description": "True makes it list mail from every account."},
                    "match_all": {"type": "boolean", "description": "True: every condition must hold. False: any."},
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
            required: &["mailbox"],
            run: Run::Now(|t, input| Box::pin(ready(t.update_smart_mailbox(input)))),
        },
        MailTool {
            name: "delete_smart_mailbox",
            label: || gettext("Deleting a smart mailbox"),
            description: "Deletes a smart mailbox by its name or id. The mail it lists stays where it is. The user approves it first.",
            input: || json!({"mailbox": {"type": "string", "description": "The smart mailbox's name or id."}}),
            required: &["mailbox"],
            run: Run::AsksFirst(|t, input| Box::pin(t.delete_smart_mailbox(input))),
        },
        MailTool {
            name: "save_template",
            label: || gettext("Saving a template"),
            description: "Saves a template the composer can insert. A template with the same name is replaced, and the user is told so. Placeholders such as {{first_name}} fill in when it is used. The user approves it first.",
            input: || {
                json!({
                    "name": {"type": "string"},
                    "body": {"type": "string", "description": "The template in Markdown."},
                    "subject": {"type": "string", "description": "The subject a message takes when it has none."}
                })
            },
            required: &["name", "body"],
            run: Run::AsksFirst(|t, input| Box::pin(t.save_template(input))),
        },
        MailTool {
            name: "delete_template",
            label: || gettext("Deleting a template"),
            description: "Deletes a saved template by name. The user approves it first.",
            input: || json!({"name": {"type": "string"}}),
            required: &["name"],
            run: Run::AsksFirst(|t, input| Box::pin(t.delete_template(input))),
        },
        MailTool {
            name: "create_contact",
            label: || gettext("Adding a contact"),
            description: "Adds a person to an account's Google Contacts. Give a name or an address at least. The user approves it first.",
            input: || {
                json!({
                    "account": account("The account whose contacts get the person. Defaults to the default account."),
                    "name": {"type": "string"},
                    "emails": {"type": "array", "items": {"type": "string"}},
                    "phones": {"type": "array", "items": {"type": "string"}},
                    "organization": {"type": "string"}
                })
            },
            required: &[],
            run: Run::AsksFirst(|t, input| Box::pin(t.create_contact(input))),
        },
        MailTool {
            name: "update_contact",
            label: || gettext("Changing a contact"),
            description: "Changes a person in an account's Google Contacts. Only the fields given change; emails and phones replace the whole list, and an empty string clears a field. The user approves it first.",
            input: || {
                json!({
                    "contact": {"type": "string", "description": "The id find_contact gave, or one of the contact's addresses."},
                    "account": account("The account the contact belongs to. Defaults to the default account."),
                    "name": {"type": "string"},
                    "emails": {"type": "array", "items": {"type": "string"}},
                    "phones": {"type": "array", "items": {"type": "string"}},
                    "organization": {"type": "string"}
                })
            },
            required: &["contact"],
            run: Run::AsksFirst(|t, input| Box::pin(t.update_contact(input))),
        },
        MailTool {
            name: "list_image_senders",
            label: || gettext("Reading who may load images"),
            description: "Lists the senders and domains whose remote images load without asking, and the app's remote images setting.",
            input: || json!({}),
            required: &[],
            run: Run::Now(|t, _| Box::pin(t.list_image_senders())),
        },
        MailTool {
            name: "allow_images",
            label: || gettext("Allowing remote images"),
            description: "Lets a sender's mail load remote images from now on, or everyone's at a domain. A remote image tells the sender when the mail was opened. The user approves it first.",
            input: || {
                json!({
                    "sender": {"type": "string", "description": "An address, or a domain such as example.com."},
                    "whole_domain": {"type": "boolean", "description": "True allows everyone at the address's domain. A bare domain always means everyone there."}
                })
            },
            required: &["sender"],
            run: Run::AsksFirst(|t, input| Box::pin(t.allow_images(input))),
        },
        MailTool {
            name: "forget_image_sender",
            label: || gettext("Blocking remote images"),
            description: "Takes a sender or domain off the list whose remote images load, so their mail asks again. The user approves it first.",
            input: || json!({"sender": {"type": "string", "description": "The address or domain as list_image_senders gave it."}}),
            required: &["sender"],
            run: Run::AsksFirst(|t, input| Box::pin(t.forget_image_sender(input))),
        },
        MailTool {
            name: "export_mail",
            label: || gettext("Exporting mail"),
            description: "Saves mail to a file: conversations as one mbox file, which other mail programs import, or one message as an .eml file. The file goes in the Downloads folder unless the user names a place. The user approves it first, and hears when a file would be replaced.",
            input: || {
                json!({
                    "targets": targets(),
                    "format": {"type": "string", "enum": ["mbox", "eml"], "description": "Defaults to mbox. eml takes one target with its message_id."},
                    "path": {"type": "string", "description": "A folder or file the user named. Relative paths count from Downloads; ~ is the home folder."}
                })
            },
            required: &["targets"],
            run: Run::AsksFirst(|t, input| Box::pin(t.export_mail(input))),
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
            name: "send_now",
            label: || gettext("Sending a waiting message"),
            description: "Sends messages from Send Later or the Outbox now, rather than at their time or next try. Give the rows list_mail returned for mailbox send_later or outbox. The user approves it first.",
            input: || json!({"targets": targets()}),
            required: &["targets"],
            run: Run::AsksFirst(|t, input| Box::pin(t.send_now(input))),
        },
        MailTool {
            name: "cancel_send",
            label: || gettext("Canceling a scheduled message"),
            description: "Stops messages in Send Later from going out. Each goes back to Gmail's Drafts; one Gmail cannot take right now opens in a composer for the user to save. The user approves it first.",
            input: || json!({"targets": targets()}),
            required: &["targets"],
            run: Run::AsksFirst(|t, input| Box::pin(t.cancel_send(input))),
        },
        MailTool {
            name: "delete_queued",
            label: || gettext("Deleting from the Outbox"),
            description: "Deletes messages from the Outbox, so they are never sent and nothing is kept. For Send Later, use cancel_send. The user approves it first.",
            input: || json!({"targets": targets()}),
            required: &["targets"],
            run: Run::AsksFirst(|t, input| Box::pin(t.delete_queued(input))),
        },
        MailTool {
            name: "reschedule",
            label: || gettext("Changing when a message goes"),
            description: "Gives messages in Send Later a new time to go out. The user approves it first.",
            input: || {
                json!({
                    "targets": targets(),
                    "at": {"type": "string", "description": "The new time, local, as YYYY-MM-DDTHH:MM."}
                })
            },
            required: &["targets", "at"],
            run: Run::AsksFirst(|t, input| Box::pin(t.reschedule(input))),
        },
        MailTool {
            name: "list_reminders",
            label: || gettext("Reading reminders"),
            description: "Lists the conversations set aside with remind_me or Remind Me, soonest first, with when each comes back to the inbox.",
            input: || json!({}),
            required: &[],
            run: Run::Now(|t, _| Box::pin(t.list_reminders())),
        },
        MailTool {
            name: "cancel_reminder",
            label: || gettext("Canceling a reminder"),
            description: "Drops the reminders on conversations and puts them back in the inbox now. The user approves it first.",
            input: || json!({"targets": targets()}),
            required: &["targets"],
            run: Run::AsksFirst(|t, input| Box::pin(t.cancel_reminder(input))),
        },
        MailTool {
            name: "change_reminder",
            label: || gettext("Changing a reminder"),
            description: "Moves the reminders on conversations to a new local time. The user approves it first.",
            input: || {
                json!({
                    "targets": targets(),
                    "at": {"type": "string", "description": "The new time, local, as YYYY-MM-DDTHH:MM."}
                })
            },
            required: &["targets", "at"],
            run: Run::AsksFirst(|t, input| Box::pin(t.change_reminder(input))),
        },
        MailTool {
            name: "unmute",
            label: || gettext("Unmuting conversations"),
            description: "Unmutes conversations and puts them back in the inbox, so their replies arrive there again. list_mail with mailbox muted finds them. Reversible with Ctrl+Z.",
            input: || json!({"targets": targets()}),
            required: &["targets"],
            run: Run::Now(|t, input| Box::pin(t.unmute(input))),
        },
        MailTool {
            name: "undo",
            label: || gettext("Undoing the last change"),
            description: "Takes back the newest mail change on the undo stack, as Ctrl+Z does, whether you or the user made it: an archive, trash, flag, label, mute, or reminder. Each call takes back one, newest first. The user approves it first.",
            input: || json!({}),
            required: &[],
            run: Run::AsksFirst(|t, input| Box::pin(t.undo(input))),
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
            name: "list_newsletters",
            label: || gettext("Finding your newsletters"),
            description: "Lists the senders whose mail reads as a newsletter over the last 90 days, newest first, with how many messages each sent, how their list lets go (way_out), and the conversation to unsubscribe through. Use it before unsubscribe, so \"the Figma one\" becomes an account and a thread_id.",
            input: || {
                json!({
                    "account": account("The account. Defaults to every connected account."),
                    "query": {"type": "string", "description": "Words to narrow the list by, matched against the sender's name and address."}
                })
            },
            required: &[],
            run: Run::Now(|t, input| Box::pin(t.list_newsletters(input))),
        },
        MailTool {
            name: "unsubscribe",
            label: || gettext("Unsubscribing"),
            description: "Leaves the mailing lists 1 to 20 conversations came from, as list_newsletters gives them: a one-click request, an email to the list, or the sender's own unsubscribe page, which Penguin Mail loads out of sight, fills in and submits. It opens one dialog naming every list and what will be pressed, and does nothing the user does not tick there. A page takes up to 45 seconds, so a long list can take minutes; wait for the answer rather than calling again. Each list comes back as done, unclear (submitted, page said nothing), waiting (the request mail sits in the Outbox until Gmail takes it), failed with a reason, opened (the page needed the user and opened in their browser), or declined.",
            input: || {
                json!({
                    "conversations": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": 20,
                        "description": "The conversations whose lists to leave, one a list.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "account": account("The account's email address."),
                                "thread_id": {"type": "string"}
                            },
                            "required": ["account", "thread_id"],
                            "additionalProperties": false
                        }
                    }
                })
            },
            required: &["conversations"],
            // The dialog this tool opens names every list, the button it
            // will press and the address it will type, and nothing runs
            // until the user ticks and confirms there. That answer is the
            // approval, so the pane's card would ask the same question
            // twice and in vaguer words.
            run: Run::Now(|t, input| Box::pin(t.unsubscribe(input))),
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
