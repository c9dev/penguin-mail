//! Google's People API: the contacts one account has written down.
//!
//! The reply is deeper than the app needs, with every field wrapped in a
//! list and marked primary or not, so this module flattens one page into
//! [`Person`] values and leaves the HTTP to [`crate::GmailClient`].

use serde::Deserialize;
use serde_json::json;

use crate::GmailError;

/// Read the account's contacts, and nothing else. Sign-in no longer asks
/// for this one on its own: [`CONTACTS_WRITE_SCOPE`] covers it.
/// [`crate::Granted::has`] still checks it, so an account that signed in
/// before this change and kept only this narrower grant still reads
/// contacts, though it cannot write them.
pub const CONTACTS_SCOPE: &str = "https://www.googleapis.com/auth/contacts.readonly";

/// Read and change the account's contacts. [`crate::FULL_SCOPES`] asks
/// for this one rather than [`CONTACTS_SCOPE`]: it covers reading too,
/// and asking the narrower scope first would mean a second consent the
/// first time the assistant adds or changes a contact.
pub const CONTACTS_WRITE_SCOPE: &str = "https://www.googleapis.com/auth/contacts";

pub const PEOPLE_API_BASE: &str = "https://people.googleapis.com/v1";

/// The fields one page carries. Anything not named here comes back empty,
/// so the request says exactly what the app stores and no more.
pub const PERSON_FIELDS: &str = "names,emailAddresses,photos,organizations,phoneNumbers,metadata,birthdays,biographies,addresses,urls,events,userDefined,memberships";

/// People per page. Google's ceiling is 1000; a smaller page costs one
/// more call on a large address book and keeps each reply small.
pub const PAGE_SIZE: u32 = 200;

/// The square Google is asked to cut each photo to. Google resizes on its
/// side, so the bytes that arrive are already avatar-sized and the app
/// stores them as they come.
pub const PHOTO_PIXELS: u32 = 128;

/// One person in an account's contacts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Person {
    /// Google's name for this person, such as `people/c17`.
    pub resource: String,
    pub details: mailrs_domain::contact::Details,
    pub name: Option<String>,
    /// Every address, the primary one first.
    pub emails: Vec<String>,
    /// Where Google serves the photo, already asked for at
    /// [`PHOTO_PIXELS`]. Google's grey silhouette is not a photo, so a
    /// contact who never set one has `None` here.
    pub photo_url: Option<String>,
    pub organization: Option<String>,
    pub phone: Option<String>,
}

/// One page of `people.connections.list`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConnectionsPage {
    pub people: Vec<Person>,
    /// Contacts the owner deleted since the sync token was issued.
    pub deleted: Vec<String>,
    /// Set while more pages follow.
    pub next_page_token: Option<String>,
    /// Google sends this with the last page. Handing it back next time
    /// asks for what changed instead of the whole address book.
    pub next_sync_token: Option<String>,
}

/// What adding or changing a contact writes. When changing one, a field
/// left `None` stays as Google holds it, and a list given replaces the
/// whole list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContactFields {
    pub expected_revision: Option<String>,
    pub expected_groups: Option<Vec<String>>,
    /// A stable UUID lets a CardDAV editor retry a partially completed creation.
    pub creation_key: Option<String>,
    pub address_book: Option<String>,
    pub photo: Option<Vec<u8>>,
    pub details: mailrs_domain::contact::Changes,
    pub name: Option<String>,
    pub emails: Option<Vec<String>>,
    pub phones: Option<Vec<String>>,
    pub organization: Option<String>,
}

impl ContactFields {
    /// The person as the People API takes one, with only the fields set.
    /// Google splits the free-form name into given and family names
    /// itself, which suits names that do not split on the last space.
    pub fn body(&self) -> serde_json::Value {
        let mut body = serde_json::Map::new();
        if let Some(name) = &self.name {
            body.insert("names".into(), json!([{"unstructuredName": name}]));
        }
        if let Some(emails) = &self.emails {
            let values: Vec<_> = emails.iter().map(|e| json!({"value": e})).collect();
            body.insert("emailAddresses".into(), values.into());
        }
        if let Some(phones) = &self.phones {
            let values: Vec<_> = phones.iter().map(|p| json!({"value": p})).collect();
            body.insert("phoneNumbers".into(), values.into());
        }
        if let Some(organization) = &self.organization {
            body.insert("organizations".into(), json!([{"name": organization}]));
        }
        let d = &self.details;
        if let Some(value) = &d.birthday {
            body.insert(
                "birthdays".into(),
                if value.is_empty() {
                    json!([])
                } else {
                    json!([{"date": date_value(value)}])
                },
            );
        }
        if let Some(value) = &d.notes {
            body.insert(
                "biographies".into(),
                if value.is_empty() {
                    json!([])
                } else {
                    json!([{"value": value, "contentType": "TEXT_PLAIN"}])
                },
            );
        }
        if let Some(values) = &d.urls {
            body.insert(
                "urls".into(),
                json!(
                    values
                        .iter()
                        .map(|v| json!({"value":v}))
                        .collect::<Vec<_>>()
                ),
            );
        }
        if let Some(values) = &d.addresses {
            body.insert("addresses".into(), json!(values.iter().map(|v| {
                let parts: Vec<_> = v.split('\n').collect();
                let get = |i| parts.get(i).copied().unwrap_or_default();
                json!({"poBox":get(0),"extendedAddress":get(1),"streetAddress":get(2),"city":get(3),"region":get(4),"postalCode":get(5),"country":get(6)})
            }).collect::<Vec<_>>()));
        }
        if let Some(values) = &d.dates {
            body.insert(
                "events".into(),
                json!(
                    values
                        .iter()
                        .map(|v| json!({"type":v.label,"date":date_value(&v.value)}))
                        .collect::<Vec<_>>()
                ),
            );
        }
        if let Some(values) = &d.custom {
            body.insert(
                "userDefined".into(),
                json!(
                    values
                        .iter()
                        .map(|v| json!({"key":v.label,"value":v.value}))
                        .collect::<Vec<_>>()
                ),
            );
        }
        body.into()
    }

    /// The fields a change names, as `updatePersonFields` lists them.
    pub fn mask(&self) -> String {
        [
            (self.name.is_some(), "names"),
            (self.emails.is_some(), "emailAddresses"),
            (self.phones.is_some(), "phoneNumbers"),
            (self.organization.is_some(), "organizations"),
            (self.details.birthday.is_some(), "birthdays"),
            (self.details.notes.is_some(), "biographies"),
            (self.details.addresses.is_some(), "addresses"),
            (self.details.urls.is_some(), "urls"),
            (self.details.dates.is_some(), "events"),
            (self.details.custom.is_some(), "userDefined"),
            (self.details.groups.is_some(), "memberships"),
        ]
        .iter()
        .filter(|(set, _)| *set)
        .map(|(_, field)| *field)
        .collect::<Vec<_>>()
        .join(",")
    }

    /// Whether the change names no field at all.
    pub fn is_empty(&self) -> bool {
        self.mask().is_empty() && self.details == Default::default() && self.photo.is_none()
    }
}

/// One person as `people.get`, `createContact` and `updateContact` send
/// them back, with the etag a later change must hand in.
pub fn parse_person(body: &str) -> Result<(Person, String), GmailError> {
    let reply: Connection =
        serde_json::from_str(body).map_err(|e| GmailError::Decode(e.to_string()))?;
    let etag = reply.etag.clone().unwrap_or_default();
    Ok((flatten(reply), etag))
}

/// Reads one page, including contacts with no email address.
pub fn parse_connections(body: &str) -> Result<ConnectionsPage, GmailError> {
    let reply: Connections =
        serde_json::from_str(body).map_err(|e| GmailError::Decode(e.to_string()))?;
    let mut page = ConnectionsPage {
        next_page_token: reply.next_page_token,
        next_sync_token: reply.next_sync_token,
        ..ConnectionsPage::default()
    };
    for person in reply.connections {
        if person.resource_name.is_empty() {
            continue;
        }
        if person.metadata.as_ref().is_some_and(|m| m.deleted) {
            page.deleted.push(person.resource_name);
            continue;
        }
        let person = flatten(person);
        page.people.push(person);
    }
    Ok(page)
}

fn flatten(person: Connection) -> Person {
    Person {
        name: primary(&person.names, |n| n.display_name.as_deref()),
        emails: addresses(&person.email_addresses),
        photo_url: person
            .photos
            .iter()
            // `default` marks Google's grey silhouette, which says this
            // contact has no photo of their own.
            .filter(|p| !p.default)
            .find_map(|p| p.url.as_deref())
            .map(|url| photo_url(url, PHOTO_PIXELS)),
        organization: primary(&person.organizations, |o| o.name.as_deref()),
        phone: primary(&person.phone_numbers, |p| p.value.as_deref()),
        details: mailrs_domain::contact::Details {
            phones: person
                .phone_numbers
                .iter()
                .filter_map(|p| p.value.clone())
                .collect(),
            revision: person.etag.clone().unwrap_or_default(),
            birthday: person
                .extra
                .get("birthdays")
                .and_then(|v| v.as_array())
                .and_then(|v| v.first())
                .map(|v| date_text(&v["date"]))
                .unwrap_or_default(),
            notes: values(&person.extra, "biographies")
                .iter()
                .filter_map(|v| v["value"].as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            urls: values(&person.extra, "urls")
                .iter()
                .filter_map(|v| v["value"].as_str().map(str::to_owned))
                .collect(),
            addresses: values(&person.extra, "addresses")
                .iter()
                .map(|v| {
                    [
                        "poBox",
                        "extendedAddress",
                        "streetAddress",
                        "city",
                        "region",
                        "postalCode",
                        "country",
                    ]
                    .map(|k| v[k].as_str().unwrap_or_default())
                    .join("\n")
                })
                .collect(),
            dates: values(&person.extra, "events")
                .iter()
                .map(|v| mailrs_domain::contact::Field {
                    label: v["type"].as_str().unwrap_or("other").into(),
                    value: date_text(&v["date"]),
                })
                .collect(),
            custom: values(&person.extra, "userDefined")
                .iter()
                .map(|v| mailrs_domain::contact::Field {
                    label: v["key"].as_str().unwrap_or_default().into(),
                    value: v["value"].as_str().unwrap_or_default().into(),
                })
                .collect(),
            groups: values(&person.extra, "memberships")
                .iter()
                .filter_map(|v| {
                    v["contactGroupMembership"]["contactGroupResourceName"]
                        .as_str()
                        .map(str::to_owned)
                })
                .collect(),
            ..Default::default()
        },
        resource: person.resource_name,
    }
}

/// The same photo at `pixels` square, cropped to the face. Google serves
/// any size from one URL, so asking for the avatar size means the bytes
/// never have to be scaled down here.
pub fn photo_url(url: &str, pixels: u32) -> String {
    let base = url.rsplit_once('=').map_or(url, |(head, tail)| {
        if tail.starts_with('s') || tail.starts_with('w') || tail.starts_with('c') {
            head
        } else {
            url
        }
    });
    format!("{base}=s{pixels}-c")
}

/// Every address, the primary one first and the rest as Google listed them.
fn addresses(fields: &[Field]) -> Vec<String> {
    let mut emails: Vec<&str> = Vec::new();
    for (primary, value) in fields
        .iter()
        .filter_map(|f| Some((f.is_primary(), f.value.as_deref()?.trim())))
        .filter(|(_, value)| value.contains('@'))
    {
        if primary {
            emails.insert(0, value);
        } else {
            emails.push(value);
        }
    }
    let mut seen = Vec::new();
    for email in emails {
        if !seen.iter().any(|e: &String| e.eq_ignore_ascii_case(email)) {
            seen.push(email.to_string());
        }
    }
    seen
}

/// The primary entry's value, or the first one with anything in it.
fn primary<T: Marked>(fields: &[T], value: impl Fn(&T) -> Option<&str>) -> Option<String> {
    let pick = |wanted: bool| {
        fields
            .iter()
            .filter(|f| f.is_primary() == wanted)
            .find_map(|f| value(f).map(str::trim).filter(|v| !v.is_empty()))
    };
    pick(true).or_else(|| pick(false)).map(str::to_string)
}

trait Marked {
    fn is_primary(&self) -> bool;
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Connections {
    #[serde(default)]
    connections: Vec<Connection>,
    next_page_token: Option<String>,
    next_sync_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Connection {
    #[serde(default)]
    resource_name: String,
    etag: Option<String>,
    metadata: Option<PersonMetadata>,
    #[serde(default)]
    names: Vec<Name>,
    #[serde(default)]
    email_addresses: Vec<Field>,
    #[serde(default)]
    photos: Vec<Photo>,
    #[serde(default)]
    organizations: Vec<Organization>,
    #[serde(default)]
    phone_numbers: Vec<Field>,
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Deserialize)]
struct PersonMetadata {
    #[serde(default)]
    deleted: bool,
}

#[derive(Default, Deserialize)]
struct FieldMetadata {
    #[serde(default)]
    primary: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Name {
    #[serde(default)]
    metadata: FieldMetadata,
    display_name: Option<String>,
}

#[derive(Deserialize)]
struct Field {
    #[serde(default)]
    metadata: FieldMetadata,
    value: Option<String>,
}

#[derive(Deserialize)]
struct Photo {
    url: Option<String>,
    /// Set on Google's stand-in silhouette.
    #[serde(default)]
    default: bool,
}

#[derive(Deserialize)]
struct Organization {
    #[serde(default)]
    metadata: FieldMetadata,
    name: Option<String>,
}

impl Marked for Name {
    fn is_primary(&self) -> bool {
        self.metadata.primary
    }
}

impl Marked for Field {
    fn is_primary(&self) -> bool {
        self.metadata.primary
    }
}

impl Marked for Organization {
    fn is_primary(&self) -> bool {
        self.metadata.primary
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A page as the People API sends it, with a contact who has several
    /// addresses, one with no photo, and a page token for what follows.
    const FIRST_PAGE: &str = include_str!("../tests/people/connections-page-1.json");
    const LAST_PAGE: &str = include_str!("../tests/people/connections-page-2.json");
    const CHANGED: &str = include_str!("../tests/people/connections-changed.json");

    #[test]
    fn a_page_flattens_into_people() {
        let page = parse_connections(FIRST_PAGE).unwrap();
        assert_eq!(page.next_page_token.as_deref(), Some("page-2"));
        assert_eq!(page.next_sync_token, None);
        assert_eq!(page.people.len(), 2);

        let mara = &page.people[0];
        assert_eq!(mara.resource, "people/c1");
        assert_eq!(mara.name.as_deref(), Some("Mara Okafor"));
        // The primary address comes first whatever order Google sent.
        assert_eq!(
            mara.emails,
            ["mara.okafor@example.org", "mara@fernwood.example"]
        );
        assert_eq!(mara.organization.as_deref(), Some("Fernwood"));
        assert_eq!(mara.phone.as_deref(), Some("+1 555 0100"));
        assert_eq!(
            mara.photo_url.as_deref(),
            Some("https://lh3.googleusercontent.com/mara=s128-c")
        );

        let theo = &page.people[1];
        assert_eq!(theo.name.as_deref(), Some("Theo Lang"));
        assert_eq!(theo.emails, ["theo@example.org"]);
        // Google's silhouette is not a photo.
        assert_eq!(theo.photo_url, None);
        assert_eq!(theo.organization, None);
    }

    #[test]
    fn the_last_page_carries_the_sync_token() {
        let page = parse_connections(LAST_PAGE).unwrap();
        assert_eq!(page.next_page_token, None);
        assert_eq!(page.next_sync_token.as_deref(), Some("sync-token-1"));
        // A contact without email still belongs in the address book window.
        assert_eq!(page.people.len(), 2);
        assert_eq!(page.people[0].resource, "people/c3");
    }

    #[test]
    fn a_sync_token_reply_names_what_was_deleted() {
        let page = parse_connections(CHANGED).unwrap();
        assert_eq!(page.deleted, ["people/c2"]);
        assert_eq!(page.people.len(), 1);
        assert_eq!(page.people[0].emails, ["mara.okafor@example.org"]);
        assert_eq!(page.next_sync_token.as_deref(), Some("sync-token-2"));
    }

    #[test]
    fn an_empty_address_book_parses() {
        let page = parse_connections("{\"totalPeople\":0}").unwrap();
        assert_eq!(page, ConnectionsPage::default());
        assert!(parse_connections("not json").is_err());
    }

    #[test]
    fn a_contact_writes_only_the_fields_it_names() {
        let fields = ContactFields {
            name: Some("Priya Shah".into()),
            phones: Some(vec!["+351 21 000 0000".into()]),
            ..ContactFields::default()
        };
        assert_eq!(
            fields.body(),
            json!({
                "names": [{"unstructuredName": "Priya Shah"}],
                "phoneNumbers": [{"value": "+351 21 000 0000"}],
            })
        );
        assert_eq!(fields.mask(), "names,phoneNumbers");
        assert!(ContactFields::default().is_empty());
    }

    #[test]
    fn one_person_parses_with_its_etag() {
        let (person, etag) = parse_person(
            r#"{"resourceName":"people/c9","etag":"%Ej4","names":[{"displayName":"Priya Shah"}],"emailAddresses":[{"value":"priya@example.org"}]}"#,
        )
        .unwrap();
        assert_eq!(person.resource, "people/c9");
        assert_eq!(person.name.as_deref(), Some("Priya Shah"));
        assert_eq!(person.emails, ["priya@example.org"]);
        assert_eq!(etag, "%Ej4");
    }

    #[test]
    fn photos_are_asked_for_at_the_size_they_are_shown() {
        assert_eq!(
            photo_url("https://x.example/p=s100", 128),
            "https://x.example/p=s128-c"
        );
        assert_eq!(
            photo_url("https://x.example/p", 64),
            "https://x.example/p=s64-c"
        );
        assert_eq!(
            photo_url("https://x.example/p=w200-h200", 64),
            "https://x.example/p=s64-c"
        );
    }
}

fn values<'a>(
    extra: &'a serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> &'a [serde_json::Value] {
    extra
        .get(key)
        .and_then(|v| v.as_array())
        .map(Vec::as_slice)
        .unwrap_or_default()
}

fn date_text(date: &serde_json::Value) -> String {
    let year = date["year"].as_u64().unwrap_or_default();
    let month = date["month"].as_u64().unwrap_or_default();
    let day = date["day"].as_u64().unwrap_or_default();
    if month == 0 || day == 0 {
        return String::new();
    }
    if year == 0 {
        format!("--{month:02}-{day:02}")
    } else {
        format!("{year:04}-{month:02}-{day:02}")
    }
}

fn date_value(text: &str) -> serde_json::Value {
    let full = if text.starts_with("--") {
        format!("2000{}", &text[1..])
    } else {
        text.to_owned()
    };
    let date = chrono::NaiveDate::parse_from_str(&full, "%Y-%m-%d")
        .or_else(|_| chrono::NaiveDate::parse_from_str(&full.replace('-', ""), "%Y%m%d"));
    match date {
        Ok(date) => {
            use chrono::Datelike;
            json!({"year":if text.starts_with("--") {0} else {date.year()},"month":date.month(),"day":date.day()})
        }
        Err(_) => serde_json::Value::Null,
    }
}

pub(crate) fn validate(fields: &ContactFields) -> Result<(), GmailError> {
    use mailrs_domain::translate::gettext;
    for date in fields
        .details
        .birthday
        .iter()
        .chain(fields.details.dates.iter().flatten().map(|v| &v.value))
    {
        if !date.is_empty() && date_value(date).is_null() {
            return Err(GmailError::Decode(gettext(
                "Enter a valid date as YYYY-MM-DD or --MM-DD.",
            )));
        }
    }
    if fields.photo.as_ref().is_some_and(|p| {
        p.len() > 512 * 1024 || (!p.is_empty() && !p.starts_with(b"\x89PNG\r\n\x1a\n"))
    }) {
        return Err(GmailError::Decode(gettext(
            "Choose a PNG contact photo smaller than 512 KB.",
        )));
    }
    Ok(())
}

/// Retain labels and structured names when an edit changes only their values.
pub(crate) fn preserve_fields(body: &mut serde_json::Value, current: &serde_json::Value) {
    for key in [
        "emailAddresses",
        "phoneNumbers",
        "organizations",
        "addresses",
        "urls",
        "events",
        "userDefined",
    ] {
        let Some(changed) = body.get_mut(key).and_then(|v| v.as_array_mut()) else {
            continue;
        };
        let Some(old) = current[key].as_array() else {
            continue;
        };
        let mut used = vec![false; old.len()];
        for (i, value) in changed.iter_mut().enumerate() {
            let matching = old.iter().enumerate().position(|(j, candidate)| {
                !used[j]
                    && value
                        .as_object()
                        .is_some_and(|v| v.iter().all(|(k, v)| candidate.get(k) == Some(v)))
            });
            let index = matching.or_else(|| (i < old.len() && !used[i]).then_some(i));
            if let Some(index) = index {
                used[index] = true;
                let mut kept = old[index].clone();
                if let Some(object) = kept.as_object_mut() {
                    object.remove("metadata");
                    if key == "addresses" && value.get("country") != object.get("country") {
                        object.remove("countryCode");
                    }
                    object.extend(value.as_object().cloned().unwrap_or_default());
                    *value = kept;
                }
            }
        }
    }
}

#[cfg(test)]
mod expanded_tests {
    use super::*;
    #[test]
    fn invalid_dates_and_photos_are_rejected_before_sending() {
        for value in ["2023-02-29", "--13-01", "broken"] {
            let mut fields = ContactFields::default();
            fields.details.birthday = Some(value.into());
            assert!(validate(&fields).is_err());
        }
        let fields = ContactFields {
            photo: Some(vec![0; 32]),
            ..Default::default()
        };
        assert!(validate(&fields).is_err());
        assert_eq!(date_value("--0229"), json!({"year":0,"month":2,"day":29}));
    }
}

#[cfg(test)]
mod address_tests {
    use super::*;
    #[test]
    fn changing_a_country_does_not_keep_the_old_country_code() {
        let current = json!({"addresses":[{"country":"Portugal","countryCode":"PT","type":"work","city":"Lisbon"}]});
        let mut body = json!({"addresses":[{"country":"Czechia","city":"Prague"}]});
        preserve_fields(&mut body, &current);
        assert_eq!(
            body,
            json!({"addresses":[{"country":"Czechia","city":"Prague","type":"work"}]})
        );
        assert!(body.get("phoneNumbers").is_none());
    }
}
