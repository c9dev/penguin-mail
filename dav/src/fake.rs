//! A CalDAV and CardDAV server in memory, behind the same [`DavApi`] the
//! real client serves: collections, resources with etags, and a change
//! log that answers `sync-collection` from any token it still keeps. A
//! test can forget every token, turn the report off, go offline or
//! refuse the login, and read how many PUTs and GETs arrived. Like the
//! real client, it names every resource by its canonical href, so a path
//! spelled `%40` and one spelled `@` are one resource.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use mailrs_domain::EpochMillis;

use crate::DavError;
use crate::client::{
    Collection, CollectionState, DavApi, Fetched, Homes, Kind, Member, Precondition, Synced,
};
use crate::ids::canonical_href;

#[derive(Default)]
pub struct FakeState {
    pub collections: Vec<Collection>,
    /// href to (etag, body).
    pub resources: BTreeMap<String, (String, String)>,
    /// (seq, href, removed), one entry per change.
    pub log: Vec<(u64, String, bool)>,
    pub seq: u64,
    /// Tokens at or below this are refused.
    pub forgotten_below: u64,
    pub no_sync: bool,
    pub auto_schedule: bool,
    /// The server says that it schedules on its calendars and not at its
    /// own address, as Nextcloud does.
    pub quiet_at_its_address: bool,
    /// The server answers 401 when asked whether it schedules, though it
    /// takes the login for everything else.
    pub refuse_options: bool,
    pub offline: bool,
    pub refuse_login: bool,
    pub puts: usize,
    pub gets: usize,
    /// Every `sync-collection` answer says more follows, as a server stuck
    /// answering 507 does.
    pub always_more: bool,
    /// How many `sync-collection` reports were asked.
    pub syncs: usize,
    /// The addresses the principal goes by.
    pub addresses: Vec<String>,
    /// How many times the homes were asked.
    pub homes: usize,
    /// PUTs to these hrefs answer 412, as when another device saved first.
    pub changed_on_put: Vec<String>,
    /// Behave as Google's CardDAV was seen to (2026-11-27, the test
    /// account): a create is stored under a name of the server's own, which
    /// `Location` gives, with the UID rewritten to it; `If-Match` is
    /// ignored; the ETag is a timestamp; FN is rebuilt from N; a birthday
    /// without a year, and CATEGORIES, are dropped, but year 1604 is kept as
    /// Apple's mark for none; a URL gets a group and
    /// a label.
    pub google_quirks: bool,
}

/// What Google's CardDAV makes of a card it was sent.
fn google_card(body: &str, uid: &str) -> String {
    let unfolded = body.replace("\r\n ", "");
    let mut name = None;
    let lines: Vec<&str> = unfolded.lines().collect();
    for line in &lines {
        if let Some(n) = line.strip_prefix("N:") {
            let mut parts = n.split(';');
            let family = parts.next().unwrap_or_default();
            let given = parts.next().unwrap_or_default();
            name = Some(format!("{given} {family}").trim().to_string());
        }
    }
    let mut out = Vec::new();
    for line in lines {
        let upper = line.to_ascii_uppercase();
        if upper.starts_with("UID:") {
            out.push(format!("UID:{uid}"));
        } else if upper.starts_with("FN:") {
            out.push(format!(
                "FN:{}",
                name.clone().unwrap_or_else(|| line[3..].to_string())
            ));
        } else if upper.starts_with("CATEGORIES") {
            // dropped
        } else if upper.starts_with("BDAY") {
            let value = line.rsplit(':').next().unwrap_or_default();
            if value.starts_with("1604") {
                out.push(format!("BDAY;VALUE=DATE,X-APPLE-OMIT-YEAR=1604:{value}"));
            } else if !value.starts_with("--") {
                out.push(format!("BDAY;VALUE=DATE:{value}"));
            }
        } else if upper.starts_with("URL") {
            let url = line
                .split_once(':')
                .map_or("", |(_, v)| v)
                .replace(':', "\\:");
            out.push(format!("item1.URL;TYPE=PREF:{url}"));
            out.push("item1.X-ABLabel:Other".into());
        } else {
            out.push(line.to_string());
        }
    }
    let mut text = out.join("\r\n");
    text.push_str("\r\n");
    text
}

#[derive(Default)]
pub struct FakeDav {
    state: Mutex<FakeState>,
}

fn collection_of(href: &str) -> String {
    match href.trim_end_matches('/').rsplit_once('/') {
        Some((parent, _)) => format!("{parent}/"),
        None => "/".into(),
    }
}

/// Whether the first DTSTART in `body` falls in `[from, to)`. A body
/// with no readable start stays in, as a server that cannot tell would
/// keep it.
fn starts_within(body: &str, from: EpochMillis, to: EpochMillis) -> bool {
    let Some(line) = body.lines().find(|l| l.starts_with("DTSTART")) else {
        return true;
    };
    let value = line.rsplit(':').next().unwrap_or_default().trim();
    let at = chrono::NaiveDateTime::parse_from_str(value.trim_end_matches('Z'), "%Y%m%dT%H%M%S")
        .map(|t| t.and_utc().timestamp_millis())
        .or_else(|_| {
            chrono::NaiveDate::parse_from_str(value, "%Y%m%d").map(|d| {
                d.and_hms_opt(0, 0, 0)
                    .unwrap_or_default()
                    .and_utc()
                    .timestamp_millis()
            })
        });
    at.map_or(true, |at| at >= from && at < to)
}

impl FakeDav {
    pub fn new() -> FakeDav {
        FakeDav::default()
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut FakeState) -> R) -> R {
        f(&mut self.state.lock().unwrap_or_else(PoisonError::into_inner))
    }

    fn lock(&self) -> MutexGuard<'_, FakeState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn add_collection(&self, href: &str, kind: Kind, name: &str, color: Option<&str>) {
        self.with(|s| {
            s.collections.push(Collection {
                href: canonical_href(href),
                kind,
                name: name.into(),
                color: color.map(str::to_string),
                components: match kind {
                    Kind::Calendar => vec!["VEVENT".into()],
                    Kind::AddressBook => Vec::new(),
                },
                can_write: true,
                timezone: None,
                sync: true,
            })
        });
    }

    /// Puts `body` at `href` as another client would, and answers its etag.
    pub fn put_resource(&self, href: &str, body: &str) -> String {
        let href = &canonical_href(href);
        self.with(|s| {
            s.seq += 1;
            let etag = format!("\"{}\"", s.seq);
            s.resources.insert(href.into(), (etag.clone(), body.into()));
            let seq = s.seq;
            s.log.push((seq, href.into(), false));
            etag
        })
    }

    pub fn remove_resource(&self, href: &str) {
        let href = &canonical_href(href);
        self.with(|s| {
            s.seq += 1;
            s.resources.remove(href);
            let seq = s.seq;
            s.log.push((seq, href.into(), true));
        });
    }

    pub fn body(&self, href: &str) -> Option<String> {
        let href = &canonical_href(href);
        self.with(|s| s.resources.get(href).map(|(_, b)| b.clone()))
    }

    pub fn etag(&self, href: &str) -> Option<String> {
        let href = &canonical_href(href);
        self.with(|s| s.resources.get(href).map(|(e, _)| e.clone()))
    }

    pub fn forget_tokens(&self) {
        self.with(|s| {
            // A bump, so tokens handed out before any change are refused too.
            s.seq += 1;
            s.forgotten_below = s.seq;
        });
    }

    pub fn set_sync(&self, supported: bool) {
        self.with(|s| s.no_sync = !supported);
    }

    /// Whether the server schedules by itself (`calendar-auto-schedule`).
    pub fn set_auto_schedule(&self, on: bool) {
        self.with(|s| s.auto_schedule = on);
    }

    pub fn set_offline(&self, offline: bool) {
        self.with(|s| s.offline = offline);
    }

    pub fn refuse_login(&self, refuse: bool) {
        self.with(|s| s.refuse_login = refuse);
    }

    pub fn puts(&self) -> usize {
        self.with(|s| s.puts)
    }

    pub fn gets(&self) -> usize {
        self.with(|s| s.gets)
    }

    fn open(&self) -> Result<MutexGuard<'_, FakeState>, DavError> {
        let state = self.lock();
        if state.offline {
            return Err(DavError::Network("the fake server is offline".into()));
        }
        if state.refuse_login {
            return Err(DavError::Unauthorized);
        }
        Ok(state)
    }
}

impl DavApi for FakeDav {
    async fn homes(&self) -> Result<Homes, DavError> {
        let mut state = self.open()?;
        state.homes += 1;
        let addresses = state.addresses.clone();
        Ok(Homes {
            principal: "/principal/".into(),
            calendar_homes: vec!["/cal/".into()],
            addressbook: Some("/card/".into()),
            addresses,
        })
    }

    async fn collections(&self, home: &str, kind: Kind) -> Result<Vec<Collection>, DavError> {
        let s = self.open()?;
        Ok(s.collections
            .iter()
            .filter(|c| c.kind == kind && c.href.starts_with(home))
            .cloned()
            .collect())
    }

    async fn state(&self, collection: &str) -> Result<CollectionState, DavError> {
        let s = self.open()?;
        let last = s
            .log
            .iter()
            .filter(|(_, h, _)| collection_of(h) == collection)
            .map(|(seq, _, _)| *seq)
            .max()
            .unwrap_or(0);
        Ok(CollectionState {
            sync_token: (!s.no_sync).then(|| format!("t{}", s.seq)),
            ctag: Some(format!("c{last}")),
        })
    }

    async fn sync(&self, collection: &str, token: &str) -> Result<Synced, DavError> {
        let collection = canonical_href(collection);
        let mut s = self.open()?;
        if s.no_sync {
            return Err(DavError::NoSyncCollection);
        }
        s.syncs += 1;
        let since = match token {
            "" => None,
            t => Some(
                t.strip_prefix('t')
                    .and_then(|n| n.parse::<u64>().ok())
                    .ok_or(DavError::InvalidSyncToken)?,
            ),
        };
        if since.is_some_and(|n| n < s.forgotten_below) {
            return Err(DavError::InvalidSyncToken);
        }
        let mut synced = Synced {
            token: format!("t{}", s.seq),
            more: s.always_more,
            ..Synced::default()
        };
        match since {
            None => {
                for (href, (etag, _)) in s
                    .resources
                    .iter()
                    .filter(|(h, _)| collection_of(h) == collection)
                {
                    synced.changed.push(Member {
                        href: href.clone(),
                        etag: etag.clone(),
                    });
                }
            }
            Some(n) => {
                let mut last: BTreeMap<&str, bool> = BTreeMap::new();
                for (_, href, removed) in s
                    .log
                    .iter()
                    .filter(|(seq, h, _)| *seq > n && collection_of(h) == collection)
                {
                    last.insert(href, *removed);
                }
                for (href, removed) in last {
                    match (removed, s.resources.get(href)) {
                        (false, Some((etag, _))) => synced.changed.push(Member {
                            href: href.into(),
                            etag: etag.clone(),
                        }),
                        _ => synced.removed.push(href.into()),
                    }
                }
            }
        }
        Ok(synced)
    }

    async fn members(
        &self,
        collection: &str,
        kind: Kind,
        range: Option<(EpochMillis, EpochMillis)>,
    ) -> Result<Vec<Member>, DavError> {
        let collection = canonical_href(collection);
        let s = self.open()?;
        Ok(s.resources
            .iter()
            .filter(|(h, _)| collection_of(h) == collection)
            .filter(|(_, (_, body))| match (kind, range) {
                (Kind::Calendar, Some((from, to))) => starts_within(body, from, to),
                _ => true,
            })
            .map(|(href, (etag, _))| Member {
                href: href.clone(),
                etag: etag.clone(),
            })
            .collect())
    }

    async fn fetch(
        &self,
        _collection: &str,
        _kind: Kind,
        hrefs: &[String],
    ) -> Result<Vec<Fetched>, DavError> {
        let mut s = self.open()?;
        s.gets += hrefs.len();
        Ok(hrefs
            .iter()
            .map(|h| canonical_href(h))
            .filter_map(|h| {
                s.resources.get(&h).map(|(etag, body)| Fetched {
                    href: h,
                    etag: etag.clone(),
                    body: body.clone(),
                })
            })
            .collect())
    }

    async fn get(&self, href: &str) -> Result<Fetched, DavError> {
        let href = &canonical_href(href);
        let mut s = self.open()?;
        s.gets += 1;
        let (etag, body) = s.resources.get(href).cloned().ok_or(DavError::NotFound)?;
        Ok(Fetched {
            href: href.into(),
            etag,
            body,
        })
    }

    async fn put(
        &self,
        href: &str,
        body: &str,
        kind: Kind,
        when: Precondition,
    ) -> Result<Option<String>, DavError> {
        self.put_at(href, body, kind, when)
            .await
            .map(|(etag, _)| etag)
    }

    async fn put_at(
        &self,
        href: &str,
        body: &str,
        _kind: Kind,
        when: Precondition,
    ) -> Result<(Option<String>, Option<String>), DavError> {
        let href = &canonical_href(href);
        let mut s = self.open()?;
        if s.google_quirks {
            s.puts += 1;
            s.seq += 1;
            let etag = format!("\"2026-11-27T16:58:25.{:03}-08:00\"", s.seq);
            let (target, location) = match when {
                Precondition::NoneMatch => {
                    let id = format!(
                        "{:016x}",
                        0x3f93_6966_140b_99d6_u64 + u64::from(s.seq as u32)
                    );
                    let target = format!("{}{id}", collection_of(href));
                    (target.clone(), Some(target))
                }
                // If-Match is ignored: a stale one overwrites, with a 204.
                Precondition::Match(_) => {
                    if !s.resources.contains_key(href) {
                        return Err(DavError::NotFound);
                    }
                    (href.clone(), Some(href.clone()))
                }
            };
            let uid = target.rsplit('/').next().unwrap_or_default().to_string();
            s.resources
                .insert(target.clone(), (etag.clone(), google_card(body, &uid)));
            let seq = s.seq;
            s.log.push((seq, target, false));
            return Ok((Some(etag), location));
        }
        if s.changed_on_put.contains(href) {
            return Err(DavError::Changed);
        }
        let held = s.resources.get(href).map(|(e, _)| e.clone());
        match (&when, &held) {
            (Precondition::NoneMatch, Some(_)) => return Err(DavError::Changed),
            (Precondition::Match(want), Some(have)) if want != have => {
                return Err(DavError::Changed);
            }
            (Precondition::Match(_), None) => return Err(DavError::NotFound),
            _ => {}
        }
        s.puts += 1;
        s.seq += 1;
        let etag = format!("\"{}\"", s.seq);
        s.resources.insert(href.into(), (etag.clone(), body.into()));
        let seq = s.seq;
        s.log.push((seq, href.into(), false));
        Ok((Some(etag), None))
    }

    async fn delete(&self, href: &str, etag: Option<&str>) -> Result<(), DavError> {
        let href = &canonical_href(href);
        let mut s = self.open()?;
        match (s.resources.get(href), etag) {
            (None, _) => return Err(DavError::NotFound),
            (Some((have, _)), Some(want)) if have != want => return Err(DavError::Changed),
            _ => {}
        }
        s.seq += 1;
        s.resources.remove(href);
        let seq = s.seq;
        s.log.push((seq, href.into(), true));
        Ok(())
    }

    async fn find_uid(&self, collection: &str, uid: &str) -> Result<Option<Fetched>, DavError> {
        let collection = canonical_href(collection);
        let s = self.open()?;
        let wanted = format!("UID:{uid}");
        Ok(s.resources
            .iter()
            .filter(|(h, _)| collection_of(h) == collection)
            .find(|(_, (_, body))| body.lines().any(|l| l.trim() == wanted))
            .map(|(href, (etag, body))| Fetched {
                href: href.clone(),
                etag: etag.clone(),
                body: body.clone(),
            }))
    }

    async fn auto_schedule(&self, at: Option<&str>) -> Result<bool, DavError> {
        let state = self.open()?;
        if state.refuse_options {
            return Err(DavError::Unauthorized);
        }
        Ok(state.auto_schedule && (at.is_some() || !state.quiet_at_its_address))
    }

    fn host(&self) -> &str {
        "dav.fake.example"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVENT: &str = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:u\r\nDTSTART:20261101T090000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    #[tokio::test]
    async fn the_homes_name_the_addresses_a_test_gave() {
        let dav = FakeDav::new();
        assert!(dav.homes().await.unwrap().addresses.is_empty());
        dav.with(|s| s.addresses = vec!["me@example.org".into()]);
        assert_eq!(dav.homes().await.unwrap().addresses, ["me@example.org"]);
    }

    #[tokio::test]
    async fn a_sync_after_a_change_elsewhere_names_that_change_alone() {
        let dav = FakeDav::new();
        dav.add_collection("/cal/work/", Kind::Calendar, "Work", Some("#3a87ad"));
        dav.put_resource("/cal/work/a.ics", "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n");
        let first = dav.sync("/cal/work/", "").await.unwrap();
        dav.put_resource("/cal/work/b.ics", "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n");
        dav.remove_resource("/cal/work/a.ics");
        let next = dav.sync("/cal/work/", &first.token).await.unwrap();
        assert_eq!(
            next.changed
                .iter()
                .map(|m| m.href.as_str())
                .collect::<Vec<_>>(),
            ["/cal/work/b.ics"]
        );
        assert_eq!(next.removed, ["/cal/work/a.ics"]);
    }

    #[tokio::test]
    async fn a_forgotten_token_is_refused() {
        let dav = FakeDav::new();
        dav.add_collection("/cal/work/", Kind::Calendar, "Work", None);
        let first = dav.sync("/cal/work/", "").await.unwrap();
        dav.forget_tokens();
        assert!(matches!(
            dav.sync("/cal/work/", &first.token).await,
            Err(DavError::InvalidSyncToken)
        ));
    }

    #[tokio::test]
    async fn a_put_against_an_old_etag_is_refused() {
        let dav = FakeDav::new();
        dav.add_collection("/cal/work/", Kind::Calendar, "Work", None);
        let etag = dav.put_resource("/cal/work/a.ics", "one");
        dav.put_resource("/cal/work/a.ics", "two");
        let refused = dav
            .put(
                "/cal/work/a.ics",
                "three",
                Kind::Calendar,
                Precondition::Match(etag),
            )
            .await;
        assert!(matches!(refused, Err(DavError::Changed)));
    }

    #[tokio::test]
    async fn a_range_keeps_the_events_that_start_inside_it() {
        let dav = FakeDav::new();
        dav.add_collection("/cal/work/", Kind::Calendar, "Work", None);
        dav.put_resource("/cal/work/a.ics", EVENT);
        let day = 86_400_000;
        let nov_1 = 1_793_491_200_000;
        let inside = dav
            .members("/cal/work/", Kind::Calendar, Some((nov_1, nov_1 + day)))
            .await
            .unwrap();
        let after = dav
            .members("/cal/work/", Kind::Calendar, Some((nov_1 + day, i64::MAX)))
            .await
            .unwrap();
        assert_eq!((inside.len(), after.len()), (1, 0));
    }

    #[tokio::test]
    async fn the_server_schedules_only_when_told_to() {
        let dav = FakeDav::new();
        assert!(!dav.auto_schedule(None).await.unwrap());
        dav.set_auto_schedule(true);
        assert!(dav.auto_schedule(None).await.unwrap());
        dav.with(|s| s.quiet_at_its_address = true);
        assert!(!dav.auto_schedule(None).await.unwrap());
        assert!(dav.auto_schedule(Some("/cal/work/")).await.unwrap());
    }
}
