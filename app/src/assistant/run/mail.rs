//! The mail tools that came after the first set: muting, erasing, Send
//! Later, templates, unsubscribing, reading an attachment, and finding a
//! person in the address book. Each one goes through the module the window
//! uses for the same thing, so the assistant cannot do what the user
//! could not.

use std::time::Duration;

use mailrs_domain::MessageBody;
use mailrs_store::{address_book, contacts, templates};
use tokio::io::AsyncWriteExt;

use super::*;
use crate::templates::{Filling, expand, today};
use crate::unsubscribe::choose;

/// The most text an attachment gives the model, in characters.
const MOST_ATTACHMENT_CHARS: usize = 20_000;

/// The most people each half of a contact search gives back.
const MOST_PEOPLE: usize = 10;

/// How long `pdftotext` may take over one file.
const PDF_SECONDS: u64 = 30;

/// How an attachment turns into text the model can read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Text,
    Html,
    Pdf,
    /// Anything else: pictures, archives, office files.
    Other,
}

/// What kind of file an attachment is, by its type and, since senders
/// often label everything `application/octet-stream`, by its name.
pub(super) fn kind_of(mime: &str, name: &str) -> Kind {
    let mime = mime.to_ascii_lowercase();
    let extension = name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();
    let ext = extension.as_str();
    if mime == "text/html" || matches!(ext, "html" | "htm") {
        Kind::Html
    } else if mime == "application/pdf" || ext == "pdf" {
        Kind::Pdf
    } else if mime.starts_with("text/")
        || matches!(
            mime.as_str(),
            "application/json" | "application/xml" | "application/csv" | "application/ics"
        )
        || matches!(
            ext,
            "txt" | "md" | "csv" | "tsv" | "json" | "xml" | "ics" | "log" | "vcf" | "yaml" | "yml"
        )
    {
        Kind::Text
    } else {
        Kind::Other
    }
}

/// The text of a PDF, from poppler's `pdftotext`. `None` when this computer
/// has no `pdftotext` to ask.
pub(super) async fn pdf_text(bytes: Vec<u8>) -> Result<Option<String>, String> {
    let spawned = tokio::process::Command::new("pdftotext")
        .args(["-layout", "-enc", "UTF-8", "-", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("Could not start pdftotext: {err}")),
    };
    // The file goes in while the text comes out, so a large PDF cannot
    // fill one pipe while pdftotext waits on the other.
    let mut stdin = child.stdin.take().ok_or("pdftotext took no input.")?;
    let feeding = tokio::spawn(async move {
        let _ = stdin.write_all(&bytes).await;
    });
    let output = tokio::time::timeout(Duration::from_secs(PDF_SECONDS), child.wait_with_output())
        .await
        .map_err(|_| "pdftotext took too long over that PDF.".to_string())?
        .map_err(|err| format!("pdftotext failed: {err}"))?;
    let _ = feeding.await;
    if !output.status.success() {
        return Err("pdftotext could not read that PDF.".into());
    }
    Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned()))
}

/// The unsubscribe links of the newest message in the thread that has
/// any, with the message they came from.
fn newest_with_list(
    found: &[(mailrs_domain::MessageMeta, Option<MessageBody>)],
) -> Option<(&mailrs_domain::MessageMeta, &MessageBody)> {
    found.iter().rev().find_map(|(meta, body)| {
        let body = body.as_ref()?;
        body.list_unsubscribe.as_ref()?;
        Some((meta, body))
    })
}

impl<A: Accounts> Tools<A> {
    pub(super) async fn find_contact(&self, input: &Value) -> ToolResult {
        let query = required(input, "query")?;
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        let wanted = query.clone();
        let (known, suggested) = self
            .read(move |c| Ok((address_book::search(c, &wanted)?, contacts::suggestions(c)?)))
            .await?;
        let people: Vec<Value> = known
            .iter()
            .take(MOST_PEOPLE)
            .map(|contact| {
                json!({
                    "id": contact.resource,
                    "name": contact.name,
                    "emails": contact.emails,
                    "organization": contact.organization,
                    "phone": contact.phone,
                    "account": self.email_of(contact.account_id),
                })
            })
            .collect();
        // People the address books do not hold, found in stored mail. They
        // come second, as the recipient suggestions put them.
        let from_mail: Vec<Value> = suggested
            .iter()
            .filter(|s| !s.known)
            .filter(|s| {
                let text =
                    format!("{} {}", s.name.as_deref().unwrap_or_default(), s.email).to_lowercase();
                words.iter().all(|word| text.contains(word))
            })
            .take(MOST_PEOPLE)
            .map(|s| json!({"name": s.name, "email": s.email}))
            .collect();
        Ok(json!({"contacts": people, "from_mail": from_mail}))
    }

    pub(super) async fn mute(&self, input: &Value) -> ToolResult {
        let targets = self.parse_targets(input)?;
        let muted = flag(input, "mute").unwrap_or(true);
        report(&self.act(targets, MailAction::Mute { muted }).await)
    }

    pub(super) async fn delete_forever<'a>(&'a self, input: &'a Value) -> Result<Plan<'a>, String> {
        let targets = self.parse_targets(input)?;
        let first = targets.first().ok_or("`targets` is empty")?.account_id;
        let account = self
            .desk
            .accounts()
            .into_iter()
            .find(|a| a.id == first)
            .ok_or("That account is gone.")?;
        let count = targets.len();
        let question = fill_plural(
            "Delete {count} conversation forever? Gmail cannot bring it back.",
            "Delete {count} conversations forever? Gmail cannot bring them back.",
            count,
            &[("count", &count.to_string())],
        );
        Ok(Plan::ask(question, self.erase(account, targets)))
    }

    async fn erase(&self, account: Account, targets: Vec<Target>) -> ToolResult {
        let mail = Arc::clone(&self.modules.mail);
        let outcome = self
            .permitted(&account, Permission::Delete, async move {
                mail.erase(&targets).await
            })
            .await?;
        self.effects.relist();
        if let (true, Some(error)) = (outcome.done.is_empty(), outcome.first_error()) {
            return Err(error.to_string());
        }
        let mut result = json!({
            "deleted": outcome.done.len(),
            "undo": "Deleted mail cannot be brought back.",
        });
        if !outcome.failed.is_empty() {
            result["failed"] = outcome
                .failed
                .iter()
                .map(|f| json!({"thread_id": f.target.thread_id, "error": f.error}))
                .collect();
        }
        Ok(result)
    }

    pub(super) async fn send_later<'a>(&'a self, input: &'a Value) -> Result<Plan<'a>, String> {
        let at = future_instant(&required(input, "at")?)?;
        let draft = match input.get("draft").filter(|v| v.is_object()) {
            Some(saved) => self.saved_draft(saved).await?,
            None => self.draft_from(input).await?,
        };
        if let Some(problem) = draft.problem() {
            return Err(problem);
        }
        let when = crate::format::future_date(at, Local::now());
        let question = fill(
            &gettext("Send “{subject}” to {recipients} {when}?"),
            &[
                ("subject", &draft.subject),
                ("recipients", &compose::format_recipients(&draft.to)),
                ("when", &when),
            ],
        );
        Ok(Plan::ask(question, async move {
            self.effects.send_later(draft, at)?;
            Ok(json!({
                "scheduled": local_text(at),
                "where": "It waits in the Send Later mailbox, where the user can change or cancel it.",
            }))
        }))
    }

    /// The draft a conversation holds, ready to send as it stands.
    async fn saved_draft(&self, saved: &Value) -> Result<Draft, String> {
        let (account, sync) = self.sync_for(&required(saved, "account")?)?;
        let thread_id = required(saved, "thread_id")?;
        let key = thread_id.clone();
        let found = self
            .read(move |c| messages::thread_messages(c, account.id, &key))
            .await?;
        let message = found
            .iter()
            .rev()
            .find(|m| m.has_label(system_label::DRAFT))
            .cloned()
            .ok_or("That conversation holds no draft.")?;
        // Sending rebuilds the message from the draft as Gmail holds it,
        // which is the only copy of its Bcc, its reply headers and its files.
        let (s, id) = (Arc::clone(&sync), message.id.clone());
        let raw = self.call(async move { s.raw_message(&id).await }).await?;
        // An encrypted draft opens only with the writer's passphrase, so
        // the writer sends that one from the composer.
        if crate::protection::draft::standard_of(&raw).is_some() {
            return Err("That draft is encrypted, and the assistant cannot open it. Ask the user to open it and choose Send Later in the composer.".into());
        }
        let id = message.id.clone();
        let draft_id = self
            .call(async move { sync.draft_id_for(&id).await })
            .await?
            .ok_or("Gmail no longer holds that draft.")?;
        let mut draft = self.effects.new_draft(account.id)?;
        crate::protection::draft::reopen_plain(&raw, &mut draft);
        draft.thread_id = (found.len() > 1).then_some(thread_id);
        draft.draft_id = Some(draft_id);
        Ok(draft)
    }

    pub(super) async fn list_templates(&self) -> ToolResult {
        let all = self.read(templates::list).await?;
        Ok(json!({
            "templates": all.iter().map(|t| json!({
                "name": t.name,
                "subject": t.subject,
                "body": t.markdown,
            })).collect::<Vec<_>>(),
        }))
    }

    pub(super) async fn insert_template(&self, input: &Value) -> ToolResult {
        let name = required(input, "template")?;
        let all = self.read(templates::list).await?;
        let template = all
            .into_iter()
            .find(|t| t.name.trim().eq_ignore_ascii_case(&name))
            .ok_or_else(|| format!("There is no template called {name}."))?;
        let mut draft = self.draft_from(input).await?;
        if draft.subject.is_empty() {
            draft.subject = template.subject.clone();
        }
        let filling = |subject: &str| Filling {
            recipient: draft.to.first().cloned(),
            subject: subject.to_string(),
            date: today(Local::now()),
        };
        // The subject fills in first, so `{{subject}}` in the body reads
        // the subject the message goes out with.
        let subject = expand(&draft.subject, &filling(&draft.subject));
        let markdown = expand(&template.markdown, &filling(&subject));
        draft.subject = subject;
        draft.markdown = markdown;
        let (subject, body) = (draft.subject.clone(), draft.markdown.clone());
        self.effects.compose(draft)?;
        Ok(json!({
            "opened": "A composer window shows the message for the user to review.",
            "subject": subject,
            "body": body,
        }))
    }

    pub(super) async fn unsubscribe<'a>(&'a self, input: &'a Value) -> Result<Plan<'a>, String> {
        let (account, sync) = self.sync_for(&required(input, "account")?)?;
        let thread_id = required(input, "thread_id")?;
        let (s, t) = (Arc::clone(&sync), thread_id.clone());
        if let Err(err) = self.call(async move { s.ensure_thread(&t).await }).await {
            tracing::info!(error = %err, "reading the stored copy of the thread");
        }
        let key = thread_id.clone();
        let metas = self
            .read(move |c| messages::thread_messages(c, account.id, &key))
            .await?;
        if metas.is_empty() {
            return Err("That conversation was not found.".into());
        }
        let mut found = Vec::new();
        for meta in metas {
            let (s, id) = (Arc::clone(&sync), meta.id.clone());
            let body = self.call(async move { s.body(&id).await }).await.ok();
            found.push((meta, body));
        }
        let (meta, body) =
            newest_with_list(&found).ok_or("That conversation has no unsubscribe link.")?;
        let how = body
            .list_unsubscribe
            .as_deref()
            .and_then(|header| choose(header, body.one_click_unsubscribe))
            .ok_or("That conversation's unsubscribe link is not one Penguin Mail can use.")?;
        let sender = meta
            .from
            .as_ref()
            .map(|a| a.display().to_string())
            .unwrap_or_else(|| gettext("this list"));
        let (kind, way) = match &how {
            Unsubscribe::OneClick(_) => (
                "one_click",
                gettext("Penguin Mail asks the sender to take you off the list."),
            ),
            Unsubscribe::Email { .. } => (
                "email",
                gettext("Penguin Mail sends the list an unsubscribe request from your account."),
            ),
            Unsubscribe::Page(_) => (
                "page",
                gettext("The sender's unsubscribe page opens in your browser."),
            ),
        };
        let question = fill(
            &gettext("Unsubscribe from {sender}?"),
            &[("sender", &sender)],
        );
        Ok(Plan::ask(format!("{question}\n\n{way}"), async move {
            self.effects.unsubscribe(account.id, how).await?;
            let mut result = json!({"unsubscribed": sender, "how": kind});
            if kind == "page" {
                result["note"] = json!(
                    "The sender's unsubscribe page opened in the user's browser. They finish there."
                );
            }
            Ok(result)
        }))
    }

    pub(super) async fn read_attachment(&self, input: &Value) -> ToolResult {
        let (_, sync) = self.sync_for(&required(input, "account")?)?;
        let message_id = required(input, "message_id")?;
        let wanted = required(input, "attachment")?;
        let (s, id) = (Arc::clone(&sync), message_id.clone());
        let body = self.call(async move { s.body(&id).await }).await?;
        let files = &body.attachments;
        let found = files
            .iter()
            .find(|a| a.filename.eq_ignore_ascii_case(&wanted))
            .or_else(|| {
                let number: usize = wanted.parse().ok()?;
                files.get(number.checked_sub(1)?)
            });
        let Some(file) = found else {
            let names: Vec<&str> = files.iter().map(|a| a.filename.as_str()).collect();
            return Err(match names.is_empty() {
                true => "That message has no attachments.".into(),
                false => format!(
                    "That message has no attachment called {wanted}. It has: {}.",
                    names.join(", ")
                ),
            });
        };
        let handle = file
            .attachment_id
            .clone()
            .ok_or("Gmail gives no way to fetch that attachment.")?;
        let bytes = self
            .call(async move { sync.attachment(&message_id, &handle).await })
            .await?;
        let mut result = json!({
            "name": file.filename,
            "type": file.mime_type,
            "size": file.size,
        });
        let text = match kind_of(&file.mime_type, &file.filename) {
            Kind::Text => Some(String::from_utf8_lossy(&bytes).into_owned()),
            Kind::Html => Some(mailrs_gmail::html_to_text(&String::from_utf8_lossy(&bytes))),
            Kind::Pdf => match self.away(pdf_text(bytes)).await?? {
                Some(text) => Some(text),
                None => {
                    result["note"] = json!(
                        "This computer has no pdftotext, so the PDF cannot be read. Installing poppler-utils adds it."
                    );
                    None
                }
            },
            Kind::Other => {
                result["note"] = json!(format!(
                    "Penguin Mail cannot read {} files as text. The user can open it from the conversation.",
                    file.mime_type
                ));
                None
            }
        };
        if let Some(text) = text {
            let mut text = text.trim().to_string();
            if text.chars().count() > MOST_ATTACHMENT_CHARS {
                text =
                    text.chars().take(MOST_ATTACHMENT_CHARS).collect::<String>() + "\n[cut short]";
            }
            result["text"] = json!(text);
        }
        Ok(result)
    }
}
