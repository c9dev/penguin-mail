//! Readers for the untagged answers a command brings back. Each takes the
//! parsed responses one at a time, the way they arrive, so a test feeds
//! it lines written by hand and no server is needed.

use async_imap::imap_proto::{
    AttributeValue, MailboxDatum, NameAttribute, Response, ResponseCode, Status, UidSetMember,
};
use chrono::DateTime;
use mailrs_domain::EpochMillis;

use crate::structure::BodyStructure;
use crate::{AppendUid, CopyUid, Fetched, FlagsOf, ImapError, Listed, Selected, SpecialUse};

/// Something that reads a command's responses, the tagged one included.
pub(crate) trait Reads {
    fn read(&mut self, response: &Response<'_>);
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
}

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
                for range in uids {
                    self.selected.vanished.insert(*range.start(), *range.end());
                }
            }
            Response::Fetch(_, attributes) => {
                if let Some(flags) = flags_of(attributes) {
                    self.selected.changed.push(flags);
                }
            }
            _ => {}
        }
    }
}

impl SelectReader {
    pub(crate) fn finish(self) -> Result<Selected, ImapError> {
        let uidvalidity = self
            .uidvalidity
            .ok_or_else(|| ImapError::Protocol("SELECT gave no UIDVALIDITY".into()))?;
        Ok(Selected {
            uidvalidity,
            ..self.selected
        })
    }
}

/// The flags of every message a FETCH answered for, in the order they came.
#[derive(Default)]
pub(crate) struct FlagsReader {
    pub(crate) flags: Vec<FlagsOf>,
}

impl Reads for FlagsReader {
    fn read(&mut self, response: &Response<'_>) {
        if let Response::Fetch(_, attributes) = response
            && let Some(flags) = flags_of(attributes)
        {
            self.flags.push(flags);
        }
    }
}

/// A FETCH answer's UID, flags and MODSEQ, or `None` for a FETCH without
/// both a UID and flags, such as a server's unasked report about a
/// message another client changed.
fn flags_of(attributes: &[AttributeValue<'_>]) -> Option<FlagsOf> {
    let mut uid = None;
    let mut flags = None;
    let mut modseq = None;
    for attribute in attributes {
        match attribute {
            AttributeValue::Uid(u) => uid = Some(*u),
            AttributeValue::Flags(list) => {
                flags = Some(list.iter().map(|f| f.to_string()).collect())
            }
            AttributeValue::ModSeq(m) => modseq = Some(*m),
            _ => {}
        }
    }
    Some(FlagsOf {
        uid: uid?,
        flags: flags?,
        modseq,
    })
}

/// The header fetch that lists messages: one [`Fetched`] per UID.
#[derive(Default)]
pub(crate) struct HeadersReader {
    pub(crate) fetched: Vec<Fetched>,
}

impl Reads for HeadersReader {
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
                AttributeValue::Flags(list) => {
                    fetched.flags = list.iter().map(|f| f.to_string()).collect();
                }
                AttributeValue::InternalDate(date) => fetched.internal_date = internal_date(date),
                AttributeValue::Rfc822Size(size) => fetched.size = Some(u64::from(*size)),
                AttributeValue::ModSeq(m) => fetched.modseq = Some(*m),
                _ => {}
            }
        }
        self.fetched.push(fetched);
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
        let Response::Fetch(_, attributes) = response else {
            return;
        };
        if !attributes
            .iter()
            .any(|a| matches!(a, AttributeValue::Uid(u) if *u == self.uid))
        {
            return;
        }
        for attribute in attributes {
            if let AttributeValue::BodySection { data, .. } = attribute {
                self.bytes = Some(data.as_deref().map(<[u8]>::to_vec).unwrap_or_default());
            }
        }
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
    fn read(&mut self, response: &Response<'_>) {
        let Response::Fetch(_, attributes) = response else {
            return;
        };
        if !attributes
            .iter()
            .any(|a| matches!(a, AttributeValue::Uid(u) if *u == self.uid))
        {
            return;
        }
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

/// SEARCH's answer, lowest UID first.
#[derive(Default)]
pub(crate) struct SearchReader {
    pub(crate) uids: Vec<u32>,
}

impl Reads for SearchReader {
    fn read(&mut self, response: &Response<'_>) {
        if let Response::MailboxData(MailboxDatum::Search(uids)) = response {
            self.uids.extend(uids);
            self.uids.sort_unstable();
            self.uids.dedup();
        }
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
        let mut reader = HeadersReader::default();
        feed(
            &mut reader,
            &[
                "* 1 FETCH (UID 12 FLAGS (\\Seen) INTERNALDATE \" 7-Feb-2026 10:00:00 +0100\" RFC822.SIZE 4201 MODSEQ (9) BODY[HEADER.FIELDS (FROM SUBJECT)] {32}\r\nFrom: a@b.pt\r\nSubject: hello\r\n\r\n)\r\n",
                "* 2 FETCH (UID 13 FLAGS (\\Flagged))\r\n",
            ],
        );
        assert_eq!(reader.fetched.len(), 1);
        let fetched = &reader.fetched[0];
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
        let mut reader = FlagsReader::default();
        feed(
            &mut reader,
            &[
                "* 3 FETCH (UID 12 FLAGS (\\Seen $Muted) MODSEQ (90))\r\n",
                "* 4 FETCH (FLAGS (\\Seen))\r\n",
            ],
        );
        assert_eq!(
            reader.flags,
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
        assert_eq!(reader.uids, [2, 84, 882]);
        let mut empty = SearchReader::default();
        feed(&mut empty, &["* SEARCH\r\n"]);
        assert!(empty.uids.is_empty());
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
}
