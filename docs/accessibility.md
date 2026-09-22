# Accessibility

What a screen reader gets from Penguin Mail, how to check it, and what is
still out of a keyboard's reach.

## Names

Every control a person acts on carries a name in the accessible tree. A
button with an icon and no label has none of its own, and GTK never reads
a tooltip out, so `mailrs::ui` holds four helpers and the call sites use
them:

- `name` sets the name.
- `name_with_shortcut` takes a tooltip that ends in its keys, such as
  `Archive (E or Ctrl+Alt+A)`, and splits it: the words become the name
  and the keys a property of their own.
- `describe` adds the line read after the name.
- `labelled_by` ties a field to the word standing beside it.

Every name is a translated string like any other word a person reads, and
the wording a count or a state decides is built by a function of its own
so a test can read it without a window.

Two things a box cannot do: a widget whose role is `generic`, which is
what a `GtkBox` gets, is given no name however one is set on it, and a
widget the list view wraps for itself cannot be named from outside
without crashing GTK. The thread row takes the `ListItem` role instead,
and then the name it builds is the one a reader hears.

## The message

`app/src/render.rs` writes the page the WebView shows, so the message
gets HTML accessibility: the subject is the page's only `h1` and each
sender is a heading under it, the details panel is a real `<details>`,
the attachment rows are a list whose links say which file they act on,
and the page carries the language it is written in. A picture the sender
described keeps their words; one nobody described is given an empty
description in `sanitize.rs`, so a reader passes over it rather than
spelling a kilobyte of base64 out.

## The composer from the keyboard

Tab goes From, To, Cc and Bcc while they show, Subject, the formatting
bar, then the body. The bar is one stop: it has the toolbar role, and
Left, Right, Home and End move between its buttons while Tab leaves it.
Tab comes back to the button you left it on. `ui::roving` holds this, and
a click on a button leaves the focus in the text.

Every formatting button has a shortcut as well, Insert Image included
(Ctrl+Shift+P). The headings and block styles have none and are reached
through More Formatting, the last button on the bar.

The chips in an address field stay out of the Tab chain too, so crossing
a field takes one press. Left at the start of the entry steps onto the
last chip, Left and Right move between chips, and Right from the last one
goes back to the entry; Home and End go to the first chip and to the
entry. Delete or Backspace removes the chip with the focus. Backspace
moves the focus to the chip before it and Delete to the one after, and
the focus goes back to the entry once no chip is left. Each chip reads as
its name and address with "Press Delete to remove" after it, and carries
a `recipient.remove` action a screen reader can run; its close button
runs the same action.
## Menus

A thread row's menu holds `Export…`, and in the Outbox `Edit…`, `Send
Now` and `Delete`. A right-click or a long press opens it, and so do Menu
and Shift+F10 on the row with the focus. The focus sits on the list item
GTK wraps each row in, and the row is a child of that item, so a key
controller on the row would never see the key: the list view takes both
keys itself and opens the menu of the row in focus, over the row. The
Keyboard Shortcuts dialog lists them.

A queued message opens in the conversation pane like any other, with a
card above it that says when it goes or why it has not gone, with
buttons that act on it.

## Checking it

```
scripts/a11y-names.sh
```

opens the demo on a hidden display, walks the accessible tree over
AT-SPI, and names every control that would be announced as nothing. It
exits 1 while anything is unnamed. `--here` reads the copy already on
your screen instead, which is how to check a dialog or the composer:
open it, then run the script.

## What the keyboard cannot reach

Nothing known. The last four gaps closed in September 2026: the row
menus open with Menu or Shift+F10, the formatting bar is one tab stop,
Insert Image has Ctrl+Shift+P, and the arrow keys reach every recipient
chip. Write a new gap down here when you find one, with why it was left.
