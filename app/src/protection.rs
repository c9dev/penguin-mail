//! What the two standards share: the words the card speaks, the MIME both
//! read the same way, and which of the two a message needs.
//!
//! `pgp` and `smime` are the adapters, one per standard, and neither has
//! to know the other exists. Each turns what its engine said into a
//! [`Found`], in words neither standard owns, and [`read`] is the one place
//! that turns a `Found` into a card and a body. The body always comes out
//! of the bytes the engine checked or opened, so a part the signature does
//! not cover never shows under the signer's name, whichever standard and
//! whichever shape of message it was.

pub mod draft;
pub mod run;

use mail_parser::{MessageParser, MimeHeaders};
use mailrs_domain::translate::{fill, fill_plural, gettext};
use mailrs_domain::{MessageBody, Protection};
use serde::{Deserialize, Serialize};

use crate::compose::Draft;
use crate::core::Core;
use crate::{pgp, smime};

/// What the engine made of one message: the mark to put above it, and the
/// body to draw in place of the one that arrived, cut from what the engine
/// checked or opened. A refusal leaves the body as it arrived.
pub struct Read {
    pub mark: Mark,
    pub body: Option<MessageBody>,
    /// The bytes of the files inside, in the order `body.attachments`
    /// lists them. The body names none by Gmail's attachment id, since
    /// Gmail's ids point at parts of the message as it arrived, which for
    /// an encrypted one is ciphertext; these bytes are what the window
    /// saves and opens.
    pub files: Vec<Vec<u8>>,
}

/// What the card says about a message, and how loudly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mark {
    pub title: String,
    /// The line under the title, when there is more to say.
    pub detail: Option<String>,
    pub tone: Tone,
}

/// How much of the message the card is vouching for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// The text is the text the signer wrote.
    Good,
    /// Something is wrong with the message.
    Bad,
    /// Nothing on this computer could check it.
    Unchecked,
}

/// Which engine a message needs, and which of its calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    Pgp(pgp::Opening),
    Smime(smime::Opening),
}

/// Which standard a message goes out under.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Standard {
    #[default]
    Pgp,
    Smime,
}

/// What each engine holds for a set of addresses. An engine this computer
/// does not have answers nothing at all, which is not the same as holding
/// nothing.
#[derive(Debug, Clone, Default)]
pub struct Held {
    pub pgp: Option<Vec<mailrs_pgp::Recipient>>,
    pub smime: Option<Vec<mailrs_smime::Recipient>>,
}

/// Which call `body` needs before it is drawn, from whichever engine. The
/// wrapper the message arrived in names its standard, and a body with no
/// wrapper is left to OpenPGP, which is the only one of the two that also
/// lives in the text.
pub fn engine(body: &MessageBody) -> Option<Engine> {
    match body.protection {
        Some(Protection::SmimeSigned) => Some(Engine::Smime(smime::Opening::Verify)),
        Some(Protection::SmimeOpaque) => Some(Engine::Smime(smime::Opening::Opaque)),
        Some(Protection::SmimeEnveloped) => Some(Engine::Smime(smime::Opening::Decrypt)),
        _ => pgp::opening(body).map(Engine::Pgp),
    }
}

/// Which standard would encrypt this draft, or what stands in the way.
///
/// OpenPGP wins when both could carry it, so that nothing about a message
/// the app already knew how to send changes the day gpgsm turns up.
/// `blind` says the draft carries a Bcc. OpenPGP keeps a blind copy blind
/// by leaving that reader's key id out of the message ([`Addressees`]);
/// S/MIME names every recipient inside the envelope and has no way not
/// to, so a draft with a Bcc goes out under OpenPGP or not encrypted.
pub fn encrypting(held: &Held, blind: bool) -> Result<Standard, String> {
    let pgp = held.pgp.as_deref().map(pgp::cannot_encrypt);
    let smime = held.smime.as_deref().map(smime::cannot_encrypt);
    match (pgp, smime) {
        (Some(None), _) => Ok(Standard::Pgp),
        (pgp, Some(None)) if blind => Err(match pgp {
            Some(Some(problem)) => fill(
                &gettext("{smime} {pgp}"),
                &[("smime", &smime_names_everyone()), ("pgp", &problem)],
            ),
            _ => smime_names_everyone(),
        }),
        (_, Some(None)) => Ok(Standard::Smime),
        (Some(Some(pgp)), Some(Some(smime))) => Err(neither(
            held.pgp.as_deref().unwrap_or_default(),
            held.smime.as_deref().unwrap_or_default(),
            &pgp,
            &smime,
        )),
        (Some(Some(problem)), None) | (None, Some(Some(problem))) => Err(problem),
        (None, None) => Err(gettext("This computer has nothing to encrypt with.")),
    }
}

/// Why S/MIME will not carry a draft with a Bcc.
pub fn smime_names_everyone() -> String {
    gettext(
        "S/MIME names every recipient inside an encrypted message, so a blind copy would not \
         stay blind.",
    )
}

/// Who an encrypted message goes to, sorted the way the engines need: the
/// people every reader may see, and the blind copies nobody else may.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Addressees {
    from: String,
    /// To and Cc, each once.
    named: Vec<String>,
    /// Bcc, less anyone already in To or Cc, whom the others see anyway.
    blind: Vec<String>,
}

impl Addressees {
    pub fn of(draft: &Draft) -> Addressees {
        let mut named: Vec<String> = Vec::new();
        for address in draft.to.iter().chain(&draft.cc) {
            add(&mut named, &address.email);
        }
        let mut blind: Vec<String> = Vec::new();
        for address in &draft.bcc {
            if !holds(&named, &address.email) {
                add(&mut blind, &address.email);
            }
        }
        Addressees {
            from: draft.from.email.trim().to_string(),
            named,
            blind,
        }
    }

    /// Whether anyone reads this message on a blind copy.
    pub fn has_blind_copy(&self) -> bool {
        self.blind.iter().any(|email| !same(email, &self.from))
    }

    /// The readers gpg writes the message for. With `own`, gpg holds a key
    /// for the sender, who goes in by name so the copy in Sent stays
    /// readable. A sender who put themselves in Bcc is named too, since
    /// the From line gives them away anyway.
    pub fn readers(&self, own: bool) -> mailrs_pgp::Readers {
        let mut named = self.named.clone();
        let mut hidden = self.blind.clone();
        if own {
            hidden.retain(|email| !same(email, &self.from));
            add(&mut named, &self.from);
        }
        mailrs_pgp::Readers { named, hidden }
    }

    /// The certificates gpgsm envelopes the message for, which it names
    /// one and all. [`encrypting`] keeps a draft with a blind copy away
    /// from S/MIME, so nobody is left out here.
    pub fn certificates(&self, own: bool) -> Vec<String> {
        let readers = self.readers(own);
        let mut all = readers.named;
        for email in readers.hidden {
            add(&mut all, &email);
        }
        all
    }
}

fn same(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

fn holds(list: &[String], email: &str) -> bool {
    list.iter().any(|held| same(held, email))
}

fn add(list: &mut Vec<String>, email: &str) {
    if !holds(list, email) {
        list.push(email.trim().to_string());
    }
}

/// Which standard signs a message from this address. The sender's own
/// holdings decide it, and OpenPGP wins a tie for the reason
/// [`encrypting`] gives.
pub fn signing(held: &Held) -> Standard {
    let key = held
        .pgp
        .as_deref()
        .is_some_and(|held| held.iter().any(|recipient| recipient.key.is_some()));
    let certificate = held
        .smime
        .as_deref()
        .is_some_and(|held| held.iter().any(|it| it.certificate.is_some()));
    match (key, certificate) {
        (false, true) => Standard::Smime,
        _ => Standard::Pgp,
    }
}

/// What each engine holds for `addresses`. An engine this computer does
/// not have is asked nothing, and one that would not answer is left out
/// the same way, since a question nobody answered is not a missing key.
/// The composer asks this as the recipients change, and the assistant
/// asks it before a message goes out encrypted.
pub async fn held(core: &Core, addresses: &[String]) -> Held {
    let mut held = Held::default();
    if core.has_gpg() {
        let wanted = addresses.to_vec();
        match core.gpg(move |pgp| pgp.keys_for(&wanted)).await {
            Ok(keys) => held.pgp = Some(keys),
            Err(err) => tracing::info!(error = %err, "could not ask gpg about the addresses"),
        }
    }
    if core.has_gpgsm() {
        let wanted = addresses.to_vec();
        match core
            .gpgsm(move |smime| smime.certificates_for(&wanted))
            .await
        {
            Ok(certificates) => held.smime = Some(certificates),
            Err(err) => tracing::info!(error = %err, "could not ask gpgsm about the addresses"),
        }
    }
    held
}

/// Which standard signs a message from `from`, going by what both engines
/// hold of the sender's own.
pub async fn signing_for(core: &Core, from: &str) -> Standard {
    let from = vec![from.trim().to_lowercase()];
    let mut held = held(core, &from).await;
    // Signing needs a secret key. Only gpgsm is asked for one outright:
    // gpg lists a key of the person's own whichever half the question was
    // about.
    if core.has_gpgsm() {
        held.smime = core
            .gpgsm(move |smime| smime.signing_certificates(&from))
            .await
            .ok();
    }
    signing(&held)
}

/// What the Encrypt button says once it works, which names the standard
/// the message would go out under rather than making the writer guess.
pub fn encrypting_with(standard: Standard) -> String {
    match standard {
        Standard::Pgp => gettext("Encrypt this message to the recipients' keys"),
        Standard::Smime => gettext("Encrypt this message to the recipients' certificates"),
    }
}

/// What to say when neither standard reaches every recipient. Somebody
/// nothing here can reach is the likelier answer, so that is the one the
/// button gives; a draft that each standard covers half of gets its own
/// sentence, because adding a key would not fix it.
fn neither(
    keys: &[mailrs_pgp::Recipient],
    certificates: &[mailrs_smime::Recipient],
    pgp: &str,
    smime: &str,
) -> String {
    let unreachable: Vec<&str> = keys
        .iter()
        .filter(|recipient| recipient.key.is_none())
        .filter(|recipient| {
            certificates
                .iter()
                .any(|other| other.address == recipient.address && other.certificate.is_none())
        })
        .map(|recipient| recipient.address.as_str())
        .collect();
    if !unreachable.is_empty() {
        return fill(
            &gettext("gpg holds no key and gpgsm no trusted certificate for {addresses}."),
            &[("addresses", &listed(&unreachable))],
        );
    }
    if keys.is_empty() || certificates.is_empty() {
        return pgp.to_string();
    }
    fill(
        &gettext("A message goes out under one standard or the other. {pgp} {smime}"),
        &[("pgp", pgp), ("smime", smime)],
    )
}

/// The two parts of the entity `raw` holds: the first whole, headers and
/// all, and the second's body on its own.
///
/// These are slices of the message rather than anything parsed and put
/// back together, because a signature covers the first part byte for byte.
/// The CRLF before a boundary belongs to the boundary, so it comes off
/// here, and a message whose lines end some other way never signed
/// anything a reader could check.
pub(crate) fn wrapper_parts(raw: &[u8]) -> Option<(&[u8], &[u8])> {
    let blank = find(raw, b"\r\n\r\n")?;
    let boundary = param(&unfolded(&raw[..blank], "content-type")?, "boundary")?;
    let open = format!("--{boundary}\r\n").into_bytes();
    let next = format!("\r\n--{boundary}").into_bytes();
    let body = &raw[blank + 4..];
    let first = &body[find(body, &open)? + open.len()..];
    let (first, after) = first.split_at(find(first, &next)?);
    let second = after.get(next.len()..)?.strip_prefix(b"\r\n")?;
    let second = &second[..find(second, &next)?];
    Some((first, &second[find(second, b"\r\n\r\n")? + 4..]))
}

/// What an engine found in one protected message, in words neither
/// standard owns. Each adapter fills one from what gpg or gpgsm said, and
/// [`read`] alone turns it into the card and the body the window shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// Whether the message arrived encrypted and the engine opened it.
    pub encrypted: bool,
    /// The signature over `part`. On an encrypted message only one that
    /// travelled inside the encryption counts, since anyone can wrap
    /// somebody else's ciphertext in a signature of their own.
    pub signature: Option<Signed>,
    /// What the engine checked or opened. The window draws this and
    /// nothing else, so a part the signature does not cover never appears
    /// under the signer's name.
    pub part: Part,
}

/// The bytes a [`Found`] is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Part {
    /// A MIME entity, headers and all: the part a `multipart/signed`
    /// covers, or what came out of the encryption.
    Entity(Vec<u8>),
    /// Text that sat inside inline armor, already decoded.
    Text(String),
}

/// One signature, as neither standard words it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signed {
    pub verdict: Verdict,
    pub signer: Signer,
    pub vouched: Vouched,
}

/// Whether the text is the text that was signed, and whether the key or
/// certificate behind it was in good standing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Good,
    /// The text and the signature disagree.
    Bad,
    /// The signature matches, and the key or certificate has run out.
    KeyExpired,
    /// The signature matches, and the key or certificate was revoked.
    KeyRevoked,
    /// The signature matches, and it carried a date that has passed.
    SignatureExpired,
    /// This computer holds nothing to check the signature against.
    NoKey,
    /// The engine could not check it and gave another reason.
    Unchecked,
}

/// Who signed, as the key or certificate names them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Signer {
    /// The name without the address, as "Ada Lovelace".
    pub name: Option<String>,
    /// The addresses the key or certificate names, in its own order.
    pub addresses: Vec<String>,
    /// The key id, for a signer the engine has no name for.
    pub key: Option<String>,
}

/// How far the person's own trust reaches the signer: their trust database
/// under OpenPGP, the chain to a root under S/MIME. It answers who signed,
/// which is a separate question from whether the text is what was signed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vouched {
    /// One of the person's own keys.
    Own,
    /// The person vouched for it, or its chain reaches a root they trust.
    Yes,
    /// People the person trusts vouched for it.
    Partly,
    /// Nobody has vouched for it, or its chain reaches no trusted root.
    Nobody,
    /// The engine said nothing about it.
    Unsaid,
    /// The person marked it as one not to trust.
    Never,
}

/// Why an engine would not answer about a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// It is encrypted to a key or certificate this computer lacks.
    NotForYou,
    /// Its parts are not where the standard keeps them.
    Unreadable,
    /// The engine failed, for the reason given, in words a person reads.
    Failed(String),
}

/// What the window shows for a protected message, from what its engine
/// found: the card, and a body cut from the bytes the engine checked or
/// opened, whatever kind of message it was. `arrived` is the body Gmail
/// read, which lends the new one only what came from the headers.
pub fn read(standard: Standard, found: Result<Found, Refusal>, arrived: &MessageBody) -> Read {
    let found = match found {
        Ok(found) => found,
        Err(refusal) => {
            return Read {
                mark: refused(standard, &refusal),
                body: None,
                files: Vec::new(),
            };
        }
    };
    let (mut body, files) = match &found.part {
        Part::Entity(entity) => opened_body(entity),
        Part::Text(text) => (
            MessageBody {
                text: Some(text.clone()),
                ..MessageBody::default()
            },
            Vec::new(),
        ),
    };
    // The headers were never inside the signature or the encryption, and
    // the details panel and the list's own buttons read them.
    body.list_unsubscribe = arrived.list_unsubscribe.clone();
    body.one_click_unsubscribe = arrived.one_click_unsubscribe;
    body.provenance = arrived.provenance.clone();
    body.protection = arrived.protection;
    let mark = match (&found.signature, found.encrypted) {
        (Some(signed), false) => signed_mark(standard, signed),
        (signature, true) => encrypted_mark(standard, signature.as_ref(), files.len()),
        // An engine that neither opened nor checked anything has nothing
        // to vouch for, which is what an unchecked signature says.
        (None, false) => signed_mark(
            standard,
            &Signed {
                verdict: Verdict::Unchecked,
                signer: Signer::default(),
                vouched: Vouched::Unsaid,
            },
        ),
    };
    Read {
        mark,
        body: Some(body),
        files,
    }
}

/// What the card says about a signature over a message that arrived in
/// the clear. The verdict and the vouching answer different questions, so
/// the title carries the one and the line under it the other.
fn signed_mark(standard: Standard, signed: &Signed) -> Mark {
    let who = signer_name(standard, &signed.signer);
    let signer = [("signer", who.as_str())];
    let pgp = standard == Standard::Pgp;
    match signed.verdict {
        Verdict::Good => Mark {
            title: fill(&gettext("Signed by {signer}"), &signer),
            detail: Some(vouching(standard, signed.vouched)),
            tone: match signed.vouched {
                Vouched::Own | Vouched::Yes | Vouched::Partly => Tone::Good,
                Vouched::Never => Tone::Bad,
                Vouched::Nobody | Vouched::Unsaid => match standard {
                    // Under OpenPGP the key is the name, so a key nobody
                    // has vouched for still signs. Under S/MIME the chain
                    // is the only thing that says who signed.
                    Standard::Pgp => Tone::Good,
                    Standard::Smime => Tone::Unchecked,
                },
            },
        },
        Verdict::Bad => Mark {
            title: gettext("This message changed after it was signed"),
            detail: Some(fill(
                &gettext("The signature of {signer} does not match what arrived."),
                &signer,
            )),
            tone: Tone::Bad,
        },
        Verdict::KeyExpired if pgp => Mark {
            title: fill(
                &gettext("Signed by {signer}, whose key has expired"),
                &signer,
            ),
            detail: Some(gettext(
                "The text is as it was written, and the key behind it ran out.",
            )),
            tone: Tone::Unchecked,
        },
        Verdict::KeyExpired => Mark {
            title: fill(
                &gettext("Signed by {signer}, whose certificate has run out"),
                &signer,
            ),
            detail: Some(gettext(
                "The text is as it was written, and the certificate behind it has expired.",
            )),
            tone: Tone::Unchecked,
        },
        Verdict::KeyRevoked if pgp => Mark {
            title: fill(
                &gettext("Signed by {signer}, who took this key back"),
                &signer,
            ),
            detail: Some(gettext(
                "The owner revoked it, so it says nothing about who wrote this.",
            )),
            tone: Tone::Bad,
        },
        Verdict::KeyRevoked => Mark {
            title: fill(
                &gettext("Signed by {signer}, whose certificate was taken back"),
                &signer,
            ),
            detail: Some(gettext(
                "Whoever issued it revoked it, so it says nothing about who wrote this.",
            )),
            tone: Tone::Bad,
        },
        Verdict::SignatureExpired => Mark {
            title: fill(
                &gettext("Signed by {signer}, and the signature has run out"),
                &signer,
            ),
            detail: Some(gettext(
                "It carried a date to stop being good on, and that date has passed.",
            )),
            tone: Tone::Unchecked,
        },
        Verdict::NoKey if pgp => Mark {
            title: gettext("Signed by a key this computer does not hold"),
            detail: Some(fill(
                &gettext("Nothing here can check it. Ask gpg for key {key}."),
                &[(
                    "key",
                    &signed
                        .signer
                        .key
                        .clone()
                        .unwrap_or_else(|| gettext("it names")),
                )],
            )),
            tone: Tone::Unchecked,
        },
        Verdict::NoKey => Mark {
            title: gettext("Signed by a certificate this computer does not hold"),
            detail: Some(gettext(
                "The message carried none either, so there is nothing here to check it \
                 against.",
            )),
            tone: Tone::Unchecked,
        },
        Verdict::Unchecked if pgp => Mark {
            title: gettext("gpg could not check this signature"),
            detail: None,
            tone: Tone::Unchecked,
        },
        Verdict::Unchecked => Mark {
            title: gettext("gpgsm could not check this signature"),
            detail: None,
            tone: Tone::Unchecked,
        },
    }
}

/// What the card says about a message that arrived encrypted, with the
/// signature that travelled inside it when it carried one.
fn encrypted_mark(standard: Standard, signed: Option<&Signed>, files: usize) -> Mark {
    let mut mark = match signed {
        Some(signed) => {
            let inside = signed_mark(standard, signed);
            match signed.verdict {
                // The one case the card says in a single sentence rather
                // than in two.
                Verdict::Good => Mark {
                    title: fill(
                        &gettext("Encrypted, and signed by {signer}"),
                        &[("signer", &signer_name(standard, &signed.signer))],
                    ),
                    ..inside
                },
                _ => Mark {
                    title: fill(&gettext("Encrypted. {what}"), &[("what", &inside.title)]),
                    ..inside
                },
            }
        }
        None => Mark {
            title: gettext("This message arrived encrypted"),
            detail: Some(gettext(
                "Nobody signed it, so it says nothing about who sent it.",
            )),
            tone: Tone::Unchecked,
        },
    };
    if let Some(line) = files_line(files) {
        mark.detail = Some(match mark.detail {
            Some(detail) => fill(
                &gettext("{detail} {files}"),
                &[("detail", &detail), ("files", &line)],
            ),
            None => line,
        });
    }
    mark
}

/// What the card says when an engine would not open a message.
fn refused(standard: Standard, refusal: &Refusal) -> Mark {
    let pgp = standard == Standard::Pgp;
    match refusal {
        Refusal::NotForYou if pgp => Mark {
            title: gettext("This message is encrypted to a key you do not hold"),
            detail: Some(gettext(
                "Whoever sent it used a key gpg has no secret half of.",
            )),
            tone: Tone::Unchecked,
        },
        Refusal::NotForYou => Mark {
            title: gettext("This message is encrypted to a certificate you do not hold"),
            detail: Some(gettext(
                "Whoever sent it used a certificate gpgsm has no secret key for.",
            )),
            tone: Tone::Unchecked,
        },
        Refusal::Unreadable => Mark {
            title: match standard {
                Standard::Pgp => gettext("This message says it is OpenPGP and is not"),
                Standard::Smime => gettext("This message says it is S/MIME and is not"),
            },
            detail: Some(gettext(
                "Its parts are not where a signed or encrypted message keeps them.",
            )),
            tone: Tone::Unchecked,
        },
        Refusal::Failed(reason) => Mark {
            title: match standard {
                Standard::Pgp => gettext("gpg could not open this message"),
                Standard::Smime => gettext("gpgsm could not open this message"),
            },
            detail: Some(reason.clone()),
            tone: Tone::Unchecked,
        },
    }
}

/// How far the person's trust reaches the signer, said plainly. A key or
/// certificate nobody vouched for still signs; the two are separate
/// answers and running them together tells people the wrong thing.
fn vouching(standard: Standard, vouched: Vouched) -> String {
    match (standard, vouched) {
        (_, Vouched::Never) => gettext("You marked this key as one not to trust."),
        (Standard::Pgp, Vouched::Own) => gettext("This is one of your own keys."),
        (Standard::Pgp, Vouched::Yes) => gettext("You have vouched for this key."),
        (Standard::Pgp, Vouched::Partly) => gettext("People you trust have vouched for this key."),
        (Standard::Pgp, Vouched::Nobody | Vouched::Unsaid) => {
            gettext("Nobody has vouched for this key, so it names no one.")
        }
        (Standard::Smime, Vouched::Own | Vouched::Yes | Vouched::Partly) => {
            gettext("Its certificate leads back to an authority you trust.")
        }
        (Standard::Smime, Vouched::Nobody) => {
            gettext("Its certificate leads back to nobody you trust, so it names no one.")
        }
        (Standard::Smime, Vouched::Unsaid) => {
            gettext("Nothing here says who that certificate belongs to.")
        }
    }
}

/// Who signed, as "Ada Lovelace <ada@example.test>" when the key or
/// certificate gives both, and as whatever it gives otherwise.
fn signer_name(standard: Standard, signer: &Signer) -> String {
    match (&signer.name, signer.addresses.first(), &signer.key) {
        (Some(name), Some(address), _) => format!("{name} <{address}>"),
        (Some(name), None, _) => name.clone(),
        (None, Some(address), _) => address.clone(),
        (None, None, Some(key)) if standard == Standard::Pgp => {
            fill(&gettext("key {key}"), &[("key", key)])
        }
        (None, None, _) => match standard {
            Standard::Pgp => gettext("a key gpg would not name"),
            Standard::Smime => gettext("a certificate gpgsm would not name"),
        },
    }
}

/// What the card says about the files inside. They came out of the
/// encryption and live in this window alone, so saving one writes the copy
/// that exists rather than fetching anything.
pub(crate) fn files_line(files: usize) -> Option<String> {
    match files {
        0 => None,
        1 => Some(gettext("It carries a file, kept in this window only.")),
        count => Some(fill_plural(
            "It carries {count} file, kept in this window only.",
            "It carries {count} files, kept in this window only.",
            count,
            &[("count", &count.to_string())],
        )),
    }
}

/// What was inside the encryption: the message to draw, and the bytes of
/// each file it carries, in the same order.
pub(crate) fn opened_body(part: &[u8]) -> (MessageBody, Vec<Vec<u8>>) {
    let Some(parsed) = MessageParser::default().parse(part) else {
        return (
            MessageBody {
                text: Some(String::from_utf8_lossy(part).into_owned()),
                ..MessageBody::default()
            },
            Vec::new(),
        );
    };
    let mut attachments = Vec::new();
    let mut files = Vec::new();
    for (index, found) in parsed.attachments().enumerate() {
        let mime_type = found
            .content_type()
            .map(|content| match content.subtype() {
                Some(subtype) => format!("{}/{subtype}", content.ctype()),
                None => content.ctype().to_string(),
            })
            .unwrap_or_else(|| "application/octet-stream".to_string())
            .to_ascii_lowercase();
        attachments.push(mailrs_domain::Attachment {
            // No part id and no attachment id: Gmail never saw this part,
            // so the window reads it out of `Read::files` by this index.
            part_id: index.to_string(),
            filename: found.attachment_name().unwrap_or("attachment").to_string(),
            mime_type,
            size: found.len() as i64,
            attachment_id: None,
            content_id: found.content_id().map(str::to_string),
        });
        files.push(found.contents().to_vec());
    }
    // An invitation that arrived signed or encrypted carries its event in
    // here, and the card reads it from the body the way it does for mail
    // in the clear.
    let calendar = parsed
        .parts
        .iter()
        .find(|part| {
            part.content_type().is_some_and(|content| {
                content.ctype().eq_ignore_ascii_case("text")
                    && content
                        .subtype()
                        .is_some_and(|subtype| subtype.eq_ignore_ascii_case("calendar"))
            })
        })
        .and_then(|part| part.text_contents())
        .filter(|ics| ics.contains("BEGIN:VCALENDAR"))
        .map(str::to_string);
    (
        MessageBody {
            // mail_parser makes HTML out of a plain text part when there is
            // no HTML one, and the window would draw that in place of the
            // text, so only a part that arrived as HTML counts.
            html: parsed
                .html_part(0)
                .filter(|part| part.is_text_html())
                .and_then(|_| parsed.body_html(0))
                .map(|html| html.into_owned()),
            text: parsed.body_text(0).map(|text| text.into_owned()),
            attachments,
            calendar,
            ..MessageBody::default()
        },
        files,
    )
}

/// "ann@example.com", "ann@example.com or bo@example.com", and with more
/// than two, commas until the last.
pub(crate) fn listed(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => (*one).to_string(),
        [rest @ .., last] => fill(
            &gettext("{names} or {last}"),
            &[("names", &rest.join(", ")), ("last", last)],
        ),
    }
}

/// The same list, for a sentence that wants "and" between the last two.
pub(crate) fn joined(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => (*one).to_string(),
        [rest @ .., last] => fill(
            &gettext("{names} and {last}"),
            &[("names", &rest.join(", ")), ("last", last)],
        ),
    }
}

/// The version out of `gpg --version`, which leads with `gpg (GnuPG) 2.4.8`.
pub(crate) fn version_of(output: &str) -> Option<String> {
    let line = output.lines().next()?.trim();
    let version = line.rsplit(' ').next()?;
    version
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_digit())
        .then(|| version.to_string())
}

/// The value of one header, the lines it folds onto joined back on. Header
/// values are only read here, so bytes that are not UTF-8 lose nothing.
pub(crate) fn unfolded(headers: &[u8], name: &str) -> Option<String> {
    let text = String::from_utf8_lossy(headers)
        .replace("\r\n ", " ")
        .replace("\r\n\t", " ");
    text.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().to_string())
    })
}

/// One parameter of a header value, without its quotes.
pub(crate) fn param(value: &str, name: &str) -> Option<String> {
    value.split(';').skip(1).find_map(|parameter| {
        let (key, value) = parameter.split_once('=')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().trim_matches('"').to_string())
    })
}

pub(crate) fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Signed messages somebody added a part to, for the adapters' tests.
#[cfg(test)]
pub(crate) mod tampered {
    use mailrs_domain::MessageBody;

    /// A signed entity with one more part after the signature, which is
    /// what somebody who wants their words under another's name sends.
    pub fn with_unsigned_part(entity: &[u8]) -> Vec<u8> {
        let entity = String::from_utf8_lossy(entity).into_owned();
        let boundary = entity
            .split("boundary=\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("a boundary")
            .to_string();
        let closing = format!("--{boundary}--\r\n");
        let extra = format!(
            "--{boundary}\r\nContent-Type: text/html\r\n\r\n<p>Pay Mallory.</p>\r\n\
             --{boundary}\r\nContent-Type: application/pdf\r\n\
             Content-Disposition: attachment; filename=\"invoice.pdf\"\r\n\r\n%PDF\r\n\
             {closing}"
        );
        entity.replace(&closing, &extra).into_bytes()
    }

    /// What Gmail made of the same message before the engine ran, had it
    /// read every part.
    pub fn as_gmail_read_it() -> MessageBody {
        MessageBody {
            html: Some("<p>Pay Mallory.</p>".into()),
            attachments: vec![mailrs_domain::Attachment {
                part_id: "3".into(),
                filename: "invoice.pdf".into(),
                mime_type: "application/pdf".into(),
                size: 4,
                attachment_id: Some("att-2".into()),
                content_id: None,
            }],
            text: Some("Meet at six.".into()),
            ..MessageBody::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use mailrs_smime::Certificate;

    use super::*;

    fn certificate(address: &str, held: bool) -> mailrs_smime::Recipient {
        mailrs_smime::Recipient {
            address: address.to_string(),
            certificate: held.then(|| Certificate {
                fingerprint: "F".repeat(40),
                subject: format!("CN={address}"),
                email: address.to_string(),
            }),
        }
    }

    fn key(address: &str, held: bool) -> mailrs_pgp::Recipient {
        mailrs_pgp::Recipient {
            address: address.to_string(),
            key: held.then(|| mailrs_pgp::Key {
                fingerprint: "F".repeat(40),
                user_id: format!("<{address}>"),
                trust: mailrs_pgp::Trust::Unknown,
            }),
        }
    }

    fn arrived(protection: Protection) -> MessageBody {
        MessageBody {
            protection: Some(protection),
            ..MessageBody::default()
        }
    }

    #[test]
    fn the_wrapper_decides_which_engine_a_message_needs() {
        assert_eq!(
            engine(&arrived(Protection::SmimeSigned)),
            Some(Engine::Smime(smime::Opening::Verify))
        );
        assert_eq!(
            engine(&arrived(Protection::SmimeOpaque)),
            Some(Engine::Smime(smime::Opening::Opaque))
        );
        assert_eq!(
            engine(&arrived(Protection::SmimeEnveloped)),
            Some(Engine::Smime(smime::Opening::Decrypt))
        );
        assert_eq!(
            engine(&arrived(Protection::Signed)),
            Some(Engine::Pgp(pgp::Opening::Verify))
        );
        assert_eq!(
            engine(&arrived(Protection::Encrypted)),
            Some(Engine::Pgp(pgp::Opening::Decrypt))
        );
        assert_eq!(engine(&MessageBody::default()), None);
    }

    #[test]
    fn openpgp_carries_the_message_when_both_standards_could() {
        let held = Held {
            pgp: Some(vec![key("ada@example.test", true)]),
            smime: Some(vec![certificate("ada@example.test", true)]),
        };
        assert_eq!(encrypting(&held, false), Ok(Standard::Pgp));
    }

    #[test]
    fn smime_carries_it_when_it_is_the_one_that_reaches_everybody() {
        let held = Held {
            pgp: Some(vec![key("ada@example.test", false)]),
            smime: Some(vec![certificate("ada@example.test", true)]),
        };
        assert_eq!(encrypting(&held, false), Ok(Standard::Smime));
    }

    #[test]
    fn a_recipient_neither_standard_reaches_is_named_once() {
        let held = Held {
            pgp: Some(vec![
                key("ada@example.test", true),
                key("bo@example.test", false),
            ]),
            smime: Some(vec![
                certificate("ada@example.test", false),
                certificate("bo@example.test", false),
            ]),
        };
        assert_eq!(
            encrypting(&held, false),
            Err("gpg holds no key and gpgsm no trusted certificate for bo@example.test.".into())
        );
    }

    #[test]
    fn a_draft_each_standard_covers_half_of_says_what_that_means() {
        let held = Held {
            pgp: Some(vec![
                key("ada@example.test", true),
                key("bo@example.test", false),
            ]),
            smime: Some(vec![
                certificate("ada@example.test", false),
                certificate("bo@example.test", true),
            ]),
        };
        let problem = encrypting(&held, false).expect_err("neither reaches both");
        assert!(
            problem.starts_with("A message goes out under one standard or the other."),
            "{problem}"
        );
        assert!(problem.contains("bo@example.test"), "{problem}");
    }

    #[test]
    fn the_only_engine_on_this_computer_is_the_one_that_answers() {
        let smime_alone = Held {
            pgp: None,
            smime: Some(vec![certificate("bo@example.test", false)]),
        };
        assert_eq!(
            encrypting(&smime_alone, false),
            Err(
                "gpgsm holds no trusted certificate for bo@example.test. A certificate that only \
             arrived in mail does not count."
                    .into()
            )
        );
        let pgp_alone = Held {
            pgp: Some(vec![key("bo@example.test", false)]),
            smime: None,
        };
        assert_eq!(
            encrypting(&pgp_alone, false),
            Err("gpg holds no key for bo@example.test.".into())
        );
    }

    #[test]
    fn a_blind_copy_goes_out_under_openpgp_when_it_can() {
        let held = Held {
            pgp: Some(vec![key("ada@example.test", true)]),
            smime: Some(vec![certificate("ada@example.test", true)]),
        };
        assert_eq!(encrypting(&held, true), Ok(Standard::Pgp));
    }

    #[test]
    fn a_blind_copy_keeps_smime_from_carrying_the_message() {
        let smime_only = Held {
            pgp: Some(vec![key("ada@example.test", false)]),
            smime: Some(vec![certificate("ada@example.test", true)]),
        };
        let problem = encrypting(&smime_only, true).expect_err("S/MIME would name the Bcc");
        assert!(
            problem.starts_with("S/MIME names every recipient"),
            "{problem}"
        );
        assert!(
            problem.ends_with("gpg holds no key for ada@example.test."),
            "and says what OpenPGP lacks: {problem}"
        );

        let no_gpg = Held {
            pgp: None,
            smime: Some(vec![certificate("ada@example.test", true)]),
        };
        assert_eq!(encrypting(&no_gpg, true), Err(smime_names_everyone()));
    }

    fn address(email: &str) -> mailrs_domain::Address {
        mailrs_domain::Address {
            name: None,
            email: email.to_string(),
        }
    }

    fn draft() -> Draft {
        let mut draft = Draft::new(1, address("ada@example.test"));
        draft.to = vec![address("bo@example.test")];
        draft.cc = vec![address("cy@example.test"), address("BO@example.test")];
        draft.bcc = vec![address("di@example.test"), address("cy@example.test")];
        draft
    }

    #[test]
    fn a_blind_copy_is_hidden_and_the_sender_named_when_they_hold_a_key() {
        let addressees = Addressees::of(&draft());
        assert!(addressees.has_blind_copy());
        assert_eq!(
            addressees.readers(true),
            mailrs_pgp::Readers {
                // Cy is in Cc as well as Bcc, so the others see Cy anyway.
                named: vec![
                    "bo@example.test".into(),
                    "cy@example.test".into(),
                    "ada@example.test".into()
                ],
                hidden: vec!["di@example.test".into()],
            }
        );
        // Without a key of the sender's own, the sender is nobody gpg can
        // encrypt to.
        assert_eq!(
            addressees.readers(false).named,
            vec!["bo@example.test".to_string(), "cy@example.test".into()]
        );
    }

    #[test]
    fn a_sender_in_their_own_blind_copy_is_named_rather_than_hidden() {
        let mut draft = draft();
        draft.bcc = vec![address("Ada@example.test")];
        let addressees = Addressees::of(&draft);
        assert!(
            !addressees.has_blind_copy(),
            "the From line names the sender anyway"
        );
        let readers = addressees.readers(true);
        assert!(readers.hidden.is_empty(), "{readers:?}");
        assert!(readers.named.contains(&"ada@example.test".to_string()));
    }

    #[test]
    fn the_sender_decides_which_standard_signs() {
        let both = Held {
            pgp: Some(vec![key("ada@example.test", true)]),
            smime: Some(vec![certificate("ada@example.test", true)]),
        };
        assert_eq!(signing(&both), Standard::Pgp);

        let certificate_only = Held {
            pgp: Some(vec![key("ada@example.test", false)]),
            smime: Some(vec![certificate("ada@example.test", true)]),
        };
        assert_eq!(signing(&certificate_only), Standard::Smime);

        assert_eq!(signing(&Held::default()), Standard::Pgp);
    }

    #[test]
    fn the_signed_part_comes_out_as_it_arrived() {
        let raw = b"From: ada@example.test\r\n\
                    Content-Type: multipart/signed; micalg=pgp-sha256;\r\n \
                    protocol=\"application/pgp-signature\"; boundary=\"edge\"\r\n\
                    \r\n\
                    --edge\r\n\
                    Content-Type: text/plain\r\n\
                    \r\n\
                    Meet at six.\r\n\
                    --edge\r\n\
                    Content-Type: application/pgp-signature\r\n\
                    \r\n\
                    -----BEGIN PGP SIGNATURE-----\r\n\
                    -----END PGP SIGNATURE-----\r\n\
                    --edge--\r\n";
        let (part, signature) = wrapper_parts(raw).expect("two parts");
        assert_eq!(part, b"Content-Type: text/plain\r\n\r\nMeet at six.");
        assert_eq!(
            signature,
            b"-----BEGIN PGP SIGNATURE-----\r\n-----END PGP SIGNATURE-----"
        );
    }

    #[test]
    fn a_message_missing_its_boundary_gives_back_nothing() {
        let raw = b"Content-Type: multipart/signed\r\n\r\nMeet at six.\r\n";
        assert!(wrapper_parts(raw).is_none());
    }

    #[test]
    fn a_file_inside_the_encryption_comes_out_with_its_bytes_and_its_type() {
        let part = b"Content-Type: multipart/mixed; boundary=\"b\"\r\n\r\n\
            --b\r\nContent-Type: text/plain\r\n\r\nHere it is.\r\n\
            --b\r\nContent-Type: image/png; name=\"cat.png\"\r\n\
            Content-Disposition: attachment; filename=\"cat.png\"\r\n\
            Content-Transfer-Encoding: base64\r\n\r\niVBORwECAwQ=\r\n--b--\r\n";
        let (body, files) = opened_body(part);
        assert_eq!(body.attachments.len(), 1);
        let found = &body.attachments[0];
        assert_eq!(found.filename, "cat.png");
        assert_eq!(
            found.mime_type, "image/png",
            "the row needs it for a picture"
        );
        assert!(
            found.attachment_id.is_none(),
            "Gmail never saw this part, so there is nothing to fetch"
        );
        // The bytes line up with the attachment list, which is how the
        // window finds them when somebody asks to save or open one.
        assert_eq!(files.len(), 1);
        assert_eq!(files[0], vec![0x89, 0x50, 0x4e, 0x47, 1, 2, 3, 4]);
    }

    #[test]
    fn the_version_is_the_last_word_of_the_first_line() {
        assert_eq!(
            version_of("gpg (GnuPG) 2.4.8\nlibgcrypt 1.12.0\n").as_deref(),
            Some("2.4.8")
        );
        assert_eq!(version_of("").as_deref(), None);
        assert_eq!(version_of("gpg: no such option").as_deref(), None);
    }
}
