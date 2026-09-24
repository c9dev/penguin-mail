---
name: Penguin Mail
description: A beautiful and functional mail and calendar client for GNOME
colors:
  accent: "#e8660c"
  accent-dark: "#ff7a1a"
  surface-window: "#f6f6f7"
  surface-window-dark: "#242428"
  surface-sidebar: "#ebebed"
  surface-sidebar-dark: "#2e2e33"
  surface-view: "#ffffff"
  surface-view-dark: "#1e1e21"
  surface-popover: "#ffffff"
  surface-popover-dark: "#36363c"
  pill: "#e3e3e7"
  pill-dark: "#3a3a40"
  hairline: "#ececef"
  hairline-dark: "#2c2c31"
  border: "#dedee3"
  border-dark: "#39393f"
  ink: "#1f1f23"
  ink-dark: "#f4f4f6"
  ink-dim: "#6e6e76"
  ink-dim-dark: "#a6a6af"
  ink-faint: "#a0a0a8"
  ink-faint-dark: "#6e6e77"
  calendar-blue: "#3584e4"
  calendar-green: "#2ec27e"
  calendar-purple: "#9141ac"
  calendar-red: "#e01b24"
  calendar-yellow: "#e5a50a"
  calendar-teal: "#2190a4"
  calendar-pink: "#c061cb"
typography:
  display:
    fontFamily: "Ubuntu Sans, Cantarell, sans-serif"
    fontSize: "22px"
    fontWeight: 800
    lineHeight: 1.2
  headline:
    fontFamily: "Ubuntu Sans, Cantarell, sans-serif"
    fontSize: "20px"
    fontWeight: 800
    lineHeight: 1.2
  title:
    fontFamily: "Ubuntu Sans, Cantarell, sans-serif"
    fontSize: "15px"
    fontWeight: 800
    lineHeight: 1.3
  body:
    fontFamily: "Ubuntu Sans, Cantarell, sans-serif"
    fontSize: "14.5px"
    fontWeight: 400
    lineHeight: 1.6
  body-strong:
    fontFamily: "Ubuntu Sans, Cantarell, sans-serif"
    fontSize: "14px"
    fontWeight: 700
    lineHeight: 1.35
  meta:
    fontFamily: "Ubuntu Sans, Cantarell, sans-serif"
    fontSize: "12.5px"
    fontWeight: 500
    lineHeight: 1.35
    fontFeature: "tnum"
  label:
    fontFamily: "Ubuntu Sans, Cantarell, sans-serif"
    fontSize: "11.5px"
    fontWeight: 700
    letterSpacing: "0.04em"
rounded:
  event: "8px"
  button: "10px"
  card: "12px"
  surface: "14px"
  pill: "999px"
spacing:
  hairline: "1px"
  xs: "4px"
  sm: "8px"
  md: "12px"
  lg: "16px"
  xl: "24px"
  gutter: "36px"
components:
  button-primary:
    backgroundColor: "{colors.accent}"
    textColor: "{colors.surface-view}"
    rounded: "{rounded.button}"
    height: "34px"
    padding: "0 16px"
  button-neutral:
    backgroundColor: "{colors.pill}"
    textColor: "{colors.ink}"
    rounded: "{rounded.button}"
    height: "34px"
    padding: "0 16px"
  toolbar-capsule:
    backgroundColor: "{colors.pill}"
    textColor: "{colors.ink}"
    rounded: "{rounded.pill}"
    height: "32px"
  chip-selected:
    backgroundColor: "{colors.accent}"
    textColor: "{colors.surface-view}"
    rounded: "{rounded.pill}"
    height: "30px"
    padding: "0 14px"
  chip:
    backgroundColor: "{colors.pill}"
    textColor: "{colors.ink}"
    rounded: "{rounded.pill}"
    height: "30px"
    width: "44px"
  sidebar-row-selected:
    textColor: "{colors.accent}"
    rounded: "{rounded.button}"
    height: "32px"
  event-card:
    textColor: "{colors.ink}"
    rounded: "{rounded.event}"
    padding: "4px 8px 4px 11px"
  popover:
    backgroundColor: "{colors.surface-popover}"
    textColor: "{colors.ink}"
    rounded: "{rounded.surface}"
    padding: "18px"
---

<!-- TARGET: this records the approved new design, not the code as it stands. It comes from the mockups in
docs/superpowers/specs/calendar-mockup/ (mockups.py holds every value), the calendar spec's "The look"
(2026-09-23-calendar-design.md) and the mail refresh spec (2026-09-24-mail-refresh-design.md). The calendar
and mail refresh plans build it. Re-run /impeccable document once they ship, to record what the code does. -->

# Design System: Penguin Mail

## Overview

**Creative North Star: "The Quiet Desk, made the GNOME way"**

Penguin Mail is a desk you work at all day: mail in one drawer, the calendar in the other, one switch at the top
of the sidebar between them. Nothing on it asks for attention it has not earned. Surfaces are quiet greys in
layers, content is the only thing with colour, and a single accent marks where you are and what the next step is.

It is made the GNOME way. The polish lives in what libadwaita already gives, done with care: exact spacing, soft
tints instead of new ornament, capsules that group what belongs together, motion that follows the hand and stops
when GNOME's animations are off. A person should not be able to tell where libadwaita ends and Penguin Mail begins.

The same materials build both screens. A selected conversation and a calendar event are the same object, a soft
tinted card with a bar of colour, so moving between Mail and Calendar feels like turning to another page of the
same notebook.

**Key Characteristics:**
- Tonal layers: window, inset sidebar, white view card; shadows only for what floats.
- One accent, the desktop's own, on at most one filled control per region.
- Tinted cards with a colour bar for selection, events and the invitation's afternoon.
- Actions grouped in pill capsules; chips name only the chosen one.
- Hairlines, never boxes; small dimmed capitals for section labels.
- Light and dark as equals, with stronger tints in dark.

## Colors

A cool, near-neutral grey scale with one warm accent and GNOME's palette reserved for calendars.

### Primary
- **Desktop Accent** (Ember Orange on the owner's desktop): the accent the person set in GNOME's settings.
  The app never picks its own. It fills the one primary button in a place, the chosen category chip, today's
  date in the mini month, the time-now line, the Undo Send pill, unread dots and unread times. Every design
  artefact shows it as orange, the default.

### Neutral
- **Desk Grey** (window): the surface behind everything, visible round the inset sidebar and the view card.
- **Drawer Grey** (sidebar): the inset sidebar panel, one step darker than the window.
- **Paper White** (view): the reading pane, the calendar grid card, the list background in light mode.
- **Popover White / Raised Slate**: menus and popovers, the one surface that floats.
- **Capsule Grey** (pill): toolbar capsules, neutral chips, neutral buttons, the Mail / Calendar switch track.
- **Hairline** and **Edge**: hour lines and row separators (hairline); card and popover outlines (edge).
- **Ink**, **Muted Ink**, **Faint Ink**: text in three strengths: names and titles; times, snippets and
  subtitles; section labels, hour labels and dates outside the month.

### Calendar colours
GNOME's own palette, used only as a calendar's or event's colour: **Blue**, **Green**, **Purple**, **Red**,
**Yellow**, **Teal**, **Pink**, plus the accent orange for the primary calendar. Avatars without a photo take
these hues at 85 % over the window.

### Named Rules
**The Desktop Accent Rule.** The accent is whatever GNOME's settings say. Draw it with libadwaita's accent
variables (`--accent-bg-color`, `--accent-color`), never a fixed hex; the hex above is the reference value only.

**The One Filled Control Rule.** A region holds at most one control filled with the accent: Yes in an answer row,
the selected chip, New Event. Everything else is capsule grey.

**The Tint, Not Fill Rule.** Selection and events are the colour mixed into the surface below: 14 % in light,
24 % in dark. Text always sits on the tint in ink, never on the full colour.

## Typography

**Body Font:** the system font (Ubuntu Sans on the target desktop, Cantarell elsewhere). No second family.

**Character:** one family doing everything through weight and size, as GNOME apps do: heavy weights (800) for
the few things that name a place, regular for reading, tabular figures wherever times line up.

### Hierarchy
- **Display** (800, 22px, 1.2): a conversation's subject above its messages.
- **Headline** (800, 20–21px, 1.2): the calendar's range title, "September" with the year beside it in Faint Ink
  at 400 and the week number smaller still.
- **Title** (800, 15px): a sender's name in a message header; an event's title in its popover (17px).
- **Body** (400, 14.5px, 1.6): message text, capped by the reading pane's width.
- **Body strong** (700, 14px): sender names and subjects in the list (800 and 700 when unread), sidebar rows
  (500, 700 when selected), event titles on cards (12–12.5px, 700).
- **Meta** (500, 12.5px, tabular figures): times, snippets, "To:" lines, event times (11px on cards).
- **Label** (700, 11.5px, 0.04em, capitals): section labels (FAVORITES, MAILBOXES, ACCOUNTS, YOUR AFTERNOON,
  WAITING FOR YOUR ANSWER), weekday names on day headings (11px, 0.06em), ALL-DAY (9.5px).

### Named Rules
**The Tabular Time Rule.** Every time, date and count that sits in a column uses tabular figures, so 09:30 and
11:00 line up.

**The Two-Weight Heading Rule.** A heading pairs a bold part and a quiet part: "September" at 800 with "2026" in
Faint Ink at 400; "WED" in small capitals with "23" at 800.

## Layout

One window, three regions: an inset sidebar (256px, set 8px inside the window's top, bottom and start edges),
the list or the calendar's header, and the view card. Mail has sidebar, list (about 392px) and reading pane;
Calendar has sidebar and the grid card, set 10px in from the sidebar and the window's end.

Spacing runs on a 4px base: 8 and 12 between related things, 16 inside cards, 24 between groups, 36 as the
reading pane's side margin. Rows in the sidebar are 32–34px apart; list rows 92px; hour rows 52px.

Narrow windows follow libadwaita's breakpoints: the sidebar collapses at 960sp, the assistant overlays at 1100sp,
and at 620sp Mail shows one pane at a time and Calendar drops Week and Month for the agenda.

### Named Rules
**The Hairline Rule.** Hairlines mark hours and separate list rows, starting where the text starts, never at the
avatar, and never touching a selected card. No boxes round groups of rows.

## Elevation & Depth

Depth comes from tone. The window is the lowest layer, the inset sidebar sits on it one shade darker, and the
view card is the brightest, with a 1px edge in light mode. Nothing inside those layers casts a shadow. Only what
floats over content does: the window itself on the desktop, popovers, menus and the small next-event card at the
sidebar's foot.

### Shadow Vocabulary
- **Floating** (`0 6px 20px rgba(0,0,0,0.18)`, 0.5 opacity in dark): popovers and menus.
- **Resting lift** (`0 1px 2.4px rgba(0,0,0,0.10)`): the white segment in the Mail / Calendar switch and the
  pressed segment of a capsule, the next-event card and the waiting-invitation cards in the sidebar.

### Named Rules
**The Floating Only Rule.** A shadow means "this is above the page". Cards that belong to the page never get one.

## Shapes

Rounded everywhere, with the radius growing with the size of the thing: 8px for event cards, 10px for buttons
and sidebar selection, 12px for cards and the sidebar panel, 14px for the window, the view card and popovers,
full pills for capsules, chips and the switch.

A coloured bar is the one recurring mark: 3.5px wide, rounded, down the start edge of an event card, a selected
all-day event and the invitation's afternoon blocks, and 4px beside a popover's title. An event not yet answered
drops its fill and draws a 1.5px dashed outline in its colour.

## Components

### Buttons
- **Shape:** gently rounded (10px), 34px tall.
- **Primary:** accent fill with white text (dark text on the brighter dark-mode accent); one per region.
- **Neutral:** capsule grey fill, ink text: Maybe, No, Show in Calendar, Join with Google Meet.
- **Linked answer row:** Yes, Maybe, No as three equal buttons 8px apart, the current answer filled.

### Toolbar capsules
Actions that belong together share one pill (32px, full radius): Reply / Reply All / Forward; Archive / Trash /
Junk; Read / Flag; Labels. Buttons inside are flat 16px symbolic icons with hairline separators; a pressed toggle
is a raised segment. More stays a lone round button.

### Chips
- **Selected:** accent pill with icon and name ("Primary").
- **Others:** capsule grey pills with only their icon, and the unread count as a small accent badge on the top
  end corner.

### Sidebar rows
Symbolic icon, name, count at the end in Muted Ink. The selected row is a 10px card tinted with the accent, its
icon, name and count in the accent. Calendars use their colour dot as the checkbox: filled with a tick when shown,
an outline when hidden, a small lock when read-only.

### Event card (signature)
The shape the whole design is built on. A rounded rectangle (8px) tinted with its colour, a 3.5px bar of the full
colour down its start edge, the title bold in ink, the time under it on one line in Muted Ink. Under 45 minutes,
title and time share one line. Declined events fade and strike through; unanswered ones are dashed outlines;
events waiting to be saved carry a small clock. The selected conversation in the list and the blocks in an
invitation's afternoon strip are this same card.

### Popovers
14–16px corners, Popover White or Raised Slate, the Floating shadow, an arrow pointing at what opened it. Content
in order: colour bar and title, time in words, detail rows with small symbolic icons, then full-width actions.

### Navigation
The Mail / Calendar switch is a pill track (capsule grey) with a raised white segment under the chosen side, both
labelled with an icon. Day / Week / Month uses the same shape. Today is a capsule grey pill; back and forward share
one capsule.

## Do's and Don'ts

### Do:
- **Do** take the accent from GNOME's settings through libadwaita's variables.
- **Do** tint selection and events at 14 % in light and 24 % in dark, with ink text on top.
- **Do** group related actions in one pill capsule, keeping every tooltip, accessible name and shortcut.
- **Do** end text that does not fit with an ellipsis on one line; never cut a line in half.
- **Do** give every time and count tabular figures.
- **Do** turn slides and springs into a 150ms cross-fade when GNOME's animations are off, and keep springs
  critically damped (no bounce) everywhere else.
- **Do** test every screen in light and dark.

### Don't:
- **Don't** fix the accent to a hex value or add an accent setting to the app.
- **Don't** fill more than one control per region with the accent.
- **Don't** put text on a full-strength calendar colour.
- **Don't** draw boxes round groups or give page cards a shadow.
- **Don't** use libadwaita's grey selection in the sidebar or the list; selection is the accent tint.
- **Don't** invent controls GNOME already has; libadwaita's widget comes first.
