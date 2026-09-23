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
# menu is in the accessible tree only while it is open.
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


def walk(node, path):
    """Records every control under `node`, and says whether a named
    control was found in there.

    A control that holds another named control is a wrapper rather than
    something a person acts on, and wanting a name of its own would be
    asking for the same words twice: a list view puts every row inside a
    list item of its own, and the row is what carries the name."""
    try:
        role = node.get_role_name()
        name = (node.get_name() or "").strip()
    except Exception:
        # A window that closed mid-walk takes its children with it.
        return False
    here = path + ["%s %r" % (role, name) if name else role]
    wraps = False
    for index in range(node.get_child_count()):
        child = node.get_child_at_index(index)
        if child is not None and walk(child, here):
            wraps = True
    if role in ACTS and (name or not wraps):
        found.append((role, name, " > ".join(here[-4:])))
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

    def scroll_down(self, x, y):
        self.xtst.XTestFakeMotionEvent(self.display, -1, x, y, 0)
        for _ in range(8):
            for pressed in (1, 0):
                self.xtst.XTestFakeButtonEvent(self.display, 5, pressed, 0)
            self.x11.XFlush(self.display)
            time.sleep(0.02)

    def escape(self):
        code = self.x11.XKeysymToKeycode(self.display, 0xFF1B)
        for pressed in (1, 0):
            self.xtst.XTestFakeKeyEvent(self.display, code, pressed, 0)
            self.x11.XFlush(self.display)
            time.sleep(0.05)


menus_seen = set()


def record(opener):
    """Adds the items of every menu showing to `found`, once for each
    distinct menu, and gives back the items that open a submenu."""
    submenus = []
    for menu in showing_menus():
        items = [n for n in nodes(menu) if n.get_role_name() in ACTS]
        said = tuple((n.get_role_name(), (n.get_name() or "").strip()) for n in items)
        if said in menus_seen:
            continue
        menus_seen.add(said)
        for index, (role, name) in enumerate(said):
            found.append((role, name, "menu from %s > %s %d" % (opener, role, index + 1)))
        submenus += [
            name for n, (_, name) in zip(items, said)
            if name and n.get_state_set().contains(Atspi.StateType.HAS_POPUP)
        ]
    return submenus


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


def in_view(node, x, y):
    """Whether the point lies inside every scrolled view that holds
    `node`, in window coordinates."""
    parent = node.get_parent()
    while parent is not None:
        if parent.get_role_name() == "scroll pane":
            box = parent.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
            if not (box.x <= x < box.x + box.width and box.y <= y < box.y + box.height):
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
            if in_view(row, x, y):
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


if sys.argv[1:] == ["--menus"]:
    open_menus()

unnamed = [row for row in found if not row[1]]
print("%d controls, %d named, %d unnamed" % (len(found), len(found) - len(unnamed), len(unnamed)))
for role, _, path in unnamed:
    print("  %s" % path)
sys.exit(1 if unnamed else 0)
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
export GSETTINGS_BACKEND=memory
export PENGUIN_MAIL_LOCALE_DIR="$PWD/target/locale"

app=$PWD/target/debug/penguin-mail
inside="
$launcher --launch-immediately &
sleep 1
$registry &
sleep 1
$app --demo >$sandbox/app.log 2>&1 &
window=\$!
python3 $walk --menus
status=\$?
kill \$window 2>/dev/null
wait \$window 2>/dev/null
exit \$status
"
xvfb-run -a --server-args="-screen 0 1400x900x24" \
    dbus-run-session -- bash -c "$inside"
