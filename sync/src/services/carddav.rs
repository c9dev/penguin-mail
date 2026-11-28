//! An address book served over CardDAV, behind the contacts service the
//! address book reads. Every address book under the account's home reads
//! as one, with one sync token that holds each book's own; a book that
//! came or went since makes the next read a whole one. Creating a contact
//! puts a new vCard 3.0 in the first book; an edit rewrites only the
//! fields it changes. A photo comes only from inside a card.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};

use mailrs_dav::ids::join;
use mailrs_dav::vcard::{self, CardFields, Photo};
use mailrs_dav::{DavApi, Kind, MULTIGET_BATCH, Precondition};
use mailrs_domain::translate::{fill, gettext};
use mailrs_gmail::{ConnectionsPage, ContactFields, Person};

use super::ContactsService;
use super::dav_read::{TOKEN_CTAG, TOKEN_SYNC, backend};
use crate::BackendError;

const PHOTO_SCHEME: &str = "carddav-photo:";

pub struct CardDav<D> {
    api: Arc<D>,
    /// Google's CardDAV, which answers differently from the standard in the
    /// ways `google_fields` and `create_contact` name.
    google: bool,
    refused: Arc<Mutex<Option<String>>>,
    pending: Arc<Mutex<Option<Pending>>>,
    serial: Arc<Mutex<u64>>,
}

/// One read of every book: (book, href) to fetch, what went, the tokens
/// after, and the books this account may only read.
struct Pending {
    serial: u64,
    queue: VecDeque<(String, String)>,
    deleted: Vec<String>,
    tokens: BTreeMap<String, String>,
    read_only: Vec<String>,
}

/// The group cards of one address book, and whether that book keeps its
/// groups as cards. iCloud does even before it holds one.
struct GroupCards {
    book: String,
    cards: Vec<(mailrs_dav::Fetched, String, Vec<String>)>,
    native: bool,
}

/// A vCard error as a backend one. A change refused before sending keeps
/// its words, since they are already the reader's.
fn card_error(err: mailrs_dav::DavError) -> BackendError {
    match err {
        mailrs_dav::DavError::Invalid(why) => BackendError::Invalid(why),
        other => BackendError::Refused(other.to_string()),
    }
}

impl<D> Clone for CardDav<D> {
    fn clone(&self) -> Self {
        CardDav {
            api: Arc::clone(&self.api),
            google: self.google,
            refused: Arc::clone(&self.refused),
            pending: Arc::clone(&self.pending),
            serial: Arc::clone(&self.serial),
        }
    }
}

fn fields(from: &ContactFields) -> CardFields {
    CardFields {
        details: from.details.clone(),
        photo: from.photo.clone(),
        name: from.name.clone(),
        emails: from.emails.clone(),
        phones: from.phones.clone(),
        organization: from.organization.clone(),
    }
}

fn person(href: &str, mut card: vcard::Card, revision: &str) -> Person {
    card.details.revision = revision.to_string();
    Person {
        resource: href.to_string(),
        photo_url: match card.photo {
            Some(Photo::Inline(_)) if card.details.members.is_none() => {
                Some(format!("{PHOTO_SCHEME}{href}\n{revision}"))
            }
            _ => None,
        },
        details: card.details,
        name: card.name,
        emails: card.emails,
        organization: card.organization,
        phone: card.phones.into_iter().next(),
    }
}

impl<D: DavApi> CardDav<D> {
    pub fn new(api: Arc<D>) -> CardDav<D> {
        CardDav {
            api,
            google: false,
            refused: Arc::default(),
            pending: Arc::default(),
            serial: Arc::default(),
        }
    }

    /// This server is Google's CardDAV. Seen live on 2026-11-27:
    /// - a create is stored under Google's own name, given in `Location`,
    ///   with the UID rewritten to it, so the card is never looked up by
    ///   the UID that was sent;
    /// - the ETag is a timestamp, and `If-Match` is ignored, so a stale
    ///   edit overwrites with a 204 and there is no 412 to wait for;
    /// - FN is rebuilt from N, so a name is written to both;
    /// - a birthday without a year is deleted, and CATEGORIES are dropped,
    ///   so groups stay on this computer.
    pub fn google(mut self) -> CardDav<D> {
        self.google = true;
        self
    }

    /// What Google's CardDAV would lose, refused or held back before the
    /// card is sent.
    fn google_fields(&self, fields: &mut CardFields) -> Result<(), BackendError> {
        if !self.google {
            return Ok(());
        }
        if let Some(birthday) = &fields.details.birthday
            && mailrs_domain::contact::date_parts(birthday)
                .is_some_and(|(year, _, _)| year.is_none())
        {
            return Err(BackendError::Invalid(gettext(
                "Google keeps a birthday only with its year. Add the year or remove the birthday.",
            )));
        }
        // CATEGORIES are dropped, so the groups are kept here.
        fields.details.groups = None;
        Ok(())
    }

    pub fn login_refused(&self) -> Option<String> {
        self.refused
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn err(&self, err: mailrs_dav::DavError) -> BackendError {
        backend(err, &self.refused)
    }

    /// Every book under the home, by href, and whether this account may
    /// write to it.
    async fn books(&self) -> Result<Vec<(String, bool)>, BackendError> {
        let homes = self.api.homes().await.map_err(|e| self.err(e))?;
        let home = homes.addressbook.ok_or(BackendError::Unsupported)?;
        let mut books: Vec<(String, bool)> = self
            .api
            .collections(&home, Kind::AddressBook)
            .await
            .map_err(|e| self.err(e))?
            .into_iter()
            .map(|c| (c.href, c.can_write))
            .collect();
        books.sort();
        *self.refused.lock().unwrap_or_else(PoisonError::into_inner) = None;
        Ok(books)
    }

    /// Reads the group cards in the book that holds `resource`.
    async fn group_cards(&self, resource: &str) -> Result<GroupCards, BackendError> {
        let book = resource
            .rsplit_once('/')
            .map(|(parent, _)| format!("{parent}/"))
            .ok_or(BackendError::Unsupported)?;
        let members = self
            .api
            .members(&book, Kind::AddressBook, None)
            .await
            .map_err(|e| self.err(e))?;
        let mut cards = Vec::new();
        for batch in members.chunks(MULTIGET_BATCH) {
            let hrefs: Vec<_> = batch.iter().map(|m| m.href.clone()).collect();
            let fetched = self
                .api
                .fetch(&book, Kind::AddressBook, &hrefs)
                .await
                .map_err(|e| self.err(e))?;
            for resource in fetched {
                let card = vcard::read_card(&resource.body).map_err(card_error)?;
                if let Some(members) = card.details.members {
                    cards.push((resource, card.name.unwrap_or_default(), members));
                }
            }
        }
        let icloud = self.api.host() == "icloud.com" || self.api.host().ends_with(".icloud.com");
        let native = !cards.is_empty() || icloud;
        Ok(GroupCards {
            book,
            cards,
            native,
        })
    }

    /// Puts `uid` in the group cards named in `wanted` and takes it out of
    /// the others, making a card for a name no card has yet.
    async fn write_groups(
        &self,
        groups: GroupCards,
        uid: &str,
        wanted: &[String],
    ) -> Result<(), BackendError> {
        let held_memberships = memberships(&groups, uid);
        let mut held = std::collections::HashSet::new();
        for (resource, name, members) in groups.cards {
            let member = members.iter().any(|m| m == uid);
            // Of two cards with one name, only the one already holding the
            // contact keeps it.
            let include = wanted.contains(&name) && (!held_memberships.contains(&name) || member);
            held.insert(name);
            if include == member {
                continue;
            }
            let body = vcard::group_membership(&resource.body, uid, include).map_err(card_error)?;
            self.api
                .put(
                    &resource.href,
                    &body,
                    Kind::AddressBook,
                    Precondition::Match(resource.etag),
                )
                .await
                .map_err(|e| self.err(e))?;
        }
        for name in wanted.iter().filter(|name| !held.contains(*name)) {
            let group_uid = format!("pm-{:032x}", rand::random::<u128>());
            let href = join(&groups.book, &format!("{group_uid}.vcf"));
            let body = vcard::new_group(&group_uid, name, uid);
            self.api
                .put(&href, &body, Kind::AddressBook, Precondition::NoneMatch)
                .await
                .map_err(|e| self.err(e))?;
        }
        Ok(())
    }

    /// Lists every book's changes since `tokens`, or every member without.
    async fn listing(
        &self,
        tokens: Option<BTreeMap<String, String>>,
    ) -> Result<Pending, BackendError> {
        let books = self.books().await?;
        if let Some(held) = &tokens {
            let held_books: Vec<&String> = held.keys().collect();
            if held_books != books.iter().map(|(book, _)| book).collect::<Vec<_>>() {
                return Err(BackendError::StateLost);
            }
        }
        let mut pending = Pending {
            serial: 0,
            queue: VecDeque::new(),
            deleted: Vec::new(),
            tokens: BTreeMap::new(),
            read_only: books
                .iter()
                .filter(|(_, writable)| !writable)
                .map(|(book, _)| book.clone())
                .collect(),
        };
        for (book, _) in books {
            let held = tokens.as_ref().and_then(|t| t.get(&book)).cloned();
            match held {
                Some(t) if t.starts_with(TOKEN_SYNC) => {
                    // A server that cuts its answer short (507) is asked again
                    // from the token it gave, as often as a calendar is; a
                    // read that stops there goes on from that token next time.
                    let mut from = t[TOKEN_SYNC.len()..].to_string();
                    for _ in 0..super::caldav::SYNC_ROUNDS {
                        let synced = self.api.sync(&book, &from).await.map_err(|e| self.err(e))?;
                        pending
                            .queue
                            .extend(synced.changed.into_iter().map(|m| (book.clone(), m.href)));
                        pending.deleted.extend(synced.removed);
                        let more = synced.more;
                        from = synced.token;
                        if !more {
                            break;
                        }
                    }
                    pending.tokens.insert(book, format!("{TOKEN_SYNC}{from}"));
                }
                Some(t) if t.starts_with(TOKEN_CTAG) => {
                    let state = self.api.state(&book).await.map_err(|e| self.err(e))?;
                    if state.ctag.as_deref() != Some(&t[TOKEN_CTAG.len()..]) {
                        return Err(BackendError::StateLost);
                    }
                    pending.tokens.insert(book, t);
                }
                Some(_) => return Err(BackendError::StateLost),
                None => {
                    let state = self.api.state(&book).await.map_err(|e| self.err(e))?;
                    let members = self
                        .api
                        .members(&book, Kind::AddressBook, None)
                        .await
                        .map_err(|e| self.err(e))?;
                    pending
                        .queue
                        .extend(members.into_iter().map(|m| (book.clone(), m.href)));
                    let token = match state.sync_token {
                        Some(sync) => format!("{TOKEN_SYNC}{sync}"),
                        None => format!("{TOKEN_CTAG}{}", state.ctag.unwrap_or_default()),
                    };
                    pending.tokens.insert(book, token);
                }
            }
        }
        Ok(pending)
    }
}

/// The names of the group cards that hold `uid`.
fn memberships(groups: &GroupCards, uid: &str) -> std::collections::HashSet<String> {
    groups
        .cards
        .iter()
        .filter(|(_, _, members)| members.iter().any(|m| m == uid))
        .map(|(_, name, _)| name.clone())
        .collect()
}

/// Refuses a group change made from an out-of-date view, and one that
/// cannot tell two groups of one name apart. `expected` holds the groups
/// the editor showed, `categories` the card's own CATEGORIES.
fn check_groups(
    groups: &GroupCards,
    uid: &str,
    wanted: &[String],
    expected: Option<&[String]>,
    categories: &[String],
) -> Result<(), BackendError> {
    let held = memberships(groups, uid);
    if let Some(expected) = expected {
        let mut actual = categories.to_vec();
        actual.extend(held.iter().cloned());
        actual.sort();
        actual.dedup();
        let mut expected = expected.to_vec();
        expected.sort();
        expected.dedup();
        if actual != expected {
            return Err(BackendError::Refused(gettext(
                "Contact groups changed on another device. Refresh contacts and reopen the editor.",
            )));
        }
    }
    for name in wanted {
        if !held.contains(name) && groups.cards.iter().filter(|(_, n, _)| n == name).count() > 1 {
            return Err(BackendError::Refused(gettext(
                "Several groups have the same name. Rename them in the account before adding this contact.",
            )));
        }
    }
    Ok(())
}

/// Splits `wanted` into the CATEGORIES the card keeps and the names that go
/// to group cards. In a book with group cards, a name only stays a category
/// when the card already had it and no group card carries it; every other
/// name goes to a card.
fn split_groups(
    groups: &GroupCards,
    wanted: &[String],
    categories: &[String],
) -> (Vec<String>, Vec<String>) {
    if !groups.native {
        return (wanted.to_vec(), Vec::new());
    }
    wanted.iter().cloned().partition(|name| {
        categories.contains(name) && !groups.cards.iter().any(|(_, card, _)| card == name)
    })
}

impl<D: DavApi> ContactsService for CardDav<D> {
    async fn address_books(&self) -> Result<Vec<(String, String)>, BackendError> {
        let homes = self.api.homes().await.map_err(|e| self.err(e))?;
        let home = homes.addressbook.ok_or(BackendError::Unsupported)?;
        let books = self
            .api
            .collections(&home, Kind::AddressBook)
            .await
            .map_err(|e| self.err(e))?;
        Ok(books
            .into_iter()
            .filter(|b| b.can_write)
            .map(|b| (b.href, b.name))
            .collect())
    }

    fn edits_details(&self) -> bool {
        true
    }

    fn keeps_groups_here(&self) -> bool {
        self.google
    }

    async fn delete_contact(
        &self,
        resource: &str,
        revision: Option<&str>,
    ) -> Result<(), BackendError> {
        let current = self.api.get(resource).await.map_err(|e| self.err(e))?;
        if revision.is_some_and(|etag| etag != current.etag) {
            return Err(self.err(mailrs_dav::DavError::Changed));
        }
        self.api
            .delete(resource, Some(current.etag.as_str()))
            .await
            .map_err(|e| self.err(e))
    }

    async fn connections(
        &self,
        page_token: Option<&str>,
        sync_token: Option<&str>,
    ) -> Result<ConnectionsPage, BackendError> {
        if page_token.is_none() {
            let tokens = match sync_token {
                Some(text) => Some(
                    serde_json::from_str::<BTreeMap<String, String>>(text)
                        .map_err(|_| BackendError::StateLost)?,
                ),
                None => None,
            };
            let mut pending = self.listing(tokens).await?;
            let mut serial = self.serial.lock().unwrap_or_else(PoisonError::into_inner);
            *serial += 1;
            pending.serial = *serial;
            *self.pending.lock().unwrap_or_else(PoisonError::into_inner) = Some(pending);
        }
        let wanted = page_token
            .map(|p| p.parse::<u64>().map_err(|_| BackendError::StateLost))
            .transpose()?;
        let (book, batch, deleted, done, serial, tokens, read_only) = {
            let mut held = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
            let pending = held
                .as_mut()
                .filter(|p| wanted.is_none_or(|w| w == p.serial))
                .ok_or(BackendError::StateLost)?;
            // One book per multiget.
            let book = pending
                .queue
                .front()
                .map(|(b, _)| b.clone())
                .unwrap_or_default();
            let mut batch = Vec::new();
            while batch.len() < MULTIGET_BATCH
                && pending.queue.front().is_some_and(|(b, _)| *b == book)
            {
                if let Some((_, href)) = pending.queue.pop_front() {
                    batch.push(href);
                }
            }
            let deleted = std::mem::take(&mut pending.deleted);
            let done = pending.queue.is_empty();
            let (serial, tokens) = (pending.serial, pending.tokens.clone());
            let read_only = pending.read_only.contains(&book);
            if done {
                *held = None;
            }
            (book, batch, deleted, done, serial, tokens, read_only)
        };
        let fetched = match batch.is_empty() {
            true => Vec::new(),
            false => self
                .api
                .fetch(&book, Kind::AddressBook, &batch)
                .await
                .map_err(|e| self.err(e))?,
        };
        let people = fetched
            .into_iter()
            .filter_map(|f| match vcard::read_card(&f.body) {
                Ok(mut card) => {
                    card.details.read_only = read_only;
                    Some(person(&f.href, card, &f.etag))
                }
                Err(err) => {
                    tracing::warn!(href = %f.href, %err, "skipped a vCard Penguin Mail cannot read");
                    None
                }
            })
            .collect();
        Ok(ConnectionsPage {
            people,
            deleted,
            next_page_token: (!done).then(|| serial.to_string()),
            next_sync_token: done.then(|| serde_json::to_string(&tokens).unwrap_or_default()),
        })
    }

    async fn contact_photo(&self, url: &str) -> Result<Vec<u8>, BackendError> {
        let href = url
            .strip_prefix(PHOTO_SCHEME)
            .ok_or(BackendError::Unsupported)?;
        let href = href.split_once('\n').map_or(href, |(href, _)| href);
        let card = self.api.get(href).await.map_err(|e| self.err(e))?;
        match vcard::read_card(&card.body).map_err(card_error)?.photo {
            Some(Photo::Inline(bytes)) => Ok(bytes),
            _ => Err(BackendError::NotFound),
        }
    }

    async fn create_contact(&self, fields_in: &ContactFields) -> Result<Person, BackendError> {
        let mut fields = fields(fields_in);
        self.google_fields(&mut fields)?;
        vcard::validate(&fields).map_err(card_error)?;
        let writable: Vec<String> = self
            .books()
            .await?
            .into_iter()
            .filter(|(_, writable)| *writable)
            .map(|(book, _)| book)
            .collect();
        let book = match &fields_in.address_book {
            Some(book) if writable.contains(book) => book.clone(),
            Some(_) => return Err(BackendError::NotFound),
            None => writable
                .into_iter()
                .next()
                .ok_or(BackendError::Unsupported)?,
        };
        let uid = match &fields_in.creation_key {
            Some(key)
                if key.len() == 36 && key.bytes().all(|c| c.is_ascii_hexdigit() || c == b'-') =>
            {
                format!("pm-{key}")
            }
            Some(_) => return Err(BackendError::Unsupported),
            None => format!("pm-{:032x}", rand::random::<u128>()),
        };
        let href = join(&book, &format!("{uid}.vcf"));
        let mut cards = None;
        if let Some(wanted) = fields_in.details.groups.as_ref().filter(|_| !self.google) {
            let groups = self.group_cards(&href).await?;
            check_groups(&groups, &uid, wanted, None, &[])?;
            let (categories, named) = split_groups(&groups, wanted, &[]);
            fields.details.groups = Some(categories);
            cards = groups.native.then_some((groups, named));
        }
        let body = vcard::new_card(&uid, &fields);
        vcard::read_card(&body).map_err(card_error)?;
        let mut href = href;
        match self
            .api
            .put_at(&href, &body, Kind::AddressBook, Precondition::NoneMatch)
            .await
        {
            // A server that names the card itself says where.
            Ok((_, Some(placed))) => href = placed,
            Ok(_) => {}
            Err(mailrs_dav::DavError::Changed) if fields_in.creation_key.is_some() => {
                // A stable editor key addresses only this creation, including a retry after a lost reply.
                let saved = self.api.get(&href).await.map_err(|e| self.err(e))?;
                let changed = vcard::patch_card(&saved.body, &fields).map_err(card_error)?;
                let before = vcard::read_card(&saved.body).map_err(card_error)?;
                let after = vcard::read_card(&changed).map_err(card_error)?;
                if before != after {
                    return Err(self.err(mailrs_dav::DavError::Changed));
                }
            }
            Err(err) => return Err(self.err(err)),
        }
        if let Some((groups, named)) = cards {
            self.write_groups(groups, &uid, &named).await.map_err(|err| {
                BackendError::Refused(fill(
                    &gettext("The contact was created, but some groups could not be saved. Retry in this editor to finish without creating another contact: {reason}"),
                    &[("reason", &err.to_string())],
                ))
            })?;
        }
        let saved = self.api.get(&href).await.map_err(|e| self.err(e))?;
        let card = vcard::read_card(&saved.body).map_err(card_error)?;
        Ok(person(&href, card, &saved.etag))
    }

    async fn update_contact(
        &self,
        resource: &str,
        fields_in: &ContactFields,
    ) -> Result<Person, BackendError> {
        let current = self.api.get(resource).await.map_err(|e| self.err(e))?;
        if fields_in
            .expected_revision
            .as_ref()
            .is_some_and(|etag| etag != &current.etag)
        {
            return Err(self.err(mailrs_dav::DavError::Changed));
        }
        let mut fields = fields(fields_in);
        self.google_fields(&mut fields)?;
        // Group cards are read and checked first and written only after
        // the contact, so a contact that changed elsewhere changes nothing.
        let mut cards = None;
        if let Some(wanted) = fields_in.details.groups.as_ref().filter(|_| !self.google) {
            let card = vcard::read_card(&current.body).map_err(card_error)?;
            let uid = card.uid.ok_or(BackendError::Unsupported)?;
            let groups = self.group_cards(resource).await?;
            check_groups(
                &groups,
                &uid,
                wanted,
                fields_in.expected_groups.as_deref(),
                &card.details.groups,
            )?;
            let (categories, named) = split_groups(&groups, wanted, &card.details.groups);
            fields.details.groups = Some(categories);
            cards = groups.native.then_some((groups, uid, named));
        }
        let body = vcard::patch_card(&current.body, &fields).map_err(card_error)?;
        // Google ignores If-Match, so the ETag read above is the whole check
        // and a save between that read and this PUT is overwritten: the
        // check is not atomic there.
        self.api
            .put(
                resource,
                &body,
                Kind::AddressBook,
                Precondition::Match(current.etag),
            )
            .await
            .map_err(|e| self.err(e))?;
        if let Some((groups, uid, named)) = cards {
            self.write_groups(groups, &uid, &named).await.map_err(|err| {
                BackendError::Refused(fill(
                    &gettext("Some group changes could not be saved. Refresh contacts before trying again: {reason}"),
                    &[("reason", &err.to_string())],
                ))
            })?;
        }
        let saved = self.api.get(resource).await.map_err(|e| self.err(e))?;
        let card = vcard::read_card(&saved.body).map_err(card_error)?;
        Ok(person(resource, card, &saved.etag))
    }
}
