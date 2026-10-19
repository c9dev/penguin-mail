//! The values the IMAP client hands back: what the server offers, its
//! mailboxes, a mailbox's state, and what a fetch reads.

use mail_parser::{MessageParser, MimeHeaders};
use mailrs_domain::{Address, EpochMillis};

use crate::{ImapError, UidSet};

/// What the server offers, read after login: servers such as Gmail and
/// iCloud list CONDSTORE and QRESYNC only to a signed-in user.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// IDLE (RFC 2177): the server tells a waiting connection about new mail.
    pub idle: bool,
    /// CONDSTORE (RFC 7162): each message carries a MODSEQ, so a sync asks
    /// only for flags that changed. QRESYNC implies it.
    pub condstore: bool,
    /// QRESYNC (RFC 7162), offered and turned on with `ENABLE QRESYNC`:
    /// one SELECT reports changed flags and expunged UIDs.
    pub qresync: bool,
    /// MOVE (RFC 6851).
    pub moves: bool,
    /// UIDPLUS (RFC 4315): COPYUID, APPENDUID and `UID EXPUNGE`.
    pub uidplus: bool,
    /// SPECIAL-USE (RFC 6154): LIST marks Sent, Drafts, Trash and the rest.
    pub special_use: bool,
    /// QUOTA (RFC 9208, RFC 2087): GETQUOTAROOT says how full the account is.
    pub quota: bool,
    /// X-GM-EXT-1: Gmail's message and thread ids, labels and raw search.
    /// [`Capabilities::all`] leaves it off, as only Gmail offers it.
    pub gmail: bool,
}

impl Capabilities {
    /// Every extension on, as Dovecot, Fastmail and Stalwart offer.
    pub fn all() -> Self {
        Capabilities {
            idle: true,
            condstore: true,
            qresync: true,
            moves: true,
            uidplus: true,
            special_use: true,
            quota: true,
            gmail: false,
        }
    }

    /// Read from the atoms of a CAPABILITY answer, in any case. `qresync`
    /// here means offered; a connection clears it when `ENABLE` fails.
    pub fn from_atoms<'a>(atoms: impl IntoIterator<Item = &'a str>) -> Self {
        let mut caps = Capabilities::default();
        for atom in atoms {
            match atom.to_ascii_uppercase().as_str() {
                "IDLE" => caps.idle = true,
                "CONDSTORE" => caps.condstore = true,
                "QRESYNC" => {
                    caps.qresync = true;
                    caps.condstore = true;
                }
                "MOVE" => caps.moves = true,
                "UIDPLUS" => caps.uidplus = true,
                "SPECIAL-USE" => caps.special_use = true,
                "QUOTA" => caps.quota = true,
                "X-GM-EXT-1" => caps.gmail = true,
                atom if atom.starts_with("QUOTA=") => caps.quota = true,
                _ => {}
            }
        }
        caps
    }
}

/// The special use a server gives a mailbox in its LIST answer (RFC 6154).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SpecialUse {
    All,
    Archive,
    Drafts,
    Flagged,
    Junk,
    Sent,
    Trash,
}

/// One resource of one quota root (RFC 9208): `STORAGE` counts KiB,
/// `MESSAGE` counts messages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuotaUse {
    pub root: String,
    /// Upper case, as RFC 9208 names it.
    pub resource: String,
    pub usage: u64,
    pub limit: u64,
}

/// One mailbox from LIST.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    /// The server's name for it, in modified UTF-7 with the hierarchy
    /// delimiter inside: every command takes this.
    pub name: String,
    /// `name` decoded for a person to read, delimiters included.
    pub display: String,
    /// What separates levels in `name`, such as `/` or `.`. `None` for a
    /// server with a flat list.
    pub delimiter: Option<char>,
    pub special_use: Option<SpecialUse>,
    /// `\Noselect` or `\NonExistent`: a parent in the tree that holds no
    /// mail and cannot be selected.
    pub no_select: bool,
}

impl Listed {
    pub fn new(
        name: impl Into<String>,
        delimiter: Option<char>,
        special_use: Option<SpecialUse>,
        no_select: bool,
    ) -> Self {
        let name = name.into();
        Listed {
            display: crate::utf7::decode(&name),
            name,
            delimiter,
            special_use,
            no_select,
        }
    }
}

/// What the adapter last knew of a mailbox, for `SELECT ... (QRESYNC ...)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Since {
    pub uidvalidity: u32,
    /// The HIGHESTMODSEQ the last sync ended at.
    pub modseq: u64,
    /// The UIDs the store holds in the mailbox, so the server reports
    /// which of them vanished without listing every expunge it remembers.
    pub known: Option<UidSet>,
}

/// A mailbox as SELECT reported it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selected {
    pub uidvalidity: u32,
    /// `None` when the server did not say, which RFC 3501 allows.
    pub uidnext: Option<u32>,
    /// `None` without CONDSTORE, or on a mailbox the server keeps no
    /// MODSEQs for (`NOMODSEQ`).
    pub highestmodseq: Option<u64>,
    /// How many messages the mailbox holds.
    pub exists: u32,
    /// The flags and keywords the server keeps for good. `\*` means it
    /// accepts any keyword a client makes up. A server that sends no
    /// PERMANENTFLAGS keeps every flag (RFC 3501 section 6.3.1), and this
    /// then lists the system flags and `\*`.
    pub permanent_flags: Vec<String>,
    /// Under QRESYNC, with a `Since` whose UIDVALIDITY still holds: the
    /// UIDs expunged since its MODSEQ. Empty otherwise. When `Since`
    /// named the store's UIDs, only those. Without them the set is the
    /// server's, and one range can name four billion UIDs: test your own
    /// UIDs with [`UidSet::contains`] rather than walk it.
    pub vanished: UidSet,
    /// Under QRESYNC, as `vanished`: the messages whose flags changed.
    pub changed: Vec<FlagsOf>,
    /// Whether the SELECT asked for QRESYNC's report: the connection has
    /// QRESYNC on and `Since` had a MODSEQ of 1 or more. When false,
    /// empty `vanished` and `changed` say nothing about what changed, so
    /// the caller must not move its MODSEQ on from them. Each connection
    /// turns QRESYNC on for itself, and on one the server may refuse.
    pub qresync: bool,
}

impl Selected {
    /// Whether the server keeps `flag` on its messages. `\*` covers
    /// keywords only: a system flag such as `\Deleted` is kept when the
    /// server lists it.
    pub fn keeps(&self, flag: &str) -> bool {
        self.permanent_flags
            .iter()
            .any(|f| (f == "\\*" && !flag.starts_with('\\')) || f.eq_ignore_ascii_case(flag))
    }
}

/// `text` as an IMAP quoted string. A caller that sends it through
/// `search_command` gets non-ASCII text and line breaks as literals.
fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        if matches!(c, '"' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// `X-GM-RAW <query>`: Gmail's own search syntax as a UID SEARCH key.
/// Text beyond printable ASCII goes as a literal when the search is sent.
pub fn gm_raw(query: &str) -> Result<String, ImapError> {
    if query.contains('\0') {
        return Err(ImapError::Invalid("a search holds a NUL".into()));
    }
    Ok(format!("X-GM-RAW {}", quoted(query)))
}

/// A search key for the messages with these X-GM-MSGIDs. Callers pass at
/// least one id and at most 100 and batch the rest.
pub fn gm_msgid(ids: &[u64]) -> String {
    match ids {
        [] => String::new(),
        [id] => format!("X-GM-MSGID {id}"),
        [id, rest @ ..] => format!("OR X-GM-MSGID {id} {}", gm_msgid(rest)),
    }
}

/// A search key for the messages of one X-GM-THRID.
pub fn gm_thrid(id: u64) -> String {
    format!("X-GM-THRID {id}")
}

/// A search key for the messages with one label. Refused for a label no
/// command can carry, so a name cannot add keys of its own. Gmail's SEARCH
/// takes the label as a string only: a system label written as an atom
/// (`X-GM-LABELS \Inbox`) is answered "Could not parse command", so even
/// those go quoted here, unlike in STORE.
pub fn gm_labels(label: &str) -> Result<String, ImapError> {
    check_label(label)?;
    let text = match label.starts_with('\\') {
        true => quoted(label),
        false => label_text(label),
    };
    Ok(format!("X-GM-LABELS {text}"))
}

/// A label as a command writes it: a system label (`\\Inbox`) as an
/// atom, any other quoted in modified UTF-7.
pub(crate) fn label_text(label: &str) -> String {
    match label.starts_with('\\') {
        true => label.to_string(),
        false => quoted(&crate::utf7::encode(label)),
    }
}

/// Checks a label can go into a command at all, so a bad one never reaches
/// the server: no NUL, CR or LF, and a system label must be an atom.
pub(crate) fn check_label(label: &str) -> Result<(), ImapError> {
    let bad_atom = |c: char| !c.is_ascii_graphic() || "(){%*\"\\]".contains(c);
    let invalid = match label.strip_prefix('\\') {
        Some(atom) => atom.is_empty() || atom.chars().any(bad_atom),
        None => label.is_empty() || label.contains(['\0', '\r', '\n']),
    };
    match invalid {
        true => Err(ImapError::Invalid(format!("{label:?} is not a label"))),
        false => Ok(()),
    }
}

/// What Gmail adds to a message over IMAP (X-GM-EXT-1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GmailMeta {
    /// X-GM-MSGID. In hex it is the Gmail API's message id.
    pub msgid: u64,
    /// X-GM-THRID. In hex it is the Gmail API's thread id.
    pub thrid: u64,
    /// Labels decoded from modified UTF-7. System labels keep their
    /// backslash: `\Inbox`, `\Important`, `\Sent`, `\Draft`, `\Starred`.
    pub labels: Vec<String>,
}

impl GmailMeta {
    /// The message id as the Gmail API writes it.
    pub fn id(&self) -> String {
        format!("{:x}", self.msgid)
    }

    /// The thread id as the Gmail API writes it.
    pub fn thread_id(&self) -> String {
        format!("{:x}", self.thrid)
    }
}

/// One message's flags, and its MODSEQ where the server has CONDSTORE.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlagsOf {
    pub uid: u32,
    pub flags: Vec<String>,
    pub modseq: Option<u64>,
    /// Gmail's ids and labels. `None` unless the fetch asked for them.
    pub gmail: Option<GmailMeta>,
}

/// What the adapter needs to list a message without its body: flags,
/// dates, size and the headers the thread list, threading and
/// unsubscribing read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fetched {
    pub uid: u32,
    pub flags: Vec<String>,
    /// INTERNALDATE: when the server received the message.
    pub internal_date: Option<EpochMillis>,
    /// RFC822.SIZE. `None` when the server sent none.
    pub size: Option<u64>,
    pub modseq: Option<u64>,
    pub from: Option<Address>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    /// Decoded from any RFC 2047 encoded words. Empty when there is none.
    pub subject: String,
    /// The header's value as sent, angle brackets included.
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    /// Oldest first, as the header lists them, each in angle brackets.
    pub references: Vec<String>,
    pub list_unsubscribe: Option<String>,
    pub list_unsubscribe_post: Option<String>,
    /// The top-level type is `multipart/mixed`, which is how most mail
    /// with attachments arrives.
    pub mixed: bool,
    /// Gmail's ids and labels. `None` unless the fetch asked for them.
    pub gmail: Option<GmailMeta>,
}

/// The header fields [`Fetched::from_header`] reads, in the form
/// `BODY.PEEK[HEADER.FIELDS (...)]` asks for them.
pub const HEADER_FIELDS: &str = "FROM TO CC SUBJECT MESSAGE-ID IN-REPLY-TO REFERENCES \
     LIST-UNSUBSCRIBE LIST-UNSUBSCRIBE-POST CONTENT-TYPE";

impl Fetched {
    /// A message's header fields read from `header`, the bytes of a
    /// header block or of the whole message. Flags, dates, size and MODSEQ
    /// stay empty for the caller to fill.
    pub fn from_header(uid: u32, header: &[u8]) -> Fetched {
        let mut fetched = Fetched {
            uid,
            ..Fetched::default()
        };
        let Some(message) = MessageParser::default().parse_headers(header) else {
            return fetched;
        };
        let addresses = |value: Option<&mail_parser::Address>| -> Vec<Address> {
            value
                .map(|list| list.iter().filter_map(address).collect())
                .unwrap_or_default()
        };
        fetched.from = addresses(message.from()).into_iter().next();
        fetched.to = addresses(message.to());
        fetched.cc = addresses(message.cc());
        fetched.subject = message.subject().unwrap_or_default().to_string();
        let raw = |name: &str| {
            message
                .header_raw(name)
                .map(unfold)
                .filter(|value| !value.is_empty())
        };
        fetched.message_id = raw("Message-ID");
        fetched.in_reply_to = raw("In-Reply-To");
        fetched.references = raw("References")
            .map(|value| {
                value
                    .split(|c: char| c.is_whitespace() || c == ',')
                    .filter(|id| !id.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        fetched.list_unsubscribe = raw("List-Unsubscribe");
        fetched.list_unsubscribe_post = raw("List-Unsubscribe-Post");
        fetched.mixed = message.content_type().is_some_and(|ct| {
            ct.ctype().eq_ignore_ascii_case("multipart")
                && ct
                    .subtype()
                    .is_some_and(|sub| sub.eq_ignore_ascii_case("mixed"))
        });
        fetched
    }
}

fn address(addr: &mail_parser::Addr) -> Option<Address> {
    let email = addr.address()?.trim().to_string();
    if email.is_empty() {
        return None;
    }
    Some(Address {
        name: addr
            .name()
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty()),
        email,
    })
}

/// A raw header value with its folds undone.
fn unfold(value: &str) -> String {
    value
        .replace("\r\n", "")
        .replace('\n', "")
        .trim()
        .to_string()
}

/// Where COPY or MOVE put the messages, from UIDPLUS's COPYUID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopyUid {
    /// The destination mailbox's UIDVALIDITY.
    pub uidvalidity: u32,
    /// Each source UID with its copy's UID in the destination.
    pub pairs: Vec<(u32, u32)>,
}

/// Where APPEND put the message, from UIDPLUS's APPENDUID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppendUid {
    pub uidvalidity: u32,
    pub uid: u32,
}

/// Why an IDLE ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Woke {
    /// The server reported new mail, an expunge or a flag change.
    Changed,
    /// The time limit passed with no word from the server.
    TimedOut,
    /// The server sent more during the IDLE than the guard lets through,
    /// and the client dropped the connection. Whatever the server reported
    /// went with it, so the caller syncs; a server that does this at every
    /// IDLE is one to ask less often.
    Dropped,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gm_raw_quotes_ascii() {
        assert_eq!(
            gm_raw("has:attachment older_than:1y").unwrap(),
            "X-GM-RAW \"has:attachment older_than:1y\""
        );
    }

    #[test]
    fn gm_raw_escapes_quotes_and_backslashes() {
        assert_eq!(
            gm_raw("subject:\"a b\" \\").unwrap(),
            "X-GM-RAW \"subject:\\\"a b\\\" \\\\\""
        );
    }

    #[test]
    fn gm_raw_sends_utf8_as_a_literal() {
        let keys = gm_raw("from:joão").unwrap();
        let (head, literals) = crate::connection::search_command(&keys).unwrap();
        assert_eq!(head, "UID SEARCH CHARSET UTF-8 X-GM-RAW ");
        assert_eq!(literals, [("from:joão".as_bytes().to_vec(), String::new())]);
    }

    #[test]
    fn gm_raw_refuses_nul() {
        assert!(matches!(gm_raw("a\0b"), Err(ImapError::Invalid(_))));
    }

    #[test]
    fn a_gm_raw_line_break_never_ends_the_command() {
        let keys = gm_raw("a\r\nA9 DELETE INBOX").unwrap();
        let (head, literals) = crate::connection::search_command(&keys).unwrap();
        assert!(!head.contains('\n'));
        assert_eq!(literals.len(), 1);
    }

    #[test]
    fn one_msgid_is_a_plain_key() {
        assert_eq!(gm_msgid(&[255]), "X-GM-MSGID 255");
    }

    #[test]
    fn several_msgids_nest_their_ors() {
        assert_eq!(
            gm_msgid(&[1, 2, 3]),
            "OR X-GM-MSGID 1 OR X-GM-MSGID 2 X-GM-MSGID 3"
        );
    }

    #[test]
    fn gm_thrid_names_a_thread() {
        assert_eq!(gm_thrid(256), "X-GM-THRID 256");
    }

    #[test]
    fn gm_labels_quotes_every_label_for_a_search() {
        assert_eq!(
            gm_labels("Fam\u{ed}lia/F\u{e9}rias 2026").unwrap(),
            "X-GM-LABELS \"Fam&AO0-lia/F&AOk-rias 2026\""
        );
        assert_eq!(gm_labels("\\Inbox").unwrap(), "X-GM-LABELS \"\\\\Inbox\"");
        assert_eq!(gm_labels("Work").unwrap(), "X-GM-LABELS \"Work\"");
        assert!(gm_labels("\\Inbox) OR ALL (").is_err());
    }

    #[test]
    fn x_gm_ext_1_marks_gmail() {
        assert!(Capabilities::from_atoms(["IMAP4rev1", "x-gm-ext-1"]).gmail);
        assert!(!Capabilities::from_atoms(["IMAP4rev1"]).gmail);
        assert!(!Capabilities::all().gmail);
    }

    const HEADER: &[u8] = b"From: =?UTF-8?Q?Jo=C3=A3o?= <joao@example.pt>\r\n\
To: ann@example.com, \"Bob B.\" <bob@example.com>\r\n\
Cc: Team: carol@example.com;\r\n\
Subject: =?UTF-8?B?UmV1bmnDo28=?=\r\n\
Message-ID: <m2@example.pt>\r\n\
In-Reply-To: <m1@example.com>\r\n\
References: <m0@example.com>\r\n <m1@example.com>\r\n\
List-Unsubscribe: <https://example.pt/u?x=1>\r\n\
List-Unsubscribe-Post: List-Unsubscribe=One-Click\r\n\
Content-Type: multipart/mixed; boundary=\"b\"\r\n\r\n";

    #[test]
    fn a_header_block_fills_every_field() {
        let fetched = Fetched::from_header(7, HEADER);
        assert_eq!(fetched.uid, 7);
        assert_eq!(
            fetched.from,
            Some(Address {
                name: Some("João".into()),
                email: "joao@example.pt".into()
            })
        );
        assert_eq!(
            fetched
                .to
                .iter()
                .map(|a| a.email.as_str())
                .collect::<Vec<_>>(),
            ["ann@example.com", "bob@example.com"]
        );
        assert_eq!(fetched.cc[0].email, "carol@example.com");
        assert_eq!(fetched.subject, "Reunião");
        assert_eq!(fetched.message_id.as_deref(), Some("<m2@example.pt>"));
        assert_eq!(fetched.in_reply_to.as_deref(), Some("<m1@example.com>"));
        assert_eq!(fetched.references, ["<m0@example.com>", "<m1@example.com>"]);
        assert_eq!(
            fetched.list_unsubscribe.as_deref(),
            Some("<https://example.pt/u?x=1>")
        );
        assert_eq!(
            fetched.list_unsubscribe_post.as_deref(),
            Some("List-Unsubscribe=One-Click")
        );
        assert!(fetched.mixed);
    }

    #[test]
    fn a_message_without_those_headers_leaves_them_empty() {
        let fetched = Fetched::from_header(1, b"Subject: hi\r\n\r\n");
        assert_eq!(fetched.from, None);
        assert!(fetched.references.is_empty());
        assert_eq!(fetched.message_id, None);
        assert!(!fetched.mixed);
    }

    #[test]
    fn capabilities_read_their_atoms_in_any_case() {
        let caps = Capabilities::from_atoms(["IMAP4rev1", "idle", "Move", "UIDPLUS", "AUTH=PLAIN"]);
        assert!(caps.idle && caps.moves && caps.uidplus);
        assert!(!caps.condstore && !caps.qresync && !caps.special_use);
    }

    #[test]
    fn quota_is_read_from_its_atom_or_its_resources() {
        assert!(Capabilities::from_atoms(["IMAP4rev1", "QUOTA"]).quota);
        assert!(Capabilities::from_atoms(["quota=res-storage"]).quota);
        assert!(!Capabilities::from_atoms(["IMAP4rev1", "IDLE"]).quota);
    }

    #[test]
    fn qresync_brings_condstore_with_it() {
        let caps = Capabilities::from_atoms(["QRESYNC"]);
        assert!(caps.qresync && caps.condstore);
    }

    #[test]
    fn a_star_in_permanent_flags_keeps_any_keyword() {
        let open = Selected {
            permanent_flags: vec!["\\Seen".into(), "\\*".into()],
            ..Selected::default()
        };
        let closed = Selected {
            permanent_flags: vec!["\\Seen".into(), "$Forwarded".into()],
            ..Selected::default()
        };
        assert!(open.keeps("$muted"));
        assert!(closed.keeps("$forwarded"));
        assert!(!closed.keeps("$muted"));
    }

    /// RFC 3501: `\*` lets a client make up keywords. A system flag the
    /// server does not list stays temporary.
    #[test]
    fn a_star_keeps_keywords_and_no_unlisted_system_flag() {
        let open = Selected {
            permanent_flags: vec!["\\Seen".into(), "\\*".into()],
            ..Selected::default()
        };
        assert!(open.keeps("\\seen"));
        assert!(open.keeps("$Junk"));
        assert!(!open.keeps("\\Deleted"));
    }

    #[test]
    fn a_listed_name_decodes_for_display() {
        let listed = Listed::new(
            "INBOX/Envoy&AOk-s",
            Some('/'),
            Some(SpecialUse::Sent),
            false,
        );
        assert_eq!(listed.display, "INBOX/Envoyés");
        assert_eq!(listed.name, "INBOX/Envoy&AOk-s");
    }
}
