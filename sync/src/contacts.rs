//! The address book of each account: reading it from Google, storing it,
//! and keeping the photos on disk.
//!
//! Contacts are off until the account owner turns them on, and turning
//! them on is what makes Google ask for the contacts permission. Until
//! then every call here answers `Permitted::NeedsPermission`, the way the
//! settings calls do.
//!
//! One refresh walks every page, stores what came back, downloads the
//! photos it has not got, and keeps Google's sync token so the next
//! refresh asks only for what changed. A photo is fetched once: the file
//! stays with its contact for as long as Google serves it from the same
//! URL.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mailrs_domain::{AccountId, Address, EpochMillis};
use mailrs_gmail::{ContactFields, GmailError, Person};
use mailrs_store::Db;
use mailrs_store::address_book::{self, Contact};
use mailrs_store::contact_offers::{self, Answer};
use mailrs_store::contacts::is_automated;

use crate::settings::Permitted;
use crate::{Accounts, AnyContacts, BackendError, ContactsService, SyncError, now_millis};

/// How long a stored address book counts as current. Contacts change far
/// more slowly than mail, and a refresh with a sync token costs one call,
/// so six hours keeps the app honest without spending anything.
pub const REFRESH_AFTER: EpochMillis = 6 * 60 * 60 * 1000;

/// Pages one refresh reads before it gives up. An address book past this
/// is either enormous or a page token that never advances.
const PAGE_LIMIT: usize = 100;

/// What one refresh brought back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Refreshed {
    /// Contacts stored, whether or not they changed.
    pub contacts: usize,
    /// Photos downloaded, which is zero on every refresh after the first.
    pub photos: usize,
    /// Accounts whose contacts Google would not hand over until the person
    /// grants the permission. The rest were read regardless.
    pub needs_permission: Vec<AccountId>,
}

/// One contact as a card shows them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    pub contact: Contact,
    /// The photo on disk, when there is one.
    pub photo: Option<PathBuf>,
}

/// What saving a sent message's new recipients did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SavedRecipients {
    /// The contacts made, in the order asked.
    pub saved: Vec<Contact>,
    /// The people the provider would not take, for a reason other than a
    /// missing permission.
    pub failed: Vec<Address>,
}

pub struct ContactBook<A: Accounts> {
    accounts: Arc<A>,
    db: Db,
    photos: PathBuf,
}

impl<A: Accounts> ContactBook<A> {
    /// An address book keeping its photos under `photos`.
    pub fn new(accounts: Arc<A>, db: Db, photos: PathBuf) -> Self {
        ContactBook {
            accounts,
            db,
            photos,
        }
    }

    /// The directory the photos live in. Joining a contact's `photo_file`
    /// to it gives the file to show.
    pub fn photo_dir(&self) -> &Path {
        &self.photos
    }

    /// Reads the account's contacts from Google and stores them, then
    /// downloads the photos that are not on disk yet. The first refresh
    /// reads the whole address book; later ones hand back the sync token
    /// and read only what changed.
    pub async fn refresh(&self, account_id: AccountId) -> Result<Permitted<Refreshed>, SyncError> {
        let contacts = self.contacts(account_id)?;
        let stored = self
            .db
            .read(move |c| address_book::book(c, account_id))
            .await?;
        let mut refreshed = match self
            .read_pages(&contacts, account_id, stored.and_then(|(t, _)| t))
            .await?
        {
            Permitted::Done(refreshed) => refreshed,
            // A token Google will not answer from means reading the whole
            // address book again, which is what an empty token does.
            Permitted::NeedsPermission => return Ok(Permitted::NeedsPermission),
        };
        refreshed.photos = self.fetch_photos(&contacts, account_id).await?;
        Ok(Permitted::Done(refreshed))
    }

    /// Refreshes every account whose address book has aged out, and leaves
    /// the rest alone. Reports what the refreshes added up to.
    pub async fn refresh_stale(
        &self,
        accounts: &[AccountId],
        now: EpochMillis,
    ) -> Result<Refreshed, SyncError> {
        let mut total = Refreshed::default();
        for account_id in accounts {
            let stored = self
                .db
                .read({
                    let account_id = *account_id;
                    move |c| address_book::book(c, account_id)
                })
                .await?;
            if stored.is_some_and(|(_, at)| now.saturating_sub(at) < REFRESH_AFTER) {
                continue;
            }
            match self.refresh(*account_id).await? {
                Permitted::Done(one) => {
                    total.contacts += one.contacts;
                    total.photos += one.photos;
                }
                Permitted::NeedsPermission => total.needs_permission.push(*account_id),
            }
        }
        Ok(total)
    }

    /// Drops everything stored for an account, photos included. Turning
    /// contacts off runs this, so nothing of the address book is left on
    /// the computer.
    pub async fn forget(&self, account_id: AccountId) -> Result<(), SyncError> {
        let files = self
            .db
            .read(move |c| {
                Ok(address_book::list(c)?
                    .into_iter()
                    .filter(|contact| contact.account_id == account_id)
                    .filter_map(|contact| contact.photo_file)
                    .collect::<Vec<_>>())
            })
            .await?;
        for file in files {
            let _ = std::fs::remove_file(self.photos.join(file));
        }
        self.db
            .write(move |c| address_book::clear(c, account_id))
            .await?;
        Ok(())
    }

    /// Adds a contact to the account's Google contacts. With `keep`, the
    /// account's address book on this computer takes the new contact too,
    /// so it shows before the next refresh; an account whose contacts are
    /// off keeps nothing here.
    pub async fn create(
        &self,
        account_id: AccountId,
        fields: &ContactFields,
        keep: bool,
    ) -> Result<Permitted<Contact>, SyncError> {
        let made = self.contacts(account_id)?.create_contact(fields).await;
        self.stored(account_id, made, keep).await
    }

    /// Changes the fields `fields` names on the contact `resource`, and
    /// the stored copy with it when `keep` says the account's contacts
    /// are on.
    pub async fn update(
        &self,
        account_id: AccountId,
        resource: &str,
        fields: &ContactFields,
        keep: bool,
    ) -> Result<Permitted<Contact>, SyncError> {
        let changed = self
            .contacts(account_id)?
            .update_contact(resource, fields)
            .await;
        self.stored(account_id, changed, keep).await
    }

    /// The contact Google sent back, stored when `keep` says so. A missing
    /// permission comes back as a value, the way the refresh reports it.
    async fn stored(
        &self,
        account_id: AccountId,
        person: Result<Person, BackendError>,
        keep: bool,
    ) -> Result<Permitted<Contact>, SyncError> {
        let person = match person {
            Ok(person) => person,
            Err(BackendError::NeedsPermission) => {
                return Ok(Permitted::NeedsPermission);
            }
            Err(err) => return Err(err.into()),
        };
        let contact = Contact {
            account_id,
            resource: person.resource,
            name: person.name,
            emails: person.emails,
            organization: person.organization,
            phone: person.phone,
            photo_url: person.photo_url,
            photo_file: None,
        };
        if keep {
            let saved = contact.clone();
            self.db
                .write(move |c| address_book::save(c, &[saved]))
                .await?;
        }
        Ok(Permitted::Done(contact))
    }

    /// The recipients of a message `account_id` sent that are worth
    /// offering to save to its contacts.
    ///
    /// Someone counts as new when this account's address book lacks them
    /// and the person has not answered an offer about them on this account
    /// before. Correspondents found in mail count as new: only an address
    /// book entry makes someone a contact. An account that keeps no
    /// contacts is offered nobody, and neither are the accounts' own
    /// addresses or no-reply senders. Each address comes once, with the
    /// first name given for it.
    pub async fn new_recipients(
        &self,
        account_id: AccountId,
        recipients: &[Address],
    ) -> Result<Vec<Address>, SyncError> {
        if self.contacts(account_id).is_err() {
            return Ok(Vec::new());
        }
        let wanted = recipients.to_vec();
        let new = self
            .db
            .read(move |c| {
                let answered = contact_offers::answered(c, account_id)?;
                let own: HashSet<String> = mailrs_store::accounts::list_accounts(c)?
                    .into_iter()
                    .map(|a| a.email.to_lowercase())
                    .collect();
                let mut new: Vec<Address> = Vec::new();
                for person in wanted {
                    let key = person.email.trim().to_lowercase();
                    let usable = key.contains('@')
                        && !key.contains(char::is_whitespace)
                        && !own.contains(&key)
                        && !is_automated(&key)
                        && !answered.contains(&key);
                    if !usable || address_book::holds(c, account_id, &key)? {
                        continue;
                    }
                    match new.iter_mut().find(|n| n.email.eq_ignore_ascii_case(&key)) {
                        Some(seen) => {
                            if seen.name.is_none() {
                                seen.name = name_of(&person);
                            }
                        }
                        None => new.push(Address {
                            name: name_of(&person),
                            email: person.email.trim().to_string(),
                        }),
                    }
                }
                Ok(new)
            })
            .await?;
        Ok(new)
    }

    /// Makes a contact of each of `people` on `account_id`, and keeps it in
    /// the address book on this computer, so none of them is offered
    /// again. A missing permission stops the run before anyone else is
    /// tried; whoever was saved by then stays saved.
    pub async fn save_recipients(
        &self,
        account_id: AccountId,
        people: &[Address],
    ) -> Result<Permitted<SavedRecipients>, SyncError> {
        let mut done = SavedRecipients::default();
        let mut stopped = false;
        for person in people {
            let fields = ContactFields {
                name: name_of(person),
                emails: Some(vec![person.email.trim().to_string()]),
                ..ContactFields::default()
            };
            match self.create(account_id, &fields, true).await {
                Ok(Permitted::Done(contact)) => done.saved.push(contact),
                Ok(Permitted::NeedsPermission) => {
                    stopped = true;
                    break;
                }
                Err(err) => {
                    tracing::warn!(error = %err, "could not save a recipient to contacts");
                    done.failed.push(person.clone());
                }
            }
        }
        let saved: Vec<String> = done
            .saved
            .iter()
            .filter_map(|contact| contact.email().map(str::to_string))
            .collect();
        if !saved.is_empty() {
            let at = now_millis();
            self.db
                .write(move |c| contact_offers::record(c, account_id, &saved, Answer::Saved, at))
                .await?;
        }
        Ok(match stopped {
            true => Permitted::NeedsPermission,
            false => Permitted::Done(done),
        })
    }

    /// Records that the person let the offer to save `emails` to
    /// `account_id` go without saving, so it is not made again.
    pub async fn decline_recipients(
        &self,
        account_id: AccountId,
        emails: &[String],
    ) -> Result<(), SyncError> {
        let emails = emails.to_vec();
        let at = now_millis();
        self.db
            .write(move |c| contact_offers::record(c, account_id, &emails, Answer::Declined, at))
            .await?;
        Ok(())
    }

    /// The contact holding `email`, for a card.
    pub async fn card(&self, email: &str) -> Result<Option<Card>, SyncError> {
        let email = email.to_string();
        let contact = self.db.read(move |c| address_book::find(c, &email)).await?;
        Ok(contact.map(|contact| Card {
            photo: contact
                .photo_file
                .as_ref()
                .map(|file| self.photos.join(file))
                .filter(|path| path.exists()),
            contact,
        }))
    }

    /// Walks the pages Google sends and stores them together once the last
    /// one is in, so a read that fails part way leaves the book as it was.
    /// A read without a sync token is the whole address book, and it
    /// replaces what is stored; one with a token brings only the changes.
    async fn read_pages(
        &self,
        contacts: &AnyContacts,
        account_id: AccountId,
        sync_token: Option<String>,
    ) -> Result<Permitted<Refreshed>, SyncError> {
        let mut token = sync_token;
        let mut retried = false;
        'whole: loop {
            let whole = token.is_none();
            let mut page_token: Option<String> = None;
            let mut read: Vec<Contact> = Vec::new();
            let mut deleted: Vec<String> = Vec::new();
            for _ in 0..PAGE_LIMIT {
                let page = match contacts
                    .connections(page_token.as_deref(), token.as_deref())
                    .await
                {
                    Ok(page) => page,
                    Err(BackendError::NeedsPermission) => {
                        return Ok(Permitted::NeedsPermission);
                    }
                    // Google stopped answering from this token. Read the
                    // whole address book again, which then replaces what
                    // is stored.
                    Err(BackendError::Gmail(GmailError::ExpiredSyncToken)) if !retried => {
                        retried = true;
                        token = None;
                        continue 'whole;
                    }
                    // CardDAV's words for the same thing: a token the
                    // server forgot, or an address book that came or went.
                    Err(BackendError::StateLost) if !retried => {
                        retried = true;
                        token = None;
                        continue 'whole;
                    }
                    Err(err) => return Err(err.into()),
                };
                read.extend(page.people.iter().map(|person| Contact {
                    account_id,
                    resource: person.resource.clone(),
                    name: person.name.clone(),
                    emails: person.emails.clone(),
                    organization: person.organization.clone(),
                    phone: person.phone.clone(),
                    photo_url: person.photo_url.clone(),
                    photo_file: None,
                }));
                deleted.extend(page.deleted);
                match page.next_page_token {
                    Some(next) => page_token = Some(next),
                    None => {
                        let stored = read.len();
                        let book = page.next_sync_token;
                        let at = now_millis();
                        self.db
                            .write(move |c| {
                                if whole {
                                    // Whoever the new read left out has gone.
                                    let stale: Vec<String> = address_book::list(c)?
                                        .into_iter()
                                        .filter(|held| held.account_id == account_id)
                                        .filter(|held| {
                                            read.iter().all(|r| r.resource != held.resource)
                                        })
                                        .map(|held| held.resource)
                                        .collect();
                                    address_book::forget(c, account_id, &stale)?;
                                }
                                address_book::save(c, &read)?;
                                address_book::forget(c, account_id, &deleted)?;
                                address_book::set_book(c, account_id, book.as_deref(), at)
                            })
                            .await?;
                        return Ok(Permitted::Done(Refreshed {
                            contacts: stored,
                            ..Refreshed::default()
                        }));
                    }
                }
            }
            // An address book past the page limit keeps what was read, but
            // replaces nothing and keeps no token, so the next refresh
            // walks it again.
            tracing::warn!(
                account = account_id,
                "gave up walking contacts after {PAGE_LIMIT} pages"
            );
            let stored = read.len();
            self.db
                .write(move |c| {
                    address_book::save(c, &read)?;
                    address_book::forget(c, account_id, &deleted)
                })
                .await?;
            return Ok(Permitted::Done(Refreshed {
                contacts: stored,
                ..Refreshed::default()
            }));
        }
    }

    /// Downloads the photos this account has no file for. A photo that
    /// fails is left for the next refresh rather than failing the whole
    /// one: the contact still has a name to show.
    async fn fetch_photos(
        &self,
        contacts: &AnyContacts,
        account_id: AccountId,
    ) -> Result<usize, SyncError> {
        let wanted = self
            .db
            .read(move |c| address_book::missing_photos(c, account_id))
            .await?;
        if wanted.is_empty() {
            return Ok(0);
        }
        if let Err(err) = std::fs::create_dir_all(&self.photos) {
            tracing::warn!(error = %err, "could not make the contact photo directory");
            return Ok(0);
        }
        let mut fetched = 0;
        for (resource, url) in wanted {
            let bytes = match contacts.contact_photo(&url).await {
                Ok(bytes) if !bytes.is_empty() => bytes,
                // The provider holds no photo for this contact; say so once.
                Ok(_) => {
                    let resource = resource.clone();
                    self.db
                        .write(move |c| address_book::set_no_photo(c, account_id, &resource))
                        .await?;
                    continue;
                }
                Err(err) => {
                    tracing::debug!(error = %err, "could not download a contact photo");
                    continue;
                }
            };
            let file = photo_file(account_id, &resource);
            if let Err(err) = std::fs::write(self.photos.join(&file), &bytes) {
                tracing::warn!(error = %err, "could not store a contact photo");
                continue;
            }
            let (resource, saved) = (resource.clone(), file.clone());
            self.db
                .write(move |c| address_book::set_photo_file(c, account_id, &resource, &saved))
                .await?;
            fetched += 1;
        }
        Ok(fetched)
    }

    fn contacts(&self, account_id: AccountId) -> Result<AnyContacts, SyncError> {
        self.accounts
            .services(account_id)
            .ok_or(SyncError::UnknownAccount(account_id))?
            .contacts
            .ok_or(SyncError::Backend(BackendError::Unsupported))
    }
}

/// The name a header gave `person`, unless it is empty or only repeats an
/// address.
fn name_of(person: &Address) -> Option<String> {
    person
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty() && !n.contains('@'))
        .map(str::to_string)
}

/// The file one contact's photo goes in. Google's resource names are
/// already unique per account; anything that is not a letter or a digit
/// comes out so the name is a filename in any directory.
fn photo_file(account_id: AccountId, resource: &str) -> String {
    let tail: String = resource
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    // A CardDAV resource is a whole URL, and its end tells two apart. The
    // name is ASCII by now, so a byte slice is safe.
    let tail = &tail[tail.len().saturating_sub(120)..];
    format!("{account_id}-{tail}.jpg")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_photo_file_is_named_after_its_account_and_contact() {
        assert_eq!(photo_file(2, "people/c17"), "2-people-c17.jpg");
        assert_eq!(photo_file(1, "../etc/passwd"), "1----etc-passwd.jpg");
    }

    #[test]
    fn a_long_resource_makes_a_short_file_name() {
        let name = photo_file(3, &format!("https://dav.example.org/{}.vcf", "a".repeat(400)));
        assert!(name.len() <= 130, "{name}");
        assert!(name.ends_with("aaa-vcf.jpg"));
    }
}
