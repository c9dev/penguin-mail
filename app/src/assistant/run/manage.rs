//! The tools that look after what the user keeps rather than the mail
//! itself: labels, smart mailboxes, templates, Google contacts, the senders
//! whose remote images load, and files of exported mail. Each one works
//! through the module the window uses for the same thing, and every change
//! asks first.

use std::path::{Path, PathBuf};

use mailrs_gmail::{ContactFields, LabelColor};
use mailrs_store::address_book::{self, Contact};
use mailrs_store::image_senders::{self, ImageSender};
use mailrs_store::templates::{self, Template};
use mailrs_store::threads;
use mailrs_sync::export;

use super::*;
use crate::images;
use crate::ui::{LABEL_COLORS, label_color_name};

/// The colours `recolor_label` takes, in the order of Gmail's palette in
/// [`LABEL_COLORS`]. Their keys make the schema's enum.
pub(super) const COLOR_KEYS: [&str; 9] = [
    "red", "orange", "yellow", "green", "teal", "blue", "purple", "pink", "gray",
];

/// What `export_mail` writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    /// Conversations, one after another, as mail programs import them.
    Mbox,
    /// One message as Gmail holds it.
    Eml,
}

impl Format {
    fn extension(self) -> &'static str {
        match self {
            Format::Mbox => "mbox",
            Format::Eml => "eml",
        }
    }
}

/// Where an export goes, and whether a file is there already.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Destination {
    path: PathBuf,
    replaces: bool,
}

/// The file an export writes to. With no place named, the file goes in
/// `downloads` under `name`, numbered past any file already there, and a
/// folder the user named gets it the same way. A file the user named is
/// taken as given: a relative path counts from `downloads`, and a file
/// already there is replaced, which the question says.
fn destination(
    downloads: &Path,
    named: Option<&str>,
    name: &str,
    extension: &str,
) -> Result<Destination, String> {
    let Some(named) = named else {
        return Ok(Destination {
            path: free_name(downloads, name),
            replaces: false,
        });
    };
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut path = match (named.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(named),
    };
    if path.is_relative() {
        path = downloads.join(path);
    }
    if path.is_dir() {
        return Ok(Destination {
            path: free_name(&path, name),
            replaces: false,
        });
    }
    if named.ends_with('/') {
        return Err(format!("There is no folder {}.", path.display()));
    }
    if path.extension().is_none() {
        path.set_extension(extension);
    }
    let folder = path.parent().filter(|p| !p.as_os_str().is_empty());
    if folder.is_some_and(|folder| !folder.is_dir()) {
        return Err(format!(
            "There is no folder {}.",
            folder.unwrap_or(Path::new("")).display()
        ));
    }
    if path.is_dir() {
        return Err(format!("{} is a folder.", path.display()));
    }
    Ok(Destination {
        replaces: path.exists(),
        path,
    })
}

/// `name` in `folder`, or the first of `name 2`, `name 3` and on that no
/// file holds yet, so an export never lands on an earlier one.
fn free_name(folder: &Path, name: &str) -> PathBuf {
    let first = folder.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) => (stem, format!(".{extension}")),
        None => (name, String::new()),
    };
    (2..)
        .map(|n| folder.join(format!("{stem} {n}{extension}")))
        .find(|path| !path.exists())
        .unwrap_or(first)
}

/// The lines under a contact question that show what the contact will
/// hold: only the fields the call gives.
fn contact_lines(fields: &ContactFields) -> String {
    let mut lines = Vec::new();
    if let Some(name) = &fields.name {
        lines.push(fill(&gettext("Name: {name}"), &[("name", name)]));
    }
    if let Some(emails) = &fields.emails {
        lines.push(fill(
            &gettext("Addresses: {addresses}"),
            &[("addresses", &emails.join(", "))],
        ));
    }
    if let Some(phones) = &fields.phones {
        lines.push(fill(
            &gettext("Phones: {phones}"),
            &[("phones", &phones.join(", "))],
        ));
    }
    if let Some(organization) = &fields.organization {
        lines.push(fill(
            &gettext("Organisation: {organisation}"),
            &[("organisation", organization)],
        ));
    }
    lines.join("\n")
}

/// A contact as the model reads one, `id` included so `update_contact`
/// can name it.
fn contact_json(contact: &Contact, account: &str) -> Value {
    json!({
        "id": contact.resource,
        "account": account,
        "name": contact.name,
        "emails": contact.emails,
        "phone": contact.phone,
        "organization": contact.organization,
    })
}

/// A list of strings from the call, trimmed and without blanks. `None`
/// when the call leaves the key out.
fn strings(input: &Value, key: &str) -> Option<Vec<String>> {
    input.get(key).and_then(Value::as_array).map(|items| {
        items
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    })
}

/// The fields a contact tool writes. A string key given empty clears the
/// field, so it counts as given.
fn contact_fields(input: &Value) -> ContactFields {
    let given = |key: &str| {
        input
            .get(key)
            .and_then(Value::as_str)
            .map(|s| s.trim().to_string())
    };
    ContactFields {
        name: given("name"),
        emails: strings(input, "emails"),
        phones: strings(input, "phones"),
        organization: given("organization"),
    }
}

fn smart_json(mailbox: &SmartMailbox) -> Value {
    json!({
        "id": mailbox.id,
        "name": mailbox.name,
        "account": mailbox.account,
        "match_all": mailbox.match_all,
        "conditions": mailbox.conditions,
        "gmail_query": mailbox.query(),
    })
}

impl<A: Accounts> Tools<A> {
    /// Runs a write on the store's writer.
    async fn write<T, F>(&self, change: F) -> Result<T, String>
    where
        F: FnOnce(&rusqlite::Connection) -> mailrs_store::Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let db = self.modules.db.clone();
        self.call(async move { db.write(change).await }).await
    }

    // ---- Labels ----------------------------------------------------------

    /// The user label an account holds under `name`, in any case.
    fn user_label(&self, account: &Account, name: &str) -> Result<Label, String> {
        self.labels_of(account.id)
            .into_iter()
            .find(|l| l.kind == LabelKind::User && l.name.eq_ignore_ascii_case(name.trim()))
            .ok_or_else(|| format!("{} has no label called {name}.", account.email))
    }

    pub(super) async fn rename_label<'a>(&'a self, input: &'a Value) -> Result<Plan<'a>, String> {
        let (account, settings) = self.settings_for(&required(input, "account")?)?;
        let label = self.user_label(&account, &required(input, "label")?)?;
        let wanted = required(input, "new_name")?;
        let question = fill(
            &gettext(
                "Rename the label “{label}” in {account} to “{name}”? Labels nested under it move along.",
            ),
            &[
                ("label", &label.name),
                ("account", &account.email),
                ("name", &wanted),
            ],
        );
        Ok(Plan::ask(question, async move {
            let (account_id, id, name) = (account.id, label.id.clone(), wanted.clone());
            self.permitted(&account, Permission::Settings, async move {
                settings.rename_label(account_id, &id, &name).await
            })
            .await?;
            Ok(json!({"renamed": label.name, "to": wanted}))
        }))
    }

    pub(super) async fn recolor_label<'a>(&'a self, input: &'a Value) -> Result<Plan<'a>, String> {
        let (account, settings) = self.settings_for(&required(input, "account")?)?;
        let label = self.user_label(&account, &required(input, "label")?)?;
        let key = required(input, "color")?.to_lowercase();
        let key = if key == "grey" { "gray".into() } else { key };
        let index = COLOR_KEYS
            .iter()
            .position(|k| *k == key)
            .ok_or_else(|| format!("Gmail has no label colour called {key}."))?;
        let (background, text) = LABEL_COLORS[index];
        let question = fill(
            &gettext("Colour the label “{label}” in {account} {color}?"),
            &[
                ("label", &label.name),
                ("account", &account.email),
                ("color", &label_color_name(index).to_lowercase()),
            ],
        );
        Ok(Plan::ask(question, async move {
            let color = LabelColor {
                background_color: background.to_string(),
                text_color: text.to_string(),
            };
            let (account_id, id) = (account.id, label.id.clone());
            self.permitted(&account, Permission::Settings, async move {
                settings.recolor_label(account_id, &id, color).await
            })
            .await?;
            Ok(json!({"label": label.name, "color": key}))
        }))
    }

    /// Counts the label's conversations in Gmail before asking, so the
    /// question says what the label is on.
    pub(super) async fn delete_label<'a>(&'a self, input: &'a Value) -> Result<Plan<'a>, String> {
        let (account, settings) = self.settings_for(&required(input, "account")?)?;
        let label = self.user_label(&account, &required(input, "label")?)?;
        let count = {
            let (settings, account_id, id) = (Arc::clone(&settings), account.id, label.id.clone());
            self.permitted(&account, Permission::Settings, async move {
                settings.label_threads(account_id, &id).await
            })
            .await?
        };
        let question = fill_plural(
            "Delete the label “{label}” from {account}? {count} conversation carries it. The mail stays in Gmail, without the label.",
            "Delete the label “{label}” from {account}? {count} conversations carry it. The mail stays in Gmail, without the label.",
            count as usize,
            &[
                ("label", &label.name),
                ("account", &account.email),
                ("count", &count.to_string()),
            ],
        );
        Ok(Plan::ask(question, async move {
            let (account_id, id) = (account.id, label.id.clone());
            self.permitted(&account, Permission::Settings, async move {
                settings.delete_label(account_id, &id).await
            })
            .await?;
            Ok(json!({"deleted": label.name, "conversations": count}))
        }))
    }

    // ---- Smart mailboxes -------------------------------------------------

    pub(super) fn list_smart_mailboxes(&self) -> Value {
        let all = self.desk.settings().smart_mailboxes;
        json!({"smart_mailboxes": all.iter().map(smart_json).collect::<Vec<_>>()})
    }

    /// The smart mailbox a call names, by id or by name.
    fn smart_named(&self, wanted: &str) -> Result<SmartMailbox, String> {
        self.desk
            .settings()
            .smart_mailboxes
            .into_iter()
            .find(|m| m.id == wanted || m.name.trim().eq_ignore_ascii_case(wanted))
            .ok_or_else(|| format!("There is no smart mailbox called {wanted}."))
    }

    pub(super) async fn delete_smart_mailbox<'a>(
        &'a self,
        input: &'a Value,
    ) -> Result<Plan<'a>, String> {
        let mailbox = self.smart_named(&required(input, "mailbox")?)?;
        let question = fill(
            &gettext("Delete the smart mailbox “{name}”? The mail it lists stays where it is."),
            &[("name", &mailbox.name)],
        );
        Ok(Plan::ask(question, async move {
            self.effects
                .change_settings(Change::DeleteSmartMailbox(mailbox.id.clone()))?;
            Ok(json!({"deleted": mailbox.name}))
        }))
    }

    /// Changes a smart mailbox in place, keeping its id so the sidebar
    /// keeps its place. Fields the call leaves out stay as they are.
    pub(super) fn update_smart_mailbox(&self, input: &Value) -> ToolResult {
        let mut mailbox = self.smart_named(&required(input, "mailbox")?)?;
        if let Some(name) = text(input, "name") {
            mailbox.name = name;
        }
        if let Some(email) = text(input, "account") {
            mailbox.account = Some(self.account_named(&email)?.email);
        }
        if flag(input, "all_accounts") == Some(true) {
            mailbox.account = None;
        }
        if let Some(all) = flag(input, "match_all") {
            mailbox.match_all = all;
        }
        if let Some(conditions) = input.get("conditions") {
            mailbox.conditions = serde_json::from_value(conditions.clone())
                .map_err(|e| format!("Could not read the conditions: {e}"))?;
        }
        if mailbox.query().is_none() {
            return Err("Give at least one condition with a value.".into());
        }
        let result = smart_json(&mailbox);
        self.effects
            .change_settings(Change::SaveSmartMailbox(Box::new(mailbox)))?;
        Ok(json!({"updated": result}))
    }

    // ---- Templates -------------------------------------------------------

    /// Saves a template, over the one with the same name when there is
    /// one, which the question says.
    pub(super) async fn save_template<'a>(&'a self, input: &'a Value) -> Result<Plan<'a>, String> {
        let name = required(input, "name")?;
        let markdown = input
            .get("body")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|b| !b.trim().is_empty())
            .ok_or("`body` is missing")?;
        let existing = self
            .read(templates::list)
            .await?
            .into_iter()
            .find(|t| t.name.trim().eq_ignore_ascii_case(&name));
        let subject = text(input, "subject")
            .or_else(|| existing.as_ref().map(|t| t.subject.clone()))
            .unwrap_or_default();
        let question = match &existing {
            Some(old) => fill(
                &gettext("Replace the template “{name}” with this one? The saved one is lost."),
                &[("name", &old.name)],
            ),
            None => fill(
                &gettext("Save a template called “{name}”?"),
                &[("name", &name)],
            ),
        };
        let preview: String = markdown.chars().take(160).collect();
        let question = format!("{question}\n\n{preview}");
        Ok(Plan::ask(question, async move {
            let replaced = existing.is_some();
            // A replaced template keeps the name it was saved under, so
            // the case the model typed does not rename it.
            let name = existing.as_ref().map_or(name, |t| t.name.clone());
            let template = Template {
                id: existing.map(|t| t.id).unwrap_or_default(),
                name: name.clone(),
                subject,
                markdown,
            };
            self.write(move |c| match replaced {
                true => templates::update(c, &template),
                false => templates::add(c, &template).map(|_| ()),
            })
            .await?;
            Ok(json!({"saved": name, "replaced": replaced}))
        }))
    }

    pub(super) async fn delete_template<'a>(
        &'a self,
        input: &'a Value,
    ) -> Result<Plan<'a>, String> {
        let name = required(input, "name")?;
        let template = self
            .read(templates::list)
            .await?
            .into_iter()
            .find(|t| t.name.trim().eq_ignore_ascii_case(&name))
            .ok_or_else(|| format!("There is no template called {name}."))?;
        let question = fill(
            &gettext("Delete the template “{name}”?"),
            &[("name", &template.name)],
        );
        Ok(Plan::ask(question, async move {
            let id = template.id;
            self.write(move |c| templates::remove(c, id)).await?;
            Ok(json!({"deleted": template.name}))
        }))
    }

    // ---- Contacts --------------------------------------------------------

    pub(super) async fn create_contact<'a>(&'a self, input: &'a Value) -> Result<Plan<'a>, String> {
        let account = self.account_or_default(input)?;
        let fields = contact_fields(input);
        let named = fields.name.as_deref().is_some_and(|n| !n.is_empty());
        let addressed = fields.emails.as_ref().is_some_and(|e| !e.is_empty());
        if !named && !addressed {
            return Err("Give the contact a name or an address.".into());
        }
        let who = match (&fields.name, fields.emails.as_ref().and_then(|e| e.first())) {
            (Some(name), _) if !name.is_empty() => name.clone(),
            (_, Some(email)) => email.clone(),
            _ => String::new(),
        };
        let question = format!(
            "{}\n\n{}",
            fill(
                &gettext("Add {contact} to the Google contacts of {account}?"),
                &[("contact", &who), ("account", &account.email)],
            ),
            contact_lines(&fields)
        );
        Ok(Plan::ask(question, async move {
            let contacts = Arc::clone(&self.modules.contacts);
            let keep = self.desk.settings().reads_contacts(&account.email);
            let account_id = account.id;
            let made = self
                .permitted(&account, Permission::ChangeContacts, async move {
                    contacts.create(account_id, &fields, keep).await
                })
                .await?;
            Ok(json!({"created": contact_json(&made, &account.email)}))
        }))
    }

    /// Finds the contact by the id `find_contact` gave or by one of its
    /// addresses, then changes the fields the call gives.
    pub(super) async fn update_contact<'a>(&'a self, input: &'a Value) -> Result<Plan<'a>, String> {
        let account = self.account_or_default(input)?;
        let wanted = required(input, "contact")?;
        let fields = contact_fields(input);
        if fields.is_empty() {
            return Err("Give at least one field to change.".into());
        }
        let account_id = account.id;
        let key = wanted.clone();
        let stored = self
            .read(move |c| {
                Ok(address_book::list(c)?.into_iter().find(|contact| {
                    contact.account_id == account_id
                        && (contact.resource == key
                            || contact.emails.iter().any(|e| e.eq_ignore_ascii_case(&key)))
                }))
            })
            .await?;
        let (resource, who) = match stored {
            Some(contact) => (contact.resource.clone(), contact.display().to_string()),
            None if wanted.starts_with("people/") => (wanted.clone(), wanted.clone()),
            None => {
                return Err(format!(
                    "{} has no contact {wanted} on this computer. find_contact gives the id to use; an account with contacts off in Preferences shows none.",
                    account.email
                ));
            }
        };
        let question = format!(
            "{}\n\n{}",
            fill(
                &gettext("Change {contact} in the Google contacts of {account}?"),
                &[("contact", &who), ("account", &account.email)],
            ),
            contact_lines(&fields)
        );
        Ok(Plan::ask(question, async move {
            let contacts = Arc::clone(&self.modules.contacts);
            let keep = self.desk.settings().reads_contacts(&account.email);
            let changed = self
                .permitted(&account, Permission::ChangeContacts, async move {
                    contacts.update(account_id, &resource, &fields, keep).await
                })
                .await?;
            Ok(json!({"updated": contact_json(&changed, &account.email)}))
        }))
    }

    // ---- Remote images ---------------------------------------------------

    pub(super) async fn list_image_senders(&self) -> ToolResult {
        let list = self.read(image_senders::list).await?;
        let setting = serde_json::to_value(self.desk.settings().remote_images).ok();
        Ok(json!({
            "remote_images": setting,
            "senders": list.iter().map(|s: &ImageSender| json!({
                "sender": s.sender,
                "whole_domain": s.whole_domain,
                "since": local_text(s.allowed_at),
            })).collect::<Vec<_>>(),
        }))
    }

    /// Allows one address, or everyone at a domain. An address with
    /// `whole_domain` set stands for its domain.
    pub(super) async fn allow_images<'a>(&'a self, input: &'a Value) -> Result<Plan<'a>, String> {
        let given = required(input, "sender")?.to_lowercase();
        let whole_domain = flag(input, "whole_domain").unwrap_or(false) || !given.contains('@');
        let sender = match (whole_domain, images::domain_of(&given)) {
            (true, Some(domain)) => domain.to_string(),
            (true, None) => given.trim_start_matches('@').to_string(),
            (false, _) => given,
        };
        if sender.is_empty() || (whole_domain && !sender.contains('.')) {
            return Err(format!("{sender} is not an address or a domain."));
        }
        let ask = match whole_domain {
            true => fill(
                &gettext("Always load images from anyone at {domain}?"),
                &[("domain", &sender)],
            ),
            false => fill(
                &gettext("Always load images from {sender}?"),
                &[("sender", &sender)],
            ),
        };
        let question = format!(
            "{ask} {}",
            gettext("Loading a remote image tells the sender when you opened their mail.")
        );
        Ok(Plan::ask(question, async move {
            let (saved, now) = (sender.clone(), Local::now().timestamp_millis());
            self.write(move |c| image_senders::allow(c, &saved, whole_domain, now))
                .await?;
            self.effects.image_senders_changed();
            Ok(json!({"allowed": sender, "whole_domain": whole_domain}))
        }))
    }

    pub(super) async fn forget_image_sender<'a>(
        &'a self,
        input: &'a Value,
    ) -> Result<Plan<'a>, String> {
        let wanted = required(input, "sender")?.to_lowercase();
        let listed = self
            .read(image_senders::list)
            .await?
            .into_iter()
            .find(|s| s.sender == wanted.trim_start_matches('@'))
            .ok_or_else(|| {
                format!("{wanted} is not on the list; list_image_senders shows who is.")
            })?;
        let question = fill(
            &gettext("Stop loading images from {sender}?"),
            &[("sender", &listed.sender)],
        );
        Ok(Plan::ask(question, async move {
            let gone = listed.sender.clone();
            self.write(move |c| image_senders::forget(c, &gone)).await?;
            self.effects.image_senders_changed();
            Ok(json!({"forgotten": listed.sender}))
        }))
    }

    // ---- Export ----------------------------------------------------------

    pub(super) async fn export_mail<'a>(&'a self, input: &'a Value) -> Result<Plan<'a>, String> {
        let targets = self.parse_targets(input)?;
        if targets.is_empty() {
            return Err("`targets` is empty".into());
        }
        let format = match text(input, "format").as_deref() {
            None | Some("mbox") => Format::Mbox,
            Some("eml") => Format::Eml,
            Some(other) => return Err(format!("Unknown format {other}.")),
        };
        let (name, what) = match format {
            Format::Mbox => self.mbox_name(&targets).await?,
            Format::Eml => self.eml_name(&targets).await?,
        };
        let place = destination(
            &self.desk.downloads(),
            text(input, "path").as_deref(),
            &name,
            format.extension(),
        )?;
        let file = place.path.display().to_string();
        let mut question = fill(
            &gettext("Export {what} to {file}?"),
            &[("what", &what), ("file", &file)],
        );
        if place.replaces {
            question.push(' ');
            question.push_str(&gettext(
                "A file with that name is there already, and exporting replaces it.",
            ));
        }
        Ok(Plan::ask(question, async move {
            let mail = Arc::clone(&self.modules.mail);
            let path = place.path.clone();
            let bytes = self
                .call(async move {
                    let bytes = match format {
                        Format::Mbox => mail.export_mbox(&targets).await?,
                        Format::Eml => {
                            let target = &targets[0];
                            let id = target.message_id.clone().unwrap_or_default();
                            mail.export_message(target.account_id, &id).await?
                        }
                    };
                    tokio::fs::write(&path, &bytes).await?;
                    Ok::<usize, anyhow::Error>(bytes.len())
                })
                .await?;
            Ok(json!({"file": file, "bytes": bytes, "replaced": place.replaces}))
        }))
    }

    /// The file name for an mbox of `targets`, as the window's Export
    /// names one, and the words the question uses for what it holds.
    async fn mbox_name(&self, targets: &[Target]) -> Result<(String, String), String> {
        let wanted: Vec<(AccountId, String)> = targets
            .iter()
            .map(|t| (t.account_id, t.thread_id.clone()))
            .collect();
        let rows = self
            .read(move |c| {
                wanted
                    .iter()
                    .map(|(account_id, id)| threads::get_thread(c, *account_id, id))
                    .collect::<mailrs_store::Result<Vec<_>>>()
            })
            .await?;
        let newest = rows
            .iter()
            .flatten()
            .map(|r| r.last_message_at)
            .max()
            .unwrap_or_else(|| Local::now().timestamp_millis());
        let count = targets.len();
        let many = fill_plural(
            "{count} conversation",
            "{count} conversations",
            count,
            &[("count", &count.to_string())],
        );
        let name = match rows.first() {
            Some(Some(row)) if count == 1 => {
                export::file_name(&row.subject, row.last_message_at, "mbox")
            }
            _ => export::file_name(&many, newest, "mbox"),
        };
        Ok((name, many))
    }

    /// The file name for one message as `.eml`, and the words the question
    /// uses for it.
    async fn eml_name(&self, targets: &[Target]) -> Result<(String, String), String> {
        let [target] = targets else {
            return Err("An .eml file holds one message: give exactly one target.".into());
        };
        let id = target.message_id.clone().ok_or(
            "An .eml file holds one message: give the target's message_id, from read_conversation.",
        )?;
        let account_id = target.account_id;
        let found = self
            .read(move |c| messages::by_ids(c, account_id, &[id]))
            .await?;
        let (subject, date) = found
            .first()
            .map(|m| (m.subject.clone(), m.date))
            .unwrap_or_else(|| (String::new(), Local::now().timestamp_millis()));
        let what = fill(
            &gettext("the message “{subject}”"),
            &[("subject", &subject)],
        );
        Ok((export::file_name(&subject, date, "eml"), what))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_export_with_no_place_named_never_lands_on_an_earlier_one() {
        let dir = tempfile::tempdir().unwrap();
        let first = destination(dir.path(), None, "Kites 2026-01-01.mbox", "mbox").unwrap();
        assert_eq!(first.path, dir.path().join("Kites 2026-01-01.mbox"));
        assert!(!first.replaces);
        std::fs::write(&first.path, b"x").unwrap();
        let second = destination(dir.path(), None, "Kites 2026-01-01.mbox", "mbox").unwrap();
        assert_eq!(second.path, dir.path().join("Kites 2026-01-01 2.mbox"));
        assert!(!second.replaces);
    }

    #[test]
    fn a_named_place_is_taken_as_given_and_says_when_it_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("mail");
        std::fs::create_dir(&folder).unwrap();
        let inside = destination(dir.path(), Some("mail"), "Kites.mbox", "mbox").unwrap();
        assert_eq!(inside.path, folder.join("Kites.mbox"));

        let file = folder.join("backup.mbox");
        std::fs::write(&file, b"x").unwrap();
        let named = destination(dir.path(), file.to_str(), "Kites.mbox", "mbox").unwrap();
        assert_eq!(
            named,
            Destination {
                path: file,
                replaces: true
            }
        );

        let bare = destination(dir.path(), Some("mail/backup-2"), "Kites.mbox", "mbox").unwrap();
        assert_eq!(bare.path, folder.join("backup-2.mbox"));
        assert!(destination(dir.path(), Some("nowhere/x.mbox"), "K.mbox", "mbox").is_err());
    }
}
