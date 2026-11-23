//! The server's mailboxes as the store keeps them: roles from SPECIAL-USE
//! (RFC 6154) where the server marks them and from a table of common names
//! where it does not, names decoded from IMAP's modified UTF-7 for a
//! person to read, and `\Noselect` parents listed so their children nest
//! under them.

use mailrs_domain::{MailboxKind, RemoteMailbox, Role};
use mailrs_imap::{Listed, SpecialUse};

use super::{Imap, ImapApi, Submit};
use crate::BackendError;

/// One mailbox as the adapter remembers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Folder {
    /// The server's name, still encoded: what every command names.
    pub id: String,
    pub role: Option<Role>,
    /// `\Noselect` or `\NonExistent`: a parent that holds no mail.
    pub parent_only: bool,
    /// `\Flagged`: a view of the flagged mail in other mailboxes.
    pub flagged: bool,
    /// The person subscribes to it on the server.
    pub subscribed: bool,
    /// Kept out of the sidebar: see [`hide_unsubscribed`].
    pub hidden: bool,
}

/// One folder on the server, as Manage Folders lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderSubscription {
    /// The server's name, still encoded: what SUBSCRIBE takes.
    pub id: String,
    /// The name a person reads, nested by slashes.
    pub name: String,
    pub subscribed: bool,
    /// A role folder, which shows whether subscribed or not, so its
    /// subscription changes nothing a person would see.
    pub always_shown: bool,
    /// A parent that holds no mail, listed for the tree under it.
    pub parent_only: bool,
}

/// Names servers give their role mailboxes without marking them, compared
/// without case. The research lists GMX, Web.de, mail.com, Yandex and
/// older Dovecot and Courier setups as needing them.
const NAMES: &[(Role, &[&str])] = &[
    (
        Role::Sent,
        &[
            "Sent",
            "Sent Items",
            "Sent Messages",
            "Sent Mail",
            "Gesendet",
            "Gesendete Objekte",
            "Gesendete Elemente",
            "Enviados",
            "Envoyés",
            "Éléments envoyés",
            "Inviata",
            "Posta inviata",
            "Verzonden",
            "Verzonden items",
            "Wysłane",
            "Отправленные",
        ],
    ),
    (
        Role::Drafts,
        &[
            "Drafts",
            "Draft",
            "Entwürfe",
            "Borradores",
            "Brouillons",
            "Bozze",
            "Concepten",
            "Kopie robocze",
            "Черновики",
        ],
    ),
    (
        Role::Trash,
        &[
            "Trash",
            "Deleted",
            "Deleted Items",
            "Deleted Messages",
            "Bin",
            "Papierkorb",
            "Gelöschte Objekte",
            "Gelöschte Elemente",
            "Papelera",
            "Corbeille",
            "Cestino",
            "Prullenbak",
            "Verwijderde items",
            "Kosz",
            "Корзина",
            "Удаленные",
        ],
    ),
    (
        Role::Junk,
        &[
            "Junk",
            "Junk E-mail",
            "Junk Mail",
            "Spam",
            "Bulk Mail",
            "Spamverdacht",
            "Correo no deseado",
            "Courrier indésirable",
            "Posta indesiderata",
            "Ongewenste e-mail",
            "Спам",
        ],
    ),
    (
        Role::Archive,
        &[
            "Archive",
            "Archives",
            "Archiv",
            "Archivo",
            "Archivio",
            "Archief",
            "Archiwum",
            "Архив",
        ],
    ),
];

/// The role a SPECIAL-USE mark names. `\Flagged` names a view, not a
/// role; the adapter reads it as the flagged keyword.
fn marked_role(special_use: Option<SpecialUse>) -> Option<Role> {
    match special_use? {
        SpecialUse::Sent => Some(Role::Sent),
        SpecialUse::Drafts => Some(Role::Drafts),
        SpecialUse::Trash => Some(Role::Trash),
        SpecialUse::Junk => Some(Role::Junk),
        SpecialUse::Archive => Some(Role::Archive),
        SpecialUse::All => Some(Role::All),
        SpecialUse::Flagged => None,
    }
}

/// The role a mailbox's name suggests. Only a top-level mailbox, or one
/// right under INBOX as Courier files every mailbox, counts: a person's
/// `Work/Archive` is theirs.
fn named_role(display: &str) -> Option<Role> {
    let name = match display.get(..6) {
        Some(prefix) if prefix.eq_ignore_ascii_case("INBOX/") => &display[6..],
        _ => display,
    };
    if name.contains('/') {
        return None;
    }
    let lower = name.to_lowercase();
    NAMES
        .iter()
        .find(|(_, names)| names.iter().any(|n| n.to_lowercase() == lower))
        .map(|(role, _)| *role)
}

/// A mailbox's name for a person: modified UTF-7 decoded, and the
/// server's hierarchy delimiter shown as the slash the sidebar nests by.
pub(super) fn display_name(name: &str, delimiter: Option<char>) -> String {
    let decoded = mailrs_imap::utf7::decode(name);
    match delimiter {
        Some(d) if d != '/' => decoded.replace(d, "/"),
        _ => decoded,
    }
}

/// The server's name for a mailbox a person named: modified UTF-7, with
/// each slash as the server's delimiter.
pub(super) fn server_name(name: &str, delimiter: Option<char>) -> String {
    let encoded = mailrs_imap::utf7::encode(name);
    match delimiter {
        Some(d) if d != '/' => encoded.replace('/', &d.to_string()),
        _ => encoded,
    }
}

/// The listing as the adapter remembers it and as the store keeps it, in
/// the server's order. Each role goes to one mailbox: the first the
/// server marks with it, else the first the name table knows.
pub(super) fn read_listing(
    listed: &[Listed],
    all_folders: bool,
) -> (Vec<Folder>, Vec<RemoteMailbox>) {
    let mut folders: Vec<Folder> = listed
        .iter()
        .map(|l| Folder {
            id: l.name.clone(),
            // A mailbox that cannot be selected holds no mail, so a mark
            // on it names no mailbox the account could keep in step.
            role: match (l.name.eq_ignore_ascii_case("INBOX"), l.no_select) {
                (true, _) => Some(Role::Inbox),
                (false, true) => None,
                (false, false) => marked_role(l.special_use),
            },
            parent_only: l.no_select,
            flagged: l.special_use == Some(SpecialUse::Flagged),
            subscribed: l.subscribed,
            hidden: false,
        })
        .collect();
    let mut taken: Vec<Role> = Vec::new();
    for folder in &mut folders {
        if let Some(role) = folder.role {
            match taken.contains(&role) {
                true => folder.role = None,
                false => taken.push(role),
            }
        }
    }
    for (folder, l) in folders.iter_mut().zip(listed) {
        if folder.role.is_some() || folder.parent_only || folder.flagged {
            continue;
        }
        if let Some(role) =
            named_role(&display_name(&l.name, l.delimiter)).filter(|r| !taken.contains(r))
        {
            folder.role = Some(role);
            taken.push(role);
        }
    }
    if !all_folders {
        hide_unsubscribed(&mut folders, listed);
    }
    let remote = folders
        .iter()
        .zip(listed)
        .map(|(folder, l)| RemoteMailbox {
            id: folder.id.clone(),
            name: display_name(&l.name, l.delimiter),
            kind: if folder.role.is_some() || folder.flagged {
                MailboxKind::System
            } else if folder.parent_only {
                MailboxKind::Group
            } else {
                MailboxKind::Folder
            },
            role: folder.role,
            color: None,
            hidden: folder.hidden,
        })
        .collect();
    (folders, remote)
}

/// The listing with every missing parent filled in as one that holds no
/// mail. RFC 3501 lets a server leave a parent out of `LIST "" "*"`, and
/// Dovecot does for a shared namespace: `shared/lists/teams/kites`
/// comes without `shared` or `shared/lists/teams`. Without them the
/// sidebar has no row to nest the child under, and it lands under
/// whichever folder precedes it.
pub(super) fn with_parents(mut listed: Vec<Listed>) -> Vec<Listed> {
    let known: std::collections::HashSet<String> = listed.iter().map(|l| l.name.clone()).collect();
    let mut added = std::collections::BTreeSet::new();
    for l in &listed {
        let Some(delimiter) = l.delimiter else {
            continue;
        };
        for (at, _) in l.name.match_indices(delimiter) {
            let parent = &l.name[..at];
            if !parent.is_empty() && !known.contains(parent) {
                added.insert((parent.to_string(), delimiter));
            }
        }
    }
    listed.extend(
        added
            .into_iter()
            .map(|(name, delimiter)| Listed::new(name, Some(delimiter), None, true)),
    );
    listed
}

/// Hides every folder the person does not subscribe to, as Thunderbird
/// does, with two exceptions. A role folder shows whatever its
/// subscription, since the app files mail into it. A folder above one
/// that shows shows too, so the tree holds together. An account
/// subscribed to nothing shows its role folders alone.
fn hide_unsubscribed(folders: &mut [Folder], listed: &[Listed]) {
    let shown: Vec<bool> = folders
        .iter()
        .map(|f| f.subscribed || f.role.is_some() || f.flagged)
        .collect();
    let names: Vec<String> = listed
        .iter()
        .map(|l| display_name(&l.name, l.delimiter))
        .collect();
    for (i, folder) in folders.iter_mut().enumerate() {
        let prefix = format!("{}/", names[i]);
        let above_shown = names
            .iter()
            .zip(&shown)
            .any(|(name, shown)| *shown && name.starts_with(&prefix));
        folder.hidden = !shown[i] && !above_shown;
    }
}

impl<I: ImapApi, S: Submit> Imap<I, S> {
    /// Lists the server's mailboxes and remembers them, so the calls that
    /// answer at once know the roles and the delimiter.
    pub(super) async fn list_mailboxes(&self) -> Result<Vec<RemoteMailbox>, BackendError> {
        let listed = with_parents(self.api.list().await?);
        let (folders, remote) = read_listing(&listed, self.settings.all_folders);
        let delimiter = listed.iter().find_map(|l| l.delimiter);
        let mut known = self.known();
        known
            .opened
            .retain(|id| folders.iter().any(|f| &f.id == id && !f.parent_only));
        known.folders = folders;
        known.delimiter = delimiter;
        Ok(remote)
    }

    /// Makes the mailbox a person named. Slashes nest it under another.
    /// CREATE does not subscribe, so the app does, or the folder the
    /// person just made would stay out of the sidebar. The folder exists
    /// once CREATE succeeds, so a refused SUBSCRIBE does not undo that:
    /// the folder lists as hidden, and Manage Folders can subscribe it.
    pub(super) async fn make_mailbox(&self, name: &str) -> Result<RemoteMailbox, BackendError> {
        let id = server_name(name, self.known().delimiter);
        self.api.create(&id).await?;
        if let Err(err) = self.api.subscribe(&id).await {
            tracing::warn!(%err, "a new folder stays unsubscribed");
        }
        self.listed_as(&id).await
    }

    /// RFC 3501 leaves it to the server whether a subscription follows a
    /// rename, and many leave it behind. Each subscribed folder renamed,
    /// children included, is subscribed again under its new name and its
    /// old name let go.
    ///
    /// The server holds the new name once RENAME succeeds, and the caller
    /// still has to move the stored mailbox and its mail to it, so nothing
    /// after RENAME but the listing may fail the rename. A refused
    /// SUBSCRIBE leaves that folder hidden, for Manage Folders to mend.
    pub(super) async fn rename_mailbox_to(
        &self,
        id: &str,
        name: &str,
    ) -> Result<RemoteMailbox, BackendError> {
        let (renamed, moved) = {
            let known = self.known();
            let renamed = server_name(name, known.delimiter);
            let below = known.delimiter.map(|d| format!("{id}{d}"));
            let moved: Vec<(String, String)> = known
                .folders
                .iter()
                .filter(|f| f.subscribed)
                .filter(|f| f.id == id || below.as_ref().is_some_and(|b| f.id.starts_with(b)))
                .map(|f| (f.id.clone(), format!("{renamed}{}", &f.id[id.len()..])))
                .collect();
            (renamed, moved)
        };
        self.api.rename(id, &renamed).await?;
        self.known().rename(id, &renamed);
        for (old, new) in moved {
            if let Err(err) = self.api.subscribe(&new).await {
                tracing::warn!(%err, "a renamed folder stays unsubscribed");
                continue;
            }
            // A server that moved the subscription itself may refuse to
            // drop a name it no longer holds; the folder is right either way.
            if let Err(err) = self.api.unsubscribe(&old).await {
                tracing::debug!(%err, "the old name's subscription stays");
            }
        }
        self.listed_as(&renamed).await
    }

    /// A subscription outlives its folder (RFC 3501), and a folder made
    /// later under the same name would show at once, so it goes too.
    pub(super) async fn remove_mailbox(&self, id: &str) -> Result<(), BackendError> {
        self.api.delete(id).await?;
        if let Err(err) = self.api.unsubscribe(id).await {
            tracing::debug!(%err, "the deleted folder's subscription stays");
        }
        self.list_mailboxes().await?;
        Ok(())
    }

    /// Every folder on the server, asked afresh, in the server's order.
    pub(crate) async fn subscriptions(&self) -> Result<Vec<FolderSubscription>, BackendError> {
        let listed = with_parents(self.api.list().await?);
        let (folders, _) = read_listing(&listed, self.settings.all_folders);
        Ok(folders
            .into_iter()
            .zip(&listed)
            .map(|(folder, l)| FolderSubscription {
                name: display_name(&l.name, l.delimiter),
                id: folder.id,
                subscribed: folder.subscribed,
                always_shown: folder.role.is_some() || folder.flagged,
                parent_only: folder.parent_only,
            })
            .collect())
    }

    /// Subscribes to `id` or lets it go. The caller lists the folders
    /// again to show the change.
    pub(crate) async fn set_subscribed(&self, id: &str, on: bool) -> Result<(), BackendError> {
        match on {
            true => self.api.subscribe(id).await?,
            false => self.api.unsubscribe(id).await?,
        }
        Ok(())
    }

    /// Lists the mailboxes again and answers the one named `id`.
    async fn listed_as(&self, id: &str) -> Result<RemoteMailbox, BackendError> {
        self.list_mailboxes()
            .await?
            .into_iter()
            .find(|m| m.id == id)
            .ok_or(BackendError::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use mailrs_domain::{MailboxKind, Role};
    use mailrs_imap::{Listed, SpecialUse};

    use super::{display_name, read_listing, server_name, with_parents};

    fn listed(name: &str, delimiter: char, special_use: Option<SpecialUse>) -> Listed {
        Listed::new(name, Some(delimiter), special_use, false)
    }

    fn parent(name: &str, delimiter: char) -> Listed {
        Listed::new(name, Some(delimiter), None, true)
    }

    fn roles(listing: &[Listed]) -> Vec<(String, Option<Role>)> {
        read_listing(listing, false)
            .0
            .into_iter()
            .map(|f| (f.id, f.role))
            .collect()
    }

    #[test]
    fn a_mailbox_that_cannot_be_selected_keeps_no_role_it_is_marked_with() {
        let listing = [
            listed("INBOX", '/', None),
            Listed::new("Archive", Some('/'), Some(SpecialUse::Archive), true),
            listed("Archive/2026", '/', None),
        ];
        assert_eq!(
            roles(&listing),
            [
                ("INBOX".to_string(), Some(Role::Inbox)),
                ("Archive".to_string(), None),
                ("Archive/2026".to_string(), None),
            ]
        );
    }

    #[test]
    fn special_use_names_the_roles_and_a_second_mark_keeps_none() {
        let listing = [
            listed("INBOX", '/', None),
            listed("Sent Items", '/', Some(SpecialUse::Sent)),
            listed("Old Sent", '/', Some(SpecialUse::Sent)),
            listed("Bin", '/', Some(SpecialUse::Trash)),
            listed("Everything", '/', Some(SpecialUse::All)),
        ];
        assert_eq!(
            roles(&listing),
            [
                ("INBOX".to_string(), Some(Role::Inbox)),
                ("Sent Items".to_string(), Some(Role::Sent)),
                ("Old Sent".to_string(), None),
                ("Bin".to_string(), Some(Role::Trash)),
                ("Everything".to_string(), Some(Role::All)),
            ]
        );
    }

    #[test]
    fn without_special_use_the_names_fill_in_in_any_language() {
        let listing = [
            listed("INBOX", '.', None),
            listed("INBOX.Gesendete Objekte", '.', None),
            listed("INBOX.Entw&APw-rfe", '.', None),
            listed("Papelera", '.', None),
            listed("Spam", '.', None),
            listed("&BBAEQARFBDgEMg-", '.', None),
            listed("Work.Archive", '.', None),
        ];
        assert_eq!(
            roles(&listing),
            [
                ("INBOX".to_string(), Some(Role::Inbox)),
                ("INBOX.Gesendete Objekte".to_string(), Some(Role::Sent)),
                ("INBOX.Entw&APw-rfe".to_string(), Some(Role::Drafts)),
                ("Papelera".to_string(), Some(Role::Trash)),
                ("Spam".to_string(), Some(Role::Junk)),
                ("&BBAEQARFBDgEMg-".to_string(), Some(Role::Archive)),
                // Only a top-level mailbox, or one right under INBOX, is
                // taken for a role by its name.
                ("Work.Archive".to_string(), None),
            ]
        );
    }

    #[test]
    fn a_noselect_parent_lists_as_a_group_that_holds_no_mail() {
        let (folders, remote) = read_listing(
            &[
                listed("INBOX", '/', None),
                parent("Archive", '/'),
                listed("Archive/2025", '/', None),
            ],
            false,
        );
        assert!(folders[1].parent_only);
        assert_eq!(folders[1].role, None, "a parent holds no mail to archive");
        assert_eq!(remote[1].kind, MailboxKind::Group);
        assert_eq!(remote[2].kind, MailboxKind::Folder);
        assert_eq!(remote[2].name, "Archive/2025");
    }

    #[test]
    fn a_flagged_view_is_a_system_mailbox_without_a_role() {
        let (folders, remote) = read_listing(
            &[
                listed("INBOX", '/', None),
                listed("Starred", '/', Some(SpecialUse::Flagged)),
            ],
            false,
        );
        assert!(folders[1].flagged);
        assert_eq!(
            (remote[1].kind, remote[1].role),
            (MailboxKind::System, None)
        );
    }

    #[test]
    fn names_show_decoded_with_slashes_and_go_back_encoded_with_the_delimiter() {
        assert_eq!(
            display_name("INBOX.Entw&APw-rfe", Some('.')),
            "INBOX/Entwürfe"
        );
        assert_eq!(display_name("Reports/Q1", Some('/')), "Reports/Q1");
        assert_eq!(server_name("Entwürfe/2026", Some('.')), "Entw&APw-rfe.2026");
        assert_eq!(server_name("R&D", Some('/')), "R&-D");
    }

    #[test]
    fn an_account_set_to_list_every_folder_hides_none() {
        let listing = [listed("INBOX", '/', None), listed("Old", '/', None)];
        let remote = read_listing(&listing, true).1;
        assert!(remote.iter().all(|m| !m.hidden), "{remote:?}");
    }

    #[test]
    fn a_parent_the_server_leaves_out_is_filled_in_holding_no_mail() {
        let listing = with_parents(vec![
            listed("INBOX", '/', None),
            listed("shared/lists/dev", '/', None),
            listed("shared/lists/teams/kites", '/', None).subscribed(true),
            parent("shared/lists", '/'),
        ]);
        let names: Vec<_> = listing
            .iter()
            .map(|l| (l.name.as_str(), l.no_select))
            .collect();
        assert_eq!(
            names,
            [
                ("INBOX", false),
                ("shared/lists/dev", false),
                ("shared/lists/teams/kites", false),
                ("shared/lists", true),
                ("shared", true),
                ("shared/lists/teams", true),
            ]
        );
        let shown: Vec<_> = read_listing(&listing, false)
            .1
            .into_iter()
            .filter(|m| !m.hidden)
            .map(|m| (m.id, m.kind))
            .collect();
        assert_eq!(
            shown,
            [
                ("INBOX".to_string(), MailboxKind::System),
                ("shared/lists/teams/kites".into(), MailboxKind::Folder),
                ("shared/lists".into(), MailboxKind::Group),
                ("shared".into(), MailboxKind::Group),
                ("shared/lists/teams".into(), MailboxKind::Group),
            ]
        );
    }

    fn hidden(listing: &[Listed]) -> Vec<(String, bool)> {
        read_listing(listing, false)
            .1
            .into_iter()
            .map(|m| (m.id, m.hidden))
            .collect()
    }

    #[test]
    fn an_account_subscribed_to_nothing_shows_its_role_folders_alone() {
        let listing = [
            listed("INBOX", '/', None),
            listed("Sent", '/', Some(SpecialUse::Sent)),
            listed("Trash", '/', None),
            listed("Projects", '/', None),
        ];
        assert_eq!(
            hidden(&listing),
            [
                ("INBOX".into(), false),
                ("Sent".into(), false),
                ("Trash".into(), false),
                ("Projects".into(), true),
            ]
        );
    }

    #[test]
    fn a_parent_shows_for_a_subscribed_child_whatever_the_delimiter() {
        let listing = [
            parent("INBOX.Lists", '.'),
            listed("INBOX.Lists.Rust", '.', None).subscribed(true),
            listed("INBOX.Listsmore", '.', None),
        ];
        assert_eq!(
            hidden(&listing),
            [
                ("INBOX.Lists".into(), false),
                ("INBOX.Lists.Rust".into(), false),
                ("INBOX.Listsmore".into(), true),
            ]
        );
    }
}
