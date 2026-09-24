//! Readers for the untagged answers a command brings back. Each takes the
//! parsed responses one at a time, the way they arrive, so a test feeds
//! it lines written by hand and no server is needed.

use async_imap::imap_proto::{
    AttributeValue, MailboxDatum, NameAttribute, Response, ResponseCode, Status, UidSetMember,
};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::ops::RangeInclusive;

use chrono::DateTime;
use mailrs_domain::EpochMillis;

use crate::guard::{BODY_BYTES, COMMAND_BYTES, SEARCH_BYTES, SELECT_BYTES};
use crate::structure::BodyStructure;
use crate::{
    AppendUid, CopyUid, Fetched, FlagsOf, ImapError, Listed, Selected, SpecialUse, UidSet,
};

/// The most untagged answers one command may bring. A server sends one
/// per message or mailbox, so a command that asks about more than this
/// has to ask in windows.
pub(crate) const MAX_ANSWERS: usize = 100_000;

/// Answers a command about `uids` may bring: three for each UID, for a
/// server that reports a flag change or an expunge beside each answer,
/// and a thousand for news about other messages, up to [`MAX_ANSWERS`].
pub(crate) fn answers_for(uids: &UidSet) -> usize {
    let most = uids.len().saturating_mul(3).saturating_add(1_000);
    usize::try_from(most)
        .unwrap_or(MAX_ANSWERS)
        .min(MAX_ANSWERS)
}

/// The most flags one message may carry in an answer. A message rarely
/// carries more than a dozen; every flag kept costs about 60 bytes, so a
/// window of 1,000 messages holds at most about 4 MB of them.
pub(crate) const MAX_FLAGS: usize = 64;

/// The most flags one command may keep across all its messages: about
/// 65 MB at worst. [`MAX_FLAGS`] alone lets an open-ended fetch keep
/// 100,000 messages of 64 flags each.
pub(crate) const MAX_FLAGS_KEPT: usize = 1_000_000;

/// The flags a command may still keep.
#[derive(Debug)]
pub(crate) struct FlagBudget {
    left: usize,
}

impl Default for FlagBudget {
    fn default() -> Self {
        FlagBudget::new(MAX_FLAGS_KEPT)
    }
}

impl FlagBudget {
    pub(crate) fn new(left: usize) -> Self {
        FlagBudget { left }
    }

    /// Gives back `before` flags a message kept and takes `after` for its
    /// new answer, or refuses when that runs past the total.
    fn swap(&mut self, before: usize, after: usize) -> Result<(), ImapError> {
        match (self.left + before).checked_sub(after) {
            Some(left) => {
                self.left = left;
                Ok(())
            }
            None => Err(ImapError::Protocol(format!(
                "the server sent more than {MAX_FLAGS_KEPT} flags to one command"
            ))),
        }
    }
}

/// Keeps `flags` as the answer for its UID, replacing an earlier one,
/// within `budget`.
fn keep_flags(
    kept: &mut BTreeMap<u32, FlagsOf>,
    flags: FlagsOf,
    budget: &mut FlagBudget,
) -> Result<(), ImapError> {
    let before = kept.get(&flags.uid).map_or(0, |f| f.flags.len());
    budget.swap(before, flags.flags.len())?;
    kept.insert(flags.uid, flags);
    Ok(())
}

/// Something that reads a command's responses, the tagged one included.
pub(crate) trait Reads {
    fn read(&mut self, response: &Response<'_>);

    /// Whether what the reader has read so far is fit to keep; the
    /// connection asks after each response and gives up on the command
    /// at the first error.
    fn check(&self) -> Result<(), ImapError> {
        Ok(())
    }

    /// How many untagged answers the command may bring before the
    /// connection gives up on it.
    fn most(&self) -> usize {
        MAX_ANSWERS
    }

    /// How many bytes the command may bring back in all.
    fn bytes(&self) -> u64 {
        COMMAND_BYTES
    }

    /// A literal in `response` this reader keeps whole. The connection
    /// then hands it over through [`Reads::keep`] in the buffer it arrived
    /// in, instead of `read` copying it.
    fn wants<'r>(&self, _response: &'r Response<'_>) -> Option<&'r [u8]> {
        None
    }

    fn keep(&mut self, _bytes: Vec<u8>) {}
}

/// Reads nothing, for commands whose only answer is OK.
impl Reads for () {
    fn read(&mut self, _response: &Response<'_>) {}
}

/// The atoms of a CAPABILITY answer, from the untagged response or from
/// the `[CAPABILITY ...]` code a greeting or a login OK carries.
#[derive(Default)]
pub(crate) struct CapabilityReader {
    pub(crate) atoms: Vec<String>,
}

impl Reads for CapabilityReader {
    fn read(&mut self, response: &Response<'_>) {
        let list = match response {
            Response::Capabilities(list) => list,
            Response::Data {
                code: Some(ResponseCode::Capabilities(list)),
                ..
            }
            | Response::Done {
                code: Some(ResponseCode::Capabilities(list)),
                ..
            } => list,
            _ => return,
        };
        self.atoms = list
            .iter()
            .map(|cap| match cap {
                async_imap::imap_proto::Capability::Imap4rev1 => "IMAP4REV1".to_string(),
                async_imap::imap_proto::Capability::Auth(mechanism) => {
                    format!("AUTH={}", mechanism.to_ascii_uppercase())
                }
                async_imap::imap_proto::Capability::Atom(atom) => atom.to_ascii_uppercase(),
            })
            .collect();
    }
}

/// SELECT's answer.
#[derive(Default)]
pub(crate) struct SelectReader {
    selected: Selected,
    uidvalidity: Option<u32>,
    /// The UIDs the store holds, whose changes are all the caller reads;
    /// it fetches new mail by UIDNEXT. `None` keeps every change.
    known: Option<UidSet>,
    /// The last change reported for each UID.
    changed: BTreeMap<u32, FlagsOf>,
    flag_budget: FlagBudget,
    /// What VANISHED named, merged: a server may name 100,000 ranges.
    vanished: UidSet,
    /// VANISHED ranges not yet merged into `vanished`. They fold into it
    /// once there are more than [`FOLD_AFTER`] plus twice as many as the
    /// set holds, so this list holds at most that many plus one answer's
    /// ranges, and a range the server repeats is kept once after each
    /// fold.
    pending: Vec<RangeInclusive<u32>>,
    error: Option<ImapError>,
}

/// VANISHED ranges a SELECT holds unmerged, beyond twice the merged set.
/// A fold sorts the pending list with the set's ranges, n log n in their
/// count, and the margin keeps a fold from running after every small
/// answer.
const FOLD_AFTER: usize = 65_536;

impl Reads for SelectReader {
    fn read(&mut self, response: &Response<'_>) {
        match response {
            Response::Data {
                status: Status::Ok,
                code: Some(code),
                ..
            } => match code {
                ResponseCode::UidValidity(v) => self.uidvalidity = Some(*v),
                ResponseCode::UidNext(n) => self.selected.uidnext = Some(*n),
                ResponseCode::HighestModSeq(m) => self.selected.highestmodseq = Some(*m),
                ResponseCode::PermanentFlags(flags) => {
                    self.selected.permanent_flags = flags.iter().map(|f| f.to_string()).collect();
                }
                _ => {}
            },
            Response::MailboxData(MailboxDatum::Exists(n)) => self.selected.exists = *n,
            Response::Vanished { uids, .. } => {
                self.pending.extend(uids.iter().cloned());
                if self.pending.len() > FOLD_AFTER + 2 * self.vanished.ranges().len() {
                    self.fold();
                }
            }
            Response::Fetch(_, attributes) => match flags_of(attributes) {
                Some(Ok(flags)) if self.known.as_ref().is_none_or(|k| k.contains(flags.uid)) => {
                    if let Err(err) = keep_flags(&mut self.changed, flags, &mut self.flag_budget) {
                        self.error = Some(err);
                    }
                }
                Some(Err(err)) => self.error = Some(err),
                _ => {}
            },
            _ => {}
        }
    }

    fn check(&self) -> Result<(), ImapError> {
        self.error.clone().map_or(Ok(()), Err)
    }

    fn bytes(&self) -> u64 {
        SELECT_BYTES
    }
}

impl SelectReader {
    pub(crate) fn new(known: Option<&UidSet>) -> Self {
        SelectReader {
            known: known.cloned(),
            ..SelectReader::default()
        }
    }

    /// Merges the pending VANISHED ranges into the set. The set's ranges
    /// join the pending list, which is sorted and merged in place, so the
    /// most held at once is that list beside the old set, and the result
    /// has no spare capacity.
    fn fold(&mut self) {
        let mut pending = std::mem::take(&mut self.pending);
        let old = std::mem::take(&mut self.vanished);
        pending.reserve_exact(old.ranges().len());
        pending.extend_from_slice(old.ranges());
        drop(old);
        self.vanished = UidSet::from(pending);
    }

    pub(crate) fn finish(mut self) -> Result<Selected, ImapError> {
        let uidvalidity = self
            .uidvalidity
            .ok_or_else(|| ImapError::Protocol("SELECT gave no UIDVALIDITY".into()))?;
        self.fold();
        Ok(Selected {
            uidvalidity,
            changed: self.changed.into_values().collect(),
            vanished: self.vanished,
            ..self.selected
        })
    }
}

/// The flags of each message asked for, lowest UID first, the last
/// answer for a UID replacing any before it. `n:*` names the last message
/// even when its UID is below `n`, and a server reports changes to other
/// messages unasked, so the rest go.
pub(crate) struct FlagsReader {
    wanted: UidSet,
    flags: BTreeMap<u32, FlagsOf>,
    flag_budget: FlagBudget,
    error: Option<ImapError>,
}

impl FlagsReader {
    pub(crate) fn new(uids: &UidSet) -> Self {
        FlagsReader {
            wanted: uids.clone(),
            flags: BTreeMap::new(),
            flag_budget: FlagBudget::default(),
            error: None,
        }
    }

    pub(crate) fn finish(self) -> Vec<FlagsOf> {
        self.flags.into_values().collect()
    }
}

impl Reads for FlagsReader {
    fn read(&mut self, response: &Response<'_>) {
        let Response::Fetch(_, attributes) = response else {
            return;
        };
        match flags_of(attributes) {
            Some(Ok(flags)) if self.wanted.contains(flags.uid) => {
                if let Err(err) = keep_flags(&mut self.flags, flags, &mut self.flag_budget) {
                    self.error = Some(err);
                }
            }
            Some(Err(err)) => self.error = Some(err),
            _ => {}
        }
    }

    fn check(&self) -> Result<(), ImapError> {
        self.error.clone().map_or(Ok(()), Err)
    }

    fn most(&self) -> usize {
        answers_for(&self.wanted)
    }
}

/// A FETCH answer's UID, flags and MODSEQ, `None` for a FETCH without
/// both a UID and flags, such as a server's unasked report about a
/// message another client changed, or an error past [`MAX_FLAGS`].
fn flags_of(attributes: &[AttributeValue<'_>]) -> Option<Result<FlagsOf, ImapError>> {
    let mut uid = None;
    let mut flags = None;
    let mut modseq = None;
    for attribute in attributes {
        match attribute {
            AttributeValue::Uid(u) => uid = Some(*u),
            AttributeValue::Flags(list) => flags = Some(list),
            AttributeValue::ModSeq(m) => modseq = Some(*m),
            _ => {}
        }
    }
    let (uid, flags) = (uid?, flags?);
    Some(flag_strings(flags).map(|flags| FlagsOf { uid, flags, modseq }))
}

/// A message's flags as strings, refused past [`MAX_FLAGS`].
fn flag_strings(list: &[Cow<'_, str>]) -> Result<Vec<String>, ImapError> {
    match list.len() > MAX_FLAGS {
        true => Err(ImapError::Protocol(format!(
            "the server gave a message {} flags, more than {MAX_FLAGS}",
            list.len()
        ))),
        false => Ok(list.iter().map(|f| f.to_string()).collect()),
    }
}

/// The header fetch that lists messages: one [`Fetched`] per UID asked
/// for, lowest first, the last answer for a UID replacing any before it,
/// the others dropped as [`FlagsReader`] drops them.
pub(crate) struct HeadersReader {
    wanted: UidSet,
    fetched: BTreeMap<u32, Fetched>,
    flag_budget: FlagBudget,
    error: Option<ImapError>,
}

impl HeadersReader {
    pub(crate) fn new(uids: &UidSet) -> Self {
        HeadersReader {
            wanted: uids.clone(),
            fetched: BTreeMap::new(),
            flag_budget: FlagBudget::default(),
            error: None,
        }
    }

    pub(crate) fn finish(self) -> Vec<Fetched> {
        self.fetched.into_values().collect()
    }
}

impl Reads for HeadersReader {
    fn check(&self) -> Result<(), ImapError> {
        self.error.clone().map_or(Ok(()), Err)
    }

    fn most(&self) -> usize {
        answers_for(&self.wanted)
    }

    fn read(&mut self, response: &Response<'_>) {
        let Response::Fetch(_, attributes) = response else {
            return;
        };
        let Some(uid) = attributes.iter().find_map(|a| match a {
            AttributeValue::Uid(u) => Some(*u),
            _ => None,
        }) else {
            return;
        };
        if !self.wanted.contains(uid) {
            return;
        }
        let header = attributes.iter().find_map(|a| match a {
            AttributeValue::BodySection {
                data: Some(data), ..
            } => Some(data.as_ref()),
            _ => None,
        });
        let Some(header) = header else {
            // A FETCH with a UID and no header is a flag report, not an
            // answer to this command.
            return;
        };
        let mut fetched = Fetched::from_header(uid, header);
        for attribute in attributes {
            match attribute {
                AttributeValue::Flags(list) => match flag_strings(list) {
                    Ok(flags) => fetched.flags = flags,
                    Err(err) => {
                        self.error = Some(err);
                        return;
                    }
                },
                AttributeValue::InternalDate(date) => fetched.internal_date = internal_date(date),
                AttributeValue::Rfc822Size(size) => fetched.size = Some(u64::from(*size)),
                AttributeValue::ModSeq(m) => fetched.modseq = Some(*m),
                _ => {}
            }
        }
        let before = self.fetched.get(&uid).map_or(0, |f| f.flags.len());
        if let Err(err) = self.flag_budget.swap(before, fetched.flags.len()) {
            self.error = Some(err);
            return;
        }
        self.fetched.insert(uid, fetched);
    }
}

/// INTERNALDATE, `17-Jul-1996 02:44:25 -0700`, in milliseconds. A day
/// below ten comes with a leading space.
pub(crate) fn internal_date(text: &str) -> Option<EpochMillis> {
    DateTime::parse_from_str(text.trim(), "%d-%b-%Y %H:%M:%S %z")
        .ok()
        .map(|date| date.timestamp_millis())
}

/// One section of one message: the bytes of `BODY[<section>]` for `uid`.
pub(crate) struct SectionReader {
    uid: u32,
    pub(crate) bytes: Option<Vec<u8>>,
}

impl SectionReader {
    pub(crate) fn new(uid: u32) -> Self {
        SectionReader { uid, bytes: None }
    }
}

impl Reads for SectionReader {
    fn read(&mut self, response: &Response<'_>) {
        let Some(attributes) = fetched_for(response, self.uid) else {
            return;
        };
        for attribute in attributes {
            if let AttributeValue::BodySection { data, .. } = attribute {
                self.bytes = Some(data.as_deref().map(<[u8]>::to_vec).unwrap_or_default());
            }
        }
    }

    fn most(&self) -> usize {
        answers_for(&UidSet::from_uids([self.uid]))
    }

    fn bytes(&self) -> u64 {
        BODY_BYTES
    }

    fn wants<'r>(&self, response: &'r Response<'_>) -> Option<&'r [u8]> {
        fetched_for(response, self.uid)?
            .iter()
            .find_map(|attribute| match attribute {
                AttributeValue::BodySection {
                    data: Some(data), ..
                } => Some(data.as_ref()),
                _ => None,
            })
    }

    fn keep(&mut self, bytes: Vec<u8>) {
        self.bytes = Some(bytes);
    }
}

/// The attributes of a FETCH answer about `uid`.
fn fetched_for<'r, 'a>(response: &'r Response<'a>, uid: u32) -> Option<&'r [AttributeValue<'a>]> {
    match response {
        Response::Fetch(_, attributes)
            if attributes
                .iter()
                .any(|a| matches!(a, AttributeValue::Uid(u) if *u == uid)) =>
        {
            Some(attributes)
        }
        _ => None,
    }
}

/// One message's BODYSTRUCTURE.
pub(crate) struct StructureReader {
    uid: u32,
    pub(crate) structure: Option<BodyStructure>,
}

impl StructureReader {
    pub(crate) fn new(uid: u32) -> Self {
        StructureReader {
            uid,
            structure: None,
        }
    }
}

impl Reads for StructureReader {
    fn most(&self) -> usize {
        answers_for(&UidSet::from_uids([self.uid]))
    }

    fn read(&mut self, response: &Response<'_>) {
        let Some(attributes) = fetched_for(response, self.uid) else {
            return;
        };
        for attribute in attributes {
            if let AttributeValue::BodyStructure(wire) = attribute {
                self.structure = Some(BodyStructure::from_wire(wire));
            }
        }
    }
}

/// LIST's answer.
#[derive(Default)]
pub(crate) struct ListReader {
    pub(crate) listed: Vec<Listed>,
}

impl Reads for ListReader {
    fn read(&mut self, response: &Response<'_>) {
        let Response::MailboxData(MailboxDatum::List {
            name_attributes,
            delimiter,
            name,
        }) = response
        else {
            return;
        };
        let mut special_use = None;
        let mut no_select = false;
        for attribute in name_attributes {
            match attribute {
                NameAttribute::NoSelect => no_select = true,
                NameAttribute::All => special_use = Some(SpecialUse::All),
                NameAttribute::Archive => special_use = Some(SpecialUse::Archive),
                NameAttribute::Drafts => special_use = Some(SpecialUse::Drafts),
                NameAttribute::Flagged => special_use = Some(SpecialUse::Flagged),
                NameAttribute::Junk => special_use = Some(SpecialUse::Junk),
                NameAttribute::Sent => special_use = Some(SpecialUse::Sent),
                NameAttribute::Trash => special_use = Some(SpecialUse::Trash),
                // RFC 5258: a name listed only because a child exists.
                NameAttribute::Extension(other) if other.eq_ignore_ascii_case("\\NonExistent") => {
                    no_select = true;
                }
                _ => {}
            }
        }
        let delimiter = delimiter.as_deref().and_then(|d| d.chars().next());
        self.listed.push(Listed::new(
            name.to_string(),
            delimiter,
            special_use,
            no_select,
        ));
    }
}

/// The most UIDs one SEARCH may bring: as many as fit its budget at a
/// digit and a space each, about two million, 8 MB kept at most.
pub(crate) const MAX_SEARCH_UIDS: usize = (SEARCH_BYTES / 2) as usize;

/// SEARCH's answer, lowest UID first once finished. Answers are sorted
/// once at the end, since a server may split one into many.
pub(crate) struct SearchReader {
    uids: Vec<u32>,
    most_uids: usize,
    error: Option<ImapError>,
}

impl Default for SearchReader {
    fn default() -> Self {
        SearchReader {
            uids: Vec::new(),
            most_uids: MAX_SEARCH_UIDS,
            error: None,
        }
    }
}

impl SearchReader {
    pub(crate) fn finish(mut self) -> Vec<u32> {
        self.uids.sort_unstable();
        self.uids.dedup();
        self.uids.shrink_to_fit();
        self.uids
    }
}

impl Reads for SearchReader {
    fn read(&mut self, response: &Response<'_>) {
        let Response::MailboxData(MailboxDatum::Search(uids)) = response else {
            return;
        };
        if self.uids.len() + uids.len() > self.most_uids {
            self.error = Some(ImapError::Protocol(format!(
                "the server named more than {} UIDs in one SEARCH",
                self.most_uids
            )));
            return;
        }
        self.uids.extend(uids);
    }

    fn check(&self) -> Result<(), ImapError> {
        self.error.clone().map_or(Ok(()), Err)
    }

    fn bytes(&self) -> u64 {
        SEARCH_BYTES
    }
}

/// COPYUID, from the tagged OK of COPY or from the untagged OK that MOVE
/// sends before its expunges (RFC 6851 section 4.3).
#[derive(Default)]
pub(crate) struct CopyUidReader {
    pub(crate) copy_uid: Option<CopyUid>,
}

impl Reads for CopyUidReader {
    fn read(&mut self, response: &Response<'_>) {
        let code = match response {
            Response::Data {
                status: Status::Ok,
                code,
                ..
            } => code,
            Response::Done {
                status: Status::Ok,
                code,
                ..
            } => code,
            _ => return,
        };
        if let Some(ResponseCode::CopyUid(uidvalidity, from, to)) = code {
            self.copy_uid = Some(CopyUid {
                uidvalidity: *uidvalidity,
                pairs: uids(from).zip(uids(to)).collect(),
            });
        }
    }
}

/// APPENDUID, from APPEND's tagged OK.
#[derive(Default)]
pub(crate) struct AppendUidReader {
    pub(crate) append_uid: Option<AppendUid>,
}

impl Reads for AppendUidReader {
    fn read(&mut self, response: &Response<'_>) {
        if let Response::Done {
            status: Status::Ok,
            code: Some(ResponseCode::AppendUid(uidvalidity, set)),
            ..
        } = response
            && let Some(uid) = uids(set).next()
        {
            self.append_uid = Some(AppendUid {
                uidvalidity: *uidvalidity,
                uid,
            });
        }
    }
}

/// The UIDs a COPYUID or APPENDUID set names, in the order it names them.
fn uids(set: &[UidSetMember]) -> impl Iterator<Item = u32> + '_ {
    set.iter().flat_map(|member| match member {
        UidSetMember::Uid(uid) => *uid..=*uid,
        UidSetMember::UidRange(range) => range.clone(),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Parses each line as a server would send it and hands it to `reader`.
    pub(crate) fn feed(reader: &mut impl Reads, lines: &[&str]) {
        for line in lines {
            let (rest, response) =
                Response::from_bytes(line.as_bytes()).unwrap_or_else(|e| panic!("{line:?}: {e:?}"));
            assert!(rest.is_empty(), "{line:?} left {rest:?}");
            reader.read(&response);
        }
    }

    #[test]
    fn a_qresync_select_reports_state_vanished_uids_and_changed_flags() {
        let mut reader = SelectReader::default();
        feed(
            &mut reader,
            &[
                "* 172 EXISTS\r\n",
                "* OK [UIDVALIDITY 3857529045] UIDs valid\r\n",
                "* OK [UIDNEXT 4392] Predicted next UID\r\n",
                "* FLAGS (\\Answered \\Flagged \\Deleted \\Seen \\Draft)\r\n",
                "* OK [PERMANENTFLAGS (\\Deleted \\Seen \\*)] Limited\r\n",
                "* OK [HIGHESTMODSEQ 715194045007] Highest\r\n",
                "* VANISHED (EARLIER) 41,43:116,118,120:211,214:540\r\n",
                "* 49 FETCH (UID 117 FLAGS (\\Seen \\Answered) MODSEQ (90060115194045001))\r\n",
                "A1 OK [READ-WRITE] mailbox selected\r\n",
            ],
        );
        let selected = reader.finish().unwrap();
        assert_eq!(selected.uidvalidity, 3857529045);
        assert_eq!(selected.uidnext, Some(4392));
        assert_eq!(selected.highestmodseq, Some(715194045007));
        assert_eq!(selected.exists, 172);
        assert_eq!(selected.permanent_flags, ["\\Deleted", "\\Seen", "\\*"]);
        assert_eq!(
            selected.vanished.to_string(),
            "41,43:116,118,120:211,214:540"
        );
        assert_eq!(
            selected.changed,
            [FlagsOf {
                uid: 117,
                flags: vec!["\\Seen".into(), "\\Answered".into()],
                modseq: Some(90060115194045001)
            }]
        );
    }

    #[test]
    fn a_select_without_uidvalidity_is_a_protocol_error() {
        let mut reader = SelectReader::default();
        feed(&mut reader, &["* 3 EXISTS\r\n"]);
        assert!(matches!(reader.finish(), Err(ImapError::Protocol(_))));
    }

    #[test]
    fn a_plain_select_has_no_modseq() {
        let mut reader = SelectReader::default();
        feed(
            &mut reader,
            &[
                "* OK [UIDVALIDITY 7] UIDs valid\r\n",
                "* OK [NOMODSEQ] Sorry, this mailbox format doesn't support modsequences\r\n",
            ],
        );
        let selected = reader.finish().unwrap();
        assert_eq!(selected.highestmodseq, None);
        assert!(selected.vanished.is_empty());
    }

    #[test]
    fn the_header_fetch_fills_fetched() {
        let mut reader = HeadersReader::new(&UidSet::from_uids([12, 13]));
        feed(
            &mut reader,
            &[
                "* 1 FETCH (UID 12 FLAGS (\\Seen) INTERNALDATE \" 7-Feb-2026 10:00:00 +0100\" RFC822.SIZE 4201 MODSEQ (9) BODY[HEADER.FIELDS (FROM SUBJECT)] {32}\r\nFrom: a@b.pt\r\nSubject: hello\r\n\r\n)\r\n",
                "* 2 FETCH (UID 13 FLAGS (\\Flagged))\r\n",
            ],
        );
        let fetched = reader.finish();
        assert_eq!(fetched.len(), 1);
        let fetched = &fetched[0];
        assert_eq!(fetched.uid, 12);
        assert_eq!(fetched.flags, ["\\Seen"]);
        assert_eq!(fetched.internal_date, Some(1_770_454_800_000));
        assert_eq!(fetched.size, Some(4201));
        assert_eq!(fetched.modseq, Some(9));
        assert_eq!(fetched.subject, "hello");
        assert_eq!(
            fetched.from.as_ref().map(|a| a.email.as_str()),
            Some("a@b.pt")
        );
    }

    #[test]
    fn flags_come_with_their_modseq_and_skip_fetches_without_a_uid() {
        let mut reader = FlagsReader::new(&UidSet::from_uids([12]));
        feed(
            &mut reader,
            &[
                "* 3 FETCH (UID 12 FLAGS (\\Seen $Muted) MODSEQ (90))\r\n",
                "* 4 FETCH (FLAGS (\\Seen))\r\n",
            ],
        );
        assert_eq!(
            reader.finish(),
            [FlagsOf {
                uid: 12,
                flags: vec!["\\Seen".into(), "$Muted".into()],
                modseq: Some(90)
            }]
        );
    }

    #[test]
    fn a_section_reads_only_for_its_uid() {
        let mut reader = SectionReader::new(9);
        feed(
            &mut reader,
            &[
                "* 1 FETCH (UID 8 FLAGS (\\Seen))\r\n",
                "* 2 FETCH (UID 9 BODY[1.2] {5}\r\nhello)\r\n",
            ],
        );
        assert_eq!(reader.bytes.as_deref(), Some(&b"hello"[..]));
        let mut missing = SectionReader::new(10);
        feed(&mut missing, &["A1 OK done\r\n"]);
        assert_eq!(missing.bytes, None);
    }

    #[test]
    fn list_reads_special_use_and_parents() {
        let mut reader = ListReader::default();
        feed(
            &mut reader,
            &[
                "* LIST (\\HasNoChildren \\Sent) \"/\" \"Sent Items\"\r\n",
                "* LIST (\\NonExistent \\HasChildren) \"/\" \"&AMk-l&AOk-ments\"\r\n",
                "* LIST (\\Noselect) \".\" INBOX.Stuff\r\n",
                "* LIST () NIL Flat\r\n",
            ],
        );
        assert_eq!(
            reader.listed,
            [
                Listed::new("Sent Items", Some('/'), Some(SpecialUse::Sent), false),
                Listed::new("&AMk-l&AOk-ments", Some('/'), None, true),
                Listed::new("INBOX.Stuff", Some('.'), None, true),
                Listed::new("Flat", None, None, false),
            ]
        );
        assert_eq!(reader.listed[1].display, "Éléments");
    }

    #[test]
    fn search_gives_sorted_uids_and_none_for_an_empty_answer() {
        let mut reader = SearchReader::default();
        feed(&mut reader, &["* SEARCH 882 2 84\r\n"]);
        assert_eq!(reader.finish(), [2, 84, 882]);
        let mut empty = SearchReader::default();
        feed(&mut empty, &["* SEARCH\r\n"]);
        assert!(empty.finish().is_empty());
    }

    #[test]
    fn copyuid_pairs_each_source_with_its_copy() {
        let mut from_move = CopyUidReader::default();
        feed(
            &mut from_move,
            &["* OK [COPYUID 38505 304,319:320 3956:3958] Done\r\n"],
        );
        assert_eq!(
            from_move.copy_uid,
            Some(CopyUid {
                uidvalidity: 38505,
                pairs: vec![(304, 3956), (319, 3957), (320, 3958)]
            })
        );
        let mut from_copy = CopyUidReader::default();
        feed(&mut from_copy, &["A4 OK [COPYUID 7 5 9] Copied\r\n"]);
        assert_eq!(
            from_copy.copy_uid,
            Some(CopyUid {
                uidvalidity: 7,
                pairs: vec![(5, 9)]
            })
        );
    }

    #[test]
    fn appenduid_reads_from_the_tagged_ok() {
        let mut reader = AppendUidReader::default();
        feed(
            &mut reader,
            &["A3 OK [APPENDUID 38505 3955] APPEND completed\r\n"],
        );
        assert_eq!(
            reader.append_uid,
            Some(AppendUid {
                uidvalidity: 38505,
                uid: 3955
            })
        );
    }

    #[test]
    fn capabilities_read_from_a_greeting_or_an_answer() {
        let mut greeting = CapabilityReader::default();
        feed(
            &mut greeting,
            &["* OK [CAPABILITY IMAP4rev1 IDLE AUTH=PLAIN] Dovecot ready.\r\n"],
        );
        assert_eq!(greeting.atoms, ["IMAP4REV1", "IDLE", "AUTH=PLAIN"]);
        let mut answer = CapabilityReader::default();
        feed(&mut answer, &["* CAPABILITY IMAP4rev1 QRESYNC Move\r\n"]);
        assert_eq!(answer.atoms, ["IMAP4REV1", "QRESYNC", "MOVE"]);
    }

    #[test]
    fn internal_dates_read_with_or_without_a_leading_space() {
        assert_eq!(
            internal_date(" 7-Feb-2026 10:00:00 +0000"),
            Some(1_770_458_400_000)
        );
        assert_eq!(
            internal_date("17-Feb-2026 10:00:00 +0100"),
            Some(1_771_318_800_000)
        );
        assert_eq!(internal_date("yesterday"), None);
    }

    #[test]
    fn headers_and_flags_keep_only_the_uids_asked_for() {
        let mut headers = HeadersReader::new(&UidSet::range(10, 11));
        feed(
            &mut headers,
            &[
                "* 1 FETCH (UID 9 BODY[HEADER.FIELDS (SUBJECT)] {12}\r\nSubject: x\r\n)\r\n",
                "* 2 FETCH (UID 10 BODY[HEADER.FIELDS (SUBJECT)] {12}\r\nSubject: y\r\n)\r\n",
            ],
        );
        let fetched = headers.finish();
        assert_eq!(fetched.len(), 1);
        assert_eq!(fetched[0].uid, 10);
        let mut flags = FlagsReader::new(&UidSet::from_uids([4]));
        feed(
            &mut flags,
            &[
                "* 1 FETCH (UID 3 FLAGS (\\Seen))\r\n",
                "* 2 FETCH (UID 4 FLAGS ())\r\n",
            ],
        );
        let flags = flags.finish();
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].uid, 4);
    }

    #[test]
    fn a_qresync_select_keeps_changes_to_known_uids_only() {
        let mut reader = SelectReader::new(Some(&UidSet::from_uids([117])));
        feed(
            &mut reader,
            &[
                "* OK [UIDVALIDITY 3] ok\r\n",
                "* 49 FETCH (UID 117 FLAGS (\\Seen) MODSEQ (9))\r\n",
                "* 50 FETCH (UID 500 FLAGS (\\Seen) MODSEQ (9))\r\n",
            ],
        );
        let selected = reader.finish().unwrap();
        assert_eq!(selected.changed.len(), 1);
        assert_eq!(selected.changed[0].uid, 117);
    }

    #[test]
    fn a_command_may_bring_a_few_answers_for_each_uid_it_names() {
        assert_eq!(answers_for(&UidSet::from_uids([1, 2])), 1_006);
        assert_eq!(answers_for(&UidSet::from_uid(1)), MAX_ANSWERS);
        assert_eq!(HeadersReader::new(&UidSet::from_uids([1])).most(), 1_003);
        assert_eq!(ListReader::default().most(), MAX_ANSWERS);
        assert_eq!(SearchReader::default().most(), MAX_ANSWERS);
        assert_eq!(SectionReader::new(1).bytes(), crate::guard::BODY_BYTES);
        assert_eq!(ListReader::default().bytes(), crate::guard::COMMAND_BYTES);
    }

    /// A SELECT and a SEARCH each run on a budget of their own, which
    /// bounds the CPU a hostile answer costs: async-imap parses its whole
    /// buffer again on each read. The SEARCH caps follow from it.
    #[test]
    fn select_and_search_run_on_their_own_budgets() {
        use crate::guard::{SEARCH_BYTES, SELECT_BYTES};
        assert_eq!(SELECT_BYTES, 4 << 20);
        assert_eq!(SEARCH_BYTES, 4 << 20);
        assert_eq!(SelectReader::default().bytes(), SELECT_BYTES);
        assert_eq!(SearchReader::default().bytes(), SEARCH_BYTES);
        // A UID takes at least a digit and a space.
        assert_eq!(MAX_SEARCH_UIDS as u64, SEARCH_BYTES / 2);
    }

    /// Folding VANISHED ranges holds one range list beside the merged
    /// set, never a copy of each: 32 lines of distinct UIDs, 3.2 million
    /// ranges of 12 bytes, merge within three times the set they make.
    #[test]
    fn a_vanished_fold_holds_one_range_list_beside_the_set() {
        let lines: Vec<String> = (0..32u32)
            .map(|line| {
                let uids: Vec<String> = (0..100_000u32)
                    .map(|i| ((line * 100_000 + i) * 2 + 1).to_string())
                    .collect();
                format!("* VANISHED (EARLIER) {}\r\n", uids.join(","))
            })
            .collect();
        let final_bytes = 32 * 100_000 * std::mem::size_of::<RangeInclusive<u32>>();
        let mark = crate::testing::HeapMark::start();
        let mut reader = SelectReader::default();
        feed(&mut reader, &["* OK [UIDVALIDITY 3] ok\r\n"]);
        for line in &lines {
            feed(&mut reader, &[line]);
        }
        let selected = reader.finish().unwrap();
        let peak = mark.peak();
        eprintln!("vanished fold: peak {peak} bytes for a set of {final_bytes}");
        assert_eq!(selected.vanished.ranges().len(), 3_200_000);
        assert!(
            peak < 3 * final_bytes,
            "peak {peak} bytes for a set of {final_bytes}"
        );
        drop(selected);
    }

    #[test]
    fn a_section_reader_takes_the_bytes_of_its_uid_whole() {
        let (_, response) =
            Response::from_bytes(b"* 2 FETCH (UID 9 BODY[1] {5}\r\nhello)\r\n").unwrap();
        let mut reader = SectionReader::new(9);
        assert_eq!(reader.wants(&response), Some(&b"hello"[..]));
        reader.keep(b"hello".to_vec());
        assert_eq!(reader.bytes.as_deref(), Some(&b"hello"[..]));
        assert_eq!(SectionReader::new(8).wants(&response), None);
    }

    #[test]
    fn flags_keep_one_answer_per_uid_the_last() {
        let mut reader = FlagsReader::new(&UidSet::from_uids([4, 5]));
        feed(
            &mut reader,
            &[
                "* 1 FETCH (UID 5 FLAGS (\\Seen))\r\n",
                "* 2 FETCH (UID 4 FLAGS ())\r\n",
                "* 1 FETCH (UID 5 FLAGS (\\Flagged))\r\n",
            ],
        );
        let flags = reader.finish();
        assert_eq!(flags.len(), 2);
        assert_eq!(flags[1].uid, 5);
        assert_eq!(flags[1].flags, ["\\Flagged"]);
        let mut select = SelectReader::default();
        feed(
            &mut select,
            &[
                "* OK [UIDVALIDITY 3] ok\r\n",
                "* 1 FETCH (UID 7 FLAGS (\\Seen) MODSEQ (4))\r\n",
                "* 1 FETCH (UID 7 FLAGS () MODSEQ (5))\r\n",
            ],
        );
        let selected = select.finish().unwrap();
        assert_eq!(selected.changed.len(), 1);
        assert_eq!(selected.changed[0].modseq, Some(5));
    }

    #[test]
    fn a_message_with_more_flags_than_the_cap_is_a_protocol_error() {
        let many = |n: usize| {
            let flags: Vec<String> = (0..n).map(|i| format!("k{i}")).collect();
            format!("* 1 FETCH (UID 4 FLAGS ({}))\r\n", flags.join(" "))
        };
        let mut at_cap = FlagsReader::new(&UidSet::from_uids([4]));
        feed(&mut at_cap, &[&many(MAX_FLAGS)]);
        assert_eq!(at_cap.check(), Ok(()));
        let mut past = FlagsReader::new(&UidSet::from_uids([4]));
        feed(&mut past, &[&many(MAX_FLAGS + 1)]);
        assert!(matches!(past.check(), Err(ImapError::Protocol(_))));
        let mut headers = HeadersReader::new(&UidSet::from_uids([4]));
        feed(
            &mut headers,
            &[&many(MAX_FLAGS + 1).replace(
                "))\r\n",
                ") BODY[HEADER.FIELDS (SUBJECT)] {12}\r\nSubject: x\r\n)\r\n",
            )],
        );
        assert!(matches!(headers.check(), Err(ImapError::Protocol(_))));
        let mut select = SelectReader::default();
        feed(&mut select, &[&many(MAX_FLAGS + 1)]);
        assert!(matches!(select.check(), Err(ImapError::Protocol(_))));
    }

    /// A QRESYNC SELECT may name 100,000 expunged ranges in one line.
    #[test]
    fn a_vanished_of_a_hundred_thousand_ranges_reads_fast() {
        let ranges: Vec<String> = (0..100_000).map(|i| (i * 2 + 1).to_string()).collect();
        let line = format!("* VANISHED (EARLIER) {}\r\n", ranges.join(","));
        let started = std::time::Instant::now();
        let mut reader = SelectReader::default();
        feed(&mut reader, &["* OK [UIDVALIDITY 3] ok\r\n", &line]);
        let selected = reader.finish().unwrap();
        assert_eq!(selected.vanished.len(), 100_000);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_flag_budget_refuses_past_its_total_and_takes_back_what_is_replaced() {
        let mut budget = FlagBudget::new(5);
        assert_eq!(budget.swap(0, 3), Ok(()));
        assert_eq!(budget.swap(3, 5), Ok(()));
        assert!(matches!(budget.swap(0, 1), Err(ImapError::Protocol(_))));
        assert_eq!(FlagBudget::default().left, MAX_FLAGS_KEPT);
    }

    #[test]
    fn readers_stop_keeping_flags_past_the_commands_total() {
        let two = "* 1 FETCH (UID 1 FLAGS (a b))\r\n";
        let other = "* 2 FETCH (UID 2 FLAGS (a b))\r\n";
        let mut flags = FlagsReader::new(&UidSet::range(1, 2));
        flags.flag_budget = FlagBudget::new(3);
        // The same message again replaces its flags, which keeps the count.
        feed(&mut flags, &[two, two]);
        assert_eq!(flags.check(), Ok(()));
        feed(&mut flags, &[other]);
        assert!(matches!(flags.check(), Err(ImapError::Protocol(_))));
        let mut select = SelectReader {
            flag_budget: FlagBudget::new(3),
            ..SelectReader::default()
        };
        feed(&mut select, &[two, other]);
        assert!(matches!(select.check(), Err(ImapError::Protocol(_))));
        let header = |uid: u32| {
            format!(
                "* {uid} FETCH (UID {uid} FLAGS (a b) BODY[HEADER.FIELDS (SUBJECT)] {{12}}\r\nSubject: x\r\n)\r\n"
            )
        };
        let mut headers = HeadersReader::new(&UidSet::range(1, 2));
        headers.flag_budget = FlagBudget::new(3);
        feed(&mut headers, &[&header(1), &header(2)]);
        assert!(matches!(headers.check(), Err(ImapError::Protocol(_))));
    }

    /// Repeated VANISHED ranges fold into the set as they come, so what a
    /// SELECT holds tracks the ranges that differ, not the ones sent.
    #[test]
    fn repeated_vanished_ranges_fold_into_the_set_as_they_come() {
        let line = format!("* VANISHED (EARLIER) {}\r\n", vec!["1"; 100_000].join(","));
        let mut reader = SelectReader::default();
        feed(&mut reader, &["* OK [UIDVALIDITY 3] ok\r\n"]);
        for _ in 0..10 {
            feed(&mut reader, &[&line]);
            // At most the fold threshold plus one line's ranges wait.
            assert!(
                reader.pending.len() <= FOLD_AFTER + 2 + 100_000,
                "{}",
                reader.pending.len()
            );
        }
        assert!(reader.pending.len() < 1_000_000);
        let selected = reader.finish().unwrap();
        assert_eq!(selected.vanished.to_string(), "1");
    }

    #[test]
    fn a_search_past_the_uid_cap_is_a_protocol_error() {
        let mut reader = SearchReader {
            most_uids: 5,
            ..SearchReader::default()
        };
        feed(&mut reader, &["* SEARCH 1 2 3\r\n"]);
        assert_eq!(reader.check(), Ok(()));
        feed(&mut reader, &["* SEARCH 4 5 6\r\n"]);
        assert!(matches!(reader.check(), Err(ImapError::Protocol(_))));
        assert_eq!(SearchReader::default().most_uids, MAX_SEARCH_UIDS);
    }

    /// Sorting once at the end keeps 100,000 small answers linear.
    #[test]
    fn many_small_search_answers_read_in_linear_time() {
        let lines: Vec<String> = (0..100_000u32)
            .rev()
            .map(|uid| format!("* SEARCH {} {}\r\n", uid + 1, uid + 1))
            .collect();
        let started = std::time::Instant::now();
        let mut reader = SearchReader::default();
        for line in &lines {
            feed(&mut reader, &[line]);
        }
        let uids = reader.finish();
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(uids.len(), 100_000);
        assert_eq!(uids.first(), Some(&1));
        assert_eq!(uids.capacity(), uids.len());
    }
}
