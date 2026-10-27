//! Online Accounts over D-Bus: `org.gnome.OnlineAccounts` on the session
//! bus. The daemon starts on demand, so a desktop without it costs nothing
//! until a Google sign-in asks.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use futures::future::BoxFuture;
use futures::stream::{BoxStream, select_all};
use futures::{FutureExt, StreamExt};
use zbus::{Connection, MatchRule, MessageStream, OwnedObjectPath, OwnedVariant};

use super::{GoaAccount, GoaChange, GoaError, GoaToken, OnlineAccounts, bounded};

const SERVICE: &str = "org.gnome.OnlineAccounts";
const ROOT: &str = "/org/gnome/OnlineAccounts";
const ACCOUNT: &str = "org.gnome.OnlineAccounts.Account";
const OAUTH2: &str = "org.gnome.OnlineAccounts.OAuth2Based";

/// Online Accounts on a session bus.
pub struct DbusAccounts {
    /// A bus address for tests; the session bus when `None`.
    address: Option<String>,
    /// The bus connection once dialed. A call that finds the bus gone
    /// drops it, so the next call dials again instead of failing until
    /// the app restarts.
    conn: Mutex<Option<Connection>>,
}

impl DbusAccounts {
    pub fn session() -> Self {
        Self {
            address: None,
            conn: Mutex::new(None),
        }
    }

    /// Online Accounts on the bus at `address`.
    pub fn at(address: &str) -> Self {
        Self {
            address: Some(address.to_string()),
            conn: Mutex::new(None),
        }
    }

    async fn connection(&self) -> Result<Connection, GoaError> {
        if let Some(conn) = self.cached() {
            return Ok(conn);
        }
        let conn = match &self.address {
            Some(address) => {
                zbus::connection::Builder::address(address.as_str())
                    .build()
                    .await
            }
            None => Connection::session().await,
        }
        .map_err(error_of)?;
        *self.conn.lock().unwrap_or_else(|e| e.into_inner()) = Some(conn.clone());
        Ok(conn)
    }

    fn cached(&self) -> Option<Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// `result`, after forgetting the connection when the bus could not be
    /// reached: the session bus may have restarted since it was dialed.
    fn settle<T>(&self, result: Result<T, GoaError>) -> Result<T, GoaError> {
        if matches!(result, Err(GoaError::Unreachable(_))) {
            *self.conn.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
        result
    }
}

/// What a failed call means to the caller.
fn error_of(err: zbus::Error) -> GoaError {
    use zbus::{DBusError, Error};
    // zbus reports the standard org.freedesktop.DBus.Error replies as
    // `FDO`, and every other error reply as `MethodError`.
    let reply = match &err {
        Error::MethodError(name, text, _) => {
            Some((name.as_str().to_string(), text.clone().unwrap_or_default()))
        }
        Error::FDO(fdo) => Some((
            fdo.name().as_str().to_string(),
            fdo.description().unwrap_or_default().to_string(),
        )),
        _ => None,
    };
    if let Some((name, text)) = reply {
        return match name.as_str() {
            "org.freedesktop.DBus.Error.ServiceUnknown"
            | "org.freedesktop.DBus.Error.AccessDenied"
            | "org.freedesktop.DBus.Error.NameHasNoOwner"
            | "org.freedesktop.DBus.Error.NoReply"
            | "org.freedesktop.DBus.Error.Disconnected" => GoaError::Unreachable(text),
            "org.gnome.OnlineAccounts.Error.NotAuthorized" => GoaError::NotAuthorized,
            "org.freedesktop.DBus.Error.UnknownObject"
            | "org.freedesktop.DBus.Error.UnknownMethod"
            | "org.freedesktop.DBus.Error.UnknownInterface" => GoaError::NoAccount,
            _ => GoaError::Other(format!("{name}: {text}")),
        };
    }
    match err {
        Error::InputOutput(_) | Error::Address(_) | Error::Handshake(_) => {
            GoaError::Unreachable(err.to_string())
        }
        other => GoaError::Other(other.to_string()),
    }
}

type Interfaces = HashMap<zbus::names::OwnedInterfaceName, HashMap<String, OwnedVariant>>;

fn text(props: &HashMap<String, OwnedVariant>, key: &str) -> String {
    props
        .get(key)
        .and_then(|v| <&str>::try_from(v).ok())
        .unwrap_or_default()
        .to_string()
}

fn flag(props: &HashMap<String, OwnedVariant>, key: &str) -> bool {
    props
        .get(key)
        .and_then(|v| bool::try_from(v).ok())
        .unwrap_or(false)
}

/// The account in `interfaces`, if it is a Google one.
fn google_of(interfaces: &Interfaces) -> Option<GoaAccount> {
    let props = interfaces
        .iter()
        .find(|(name, _)| name.as_str() == ACCOUNT)?
        .1;
    (text(props, "ProviderType") == "google").then(|| GoaAccount {
        id: text(props, "Id"),
        identity: text(props, "Identity"),
        presentation: text(props, "PresentationIdentity"),
        mail_disabled: flag(props, "MailDisabled"),
        attention_needed: flag(props, "AttentionNeeded"),
    })
}

async fn list(conn: &Connection) -> Result<Vec<GoaAccount>, GoaError> {
    let reply = conn
        .call_method(
            Some(SERVICE),
            ROOT,
            Some("org.freedesktop.DBus.ObjectManager"),
            "GetManagedObjects",
            &(),
        )
        .await
        .map_err(error_of)?;
    let objects: HashMap<OwnedObjectPath, Interfaces> = reply
        .body()
        .deserialize()
        .map_err(|e| GoaError::Other(e.to_string()))?;
    let mut found: Vec<_> = objects.values().filter_map(google_of).collect();
    found.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(found)
}

fn id_of(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

fn account_path(id: &str) -> String {
    format!("{ROOT}/Accounts/{id}")
}

impl OnlineAccounts for DbusAccounts {
    fn google_accounts(&self) -> BoxFuture<'_, Result<Vec<GoaAccount>, GoaError>> {
        Box::pin(async {
            let found = bounded(async {
                let conn = self.connection().await?;
                list(&conn).await
            })
            .await;
            self.settle(found)
        })
    }

    fn access_token<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<GoaToken, GoaError>> {
        Box::pin(async move {
            let got = bounded(async move {
                let conn = self.connection().await?;
                let reply = conn
                    .call_method(
                        Some(SERVICE),
                        account_path(id),
                        Some(OAUTH2),
                        "GetAccessToken",
                        &(),
                    )
                    .await
                    .map_err(error_of)?;
                let (token, secs): (String, i32) = reply
                    .body()
                    .deserialize()
                    .map_err(|e| GoaError::Other(e.to_string()))?;
                Ok(GoaToken {
                    token,
                    expires_in: Duration::from_secs(secs.max(0) as u64),
                })
            })
            .await;
            self.settle(got)
        })
    }

    fn ensure_credentials<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), GoaError>> {
        Box::pin(async move {
            let done = bounded(async move {
                let conn = self.connection().await?;
                conn.call_method(
                    Some(SERVICE),
                    account_path(id),
                    Some(ACCOUNT),
                    "EnsureCredentials",
                    &(),
                )
                .await
                .map_err(error_of)?;
                Ok(())
            })
            .await;
            self.settle(done)
        })
    }

    fn changes(&self) -> BoxFuture<'_, Result<BoxStream<'static, GoaChange>, GoaError>> {
        Box::pin(async {
            let opened = bounded(async {
                let conn = self.connection().await?;
                let added = MatchRule::builder()
                    .msg_type(zbus::message::Type::Signal)
                    .sender(SERVICE)
                    .interface("org.freedesktop.DBus.ObjectManager")
                    .member("InterfacesAdded")
                    .build()
                    .map_err(error_of)?;
                let removed = MatchRule::builder()
                    .msg_type(zbus::message::Type::Signal)
                    .sender(SERVICE)
                    .interface("org.freedesktop.DBus.ObjectManager")
                    .member("InterfacesRemoved")
                    .build()
                    .map_err(error_of)?;
                let changed = MatchRule::builder()
                    .msg_type(zbus::message::Type::Signal)
                    .sender(SERVICE)
                    .interface("org.freedesktop.DBus.Properties")
                    .member("PropertiesChanged")
                    .path_namespace(format!("{ROOT}/Accounts"))
                    .build()
                    .map_err(error_of)?;
                let mut streams = Vec::new();
                for rule in [added, removed, changed] {
                    streams.push(
                        MessageStream::for_match_rule(rule, &conn, None)
                            .await
                            .map_err(error_of)?,
                    );
                }
                let again = conn.clone();
                let stream = select_all(streams)
                    .filter_map(move |message| {
                        let conn = again.clone();
                        async move {
                            let message = message.ok()?;
                            let header = message.header();
                            let member = header.member()?.as_str().to_string();
                            let path = if member == "PropertiesChanged" {
                                header.path()?.as_str().to_string()
                            } else {
                                let (path, _): (OwnedObjectPath, Interfaces) = match member.as_str()
                                {
                                    "InterfacesAdded" => message.body().deserialize().ok()?,
                                    _ => {
                                        let (path, _): (OwnedObjectPath, Vec<String>) =
                                            message.body().deserialize().ok()?;
                                        return Some(GoaChange::Removed {
                                            id: id_of(path.as_str()),
                                        });
                                    }
                                };
                                path.as_str().to_string()
                            };
                            let id = id_of(&path);
                            let found = bounded(list(&conn)).await.ok()?;
                            let account = found.into_iter().find(|a| a.id == id)?;
                            Some(if member == "InterfacesAdded" {
                                GoaChange::Added(account)
                            } else {
                                GoaChange::Changed(account)
                            })
                        }
                    })
                    .boxed();
                Ok(stream)
            })
            .await;
            self.settle(opened)
        })
        .boxed()
    }
}
