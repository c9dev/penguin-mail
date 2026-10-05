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
use mailrs_gmail::{ConnectionsPage, ContactFields, Person};

use super::ContactsService;
use super::dav_read::{TOKEN_CTAG, TOKEN_SYNC, backend};
use crate::BackendError;

const PHOTO_SCHEME: &str = "carddav-photo:";

pub struct CardDav<D> {
    api: Arc<D>,
    refused: Arc<Mutex<Option<String>>>,
    pending: Arc<Mutex<Option<Pending>>>,
    serial: Arc<Mutex<u64>>,
}

/// One read of every book: (book, href) to fetch, what went, the tokens after.
struct Pending {
    serial: u64,
    queue: VecDeque<(String, String)>,
    deleted: Vec<String>,
    tokens: BTreeMap<String, String>,
}

impl<D> Clone for CardDav<D> {
    fn clone(&self) -> Self {
        CardDav { api: Arc::clone(&self.api), refused: Arc::clone(&self.refused), pending: Arc::clone(&self.pending), serial: Arc::clone(&self.serial) }
    }
}

fn fields(from: &ContactFields) -> CardFields {
    CardFields { name: from.name.clone(), emails: from.emails.clone(), phones: from.phones.clone(), organization: from.organization.clone() }
}

fn person(href: &str, card: vcard::Card) -> Person {
    Person {
        resource: href.to_string(),
        photo_url: match card.photo {
            Some(Photo::Inline(_)) => Some(format!("{PHOTO_SCHEME}{href}")),
            _ => None,
        },
        name: card.name,
        emails: card.emails,
        organization: card.organization,
        phone: card.phones.into_iter().next(),
    }
}

impl<D: DavApi> CardDav<D> {
    pub fn new(api: Arc<D>) -> CardDav<D> {
        CardDav { api, refused: Arc::default(), pending: Arc::default(), serial: Arc::default() }
    }

    pub fn login_refused(&self) -> Option<String> {
        self.refused.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn err(&self, err: mailrs_dav::DavError) -> BackendError {
        backend(err, &self.refused)
    }

    async fn books(&self) -> Result<Vec<String>, BackendError> {
        let homes = self.api.homes().await.map_err(|e| self.err(e))?;
        let home = homes.addressbook.ok_or(BackendError::Unsupported)?;
        let mut books: Vec<String> = self.api.collections(&home, Kind::AddressBook).await.map_err(|e| self.err(e))?.into_iter().map(|c| c.href).collect();
        books.sort();
        *self.refused.lock().unwrap_or_else(PoisonError::into_inner) = None;
        Ok(books)
    }

    /// Lists every book's changes since `tokens`, or every member without.
    async fn listing(&self, tokens: Option<BTreeMap<String, String>>) -> Result<Pending, BackendError> {
        let books = self.books().await?;
        if let Some(held) = &tokens {
            let held_books: Vec<&String> = held.keys().collect();
            if held_books != books.iter().collect::<Vec<_>>() {
                return Err(BackendError::StateLost);
            }
        }
        let mut pending = Pending { serial: 0, queue: VecDeque::new(), deleted: Vec::new(), tokens: BTreeMap::new() };
        for book in books {
            let held = tokens.as_ref().and_then(|t| t.get(&book)).cloned();
            match held {
                Some(t) if t.starts_with(TOKEN_SYNC) => {
                    // A server that cuts its answer short (507) is asked again
                    // from the token it gave.
                    let mut from = t[TOKEN_SYNC.len()..].to_string();
                    loop {
                        let synced = self.api.sync(&book, &from).await.map_err(|e| self.err(e))?;
                        pending.queue.extend(synced.changed.into_iter().map(|m| (book.clone(), m.href)));
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
                    let members = self.api.members(&book, Kind::AddressBook, None).await.map_err(|e| self.err(e))?;
                    pending.queue.extend(members.into_iter().map(|m| (book.clone(), m.href)));
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

impl<D: DavApi> ContactsService for CardDav<D> {
    async fn connections(&self, page_token: Option<&str>, sync_token: Option<&str>) -> Result<ConnectionsPage, BackendError> {
        if page_token.is_none() {
            let tokens = match sync_token {
                Some(text) => Some(serde_json::from_str::<BTreeMap<String, String>>(text).map_err(|_| BackendError::StateLost)?),
                None => None,
            };
            let mut pending = self.listing(tokens).await?;
            let mut serial = self.serial.lock().unwrap_or_else(PoisonError::into_inner);
            *serial += 1;
            pending.serial = *serial;
            *self.pending.lock().unwrap_or_else(PoisonError::into_inner) = Some(pending);
        }
        let wanted = page_token.map(|p| p.parse::<u64>().map_err(|_| BackendError::StateLost)).transpose()?;
        let (book, batch, deleted, done, serial, tokens) = {
            let mut held = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
            let pending = held.as_mut().filter(|p| wanted.is_none_or(|w| w == p.serial)).ok_or(BackendError::StateLost)?;
            // One book per multiget.
            let book = pending.queue.front().map(|(b, _)| b.clone()).unwrap_or_default();
            let mut batch = Vec::new();
            while batch.len() < MULTIGET_BATCH && pending.queue.front().is_some_and(|(b, _)| *b == book) {
                if let Some((_, href)) = pending.queue.pop_front() {
                    batch.push(href);
                }
            }
            let deleted = std::mem::take(&mut pending.deleted);
            let done = pending.queue.is_empty();
            let (serial, tokens) = (pending.serial, pending.tokens.clone());
            if done {
                *held = None;
            }
            (book, batch, deleted, done, serial, tokens)
        };
        let fetched = match batch.is_empty() {
            true => Vec::new(),
            false => self.api.fetch(&book, Kind::AddressBook, &batch).await.map_err(|e| self.err(e))?,
        };
        let people = fetched
            .into_iter()
            .filter_map(|f| match vcard::read_card(&f.body) {
                Ok(card) => Some(person(&f.href, card)),
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
        let href = url.strip_prefix(PHOTO_SCHEME).ok_or(BackendError::Unsupported)?;
        let card = self.api.get(href).await.map_err(|e| self.err(e))?;
        match vcard::read_card(&card.body).map_err(|e| BackendError::Refused(e.to_string()))?.photo {
            Some(Photo::Inline(bytes)) => Ok(bytes),
            _ => Err(BackendError::NotFound),
        }
    }

    async fn create_contact(&self, fields_in: &ContactFields) -> Result<Person, BackendError> {
        let book = self.books().await?.into_iter().next().ok_or(BackendError::Unsupported)?;
        let uid = format!("pm-{:032x}", rand::random::<u128>());
        let href = join(&book, &format!("{uid}.vcf"));
        let body = vcard::new_card(&uid, &fields(fields_in));
        self.api.put(&href, &body, Kind::AddressBook, Precondition::NoneMatch).await.map_err(|e| self.err(e))?;
        let card = vcard::read_card(&body).map_err(|e| BackendError::Refused(e.to_string()))?;
        Ok(person(&href, card))
    }

    async fn update_contact(&self, resource: &str, fields_in: &ContactFields) -> Result<Person, BackendError> {
        let current = self.api.get(resource).await.map_err(|e| self.err(e))?;
        let body = vcard::patch_card(&current.body, &fields(fields_in)).map_err(|e| BackendError::Refused(e.to_string()))?;
        self.api.put(resource, &body, Kind::AddressBook, Precondition::Match(current.etag)).await.map_err(|e| self.err(e))?;
        let card = vcard::read_card(&body).map_err(|e| BackendError::Refused(e.to_string()))?;
        Ok(person(resource, card))
    }
}
