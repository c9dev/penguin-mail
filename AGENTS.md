# Penguin Mail

A Gmail client for GNOME in Rust: GTK4, libadwaita, WebKitGTK 6, a tray
icon, several accounts synced into SQLite. Targets Ubuntu 26.04 and Rust
1.98. The crate map is in `README.md` under "How it is built"; the words
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
  trust dialog on the owner's screen. In product code, every read-only
  `gpgsm` call goes through `read_only()` (`--pinentry-mode error`) for
  the same reason; only decryption may ask for a passphrase.
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
Take screenshots of the hidden display to check a visual change.
Render SVGs with `rsvg-convert`: ImageMagick mangles gradients and makes
a good icon look broken.

## How the code is shaped

- **Mail actions live in `sync`**, not in windows. Archive, flag,
  label, list a mailbox, change an automatic reply: one module each in
  `sync`, called by both the window and the assistant. A window that
  starts doing its own Gmail work is drifting.
- **Late answers.** Any `await` in the UI can finish after the reader
  has moved to another thread. Before touching the conversation view
  after an await, check `ConversationView::is_showing(account_id,
  thread_id)` for the thread you started on. `app/src/protection/run.rs`
  has the rule behind ports (`Desk`, `Effects`, `Wanted::still`) with
  tests over a fake window; seven older call sites still check by hand
  (listed in the backlog).
- **The open thread changes through named methods** on
  `ConversationView` (`bodies_arrived`, `translated`, `engine_answered`,
  and so on), and is read through `read` and `find`. Add a named change
  rather than reaching into `OpenThread`.
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
- Prose in the UI, comments, docs, and commit messages follows the
  stop-slop and unslop rules: plain statements, no em dashes, no
  adverbs propping up verbs. Comments explain why, in full sentences,
  matching the density of the code around them.

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
