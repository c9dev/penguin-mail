//! Limits on what a server may send, checked in the bytes as they arrive,
//! under async-imap and before anything parses them.
//!
//! imap-proto parses nested lists (BODYSTRUCTURE, body extensions, THREAD)
//! by recursion with no limit, and async-imap parses whatever the server
//! sends, so a few kilobytes of `(` from a hostile server would overflow
//! the stack in the parse or in dropping what it built. async-imap also
//! buffers an answer of up to 512 MiB before it gives up. The guard counts
//! parenthesis depth, line length, literal size and the bytes one command
//! brings, and fails the read past any of them.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll, ready};

use mailrs_mime::MAX_DEPTH;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// The deepest nesting a response may reach. A structure this crate keeps
/// whole, [`MAX_DEPTH`] parts deep, nests one level per part, one for the
/// FETCH list and up to three for the parameters and disposition of its
/// innermost part. A debug build spends about 20 KB of stack a level
/// parsing BODYSTRUCTURE, so this many levels fit a 2 MB tokio worker.
pub(crate) const MAX_NESTING: usize = MAX_DEPTH + 8;

/// The largest literal a server may send: the largest message part the
/// app fetches whole. A literal must also fit what the command's budget
/// has left, since async-imap sizes its buffer to the literal as soon as
/// it reads the announcement.
pub(crate) const MAX_LITERAL: u64 = 128 << 20;

/// The longest line outside literals, which a FETCH of headers or flags
/// and a LIST entry fit. An untagged SEARCH or ESEARCH answer may run
/// longer; the command's budget bounds it.
pub(crate) const MAX_LINE: usize = 1 << 20;

/// What one command may bring back in all, literals included: every
/// command but a body fetch and IDLE, from signing in to a window of
/// headers, flags or a mailbox list.
pub(crate) const COMMAND_BYTES: u64 = 32 << 20;

/// What a command fetching one body section may bring back: the section
/// and the line around it.
pub(crate) const BODY_BYTES: u64 = MAX_LITERAL + MAX_LINE as u64;

/// What the server may send during one IDLE, until its tagged answer:
/// news of new mail, expunges and flag changes, a few dozen bytes each.
pub(crate) const IDLE_BYTES: u64 = 1 << 20;

/// The words that open an answer listing UIDs on one line, which may run
/// past [`MAX_LINE`]: a mailbox of 200,000 messages lists in about
/// 1.3 MB. The command's byte budget bounds it instead; 32 MiB holds about
/// four million UIDs.
const SEARCH: [&[u8]; 2] = [b"SEARCH", b"ESEARCH"];

/// The words that open a status response, whose text imap-proto reads to
/// the line end, braces and all.
const STATUS: [&[u8]; 5] = [b"OK", b"NO", b"BAD", b"BYE", b"PREAUTH"];

/// A stream whose answers stay within the limits above.
#[derive(Debug)]
pub(crate) struct Guarded<S> {
    inner: S,
    scan: Scan,
}

impl<S> Guarded<S> {
    pub(crate) fn new(inner: S) -> Self {
        Guarded {
            inner,
            scan: Scan::new(Limits::default()),
        }
    }

    /// Starts a command that may bring back `bytes` in all.
    pub(crate) fn expect(&mut self, bytes: u64) {
        self.scan.expect(bytes);
    }
}

#[derive(Clone, Copy, Debug)]
struct Limits {
    line: usize,
    budget: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            line: MAX_LINE,
            budget: COMMAND_BYTES,
        }
    }
}

/// How far the guard has read into a response's first two words: the tag
/// (`*`, `+` or a command's), then the word that says whether the
/// response is a status response.
#[derive(Debug, Default)]
enum Head {
    #[default]
    Start,
    /// `*`, which opens an untagged response.
    Star,
    Tag,
    /// The second word so far, `None` once longer than any word it could
    /// be, and whether the response is untagged.
    Word(Option<([u8; 7], usize)>, bool),
    /// A status response or a continuation: text to the line end.
    Status,
    /// An untagged SEARCH or ESEARCH, which lists UIDs on one line.
    Search,
    Other,
}

/// A `{digits}` in the making, which announces a literal when CRLF
/// follows it.
#[derive(Debug, Default)]
enum Brace {
    #[default]
    None,
    Open(Option<u64>),
    Closed(u64),
    ClosedCr(u64),
}

/// Where the reader stands in the server's bytes.
#[derive(Debug)]
struct Scan {
    limits: Limits,
    /// Bytes the current command may still bring.
    budget: u64,
    head: Head,
    depth: usize,
    /// Bytes of the current line outside literals.
    line: usize,
    quoted: bool,
    escaped: bool,
    brace: Brace,
    /// Bytes of a literal still to come.
    literal: u64,
    refused: Option<&'static str>,
}

impl Scan {
    fn new(limits: Limits) -> Self {
        Scan {
            limits,
            budget: limits.budget,
            head: Head::Start,
            depth: 0,
            line: 0,
            quoted: false,
            escaped: false,
            brace: Brace::None,
            literal: 0,
            refused: None,
        }
    }

    fn expect(&mut self, bytes: u64) {
        self.budget = bytes;
    }

    fn feed(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut rest = bytes;
        while !rest.is_empty() && self.refused.is_none() {
            let Some(budget) = self.budget.checked_sub(1) else {
                self.refused = Some("a command's answer grew past its limit");
                break;
            };
            if self.literal > 0 {
                let skip = self.literal.min(rest.len() as u64).min(self.budget);
                self.literal -= skip;
                self.budget -= skip;
                rest = &rest[skip as usize..];
                continue;
            }
            self.budget = budget;
            self.step(rest[0]);
            rest = &rest[1..];
        }
        match self.refused {
            Some(why) => Err(io::Error::new(io::ErrorKind::InvalidData, why)),
            None => Ok(()),
        }
    }

    fn step(&mut self, byte: u8) {
        if byte == b'\n' {
            self.line_end();
            return;
        }
        self.line += 1;
        if self.line > self.limits.line && !matches!(self.head, Head::Search) {
            self.refused = Some("the server sent a line past the limit");
            return;
        }
        self.classify(byte);
        if self.quoted {
            match (self.escaped, byte) {
                (true, _) => self.escaped = false,
                (false, b'\\') => self.escaped = true,
                (false, b'"') => self.quoted = false,
                _ => {}
            }
            return;
        }
        self.brace = match (std::mem::take(&mut self.brace), byte) {
            (Brace::Open(n), b'0'..=b'9') => Brace::Open(Some(
                n.unwrap_or(0)
                    .saturating_mul(10)
                    .saturating_add(u64::from(byte - b'0')),
            )),
            (Brace::Open(Some(n)), b'}') => Brace::Closed(n),
            (Brace::Closed(n), b'\r') => Brace::ClosedCr(n),
            (_, b'{') => Brace::Open(None),
            _ => Brace::None,
        };
        match byte {
            b'"' => self.quoted = true,
            b'(' => {
                self.depth += 1;
                if self.depth > MAX_NESTING {
                    self.refused = Some("the server nested an answer past the limit");
                }
            }
            b')' => self.depth = self.depth.saturating_sub(1),
            _ => {}
        }
    }

    /// Follows the first two words, which decide whether imap-proto reads
    /// the rest of the response as text.
    fn classify(&mut self, byte: u8) {
        self.head = match (std::mem::take(&mut self.head), byte) {
            (Head::Start, b'+') => Head::Status,
            (Head::Start, b'*') => Head::Star,
            (Head::Star, b' ') => Head::Word(Some(([0; 7], 0)), true),
            (Head::Start | Head::Tag, b' ') => Head::Word(Some(([0; 7], 0)), false),
            (Head::Start | Head::Star | Head::Tag, _) => Head::Tag,
            (Head::Word(word, untagged), b' ' | b'\r') => {
                let word = word.as_ref().map(|(bytes, len)| &bytes[..*len]);
                let is = |words: &[&[u8]]| {
                    word.is_some_and(|w| words.iter().any(|s| s.eq_ignore_ascii_case(w)))
                };
                if is(&STATUS) {
                    Head::Status
                } else if untagged && is(&SEARCH) {
                    Head::Search
                } else {
                    Head::Other
                }
            }
            (Head::Word(Some((mut bytes, len)), untagged), _) if len < bytes.len() => {
                bytes[len] = byte;
                Head::Word(Some((bytes, len + 1)), untagged)
            }
            (Head::Word(_, untagged), _) => Head::Word(None, untagged),
            (head, _) => head,
        };
    }

    /// A line feed outside a literal: the start of a literal after
    /// `{n}` and CR, or else the end of the response.
    fn line_end(&mut self) {
        let announced = match std::mem::take(&mut self.brace) {
            Brace::ClosedCr(n) => Some(n),
            _ => None,
        };
        self.line = 0;
        self.quoted = false;
        self.escaped = false;
        match (announced, &self.head) {
            (Some(n), Head::Other | Head::Search) if n > MAX_LITERAL.min(self.budget) => {
                self.refused = Some("the server announced a literal past the limit");
            }
            (Some(n), Head::Other | Head::Search) => self.literal = n,
            // imap-proto reads a status response's `{n}` as text, unless
            // it sits in a response code that then parses whole; when the
            // code fails after the literal, imap-proto backtracks to text
            // and parses the literal's bytes as new responses. No reading
            // of those bytes is safe under both, and no server sends a
            // literal in a status response, so refuse it.
            (Some(_), Head::Status) => {
                self.refused = Some("a status response announced a literal");
            }
            _ => {
                self.depth = 0;
                self.head = Head::Start;
            }
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Guarded<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = &mut *self;
        let start = buf.filled().len();
        ready!(Pin::new(&mut this.inner).poll_read(cx, buf))?;
        Poll::Ready(this.scan.feed(&buf.filled()[start..]))
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Guarded<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan_with(limits: Limits, bytes: &[u8]) -> Result<usize, ()> {
        let mut scan = Scan::new(limits);
        scan.feed(bytes).map_err(|_| ())?;
        Ok(scan.depth)
    }

    fn scan(bytes: &[u8]) -> Result<usize, ()> {
        scan_with(Limits::default(), bytes)
    }

    fn deep() -> String {
        "(".repeat(MAX_NESTING + 1)
    }

    #[test]
    fn parentheses_count_until_the_line_ends() {
        assert_eq!(scan(b"* 1 FETCH (FLAGS (\\Seen"), Ok(2));
        assert_eq!(scan(b"* 1 FETCH (FLAGS (\\Seen))\r\n"), Ok(0));
        assert_eq!(scan(b"* 1 FETCH (UID 1 BODY[] {3}\r\n"), Ok(1));
    }

    #[test]
    fn parentheses_in_quoted_strings_and_literals_do_not_count() {
        let line = format!(
            "* 1 FETCH (ENVELOPE (\"{}\\\"(\" NIL) BODY[] {{4}}\r\n((((",
            "(".repeat(500)
        );
        assert_eq!(scan(line.as_bytes()), Ok(1));
    }

    #[test]
    fn a_line_nested_past_the_cap_is_refused() {
        let at_cap = "(".repeat(MAX_NESTING);
        assert_eq!(scan(at_cap.as_bytes()), Ok(MAX_NESTING));
        assert_eq!(scan(deep().as_bytes()), Err(()));
    }

    #[test]
    fn the_count_carries_across_reads() {
        let mut scan = Scan::new(Limits::default());
        for chunk in [&b"* 1 FETCH (BODY[] {"[..], b"2}\r", b"\n)", b")(", b"\r\n"] {
            scan.feed(chunk).unwrap();
        }
        assert_eq!(scan.depth, 0);
        scan.feed(b"* 2 FETCH ").unwrap();
        scan.feed("(".repeat(MAX_NESTING).as_bytes()).unwrap();
        assert!(scan.feed(b"(").is_err());
        // Once refused, the stream stays refused.
        assert!(scan.feed(b"\r\n").is_err());
    }

    /// imap-proto reads a status response's text to the line end, braces
    /// included, and falls back to text when a response code fails to
    /// parse after taking a literal, so `{n}` there never skips bytes.
    #[test]
    fn a_status_line_that_announces_a_literal_is_refused() {
        let long_tag = "A".repeat(30);
        for line in [
            format!("{long_tag} OK x{{99999}}\r\n"),
            "* OK x{99999}\r\n".to_string(),
            "* bye {99999}\r\n".to_string(),
            "A0001 NO [ALERT] {99999}\r\n".to_string(),
            "+ go {99999}\r\n".to_string(),
            "+{99999}\r\n".to_string(),
            "* OK [BADCHARSET ({4}\r\nAAAA)] x{99999}\r\n".to_string(),
            "+ [BADCHARSET ({4}\r\nAAAA)] x{99999}\r\n".to_string(),
        ] {
            assert_eq!(
                scan(format!("{line}{}", deep()).as_bytes()),
                Err(()),
                "{line:?}"
            );
        }
    }

    #[test]
    fn a_status_word_must_stand_alone() {
        // "OKAY" is no status, so its literal is one: imap-proto rejects
        // the line, and nothing after it is parsed.
        assert_eq!(scan(b"* OKAY {3}\r\n((("), Ok(0));
        assert_eq!(scan(b"* ok\r\n* 1 FETCH ("), Ok(1));
    }

    #[test]
    fn a_literal_outside_a_status_line_holds_text() {
        let deep = deep();
        let line = format!("* LIST () \"/\" {{{}}}\r\n{deep}", deep.len());
        assert_eq!(scan(line.as_bytes()), Ok(0));
    }

    #[test]
    fn only_braced_digits_before_crlf_announce_a_literal() {
        // imap-proto takes `{digits}` and CRLF, nothing looser.
        for line in [
            "* 1 FETCH ({}\r\n",
            "* 1 FETCH ({3}\n",
            "* 1 FETCH ({3+}\r\n",
        ] {
            let bytes = format!("{line}(((");
            assert_eq!(scan(bytes.as_bytes()), Ok(3), "{line:?}");
        }
    }

    #[test]
    fn a_literal_over_the_cap_is_refused_when_announced() {
        // A body fetch's budget holds the largest literal, and no more.
        let body = Limits {
            budget: BODY_BYTES,
            ..Limits::default()
        };
        let line = format!("* 1 FETCH (UID 1 BODY[] {{{}}}\r\n", MAX_LITERAL + 1);
        assert_eq!(scan_with(body, line.as_bytes()), Err(()));
        let line = format!("* 1 FETCH (UID 1 BODY[] {{{MAX_LITERAL}}}\r\n");
        assert_eq!(scan_with(body, line.as_bytes()), Ok(1));
    }

    #[test]
    fn a_line_over_the_cap_is_refused_but_a_literal_is_not_a_line() {
        let limits = Limits {
            line: 16,
            ..Limits::default()
        };
        assert_eq!(scan_with(limits, b"* LIST () \"/\" abcdef"), Err(()));
        let literal = format!("* 1 FETCH ({{64}}\r\n{})\r\n", "x".repeat(64));
        assert_eq!(scan_with(limits, literal.as_bytes()), Ok(0));
    }

    fn search_line(uids: u32) -> String {
        let mut line = String::from("* SEARCH");
        for uid in 1..=uids {
            line.push(' ');
            line.push_str(&uid.to_string());
        }
        line + "\r\n"
    }

    /// A mailbox of 200,000 messages lists in one SEARCH line of about
    /// 1.3 MB, past the line cap, which such an answer is spared.
    #[test]
    fn a_search_answer_may_run_past_the_line_cap() {
        let line = search_line(200_000);
        assert!(line.len() > MAX_LINE);
        assert_eq!(scan(line.as_bytes()), Ok(0));
        let esearch = format!(
            "* ESEARCH (TAG \"A1\") UID ALL 1:{}\r\n",
            "9".repeat(MAX_LINE)
        );
        assert_eq!(scan(esearch.as_bytes()), Ok(0));
    }

    #[test]
    fn only_an_untagged_search_answer_is_spared_the_line_cap() {
        let limits = Limits {
            line: 16,
            ..Limits::default()
        };
        for line in [
            "A1 SEARCH 1 2 3 4 5 6 7 8\r\n",
            "* SEARCHES 1 2 3 4 5 6\r\n",
            "* 1 FETCH (SEARCH 1 2 3)\r\n",
        ] {
            assert_eq!(scan_with(limits, line.as_bytes()), Err(()), "{line:?}");
        }
        assert_eq!(scan_with(limits, b"* search 1 2 3 4 5 6 7 8 9\r\n"), Ok(0));
    }

    #[test]
    fn a_search_answer_past_the_budget_is_a_protocol_error() {
        let line = search_line(20_000);
        let limits = Limits {
            budget: line.len() as u64 - 1,
            ..Limits::default()
        };
        let mut scan = Scan::new(limits);
        let err = scan.feed(line.as_bytes()).unwrap_err();
        assert!(matches!(
            crate::refusal::from_io(err),
            crate::ImapError::Protocol(_)
        ));
    }

    #[test]
    fn a_command_may_bring_only_its_budget_of_bytes() {
        let limits = Limits {
            budget: 64,
            ..Limits::default()
        };
        let mut scan = Scan::new(limits);
        scan.feed(&[b'x'; 64]).unwrap();
        assert!(scan.feed(b"x").is_err());
        let mut scan = Scan::new(limits);
        scan.feed(&[b'x'; 60]).unwrap();
        scan.expect(64);
        scan.feed(&[b'x'; 64]).unwrap();
    }

    /// async-imap sizes its buffer to a literal as soon as it reads the
    /// announcement, so a literal the budget cannot hold is refused then,
    /// before its bytes arrive.
    #[test]
    fn a_literal_past_what_the_budget_has_left_is_refused_when_announced() {
        let limits = Limits {
            budget: 1_000,
            ..Limits::default()
        };
        assert_eq!(
            scan_with(limits, b"* 1 FETCH (UID 1 BODY[] {2000}\r\n"),
            Err(())
        );
        assert_eq!(
            scan_with(limits, b"* 1 FETCH (UID 1 BODY[] {900}\r\n"),
            Ok(1)
        );
    }
}

/// A bounded random test of the guard against imap-proto itself.
/// Responses come from tags of any length, `*` and `+`, status words,
/// response codes, quoted strings with escapes, `{n}` and `~{n}` literals
/// and nested lists, and reach the guard split at random points.
#[cfg(test)]
mod fuzz {
    use async_imap::imap_proto::{AttributeValue, BodyExtension, BodyStructure, Response};

    use super::*;

    /// xorshift64*, seeded, so a failure repeats.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }

        fn one_in(&mut self, n: usize) -> bool {
            self.below(n) == 0
        }

        fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
            items[self.below(items.len())]
        }
    }

    /// A stream of responses, and what the generator knows about it: how
    /// deep its lists nest outside quoted strings and literals, and
    /// whether it holds something the guard refuses by design.
    #[derive(Default)]
    struct Stream {
        bytes: Vec<u8>,
        depth: usize,
        deepest: usize,
        /// A status response that announces a literal.
        status_literal: bool,
    }

    impl Stream {
        /// Structural text: every parenthesis counts.
        fn raw(&mut self, text: &str) {
            for byte in text.bytes() {
                match byte {
                    b'(' => {
                        self.depth += 1;
                        self.deepest = self.deepest.max(self.depth);
                    }
                    b')' => self.depth = self.depth.saturating_sub(1),
                    _ => {}
                }
            }
            self.bytes.extend_from_slice(text.as_bytes());
        }

        fn quoted(&mut self, rng: &mut Rng) {
            self.bytes.push(b'"');
            for _ in 0..rng.below(12) {
                match rng.below(8) {
                    0 => self.bytes.extend_from_slice(b"\\\""),
                    1 => self.bytes.extend_from_slice(b"\\\\"),
                    2 => self.bytes.push(b'('),
                    3 => self.bytes.push(b')'),
                    4 => self.bytes.extend_from_slice(b"{5}"),
                    _ => self.bytes.push(b'a' + rng.below(26) as u8),
                }
            }
            self.bytes.push(b'"');
        }

        fn literal(&mut self, rng: &mut Rng, eight: bool) {
            let mut content = Vec::new();
            for _ in 0..rng.below(40) {
                let piece: &[u8] = match rng.below(9) {
                    0 => b"(((((",
                    1 => b")",
                    2 => b"\r\n",
                    3 => b"\"",
                    4 => b"{3}\r\n",
                    5 => b"* OK x{9}\r\n",
                    6 => b"A1 OK ",
                    _ => b"text ",
                };
                content.extend_from_slice(piece);
            }
            if eight {
                self.bytes.push(b'~');
            }
            self.bytes
                .extend_from_slice(format!("{{{}}}\r\n", content.len()).as_bytes());
            self.bytes.extend_from_slice(&content);
        }

        fn tag(&mut self, rng: &mut Rng) {
            match rng.below(4) {
                0 => {
                    let long: String = (0..1 + rng.below(40))
                        .map(|i| char::from(b'A' + (i % 26) as u8))
                        .collect();
                    self.raw(&long);
                }
                1 => self.raw("A0001"),
                _ => self.raw("*"),
            }
        }

        /// Status text with few parentheses, so it never nests past the
        /// cap on its own.
        fn text(&mut self, rng: &mut Rng) {
            for _ in 0..rng.below(6) {
                let word = rng.pick(&["done", "(fine)", "[x]", "a\"b", "{5}", "(", ")"]);
                self.raw(" ");
                self.raw(word);
            }
        }

        fn status(&mut self, rng: &mut Rng) {
            if rng.one_in(4) {
                self.raw(rng.pick(&["+", "+ "]));
            } else {
                self.tag(rng);
                self.raw(" ");
                self.raw(rng.pick(&["OK", "no", "Bad", "BYE", "PREAUTH"]));
                self.raw(" ");
            }
            match rng.below(6) {
                0 => self.raw("[ALERT]"),
                1 => self.raw("[PERMANENTFLAGS (\\Seen \\*)]"),
                2 => {
                    self.raw("[BADCHARSET (utf-8 ");
                    self.quoted(rng);
                    self.raw(")]");
                }
                3 => {
                    self.raw("[BADCHARSET (");
                    self.status_literal = true;
                    self.literal(rng, false);
                    self.raw(")]");
                }
                _ => {}
            }
            self.text(rng);
            if self.bytes.ends_with(b"}") {
                // imap-proto reads a closing `{5}` as text, the guard as
                // an announced literal.
                self.status_literal = true;
            }
            if rng.one_in(6) {
                // imap-proto reads this as text.
                self.status_literal = true;
                self.raw(" x{200}");
            }
            self.raw("\r\n");
        }

        fn body(&mut self, rng: &mut Rng, levels: usize) {
            if levels <= 1 {
                self.raw("(\"text\" \"plain\" (\"name\" ");
                self.quoted(rng);
                self.raw(") NIL NIL \"7bit\" 5 1 NIL NIL NIL NIL");
                if rng.one_in(3) {
                    let nest = rng.below(4);
                    self.raw(" ");
                    self.raw(&"(".repeat(nest + 1));
                    self.raw("1 ");
                    self.quoted(rng);
                    self.raw(&")".repeat(nest + 1));
                }
                self.raw(")");
                return;
            }
            self.raw("(");
            // Two children only near the leaves, so a deep body stays small.
            let children = if levels < 5 { 1 + rng.below(2) } else { 1 };
            for _ in 0..children {
                self.body(rng, levels - 1);
            }
            self.raw(" \"mixed\")");
        }

        fn fetch(&mut self, rng: &mut Rng) {
            self.raw(&format!(
                "* {} FETCH (UID {}",
                1 + rng.below(9),
                1 + rng.below(99)
            ));
            for _ in 0..1 + rng.below(3) {
                match rng.below(5) {
                    0 => self.raw(" FLAGS (\\Seen $Junk)"),
                    1 => {
                        self.raw(" BODY[] ");
                        let eight = rng.one_in(8);
                        self.literal(rng, eight);
                    }
                    2 => {
                        self.raw(" BODY[1] ");
                        self.quoted(rng);
                    }
                    _ => {
                        let levels = match rng.below(10) {
                            0 => 60 + rng.below(20),
                            1 => 100 + rng.below(200),
                            _ => 1 + rng.below(8),
                        };
                        self.raw(" BODYSTRUCTURE ");
                        self.body(rng, levels);
                    }
                }
            }
            self.raw(")\r\n");
        }

        fn search(&mut self, rng: &mut Rng) {
            self.raw(rng.pick(&["* SEARCH", "* search", "A1 SEARCH"]));
            for _ in 0..rng.below(20) {
                self.raw(&format!(" {}", 1 + rng.below(99_999)));
            }
            self.raw("\r\n");
        }

        fn list(&mut self, rng: &mut Rng) {
            self.raw("* LIST (\\HasNoChildren) \"/\" ");
            match rng.one_in(2) {
                true => self.literal(rng, false),
                false => self.quoted(rng),
            }
            self.raw("\r\n");
        }
    }

    fn stream(rng: &mut Rng) -> Stream {
        let mut stream = Stream::default();
        for _ in 0..1 + rng.below(6) {
            // Each response starts at depth 0 for imap-proto and the guard.
            stream.depth = 0;
            match rng.below(5) {
                0 => stream.status(rng),
                1 => stream.list(rng),
                2 => stream.search(rng),
                _ => stream.fetch(rng),
            }
        }
        stream
    }

    fn deepest_extension(extension: &Option<BodyExtension<'_>>) -> usize {
        fn depth(extension: &BodyExtension<'_>) -> usize {
            match extension {
                BodyExtension::List(items) => 1 + items.iter().map(depth).max().unwrap_or(0),
                _ => 0,
            }
        }
        extension.as_ref().map_or(0, depth)
    }

    /// How deep imap-proto recursed to build `body`, counted from what it
    /// built: one level a part, and the lists of its extension data.
    fn body_depth(body: &BodyStructure<'_>) -> usize {
        match body {
            BodyStructure::Multipart {
                bodies, extension, ..
            } => {
                let children = bodies.iter().map(body_depth).max().unwrap_or(0);
                1 + children.max(deepest_extension(extension))
            }
            BodyStructure::Message {
                body, extension, ..
            } => 1 + body_depth(body).max(deepest_extension(extension)),
            BodyStructure::Basic { extension, .. } | BodyStructure::Text { extension, .. } => {
                1 + deepest_extension(extension)
            }
        }
    }

    fn response_depth(response: &Response<'_>) -> usize {
        match response {
            Response::Fetch(_, attributes) => {
                1 + attributes
                    .iter()
                    .map(|a| match a {
                        AttributeValue::BodyStructure(body) => body_depth(body),
                        _ => 0,
                    })
                    .max()
                    .unwrap_or(0)
            }
            _ => 0,
        }
    }

    /// The responses imap-proto reads from `bytes`, as far as it can, and
    /// whether it read all of them.
    fn parse(bytes: &[u8]) -> (Vec<usize>, bool) {
        let mut depths = Vec::new();
        let mut rest = bytes;
        while !rest.is_empty() {
            match async_imap::imap_proto::parser::parse_response(rest) {
                Ok((after, response)) => {
                    depths.push(response_depth(&response));
                    rest = after;
                }
                Err(_) => return (depths, false),
            }
        }
        (depths, true)
    }

    #[test]
    fn the_guard_refuses_what_nests_too_deep_and_passes_what_parses() {
        // imap-proto may recurse a few hundred levels here when the guard
        // is wrong; that should fail an assertion, not the stack.
        std::thread::Builder::new()
            .stack_size(256 << 20)
            .spawn(|| {
                let mut rng = Rng(0x5eed_1a2b_3c4d_5e6f);
                let (mut refused, mut passed, mut parsed_within) = (0, 0, 0);
                for round in 0..3_000 {
                    let stream = stream(&mut rng);
                    let mut scan = Scan::new(Limits::default());
                    let mut accepted = 0;
                    let mut rest = &stream.bytes[..];
                    let mut ok = true;
                    while !rest.is_empty() {
                        let size = (1 + rng.below(64)).min(rest.len());
                        if scan.feed(&rest[..size]).is_err() {
                            ok = false;
                            break;
                        }
                        accepted += size;
                        rest = &rest[size..];
                    }
                    let shown = String::from_utf8_lossy(&stream.bytes);
                    if stream.deepest > MAX_NESTING {
                        assert!(
                            !ok,
                            "round {round}: passed {} levels: {shown}",
                            stream.deepest
                        );
                    }
                    // What imap-proto builds from the bytes the guard let
                    // through never nests past the cap.
                    let (depths, _) = parse(&stream.bytes[..accepted]);
                    assert!(
                        depths.iter().all(|&d| d <= MAX_NESTING),
                        "round {round}: parsed {depths:?}: {shown}"
                    );
                    let (_, whole) = parse(&stream.bytes);
                    if whole && stream.deepest <= MAX_NESTING && !stream.status_literal {
                        assert!(ok, "round {round}: refused what imap-proto parses: {shown}");
                        parsed_within += 1;
                    } else if !whole && std::env::var_os("FUZZ_SHOW").is_some() {
                        eprintln!("unparsed: {}", shown.chars().take(300).collect::<String>());
                    }
                    match ok {
                        true => passed += 1,
                        false => refused += 1,
                    }
                }
                // Each check runs often enough to mean something.
                eprintln!(
                    "{refused} refused, {passed} passed, {parsed_within} parsed within the cap"
                );
                assert!(
                    refused > 300 && passed > 300 && parsed_within > 300,
                    "{refused} refused, {passed} passed, {parsed_within} parsed within the cap"
                );
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
