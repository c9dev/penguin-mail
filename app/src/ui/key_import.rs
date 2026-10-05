//! Importing an OpenPGP key or an S/MIME certificate from a file the
//! person picks in Preferences, and the dialog that says what came in.
//!
//! The snap keeps a GnuPG home of its own, so nothing in `~/.gnupg`
//! reaches it; this is how its keys get there. The import goes into
//! whatever home gpg and gpgsm already use, `GNUPGHOME` when it is set.
//! The text half, [`report`], is sentences about plain data and is tested
//! as such.

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use mailrs_domain::translate::{fill, fill_plural, gettext};
use mailrs_pgp::gnupg::{Change, Import};
use mailrs_smime::SmimeError;

use crate::app::App;

/// The Import button for `kind`, for the end of its row in Preferences.
/// It asks for a file, imports it, says what came in over `over`, and
/// calls `imported` after anything came in, so the rows can say what
/// the keyring holds now.
pub fn button(
    app: &Rc<App>,
    kind: Kind,
    over: &adw::PreferencesDialog,
    imported: impl Fn() + 'static,
) -> gtk::Button {
    // A child label rather than the button's own, so the name set below
    // reaches a screen reader: both rows' buttons read "Import…". The
    // spinner shares a stack with it, which keeps the button's width.
    let stack = gtk::Stack::new();
    stack.add_named(&gtk::Label::new(Some(&gettext("Import…"))), Some("label"));
    stack.add_named(&adw::Spinner::new(), Some("spinner"));
    let button = gtk::Button::builder()
        .child(&stack)
        .valign(gtk::Align::Center)
        .build();
    super::name(
        &button,
        &match kind {
            Kind::Pgp => gettext("Import OpenPGP Keys"),
            Kind::Smime => gettext("Import S/MIME Certificates"),
        },
    );
    // The dialog holds this button, so the button holds the dialog weakly.
    let (app, over, imported) = (Rc::downgrade(app), over.downgrade(), Rc::new(imported));
    button.connect_clicked(move |button| {
        let (Some(app), Some(over)) = (app.upgrade(), over.upgrade()) else {
            return;
        };
        let (imported, button) = (Rc::clone(&imported), button.clone());
        glib::spawn_future_local(async move {
            let window = over.root().and_downcast::<gtk::Window>();
            // The file is read only once the person picks it.
            let Ok(file) = chooser(kind).open_future(window.as_ref()).await else {
                return;
            };
            let named = file
                .basename()
                .map(|name| name.display().to_string())
                .unwrap_or_default();
            match file.load_contents_future().await {
                Ok((bytes, _)) => {
                    attempt(app, kind, Rc::new(bytes.to_vec()), over, button, imported).await;
                }
                Err(err) => {
                    let said = report(kind, Err(unreadable(&named, err.message())));
                    dialog(&said).present(Some(&over));
                }
            }
        });
    });
    button
}

/// The file chooser for `kind`, filtered to the files that hold one,
/// with every file a choice away for a key saved under another name.
fn chooser(kind: Kind) -> gtk::FileDialog {
    let (title, name, suffixes): (String, String, &[&str]) = match kind {
        Kind::Pgp => (
            gettext("Import OpenPGP Keys"),
            gettext("OpenPGP Keys"),
            &["asc", "gpg", "pgp", "key"],
        ),
        Kind::Smime => (
            gettext("Import S/MIME Certificates"),
            gettext("Certificates"),
            &["p12", "pfx", "pem", "crt", "cer"],
        ),
    };
    let keys = gtk::FileFilter::new();
    keys.set_name(Some(&name));
    for suffix in suffixes {
        keys.add_suffix(suffix);
    }
    let all = gtk::FileFilter::new();
    all.set_name(Some(&gettext("All Files")));
    all.add_pattern("*");
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&keys);
    filters.append(&all);
    gtk::FileDialog::builder()
        .title(title)
        .modal(true)
        .filters(&filters)
        .default_filter(&keys)
        .build()
}

/// One run of the import, then the dialog that says how it went. Try
/// Again on that dialog runs it once more with the same bytes, so a
/// mistyped passphrase does not mean picking the file again.
async fn attempt(
    app: Rc<App>,
    kind: Kind,
    file: Rc<Vec<u8>>,
    over: adw::PreferencesDialog,
    button: gtk::Button,
    imported: Rc<dyn Fn()>,
) {
    loop {
        busy(&button, true);
        let outcome = run(&app, kind, &file).await;
        busy(&button, false);
        // The button went insensitive while the import ran, and took the
        // keyboard focus with it.
        button.grab_focus();
        if outcome.is_ok() {
            imported();
        }
        let said = report(kind, outcome.as_deref().map_err(Clone::clone));
        if dialog(&said).choose_future(Some(&over)).await != RETRY {
            return;
        }
    }
}

/// Imports `file` through the engine for `kind`.
async fn run(app: &App, kind: Kind, file: &Rc<Vec<u8>>) -> Result<Vec<Import>, Failure> {
    // The engines run on a blocking thread, which takes its own copy.
    let bytes = file.as_ref().clone();
    let ran = match kind {
        Kind::Pgp => app
            .core
            .gpg(move |pgp| Ok(pgp.import(&bytes)))
            .await
            .map(|found| found.map_err(|err| Failure::Other(crate::pgp::explain(&err)))),
        Kind::Smime => app
            .core
            .gpgsm(move |smime| Ok(smime.import(&bytes)))
            .await
            .map(|found| {
                found.map_err(|err| match err {
                    SmimeError::WrongPassphrase | SmimeError::NoPassphrase => {
                        Failure::Passphrase(crate::smime::explain(&err))
                    }
                    other => Failure::Other(crate::smime::explain(&other)),
                })
            }),
    };
    ran.unwrap_or_else(|err| Err(Failure::Other(err.to_string())))
}

/// Swaps the button's label for a spinner while gpg or gpgsm runs, which
/// can be as long as a pinentry stays open. The stack keeps the button at
/// the label's width.
fn busy(button: &gtk::Button, running: bool) {
    button.set_sensitive(!running);
    if let Some(stack) = button.child().and_downcast::<gtk::Stack>() {
        stack.set_visible_child_name(if running { "spinner" } else { "label" });
    }
}

/// The response that runs the import again.
const RETRY: &str = "retry";

/// The dialog that says how an import went.
fn dialog(said: &Report) -> adw::AlertDialog {
    let dialog = adw::AlertDialog::new(Some(&said.heading), Some(&said.body));
    if !said.lines.is_empty() {
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .build();
        for line in &said.lines {
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&line.title))
                .build();
            if let Some(subtitle) = &line.subtitle {
                row.set_subtitle(&glib::markup_escape_text(subtitle));
            }
            row.add_suffix(
                &gtk::Label::builder()
                    .label(&line.status)
                    .css_classes(["dim-label", "caption"])
                    .build(),
            );
            list.append(&row);
        }
        if said.more > 0 {
            list.append(
                &adw::ActionRow::builder()
                    .title(more_text(said.more))
                    .css_classes(["dim-label"])
                    .build(),
            );
        }
        dialog.set_extra_child(Some(&list));
    }
    if said.retry {
        dialog.add_responses(&[("close", &gettext("Close")), (RETRY, &gettext("Try Again"))]);
        dialog.set_response_appearance(RETRY, adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some(RETRY));
    } else {
        dialog.add_responses(&[("close", &gettext("Close"))]);
        dialog.set_default_response(Some("close"));
    }
    dialog.set_close_response("close");
    dialog
}

/// Which engine a file goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Pgp,
    Smime,
}

/// Why an import brought nothing in, in the reader's words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The passphrase was wrong or never came. Trying again makes sense.
    Passphrase(String),
    /// Anything else: not a key, an unreadable file, a program that would
    /// not run.
    Other(String),
}

/// What the dialog after an import says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub heading: String,
    pub body: String,
    /// One row per key or certificate, the first [`SHOWN`] of them.
    pub lines: Vec<Line>,
    /// How many more came in than the rows show.
    pub more: usize,
    /// Whether to offer Try Again.
    pub retry: bool,
}

/// One key or certificate on the dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// The name on it, or its address, or its fingerprint.
    pub title: String,
    /// The address, when the title is the name.
    pub subtitle: Option<String>,
    /// What the import did for it.
    pub status: String,
}

/// How many rows the dialog shows before it counts the rest.
pub const SHOWN: usize = 6;

/// The dialog's words for one import of `kind`.
pub fn report(kind: Kind, outcome: Result<&[Import], Failure>) -> Report {
    let found = match outcome {
        Ok(found) => found,
        Err(failure) => {
            let (body, retry) = match failure {
                Failure::Passphrase(reason) => (reason, true),
                Failure::Other(reason) => (reason, false),
            };
            return Report {
                heading: match kind {
                    Kind::Pgp => gettext("Key Not Imported"),
                    Kind::Smime => gettext("Certificate Not Imported"),
                },
                body,
                lines: Vec::new(),
                more: 0,
                retry,
            };
        }
    };
    let changed = found
        .iter()
        .filter(|import| import.change != Change::Unchanged)
        .count();
    let secret = found
        .iter()
        .any(|import| import.secret && import.change == Change::New);
    let count = changed.to_string();
    let heading = match (kind, changed) {
        (_, 0) => gettext("Already Imported"),
        (Kind::Pgp, _) => fill_plural(
            "Key Imported",
            "{count} Keys Imported",
            changed,
            &[("count", &count)],
        ),
        (Kind::Smime, _) => fill_plural(
            "Certificate Imported",
            "{count} Certificates Imported",
            changed,
            &[("count", &count)],
        ),
    };
    let body = match (kind, changed, secret) {
        (Kind::Pgp, 0, _) => gettext("gpg already holds everything in this file."),
        (Kind::Smime, 0, _) => gettext("gpgsm already holds everything in this file."),
        (Kind::Pgp, _, true) => gettext(
            "You can sign and decrypt with the secret key now. gpg asks for its passphrase \
             the first time.",
        ),
        (Kind::Smime, _, true) => gettext("You can sign and decrypt with the secret key now."),
        (Kind::Pgp, _, false) => fill_plural(
            "You can encrypt to this person and check their signatures.",
            "You can encrypt to these people and check their signatures.",
            changed,
            &[],
        ),
        (Kind::Smime, _, false) => fill_plural(
            "Penguin Mail can check signatures made with this certificate.",
            "Penguin Mail can check signatures made with these certificates.",
            changed,
            &[],
        ),
    };
    Report {
        heading,
        body,
        lines: found.iter().take(SHOWN).map(line).collect(),
        more: found.len().saturating_sub(SHOWN),
        retry: false,
    }
}

/// The row for one key or certificate.
fn line(import: &Import) -> Line {
    let (title, subtitle) = match (&import.name, &import.address) {
        (Some(name), address) => (name.clone(), address.clone()),
        (None, Some(address)) => (address.clone(), None),
        (None, None) => (short(&import.fingerprint), None),
    };
    let status = match (import.change, import.secret) {
        (Change::New, true) => gettext("New, with secret key"),
        (Change::New, false) => gettext("New"),
        (Change::Updated, _) => gettext("Updated"),
        (Change::Unchanged, _) => gettext("Already here"),
    };
    Line {
        title,
        subtitle,
        status,
    }
}

/// The last sixteen hex digits of a fingerprint in groups of four, the
/// way GnuPG prints a long key id, for a key that names nobody.
fn short(fingerprint: &str) -> String {
    let tail = fingerprint
        .get(fingerprint.len().saturating_sub(16)..)
        .unwrap_or(fingerprint);
    tail.as_bytes()
        .chunks(4)
        .map(|group| String::from_utf8_lossy(group).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The row under the others when more came in than they show.
pub fn more_text(more: usize) -> String {
    fill_plural(
        "And {count} more",
        "And {count} more",
        more,
        &[("count", &more.to_string())],
    )
}

/// The words for a file that could not be read from disk.
pub fn unreadable(file: &str, reason: &str) -> Failure {
    Failure::Other(fill(
        &gettext("Could not read {file}: {reason}"),
        &[("file", file), ("reason", reason)],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn import(change: Change, secret: bool) -> Import {
        Import {
            fingerprint: "0D89BEDC149B58A80A07BF825DC5D8408E896ECA".into(),
            change,
            secret,
            name: Some("Ada Lovelace".into()),
            address: Some("ada@example.test".into()),
        }
    }

    #[test]
    fn one_new_key_names_its_owner_and_address() {
        let said = report(Kind::Pgp, Ok(&[import(Change::New, false)]));
        assert_eq!(said.heading, "Key Imported");
        assert_eq!(
            said.lines,
            [Line {
                title: "Ada Lovelace".into(),
                subtitle: Some("ada@example.test".into()),
                status: "New".into(),
            }]
        );
        assert_eq!(
            said.body,
            "You can encrypt to this person and check their signatures."
        );
        assert!(!said.retry);
    }

    #[test]
    fn the_heading_counts_what_changed() {
        let found = [
            import(Change::New, false),
            import(Change::Updated, false),
            import(Change::Unchanged, false),
        ];
        let said = report(Kind::Pgp, Ok(&found));
        assert_eq!(said.heading, "2 Keys Imported");
        assert_eq!(said.lines[1].status, "Updated");
        assert_eq!(said.lines[2].status, "Already here");
    }

    #[test]
    fn a_new_secret_key_says_what_it_is_for() {
        let said = report(Kind::Smime, Ok(&[import(Change::New, true)]));
        assert_eq!(said.heading, "Certificate Imported");
        assert_eq!(said.lines[0].status, "New, with secret key");
        assert_eq!(
            said.body,
            "You can sign and decrypt with the secret key now."
        );
    }

    #[test]
    fn a_file_the_keyring_already_holds_says_nothing_changed() {
        let said = report(Kind::Smime, Ok(&[import(Change::Unchanged, true)]));
        assert_eq!(said.heading, "Already Imported");
        assert_eq!(said.body, "gpgsm already holds everything in this file.");
        assert_eq!(said.lines[0].status, "Already here");
    }

    #[test]
    fn a_wrong_passphrase_offers_another_try() {
        let said = report(
            Kind::Smime,
            Err(Failure::Passphrase(
                "That passphrase does not open this file.".into(),
            )),
        );
        assert_eq!(said.heading, "Certificate Not Imported");
        assert_eq!(said.body, "That passphrase does not open this file.");
        assert!(said.retry);
        assert!(said.lines.is_empty());
    }

    #[test]
    fn a_file_that_is_not_a_key_offers_no_second_try() {
        let said = report(
            Kind::Pgp,
            Err(Failure::Other("This file holds no OpenPGP key.".into())),
        );
        assert_eq!(said.heading, "Key Not Imported");
        assert!(!said.retry);
    }

    #[test]
    fn a_long_file_shows_some_rows_and_counts_the_rest() {
        let found = vec![import(Change::New, false); SHOWN + 3];
        let said = report(Kind::Pgp, Ok(&found));
        assert_eq!(said.lines.len(), SHOWN);
        assert_eq!(said.more, 3);
        assert_eq!(more_text(said.more), "And 3 more");
    }

    #[test]
    fn a_key_without_a_name_goes_by_its_address_or_its_fingerprint() {
        let mut bare = import(Change::New, false);
        bare.name = None;
        assert_eq!(line(&bare).title, "ada@example.test");
        assert_eq!(line(&bare).subtitle, None);
        bare.address = None;
        assert_eq!(line(&bare).title, "5DC5 D840 8E89 6ECA");
    }
}
