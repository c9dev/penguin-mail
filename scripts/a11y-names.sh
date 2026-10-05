#!/usr/bin/env bash
# Walks the accessible tree of a running Penguin Mail and names every
# control a screen reader would announce as nothing.
#
#   scripts/a11y-names.sh          open the demo on a hidden display
#   scripts/a11y-names.sh --here   read the copy already on your screen
#
# A button with an icon and no label is what this is looking for: GTK
# gives it no name, and a tooltip is not one. The exit status is 1 while
# anything is unnamed, so a build machine can hold the line.
#
# On the hidden display it also opens every menu it can reach, since a
# menu is in the accessible tree only while it is open, opens a
# conversation to read its message view and invitation card, opens a
# reply and a forward in the composer with their history shown, opens Add
# Account on its tiles and its Other page, and walks the calendar.
#
# The hidden display needs Xvfb, dbus-run-session, at-spi2-core, python3
# with the GObject bindings, and the XTest library to click and type:
#   sudo apt install xvfb dbus-daemon at-spi2-core python3-gi libxtst6
set -euo pipefail

cd "$(dirname "$0")/.."
here=${1:-}

walk=$(mktemp)
trap 'rm -f "$walk"' EXIT
cat > "$walk" <<'PYTHON'
import re
import sys
import time

import gi

gi.require_version("Atspi", "2.0")
from gi.repository import Atspi

# How many screens of each list to look through for rows with menus.
PAGES = 6

# How long to wait for the window to reach the bus. A cold demo store
# takes a few seconds to fill before anything is drawn.
PATIENCE = 60

# Roles a person acts on. Everything else is scenery, and a heading or a
# label says what it says through its own text.
# Both spellings of a plain button are here: AT-SPI has called it one and
# then the other, and which one arrives is the toolkit's business.
ACTS = {
    "button", "check box", "check menu item", "combo box", "entry", "link",
    "list item", "menu item", "page tab", "password text", "push button",
    "radio button", "radio menu item", "slider", "spin button", "switch",
    "table cell", "text", "toggle button",
}

found = []


def webkit_key_text(node, role, name):
    """Whether `node` is the text view WebKit keeps inside every web view.
    WebKitGTK parents a GtkTextView to the view, unseen at 0 x 0, to turn
    key bindings such as Ctrl+C and Ctrl+A into editing commands, and it
    reaches the bus as an editable, multi-line "text" with no name. Nobody
    can land on it, and hiding the widget would take copy away from the
    message view, so the walk skips it. The match is narrow: that role,
    no name, no size, no children, and a sibling holding the web page's
    "filler", which only a web view has."""
    if role != "text" or name or node.get_child_count():
        return False
    try:
        box = node.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
        states = node.get_state_set()
        parent = node.get_parent()
    except Exception:
        return False
    if box.width or box.height or parent is None:
        return False
    if not (states.contains(Atspi.StateType.EDITABLE) and states.contains(Atspi.StateType.MULTI_LINE)):
        return False
    for index in range(parent.get_child_count()):
        sibling = parent.get_child_at_index(index)
        if sibling is None or sibling == node:
            continue
        for inner in range(sibling.get_child_count()):
            child = sibling.get_child_at_index(inner)
            if child is not None and child.get_role_name() == "filler":
                return True
    return False


def walk(node, path, sink=None):
    """Records every control under `node` into `sink` (`found` by
    default), and says whether a named control was found in there.

    A control that holds another named control is a wrapper rather than
    something a person acts on, and wanting a name of its own would be
    asking for the same words twice: a list view puts every row inside a
    list item of its own, and the row is what carries the name."""
    if sink is None:
        sink = found
    try:
        role = node.get_role_name()
        name = (node.get_name() or "").strip()
    except Exception:
        # A window that closed mid-walk takes its children with it.
        return False
    if webkit_key_text(node, role, name):
        return False
    here = path + ["%s %r" % (role, name) if name else role]
    wraps = False
    for index in range(node.get_child_count()):
        child = node.get_child_at_index(index)
        if child is not None and walk(child, here, sink):
            wraps = True
    if role in ACTS and (name or not wraps):
        sink.append((role, name, " > ".join(here[-4:])))
    return (role in ACTS and bool(name)) or wraps


def penguins():
    desktop = Atspi.get_desktop(0)
    apps = [desktop.get_child_at_index(i) for i in range(desktop.get_child_count())]
    named = [(a, a.get_name() or "") for a in apps if a is not None]
    return [a for a, name in named if name.startswith("io.github.c9dev")], [
        name for _, name in named
    ]


def controls():
    found.clear()
    for app in penguins()[0]:
        walk(app, [])
    return len(found)


# The app reaches the bus before it has drawn anything, and the mailboxes
# arrive from the store a moment after that. Wait for the count to hold
# still rather than for a guessed number of seconds.
waited, before = 0, -1
while waited < PATIENCE:
    now = controls()
    if now > 0 and now == before:
        break
    before = now
    time.sleep(2)
    waited += 2
if not found:
    print("Penguin Mail never reached the accessibility bus.", file=sys.stderr)
    print("On it after %ds: %s" % (waited, ", ".join(penguins()[1]) or "nothing"), file=sys.stderr)
    sys.exit(2)


# A menu reaches the accessible tree only while it is open, so the walk
# above never sees one. On the hidden display the script opens every
# menu it can reach: the right-click menu of each row on screen, and the
# menu behind each button that has one, with each submenu of both. It
# needs a pointer and a keyboard for the rows, and on your own screen
# those are yours, so --here leaves menus alone.
def nodes(node):
    yield node
    for index in range(node.get_child_count()):
        child = node.get_child_at_index(index)
        if child is not None:
            yield from nodes(child)


def showing_menus():
    try:
        return [n for app in penguins()[0] for n in nodes(app) if n.get_role_name() == "menu"]
    except Exception:
        return []


def wait_until(test, seconds):
    end = time.time() + seconds
    while time.time() < end:
        if test():
            return True
        time.sleep(0.1)
    return test()


class Input:
    """Pointer and keys on the hidden display, through the XTest
    extension. AT-SPI's own event synthesis reaches nothing under Xvfb."""

    def __init__(self):
        import ctypes

        try:
            self.x11 = ctypes.CDLL("libX11.so.6")
            self.xtst = ctypes.CDLL("libXtst.so.6")
        except OSError:
            print("a11y-names.sh needs libxtst6 to open menus", file=sys.stderr)
            sys.exit(2)
        self.x11.XOpenDisplay.restype = ctypes.c_void_p
        self.display = ctypes.c_void_p(self.x11.XOpenDisplay(None))

    def right_click(self, x, y):
        self.xtst.XTestFakeMotionEvent(self.display, -1, x, y, 0)
        self.x11.XFlush(self.display)
        time.sleep(0.1)
        for pressed in (1, 0):
            self.xtst.XTestFakeButtonEvent(self.display, 3, pressed, 0)
            self.x11.XFlush(self.display)
            time.sleep(0.05)

    def click(self, x, y):
        self.xtst.XTestFakeMotionEvent(self.display, -1, x, y, 0)
        self.x11.XFlush(self.display)
        time.sleep(0.1)
        for pressed in (1, 0):
            self.xtst.XTestFakeButtonEvent(self.display, 1, pressed, 0)
            self.x11.XFlush(self.display)
            time.sleep(0.05)

    def scroll_down(self, x, y):
        self.xtst.XTestFakeMotionEvent(self.display, -1, x, y, 0)
        for _ in range(8):
            for pressed in (1, 0):
                self.xtst.XTestFakeButtonEvent(self.display, 5, pressed, 0)
            self.x11.XFlush(self.display)
            time.sleep(0.02)

    def resize(self, width, height):
        """Sizes every top-level window, as a window manager would; the
        hidden display has none."""
        import ctypes

        self.x11.XDefaultRootWindow.restype = ctypes.c_ulong
        root = self.x11.XDefaultRootWindow(self.display)
        root_ret, parent = ctypes.c_ulong(), ctypes.c_ulong()
        children = ctypes.POINTER(ctypes.c_ulong)()
        count = ctypes.c_uint()
        self.x11.XQueryTree(self.display, ctypes.c_ulong(root), ctypes.byref(root_ret),
                            ctypes.byref(parent), ctypes.byref(children), ctypes.byref(count))
        for index in range(count.value):
            self.x11.XMoveResizeWindow(self.display, ctypes.c_ulong(children[index]), 0, 0, width, height)
        self.x11.XFlush(self.display)

    def escape(self):
        code = self.x11.XKeysymToKeycode(self.display, 0xFF1B)
        for pressed in (1, 0):
            self.xtst.XTestFakeKeyEvent(self.display, code, pressed, 0)
            self.x11.XFlush(self.display)
            time.sleep(0.05)


menus_seen = set()


def record(opener):
    """Adds the items of every menu showing to `found`, once for each
    distinct menu, and gives back the items that open a submenu.

    GTK rebuilds a menu's items when its model changes, so an item read a
    moment ago can be gone from the bus. A read that meets one starts
    again on the rebuilt menu, and nothing is recorded until a read gets
    through whole: skipping the menu could hide an item with no name."""
    for _ in range(3):
        try:
            read = read_menus()
        except Exception:
            time.sleep(0.3)
            continue
        submenus = []
        for said, opens in read:
            if said in menus_seen:
                continue
            menus_seen.add(said)
            for index, (role, name) in enumerate(said):
                found.append((role, name, "menu from %s > %s %d" % (opener, role, index + 1)))
            submenus += opens
        return submenus
    print("A menu from %s kept changing while it was read." % opener, file=sys.stderr)
    sys.exit(2)


def read_menus():
    """Every menu showing, as its items' roles and names and the names of
    the items that open a submenu."""
    read = []
    for menu in showing_menus():
        items = [n for n in nodes(menu) if n.get_role_name() in ACTS]
        said = tuple((n.get_role_name(), (n.get_name() or "").strip()) for n in items)
        opens = [
            name for n, (_, name) in zip(items, said)
            if name and n.get_state_set().contains(Atspi.StateType.HAS_POPUP)
        ]
        read.append((said, opens))
    return read


def close(keys):
    for _ in range(3):
        if wait_until(lambda: not showing_menus(), 0.5):
            return
        keys.escape()
    print("A menu would not close.", file=sys.stderr)
    sys.exit(2)


def visit(opener, open_menu, keys):
    """Opens a menu with `open_menu`, records it, then opens it again for
    each of its submenus. Gives back whether a menu opened."""
    if not (open_menu() and wait_until(showing_menus, 1.0)):
        close(keys)
        return False
    time.sleep(0.3)
    submenus = record(opener)
    close(keys)
    for title in submenus:
        if not (open_menu() and wait_until(showing_menus, 1.0)):
            break
        item = next(
            (n for m in showing_menus() for n in nodes(m) if (n.get_name() or "").strip() == title),
            None,
        )
        if item is not None and item.get_action_iface() is not None:
            item.get_action_iface().do_action(0)
            time.sleep(0.8)
            record("%s > %s" % (opener, title))
        close(keys)
    return True


def in_view(node, row):
    """Whether the whole of `row`, a box in window coordinates, lies inside
    every scrolled view that holds `node`. Only its middle is clicked, but
    the extents the accessibility bus reports sit some pixels off from where
    the row is drawn, so a row cut off at the foot of its list can have its
    middle in the list by the numbers and the click below it."""
    parent = node.get_parent()
    while parent is not None:
        if parent.get_role_name() == "scroll pane":
            box = parent.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
            if not (box.x <= row.x and row.x + row.width <= box.x + box.width
                    and box.y <= row.y and row.y + row.height <= box.y + box.height):
                return False
        parent = parent.get_parent()
    return True


def rows_in_view(bounds):
    """Every row on screen, with the point at its middle."""
    for app in penguins()[0]:
        for row in nodes(app):
            try:
                if row.get_role_name() != "list item":
                    continue
                box = row.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
                name = (row.get_name() or "").strip()
            except Exception:
                continue
            # Window coordinates stand for screen ones here: nothing
            # manages the hidden display, so the window sits at its top
            # left corner.
            x, y = box.x + box.width // 2, box.y + box.height // 2
            if box.width <= 0 or not (0 < x < bounds.width and 0 < y < bounds.height):
                continue
            # A row scrolled out of its list still has a place. A click
            # there lands on whatever covers it, and at the foot of the
            # sidebar that opens the window menu GTK draws when no window
            # manager is running, which is GTK's and not the app's.
            if in_view(row, box):
                yield name, x, y


def open_menus():
    keys = Input()
    opened = 0
    frame = next(n for app in penguins()[0] for n in nodes(app) if n.get_role_name() == "frame")
    bounds = frame.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
    # Rows go by name: two rows that read the same, such as the Inbox of
    # two accounts, offer the same menu.
    tried = set()
    for _ in range(PAGES):
        fresh = [(name, x, y) for name, x, y in rows_in_view(bounds) if name not in tried]
        if not fresh:
            break
        for name, x, y in fresh:
            tried.add(name)

            def right_click(x=x, y=y):
                keys.right_click(x, y)
                return True

            opened += visit("row %r" % name[:40], right_click, keys)
        # The rows below the fold have menus too, such as those of labels
        # and smart mailboxes.
        for app in penguins()[0]:
            for pane in nodes(app):
                if pane.get_role_name() == "scroll pane":
                    box = pane.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
                    if box.width > 0 and box.height > 0:
                        keys.scroll_down(box.x + box.width // 2, box.y + box.height // 2)
        time.sleep(0.5)
    # A right click on a thread row opened its conversation, and that
    # brought the conversation's own buttons out.
    buttons = [
        n for app in penguins()[0] for n in nodes(app)
        if n.get_role_name() == "toggle button"
        and n.get_state_set().contains(Atspi.StateType.HAS_POPUP)
        and n.get_state_set().contains(Atspi.StateType.SHOWING)
    ]
    for button in buttons:
        name = (button.get_name() or "").strip()

        def press(button=button):
            action = button.get_action_iface()
            return action is not None and action.do_action(0)

        opened += visit("button %r" % name[:40], press, keys)
    if not opened:
        print("No menu opened, so none was checked.", file=sys.stderr)
        sys.exit(2)
    print("%d menus opened" % opened)


def walk_conversation(keys):
    """Opens the design review invitation, or the first conversation in
    the list when that one is not there, and walks the conversation pane
    once its message view has loaded, into a list of its own like the
    calendar's. The walk above ran before any conversation was open, so
    without this nothing in the message view, the invitation card among
    it, was ever read.

    Reports the conversation's own line and gives back how many of its
    controls came back with no name."""

    def rows():
        for app in penguins()[0]:
            for n in nodes(app):
                try:
                    if n.get_role_name() == "list item":
                        yield n, (n.get_name() or "").strip()
                except Exception:
                    continue

    def pane():
        for app in penguins()[0]:
            for n in nodes(app):
                try:
                    if n.get_role_name() == "grouping" and (n.get_name() or "").strip() == "Conversation":
                        return n
                except Exception:
                    continue
        return None

    def page_loaded():
        conversation = pane()
        return conversation is not None and any(
            n.get_role_name() == "document web" for n in nodes(conversation)
        )

    frame = next(n for app in penguins()[0] for n in nodes(app) if n.get_role_name() == "frame")
    bounds = frame.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
    def clickable(row):
        box = row.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
        x, y = box.x + box.width // 2, box.y + box.height // 2
        if box.width > 0 and 0 < x < bounds.width and 0 < y < bounds.height and in_view(row, box):
            return x, y
        return None

    # The menu walk's right clicks may have left some other conversation
    # open; the invitation is the one worth reading, for its card.
    invitation = [row for row, name in rows() if "Offline editor design review" in name]
    others = [row for row, name in rows() if name] if not page_loaded() else []
    for row in invitation + others:
        spot = clickable(row)
        if spot is not None:
            # A click selects the row, which opens it in the pane; Enter
            # would open it in a window of its own.
            keys.click(*spot)
            break
    if not wait_until(page_loaded, 20.0):
        print("No conversation opened with its message view.", file=sys.stderr)
        sys.exit(2)
    # The page reaches the bus a moment before its own contents do.
    time.sleep(2)
    conversation_found = []
    walk(pane(), ["Conversation"], conversation_found)
    unnamed = [row for row in conversation_found if not row[1]]
    print("conversation: %d controls, %d unnamed" % (len(conversation_found), len(unnamed)))
    for role, _, path in unnamed:
        print("  %s" % path)
    return len(unnamed)


def walk_calendar(keys):
    """Switches to the Calendar space and walks each of its views, Day,
    Week, Month and the narrow List, into a list of its own, so `found`'s
    own count stays what the mail walk left it. In Week it opens the
    popover of an invitation on the range on screen, so Join and Yes,
    Maybe and No are walked; in Month it opens a crowded day's "N more"
    list. It also reads the main menu while the calendar shows, since
    Show Declined Events is there only then.

    Reports the calendar's own line and gives back how many of its
    controls came back with no name, which the exit status adds to the
    mail walk's own count."""

    def find_first(predicate):
        for app in penguins()[0]:
            for n in nodes(app):
                try:
                    role = n.get_role_name()
                    name = (n.get_name() or "").strip()
                except Exception:
                    continue
                if predicate(role, name):
                    return n
        return None

    def on_screen(node):
        try:
            return node.get_state_set().contains(Atspi.StateType.SHOWING)
        except Exception:
            return False

    def in_this_range(node):
        try:
            return node.get_state_set().contains(Atspi.StateType.VISIBLE)
        except Exception:
            return False

    def activate(node):
        # A libadwaita toggle may carry no AT-SPI action, unlike a plain
        # button; click its centre instead, the way a row's menu opens.
        try:
            action = node.get_action_iface()
        except Exception:
            action = None
        if action is not None:
            try:
                if action.do_action(0):
                    return True
            except Exception:
                pass
        try:
            box = node.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
        except Exception:
            return False
        keys.click(box.x + box.width // 2, box.y + box.height // 2)
        return True

    def total_nodes():
        try:
            return sum(1 for app in penguins()[0] for _ in nodes(app))
        except Exception:
            return -1

    def settle():
        """Waits for the tree to hold still after a view changed: its
        pages fill from the store a moment after they appear."""
        before = -1
        for _ in range(20):
            time.sleep(0.5)
            now = total_nodes()
            if now == before and now > 0:
                return
            before = now

    calendar_found = []

    def walk_view(view):
        for app in penguins()[0]:
            walk(app, [view], calendar_found)

    def button_named(pattern):
        return find_first(
            lambda role, name: role in ("button", "push button") and re.search(pattern, name)
        )

    # Libadwaita's own toggles arrive as either "toggle button" or "radio
    # button", so the switch is found by its name and any acted-on role.
    # While mail shows, the name carries the invitations waiting for an
    # answer: "Calendar, 2 waiting for your answer".
    toggle = find_first(
        lambda role, name: role in ACTS and (name == "Calendar" or name.startswith("Calendar, "))
    )
    if toggle is None or not activate(toggle):
        print("No 'Calendar' toggle to switch spaces.", file=sys.stderr)
        sys.exit(2)
    # A plain gtk::Button arrives as "button" here, not "push button";
    # both spellings are in ACTS for the same reason the mail walk needs
    # them (see the comment there).
    today = None
    if wait_until(lambda: button_named(r"^Today$") is not None, 5.0):
        today = button_named(r"^Today$")
    if today is None:
        print("The calendar page never reached the accessibility bus.", file=sys.stderr)
        sys.exit(2)
    # Nothing gives the hidden display's window the keyboard until
    # something in it is clicked; Today is safe to press.
    box = today.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
    keys.click(box.x + box.width // 2, box.y + box.height // 2)
    settle()

    def show_view(view):
        switch = find_first(lambda role, name: role in ACTS and name == view)
        if switch is None or not activate(switch):
            print("No %r in the view switch." % view, file=sys.stderr)
            sys.exit(2)
        settle()

    for view in ("Day", "Week", "Month"):
        show_view(view)
        walk_view(view)
        if view == "Week":
            # An invitation on the range on screen: its popover holds the
            # answer buttons. The ranges either side are hidden from the
            # bus, so the first match is one a person can see.
            def invitations():
                found = []
                for app in penguins()[0]:
                    for n in nodes(app):
                        try:
                            if n.get_role_name() in ("button", "push button") and (
                                n.get_name() or ""
                            ).strip().startswith("Quarterly review,"):
                                found.append(n)
                        except Exception:
                            continue
                return found

            def shown_invitation():
                # The carousel keeps the weeks either side, and may hold a
                # copy of this one while it recycles a page, so the first
                # match is not always the one a person sees. Prefer a block
                # showing on screen; in the evening the week opens scrolled
                # past 15:00, so take one visible in this week after that.
                found = invitations()
                for test in (on_screen, in_this_range):
                    for n in found:
                        if test(n):
                            return n
                return None

            invitation = None
            if wait_until(lambda: shown_invitation() is not None, 5.0):
                invitation = shown_invitation()
            if invitation is None:
                print("No invitation on the week on screen to open.", file=sys.stderr)
                seen = []
                for app in penguins()[0]:
                    for n in nodes(app):
                        try:
                            name = (n.get_name() or "").strip()
                            if n.get_role_name() in ("button", "push button") and ", " in name and ":" in name:
                                seen.append(name[:50])
                        except Exception:
                            continue
                print("  event blocks seen: %d, e.g. %s" % (len(seen), seen[:12]), file=sys.stderr)
                for n in invitations():
                    print(
                        "  found %r showing=%s visible=%s"
                        % ((n.get_name() or "")[:60], on_screen(n), in_this_range(n)),
                        file=sys.stderr,
                    )
                sys.exit(2)
            activate(invitation)
            if not wait_until(lambda: button_named(r"^Maybe$") is not None, 5.0):
                print("The invitation's popover showed no answers.", file=sys.stderr)
                sys.exit(2)
            time.sleep(0.2)
            walk_view("Week popover")
            keys.escape()
            settle()
        if view == "Month":
            # A week row grows to hold its events while the window has
            # room, so the window goes short enough for the busiest week
            # to fold a day into "N more".
            keys.resize(1400, 560)
            wait_until(lambda: button_named(r"^\d+ more events? on ") is not None, 5.0)
            more = button_named(r"^\d+ more events? on ")
            if more is None:
                print("No crowded day in the month to open.", file=sys.stderr)
                sys.exit(2)
            before = total_nodes()
            activate(more)
            wait_until(lambda: total_nodes() != before, 5.0)
            time.sleep(0.3)
            walk_view("Month more")
            keys.escape()
            keys.resize(1400, 900)
            settle()

    # The narrow window shows the agenda in place of Week and Month. Its
    # switch says Agenda as the wide one does, so wait for Week to go too.
    show_view("Week")
    keys.resize(600, 900)

    def narrow_switch():
        def named(label):
            return find_first(lambda role, name: role in ACTS and name == label) is not None

        return named("Agenda") and not named("Week")

    if not wait_until(narrow_switch, 5.0):
        print("A narrow window never offered the agenda.", file=sys.stderr)
        sys.exit(2)
    settle()
    walk_view("Agenda")
    keys.resize(1400, 900)
    settle()

    # The main menu, while the calendar shows, which adds Show Declined
    # Events to it. `record` files its items with the mail walk's menus.
    menu = find_first(lambda role, name: role == "toggle button" and name == "Main Menu")

    def press_menu():
        action = menu.get_action_iface() if menu is not None else None
        return action is not None and action.do_action(0)

    if menu is None or not visit("button 'Main Menu' in the calendar", press_menu, keys):
        print("The main menu would not open in the calendar.", file=sys.stderr)
        sys.exit(2)
    if not any(name == "Show Declined Events" for _, name, _ in found):
        print("The main menu in the calendar had no Show Declined Events.", file=sys.stderr)
        sys.exit(2)

    # New Event, the editor and its Custom Repeat page. The button stays
    # put across every view visited above, so this comes last rather than
    # once per view.
    new_event = button_named(r"^New Event$")
    if new_event is None or not activate(new_event):
        print("No 'New Event' button on the calendar page.", file=sys.stderr)
        sys.exit(2)
    if not wait_until(
        lambda: find_first(
            lambda role, name: role not in ("button", "push button") and name == "New Event"
        )
        is not None,
        5.0,
    ):
        print("The New Event dialog never reached the accessibility bus.", file=sys.stderr)
        sys.exit(2)
    settle()
    more = find_first(lambda role, name: role in ACTS and name == "More")
    if more is None or not activate(more):
        print("No 'More' row in the editor.", file=sys.stderr)
        sys.exit(2)
    settle()
    walk_view("Editor")
    repeats = find_first(lambda role, name: role in ACTS and name.startswith("Repeats"))
    if repeats is None or not activate(repeats):
        print("No 'Repeats' row in the editor.", file=sys.stderr)
        sys.exit(2)
    # The choices are a dropdown's own rows: each is a nameless "list
    # item" wrapping a "label", not a button, so it is found and read by
    # that label's text.
    if not wait_until(
        lambda: find_first(lambda role, name: role == "label" and name == "Custom…") is not None,
        5.0,
    ):
        print("The Repeats row offered no 'Custom…' choice.", file=sys.stderr)
        sys.exit(2)
    # The popover keeps settling a moment after the label lands, and its
    # row offers only a scroll-to action, which `activate` would call
    # and call the row picked; picking one takes a real click.
    time.sleep(0.5)
    custom_label = find_first(lambda role, name: role == "label" and name == "Custom…")
    custom_row = custom_label
    while custom_row is not None and custom_row.get_role_name() != "list item":
        custom_row = custom_row.get_parent()
    if custom_row is None:
        print("No 'list item' row held the 'Custom…' choice.", file=sys.stderr)
        sys.exit(2)
    box = custom_row.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
    keys.click(box.x + box.width // 2, box.y + box.height // 2)
    if not wait_until(
        lambda: find_first(lambda role, name: name == "Custom Repeat") is not None, 5.0
    ):
        print("The Custom Repeat page never showed.", file=sys.stderr)
        sys.exit(2)
    settle()
    walk_view("Custom Repeat")
    back = button_named(r"^Back$")
    if back is None or not activate(back):
        print("No back button on the Custom Repeat page.", file=sys.stderr)
        sys.exit(2)
    settle()

    # Out of office, picked from the Type row, shows the rows that ask
    # which meetings to decline and with what message.
    kind = find_first(lambda role, name: role in ACTS and name.startswith("Type"))
    if kind is None or not activate(kind):
        print("No 'Type' row in the editor.", file=sys.stderr)
        sys.exit(2)
    if not wait_until(
        lambda: find_first(lambda role, name: role == "label" and name == "Out of office") is not None,
        5.0,
    ):
        print("The Type row offered no 'Out of office' choice.", file=sys.stderr)
        sys.exit(2)
    time.sleep(0.5)

    # The week's own out-of-office block has a label of the same words,
    # so the choice is the one inside a list item.
    def in_list_item(node):
        while node is not None and node.get_role_name() != "list item":
            node = node.get_parent()
        return node

    away_row = None
    for app in penguins()[0]:
        for node in nodes(app):
            try:
                if node.get_role_name() == "label" and (node.get_name() or "") == "Out of office":
                    away_row = in_list_item(node)
            except Exception:
                continue
            if away_row is not None:
                break
        if away_row is not None:
            break
    if away_row is None:
        print("No 'list item' row held the 'Out of office' choice.", file=sys.stderr)
        sys.exit(2)
    box = away_row.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
    keys.click(box.x + box.width // 2, box.y + box.height // 2)
    if not wait_until(
        lambda: find_first(lambda role, name: role in ACTS and name.startswith("Decline")) is not None, 5.0
    ):
        print("Out of office showed no 'Decline' row.", file=sys.stderr)
        sys.exit(2)
    settle()
    walk_view("Editor, Out of office")
    if find_first(lambda role, name: name.startswith("Message")) is None:
        print("Out of office showed no 'Message' row.", file=sys.stderr)
        sys.exit(2)
    cancel = button_named(r"^Cancel$")
    if cancel is None or not activate(cancel):
        print("No 'Cancel' button in the editor.", file=sys.stderr)
        sys.exit(2)
    settle()

    unnamed = [row for row in calendar_found if not row[1]]
    print("calendar: %d controls in four views, two popovers, the editor with out of office and its Custom page, %d unnamed"
          % (len(calendar_found), len(unnamed)))
    for role, _, path in unnamed:
        print("  %s" % path)
    return len(unnamed)


def walk_composer():
    """Answers the open conversation with Reply and with Forward, and walks
    each composer twice: with its history folded behind the "•••" button,
    and after that button has put a reply's quote in the body or shown the
    forwarded message under the editor, in a web view of its own. Each
    composer closes without saving before the next step.

    Reports the composer's own line and gives back how many of its
    controls came back with no name, counting as one each a history
    button or a forwarded page that could not be found by its name."""

    def named(role, name, under=None):
        for node in nodes(under) if under is not None else (
            n for app in penguins()[0] for n in nodes(app)
        ):
            try:
                if node.get_role_name() == role and (node.get_name() or "").strip() == name:
                    return node
            except Exception:
                continue
        return None

    def frame_starting(prefix):
        for app in penguins()[0]:
            for index in range(app.get_child_count()):
                window = app.get_child_at_index(index)
                try:
                    if window is not None and (window.get_name() or "").startswith(prefix):
                        return window
                except Exception:
                    continue
        return None

    def press(node):
        node.get_action_iface().do_action(0)

    def close(window, prefix):
        # The header's close button, then Discard when the composer asks
        # whether to keep a draft.
        button = named("button", "Close", window)
        if button is None:
            print("The composer has no Close button by name.", file=sys.stderr)
            return False
        press(button)
        wait_until(lambda: named("button", "Discard", window) is not None, 3.0)
        discard = named("button", "Discard", window)
        if discard is not None:
            press(discard)
        return wait_until(lambda: frame_starting(prefix) is None, 5.0)

    composer_found = []
    missing = 0
    for action, prefix, drop in (
        ("Reply", "Re:", "Remove Quoted Text"),
        ("Forward", "Fwd:", "Do Not Forward the Original"),
    ):
        button = named("button", action)
        if button is None:
            print("No %s button in the open conversation." % action, file=sys.stderr)
            missing += 1
            continue
        press(button)
        if not wait_until(lambda: frame_starting(prefix) is not None, 20.0):
            print("%s opened no composer." % action, file=sys.stderr)
            missing += 1
            continue
        window = frame_starting(prefix)
        wait_until(lambda: named("button", "Show trimmed content", window) is not None, 10.0)
        pill = named("button", "Show trimmed content", window)
        if pill is None or named("button", drop, window) is None:
            print("The %s composer showed no folded history by name." % action, file=sys.stderr)
            missing += 1
        walk(window, ["Composer %s, folded" % action], composer_found)
        if pill is not None:
            press(pill)
            if action == "Reply":
                wait_until(lambda: named("button", "Show trimmed content", window) is None, 5.0)
            else:
                # The page reaches the bus a moment after its box does.
                def shown():
                    group = named("grouping", "Forwarded Message", window)
                    return group is not None and named("document web", "Forwarded Message", group) is not None
                if not wait_until(shown, 15.0):
                    print("The forwarded message showed no named page.", file=sys.stderr)
                    missing += 1
            walk(window, ["Composer %s, unfolded" % action], composer_found)
        if not close(window, prefix):
            print("The %s composer would not close." % action, file=sys.stderr)
            missing += 1
    unnamed = [row for row in composer_found if not row[1]]
    print("composer: %d controls in a reply and a forward, folded and unfolded, %d unnamed"
          % (len(composer_found), len(unnamed)))
    for role, _, path in unnamed:
        print("  %s" % path)
    return len(unnamed) + missing


def walk_add_account(keys):
    """Opens Add Account from the main menu and walks its first page, the
    provider tiles, then the address page behind the Other tile, into a
    list of its own. The dialog closes with Escape before the calendar
    walk.

    Reports the dialog's own line and gives back how many of its controls
    came back with no name, counting as one each page that did not open."""

    def named(role, name):
        for app in penguins()[0]:
            for node in nodes(app):
                try:
                    if node.get_role_name() == role and (node.get_name() or "").strip() == name:
                        return node
                except Exception:
                    continue
        return None

    def showing(node):
        try:
            return node.get_state_set().contains(Atspi.StateType.SHOWING)
        except Exception:
            return False

    def dialog_of(node):
        while node is not None and node.get_role_name() != "dialog":
            node = node.get_parent()
        return node

    added_found = []
    menu = named("toggle button", "Main Menu")
    if menu is None or not menu.get_action_iface().do_action(0):
        print("The main menu would not open for Add Account.", file=sys.stderr)
        return 1
    # A popover's items answer do_action without the SHOWING state.
    wait_until(lambda: named("menu item", "Add Account…") is not None, 3.0)
    item = named("menu item", "Add Account…")
    if item is None:
        print("The main menu has no Add Account item.", file=sys.stderr)
        close(keys)
        return 1
    item.get_action_iface().do_action(0)
    google = "Google, Gmail, Workspace, signs in through your browser"
    if not wait_until(lambda: (lambda n: n is not None and showing(n))(named("button", google)), 10.0):
        print("Add Account showed no provider tiles by name.", file=sys.stderr)
        return 1
    time.sleep(1.0)
    dialog = dialog_of(named("button", google))
    walk(dialog, ["Add Account, tiles"], added_found)
    other = named("button", "Other, Any server")
    missing = 0
    if other is None:
        print("Add Account has no Other tile by name.", file=sys.stderr)
        missing += 1
    else:
        other.get_action_iface().do_action(0)
        if wait_until(lambda: (lambda n: n is not None and showing(n))(named("text", "Email Address")), 10.0):
            time.sleep(1.0)
            walk(dialog, ["Add Account, Other"], added_found)
        else:
            print("The Other tile opened no address page.", file=sys.stderr)
            missing += 1
    for _ in range(3):
        keys.escape()
        time.sleep(0.4)
        if named("button", google) is None:
            break
    unnamed = [row for row in added_found if not row[1]]
    print("add account: %d controls on the tiles and the Other page, %d unnamed"
          % (len(added_found), len(unnamed)))
    for role, _, path in unnamed:
        print("  %s" % path)
    return len(unnamed) + missing


calendar_unnamed = conversation_unnamed = composer_unnamed = added_unnamed = 0
if sys.argv[1:] == ["--menus"]:
    open_menus()
    conversation_unnamed = walk_conversation(Input())
    composer_unnamed = walk_composer()
    added_unnamed = walk_add_account(Input())
    calendar_unnamed = walk_calendar(Input())

unnamed = [row for row in found if not row[1]]
print("%d controls, %d named, %d unnamed" % (len(found), len(found) - len(unnamed), len(unnamed)))
for role, _, path in unnamed:
    print("  %s" % path)
sys.exit(1 if (unnamed or calendar_unnamed or conversation_unnamed or composer_unnamed or added_unnamed) else 0)
PYTHON

if [ "$here" = --here ]; then
    exec python3 "$walk"
fi

if [ -n "$here" ]; then
    echo "a11y-names.sh takes --here or nothing" >&2
    exit 2
fi

for tool in Xvfb dbus-run-session python3; do
    if ! command -v "$tool" >/dev/null; then
        echo "a11y-names.sh needs $tool; see the comment at the top" >&2
        exit 2
    fi
done
registry=/usr/libexec/at-spi2-registryd
launcher=/usr/libexec/at-spi-bus-launcher
if [ ! -x "$registry" ] || [ ! -x "$launcher" ]; then
    echo "a11y-names.sh needs at-spi2-core installed" >&2
    exit 2
fi

cargo build --quiet -p mailrs

# The demo store, so the run touches no account and no real mail. Its own
# home and runtime directory keep the settings of the copy you use out of
# it, and a memory backend keeps this run's out of yours.
sandbox=$(mktemp -d)
trap 'rm -f "$walk"; take_down; rm -rf "$sandbox"' EXIT

# The registry forks and leaves the pid it was started under behind, so
# killing that pid kills nothing and an orphan lives on against a display
# that has gone. Everything this run started inherited the sandbox path
# and nothing else on the machine carries it, which is what says who to
# take down without reaching the daemons of the desktop you are sitting
# at.
take_down() {
    for proc in /proc/[0-9]*; do
        pid=${proc#/proc/}
        [ "$pid" = "$$" ] && continue
        if tr '\0' '\n' 2>/dev/null <"$proc/environ" |
            grep -qxF "XDG_RUNTIME_DIR=$sandbox/run"; then
            kill "$pid" 2>/dev/null
        fi
    done
}
export HOME="$sandbox/home"
export XDG_RUNTIME_DIR="$sandbox/run"
mkdir -p "$HOME" "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"
# A GnuPG home of the run's own. With only HOME moved, gpg still takes
# $HOME/.gnupg for the default home and talks to the agent already running
# for the person's own keys, so a secret key the run imports would land in
# their keyring. A home by another name gets an agent of its own.
export GNUPGHOME="$sandbox/gnupg"
mkdir -m 700 "$GNUPGHOME"
export GSETTINGS_BACKEND=memory
export PENGUIN_MAIL_LOCALE_DIR="$PWD/target/locale"

app=$PWD/target/debug/penguin-mail

# WebKit runs its helpers, including the proxy that carries the
# accessibility bus into a page, inside bubblewrap. An unprivileged
# container, which is what CI runs in, cannot make the namespaces that
# needs, and WebKit then aborts the app the moment the walk opens a
# conversation. There, and only there, the demo runs without WebKit's
# sandbox: it shows its own sample mail with remote images blocked.
webkit=
if ! bwrap --ro-bind / / true 2>/dev/null; then
    echo "bubblewrap cannot start here, so the demo runs without WebKit's sandbox" >&2
    webkit=WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1
fi
# New events start on the demo's Google Workspace calendar, whose editor
# has the Type row the walk opens. The demo's first account plays a
# personal Gmail one, which makes events alone.
printf 'last_calendar_account = "dana@fernwood.example"\n' >"$sandbox/settings.toml"
export MAILRS_SETTINGS="$sandbox/settings.toml"
inside="
$launcher --launch-immediately &
sleep 1
$registry &
sleep 1
env $webkit $app --demo >$sandbox/app.log 2>&1 &
window=\$!
python3 $walk --menus
status=\$?
kill \$window 2>/dev/null
wait \$window 2>/dev/null
# A failed run says why the app did what it did: a crash or a warning in
# its log is often the whole answer, and CI keeps nothing else.
if [ \$status -ne 0 ]; then
    echo '--- the end of the app log ---' >&2
    tail -n 60 $sandbox/app.log >&2
fi
exit \$status
"
xvfb-run -a --server-args="-screen 0 1400x900x24" \
    dbus-run-session -- bash -c "$inside"
