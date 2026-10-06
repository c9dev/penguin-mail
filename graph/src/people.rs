//! Contacts. Graph keeps them in folders; a delta runs per folder, and the
//! adapter merges them into one address book.

use serde::Deserialize;
use serde_json::Value;

use crate::error::GraphError;
use crate::http::{DeltaPage, Graph, Method, Page};
use crate::model::{EmailAddress, Removed};

const PAGE_PREFER: &str = "odata.maxpagesize=50";
const CONTACT_FIELDS: &str = "id,displayName,givenName,surname,emailAddresses,companyName,mobilePhone,businessPhones,homePhones,parentFolderId";

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct ContactFolder {
    pub id: String,
    pub display_name: String,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct GraphContact {
    pub id: String,
    pub display_name: Option<String>,
    pub given_name: Option<String>,
    pub surname: Option<String>,
    pub email_addresses: Vec<EmailAddress>,
    pub company_name: Option<String>,
    pub mobile_phone: Option<String>,
    pub business_phones: Vec<String>,
    pub home_phones: Vec<String>,
    pub parent_folder_id: Option<String>,
    #[serde(rename = "@removed")]
    pub removed: Option<Removed>,
}

impl Graph {
    /// The folders below the default one. The default "Contacts" folder is
    /// not among them; see [`Graph::default_contact_folder`].
    pub async fn contact_folders(&self) -> Result<Vec<ContactFolder>, GraphError> {
        self.get_all("me/contactFolders", &[("$top", "100")]).await
    }

    /// The default folder's id, read off one of its contacts, or `None`
    /// while it holds none.
    pub async fn default_contact_folder(&self) -> Result<Option<String>, GraphError> {
        let page: Page<GraphContact> = self
            .get(
                "me/contacts",
                &[("$top", "1"), ("$select", "parentFolderId")],
            )
            .await?;
        Ok(page
            .value
            .into_iter()
            .next()
            .and_then(|c| c.parent_folder_id))
    }

    pub async fn contact_delta(
        &self,
        folder: &str,
        link: Option<&str>,
    ) -> Result<DeltaPage<GraphContact>, GraphError> {
        if let Some(link) = link {
            return self.follow(link, &[PAGE_PREFER]).await;
        }
        self.get_with(
            &format!("me/contactFolders/{folder}/contacts/delta"),
            &[("$select", CONTACT_FIELDS)],
            &[PAGE_PREFER],
        )
        .await
    }

    /// The contact's photo, `None` when it has none.
    pub async fn contact_photo(
        &self,
        id: &str,
        limit: usize,
    ) -> Result<Option<Vec<u8>>, GraphError> {
        match self
            .get_bytes(&format!("me/contacts/{id}/photo/$value"), limit)
            .await
        {
            Ok(bytes) => Ok(Some(bytes)),
            Err(GraphError::NotFound) => Ok(None),
            Err(err) => Err(err),
        }
    }

    pub async fn create_contact(&self, body: &Value) -> Result<GraphContact, GraphError> {
        self.send(Method::Post, "me/contacts", Some(body), &[])
            .await?
            .ok_or_else(|| GraphError::Decode("no contact in the answer".into()))
    }

    /// One contact, with only its display name filled in.
    pub async fn contact_name(&self, id: &str) -> Result<GraphContact, GraphError> {
        self.get(&format!("me/contacts/{id}"), &[("$select", "displayName")]).await
    }

    pub async fn update_contact(&self, id: &str, body: &Value) -> Result<GraphContact, GraphError> {
        self.send(Method::Patch, &format!("me/contacts/{id}"), Some(body), &[])
            .await?
            .ok_or_else(|| GraphError::Decode("no contact in the answer".into()))
    }
}
