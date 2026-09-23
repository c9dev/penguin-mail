# mailrs-smime

S/MIME mail through the person's own GnuPG. Every call here runs the
`gpgsm` binary, which ships with GnuPG beside `gpg`, so the certificates,
the agent, the pinentry and the list of roots to trust are the ones the
person already has. This crate holds no key and asks for no passphrase.

Nothing in it draws anything. It reads parts and writes parts; what a
window shows about a signature is the caller's to decide.

It is the OpenPGP crate's twin, and the two answer the same questions in
the same order. Where they differ, S/MIME is the reason: CMS travels as
base64 rather than as armor, a signer is named by the subject of a
certificate rather than by a user id, and the certificate's chain is what
says who that signer is.

## Starting up

Call `Smime::find()` once. It looks for `gpgsm` on PATH and answers
`SmimeError::NoGpgsm` when it finds none. That answer is the moment to
leave the S/MIME controls out of the window, rather than offering a Sign
button that fails on send.

Every call blocks. gpgsm may put a pinentry in front of the person, who
takes as long as they take, so these run on a thread of their own, like the
store's calls do, and never on the GTK thread.

## Reading a message that arrived

Look at the top-level part and take the first branch that matches. Each
one takes the body of a part as it arrived, base64 and all: these calls
decode it themselves rather than making every caller do it the same way.

**`multipart/signed`, protocol `application/pkcs7-signature`.** Take the
first part's bytes exactly as they arrived: everything between `--boundary`
CRLF and the CRLF before the next boundary, headers included, with no
parsing and rebuilding in between. Take the second part's body. Then:

```rust
let found = smime.verify(&signed_part, &signature)?;
```

`found.verdict` says whether the text is the text that was signed, and
`found.chain` says whether the certificate behind it reaches a root this
computer trusts. They are separate questions and a window that runs them
together misleads people: `Verdict::NoCertificate` means nothing could be
checked, which is not `Verdict::Bad`, and `Verdict::Good` with
`Chain::Untrusted` means the text is as the signer wrote it and nothing
here vouches for the name on the certificate. `found.subject` and
`found.email` are who that certificate says the signer is. Draw the first
part as the message.

gpgsm asks dirmngr whether each certificate below the root was revoked,
and dirmngr asks the certificate authority's server. `verify` gives that
ten seconds (`with_revocation_wait` changes it), then kills the run and
checks again with `--disable-crl-checks`. When the chain holds without the
revocation check and failed or ran out of time with it,
`Chain::RevocationUnknown` says so: the signature is checked, the chain
reaches a trusted root, and nobody could say whether the certificate was
revoked. Show that as unchecked, not as good. A certificate the CRL lists
comes back as `Verdict::RevokedCertificate`.

**`application/pkcs7-mime`, `smime-type=signed-data`.** The same signature,
with the message inside the blob rather than beside it. Outlook sends this
unless somebody told it not to, and a client that does not know the shape
shows people an attachment called `smime.p7s`.

```rust
let opened = smime.open_signed(&blob)?;
```

`opened.part` is the MIME entity that was inside: parse it and draw it as
the message. `opened.signature` is what gpgsm made of the signature over
it.

**`application/pkcs7-mime`, `smime-type=enveloped-data`.**

```rust
let part = smime.decrypt(&blob)?;
```

What comes back is a MIME entity in its own right. gpgsm opens one wrapper
at a time, so a message that was signed before it was enveloped gives back
a signed entity here, and checking that signature means going round again
with what is inside.

`SmimeError::NotForYou` means the message was enveloped to nobody this
computer holds a secret key for. Say so where the message would have been,
rather than showing an empty body.

## Before sending

Ask first, while the person is still typing:

```rust
let held = smime.certificates_for(&addresses)?;
let can_encrypt = held.iter().all(|recipient| recipient.certificate.is_some());
```

This reads the local keybox only, so it is quick enough to ask again each
time a recipient changes. Offer encryption when every recipient has a
certificate, and name the ones that do not when they ask why it is off.
`signing_certificates` answers the other half: which of the addresses this
person sends from gpgsm holds a secret key for, which is what signing
needs.

Then build the message body as one MIME entity, headers and a blank line
and a body, without the message's own `From`, `To` or `Subject`. Hand that
over:

```rust
let body = smime.sign(&part, "ada@example.com")?;
let body = smime.encrypt(&part, &to, Some("ada@example.com"))?;
```

`to` is everyone who can open the message, the sender included when they
want to read their own copy in Sent. CMS names every one of them inside
the envelope by issuer and serial number, and has no way to leave one out,
so a blind copy cannot stay blind under S/MIME. A caller with a Bcc to
encrypt uses OpenPGP, whose hidden recipients can.

Both give back a whole entity: the headers that describe it, a blank line,
then the body. Put those headers on the message being sent and use the rest
as its body.

After that, leave the bytes alone. Re-wrapping a line, re-encoding a part
or adding a header inside the signed part breaks the signature, and the
person who gets the message sees a warning rather than a message.

## What this crate decides on its own

- The part is put in canonical form first, by
  `mailrs_pgp::mime::canonical`. Getting a part into the shape that can be
  signed is the same job under both standards, and one copy of it cannot
  drift from the other.
- Blank lines at the very end of the signed part come off, so there is one
  reading of where the signed bytes stop.
- A signature goes out detached, in a `multipart/signed`, rather than
  inside a blob. Mail signed that way still reads as mail in a client that
  knows nothing about S/MIME.
- `encrypt` signs first and envelopes the signed entity, which is what RFC
  8551 describes and what gpgsm allows: it signs or encrypts in one run,
  never both.
- Only a certificate whose chain reaches a root in the person's trust
  list takes a message. gpgsm stores every certificate a signature carries
  when it checks that signature, so a stranger who signs one message with a
  certificate naming `bob@company.test` would otherwise get the next
  message meant for Bob. `certificates_for` lists with `--with-validation`
  and counts only `u` and `f`, and `encrypt` names each recipient by the
  fingerprint of that certificate and runs without `--always-trust`, so
  gpgsm checks the chain again, revocation included.
- `encrypt` adds nobody on its own. Signing as the sender does not make
  the sender a recipient; the caller puts them in `to`.
- The `micalg` parameter names the digest gpgsm reported signing with,
  under the name RFC 8551 gives it. A digest this crate has no name for
  leaves the parameter out rather than putting the wrong one in.

## Tests

`cargo test -p mailrs-smime`. The round trips build a GnuPG home under a
temp directory, generate a key and a self-signed certificate in it, mark
that certificate as a root to trust, and run real signatures through it.
They touch no keybox of the person running them, and they say so and stop
when this computer has no gpgsm.

`tests/revocation.rs` makes a certificate authority and a certificate it
issued, whose CRL distribution point is a port the test holds: silent,
refusing, or serving a CRL that openssl signs with the authority's key.
Those tests start a dirmngr for their home and stop it when they finish.
Without openssl, the CRL tests skip, or fail under
`PENGUIN_MAIL_REQUIRE_CRYPTO`.
