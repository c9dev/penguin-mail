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
pub const MOST_PHOTO_BYTES: usize = mailrs_domain::contact::MOST_PHOTO_BYTES;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Card {
    pub uid: Option<String>,
    pub details: mailrs_domain::contact::Details,
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
    pub details: mailrs_domain::contact::Changes,
    pub photo: Option<Vec<u8>>,
    pub name: Option<String>,
    pub emails: Option<Vec<String>>,
    pub phones: Option<Vec<String>>,
    pub organization: Option<String>,
}

fn parse(text: &str) -> Result<VCard, DavError> {
    if text.len() > MOST_RESOURCE_BYTES {
        return Err(DavError::TooLarge(MOST_RESOURCE_BYTES));
    }
    let mut card =
        VCard::parse(text).map_err(|other| DavError::Parse(format!("not a vCard: {other:?}")))?;
    // calcard truncates dashed dates without a year. Restore date-only values
    // from the source so unrelated edits cannot lose a component.
    let unfolded = text
        .replace("\r\n ", "")
        .replace("\r\n\t", "")
        .replace("\n ", "")
        .replace("\n\t", "");
    let mut dates = unfolded.lines().filter_map(|line| {
        let (head, value) = line.rsplit_once(':')?;
        let name = head.split(';').next()?.rsplit('.').next()?;
        let property = match name.to_ascii_uppercase().as_str() {
            "BDAY" => VCardProperty::Bday,
            "ANNIVERSARY" => VCardProperty::Anniversary,
            _ => return None,
        };
        Some((property, value.trim()))
    });
    let v4 = card.version().is_some_and(|v| v as u8 >= 40);
    for entry in &mut card.entries {
        if matches!(entry.name, VCardProperty::Bday | VCardProperty::Anniversary)
            && let Some((property, value)) = dates.next()
            && property == entry.name
            && mailrs_domain::contact::is_date(value)
            // Apple writes year 1604 for a birthday without one. Keep its
            // line as written so an unrelated edit does not rewrite it.
            && !value.starts_with("1604")
        {
            let mut value = mailrs_domain::contact::date_for_edit(value);
            if v4 {
                value = match value.strip_prefix("--") {
                    Some(rest) => format!("--{}", rest.replace('-', "")),
                    None => value.replace('-', ""),
                };
            }
            entry.values = vec![VCardValue::Text(value)];
        }
        if let VCardProperty::Other(name) = &mut entry.name {
            name.make_ascii_uppercase();
        }
    }
    Ok(card)
}

fn texts(entry: &VCardEntry) -> Vec<String> {
    entry
        .values
        .iter()
        .flat_map(|value| match value {
            VCardValue::Component(parts) => parts.clone(),
            other => other
                .as_text()
                .map(|t| vec![t.to_string()])
                .unwrap_or_default(),
        })
        .collect()
}

fn first_text(entry: &VCardEntry) -> Option<String> {
    texts(entry)
        .into_iter()
        .map(|t| t.trim().to_string())
        .find(|t| !t.is_empty())
}

/// Whether the line marks itself preferred: `TYPE=pref` in 3.0, `PREF=1`
/// in 4.0. Read off the written line's parameters, which both carry.
fn preferred(entry: &VCardEntry) -> bool {
    let mut line = String::new();
    let _ = entry.write_to(&mut line, true);
    line.split(':')
        .next()
        .is_some_and(|head| head.to_ascii_uppercase().contains("PREF"))
}

fn of(card: &VCard, property: VCardProperty) -> impl Iterator<Item = &VCardEntry> {
    card.entries.iter().filter(move |e| e.name == property)
}

fn listed(card: &VCard, property: VCardProperty) -> Vec<String> {
    let (mut first, mut rest): (Vec<&VCardEntry>, Vec<&VCardEntry>) =
        of(card, property).partition(|e| preferred(e));
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
                    Photo::Inline(
                        base64::engine::general_purpose::STANDARD
                            .decode(encoded.trim())
                            .ok()?,
                    )
                }
                None if text.starts_with("https://") || text.starts_with("http://") => {
                    Photo::Uri(text)
                }
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
    let name = of(&card, VCardProperty::Fn)
        .next()
        .and_then(first_text)
        .or_else(|| {
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
        details: details(&card),
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
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace('\n', "\\n")
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
        out.push((
            VCardProperty::Email,
            emails
                .iter()
                .map(|e| format!("EMAIL;TYPE=INTERNET:{}", escape(e.trim())))
                .collect(),
        ));
    }
    if let Some(phones) = &fields.phones {
        out.push((
            VCardProperty::Tel,
            phones
                .iter()
                .map(|p| format!("TEL:{}", escape(p)))
                .collect(),
        ));
    }
    if let Some(org) = &fields.organization {
        out.push((VCardProperty::Org, vec![format!("ORG:{}", escape(org))]));
    }
    extra_lines(fields, &mut out);
    out
}

pub fn new_card(uid: &str, fields: &CardFields) -> String {
    let mut text =
        format!("BEGIN:VCARD\r\nVERSION:3.0\r\nPRODID:-//Penguin Mail//EN\r\nUID:{uid}\r\n");
    if fields.name.is_none() {
        // FN is required in every vCard version.
        let fallback = fields
            .emails
            .as_ref()
            .and_then(|e| e.first())
            .cloned()
            .unwrap_or_default();
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
    let card = parse(&format!(
        "BEGIN:VCARD\r\nVERSION:{version}\r\n{}\r\nEND:VCARD\r\n",
        lines.join("\r\n")
    ))?;
    Ok(card
        .entries
        .into_iter()
        .filter(|e| e.name != VCardProperty::Version)
        .collect())
}

pub fn patch_card(existing: &str, fields: &CardFields) -> Result<String, DavError> {
    validate(fields)?;
    let mut card = parse(existing)?;
    let before = read_card(existing)?;
    let v4 = card.version().is_some_and(|v| v as u8 >= 40);
    let unchanged = |property: &VCardProperty| match property {
        VCardProperty::Fn | VCardProperty::N => fields.name == before.name,
        VCardProperty::Email => fields.emails.as_ref() == Some(&before.emails),
        VCardProperty::Tel => fields.phones.as_ref() == Some(&before.phones),
        VCardProperty::Org => fields.organization == before.organization,
        _ => false,
    };
    if let Some(custom) = &fields.details.custom {
        let removed: Vec<_> = before
            .details
            .custom
            .iter()
            // A label that is no property name lives in the labelled form,
            // dropped below; "X-ABDATE" there must not take the dates along.
            .filter(|old| x_name(&old.label) && !custom.iter().any(|new| new.label == old.label))
            .map(|old| old.label.clone())
            .collect();
        card.entries.retain(|e| {
            !removed
                .iter()
                .any(|name| e.name == VCardProperty::Other(name.clone()))
        });
    }
    for (touched, labelled) in [
        (fields.details.dates.is_some(), "X-ABDATE"),
        (fields.details.custom.is_some(), LABELLED),
    ] {
        if !touched {
            continue;
        }
        let labelled = VCardProperty::Other(labelled.into());
        let groups: Vec<_> = of(&card, labelled.clone())
            .filter_map(|e| e.group.clone())
            .collect();
        card.entries.retain(|e| {
            e.name != labelled
                && !(e.name == VCardProperty::Other("X-ABLABEL".into())
                    && e.group.as_ref().is_some_and(|g| groups.contains(g)))
        });
    }
    for (property, mut lines) in lines_for(fields) {
        if v4 && property == VCardProperty::Photo {
            lines = fields
                .photo
                .iter()
                .filter(|b| !b.is_empty())
                .map(|b| {
                    format!(
                        "PHOTO:data:image/png;base64,{}",
                        base64::engine::general_purpose::STANDARD.encode(b)
                    )
                })
                .collect();
        }
        if unchanged(&property) {
            continue;
        }
        let mut new = entries(&lines, v4)?;
        let mut renamed = std::collections::HashMap::new();
        for entry in &mut new {
            if let Some(group) = &entry.group
                && (group.starts_with("pmdate") || group.starts_with("pmfield"))
            {
                let replacement = renamed.entry(group.clone()).or_insert_with(|| {
                    let mut name = group.clone();
                    while card.entries.iter().any(|e| e.group.as_ref() == Some(&name)) {
                        name.push('x');
                    }
                    name
                });
                entry.group = Some(replacement.clone());
            }
        }
        // Reuse entries whose value survived, including their TYPE and Apple labels.
        let old: Vec<_> = card
            .entries
            .iter()
            .filter(|e| e.name == property)
            .cloned()
            .collect();
        let mut used = vec![false; old.len()];
        let mut matched = vec![false; new.len()];
        for (index, entry) in new.iter_mut().enumerate() {
            if let Some(i) = old
                .iter()
                .enumerate()
                .position(|(i, e)| !used[i] && e.values == entry.values)
            {
                *entry = old[i].clone();
                used[i] = true;
                matched[index] = true;
            }
        }
        // A changed value in the same position retains its label and type too.
        for (i, entry) in new.iter_mut().enumerate() {
            if !matched[i]
                && old.get(i).is_some()
                && !used[i]
                && matches!(
                    property,
                    VCardProperty::Email
                        | VCardProperty::Tel
                        | VCardProperty::Adr
                        | VCardProperty::Url
                )
            {
                entry.group = old[i].group.clone();
                entry.params = old[i].params.clone();
                used[i] = true;
            }
        }
        // The first line of the property keeps its place in the card.
        let at = card
            .entries
            .iter()
            .position(|e| e.name == property)
            .unwrap_or(card.entries.len());
        card.entries.retain(|e| e.name != property);
        let at = at.min(card.entries.len());
        for (offset, entry) in new.into_iter().enumerate() {
            card.entries.insert(at + offset, entry);
        }
    }
    let written = card.to_string();
    parse(&written)?;
    Ok(written)
}

fn written_value(entry: &VCardEntry) -> String {
    let mut line = String::new();
    let _ = entry.write_to(&mut line, true);
    line.split_once(':')
        .map(|(_, v)| v.trim().to_string())
        .unwrap_or_default()
}

fn details(card: &VCard) -> mailrs_domain::contact::Details {
    use mailrs_domain::contact::{Details, Field};
    Details {
        revision: String::new(),
        read_only: false,
        uid: card.uid().unwrap_or_default().to_string(),
        members: is_group(card).then(|| {
            card.entries
                .iter()
                .filter(|e| {
                    e.name == VCardProperty::Member
                        || e.name == VCardProperty::Other("X-ADDRESSBOOKSERVER-MEMBER".into())
                })
                .filter_map(first_text)
                .map(|v| v.strip_prefix("urn:uuid:").unwrap_or(&v).to_string())
                .collect()
        }),
        phones: listed(card, VCardProperty::Tel),
        birthday: of(card, VCardProperty::Bday)
            .next()
            .map(|entry| mailrs_domain::contact::date_for_edit(&written_value(entry)))
            .unwrap_or_default(),
        notes: listed(card, VCardProperty::Note).join("\n"),
        addresses: of(card, VCardProperty::Adr)
            .map(|e| {
                std::array::from_fn(|i| match e.values.get(i) {
                    Some(VCardValue::Component(parts)) => parts.join(", "),
                    Some(value) => value.as_text().unwrap_or_default().to_string(),
                    None => String::new(),
                })
            })
            .collect(),
        urls: listed(card, VCardProperty::Url),
        // Some exporters escape a complete category list as one text value.
        groups: of(card, VCardProperty::Categories)
            .flat_map(texts)
            .flat_map(|value| {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .fold(Vec::new(), |mut groups, group| {
                if !groups.contains(&group) {
                    groups.push(group);
                }
                groups
            }),
        dates: card
            .entries
            .iter()
            .filter_map(|e| {
                if e.name == VCardProperty::Anniversary {
                    Some(Field {
                        label: "Anniversary".into(),
                        value: mailrs_domain::contact::date_for_edit(&written_value(e)),
                    })
                } else if e.name == VCardProperty::Other("X-ABDATE".into()) {
                    let label = card
                        .entries
                        .iter()
                        .find(|label| {
                            label.name == VCardProperty::Other("X-ABLABEL".into())
                                && label.group == e.group
                        })
                        .and_then(first_text)
                        .unwrap_or_default();
                    Some(Field {
                        label,
                        value: first_text(e).unwrap_or_default(),
                    })
                } else {
                    None
                }
            })
            .collect(),
        custom: card
            .entries
            .iter()
            .filter_map(|e| match &e.name {
                VCardProperty::Other(name) if name == LABELLED => {
                    let label = card
                        .entries
                        .iter()
                        .find(|label| {
                            label.name == VCardProperty::Other("X-ABLABEL".into())
                                && label.group == e.group
                        })
                        .and_then(first_text)?;
                    first_text(e).map(|value| Field { label, value })
                }
                VCardProperty::Other(name) if editable_custom(name) => {
                    first_text(e).map(|value| Field {
                        label: name.clone(),
                        value,
                    })
                }
                _ => None,
            })
            .collect(),
    }
}

fn extra_lines(fields: &CardFields, out: &mut Vec<(VCardProperty, Vec<String>)>) {
    let d = &fields.details;
    for (property, value) in [
        (VCardProperty::Bday, &d.birthday),
        (VCardProperty::Note, &d.notes),
    ] {
        if let Some(value) = value {
            let lines = if value.is_empty() {
                vec![]
            } else {
                vec![format!("{}:{}", property.as_str(), escape(value))]
            };
            out.push((property, lines));
        }
    }
    if let Some(values) = &d.addresses {
        out.push((
            VCardProperty::Adr,
            values
                .iter()
                .map(|parts| format!("ADR:{}", parts.each_ref().map(|p| escape(p)).join(";")))
                .collect(),
        ));
    }
    if let Some(values) = &d.urls {
        out.push((
            VCardProperty::Url,
            values
                .iter()
                .map(|s| format!("URL:{}", escape(s)))
                .collect(),
        ));
    }
    if let Some(values) = &d.groups {
        out.push((
            VCardProperty::Categories,
            if values.is_empty() {
                vec![]
            } else {
                vec![format!(
                    "CATEGORIES:{}",
                    values
                        .iter()
                        .map(|s| escape(s))
                        .collect::<Vec<_>>()
                        .join(",")
                )]
            },
        ));
    }
    if let Some(values) = &d.dates {
        out.push((
            VCardProperty::Anniversary,
            values
                .iter()
                .enumerate()
                .flat_map(|(i, s)| {
                    if s.label == "Anniversary" {
                        vec![format!("ANNIVERSARY:{}", escape(&s.value))]
                    } else {
                        vec![
                            format!("pmdate{i}.X-ABDATE:{}", escape(&s.value)),
                            format!("pmdate{i}.X-ABLABEL:{}", escape(&s.label)),
                        ]
                    }
                })
                .collect(),
        ));
    }
    if let Some(values) = &d.custom {
        // One entry per name, since each entry replaces every line of its
        // property: a card may carry two X-SOCIALPROFILE lines.
        let mut by_name: Vec<(&str, Vec<String>)> = Vec::new();
        // A label a person typed ("Badge") is no property name, so it goes
        // in Apple's labelled form, as dates do.
        let mut labelled = Vec::new();
        for (i, field) in values.iter().enumerate() {
            if !x_name(&field.label) {
                if !field.value.is_empty() {
                    labelled.push(format!("pmfield{i}.{LABELLED}:{}", escape(&field.value)));
                    labelled.push(format!("pmfield{i}.X-ABLABEL:{}", escape(&field.label)));
                }
                continue;
            }
            let line = (!field.value.is_empty())
                .then(|| format!("{}:{}", field.label, escape(&field.value)));
            match by_name.iter_mut().find(|(name, _)| *name == field.label) {
                Some((_, lines)) => lines.extend(line),
                None => by_name.push((&field.label, line.into_iter().collect())),
            }
        }
        for (name, lines) in by_name {
            out.push((VCardProperty::Other(name.to_string()), lines));
        }
        out.push((VCardProperty::Other(LABELLED.into()), labelled));
    }
    if let Some(bytes) = &fields.photo {
        out.push((
            VCardProperty::Photo,
            if bytes.is_empty() {
                vec![]
            } else {
                vec![format!(
                    "PHOTO;ENCODING=b;TYPE=PNG:{}",
                    base64::engine::general_purpose::STANDARD.encode(bytes)
                )]
            },
        ));
    }
}

/// The property that holds a custom field under a label a person typed.
const LABELLED: &str = "X-PENGUIN-FIELD";

fn editable_custom(name: &str) -> bool {
    name.starts_with("X-")
        && name != LABELLED
        && !name.starts_with("X-AB")
        && !name.starts_with("X-ADDRESSBOOKSERVER-")
}

/// Whether a custom field's label can be its own property name.
fn x_name(name: &str) -> bool {
    editable_custom(name)
        && name.len() >= 3
        && name
            .bytes()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'-')
}

/// Reject property-name injection and malformed dates before a network write.
pub fn validate(fields: &CardFields) -> Result<(), DavError> {
    mailrs_domain::contact::check(&fields.details, fields.photo.as_deref())
        .map_err(DavError::Invalid)?;
    Ok(())
}

fn is_group(card: &VCard) -> bool {
    card.entries.iter().any(|e| {
        (e.name == VCardProperty::Kind
            || e.name == VCardProperty::Other("X-ADDRESSBOOKSERVER-KIND".into()))
            && first_text(e).is_some_and(|v| v.eq_ignore_ascii_case("group"))
    })
}

pub fn new_group(uid: &str, name: &str, member: &str) -> String {
    format!(
        "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:{}\r\nFN:{}\r\nX-ADDRESSBOOKSERVER-KIND:group\r\nX-ADDRESSBOOKSERVER-MEMBER:urn:uuid:{}\r\nEND:VCARD\r\n",
        escape(uid),
        escape(name),
        escape(member)
    )
}

pub fn group_membership(text: &str, uid: &str, include: bool) -> Result<String, DavError> {
    let mut card = parse(text)?;
    if !is_group(&card) {
        return Err(DavError::Parse("not a group".into()));
    }
    let member = format!("urn:uuid:{uid}");
    card.entries.retain(|e| {
        !((e.name == VCardProperty::Member
            || e.name == VCardProperty::Other("X-ADDRESSBOOKSERVER-MEMBER".into()))
            && first_text(e).is_some_and(|v| v == member))
    });
    if include {
        let v4 = card.version().is_some_and(|v| v as u8 >= 40);
        let property = if v4 {
            "MEMBER"
        } else {
            "X-ADDRESSBOOKSERVER-MEMBER"
        };
        card.entries
            .extend(entries(&[format!("{property}:{}", escape(&member))], v4)?);
    }
    Ok(card.to_string())
}
