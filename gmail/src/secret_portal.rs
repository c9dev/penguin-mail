//! Secrets through the Secret portal (`org.freedesktop.portal.Secret`),
//! compiled only with the `packaging-flatpak` feature. Outside a Flatpak
//! this module does not exist, and nothing in the crate links `oo7` or
//! reaches the portal.
//!
//! Every call opens its own `oo7::Keyring`, asks it once, and lets it go;
//! a secret is read or written rarely enough that the cost of doing so
//! is not worth avoiding. `oo7::Keyring::new` already tells a sandboxed
//! app from a host one, so the same call that gives a Flatpak its own
//! encrypted file gives a test the real Secret Service were this ever
//! run unsandboxed; only the Flatpak build takes this path at all.

use std::collections::HashMap;
use std::future::Future;
use std::sync::OnceLock;

use oo7::{Keyring, Secret};

/// The keyring's own way to find one secret among the rest: a service
/// name shared by many entries, and a user name that picks one of them,
/// the same two attributes the `keyring` crate's Secret Service backend
/// stores a password under.
fn attributes<'a>(service: &'a str, user: &'a str) -> HashMap<&'a str, &'a str> {
    HashMap::from([("service", service), ("username", user)])
}

/// A secret's bytes, read back as the text it was saved as.
fn as_text(secret: &Secret) -> Result<String, String> {
    std::str::from_utf8(secret)
        .map(str::to_string)
        .map_err(|err| err.to_string())
}

/// Reads a secret through the portal, or `None` when there is none.
pub fn load(service: &str, user: &str) -> Result<Option<String>, String> {
    let (service, user) = (service.to_string(), user.to_string());
    run(async move {
        let keyring = Keyring::new().await.map_err(|err| err.to_string())?;
        let items = keyring
            .search_items(&attributes(&service, &user))
            .await
            .map_err(|err| err.to_string())?;
        let Some(item) = items.first() else {
            return Ok(None);
        };
        let secret = item.secret().await.map_err(|err| err.to_string())?;
        as_text(&secret).map(Some)
    })
}

/// Writes a secret through the portal, replacing one already there under
/// the same service and user.
pub fn save(service: &str, user: &str, secret: &str) -> Result<(), String> {
    let (service, user) = (service.to_string(), user.to_string());
    let secret = Secret::text(secret);
    run(async move {
        let keyring = Keyring::new().await.map_err(|err| err.to_string())?;
        keyring
            .create_item(&user, &attributes(&service, &user), secret, true)
            .await
            .map_err(|err| err.to_string())
    })
}

/// Removes a secret through the portal. Succeeds when there is nothing to
/// remove.
pub fn delete(service: &str, user: &str) -> Result<(), String> {
    let (service, user) = (service.to_string(), user.to_string());
    run(async move {
        let keyring = Keyring::new().await.map_err(|err| err.to_string())?;
        keyring
            .delete(&attributes(&service, &user))
            .await
            .map_err(|err| err.to_string())
    })
}

/// Runs a portal call to completion on the one runtime every portal call
/// shares. Callers already run off the GTK thread, the way they do for the
/// desktop keyring, so blocking here costs nothing a caller has not
/// already budgeted for.
///
/// The runtime must outlive every call. ashpd opens one session bus
/// connection per process and keeps it, and zbus reads that connection on
/// a task of the runtime that opened it. A runtime per call ended that
/// task with the first call, so every later call sent its request and
/// waited for an answer nothing read: signing in hung after the browser
/// said it was done, with no error.
fn run<T: Send + 'static>(fut: impl Future<Output = T> + Send + 'static) -> T {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    let runtime = RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("secret-portal")
            .enable_all()
            .build()
            .expect("a runtime for the secret portal")
    });
    // Spawned rather than block_on, so a caller on any thread, inside a
    // runtime or not, waits the same way.
    let (tx, rx) = std::sync::mpsc::channel();
    runtime.spawn(async move {
        let _ = tx.send(fut.await);
    });
    rx.recv().expect("the secret portal's runtime dropped a call")
}

#[cfg(test)]
mod tests {
    use oo7::file::UnlockedKeyring;

    use super::*;

    /// zbus reads ashpd's one connection on a task of the runtime that
    /// opened it. A task one call starts must still run at the next call,
    /// or every call after the first waits for an answer nobody reads.
    #[test]
    // The block hands back the task's handle on purpose, to look at it
    // from the next call rather than wait for it.
    #[allow(clippy::async_yields_async)]
    fn a_task_one_call_starts_outlives_it() {
        let reader = run(async {
            tokio::spawn(async {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                }
            })
        });
        let alive = run(async move {
            tokio::task::yield_now().await;
            !reader.is_finished()
        });
        assert!(alive);
    }
    async fn empty_keyring() -> UnlockedKeyring {
        UnlockedKeyring::temporary(Secret::random().expect("random bytes for a test secret"))
            .await
            .expect("a keyring kept in memory only")
    }

    #[tokio::test]
    async fn a_secret_comes_back_until_it_is_deleted() {
        let keyring = empty_keyring().await;
        let attrs = attributes("penguin-mail-imap", "7");

        assert!(keyring.search_items(&attrs).await.unwrap().is_empty());

        keyring
            .create_item("7", &attrs, "hunter2", true)
            .await
            .unwrap();
        let items = keyring.search_items(&attrs).await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(
            as_text(&items[0].as_unlocked().secret()).unwrap(),
            "hunter2"
        );

        keyring.delete(&attrs).await.unwrap();
        assert!(keyring.search_items(&attrs).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn saving_again_replaces_the_secret_rather_than_adding_a_second_one() {
        let keyring = empty_keyring().await;
        let attrs = attributes("penguin-mail-imap", "7");

        keyring.create_item("7", &attrs, "first", true).await.unwrap();
        keyring.create_item("7", &attrs, "second", true).await.unwrap();

        let items = keyring.search_items(&attrs).await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(as_text(&items[0].as_unlocked().secret()).unwrap(), "second");
    }

    #[tokio::test]
    async fn deleting_with_nothing_stored_is_not_an_error() {
        let keyring = empty_keyring().await;
        keyring
            .delete(&attributes("penguin-mail-imap", "7"))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn one_account_never_sees_another_accounts_secret() {
        let keyring = empty_keyring().await;
        keyring
            .create_item("7", &attributes("penguin-mail-imap", "7"), "seven", true)
            .await
            .unwrap();

        assert!(
            keyring
                .search_items(&attributes("penguin-mail-imap", "8"))
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            keyring
                .search_items(&attributes("mailrs", "7"))
                .await
                .unwrap()
                .is_empty()
        );
    }
}
