# mailrs-pgp

OpenPGP mail through the person's own GnuPG. Every call here runs the `gpg`
binary, so the keys, the agent, the pinentry and the trust database are the
ones the person already has. This crate holds no key and asks for no
passphrase.

Nothing in it draws anything. It reads parts and writes parts; what a window
shows about a signature is the caller's to decide.

## Starting up

Call `Pgp::find()` once. It looks for `gpg` on PATH, then `gpg2`, and answers
`PgpError::NoGpg` when it finds neither. That answer is the moment to leave
the OpenPGP controls out of the window, rather than offering a Sign button
that fails on send.

Every call blocks. gpg may put a pinentry in front of the person, who takes
as long as they take, so these run on a thread of their own, like the store's
calls do, and never on the GTK thread.

## Reading a message that arrived

Look at the top-level part and take the first branch that matches.

**`multipart/signed`, protocol `application/pgp-signature`.** Take the first
part's bytes exactly as they arrived: everything between `--boundary` CRLF
and the CRLF before the next boundary, headers included, with no parsing and
rebuilding in between. Take the second part's body, the armor on its own.
Then:

```rust
let found = pgp.verify(&signed_part, &signature)?;
```

`found.verdict` says what happened and `found.trust` says how far the signing
key's owner is vouched for. They are separate questions and a window that
runs them together misleads people: `Verdict::NoKey` means nothing could be
checked, which is not `Verdict::Bad`, and `Verdict::Good` with
`Trust::Unknown` means the text is as the signer wrote it and nobody has said
who the signer is. `found.is_good()` is the only case that means the message
came from that key's owner unchanged. Draw the first part as the message.

**`multipart/encrypted`, protocol `application/pgp-encrypted`.** The first
part carries `Version: 1` and nothing worth reading. Pass the second part's
body:

```rust
let opened = pgp.decrypt(&ciphertext)?;
```

`opened.part` is a MIME entity in its own right: parse it and draw it as the
message. `opened.signature` is the signature that travelled inside the
encryption, which is the only kind worth showing on an encrypted message,
since anyone can wrap somebody else's ciphertext in a signature of their own.

**A `text/plain` body with armor in it.** Plenty of mail still puts PGP in the
body. Ask before drawing:

```rust
if inline::armor(&body).is_some() {
    let opened = pgp.open_inline(&body)?;
}
```

`opened.text` is the bytes that were inside, in whatever character set the
sender used, so decode them the way any other body is decoded. It covers both
an encrypted message and text left readable with a signature under it;
`opened.signature` comes back for either.

`PgpError::NotForYou` means the message was encrypted to nobody this computer
holds a key for. Say so where the message would have been, rather than
showing an empty body.

## Before sending

Ask first, while the person is still typing:

```rust
let held = pgp.keys_for(&addresses)?;
let can_encrypt = held.iter().all(|recipient| recipient.key.is_some());
```

This reads the local keyring only, so it is quick enough to ask again each
time a recipient changes. Offer encryption when every recipient has a key,
and name the ones that do not when they ask why it is off. The `trust` on a
key belongs in a warning beside the Send button, not in this decision.

Then build the message body as one MIME entity, headers and a blank line and
a body, without the message's own `From`, `To` or `Subject`. Hand that over:

```rust
let body = pgp.sign(&part, "ada@example.com")?;
let readers = Readers {
    named: vec!["bo@example.com".into(), "ada@example.com".into()],
    hidden: vec!["cy@example.com".into()],
};
let body = pgp.encrypt(&part, &readers, Some("ada@example.com"))?;
```

`named` is To, Cc and the sender, whose own copy in Sent stays readable only
if they are on the list. `hidden` is Bcc. gpg writes a key id for each named
reader into the message, where anyone who receives it can list them with
`gpg --list-packets`. A hidden reader gets a key id of zero, so the others
learn that someone else can open the message and not who; the hidden reader's
gpg tries each of its secret keys until one fits.

Both give back a whole entity: a `Content-Type` header naming the boundary, a
blank line, then the parts. Put that header on the message being sent and use
the rest as its body.

After that, leave the bytes alone. Re-wrapping a line, re-encoding a part or
adding a header inside the signed part breaks the signature, and the person
who gets the message sees a warning rather than a message.

## What this crate decides on its own

Signing a part that no server can spoil is the whole job, so it makes a few
calls without asking:

- The part is put in canonical form first: CRLF everywhere, and any body with
  whitespace at the end of a line or a byte above ASCII is re-encoded
  quoted-printable, with the header changed to match. A server that strips
  trailing whitespace then finds none to strip. A part that is itself
  multipart is walked, so each part inside is protected in its own right.
- Blank lines at the very end of the signed part come off, so there is one
  reading of where the signed bytes stop.
- `encrypt` runs gpg with `--trust-model always`. gpg otherwise refuses in
  batch mode to encrypt to a key its owner never signed, which is most keys
  anybody holds, and a batch run cannot ask. `keys_for` reports the trust so
  the caller can put it in front of the person; refusing to send is the wrong
  place to raise it.
- `encrypt` adds nobody on its own. Signing as the sender does not make the
  sender a reader; the caller names them in `Readers`.
- The `micalg` parameter names the digest gpg reported signing with. A digest
  this crate has no name for leaves the parameter out rather than putting the
  wrong one in.

## Tests

`cargo test -p mailrs-pgp`. The round trips build a GnuPG home under a temp
directory, generate an ed25519 key in it and run real signatures through it.
They touch no keyring of the person running them, and they say so and stop
when this computer has no gpg.
