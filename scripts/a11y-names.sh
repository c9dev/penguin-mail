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
# The hidden display needs Xvfb, dbus-run-session, at-spi2-core, and
# python3 with the GObject bindings:
#   sudo apt install xvfb dbus-daemon at-spi2-core python3-gi
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

# How long to wait for the window to reach the bus. A cold demo store
# takes a few seconds to fill before anything is drawn.
PATIENCE = 60

# Roles a person acts on. Everything else is scenery, and a heading or a
# label says what it says through its own text.
# Both spellings of a plain button are here: AT-SPI has called it one and
# then the other, and which one arrives is the toolkit's business.
ACTS = {
    "button", "check box", "combo box", "entry", "link", "list item",
    "menu item", "page tab", "password text", "push button",
    "radio button", "slider", "spin button", "switch", "table cell",
    "text", "toggle button",
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
    return [a for a, name in named if name.startswith("dev.penguinmail")], [
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
trap 'rm -f "$walk"; rm -rf "$sandbox"' EXIT
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
python3 $walk
status=\$?
kill \$window 2>/dev/null
exit \$status
"
xvfb-run -a --server-args="-screen 0 1400x900x24" \
    dbus-run-session -- bash -c "$inside"
