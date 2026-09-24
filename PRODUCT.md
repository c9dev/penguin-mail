# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

Penguin Mail is a native Linux desktop app in Rust with GTK 4 and libadwaita, not a web app. The schema has no
value for a GTK desktop app, so this records the nearest one. Its styling is GTK CSS (`app/data/style.css`), and
the GNOME Human Interface Guidelines and libadwaita's own patterns govern the design, not web conventions.

## Users

Privacy-minded power users on a GNOME desktop: people who want a native, keyboard-driven mail client with signing,
encryption and an assistant, and who would otherwise use Thunderbird, Evolution, Geary or Gmail in the browser.
They keep several accounts, read mail in more than one language, and expect the app to feel part of the desktop.
The owner is one of them and uses the app every day, reading the interface in English and receiving mail in
European Portuguese.

## Product Purpose

A beautiful and functional mail and calendar client for Linux, which the platform lacks. It reads, sorts and writes
mail across several accounts from a copy kept on the computer, and adds a calendar beside the mail (designed in
`docs/superpowers/specs/2026-09-23-calendar-design.md`, not yet built). Success is people choosing it over the
browser and the older clients because it is both nicer to use and more capable.

## Positioning

- **Native GNOME and fast.** A libadwaita app that belongs on the desktop, reading from a local copy so mail opens
  in milliseconds.
- **Private by default.** Remote images blocked until asked for, mail kept on this computer, OpenPGP and S/MIME
  through the person's own GnuPG, no tracking.
- **An assistant that acts.** It searches, sorts, drafts, unsubscribes and changes settings, asks before it acts,
  and works with any model, local ones included.
- **Gmail's features, natively.** Categories, labels, filters, automatic replies, send-as aliases and Google
  Calendar invitations without the browser.

## Operating Context

A GNOME desktop on Linux (Ubuntu 26.04 is the target), with the app living in the tray and syncing in the
background. Installed from the project's apt repository, the Snap Store (pending review), a release tarball, or a
Flatpak bundle; Flathub is on hold. Several Gmail accounts at once today; IMAP and SMTP, Microsoft, and CalDAV are
planned (`docs/superpowers/specs/2026-09-22-other-providers-overview.md`). Screen readers and keyboard-only use are
supported and checked.

## Capabilities and Constraints

- Mail: unified and per-account mailboxes, conversations, Gmail categories, labels, flags in colours, VIPs, Follow
  Up, Remind Me, Send Later with Undo Send, an outbox that works offline, templates, a rich-text composer,
  unsubscribe that finishes the list's own page, Hide My Email aliases, rules and automatic replies.
- Security: OpenPGP and S/MIME signing and encryption through GnuPG, revocation checks, signed update checks.
- Assistant: models from Anthropic, OpenAI-compatible servers, local servers or a Claude subscription; MCP
  servers; sandboxed skills; every change asks first unless the person chose Always Allow.
- Languages: American English source strings, British English generated from them, European Portuguese kept
  complete.
- Terms are defined in `CONTEXT.md`; the code's rules are in `AGENTS.md`.
- Undecided: whether the accent colour becomes an in-app setting or keeps following the desktop's accent (see
  Brand Commitments).

## Brand Commitments

- **The icon idiom.** The app icon sits on the Gruvbox Plus Dark squircle with its bevel: taupe behind a cream
  penguin with an orange beak. New icons follow that idiom.
- **The accent is configurable and orange by default.** Mockups, screenshots and the demo video use the orange.
- **Plain-spoken voice.** Every string, comment, doc and commit follows the stop-slop and unslop rules: plain
  statements, no filler, no hype.
- **GNOME HIG first.** libadwaita patterns and the GNOME Human Interface Guidelines win over custom interface.

## Evidence on Hand

- Screenshots of the real app in `docs/screenshots/` (inbox, dark, categories, composer, assistant, flags, VIPs,
  selection, send later, rules, preferences, phone width, welcome, Hide My Email, automatic reply).
- A demo video on YouTube (https://youtu.be/0PyJCsw1FSE) with its poster `docs/screenshots/tour.png`.
- Approved mockups of the calendar and the refreshed mail screen in `docs/superpowers/specs/calendar-mockup/`.
- No user counts, reviews, testimonials, press or benchmarks against other clients exist. Do not invent any.

## Product Principles

1. **Belong on the desktop.** Follow GNOME's patterns before inventing new ones; the app should feel like part of
   the system.
2. **The person's mail stays theirs.** Keep it on the computer, load nothing remote without asking, and never let
   the assistant act without consent.
3. **Fast because it is local.** Read from the copy on disk; the network is for keeping it fresh, not for showing
   what is already known.
4. **Capable without clutter.** Power features sit one step deeper than the common path, and undo beats
   confirmation.
5. **Say what happened.** Every message names the actual thing that went wrong and what the person can do next.

## Accessibility & Inclusion

Every control has an accessible name; `scripts/a11y-names.sh` checks it in CI, including every menu. Keyboard
shortcuts cover the main actions and are listed in the Keyboard Shortcuts dialog. Motion follows GNOME's
animations setting. The interface ships in American English, British English and European Portuguese. Details in
`docs/accessibility.md`.
