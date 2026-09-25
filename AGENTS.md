# Penguin Mail

A Gmail client for GNOME in Rust: GTK4, libadwaita, WebKitGTK 6, a tray
icon, several accounts synced into SQLite. Targets Ubuntu 26.04 and Rust
1.98. The crate map is in `CONTRIBUTING.md` under "How it is built"; the words
the code uses are in `CONTEXT.md`. Read both before changing behaviour.

## Where to look

- GitHub Issues: the public backlog. Start here when asked what is left
  or what to do next.
- `CONTEXT.md`: the glossary. A new domain term goes in here in the same
  change that introduces it.
- `docs/accessibility.md`, `docs/assistant.md`, `docs/setup.md`,
  `po/README.md`: read the one matching the area you touch.
- On the owner's machine only, and gitignored: `docs/remaining-work.md`,
  their own backlog, which you update when you close or find an item;
  `docs/superpowers/`, the design specs and build plans, history rather
  than instructions; and `docs/agents/`, notes for their agent skills. A
  contributor's checkout has none of these. Never commit them.

## The gate

A change is done when all four pass on the tree you are about to commit,
run after your last edit:

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
scripts/update-po.sh --check      # stale? run scripts/update-po.sh, commit the result
scripts/a11y-names.sh             # only when you touched the UI; exits 1 on an unnamed control
```

Any edit to a file holding translatable strings moves line numbers in
`po/penguin-mail.pot`, so `--check` goes stale from edits that change no
words. Run it last.

Read exit statuses from the command itself. `cargo test | tail` reports
`tail`'s status; use `${pipestatus[1]}` in zsh, or send the output to a
file and check `$?`. Never pipe `scripts/a11y-names.sh`: its Xvfb child
holds the pipe open and the command hangs.

Installing for the owner: `NO_AUTOSTART=1 scripts/install.sh`.

## Testing traps

- **One GTK test per test binary.** GTK belongs to the thread that
  starts it and the harness gives each test its own thread, so a second
  test calling `gtk::init()` crashes the binary with SIGSEGV. The one
  that exists is in `app/src/ui/composer/richbuffer.rs`; fold new GTK
  checks into it, or test the logic without widgets.
- **GnuPG tests** build a throwaway keyring and skip when `gpg` or
  `gpgsm` is missing. `PENGUIN_MAIL_REQUIRE_CRYPTO=1` turns the skip into
  a failure. Fixtures write `pinentry-program /bin/false` into
  `gpg-agent.conf`; keep that in any new fixture, or each run puts a
  trust dialog on the owner's screen. In product code, every `gpg` and
  `gpgsm` run goes through `mailrs_pgp::gnupg::Program::run`, which takes
  `Pinentry::Never` (`--pinentry-mode error`) or `Pinentry::MayAsk`; only
  decrypting and signing may ask for a passphrase.
- **Sandbox tests** for skill scripts run real `bwrap` and skip when it
  is missing or cannot start, as in an unprivileged container.
  `PENGUIN_MAIL_REQUIRE_SANDBOX=1` turns the skip into a failure.
- **Docker tests** (`testmail/`, `imap/tests/dovecot*.rs`,
  `sync/tests/dovecot.rs`) start Dovecot and Mailpit and skip when Docker
  is missing or cannot start. `PENGUIN_MAIL_REQUIRE_IMAP=1` turns the skip
  into a failure; CI's `imap` job sets it and the gate does not. The
  files under `imap/tests/` and `sync/tests/` hold one test each, because
  that test points `SSL_CERT_FILE` at a root made for the run: add a step
  to it rather than a second test. The sync suite spawns its body on a
  runtime built with `mailrs_sync::WORKER_STACK`, as the app does, so a
  stack too small for a debug build fails there. Containers
  go by id when a test ends; one left by a killed run carries the label
  `io.github.c9dev.penguin-mail.test`. Remove it by its id.
- **Migrations** live in one ordered array in `store/src/schema.rs`,
  numbered by position and tracked with `PRAGMA user_version`. Append
  only. Two branches that each add one collide on the number: renumber
  the later one on merge. Tests that hand-build an old schema must
  contain every table a later migration touches.

## Seeing the UI

`cargo run -p mailrs -- --demo` opens three sample accounts in a
throwaway store; nothing talks to Google. With no display, run it under
`Xvfb` and `dbus-run-session`; `scripts/a11y-names.sh` shows the full
recipe, including waiting for the window to reach the accessibility bus.
Take screenshots of the hidden display with `scripts/demo-shot.sh out.png`
(`--run drive.py` clicks or types first). It takes down the accessibility
registry it starts; a hand-made recipe leaves one running for every shot.
Render SVGs with `rsvg-convert`: ImageMagick mangles gradients and makes
a good icon look broken.

## How the code is shaped

- **Mail actions live in `sync`**, not in windows. Archive, flag,
  label, list a mailbox, change an automatic reply: one module each in
  `sync`, called by both the window and the assistant. A window that
  starts doing its own Gmail work is drifting.
- **Late answers.** Any `await` in the UI can finish after the reader
  has moved to another conversation. A run that talks to the window
  holds a `Wanted` (`app/src/wanted.rs`) for the `Target` it started on
  and reaches its effect port only through it: `wait` and `ask` drop an
  answer once that target has left the screen, and `on_screen` makes a
  change only while it is there. The thread run
  (`app/src/open_thread/run.rs`) and the engine run
  (`app/src/protection/run.rs`) work this way, each with a fake window
  and tests. New work on the open thread belongs in the thread run as a
  step; decide what it leaves stale in `Stale::after`. Window code that
  awaits outside a run, such as a dialog, keeps the target it started
  from and checks `ConversationView::is_showing(&target)` before
  touching the view.
- **The open thread changes through named methods** on
  `ConversationView` (`bodies_arrived`, `translated`, `engine_answered`,
  and so on), and is read through `read` and `find`. Add a named change
  rather than reaching into `OpenThread`, and put its data half on
  `OpenThread` (`take_bodies`, `take_engine_answer`), so the thread
  run's fake changes the thread the way the view does.
- **Architecture vocabulary** is the `codebase-design` skill's: module,
  interface, depth, seam, adapter, leverage, locality. A seam gets
  introduced when a second adapter exists, not before.

## Words a person reads

- Every user-facing string goes through `mailrs_domain::translate`
  (`gettext`, `ngettext`, `fill`, `fill_plural`), placeholders named
  like `{reason}`. A new file with such strings goes into
  `po/POTFILES.in`; `update-po.sh` warns when the list drifts.
- The owner reads the UI in English and receives mail in European
  Portuguese. `po/pt_PT.po` is kept complete: translate new strings into
  it in the same change.
- Write source strings in American English (color, organize, canceled).
  British English is `po/en_GB.po`, generated by `scripts/en-gb.py` when
  `scripts/update-po.sh` runs; never edit it by hand, change the rules.
- Prose in the UI, comments, docs, and commit messages follows the
  stop-slop and unslop rules: plain statements, no em dashes, no
  adverbs propping up verbs. Comments explain why, in full sentences,
  matching the density of the code around them.

## The changelog

A change someone using the app would notice gets a line under
`## Unreleased` in `CHANGELOG.md`, in the same commit, under `### New`,
`### Improved` or `### Fixed`. Write it for that person: what they can now
do or what stopped going wrong, in plain words, one line. "Search finds
mail in every account again", not "Pass the account filter through to the
listing". Refactors, tests, CI and docs get no line. `scripts/release.sh`
turns the section into the release notes.

## Commits

Subject: one plain sentence saying what changed for a person or for the
code, capitalised, no prefix, no full stop ("Let a recipient field take
the cursor back", "Put the engine run behind a desk and an effect
port"). Body: why, and anything a reviewer would otherwise have to
rediscover. Commit on `main` in small steps; push when asked.

Parallel agents work in worktrees under `.claude/worktrees/`
(gitignored). After merging one, remove its worktree and delete both
its branches; `git log main..<branch>` must be empty before a delete.

## Working with the owner

The standing instruction is to decide on taste and keep going without
stopping to ask: build a complete, good-looking client. Ask only for
decisions that are theirs, such as the licence or anything outward
facing. Their desktop uses the Gruvbox Plus Dark icon pack; the app icon
sits on that pack's squircle with its bevel, taupe behind a cream
penguin and an orange beak. Keep new icons in that idiom.

## Agent skills

### Issue tracker

Issues live in GitHub Issues for c9dev/penguin-mail, through the `gh` CLI. Anyone can open one, through the forms in `.github/ISSUE_TEMPLATE/`.

### Triage labels

The five triage labels: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`. The issue forms add `needs-triage`.

### Domain docs

Single-context: one `CONTEXT.md` and `docs/adr/` at the repo root.
