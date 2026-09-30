//! Whether a snap can reach the desktop's keyring. The snap keeps Google
//! refresh tokens, IMAP passwords and AI keys in the Secret Service through
//! the `password-manager-service` plug, and the Snap Store does not connect
//! that plug on install. Until the person connects it, every save to the
//! keyring fails, so the window says what to run, and Add Account says the
//! same in place of the keyring's own error.

use std::sync::OnceLock;

use gtk::{gio, glib};
use mailrs_domain::translate::{fill, gettext};
use mailrs_sync::passwords::PasswordError;
use mailrs_sync::sign_in::{ImapSignInError, SignInError};

use crate::packaging::Packaging;

/// What connects the plug. It names the snap, so it stays the same in
/// every language.
pub const COMMAND: &str = "snap connect penguin-mail:password-manager-service";

/// What the app knows about the plug.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plug {
    /// Not a snap: the app reaches the keyring without a plug.
    NotNeeded,
    Connected,
    Disconnected,
    /// A snap where snapd gave no answer, or the check has not run yet.
    Unknown,
}

impl Plug {
    /// Whether the window shows the notice that says to connect the plug.
    /// An unknown answer shows nothing, since a notice that may be wrong
    /// would stay on screen for good.
    pub fn wants_notice(self) -> bool {
        self == Plug::Disconnected
    }

    /// Whether a keyring that refused a save points to the plug. A snap
    /// that snapd did not answer for gets the benefit of the doubt, since
    /// the plug is the likeliest reason there.
    pub fn explains_refusal(self) -> bool {
        matches!(self, Plug::Disconnected | Plug::Unknown)
    }
}

/// Reads the plug for a build of `packaging`. `ask` runs `snapctl
/// is-connected` and returns its exit code, `None` when it could not run;
/// only a snap calls it.
pub fn read(packaging: Packaging, ask: impl FnOnce() -> Option<i32>) -> Plug {
    if packaging != Packaging::Snap {
        return Plug::NotNeeded;
    }
    match ask() {
        Some(0) => Plug::Connected,
        Some(1) => Plug::Disconnected,
        _ => Plug::Unknown,
    }
}

/// Runs `snapctl is-connected`, which exits 0 when the plug is connected
/// and 1 when it is not.
fn ask_snapctl() -> Option<i32> {
    let status = std::process::Command::new("snapctl")
        .args(["is-connected", "password-manager-service"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    match status {
        Ok(status) => status.code(),
        Err(err) => {
            tracing::warn!(error = %err, "snapctl could not run");
            None
        }
    }
}

/// The answer for this process, once `check` has one.
static CHECKED: OnceLock<Plug> = OnceLock::new();

/// The plug as the last check found it; `Unknown` in a snap before the
/// check has run.
pub fn current() -> Plug {
    CHECKED
        .get()
        .copied()
        .unwrap_or(match crate::packaging::BUILT_FOR {
            Packaging::Snap => Plug::Unknown,
            _ => Plug::NotNeeded,
        })
}

/// Reads the plug once per process, off the main thread, since snapctl
/// talks to snapd. `unplugged_demo` stands in a disconnected plug for the
/// demo, so its notice can be seen without a snap.
pub async fn check(unplugged_demo: bool) -> Plug {
    if let Some(plug) = CHECKED.get() {
        return *plug;
    }
    let plug = if unplugged_demo {
        Plug::Disconnected
    } else {
        gio::spawn_blocking(|| read(crate::packaging::BUILT_FOR, ask_snapctl))
            .await
            .unwrap_or(Plug::Unknown)
    };
    if plug != Plug::NotNeeded {
        tracing::info!(?plug, "read the password-manager-service plug");
    }
    *CHECKED.get_or_init(|| plug)
}

/// Whether `err` says the keyring would not keep a Google refresh token
/// or an IMAP password.
pub fn is_keyring_refusal(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        matches!(cause.downcast_ref(), Some(SignInError::Token(_)))
            || matches!(cause.downcast_ref(), Some(ImapSignInError::Password(_)))
            || cause.downcast_ref::<PasswordError>().is_some()
    })
}

/// The notice's words as Pango markup, with the command in monospace.
pub fn notice_markup() -> String {
    let words = gettext(
        "Penguin Mail cannot save sign-ins until the snap can reach your keyring. \
         Run {command} in a terminal, then restart Penguin Mail.",
    );
    fill(
        &glib::markup_escape_text(&words),
        &[(
            "command",
            &format!("<tt>{}</tt>", glib::markup_escape_text(COMMAND)),
        )],
    )
}

#[cfg(test)]
mod tests {
    use mailrs_store::StoreError;

    use super::*;

    fn never_asked() -> Option<i32> {
        panic!("only a snap asks snapd")
    }

    #[test]
    fn a_build_that_is_not_a_snap_needs_no_plug_and_asks_snapd_nothing() {
        for packaging in [Packaging::Native, Packaging::Rpm, Packaging::Flatpak] {
            assert_eq!(read(packaging, never_asked), Plug::NotNeeded);
        }
    }

    #[test]
    fn snapctl_exiting_zero_means_connected() {
        assert_eq!(read(Packaging::Snap, || Some(0)), Plug::Connected);
    }

    #[test]
    fn snapctl_exiting_one_means_disconnected() {
        assert_eq!(read(Packaging::Snap, || Some(1)), Plug::Disconnected);
    }

    #[test]
    fn snapctl_failing_or_missing_leaves_the_plug_unknown() {
        assert_eq!(read(Packaging::Snap, || Some(2)), Plug::Unknown);
        assert_eq!(read(Packaging::Snap, || None), Plug::Unknown);
    }

    #[test]
    fn only_a_disconnected_plug_puts_up_the_notice() {
        assert!(Plug::Disconnected.wants_notice());
        for plug in [Plug::NotNeeded, Plug::Connected, Plug::Unknown] {
            assert!(!plug.wants_notice(), "{plug:?}");
        }
    }

    #[test]
    fn a_refusal_points_to_the_plug_unless_it_is_known_connected() {
        assert!(Plug::Disconnected.explains_refusal());
        assert!(Plug::Unknown.explains_refusal());
        assert!(!Plug::Connected.explains_refusal());
        assert!(!Plug::NotNeeded.explains_refusal());
    }

    #[test]
    fn a_refresh_token_the_keyring_refused_is_a_keyring_refusal() {
        let err = anyhow::Error::new(SignInError::Token("no such interface".into()));
        assert!(is_keyring_refusal(&err));
    }

    #[test]
    fn an_imap_password_the_keyring_refused_is_a_keyring_refusal() {
        let err = anyhow::Error::new(ImapSignInError::Password(PasswordError::Keyring(
            "no such interface".into(),
        )));
        assert!(is_keyring_refusal(&err));
    }

    #[test]
    fn a_refusal_wrapped_in_context_still_counts() {
        let err = anyhow::Error::new(PasswordError::Keyring("denied".into()))
            .context("saving the password");
        assert!(is_keyring_refusal(&err));
    }

    #[test]
    fn other_sign_in_errors_are_not_keyring_refusals() {
        let taken = anyhow::Error::new(SignInError::Taken {
            address: "ana@example.com".into(),
            provider: "Gmail".into(),
        });
        assert!(!is_keyring_refusal(&taken));
        let store = anyhow::Error::new(SignInError::Store(StoreError::Closed));
        assert!(!is_keyring_refusal(&store));
        assert!(!is_keyring_refusal(&anyhow::anyhow!("Canceled.")));
    }

    #[test]
    fn the_notice_names_the_command_in_monospace_and_says_to_restart() {
        let markup = notice_markup();
        assert!(markup.contains(&format!("<tt>{COMMAND}</tt>")), "{markup}");
        assert!(markup.contains("restart"), "{markup}");
    }
}
