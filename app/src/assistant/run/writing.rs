//! The writing tools: drafting and sending a message, with a blind copy,
//! files, a forwarded message, a signature or encryption; and the drafts
//! Gmail keeps, listed, changed and deleted. A message takes the paths the
//! composer's takes: `compose` builds it, `protection` decides how it is
//! signed or encrypted, and the window sends it or saves it.
//!
//! A file on this computer goes into a message only once the user has
//! approved a question that names it. Until then the tool knows where the
//! file is and nothing of what it holds.

use std::cmp::Reverse;
use std::path::{Path, PathBuf};

use mailrs_domain::{Address, Attachment, MessageBody, MessageMeta};

use super::mail::named_attachment;
use super::*;
use crate::compose::{OutgoingAttachment, ReplyKind};
use crate::protection::{self, Addressees, Engine};

/// The largest file from this computer a message takes. Gmail refuses a
/// message over 25 MB, whatever it carries.
const MOST_FILE_BYTES: u64 = 25 * 1024 * 1024;

/// The most drafts `list_drafts` gives back.
const MOST_DRAFTS: usize = 100;

/// A file on this computer that a message will carry once the user says
/// yes: its place, checked, and nothing read yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LocalFile {
    pub path: PathBuf,
}

/// A message a writing tool put together, and the files on this computer
/// it still has to read in.
struct Written {
    draft: Draft,
    local: Vec<LocalFile>,
}

/// The changes `edit_draft` makes, gathered before the question so the
/// question can name them.
struct Edits {
    to: Option<Vec<Address>>,
    cc: Option<Vec<Address>>,
    bcc: Option<Vec<Address>>,
    subject: Option<String>,
    body: Option<String>,
    remove: Vec<String>,
    added: Vec<OutgoingAttachment>,
    local: Vec<LocalFile>,
    sign: Option<bool>,
    encrypt: Option<bool>,
}

/// The file a tool names at `given`, once it is a file a message may
/// carry: an absolute path, or one under `~/`, to a readable file of at
/// most 25 MB that [`off_limits`] allows. `home` is the user's home folder
/// and `private` the folders Penguin Mail keeps its own data in.
pub(super) fn local_file(
    given: &str,
    home: &Path,
    private: &[PathBuf],
) -> Result<LocalFile, String> {
    let expanded = match given.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => PathBuf::from(given),
    };
    if !expanded.is_absolute() {
        return Err(format!(
            "Give the whole path of {given}, starting with / or ~/."
        ));
    }
    // Every link resolves first, so a link that leads into ~/.ssh counts
    // as ~/.ssh.
    let path =
        std::fs::canonicalize(&expanded).map_err(|_| format!("There is no file at {given}."))?;
    if off_limits(&path, home, private) {
        return Err(format!(
            "{given} is in a folder that holds keys, passwords or settings, so the assistant will not attach it. The user can attach it in the composer."
        ));
    }
    let meta = std::fs::metadata(&path).map_err(|err| format!("Could not read {given}: {err}"))?;
    if !meta.is_file() {
        return Err(format!("{given} is not a file."));
    }
    if meta.len() > MOST_FILE_BYTES {
        return Err(format!(
            "{given} is larger than 25 MB, more than Gmail takes in one message."
        ));
    }
    Ok(LocalFile { path })
}

/// Whether the assistant must leave the file at `path` alone. It attaches
/// what sits in the home folder, outside any hidden folder, and what sits
/// in the temporary and removable-media folders. Hidden folders such as
/// `~/.ssh` and `~/.gnupg` hold keys, passwords and settings, and the rest
/// of the system holds the computer's own; the folders in `private` hold
/// Penguin Mail's mail store, settings and tokens wherever they live.
pub(super) fn off_limits(path: &Path, home: &Path, private: &[PathBuf]) -> bool {
    if private.iter().any(|dir| path.starts_with(dir)) {
        return true;
    }
    let hidden = |inside: &Path| {
        inside
            .components()
            .any(|part| part.as_os_str().to_string_lossy().starts_with('.'))
    };
    if let Ok(inside) = path.strip_prefix(home) {
        return hidden(inside);
    }
    let open = ["/tmp", "/media", "/mnt", "/run/media"];
    match open.iter().find_map(|root| path.strip_prefix(root).ok()) {
        Some(inside) => hidden(inside),
        None => true,
    }
}

/// The folders Penguin Mail keeps its own data in: the mail store, the
/// settings and the cache, each as the system resolves it.
fn private_dirs() -> Vec<PathBuf> {
    let name = mailrs_sync::config::DIR_NAME;
    let mut dirs = vec![
        gtk::glib::user_config_dir().join(name),
        gtk::glib::user_data_dir().join(name),
        gtk::glib::user_cache_dir().join(name),
    ];
    dirs.extend(mailrs_sync::config::data_dir().ok());
    dirs.extend(
        mailrs_sync::config::config_path()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf)),
    );
    let resolved: Vec<PathBuf> = dirs
        .iter()
        .filter_map(|dir| std::fs::canonicalize(dir).ok())
        .collect();
    dirs.extend(resolved);
    dirs
}

/// Whether what Gmail holds of this message is ciphertext, or a signed
/// blob that hides its text. The assistant never sends such a message on:
/// only the person who can open it knows what it says.
fn ciphertext(body: &MessageBody) -> bool {
    matches!(
        protection::engine(body),
        Some(Engine::Pgp(crate::pgp::Opening::Decrypt))
            | Some(Engine::Smime(
                crate::smime::Opening::Decrypt | crate::smime::Opening::Opaque
            ))
    ) || body
        .text
        .as_deref()
        .is_some_and(|text| text.contains("-----BEGIN PGP MESSAGE-----"))
}

/// Every address a message goes to, lower case and each once, as the
/// composer asks the engines about them.
fn recipients(draft: &Draft) -> Vec<String> {
    let mut found: Vec<String> = draft
        .to
        .iter()
        .chain(&draft.cc)
        .chain(&draft.bcc)
        .map(|a| a.email.trim().to_lowercase())
        .filter(|email| compose::is_address(email))
        .collect();
    found.sort();
    found.dedup();
    found
}

/// Whether the engines hold a key or certificate of the sender's own.
fn holds_own(held: &Held) -> bool {
    held.pgp
        .as_deref()
        .is_some_and(|all| all.iter().any(|r| r.key.is_some()))
        || held
            .smime
            .as_deref()
            .is_some_and(|all| all.iter().any(|r| r.certificate.is_some()))
}

/// People as a tool writes them back: `Name <address>`.
fn people(list: &[Address]) -> Vec<String> {
    list.iter()
        .map(
            |a| match a.name.as_deref().filter(|n| !n.trim().is_empty()) {
                Some(name) => format!("{name} <{}>", a.email),
                None => a.email.clone(),
            },
        )
        .collect()
}

/// A list of addresses a tool was given, or `None` when the key is absent.
fn addresses(input: &Value, key: &str) -> Option<Vec<Address>> {
    let items = input.get(key)?.as_array()?;
    let joined = items
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    Some(compose::parse_recipients(&joined))
}

/// The names of the files a message carries, leaving out the images its
/// text shows.
fn file_names(draft: &Draft) -> Vec<String> {
    draft
        .attachments
        .iter()
        .filter(|a| a.content_id.is_none())
        .map(|a| a.filename.clone())
        .collect()
}

/// A subject for a question, or words that say there is none.
fn subject_of(subject: &str) -> String {
    match subject.trim() {
        "" => gettext("(no subject)"),
        subject => subject.to_string(),
    }
}

/// How a message goes out, for a question, when it is signed or encrypted.
fn protected_line(draft: &Draft) -> Option<String> {
    let standard = match draft.standard {
        Standard::Pgp => "OpenPGP",
        Standard::Smime => "S/MIME",
    };
    let said = match (draft.sign, draft.encrypt) {
        (false, false) => return None,
        (true, false) => gettext("Signed with {standard}"),
        (false, true) => gettext("Encrypted with {standard}"),
        (true, true) => gettext("Signed and encrypted with {standard}"),
    };
    Some(fill(&said, &[("standard", standard)]))
}

/// The lines under a question about a message that say what else it
/// carries: who gets a blind copy, what it forwards, its files, the files
/// it reads from this computer, and how it is protected.
fn extras(draft: &Draft, local: &[LocalFile]) -> Vec<String> {
    let mut lines = Vec::new();
    if !draft.bcc.is_empty() {
        lines.push(fill(
            &gettext("Blind copy to {recipients}"),
            &[("recipients", &compose::format_recipients(&draft.bcc))],
        ));
    }
    if let Some(forwarded) = &draft.forwarded {
        lines.push(fill(
            &gettext("Forwards “{subject}” from {sender}"),
            &[
                ("subject", &subject_of(&forwarded.subject)),
                ("sender", &forwarded.from),
            ],
        ));
    }
    let names = file_names(draft);
    if !names.is_empty() {
        lines.push(fill(
            &gettext("Attached: {files}"),
            &[("files", &names.join(", "))],
        ));
    }
    if !local.is_empty() {
        lines.push(from_this_computer(local));
    }
    lines.extend(protected_line(draft));
    lines
}

/// The line that names the files a message reads from this computer, by
/// their whole paths, so the user sees which file goes out.
fn from_this_computer(local: &[LocalFile]) -> String {
    let paths: Vec<String> = local.iter().map(|f| f.path.display().to_string()).collect();
    fill(
        &gettext("From this computer: {files}"),
        &[("files", &paths.join(", "))],
    )
}

/// A question with its lines under it.
fn with_lines(question: String, lines: &[String]) -> String {
    match lines.is_empty() {
        true => question,
        false => format!("{question}\n\n{}", lines.join("\n")),
    }
}

impl<A: Accounts> Tools<A> {
    // ---- Composing and sending ---------------------------------------------

    /// Opens a composer on the message for the user to review. It asks
    /// first only when the message reads a file from this computer.
    pub(super) async fn draft<'a>(&'a self, input: &'a Value) -> Result<Plan<'a>, String> {
        let Written { mut draft, local } = self.written(input).await?;
        // The composer checks the keys again and says what stops it, so a
        // message that cannot be encrypted still opens, with the reason.
        let note = match self.protect(&mut draft, input).await {
            Ok(()) => None,
            Err(problem) => {
                draft.sign |= flag(input, "sign") == Some(true);
                draft.encrypt |= flag(input, "encrypt") == Some(true);
                Some(problem)
            }
        };
        let question = (!local.is_empty()).then(|| {
            let question = fill_plural(
                "Put a file from this computer in a draft?",
                "Put {count} files from this computer in a draft?",
                local.len(),
                &[("count", &local.len().to_string())],
            );
            with_lines(question, &[from_this_computer(&local)])
        });
        let change = async move {
            let mut draft = draft;
            draft.attachments.extend(self.read_local(&local).await?);
            self.effects.compose(draft)?;
            let mut result =
                json!({"opened": "A composer window shows the draft for the user to review."});
            if let Some(note) = note {
                result["note"] = json!(note);
            }
            Ok(result)
        };
        Ok(match question {
            Some(question) => Plan::ask(question, change),
            None => Plan::without_asking(change),
        })
    }

    pub(super) async fn send<'a>(&'a self, input: &'a Value) -> Result<Plan<'a>, String> {
        let Written { mut draft, local } = self.written(input).await?;
        if let Some(problem) = draft.problem() {
            return Err(problem);
        }
        self.protect(&mut draft, input).await?;
        let to = compose::format_recipients(&draft.to);
        let question = fill(
            &gettext("Send “{subject}” to {recipients}?"),
            &[("subject", &draft.subject), ("recipients", &to)],
        );
        let question = with_lines(question, &extras(&draft, &local));
        Ok(Plan::ask(question, async move {
            let mut draft = draft;
            draft.attachments.extend(self.read_local(&local).await?);
            let delay = self.desk.settings().undo_send.seconds();
            let (sign, encrypt) = (draft.sign, draft.encrypt);
            self.effects.send(draft)?;
            let mut result = json!({"sent": true, "undo_seconds": delay});
            if encrypt {
                result["encrypted"] = json!(true);
            }
            if sign {
                result["signed"] = json!(true);
            }
            Ok(result)
        }))
    }

    /// The message the fields `message_fields` describes, signed or
    /// encrypted as asked, for the tools that hand it on without a question
    /// of their own about its files. Files from this computer need the
    /// question `draft_email` and `send_email` ask, so they are refused
    /// here.
    pub(super) async fn draft_from(&self, input: &Value) -> Result<Draft, String> {
        let Written { mut draft, local } = self.written(input).await?;
        if !local.is_empty() {
            return Err(
                "Only draft_email and send_email attach files from this computer, since they show the user which files first."
                    .into(),
            );
        }
        self.protect(&mut draft, input).await?;
        Ok(draft)
    }

    /// The message `message_fields` describes: its people and words,
    /// threaded into the conversation `reply_to` names, carrying the
    /// message `forward` names and the files `attachments` names. Files
    /// from this computer are checked here and read once the user agrees.
    async fn written(&self, input: &Value) -> Result<Written, String> {
        let reply = input.get("reply_to").filter(|v| v.is_object());
        let forward = input.get("forward").filter(|v| v.is_object());
        if reply.is_some() && forward.is_some() {
            return Err("Give reply_to or forward, not both.".into());
        }
        let reply_account = match reply {
            Some(r) => Some(self.account_named(&required(r, "account")?)?),
            None => None,
        };
        let account = match text(input, "account") {
            Some(email) => self.account_named(&email)?,
            None => match &reply_account {
                Some(a) => a.clone(),
                None => {
                    let id = self.desk.default_account().ok_or("Add an account first.")?;
                    self.desk
                        .accounts()
                        .into_iter()
                        .find(|a| a.id == id)
                        .ok_or("Add an account first.")?
                }
            },
        };
        let mut draft = self.effects.new_draft(account.id)?;
        draft.to = addresses(input, "to").unwrap_or_default();
        draft.cc = addresses(input, "cc").unwrap_or_default();
        draft.bcc = addresses(input, "bcc").unwrap_or_default();
        draft.subject = text(input, "subject").unwrap_or_default();
        draft.markdown = input
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if let (Some(r), Some(reply_account)) = (reply, &reply_account)
            && reply_account.id == account.id
        {
            let thread_id = required(r, "thread_id")?;
            let key = thread_id.clone();
            let found = self
                .read(move |c| messages::thread_messages(c, account.id, &key))
                .await?;
            let parent = found
                .iter()
                .rev()
                .find(|m| !m.in_role(Role::Drafts));
            draft.thread_id = Some(thread_id);
            draft.in_reply_to = parent.and_then(|m| m.rfc822_msgid.clone());
            draft.references = found
                .iter()
                .filter_map(|m| m.rfc822_msgid.clone())
                .collect();
            if draft.subject.is_empty()
                && let Some(parent) = parent
            {
                draft.subject = if parent.subject.to_lowercase().starts_with("re:") {
                    parent.subject.clone()
                } else {
                    format!("Re: {}", parent.subject)
                };
            }
        }
        if let Some(forward) = forward {
            self.forward_into(&mut draft, forward).await?;
        }
        let (files, local) = self.gathered(account.id, input).await?;
        draft.attachments.extend(files);
        Ok(Written { draft, local })
    }

    /// Puts the message `forward` names into `draft`, as the conversation's
    /// Forward button does: its text and HTML under a forwarded header, and
    /// its files.
    async fn forward_into(&self, draft: &mut Draft, forward: &Value) -> Result<(), String> {
        let (account, sync) = self.sync_for(&required(forward, "account")?)?;
        let message_id = required(forward, "message_id")?;
        let key = vec![message_id.clone()];
        let original = self
            .read(move |c| messages::by_ids(c, account.id, &key))
            .await?
            .into_iter()
            .next()
            .ok_or("That message is not stored. Read its conversation first, then forward it by the message_id read_conversation gave.")?;
        let body = self.readable_body(&sync, &message_id).await?;
        let (text, html) = (compose::body_text(&body), body.html.clone());
        let made = compose::respond(
            ReplyKind::Forward,
            draft.account_id,
            std::slice::from_ref(&draft.from),
            &original,
            &text,
            html.as_deref(),
            &[],
        );
        if draft.subject.is_empty() {
            draft.subject = made.subject;
        }
        draft.forwarded = made.forwarded;
        for found in body.attachments {
            let data = self.fetched(&sync, &message_id, &found).await?;
            draft
                .attachments
                .push(compose::forwarded_file(found, data, html.as_deref()));
        }
        Ok(())
    }

    /// A message's body, unless it is one [`ciphertext`] keeps the assistant
    /// from sending on.
    async fn readable_body(
        &self,
        sync: &Arc<AccountSync>,
        message_id: &str,
    ) -> Result<MessageBody, String> {
        let (s, id) = (Arc::clone(sync), message_id.to_string());
        let body = self.call(async move { s.body(&id).await }).await?;
        if ciphertext(&body) {
            return Err("That message arrived encrypted, and the assistant does not send it on. The user can forward it, or save its files, from the conversation.".into());
        }
        Ok(body)
    }

    /// The bytes of one attachment, from Gmail.
    async fn fetched(
        &self,
        sync: &Arc<AccountSync>,
        message_id: &str,
        found: &Attachment,
    ) -> Result<Vec<u8>, String> {
        let handle = found.attachment_id.clone().ok_or_else(|| {
            format!(
                "Gmail gives no way to fetch {}, so it cannot go with the message.",
                found.filename
            )
        })?;
        let (s, id) = (Arc::clone(sync), message_id.to_string());
        self.call(async move { s.attachment(&id, &handle).await })
            .await
            .map_err(|err| format!("Could not fetch {}: {err}", found.filename))
    }

    /// The files `attachments` names: those out of messages fetched now,
    /// and those on this computer checked and left for later. A message's
    /// file comes from `account_id`'s mail unless the item names another
    /// account.
    async fn gathered(
        &self,
        account_id: AccountId,
        input: &Value,
    ) -> Result<(Vec<OutgoingAttachment>, Vec<LocalFile>), String> {
        let Some(items) = input.get("attachments").and_then(Value::as_array) else {
            return Ok((Vec::new(), Vec::new()));
        };
        let (mut files, mut local) = (Vec::new(), Vec::new());
        let mut private = None;
        for item in items {
            if let Some(path) = text(item, "path") {
                let private = private.get_or_insert_with(private_dirs);
                local.push(local_file(&path, &gtk::glib::home_dir(), private)?);
                continue;
            }
            let email = text(item, "account").unwrap_or_else(|| self.email_of(account_id));
            let (_, sync) = self.sync_for(&email)?;
            let message_id = required(item, "message_id")
                .map_err(|_| "Each attachment needs a message_id and attachment, or a path.")?;
            let wanted = required(item, "attachment")?;
            let body = self.readable_body(&sync, &message_id).await?;
            let found = named_attachment(&body.attachments, &wanted)?.clone();
            let data = self.fetched(&sync, &message_id, &found).await?;
            files.push(OutgoingAttachment {
                filename: found.filename,
                mime_type: found.mime_type,
                data,
                content_id: None,
            });
        }
        Ok((files, local))
    }

    /// Reads the files on this computer a message carries, once the user
    /// has agreed to them. The type comes from the name and the first
    /// bytes, as the composer's Attach does.
    async fn read_local(&self, local: &[LocalFile]) -> Result<Vec<OutgoingAttachment>, String> {
        let mut files = Vec::new();
        for file in local {
            let path = file.path.clone();
            let data = self
                .call(async move { tokio::fs::read(&path).await })
                .await
                .map_err(|err| format!("Could not read {}: {err}", file.path.display()))?;
            let filename = file
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "attachment".into());
            let (guess, _) = gtk::gio::content_type_guess(Some(&filename), &data[..]);
            let mime_type = gtk::gio::content_type_get_mime_type(&guess)
                .map(|m| m.to_string())
                .unwrap_or_else(|| "application/octet-stream".into());
            files.push(OutgoingAttachment {
                filename,
                mime_type,
                data,
                content_id: None,
            });
        }
        Ok(files)
    }

    /// Signs or encrypts `draft` as the call asks, and where it says
    /// nothing, as the user's settings ask the composer to. Encrypting
    /// needs a key or certificate for every recipient, and the error names
    /// whoever lacks one. A setting that cannot be met leaves the message
    /// as it is, the way the composer leaves its toggle off.
    async fn protect(&self, draft: &mut Draft, input: &Value) -> Result<(), String> {
        let settings = self.desk.settings();
        let (asked_sign, asked_encrypt) = (flag(input, "sign"), flag(input, "encrypt"));
        let sign = asked_sign.unwrap_or(settings.sign_by_default);
        let encrypt = asked_encrypt.unwrap_or(settings.encrypt_when_possible);
        if encrypt {
            let held = self.effects.keys(recipients(draft)).await;
            let blind = Addressees::of(draft).has_blind_copy();
            match protection::encrypting(&held, blind) {
                Ok(standard) => {
                    draft.encrypt = true;
                    draft.standard = standard;
                }
                Err(problem) if asked_encrypt == Some(true) => {
                    return Err(format!("The message cannot be encrypted: {problem}"));
                }
                Err(_) => {}
            }
        }
        if sign {
            let from = draft.from.email.trim().to_lowercase();
            let own = self.effects.keys(vec![from.clone()]).await;
            if holds_own(&own) {
                draft.sign = true;
                // A signature on an encrypted message goes inside it, so
                // the recipients' standard carries both.
                if !draft.encrypt {
                    draft.standard = self.effects.signing_standard(from).await;
                }
            } else if asked_sign == Some(true) {
                return Err(format!(
                    "The message cannot be signed: this computer holds no key or certificate for {from}."
                ));
            }
        }
        Ok(())
    }

    // ---- Drafts ------------------------------------------------------------

    /// The drafts the store holds, newest first, across the accounts or in
    /// the one the call names.
    pub(super) async fn list_drafts(&self, input: &Value) -> ToolResult {
        let accounts = match text(input, "account") {
            Some(email) => vec![self.account_named(&email)?],
            None => self.desk.accounts(),
        };
        let mut found: Vec<(String, MessageMeta)> = Vec::new();
        for account in accounts {
            let id = account.id;
            let drafts = self
                .read(move |c| {
                    let ids: Vec<String> = messages::held_by(c, id, &MailSet::Role(Role::Drafts))?
                        .into_iter()
                        .collect();
                    messages::by_ids(c, id, &ids)
                })
                .await?;
            found.extend(drafts.into_iter().map(|m| (account.email.clone(), m)));
        }
        found.sort_by_key(|(_, m)| Reverse(m.date));
        found.truncate(MOST_DRAFTS);
        Ok(json!({
            "count": found.len(),
            "drafts": found.iter().map(|(account, m)| json!({
                "account": account,
                "message_id": m.id,
                "thread_id": m.thread_id,
                "subject": m.subject,
                "to": people(&m.to),
                "cc": people(&m.cc),
                "date": crate::format::local(m.date).map(|d| d.format("%Y-%m-%d %H:%M").to_string()),
            })).collect::<Vec<_>>(),
        }))
    }

    /// The stored draft `message_id` names in the account.
    async fn stored_draft(
        &self,
        account_id: AccountId,
        message_id: &str,
    ) -> Result<MessageMeta, String> {
        let key = vec![message_id.to_string()];
        self.read(move |c| messages::by_ids(c, account_id, &key))
            .await?
            .into_iter()
            .find(|m| m.in_role(Role::Drafts))
            .ok_or_else(|| "There is no draft with that message_id. list_drafts gives the drafts and their ids.".into())
    }

    /// Changes a draft Gmail keeps and saves it back, as reopening it in the
    /// composer and pressing Save Draft would. An encrypted draft opens
    /// through its engine and goes back encrypted to the writer's own key.
    /// What changes is known before the question; the draft itself opens
    /// only after the user agrees, since opening an encrypted one may ask
    /// for a passphrase.
    pub(super) async fn edit_draft<'a>(&'a self, input: &'a Value) -> Result<Plan<'a>, String> {
        let (account, sync) = self.sync_for(&required(input, "account")?)?;
        let message_id = required(input, "message_id")?;
        let stored = self.stored_draft(account.id, &message_id).await?;
        let (added, local) = self.gathered(account.id, input).await?;
        let edits = Edits {
            to: addresses(input, "to"),
            cc: addresses(input, "cc"),
            bcc: addresses(input, "bcc"),
            subject: input
                .get("subject")
                .and_then(Value::as_str)
                .map(|s| s.trim().to_string()),
            body: input
                .get("body")
                .and_then(Value::as_str)
                .map(str::to_string),
            remove: input
                .get("remove_attachments")
                .and_then(Value::as_array)
                .map(|all| {
                    all.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            added,
            local,
            sign: flag(input, "sign"),
            encrypt: flag(input, "encrypt"),
        };
        let lines = edit_lines(&edits);
        if lines.is_empty() {
            return Err("Name at least one change: to, cc, bcc, subject, body, attachments, remove_attachments, sign or encrypt.".into());
        }
        let question = with_lines(
            fill(
                &gettext("Change the draft “{subject}”?"),
                &[("subject", &subject_of(&stored.subject))],
            ),
            &lines,
        );
        Ok(Plan::ask(question, async move {
            let key = stored.thread_id.clone();
            let thread = self
                .read(move |c| messages::thread_messages(c, account.id, &key))
                .await?;
            let (s, id) = (Arc::clone(&sync), message_id.clone());
            let draft_id = self
                .call(async move { s.draft_id_for(&id).await })
                .await?
                .ok_or("Gmail no longer holds that draft.")?;
            // Only the message as Gmail holds it carries the Bcc, the reply
            // headers and the bytes of the files, readable or encrypted.
            let (s, id) = (Arc::clone(&sync), message_id.clone());
            let raw = self.call(async move { s.raw_message(&id).await }).await?;
            let blank = self.effects.new_draft(account.id)?;
            let mut draft = self.effects.reopen_draft(raw, blank).await?;
            draft.draft_id = Some(draft_id);
            draft.thread_id = (thread.len() > 1).then_some(stored.thread_id.clone());
            let missing = self.apply(&mut draft, edits).await?;
            self.effects.save_draft(draft.clone()).await?;
            let mut result = json!({
                "saved": true,
                "subject": draft.subject,
                "to": people(&draft.to),
                "cc": people(&draft.cc),
                "bcc": people(&draft.bcc),
                "attachments": file_names(&draft),
                "encrypted": draft.encrypt,
                "signed": draft.sign,
            });
            if !missing.is_empty() {
                result["not_found"] = json!(format!(
                    "The draft had no file called {}, so nothing was removed for that name.",
                    missing.join(", ")
                ));
            }
            Ok(result)
        }))
    }

    /// Lays `edits` over a reopened draft. Gives back the names of files
    /// to remove that the draft did not carry.
    async fn apply(&self, draft: &mut Draft, edits: Edits) -> Result<Vec<String>, String> {
        if let Some(to) = edits.to {
            draft.to = to;
        }
        if let Some(cc) = edits.cc {
            draft.cc = cc;
        }
        if let Some(bcc) = edits.bcc {
            draft.bcc = bcc;
        }
        if let Some(subject) = edits.subject {
            draft.subject = subject;
        }
        if let Some(body) = edits.body {
            // The styled body decides what goes out when there is one, so
            // new words in Markdown replace it.
            draft.markdown = body;
            draft.rich = None;
        }
        let missing: Vec<String> = edits
            .remove
            .iter()
            .filter(|name| {
                !draft
                    .attachments
                    .iter()
                    .any(|a| a.filename.eq_ignore_ascii_case(name))
            })
            .cloned()
            .collect();
        draft.attachments.retain(|a| {
            !edits
                .remove
                .iter()
                .any(|name| a.filename.eq_ignore_ascii_case(name))
        });
        draft.attachments.extend(edits.added);
        draft
            .attachments
            .extend(self.read_local(&edits.local).await?);
        if let Some(sign) = edits.sign {
            draft.sign = sign;
        }
        if let Some(encrypt) = edits.encrypt {
            draft.encrypt = encrypt;
        }
        // The draft waits encrypted to the writer's own key. The standard
        // the recipients can read decides which engine seals it, when one
        // reaches them all; otherwise the draft keeps the one it had.
        if draft.encrypt {
            let held = self.effects.keys(recipients(draft)).await;
            if let Ok(standard) =
                protection::encrypting(&held, Addressees::of(draft).has_blind_copy())
            {
                draft.standard = standard;
            }
        }
        Ok(missing)
    }

    /// Deletes a draft from Gmail for good, after asking.
    pub(super) async fn delete_draft<'a>(&'a self, input: &'a Value) -> Result<Plan<'a>, String> {
        let (account, sync) = self.sync_for(&required(input, "account")?)?;
        let message_id = required(input, "message_id")?;
        let stored = self.stored_draft(account.id, &message_id).await?;
        // A scheduled message holds its Gmail draft. Deleting the draft
        // here would leave the outbox row pointing at nothing.
        let waiting = Target {
            account_id: account.id,
            thread_id: stored.thread_id.clone(),
            message_id: Some(message_id.clone()),
        };
        let outbox = self.outbox();
        if !self
            .call(async move { outbox.named(&[waiting]).await })
            .await?
            .is_empty()
        {
            return Err("That draft is waiting to go out. cancel_send stops it first.".into());
        }
        let question = fill(
            &gettext("Delete the draft “{subject}”? Gmail cannot bring it back."),
            &[("subject", &subject_of(&stored.subject))],
        );
        Ok(Plan::ask(question, async move {
            let gone = self
                .call(async move { sync.discard_draft(&message_id).await })
                .await?;
            if !gone {
                return Err("Gmail no longer holds that draft.".into());
            }
            self.effects.relist();
            Ok(json!({"deleted": true, "subject": stored.subject}))
        }))
    }
}

/// What `edit_draft`'s question says under its first line: one line per
/// change.
fn edit_lines(edits: &Edits) -> Vec<String> {
    let mut lines = Vec::new();
    let named = |said: &str, list: &[Address]| {
        let who = match list.is_empty() {
            true => gettext("nobody"),
            false => compose::format_recipients(list),
        };
        fill(said, &[("recipients", &who)])
    };
    if let Some(to) = &edits.to {
        lines.push(named(&gettext("To: {recipients}"), to));
    }
    if let Some(cc) = &edits.cc {
        lines.push(named(&gettext("Cc: {recipients}"), cc));
    }
    if let Some(bcc) = &edits.bcc {
        lines.push(named(&gettext("Bcc: {recipients}"), bcc));
    }
    if let Some(subject) = &edits.subject {
        lines.push(fill(
            &gettext("Subject: {subject}"),
            &[("subject", &subject_of(subject))],
        ));
    }
    if edits.body.is_some() {
        lines.push(gettext("New text in place of the old"));
    }
    if !edits.remove.is_empty() {
        lines.push(fill(
            &gettext("Takes out: {files}"),
            &[("files", &edits.remove.join(", "))],
        ));
    }
    let added: Vec<String> = edits.added.iter().map(|a| a.filename.clone()).collect();
    if !added.is_empty() {
        lines.push(fill(
            &gettext("Attached: {files}"),
            &[("files", &added.join(", "))],
        ));
    }
    if !edits.local.is_empty() {
        lines.push(from_this_computer(&edits.local));
    }
    match edits.encrypt {
        Some(true) => lines.push(gettext("Goes out encrypted")),
        Some(false) => lines.push(gettext("Goes out unencrypted")),
        None => {}
    }
    match edits.sign {
        Some(true) => lines.push(gettext("Goes out signed")),
        Some(false) => lines.push(gettext("Goes out unsigned")),
        None => {}
    }
    lines
}
