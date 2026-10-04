//! A vCard, 3.0 or 4.0, and the fields Penguin Mail keeps for a contact:
//! the name, addresses with the preferred one first, phone numbers, the
//! organization and a photo. A patch replaces only the fields it names,
//! and a field is replaced only when its value changed, so Apple's
//! grouped labels and every property nobody here reads stay as they were.
//! A photo named by URL is reported but never fetched here.

use base64::Engine;
use calcard::vcard::{VCard, VCardEntry, VCardProperty, VCardValue};

use crate::{DavError, MOST_RESOURCE_BYTES};

/// The largest photo kept. A contact photo shows at 64 pixels.
pub const MOST_PHOTO_BYTES: usize = 512 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Card {
    pub uid: Option<String>,
    pub name: Option<String>,
    /// The preferred address first.
    pub emails: Vec<String>,
    pub phones: Vec<String>,
    pub organization: Option<String>,
    pub photo: Option<Photo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Photo {
    /// Served at this URL.
    Uri(String),
    /// Carried in the card.
    Inline(Vec<u8>),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CardFields {
    pub name: Option<String>,
    pub emails: Option<Vec<String>>,
    pub phones: Option<Vec<String>>,
    pub organization: Option<String>,
}

fn parse(text: &str) -> Result<VCard, DavError> {
    if text.len() > MOST_RESOURCE_BYTES {
        return Err(DavError::TooLarge(MOST_RESOURCE_BYTES));
    }
    VCard::parse(text).map_err(|other| DavError::Parse(format!("not a vCard: {other:?}")))
}

fn texts(entry: &VCardEntry) -> Vec<String> {
    entry
        .values
        .iter()
        .flat_map(|value| match value {
            VCardValue::Component(parts) => parts.clone(),
            other => other.as_text().map(|t| vec![t.to_string()]).unwrap_or_default(),
        })
        .collect()
}

fn first_text(entry: &VCardEntry) -> Option<String> {
    texts(entry).into_iter().map(|t| t.trim().to_string()).find(|t| !t.is_empty())
}

/// Whether the line marks itself preferred: `TYPE=pref` in 3.0, `PREF=1`
/// in 4.0. Read off the written line's parameters, which both carry.
fn preferred(entry: &VCardEntry) -> bool {
    let mut line = String::new();
    let _ = entry.write_to(&mut line, true);
    line.split(':').next().is_some_and(|head| head.to_ascii_uppercase().contains("PREF"))
}

fn of(card: &VCard, property: VCardProperty) -> impl Iterator<Item = &VCardEntry> {
    card.entries.iter().filter(move |e| e.name == property)
}

fn listed(card: &VCard, property: VCardProperty) -> Vec<String> {
    let (mut first, mut rest): (Vec<&VCardEntry>, Vec<&VCardEntry>) = of(card, property).partition(|e| preferred(e));
    first.append(&mut rest);
    first.into_iter().filter_map(first_text).collect()
}

fn photo(card: &VCard) -> Option<Photo> {
    let entry = of(card, VCardProperty::Photo).next()?;
    let photo = match entry.values.first()? {
        VCardValue::Binary(data) => Photo::Inline(data.data.clone()),
        value => {
            let text = value.as_text()?.trim().to_string();
            match text.strip_prefix("data:") {
                Some(rest) => {
                    let (_, encoded) = rest.split_once(',')?;
                    // Refuse before decoding: base64 is 4 bytes per 3.
                    if encoded.len() > MOST_PHOTO_BYTES / 3 * 4 + 4 {
                        return None;
                    }
                    Photo::Inline(base64::engine::general_purpose::STANDARD.decode(encoded.trim()).ok()?)
                }
                None if text.starts_with("https://") || text.starts_with("http://") => Photo::Uri(text),
                None => return None,
            }
        }
    };
    match &photo {
        Photo::Inline(bytes) if bytes.len() > MOST_PHOTO_BYTES => None,
        _ => Some(photo),
    }
}

pub fn read_card(text: &str) -> Result<Card, DavError> {
    let card = parse(text)?;
    let name = of(&card, VCardProperty::Fn).next().and_then(first_text).or_else(|| {
        // N is family;given;additional;prefix;suffix.
        let parts = texts(of(&card, VCardProperty::N).next()?);
        let joined = [parts.get(1), parts.first()]
            .into_iter()
            .flatten()
            .filter(|p| !p.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        (!joined.is_empty()).then_some(joined)
    });
    Ok(Card {
        uid: card.uid().map(str::to_string),
        name,
        emails: listed(&card, VCardProperty::Email),
        phones: listed(&card, VCardProperty::Tel),
        organization: of(&card, VCardProperty::Org).next().and_then(first_text),
        photo: photo(&card),
    })
}

/// Text escaped for a vCard value.
fn escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace(';', "\\;").replace(',', "\\,").replace('\n', "\\n")
}

/// The N value for a free-form name: the last word as the family name,
/// the rest as given names, as most address books split one.
fn structured(name: &str) -> String {
    match name.trim().rsplit_once(' ') {
        Some((given, family)) => format!("{};{};;;", escape(family), escape(given)),
        None => format!("{};;;;", escape(name.trim())),
    }
}

fn lines_for(fields: &CardFields) -> Vec<(VCardProperty, Vec<String>)> {
    let mut out = Vec::new();
    if let Some(name) = &fields.name {
        out.push((VCardProperty::Fn, vec![format!("FN:{}", escape(name))]));
        out.push((VCardProperty::N, vec![format!("N:{}", structured(name))]));
    }
    if let Some(emails) = &fields.emails {
        out.push((VCardProperty::Email, emails.iter().map(|e| format!("EMAIL;TYPE=INTERNET:{}", e.trim())).collect()));
    }
    if let Some(phones) = &fields.phones {
        out.push((VCardProperty::Tel, phones.iter().map(|p| format!("TEL:{}", escape(p))).collect()));
    }
    if let Some(org) = &fields.organization {
        out.push((VCardProperty::Org, vec![format!("ORG:{}", escape(org))]));
    }
    out
}

pub fn new_card(uid: &str, fields: &CardFields) -> String {
    let mut text = format!("BEGIN:VCARD\r\nVERSION:3.0\r\nPRODID:-//Penguin Mail//EN\r\nUID:{uid}\r\n");
    if fields.name.is_none() {
        // FN is required in every vCard version.
        let fallback = fields.emails.as_ref().and_then(|e| e.first()).cloned().unwrap_or_default();
        text.push_str(&format!("FN:{}\r\n", escape(&fallback)));
    }
    for (_, lines) in lines_for(fields) {
        for line in lines {
            text.push_str(&line);
            text.push_str("\r\n");
        }
    }
    text.push_str("END:VCARD\r\n");
    text
}

/// The entries `lines` hold, read inside a card of the same version.
fn entries(lines: &[String], v4: bool) -> Result<Vec<VCardEntry>, DavError> {
    if lines.is_empty() {
        return Ok(Vec::new());
    }
    let version = if v4 { "4.0" } else { "3.0" };
    let card = parse(&format!("BEGIN:VCARD\r\nVERSION:{version}\r\n{}\r\nEND:VCARD\r\n", lines.join("\r\n")))?;
    Ok(card.entries.into_iter().filter(|e| e.name != VCardProperty::Version).collect())
}

pub fn patch_card(existing: &str, fields: &CardFields) -> Result<String, DavError> {
    let mut card = parse(existing)?;
    let before = read_card(existing)?;
    let v4 = card.version().is_some_and(|v| v as u8 >= 40);
    let unchanged = |property: &VCardProperty| match property {
        VCardProperty::Fn | VCardProperty::N => fields.name == before.name,
        VCardProperty::Email => fields.emails.as_ref() == Some(&before.emails),
        VCardProperty::Tel => fields.phones.as_ref() == Some(&before.phones),
        VCardProperty::Org => fields.organization == before.organization,
        _ => true,
    };
    for (property, lines) in lines_for(fields) {
        if unchanged(&property) {
            continue;
        }
        let new = entries(&lines, v4)?;
        // The first line of the property keeps its place in the card.
        let at = card.entries.iter().position(|e| e.name == property).unwrap_or(card.entries.len());
        card.entries.retain(|e| e.name != property);
        let at = at.min(card.entries.len());
        for (offset, entry) in new.into_iter().enumerate() {
            card.entries.insert(at + offset, entry);
        }
    }
    Ok(card.to_string())
}
