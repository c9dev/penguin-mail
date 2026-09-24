//! The values the IMAP client hands back: what the server offers, its
//! mailboxes, a mailbox's state, and what a fetch reads.

use mail_parser::{MessageParser, MimeHeaders};
use mailrs_domain::{Address, EpochMillis};

use crate::UidSet;

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
    /// accepts any keyword a client makes up.
    pub permanent_flags: Vec<String>,
    /// Under QRESYNC, with a `Since` whose UIDVALIDITY still holds: the
    /// UIDs expunged since its MODSEQ. Empty otherwise.
    pub vanished: UidSet,
    /// Under QRESYNC, as `vanished`: the messages whose flags changed.
    pub changed: Vec<FlagsOf>,
}

impl Selected {
    /// Whether the server keeps `keyword` on its messages.
    pub fn keeps(&self, keyword: &str) -> bool {
        self.permanent_flags
            .iter()
            .any(|f| f == "\\*" || f.eq_ignore_ascii_case(keyword))
    }
}

/// One message's flags, and its MODSEQ where the server has CONDSTORE.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlagsOf {
    pub uid: u32,
    pub flags: Vec<String>,
    pub modseq: Option<u64>,
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
