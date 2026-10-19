//! A Google client a person made in their own Google Cloud project and
//! names to Penguin Mail in two environment variables, for when the
//! built-in client cannot sign in. An account signed in with one keeps
//! it in the keyring, beside its refresh token, and refreshes through it
//! from then on, whether or not the variables are set at a later start.

use std::sync::Arc;

use mailrs_domain::AccountId;
use mailrs_domain::translate::{fill, gettext};
use mailrs_gmail::OAuthClient;
use serde::{Deserialize, Serialize};

use crate::passwords::{PasswordError, PasswordStore, Secrets};

/// The variable naming the own client's id. A release build reads the
/// same name at build time for the built-in client.
pub const ID_VAR: &str = "PENGUIN_MAIL_GOOGLE_CLIENT_ID";
/// The variable naming the own client's secret.
pub const SECRET_VAR: &str = "PENGUIN_MAIL_GOOGLE_CLIENT_SECRET";

/// An own Google client's id and secret. No `Debug`, so neither reaches
/// a log line.
#[derive(Clone, Serialize, Deserialize)]
pub struct OwnClient {
    client_id: String,
    client_secret: String,
    /// The Google Cloud project the key belongs to, when the file named it.
    #[serde(default)]
    project_id: Option<String>,
}

impl OwnClient {
    pub fn new(client_id: impl Into<String>, client_secret: impl Into<String>) -> Self {
        OwnClient {
            client_id: client_id.into(),
            client_secret: client_secret.into(),
            project_id: None,
        }
    }

    /// The client's id, as Google prints it.
    pub fn id(&self) -> &str {
        &self.client_id
    }

    /// The project the key file named, if any.
    pub fn project_id(&self) -> Option<&str> {
        self.project_id.as_deref()
    }

    /// The client to sign in and refresh tokens with.
    pub fn oauth(&self) -> OAuthClient {
        OAuthClient::new(&self.client_id, &self.client_secret)
    }

    /// The text the keyring keeps for an account.
    pub fn encode(&self) -> String {
        // Two strings always serialize.
        serde_json::to_string(self).unwrap_or_default()
    }

    /// The client from what the keyring kept, or `None` when the entry
    /// is not one this module wrote.
    pub fn decode(kept: &str) -> Option<OwnClient> {
        let own: OwnClient = serde_json::from_str(kept).ok()?;
        let blank = own.client_id.trim().is_empty() || own.client_secret.trim().is_empty();
        (!blank).then_some(own)
    }
}

/// The two variables do not name a client.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OwnClientError {
    /// One is set and the other is not. Penguin Mail says so rather than
    /// sign in with the built-in client the person meant to avoid.
    #[error("{}", half(.set, .missing))]
    Half {
        set: &'static str,
        missing: &'static str,
    },
}

fn half(set: &str, missing: &str) -> String {
    fill(
        &gettext(
            "{set} is set but {missing} is not. Set both to sign in with your own Google client, \
             or neither to use Penguin Mail's.",
        ),
        &[("set", set), ("missing", missing)],
    )
}

/// The keyring user the person's project sits under. SQLite row ids start
/// at 1, so no account shares it.
pub const PROJECT_KEY: AccountId = 0;

/// The project the person set up for an earlier account, if one is kept.
/// A keyring that fails, or an entry this version cannot read, counts as
/// none.
pub async fn saved_project(secrets: &Secrets) -> Option<OwnClient> {
    let store = Arc::clone(&secrets.google_project);
    let kept = tokio::task::spawn_blocking(move || store.load(PROJECT_KEY))
        .await
        .map_err(|err| err.to_string())
        .and_then(|read| read.map_err(|err| err.to_string()));
    match kept {
        Ok(kept) => kept.as_deref().and_then(OwnClient::decode),
        Err(reason) => {
            tracing::warn!(%reason, "could not read the kept Google project");
            None
        }
    }
}

/// Keeps `own` as the person's project, for any later Google account.
pub async fn keep_project(secrets: &Secrets, own: &OwnClient) -> Result<(), PasswordError> {
    let store = Arc::clone(&secrets.google_project);
    let encoded = own.encode();
    tokio::task::spawn_blocking(move || store.save(PROJECT_KEY, &encoded))
        .await
        .map_err(|err| PasswordError::Keyring(err.to_string()))?
}

/// The own client two values name: `None` when neither is set, an error
/// when only one is. A blank value counts as unset.
pub fn from_values(
    id: Option<&str>,
    secret: Option<&str>,
) -> Option<Result<OwnClient, OwnClientError>> {
    let id = id.map(str::trim).filter(|v| !v.is_empty());
    let secret = secret.map(str::trim).filter(|v| !v.is_empty());
    match (id, secret) {
        (None, None) => None,
        (Some(id), Some(secret)) => Some(Ok(OwnClient::new(id, secret))),
        (Some(_), None) => Some(Err(OwnClientError::Half {
            set: ID_VAR,
            missing: SECRET_VAR,
        })),
        (None, Some(_)) => Some(Err(OwnClientError::Half {
            set: SECRET_VAR,
            missing: ID_VAR,
        })),
    }
}

/// The own client the environment names now, read at each sign-in.
pub fn from_env() -> Option<Result<OwnClient, OwnClientError>> {
    let id = std::env::var(ID_VAR).ok();
    let secret = std::env::var(SECRET_VAR).ok();
    from_values(id.as_deref(), secret.as_deref())
}

/// The largest key file read. Google's is under 1 KiB.
pub const FILE_LIMIT: usize = 64 * 1024;

/// What every client id Google makes ends with.
const ID_SUFFIX: &str = ".apps.googleusercontent.com";

/// Why a key file or a pasted pair does not make a client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FileProblem {
    /// A Web application key, which a desktop sign-in cannot use.
    #[error("{}", words(FileProblem::Website))]
    Website,
    /// Not the JSON file Google gives for a desktop key.
    #[error("{}", words(FileProblem::NotTheFile))]
    NotTheFile,
    /// The file holds an empty secret.
    #[error("{}", words(FileProblem::NoSecret))]
    NoSecret,
    /// One pasted value, or none.
    #[error("{}", words(FileProblem::Half))]
    Half,
    /// A pasted id that does not end the way Google's ids do.
    #[error("{}", words(FileProblem::WrongId))]
    WrongId,
    /// The file could not be read at all, as one without permission.
    #[error("{}", words(FileProblem::Unreadable))]
    Unreadable,
}

fn words(problem: FileProblem) -> String {
    match problem {
        FileProblem::Website => {
            // This is a button on Google's page; use the words Google's page shows in your language.
            gettext("This key is for a website. Go back to step 5 and choose Desktop app.")
        }
        FileProblem::NotTheFile => gettext("This isn't the file from step 5."),
        FileProblem::NoSecret => gettext(
            "This file has no secret. Make a new key in step 5 and download it straight away.",
        ),
        FileProblem::Half => {
            // This is a button on Google's page; use the words Google's page shows in your language.
            gettext("Paste both the Client ID and the Client secret")
        }
        FileProblem::WrongId => {
            // This is a button on Google's page; use the words Google's page shows in your language.
            gettext("Copy the Client ID from step 5 again. It ends in .apps.googleusercontent.com.")
        }
        // Translators: said under the key file a person chose in the Google project setup.
        FileProblem::Unreadable => gettext("Penguin Mail can't open this file. Choose another."),
    }
}

/// The client in the file Google gives for a desktop key. Reads at most
/// `FILE_LIMIT` bytes and ignores every key but the id, secret and project.
pub fn read_file(bytes: &[u8]) -> Result<OwnClient, FileProblem> {
    if bytes.len() > FILE_LIMIT {
        return Err(FileProblem::NotTheFile);
    }
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| FileProblem::NotTheFile)?;
    if value.get("web").is_some_and(serde_json::Value::is_object) {
        return Err(FileProblem::Website);
    }
    let installed = value
        .get("installed")
        .and_then(serde_json::Value::as_object)
        .ok_or(FileProblem::NotTheFile)?;
    let text = |key: &str| installed.get(key).and_then(serde_json::Value::as_str);
    let id = text("client_id").map(str::trim).unwrap_or_default();
    if !id.ends_with(ID_SUFFIX) {
        return Err(FileProblem::NotTheFile);
    }
    let secret = text("client_secret").map(str::trim).unwrap_or_default();
    if secret.is_empty() {
        return Err(FileProblem::NoSecret);
    }
    let mut own = OwnClient::new(id, secret);
    own.project_id = text("project_id")
        .map(str::trim)
        .filter(|project| !project.is_empty())
        .map(str::to_owned);
    Ok(own)
}

/// The client two pasted values name. Each loses its whitespace and one
/// pair of surrounding quotes, as a JSON view shows them.
pub fn from_pasted(id: &str, secret: &str) -> Result<OwnClient, FileProblem> {
    let unquote = |value: &str| {
        let value = value.trim();
        value
            .strip_prefix('"')
            .and_then(|inner| inner.strip_suffix('"'))
            .unwrap_or(value)
            .to_owned()
    };
    let (id, secret) = (unquote(id), unquote(secret));
    let own = match from_values(Some(&id), Some(&secret)) {
        Some(Ok(own)) => own,
        Some(Err(_)) | None => return Err(FileProblem::Half),
    };
    if own.id().ends_with(ID_SUFFIX) {
        Ok(own)
    } else {
        Err(FileProblem::WrongId)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn a_kept_project_reads_back() {
        let secrets = Secrets::memory();
        let own = OwnClient::new("123-abc.apps.googleusercontent.com", "GOCSPX-abc");
        keep_project(&secrets, &own).await.unwrap();
        let back = saved_project(&secrets).await.unwrap();
        assert_eq!(back.id(), "123-abc.apps.googleusercontent.com");
        // Compared as a bool so a failure never prints the secret.
        assert!(back.encode() == own.encode());
    }

    #[tokio::test]
    async fn no_project_kept_is_none() {
        assert!(saved_project(&Secrets::memory()).await.is_none());
    }

    #[tokio::test]
    async fn a_project_entry_that_does_not_decode_is_none() {
        let secrets = Secrets::memory();
        secrets
            .google_project
            .save(PROJECT_KEY, "not json")
            .unwrap();
        assert!(saved_project(&secrets).await.is_none());
    }

    #[test]
    fn both_values_make_a_client() {
        let own = from_values(Some("own.apps.googleusercontent.com"), Some("GOCSPX-own"))
            .expect("both are set")
            .expect("both are valid");
        assert_eq!(own.oauth().id(), "own.apps.googleusercontent.com");
    }

    #[test]
    fn neither_value_means_no_own_client() {
        assert!(from_values(None, None).is_none());
    }

    #[test]
    fn blank_values_count_as_unset() {
        assert!(from_values(Some(" "), Some("")).is_none());
    }

    #[test]
    fn an_id_without_a_secret_names_the_missing_secret() {
        let Some(Err(half)) = from_values(Some("own.apps.googleusercontent.com"), None) else {
            panic!("one value alone is an error");
        };
        assert_eq!(
            half,
            OwnClientError::Half {
                set: ID_VAR,
                missing: SECRET_VAR
            }
        );
    }

    #[test]
    fn a_secret_without_an_id_names_the_missing_id() {
        let Some(Err(half)) = from_values(Some(""), Some("GOCSPX-own")) else {
            panic!("one value alone is an error");
        };
        assert_eq!(
            half,
            OwnClientError::Half {
                set: SECRET_VAR,
                missing: ID_VAR
            }
        );
    }

    #[test]
    fn the_error_names_both_variables() {
        let half = OwnClientError::Half {
            set: ID_VAR,
            missing: SECRET_VAR,
        };
        assert_eq!(
            half.to_string(),
            "PENGUIN_MAIL_GOOGLE_CLIENT_ID is set but PENGUIN_MAIL_GOOGLE_CLIENT_SECRET is not. \
             Set both to sign in with your own Google client, or neither to use Penguin Mail's."
        );
    }

    #[test]
    fn a_kept_client_reads_back_as_it_was() {
        let own = OwnClient::new("own.apps.googleusercontent.com", "GOCSPX-own");
        let back = OwnClient::decode(&own.encode()).expect("it reads back");
        assert_eq!(back.oauth().id(), "own.apps.googleusercontent.com");
        assert_eq!(back.encode(), own.encode());
    }

    #[test]
    fn a_kept_entry_that_does_not_parse_is_no_client() {
        assert!(OwnClient::decode("not json").is_none());
        assert!(OwnClient::decode(r#"{"client_id":"","client_secret":"x"}"#).is_none());
    }

    const DESKTOP: &str = r#"{"installed":{"client_id":"123-abc.apps.googleusercontent.com","project_id":"penguin-mail-4711","auth_uri":"https://accounts.google.com/o/oauth2/auth","token_uri":"https://oauth2.googleapis.com/token","auth_provider_x509_cert_url":"https://www.googleapis.com/oauth2/v1/certs","client_secret":"GOCSPX-abc","redirect_uris":["http://localhost"]}}"#;

    #[test]
    fn a_desktop_file_gives_its_id_and_secret() {
        let own = read_file(DESKTOP.as_bytes()).unwrap();
        assert_eq!(own.id(), "123-abc.apps.googleusercontent.com");
        assert_eq!(own.oauth().id(), "123-abc.apps.googleusercontent.com");
    }

    #[test]
    fn a_desktop_file_gives_its_project() {
        let own = read_file(DESKTOP.as_bytes()).unwrap();
        assert_eq!(own.project_id(), Some("penguin-mail-4711"));
    }

    #[test]
    fn a_kept_entry_from_before_the_project_still_decodes() {
        let kept = r#"{"client_id":"own.apps.googleusercontent.com","client_secret":"GOCSPX-own"}"#;
        let own = OwnClient::decode(kept).expect("an old entry decodes");
        assert_eq!(own.project_id(), None);
    }

    #[test]
    fn a_file_with_a_byte_order_mark_still_reads() {
        let bytes = [b"\xEF\xBB\xBF".as_slice(), DESKTOP.as_bytes()].concat();
        assert!(read_file(&bytes).is_ok());
    }

    #[test]
    fn a_web_file_is_refused_as_a_website_key() {
        let web = DESKTOP.replace("\"installed\"", "\"web\"");
        assert_eq!(read_file(web.as_bytes()).err(), Some(FileProblem::Website));
    }

    #[test]
    fn a_service_account_file_is_not_the_file() {
        let file = r#"{"type":"service_account","project_id":"p","private_key":"x"}"#;
        assert_eq!(
            read_file(file.as_bytes()).err(),
            Some(FileProblem::NotTheFile)
        );
    }

    #[test]
    fn invalid_json_is_not_the_file() {
        assert_eq!(
            read_file(b"{\"installed\":").err(),
            Some(FileProblem::NotTheFile)
        );
    }

    #[test]
    fn an_id_from_elsewhere_is_not_the_file() {
        let file = DESKTOP.replace(".apps.googleusercontent.com", ".example.com");
        assert_eq!(
            read_file(file.as_bytes()).err(),
            Some(FileProblem::NotTheFile)
        );
    }

    #[test]
    fn an_empty_secret_says_the_file_has_none() {
        let file = DESKTOP.replace("GOCSPX-abc", "");
        assert_eq!(
            read_file(file.as_bytes()).err(),
            Some(FileProblem::NoSecret)
        );
    }

    #[test]
    fn a_file_over_the_limit_is_refused_unread() {
        let big = vec![b' '; FILE_LIMIT + 1];
        assert_eq!(read_file(&big).err(), Some(FileProblem::NotTheFile));
    }

    #[test]
    fn half_a_pasted_pair_asks_for_both() {
        assert_eq!(
            from_pasted("123-abc.apps.googleusercontent.com", " ").err(),
            Some(FileProblem::Half)
        );
    }

    #[test]
    fn nothing_pasted_asks_for_both() {
        assert_eq!(from_pasted("", "\n").err(), Some(FileProblem::Half));
    }

    #[test]
    fn pasted_values_lose_whitespace_and_quotes() {
        let own = from_pasted(
            " \"123-abc.apps.googleusercontent.com\"\n",
            "\"GOCSPX-abc\" ",
        )
        .unwrap();
        assert_eq!(own.id(), "123-abc.apps.googleusercontent.com");
    }

    #[test]
    fn a_pasted_id_from_elsewhere_is_refused() {
        assert_eq!(
            from_pasted("123-abc", "GOCSPX-abc").err(),
            Some(FileProblem::WrongId)
        );
    }

    #[test]
    fn the_problems_never_name_the_environment() {
        for problem in [
            FileProblem::Website,
            FileProblem::NotTheFile,
            FileProblem::NoSecret,
            FileProblem::Half,
            FileProblem::WrongId,
        ] {
            assert!(!problem.to_string().contains("PENGUIN_MAIL"), "{problem}");
        }
    }

    #[test]
    fn the_problems_use_the_words_of_the_setup() {
        assert_eq!(
            FileProblem::NotTheFile.to_string(),
            "This isn't the file from step 5."
        );
        assert_eq!(
            FileProblem::Half.to_string(),
            "Paste both the Client ID and the Client secret"
        );
    }
}
