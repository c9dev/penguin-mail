//! The fake's mail: folders, categories, messages and their log.

use mail_builder::MessageBuilder;
use mail_parser::MimeHeaders;
use mailrs_graph::{
    AttachmentInfo, DeltaPage, EmailAddress, Flag, GraphError, ItemBody, Listing, MailFolder,
    MasterCategory, Message, MessageBody, Page, Recipient, Removed, Write,
};
use serde_json::Value;

use super::{
    Answer, Area, FakeFolder, FakeGraph, FakeMessage, GraphState, Logged, Pending, Round, Upload,
    link, read_link,
};

/// A message to deliver, with what a test cares about and defaults for
/// the rest.
#[derive(Debug, Clone)]
pub struct FakeMail {
    pub from: (&'static str, &'static str),
    pub to: &'static str,
    pub subject: &'static str,
    pub text: &'static str,
    /// Milliseconds since the epoch.
    pub at: i64,
    pub conversation: Option<&'static str>,
    /// File name, content type and bytes.
    pub files: Vec<(&'static str, &'static str, Vec<u8>)>,
}

impl Default for FakeMail {
    fn default() -> Self {
        FakeMail {
            from: ("Ann", "ann@example.com"),
            to: "me@outlook.com",
            subject: "Hello",
            text: "Hello there.",
            at: 0,
            conversation: None,
            files: Vec::new(),
        }
    }
}

impl FakeGraph {
    /// Puts `mail` in `folder`, unread, and logs it. Answers its id.
    pub fn deliver(&self, folder: &str, mail: FakeMail) -> String {
        self.with(|s| {
            let id = s.new_id("msg");
            let when = chrono::DateTime::from_timestamp_millis(mail.at).unwrap_or_default();
            let mut builder = MessageBuilder::new()
                .from((mail.from.0, mail.from.1))
                .to(mail.to)
                .subject(mail.subject)
                .message_id(format!("{id}@outlook.example"))
                .date(when.timestamp())
                .text_body(mail.text);
            for (name, kind, bytes) in &mail.files {
                builder = builder.attachment(*kind, *name, bytes.clone());
            }
            let raw = builder.write_to_vec().unwrap_or_default();
            let files: Vec<(AttachmentInfo, Vec<u8>)> = mail
                .files
                .iter()
                .enumerate()
                .map(|(i, (name, kind, bytes))| {
                    (
                        AttachmentInfo {
                            id: format!("{id}-att-{i}"),
                            name: name.to_string(),
                            content_type: kind.to_string(),
                            size: bytes.len() as i64,
                            ..AttachmentInfo::default()
                        },
                        bytes.clone(),
                    )
                })
                .collect();
            let message = Message {
                id: id.clone(),
                conversation_id: Some(
                    mail.conversation.map_or_else(|| format!("conv-{id}"), str::to_string),
                ),
                internet_message_id: Some(format!("<{id}@outlook.example>")),
                parent_folder_id: Some(folder.to_string()),
                subject: Some(mail.subject.into()),
                body_preview: Some(mail.text.chars().take(255).collect()),
                received_date_time: Some(when.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
                from: Some(recipient(mail.from.0, mail.from.1)),
                to_recipients: vec![recipient("", mail.to)],
                has_attachments: Some(!files.is_empty()),
                is_read: Some(false),
                is_draft: Some(false),
                flag: Some(Flag { flag_status: "notFlagged".into() }),
                categories: Some(Vec::new()),
                inference_classification: Some("focused".into()),
                single_value_extended_properties: Some(vec![mailrs_graph::ExtendedProperty {
                    id: "Integer 0xe08".into(),
                    value: raw.len().to_string(),
                }]),
                ..Message::default()
            };
            s.messages.insert(id.clone(), FakeMessage { folder: folder.into(), message, raw, files });
            log(s, folder, &id);
            id
        })
    }

    pub fn move_message(&self, id: &str, folder: &str) {
        self.with(|s| move_to(s, id, folder));
    }

    pub fn mark(&self, id: &str, read: Option<bool>, flagged: Option<bool>) {
        self.with(|s| {
            let Some(held) = s.messages.get_mut(id) else { return };
            if let Some(read) = read {
                held.message.is_read = Some(read);
            }
            if let Some(flagged) = flagged {
                held.message.flag = Some(flag(flagged));
            }
            let folder = held.folder.clone();
            log(s, &folder, id);
        });
    }

    pub fn tag(&self, id: &str, categories: &[&str]) {
        self.with(|s| {
            let Some(held) = s.messages.get_mut(id) else { return };
            held.message.categories = Some(categories.iter().map(|c| c.to_string()).collect());
            let folder = held.folder.clone();
            log(s, &folder, id);
        });
    }

    pub fn classify(&self, id: &str, other: bool) {
        self.with(|s| {
            let Some(held) = s.messages.get_mut(id) else { return };
            held.message.inference_classification = Some(classification(other));
            let folder = held.folder.clone();
            log(s, &folder, id);
        });
    }

    /// Erases a message for good, as another client's Delete Forever does.
    pub fn delete(&self, id: &str) {
        self.with(|s| {
            if let Some(held) = s.messages.remove(id) {
                log(s, &held.folder, id);
            }
        });
    }
}

fn flag(flagged: bool) -> Flag {
    Flag { flag_status: if flagged { "flagged" } else { "notFlagged" }.into() }
}

fn classification(other: bool) -> String {
    if other { "other" } else { "focused" }.into()
}

fn recipient(name: &str, address: &str) -> Recipient {
    Recipient {
        email_address: EmailAddress {
            name: (!name.is_empty()).then(|| name.to_string()),
            address: Some(address.to_string()),
        },
    }
}

fn log(s: &mut GraphState, folder: &str, id: &str) {
    let seq = s.next_seq();
    s.mail_log.push(Logged { seq, place: folder.to_string(), id: id.to_string() });
}

fn move_to(s: &mut GraphState, id: &str, folder: &str) {
    let Some(held) = s.messages.get_mut(id) else { return };
    let from = std::mem::replace(&mut held.folder, folder.to_string());
    held.message.parent_folder_id = Some(folder.to_string());
    log(s, &from, id);
    log(s, folder, id);
}

fn millis(text: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(text).map_or(0, |t| t.timestamp_millis())
}

fn removed_message(id: &str) -> Message {
    Message {
        id: id.to_string(),
        removed: Some(Removed { reason: Some("deleted".into()) }),
        ..Message::default()
    }
}

/// The folder as Graph describes it, with the counts it keeps.
fn folder_info(s: &GraphState, id: &str) -> Option<MailFolder> {
    let folder = s.folders.get(id)?;
    let held = || s.messages.values().filter(|m| m.folder == id);
    Some(MailFolder {
        id: id.to_string(),
        display_name: folder.name.clone(),
        parent_folder_id: folder.parent.clone(),
        child_folder_count: s.folders.values().filter(|f| f.parent.as_deref() == Some(id)).count() as u32,
        total_item_count: held().count() as u64,
        unread_item_count: held().filter(|m| m.message.is_read != Some(true)).count() as u64,
        ..MailFolder::default()
    })
}

/// Files `raw` as a new message in `folder`, reading its headers, body
/// and files the way Exchange does on receipt.
fn file_message(s: &mut GraphState, folder: &str, raw: Vec<u8>, draft: bool) -> Answer<String> {
    let parsed = mail_parser::MessageParser::default()
        .parse(&raw)
        .ok_or_else(|| GraphError::Decode("not a message".into()))?;
    let id = s.new_id("msg");
    let address = |a: &mail_parser::Addr| recipient(a.name.as_deref().unwrap_or(""), a.address.as_deref().unwrap_or(""));
    let preview: String = parsed.body_text(0).unwrap_or_default().chars().take(255).collect();
    let files: Vec<(AttachmentInfo, Vec<u8>)> = parsed
        .attachments()
        .enumerate()
        .map(|(i, part)| {
            let kind = part
                .content_type()
                .map_or_else(|| "application/octet-stream".to_string(), |c| match c.subtype() {
                    Some(sub) => format!("{}/{sub}", c.ctype()),
                    None => c.ctype().to_string(),
                });
            (
                AttachmentInfo {
                    id: format!("{id}-att-{i}"),
                    name: part.attachment_name().unwrap_or("file").to_string(),
                    content_type: kind,
                    size: part.contents().len() as i64,
                    ..AttachmentInfo::default()
                },
                part.contents().to_vec(),
            )
        })
        .collect();
    let when = parsed.date().map_or(0, |d| d.to_timestamp() * 1000);
    let message = Message {
        id: id.clone(),
        conversation_id: Some(format!("conv-{id}")),
        internet_message_id: Some(
            parsed.message_id().map_or_else(|| format!("<{id}@outlook.example>"), |m| format!("<{m}>")),
        ),
        parent_folder_id: Some(folder.to_string()),
        subject: parsed.subject().map(str::to_string),
        body_preview: Some(preview),
        received_date_time: Some(
            chrono::DateTime::from_timestamp_millis(when)
                .unwrap_or_default()
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        ),
        from: parsed.from().and_then(|a| a.first()).map(address),
        to_recipients: parsed.to().map(|a| a.iter().map(address).collect()).unwrap_or_default(),
        has_attachments: Some(!files.is_empty()),
        is_read: Some(true),
        is_draft: Some(draft),
        flag: Some(flag(false)),
        categories: Some(Vec::new()),
        inference_classification: Some(classification(false)),
        single_value_extended_properties: Some(vec![mailrs_graph::ExtendedProperty {
            id: "Integer 0xe08".into(),
            value: raw.len().to_string(),
        }]),
        ..Message::default()
    };
    s.messages.insert(id.clone(), FakeMessage { folder: folder.into(), message, raw, files });
    log(s, folder, &id);
    Ok(id)
}

fn held<'a>(s: &'a GraphState, id: &str) -> Answer<&'a FakeMessage> {
    s.messages.get(id).ok_or(GraphError::NotFound)
}

pub(super) fn well_known(s: &mut GraphState, names: &[&str]) -> Answer<Vec<Option<MailFolder>>> {
    s.refuses(Area::Mail)?;
    Ok(names
        .iter()
        .map(|name| s.well_known.get(*name).and_then(|id| folder_info(s, id)))
        .collect())
}

pub(super) fn folders(s: &mut GraphState, parent: Option<&str>, _next: Option<&str>) -> Answer<Page<MailFolder>> {
    s.refuses(Area::Mail)?;
    let value = s
        .folders
        .iter()
        .filter(|(_, f)| f.parent.as_deref() == parent)
        .filter_map(|(id, _)| folder_info(s, id))
        .collect();
    Ok(Page { value, next_link: None })
}

pub(super) fn categories(s: &mut GraphState) -> Answer<Vec<MasterCategory>> {
    s.refuses(Area::Mail)?;
    Ok(s.categories.clone())
}

pub(super) fn message_delta(
    s: &mut GraphState,
    folder: &str,
    link_text: Option<&str>,
    since: &str,
) -> Answer<DeltaPage<Message>> {
    s.refuses(Area::Mail)?;
    if !s.folders.contains_key(folder) {
        return Err(GraphError::NotFound);
    }
    let since_ms = millis(since);
    let s = &*s;
    Round { log: &s.mail_log, expired_before: s.expired_before, now: s.seq, place: folder }.run(
        link_text,
        || {
            let mut here: Vec<&FakeMessage> = s
                .messages
                .values()
                .filter(|m| m.folder == folder && m.message.received_millis().unwrap_or(0) >= since_ms)
                .collect();
            here.sort_by_key(|m| std::cmp::Reverse(m.message.received_millis()));
            here.into_iter().map(|m| m.message.clone()).collect()
        },
        |id| s.messages.get(id).filter(|m| m.folder == folder).map(|m| m.message.clone()),
        removed_message,
    )
}

/// One clause of a KQL search, down to the words it looks for: the `key:`
/// prefix, a comparison sign and the quotes are dropped.
fn clause_words(clause: &str) -> String {
    let clause = clause.trim();
    let clause = match clause.split_once(':') {
        Some((key, rest)) if key.chars().all(char::is_alphabetic) => rest,
        _ => clause,
    };
    clause.trim_start_matches(['>', '<', '=']).replace('"', "").trim().to_lowercase()
}

fn matches_search(m: &FakeMessage, kql: &str) -> bool {
    let from = m.message.from.as_ref().map(|f| &f.email_address);
    let haystack = [
        m.message.subject.as_deref(),
        from.and_then(|f| f.address.as_deref()),
        from.and_then(|f| f.name.as_deref()),
        m.message.body_preview.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("\n")
    .to_lowercase();
    kql.split(" AND ").all(|clause| {
        let lower = clause.to_lowercase();
        // The fake keeps no sizes to compare and checks no dates here.
        if lower.contains("received") || lower.contains("size") {
            return true;
        }
        let words = clause_words(clause);
        words.is_empty() || haystack.contains(&words)
    })
}

pub(super) fn list_messages(
    s: &mut GraphState,
    listing: &Listing,
    next: Option<&str>,
) -> Answer<Page<Message>> {
    s.refuses(Area::Mail)?;
    let (listing, offset, slot) = match next {
        Some(text) => {
            let (kind, place, _, offset) = read_link(text).ok_or_else(|| GraphError::Decode("bad link".into()))?;
            let slot: usize = place.parse().map_err(|_| GraphError::Decode("bad link".into()))?;
            match (kind.as_str(), s.pending.get(slot)) {
                ("list", Some(Pending::Listing(l))) => (l.clone(), offset, Some(slot)),
                _ => return Err(GraphError::Decode("bad link".into())),
            }
        }
        None => (listing.clone(), 0, None),
    };
    let since = listing.received_since.as_deref().map(millis);
    let mut found: Vec<&FakeMessage> = s
        .messages
        .values()
        .filter(|m| listing.folder.as_ref().is_none_or(|f| &m.folder == f))
        .filter(|m| since.is_none_or(|t| m.message.received_millis().unwrap_or(0) >= t))
        .filter(|m| listing.conversation.as_ref().is_none_or(|c| m.message.conversation_id.as_ref() == Some(c)))
        .filter(|m| {
            listing.internet_message_id.as_ref().is_none_or(|i| m.message.internet_message_id.as_ref() == Some(i))
        })
        .filter(|m| listing.search.as_deref().is_none_or(|kql| matches_search(m, kql)))
        .collect();
    found.sort_by_key(|m| std::cmp::Reverse(m.message.received_millis()));
    let top = listing.top.clamp(1, 1000) as usize;
    let value: Vec<Message> = found.iter().skip(offset).take(top).map(|m| m.message.clone()).collect();
    let more = offset + value.len() < found.len();
    let next_link = if more {
        let slot = match slot {
            Some(slot) => slot.to_string(),
            None => s.remember(Pending::Listing(listing)),
        };
        Some(link("list", &slot, 0, Some(offset + value.len())))
    } else {
        None
    };
    Ok(Page { value, next_link })
}

pub(super) fn messages(s: &mut GraphState, ids: &[String]) -> Answer<Vec<Answer<Message>>> {
    s.refuses(Area::Mail)?;
    Ok(ids.iter().map(|id| held(s, id).map(|m| m.message.clone())).collect())
}

pub(super) fn raw(s: &mut GraphState, id: &str, limit: usize) -> Answer<Vec<u8>> {
    s.refuses(Area::Mail)?;
    let held = held(s, id)?;
    if held.raw.len() > limit {
        return Err(GraphError::TooLarge { limit });
    }
    Ok(held.raw.clone())
}

pub(super) fn body(s: &mut GraphState, id: &str) -> Answer<MessageBody> {
    s.refuses(Area::Mail)?;
    let held = held(s, id)?;
    let text = mail_parser::MessageParser::default()
        .parse(&held.raw)
        .and_then(|m| m.body_text(0).map(|t| t.to_string()))
        .or_else(|| held.message.body_preview.clone())
        .unwrap_or_default();
    Ok(MessageBody {
        body: ItemBody { content_type: "text".into(), content: text },
        attachments: held.files.iter().map(|(info, _)| info.clone()).collect(),
    })
}

pub(super) fn attachment(s: &mut GraphState, message: &str, attachment: &str, limit: usize) -> Answer<Vec<u8>> {
    s.refuses(Area::Mail)?;
    let (_, bytes) = held(s, message)?
        .files
        .iter()
        .find(|(info, _)| info.id == attachment)
        .ok_or(GraphError::NotFound)?;
    if bytes.len() > limit {
        return Err(GraphError::TooLarge { limit });
    }
    Ok(bytes.clone())
}

fn write_one(s: &mut GraphState, write: &Write) -> Answer<()> {
    match write {
        Write::Move { id, folder } => {
            held(s, id)?;
            if !s.folders.contains_key(folder) {
                return Err(GraphError::NotFound);
            }
            move_to(s, id, folder);
        }
        Write::Patch { id, patch } => {
            let held = s.messages.get_mut(id).ok_or(GraphError::NotFound)?;
            if let Some(read) = patch.is_read {
                held.message.is_read = Some(read);
            }
            if let Some(flagged) = patch.flagged {
                held.message.flag = Some(flag(flagged));
            }
            if let Some(categories) = &patch.categories {
                held.message.categories = Some(categories.clone());
            }
            if let Some(other) = patch.other {
                held.message.inference_classification = Some(classification(other));
            }
            let folder = held.folder.clone();
            log(s, &folder, id);
        }
        Write::Delete { id } => {
            held(s, id)?;
            let bin = s.well_known.get("deleteditems").cloned().ok_or(GraphError::NotFound)?;
            move_to(s, id, &bin);
        }
        Write::PermanentDelete { id } => {
            let gone = s.messages.remove(id).ok_or(GraphError::NotFound)?;
            log(s, &gone.folder, id);
        }
    }
    Ok(())
}

pub(super) fn apply(s: &mut GraphState, writes: &[Write]) -> Answer<Vec<Answer<()>>> {
    s.refuses(Area::Mail)?;
    Ok(writes.iter().map(|w| write_one(s, w)).collect())
}

pub(super) fn create_folder(s: &mut GraphState, parent: Option<&str>, name: &str) -> Answer<MailFolder> {
    s.refuses(Area::Mail)?;
    if parent.is_some_and(|p| !s.folders.contains_key(p)) {
        return Err(GraphError::NotFound);
    }
    let id = s.new_id("folder");
    s.folders.insert(id.clone(), FakeFolder { name: name.into(), parent: parent.map(str::to_string) });
    folder_info(s, &id).ok_or(GraphError::NotFound)
}

pub(super) fn rename_folder(s: &mut GraphState, id: &str, name: &str) -> Answer<MailFolder> {
    s.refuses(Area::Mail)?;
    s.folders.get_mut(id).ok_or(GraphError::NotFound)?.name = name.into();
    folder_info(s, id).ok_or(GraphError::NotFound)
}

pub(super) fn delete_folder(s: &mut GraphState, id: &str) -> Answer<()> {
    s.refuses(Area::Mail)?;
    if !s.folders.contains_key(id) {
        return Err(GraphError::NotFound);
    }
    // The folder, its children and theirs, with every message in them.
    let mut doomed = vec![id.to_string()];
    let mut at = 0;
    while at < doomed.len() {
        let parent = doomed[at].clone();
        doomed.extend(s.folders.iter().filter(|(_, f)| f.parent.as_deref() == Some(&parent)).map(|(k, _)| k.clone()));
        at += 1;
    }
    let lost: Vec<(String, String)> = s
        .messages
        .iter()
        .filter(|(_, m)| doomed.contains(&m.folder))
        .map(|(mid, m)| (mid.clone(), m.folder.clone()))
        .collect();
    for (mid, folder) in lost {
        s.messages.remove(&mid);
        log(s, &folder, &mid);
    }
    for gone in &doomed {
        s.folders.remove(gone);
    }
    s.well_known.retain(|_, v| !doomed.contains(v));
    Ok(())
}

pub(super) fn set_category_color(s: &mut GraphState, id: &str, color: &str) -> Answer<MasterCategory> {
    s.refuses(Area::Mail)?;
    let category = s.categories.iter_mut().find(|c| c.id == id).ok_or(GraphError::NotFound)?;
    category.color = color.into();
    Ok(category.clone())
}

pub(super) fn send_mime(s: &mut GraphState, raw: &[u8]) -> Answer<()> {
    s.refuses(Area::Mail)?;
    let sent = s.well_known.get("sentitems").cloned().ok_or(GraphError::NotFound)?;
    file_message(s, &sent, raw.to_vec(), false)?;
    s.sent.push(raw.to_vec());
    Ok(())
}

fn draft_message(s: &mut GraphState, raw: Vec<u8>) -> Answer<Message> {
    let drafts = s.well_known.get("drafts").cloned().ok_or(GraphError::NotFound)?;
    let id = file_message(s, &drafts, raw, true)?;
    Ok(held(s, &id)?.message.clone())
}

pub(super) fn create_draft_mime(s: &mut GraphState, raw: &[u8]) -> Answer<Message> {
    s.refuses(Area::Mail)?;
    draft_message(s, raw.to_vec())
}

pub(super) fn create_draft(s: &mut GraphState, draft: &Value) -> Answer<Message> {
    s.refuses(Area::Mail)?;
    let mut builder = MessageBuilder::new()
        .from(s.me.as_str())
        .subject(draft["subject"].as_str().unwrap_or_default())
        .text_body(draft["body"]["content"].as_str().unwrap_or_default());
    if let Some(id) = draft["internetMessageId"].as_str() {
        builder = builder.message_id(id.trim_matches(['<', '>']));
    }
    for to in draft["toRecipients"].as_array().into_iter().flatten() {
        if let Some(address) = to["emailAddress"]["address"].as_str() {
            builder = builder.to(address);
        }
    }
    let raw = builder.write_to_vec().map_err(|e| GraphError::Decode(e.to_string()))?;
    draft_message(s, raw)
}

pub(super) fn upload_session(
    s: &mut GraphState,
    message: &str,
    name: &str,
    size: u64,
    is_inline: bool,
    content_id: Option<&str>,
) -> Answer<String> {
    s.refuses(Area::Mail)?;
    held(s, message)?;
    s.next_id += 1;
    let url = format!("fake:upload:{}", s.next_id);
    s.uploads.insert(
        url.clone(),
        Upload {
            message: message.into(),
            name: name.into(),
            size,
            is_inline,
            content_id: content_id.map(str::to_string),
            bytes: Vec::new(),
        },
    );
    Ok(url)
}

pub(super) fn upload_chunk(s: &mut GraphState, url: &str, offset: u64, total: u64, bytes: &[u8]) -> Answer<bool> {
    s.refuses(Area::Mail)?;
    let upload = s.uploads.get_mut(url).ok_or(GraphError::NotFound)?;
    if offset != upload.bytes.len() as u64 {
        return Err(GraphError::Conflict);
    }
    upload.bytes.extend_from_slice(bytes);
    if (upload.bytes.len() as u64) < total.max(upload.size) {
        return Ok(false);
    }
    let Some(done) = s.uploads.remove(url) else { return Ok(false) };
    let held = s.messages.get_mut(&done.message).ok_or(GraphError::NotFound)?;
    let info = AttachmentInfo {
        id: format!("{}-att-{}", done.message, held.files.len()),
        name: done.name,
        content_type: "application/octet-stream".into(),
        size: done.bytes.len() as i64,
        is_inline: done.is_inline,
        content_id: done.content_id,
    };
    held.files.push((info, done.bytes));
    held.message.has_attachments = Some(true);
    let folder = held.folder.clone();
    log(s, &folder, &done.message);
    Ok(true)
}

pub(super) fn send_draft(s: &mut GraphState, id: &str) -> Answer<()> {
    s.refuses(Area::Mail)?;
    let raw = held(s, id)?.raw.clone();
    let sent = s.well_known.get("sentitems").cloned().ok_or(GraphError::NotFound)?;
    move_to(s, id, &sent);
    if let Some(message) = s.messages.get_mut(id) {
        message.message.is_draft = Some(false);
    }
    s.sent.push(raw);
    Ok(())
}

pub(super) fn delete_message(s: &mut GraphState, id: &str) -> Answer<()> {
    s.refuses(Area::Mail)?;
    let gone = s.messages.remove(id).ok_or(GraphError::NotFound)?;
    log(s, &gone.folder, id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use mailrs_graph::{GraphError, Write};

    use crate::fake::{FakeGraph, FakeMail};
    use crate::services::microsoft::GraphApi;

    // `FakeMail` defaults to the epoch, so the window starts there.
    const SINCE: &str = "1970-01-01T00:00:00Z";

    async fn to_the_end(
        fake: &FakeGraph,
        folder: &str,
        link: Option<String>,
    ) -> (Vec<mailrs_graph::Message>, String) {
        let (mut seen, mut link) = (Vec::new(), link);
        loop {
            let page = fake.message_delta(folder, link.as_deref(), SINCE).await.unwrap();
            seen.extend(page.value);
            match (page.next_link, page.delta_link) {
                (Some(next), _) => link = Some(next),
                (None, Some(delta)) => return (seen, delta),
                (None, None) => panic!("a delta page with neither link"),
            }
        }
    }

    #[tokio::test]
    async fn a_delta_names_a_move_as_a_removal_and_an_addition() {
        let fake = FakeGraph::new();
        let (inbox, archive) = (fake.folder_id("inbox"), fake.folder_id("archive"));
        let id = fake.deliver(&inbox, FakeMail { subject: "Lunch", ..FakeMail::default() });
        let (first, inbox_link) = to_the_end(&fake, &inbox, None).await;
        assert_eq!(first.len(), 1);
        let (_, archive_link) = to_the_end(&fake, &archive, None).await;
        fake.move_message(&id, &archive);
        let (from_inbox, _) = to_the_end(&fake, &inbox, Some(inbox_link)).await;
        let (into_archive, _) = to_the_end(&fake, &archive, Some(archive_link)).await;
        assert!(from_inbox[0].removed.is_some() && from_inbox[0].id == id);
        assert!(into_archive[0].removed.is_none() && into_archive[0].id == id, "the id stays");
    }

    #[tokio::test]
    async fn an_expired_link_loses_its_place() {
        let fake = FakeGraph::new();
        let inbox = fake.folder_id("inbox");
        let (_, link) = to_the_end(&fake, &inbox, None).await;
        fake.expire_links();
        let lost = fake.message_delta(&inbox, Some(&link), SINCE).await;
        assert!(matches!(lost, Err(GraphError::SyncStateLost)));
    }

    #[tokio::test]
    async fn a_round_started_after_the_links_expired_keeps_its_place() {
        let fake = FakeGraph::new();
        let inbox = fake.folder_id("inbox");
        fake.expire_links();
        let (_, link) = to_the_end(&fake, &inbox, None).await;
        assert!(fake.message_delta(&inbox, Some(&link), SINCE).await.is_ok());
    }

    #[tokio::test]
    async fn a_delta_pages_fifty_at_a_time() {
        let fake = FakeGraph::new();
        let inbox = fake.folder_id("inbox");
        for i in 0..120 {
            fake.deliver(&inbox, FakeMail { at: i, ..FakeMail::default() });
        }
        let page = fake.message_delta(&inbox, None, SINCE).await.unwrap();
        assert_eq!(page.value.len(), 50);
        assert!(page.next_link.is_some() && page.delta_link.is_none());
    }

    #[tokio::test]
    async fn a_write_to_a_missing_message_fails_alone() {
        let fake = FakeGraph::new();
        let inbox = fake.folder_id("inbox");
        let id = fake.deliver(&inbox, FakeMail::default());
        let done = fake
            .apply(&[
                Write::Delete { id: "gone".into() },
                Write::Move { id: id.clone(), folder: fake.folder_id("archive") },
            ])
            .await
            .unwrap();
        assert!(matches!(done[0], Err(GraphError::NotFound)));
        assert!(done[1].is_ok());
    }

    #[tokio::test]
    async fn a_search_finds_the_subject_without_regard_to_case() {
        let fake = FakeGraph::new();
        let inbox = fake.folder_id("inbox");
        fake.deliver(&inbox, FakeMail { subject: "Weekly Report", ..FakeMail::default() });
        fake.deliver(&inbox, FakeMail { subject: "Lunch", ..FakeMail::default() });
        let listing = mailrs_graph::Listing {
            search: Some("subject:\"weekly report\"".into()),
            top: 10,
            ..mailrs_graph::Listing::default()
        };
        let page = fake.list_messages(&listing, None).await.unwrap();
        assert_eq!(page.value.len(), 1);
    }

    #[tokio::test]
    async fn a_long_listing_carries_on_through_its_next_link() {
        let fake = FakeGraph::new();
        let inbox = fake.folder_id("inbox");
        for i in 0..5 {
            fake.deliver(&inbox, FakeMail { at: i, ..FakeMail::default() });
        }
        let listing = mailrs_graph::Listing { folder: Some(inbox), top: 3, ..Default::default() };
        let first = fake.list_messages(&listing, None).await.unwrap();
        assert_eq!(first.value.len(), 3);
        let second = fake.list_messages(&listing, first.next_link.as_deref()).await.unwrap();
        assert_eq!(second.value.len(), 2);
        assert!(second.next_link.is_none());
    }

    #[tokio::test]
    async fn a_sent_message_lands_in_sent_items_read() {
        let fake = FakeGraph::new();
        let raw = b"From: me@outlook.com\r\nTo: ann@example.com\r\nSubject: Hi\r\nMessage-ID: <a@b>\r\n\r\nHello\r\n";
        fake.send_mime(raw).await.unwrap();
        let sent = fake.folder_id("sentitems");
        let page = fake
            .list_messages(&mailrs_graph::Listing { folder: Some(sent), top: 10, ..Default::default() }, None)
            .await
            .unwrap();
        assert_eq!(page.value[0].subject.as_deref(), Some("Hi"));
        assert_eq!(page.value[0].is_read, Some(true));
    }

    #[tokio::test]
    async fn an_upload_in_pieces_adds_the_file_to_the_draft() {
        let fake = FakeGraph::new();
        let draft = fake.create_draft(&serde_json::json!({"subject": "With file"})).await.unwrap();
        let url = fake.upload_session(&draft.id, "a.txt", 4, false, None).await.unwrap();
        assert!(!fake.upload_chunk(&url, 0, 4, b"ab").await.unwrap());
        assert!(fake.upload_chunk(&url, 1, 4, b"cd").await.is_err(), "a gap is refused");
        assert!(fake.upload_chunk(&url, 2, 4, b"cd").await.unwrap());
        let body = fake.body(&draft.id).await.unwrap();
        assert_eq!(body.attachments[0].name, "a.txt");
        assert_eq!(fake.attachment(&draft.id, &body.attachments[0].id, 100).await.unwrap(), b"abcd");
    }
}
