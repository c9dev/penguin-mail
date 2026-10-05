//! The fake's contacts: folders, contacts and their log.

use mailrs_graph::{
    ContactFolder, DeltaPage, EmailAddress, GraphContact, GraphError, Removed,
};
use serde_json::Value;

use super::{Answer, Area, FakeGraph, GraphState, Logged, Round};

impl FakeGraph {
    /// Stores `contact` in `folder` and logs it.
    pub fn put_contact(&self, folder: &str, contact: GraphContact) {
        self.with(|s| {
            let id = contact.id.clone();
            let contact = GraphContact { parent_folder_id: Some(folder.to_string()), ..contact };
            s.contacts.insert(id.clone(), (folder.to_string(), contact));
            log(s, folder, &id);
        });
    }

    /// Erases a contact, as another client does.
    pub fn remove_contact(&self, id: &str) {
        self.with(|s| {
            if let Some((folder, _)) = s.contacts.remove(id) {
                log(s, &folder, id);
            }
        });
    }
}

fn log(s: &mut GraphState, folder: &str, id: &str) {
    let seq = s.next_seq();
    s.contact_log.push(Logged { seq, place: folder.to_string(), id: id.to_string() });
}

/// The default folder: the first, which `contact_folders` leaves out.
fn default_folder(s: &GraphState) -> Option<&ContactFolder> {
    s.contact_folders.first()
}

/// Writes the fields a create or update body sets onto `contact`.
fn read_body(contact: &mut GraphContact, body: &Value) {
    if let Some(name) = body["displayName"].as_str() {
        contact.display_name = Some(name.into());
    }
    if let Some(name) = body["givenName"].as_str() {
        contact.given_name = Some(name.into());
    }
    if let Some(name) = body["surname"].as_str() {
        contact.surname = Some(name.into());
    }
    if let Some(addresses) = body.get("emailAddresses").and_then(|v| serde_json::from_value::<Vec<EmailAddress>>(v.clone()).ok()) {
        contact.email_addresses = addresses;
    }
    if let Some(phone) = body["mobilePhone"].as_str() {
        contact.mobile_phone = Some(phone.into());
    }
    if let Some(company) = body["companyName"].as_str() {
        contact.company_name = Some(company.into());
    }
}

pub(super) fn contact_folders(s: &mut GraphState) -> Answer<Vec<ContactFolder>> {
    s.refuses(Area::Contacts)?;
    Ok(s.contact_folders.iter().skip(1).cloned().collect())
}

/// Graph shows the default folder's id only through a contact in it.
pub(super) fn default_contact_folder(s: &mut GraphState) -> Answer<Option<String>> {
    s.refuses(Area::Contacts)?;
    let Some(default) = default_folder(s) else { return Ok(None) };
    Ok(s.contacts.values().any(|(f, _)| *f == default.id).then(|| default.id.clone()))
}

pub(super) fn contact_delta(
    s: &mut GraphState,
    folder: &str,
    link_text: Option<&str>,
) -> Answer<DeltaPage<GraphContact>> {
    s.refuses(Area::Contacts)?;
    if !s.contact_folders.iter().any(|f| f.id == folder) {
        return Err(GraphError::NotFound);
    }
    let s = &*s;
    Round { log: &s.contact_log, expired_before: s.expired_before, now: s.seq, place: folder }.run(
        link_text,
        || s.contacts.values().filter(|(f, _)| f == folder).map(|(_, c)| c.clone()).collect(),
        |id| s.contacts.get(id).filter(|(f, _)| f == folder).map(|(_, c)| c.clone()),
        |id| GraphContact {
            id: id.to_string(),
            removed: Some(Removed { reason: Some("deleted".into()) }),
            ..GraphContact::default()
        },
    )
}

pub(super) fn contact_photo(s: &mut GraphState, id: &str, limit: usize) -> Answer<Option<Vec<u8>>> {
    s.refuses(Area::Contacts)?;
    match s.photos.get(id) {
        Some(bytes) if bytes.len() > limit => Err(GraphError::TooLarge { limit }),
        found => Ok(found.cloned()),
    }
}

pub(super) fn create_contact(s: &mut GraphState, body: &Value) -> Answer<GraphContact> {
    s.refuses(Area::Contacts)?;
    let folder = default_folder(s).map(|f| f.id.clone()).ok_or(GraphError::NotFound)?;
    let id = s.new_id("contact");
    let mut contact = GraphContact { id: id.clone(), parent_folder_id: Some(folder.clone()), ..GraphContact::default() };
    read_body(&mut contact, body);
    s.contacts.insert(id.clone(), (folder.clone(), contact.clone()));
    log(s, &folder, &id);
    Ok(contact)
}

pub(super) fn contact_name(s: &mut GraphState, id: &str) -> Answer<GraphContact> {
    s.refuses(Area::Contacts)?;
    let (_, contact) = s.contacts.get(id).ok_or(GraphError::NotFound)?;
    Ok(GraphContact { id: id.into(), display_name: contact.display_name.clone(), ..GraphContact::default() })
}

pub(super) fn update_contact(s: &mut GraphState, id: &str, body: &Value) -> Answer<GraphContact> {
    s.refuses(Area::Contacts)?;
    let (folder, contact) = s.contacts.get_mut(id).ok_or(GraphError::NotFound)?;
    read_body(contact, body);
    // Outlook.com rebuilds the display name from the name parts, or the
    // company when there are none, on a change that leaves it out. Graph's
    // contact reference says to send it with every update to keep it.
    if body.get("displayName").is_none() {
        let parts = [contact.given_name.as_deref(), contact.surname.as_deref()].into_iter().flatten().collect::<Vec<_>>().join(" ");
        let rebuilt = Some(parts).filter(|p| !p.is_empty()).or_else(|| contact.company_name.clone());
        if rebuilt.is_some() {
            contact.display_name = rebuilt;
        }
    }
    let (folder, contact) = (folder.clone(), contact.clone());
    log(s, &folder, id);
    Ok(contact)
}

#[cfg(test)]
mod tests {
    use mailrs_graph::GraphContact;

    use crate::fake::FakeGraph;
    use crate::services::microsoft::GraphApi;

    #[tokio::test]
    async fn a_contact_delta_names_a_removal_by_id() {
        let fake = FakeGraph::new();
        fake.put_contact("contacts-1", GraphContact { id: "c1".into(), ..GraphContact::default() });
        let first = fake.contact_delta("contacts-1", None).await.unwrap();
        assert_eq!(first.value.len(), 1);
        let link = first.delta_link.unwrap();
        fake.remove_contact("c1");
        let next = fake.contact_delta("contacts-1", Some(&link)).await.unwrap();
        assert!(next.value[0].removed.is_some() && next.value[0].id == "c1");
    }

    #[tokio::test]
    async fn the_default_folder_is_known_once_it_holds_a_contact() {
        let fake = FakeGraph::new();
        assert_eq!(fake.default_contact_folder().await.unwrap(), None);
        fake.create_contact(&serde_json::json!({"displayName": "Ann", "emailAddresses": [{"address": "ann@example.com"}]}))
            .await
            .unwrap();
        assert_eq!(fake.default_contact_folder().await.unwrap().as_deref(), Some("contacts-1"));
        assert!(fake.contact_folders().await.unwrap().is_empty(), "the default folder is not listed");
    }
}
