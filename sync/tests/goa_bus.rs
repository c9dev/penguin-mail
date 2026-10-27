//! `DbusAccounts` against a stand-in for Online Accounts on a private bus.
//! Skips when `dbus-daemon` is missing.

#![cfg(target_os = "linux")]

use std::process::{Child, Command, Stdio};

use futures::StreamExt;
use mailrs_sync::goa::{DbusAccounts, GoaChange, GoaError, OnlineAccounts};
use zbus::ObjectPath;
use zbus::object_server::SignalEmitter;

struct Bus {
    child: Child,
    _config: tempfile::NamedTempFile,
    address: String,
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A session bus with no service folders: the machine's own goa-daemon
/// must not start on it.
const CONFIG: &str = r#"<busconfig>
  <type>session</type>
  <listen>unix:tmpdir=/tmp</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>"#;

fn start_bus() -> Option<Bus> {
    start_bus_listening(CONFIG)
}

fn start_bus_listening(config_text: &str) -> Option<Bus> {
    use std::io::Write;
    let mut config = tempfile::NamedTempFile::new().ok()?;
    config.write_all(config_text.as_bytes()).ok()?;
    let mut child = Command::new("dbus-daemon")
        .arg(format!("--config-file={}", config.path().display()))
        .args(["--print-address", "--nofork"])
        .stdout(Stdio::piped())
        .spawn()
        .ok()?;
    let out = child.stdout.take()?;
    let mut line = String::new();
    std::io::BufRead::read_line(&mut std::io::BufReader::new(out), &mut line).ok()?;
    Some(Bus {
        child,
        _config: config,
        address: line.trim().to_string(),
    })
}

#[derive(zbus::DBusError, Debug)]
#[zbus(prefix = "org.gnome.OnlineAccounts.Error")]
enum FakeError {
    #[zbus(error)]
    ZBus(zbus::Error),
    NotAuthorized(String),
}

struct Account {
    provider: &'static str,
    id: &'static str,
    identity: &'static str,
    mail_disabled: bool,
}

#[zbus::interface(name = "org.gnome.OnlineAccounts.Account")]
impl Account {
    #[zbus(property)]
    fn provider_type(&self) -> String {
        self.provider.into()
    }
    #[zbus(property)]
    fn id(&self) -> String {
        self.id.into()
    }
    #[zbus(property)]
    fn identity(&self) -> String {
        self.identity.into()
    }
    #[zbus(property)]
    fn presentation_identity(&self) -> String {
        self.identity.into()
    }
    #[zbus(property)]
    fn mail_disabled(&self) -> bool {
        self.mail_disabled
    }
    #[zbus(property)]
    fn attention_needed(&self) -> bool {
        false
    }
    fn ensure_credentials(&self) -> i32 {
        3599
    }
}

struct OAuth {
    refuse: bool,
}

#[zbus::interface(name = "org.gnome.OnlineAccounts.OAuth2Based")]
impl OAuth {
    fn get_access_token(&self) -> Result<(String, i32), FakeError> {
        if self.refuse {
            Err(FakeError::NotAuthorized("sign in again".into()))
        } else {
            Ok(("t1".into(), 3599))
        }
    }
}

const ROOT: &str = "/org/gnome/OnlineAccounts";

fn path(id: &str) -> ObjectPath<'static> {
    ObjectPath::try_from(format!("{ROOT}/Accounts/{id}")).unwrap()
}

#[tokio::test]
async fn the_client_reads_accounts_tokens_and_changes_over_a_bus() {
    let Some(bus) = start_bus() else {
        eprintln!("dbus-daemon is missing; skipping");
        return;
    };
    let server = zbus::connection::Builder::address(bus.address.as_str())
        .name("org.gnome.OnlineAccounts")
        .build()
        .await
        .unwrap();
    let objects = server.object_server();
    objects.at(ROOT, zbus::fdo::ObjectManager).await.unwrap();
    let google = Account {
        provider: "google",
        id: "account_1_0",
        identity: "dana@example.com",
        mail_disabled: false,
    };
    objects.at(path("account_1_0"), google).await.unwrap();
    objects
        .at(path("account_1_0"), OAuth { refuse: false })
        .await
        .unwrap();
    let other = Account {
        provider: "exchange",
        id: "account_2_0",
        identity: "x@example.com",
        mail_disabled: false,
    };
    objects.at(path("account_2_0"), other).await.unwrap();
    let bad = Account {
        provider: "google",
        id: "account_bad",
        identity: "bad@example.com",
        mail_disabled: false,
    };
    objects.at(path("account_bad"), bad).await.unwrap();
    objects
        .at(path("account_bad"), OAuth { refuse: true })
        .await
        .unwrap();

    let accounts = DbusAccounts::at(&bus.address);
    let list = accounts.google_accounts().await.unwrap();
    let mut ids: Vec<_> = list.iter().map(|a| a.id.as_str()).collect();
    ids.sort();
    assert_eq!(ids, ["account_1_0", "account_bad"]);
    let dana = list.iter().find(|a| a.id == "account_1_0").unwrap();
    assert_eq!(dana.identity, "dana@example.com");

    assert_eq!(
        accounts.access_token("account_1_0").await.unwrap().token,
        "t1"
    );
    assert!(matches!(
        accounts.access_token("account_bad").await,
        Err(GoaError::NotAuthorized)
    ));
    assert!(matches!(
        accounts.access_token("account_gone").await,
        Err(GoaError::NoAccount)
    ));
    accounts.ensure_credentials("account_1_0").await.unwrap();

    let mut changes = accounts.changes().await.unwrap();
    let late = Account {
        provider: "google",
        id: "account_3_0",
        identity: "late@example.com",
        mail_disabled: false,
    };
    objects.at(path("account_3_0"), late).await.unwrap();
    let added = tokio::time::timeout(std::time::Duration::from_secs(5), changes.next())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(added, GoaChange::Added(a) if a.id == "account_3_0"));

    let iface = objects
        .interface::<_, Account>(path("account_3_0"))
        .await
        .unwrap();
    iface.get_mut().await.mail_disabled = true;
    let emitter = SignalEmitter::new(&server, path("account_3_0")).unwrap();
    iface
        .get()
        .await
        .mail_disabled_changed(&emitter)
        .await
        .unwrap();
    let changed = tokio::time::timeout(std::time::Duration::from_secs(5), changes.next())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(changed, GoaChange::Changed(a) if a.id == "account_3_0" && a.mail_disabled));
}

#[tokio::test]
async fn a_bus_with_no_online_accounts_is_unreachable() {
    let Some(bus) = start_bus() else {
        eprintln!("dbus-daemon is missing; skipping");
        return;
    };
    let accounts = DbusAccounts::at(&bus.address);
    assert!(matches!(
        accounts.google_accounts().await,
        Err(GoaError::Unreachable(_))
    ));
}

/// The session bus goes away and comes back at the same address: the next
/// call dials again instead of failing on the dead connection.
#[tokio::test]
async fn a_restarted_bus_is_dialed_again() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("bus");
    let config = CONFIG.replace(
        "unix:tmpdir=/tmp",
        &format!("unix:path={}", socket.display()),
    );
    let Some(first) = start_bus_listening(&config) else {
        eprintln!("dbus-daemon is missing; skipping");
        return;
    };
    // Without the bus's GUID, which a new daemon does not share.
    let address = format!("unix:path={}", socket.display());
    let accounts = DbusAccounts::at(&address);
    assert!(matches!(
        accounts.google_accounts().await,
        Err(GoaError::Unreachable(_))
    ));
    let server = zbus::connection::Builder::address(address.as_str())
        .name("org.gnome.OnlineAccounts")
        .build()
        .await
        .unwrap();
    server
        .object_server()
        .at(ROOT, zbus::fdo::ObjectManager)
        .await
        .unwrap();
    assert!(accounts.google_accounts().await.unwrap().is_empty());

    drop(first);
    drop(server);
    assert!(accounts.google_accounts().await.is_err());
    let _ = std::fs::remove_file(&socket);

    let Some(_second) = start_bus_listening(&config) else {
        panic!("the bus did not start again");
    };
    let server = zbus::connection::Builder::address(address.as_str())
        .name("org.gnome.OnlineAccounts")
        .build()
        .await
        .unwrap();
    server
        .object_server()
        .at(ROOT, zbus::fdo::ObjectManager)
        .await
        .unwrap();
    assert!(accounts.google_accounts().await.unwrap().is_empty());
}
