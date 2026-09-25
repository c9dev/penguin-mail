# 1. Keep a local copy of each account's calendars

Status: Accepted, 2026-09-25

## Context

The assistant answers questions about the user's week, and the
invitation card says what else the hour holds while a message is open.
Both need an account's calendars and events on hand, repeatedly, without
a Google Calendar call for each question and each opened message: a
call costs quota, waits on the network, and, for the clash line, runs
once per invitation shown.

Mail already keeps a local copy for the same reason: the store holds
messages and threads, filled from Gmail's history feed, so the window
reads them without asking Gmail each time. This decision keeps a copy
of calendars the same way, for Gmail accounts first; CalDAV and
Microsoft Graph join later behind the same seam.

## Decision

Calendars and events live in their own tables in the SQLite store
(`calendars`, `events`, `event_guests`, `calendar_changes`, migration
33), filled from each provider's own change feed: Google's
`syncToken`-based incremental list, one page at a time, the same shape
of feed mail sync already reads. A repeating event is stored once, as
its `RRULE`, `EXDATE` and `RDATE` lines, not as one row per occurrence;
`mailrs_domain::calendar::expand` turns the rule into occurrences only
when something reads a range, which keeps a five-year-old daily meeting
as cheap to store as one that happened once.

`mailrs_sync::calendar_copy::CalendarCopy` owns the read and the queue
of edits made here that the provider has not taken yet
(`mailrs_store::calendar::QueuedChange`), sent in order, right after
the edit that queued them rather than on the next tick.
A provider-neutral error (`BackendError::StateLost`, `Changed`,
`Refused`) tells the copy what to do with a stale read or a refused
write without it ever naming a provider's own error codes; only the
Google adapter maps Google's.

The app calls `CalendarCopy::refresh_due` from its own 15-second timer
(`App::watch_calendars` in `app/src/app.rs`), the one that also drives
the address book (`App::watch_contacts`), rather than from the mail
sync engine's own tick (`AccountSync`). `CalendarCopy` decides for
itself which account is actually due, at a one-minute cadence while the
window is open and five minutes while only the tray runs, so most ticks
of the app's timer cost nothing. The mail engine's loop gains no new
state, no new per-tick work, and no new failure mode from the
calendar. A slow calendar read does not hold up the account's mail
sync, and the two never wait on each other.

## Options turned down

**Raw iCalendar**, parsed from a `.ics` blob on every read. Answering
"what does Tuesday look like" still needs a time-ordered index to avoid
scanning every event on every read, so this option builds the same
index as the chosen one and adds a parse on top. Google's own fields,
such as a guest's response status, an event's colour, its conference
link and its etag, have no home in plain iCalendar, so they would need
a side table regardless, and keeping the raw text as well would save
nothing.

**Evolution Data Server (EDS)**, letting GNOME's own calendar backend
own the copy instead of Penguin Mail. EDS ties the app to GNOME Online
Accounts for sign-in, which Penguin Mail does not use and does not want
to require. Reaching EDS over D-Bus from a Flatpak or Snap needs a wide
hole in the sandbox, wider than anything else the app asks for, and
weakens the confinement the packaging otherwise holds to. It would also
take the copy out of the app's own hands: the read cadence, the change
queue, and the provider-neutral error mapping `BackendError` gives
every other backend would instead depend on what EDS chooses to do,
and on a system service already running and configured, which is not
guaranteed on every desktop Penguin Mail targets.

## Consequences

The assistant, the invitation card, and the calendar view stage 2
brings read an account's calendars with no network call once the copy
has synced it, and the change queue lets an edit made offline reach the
provider once the network returns. The store gains four tables and a
row per pending edit; `CalendarCopy` gains a background timer of its
own, with its own lock (`CalendarCopy`'s `running` mutex) so a slow
read is never joined by a second one reading, or, once an edit is
queued, sending the same change twice.

A calendar backend that is not Google, added in a later stage, must
speak only `BackendError`'s neutral kinds inside `CalendarCopy`; the
copy gains no branch per provider, and a provider's own errors are the
adapter's problem alone, mapped once at the edge.

Running the calendar's poll from the app's own timer, separately from
the mail engine's, means two poll loops run in the process instead of
one. Neither depends on the other's health: a calendar read that stalls
does not slow mail sync. The copy reads through the account's running
engine, though, so an account paused or signed out is not read until it
runs again. The cost is one more timer to
reason about, accepted because folding the calendar into the mail
engine's loop would have threaded new state through `AccountSync` for
every account, mail-only accounts included, to serve a poll only
calendar-capable accounts need.
