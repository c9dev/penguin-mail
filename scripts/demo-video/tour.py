"""Drives the demo through a tour of Penguin Mail while GNOME Shell records it.

Run through scripts/demo-video.sh, which starts the headless GNOME Shell,
PipeWire and the tour extension in a throwaway session.

    python3 tour.py APP OUT_DIR SANDBOX [probe]

The pointer and the keyboard go through the extension's virtual devices,
so the video shows a cursor that travels to what it clicks. Controls are
found by role and accessible name, as in scripts/screenshots.py, and their
place on screen is their place in the window plus the window's frame,
which the extension reports. Each scene writes its start time to
OUT_DIR/marks.json, and compose.py puts the captions there.

With `probe`, the tour stops at each state it will visit and writes the
accessible tree to OUT_DIR/tree-NAME.txt instead of recording.
"""

import json
import math
import os
import subprocess
import sys
import time

import gi

gi.require_version("Atspi", "2.0")
gi.require_version("Gio", "2.0")
from gi.repository import Atspi, Gio, GLib  # noqa: E402

APP, OUT, SANDBOX = sys.argv[1:4]
PROBE = sys.argv[4:5] == ["probe"]
HERE = os.path.dirname(os.path.abspath(__file__))
APP_ID = "io.github.c9dev.PenguinMail.Demo"

# Where the main window sits on the 1920x1080 monitor: clear of the top
# bar, with room below it for the captions.
WINDOW = (180, 118, 1560, 840)

bus = Gio.bus_get_sync(Gio.BusType.SESSION)


def tour(method, args=None, reply=None):
    """Calls the extension."""
    result = bus.call_sync(
        "org.gnome.Shell",
        "/dev/penguinmail/Tour",
        "dev.penguinmail.Tour",
        method,
        args,
        GLib.VariantType(reply) if reply else None,
        Gio.DBusCallFlags.NONE,
        -1,
        None,
    )
    return result.unpack() if result is not None else None


# ---- Pointer and keyboard -------------------------------------------------

BTN_LEFT, BTN_RIGHT = 1, 3
pointer = [960.0, 540.0]


def ease(t):
    return 0.5 - math.cos(math.pi * t) / 2


def glide(x, y, seconds=None):
    """Moves the pointer to (x, y) along a slight curve, fast in the middle
    and slow at both ends, as a hand moves a mouse."""
    x0, y0 = pointer
    distance = math.hypot(x - x0, y - y0)
    if seconds is None:
        seconds = min(0.95, 0.35 + distance / 2200)
    steps = max(2, int(seconds * 60))
    # A bow of a few percent of the distance, to one side.
    bow = distance * 0.06
    nx, ny = (-(y - y0) / distance, (x - x0) / distance) if distance else (0, 0)
    start = time.monotonic()
    for step in range(1, steps + 1):
        t = ease(step / steps)
        lift = math.sin(math.pi * t) * bow
        px = x0 + (x - x0) * t + nx * lift
        py = y0 + (y - y0) * t + ny * lift
        tour("Pointer", GLib.Variant("(dd)", (px, py)))
        target = start + step / 60
        delay = target - time.monotonic()
        if delay > 0:
            time.sleep(delay)
    pointer[:] = [x, y]


def press(button=BTN_LEFT):
    tour("Button", GLib.Variant("(ub)", (button, True)))
    time.sleep(0.07)
    tour("Button", GLib.Variant("(ub)", (button, False)))


KEYVALS = {
    "ctrl": 0xFFE3,
    "shift": 0xFFE1,
    "alt": 0xFFE9,
    "super": 0xFFEB,
    "Return": 0xFF0D,
    "Escape": 0xFF1B,
    "Tab": 0xFF09,
    "BackSpace": 0xFF08,
    "Down": 0xFF54,
    "Up": 0xFF52,
    "Left": 0xFF51,
    "Right": 0xFF53,
    "Delete": 0xFFFF,
}


def keyval(name):
    if name in KEYVALS:
        return KEYVALS[name]
    if len(name) == 1:
        code = ord(name)
        return code if code < 0x100 else 0x01000000 + code
    raise ValueError("no key named %r" % name)


def key(combo):
    """Presses a combination such as "ctrl+j" or "Escape"."""
    values = [keyval(part) for part in combo.split("+")]
    for value in values:
        tour("Key", GLib.Variant("(ub)", (value, True)))
    time.sleep(0.05)
    for value in reversed(values):
        tour("Key", GLib.Variant("(ub)", (value, False)))
    time.sleep(0.25)


def type_text(text, pace=0.045):
    """Types as a person does: a steady rhythm, a little slower after a
    word and a sentence."""
    for char in text:
        value = keyval("Return" if char == "\n" else char)
        tour("Key", GLib.Variant("(ub)", (value, True)))
        tour("Key", GLib.Variant("(ub)", (value, False)))
        pause = pace
        if char == " ":
            pause *= 1.6
        elif char in ".,?!\n":
            pause *= 4
        time.sleep(pause)


# ---- The accessibility tree -----------------------------------------------


def app_node():
    desktop = Atspi.get_desktop(0)
    for index in range(desktop.get_child_count()):
        child = desktop.get_child_at_index(index)
        if child is not None and (child.get_name() or "").startswith("io.github.c9dev"):
            return child
    return None


def showing(node):
    try:
        return node.get_state_set().contains(Atspi.StateType.SHOWING)
    except Exception:
        return False


def find_all(node, role=None, name=None, contains=None):
    found = []

    def walk(n):
        try:
            r = n.get_role_name()
            label = (n.get_name() or "").strip()
        except Exception:
            return
        if (role is None or r == role) and (name is None or label == name) and (
            contains is None or contains in label
        ):
            found.append(n)
        for index in range(n.get_child_count()):
            child = n.get_child_at_index(index)
            if child is not None:
                walk(child)

    if node is not None:
        walk(node)
    return found


def find(role=None, name=None, contains=None, patience=30, within=None):
    deadline = time.time() + patience
    while time.time() < deadline:
        for hit in find_all(within or app_node(), role, name, contains):
            if showing(hit):
                return hit
        time.sleep(0.3)
    dump("failed")
    raise SystemExit("nothing showing with role %r and name %r" % (role, name or contains))


def window_of(node):
    """The title of the top-level window that holds `node`."""
    while node is not None:
        try:
            if node.get_role_name() in ("frame", "window", "dialog") and node.get_parent().get_role_name() == "application":
                return node.get_name()
        except Exception:
            return None
        node = node.get_parent()
    return None


def extents(node):
    return Atspi.Component.get_extents(node, Atspi.CoordType.WINDOW)


def web_offset(node):
    """WebKit reports what is inside a message relative to the web view,
    not the window. The web view's own place is that of the first widget
    above the web content that sits away from the corner."""
    ancestor = node
    while ancestor is not None and ancestor.get_role_name() != "document web":
        ancestor = ancestor.get_parent()
    if ancestor is None:
        return 0, 0
    ancestor = ancestor.get_parent()
    while ancestor is not None:
        e = extents(ancestor)
        if (e.x, e.y) != (0, 0):
            return e.x, e.y
        ancestor = ancestor.get_parent()
    return 0, 0


def centre(node, dx=0.5, dy=0.5):
    """Where `node` is on the screen, at a fraction of its size."""
    e = extents(node)
    ox, oy = web_offset(node)
    title = window_of(node) or "Penguin Mail"
    fx, fy, _, _ = tour("Frame", GLib.Variant("(s)", (title,)), "((iiii))")[0]
    return fx + ox + e.x + e.width * dx, fy + oy + e.y + e.height * dy


def click(role=None, name=None, contains=None, button=BTN_LEFT, dx=0.5, dy=0.5, node=None, linger=0.25):
    node = node or find(role, name, contains)
    x, y = centre(node, dx, dy)
    glide(x, y)
    time.sleep(linger)
    press(button)
    return node


def hover(role=None, name=None, contains=None, dx=0.5, dy=0.5, node=None):
    node = node or find(role, name, contains)
    glide(*centre(node, dx, dy))
    return node


def dump(label):
    path = os.path.join(OUT, "tree-%s.txt" % label)
    with open(path, "w") as out:

        def walk(n, depth):
            try:
                role, name = n.get_role_name(), n.get_name()
            except Exception:
                return
            try:
                e = Atspi.Component.get_extents(n, Atspi.CoordType.WINDOW)
                where = "[%d,%d %dx%d]" % (e.x, e.y, e.width, e.height)
            except Exception:
                where = ""
            out.write("%s%s %r %s %s\n" % ("  " * depth, role, name, "S" if showing(n) else "-", where))
            for index in range(n.get_child_count()):
                child = n.get_child_at_index(index)
                if child is not None:
                    walk(child, depth + 1)

        walk(app_node(), 0)
    print("wrote", path, flush=True)


# ---- Recording ------------------------------------------------------------

recorder = None
started = None
marks = []


def start_recording(name="raw.mkv"):
    """Starts recording and waits for the first frames."""
    global recorder, started
    recorder = subprocess.Popen(
        ["python3", os.path.join(HERE, "record.py"), os.path.join(OUT, name)],
        stdout=subprocess.PIPE,
        text=True,
    )
    if recorder.stdout.readline().strip() != "recording":
        raise SystemExit("the recorder did not start")
    started = time.monotonic()


def stop_recording(name="raw.mkv"):
    recorder.send_signal(subprocess.signal.SIGINT)
    recorder.wait(timeout=120)
    path = os.path.join(OUT, name)
    if not os.path.exists(path) or os.path.getsize(path) == 0:
        raise SystemExit("the recorder wrote nothing to %s" % path)
    with open(os.path.join(OUT, "marks.json"), "w") as out:
        json.dump(marks, out, indent=1)


def mark(caption, detail=""):
    """Notes that a scene starts now, with the caption compose.py shows
    over it."""
    marks.append({"at": round(time.monotonic() - started, 2), "caption": caption, "detail": detail})
    print("scene", marks[-1], flush=True)


# ---- The app --------------------------------------------------------------


def settings_file(extra=""):
    path = os.path.join(SANDBOX, "settings.toml")
    with open(path, "w") as out:
        out.write(extra)
    return path


def launch_directly(settings=""):
    environment = dict(os.environ, MAILRS_SETTINGS=settings_file(settings))
    log = open(os.path.join(SANDBOX, "app.log"), "a")
    return subprocess.Popen([APP, "--demo"], env=environment, stdout=log, stderr=log)


def wait_for_window(words="Penguin Mail", patience=60):
    deadline = time.time() + patience
    while time.time() < deadline:
        if any(words in title for title in tour("Titles", None, "(as)")[0]):
            return
        time.sleep(0.2)
    raise SystemExit("no window titled %r" % words)


def place_main_window():
    tour("Place", GLib.Variant("(siiii)", ("Penguin Mail", *WINDOW)))
    find("list item", contains="Saturday hike?", patience=60)


def probe():
    start_recording("probe.mkv")
    run = launch_directly()
    wait_for_window()
    key("Escape")
    time.sleep(1)
    place_main_window()
    time.sleep(2)
    dump("main")
    click("list item", contains="Saturday hike?", button=BTN_RIGHT)
    time.sleep(1.5)
    dump("row-menu")
    key("Escape")
    time.sleep(0.5)
    click("list item", contains="Saturday hike?")
    time.sleep(3)
    dump("thread")
    click("list item", contains="Updated invitation: Sprint planning")
    time.sleep(3)
    dump("invitation")
    click("list item", contains="Five autumn loops")
    time.sleep(3)
    dump("newsletter")
    key("ctrl+j")
    time.sleep(2)
    dump("assistant")
    key("ctrl+j")
    time.sleep(1)
    click("list item", contains="Saturday hike?")
    time.sleep(2)
    key("r")
    time.sleep(3)
    dump("composer")
    stop_recording("probe.mkv")
    run.terminate()


# ---- The tour -------------------------------------------------------------

sys.path.insert(0, os.path.dirname(HERE))
import scripted_model  # noqa: E402

# Word by word, at the pace a person reads along.
scripted_model.PACE = 0.035


def hold(seconds):
    time.sleep(seconds)


def glide_over(*nodes, pause=0.35):
    """Runs the pointer over several things in turn, as an eye would."""
    for node in nodes:
        hover(node=node)
        hold(pause)


def row(subject):
    return find("list item", contains=subject)


def set_width(width, seconds=1.4):
    """Narrows or widens the main window around its centre, in steps."""
    x, y, w, h = WINDOW
    middle = x + w / 2
    _, _, current, _ = tour("Frame", GLib.Variant("(s)", ("Penguin Mail",)), "((iiii))")[0]
    steps = int(seconds * 30)
    for step in range(1, steps + 1):
        now = current + (width - current) * ease(step / steps)
        tour("Place", GLib.Variant("(siiii)", ("Penguin Mail", int(middle - now / 2), y, int(now), h)))
        time.sleep(1 / 30)


def run_tour():
    settings = '[ai]\nprovider = "local"\nbase_url = "%s"\nlocal_model = "%s"\n' % (
        scripted_model.scripted_model(),
        scripted_model.MODEL,
    )
    app = launch_directly(settings)
    wait_for_window()
    key("Escape")
    hold(1)
    place_main_window()
    glide(960, 600, 0.2)
    hold(2)
    # The tour opens in the overview, with the window waiting in it.
    key("super")
    hold(1.5)
    start_recording()
    hold(1.2)

    mark("Penguin Mail", "A Gmail client for GNOME, written in Rust")
    x, y, w, h = WINDOW
    glide(960, 560)
    hold(0.4)
    press()
    hold(2.2)

    mark("Every account, one inbox", "Your Gmail accounts sync in the background and meet in one list")
    glide_over(
        find("list item", contains="dana.reyes@example.com"),
        find("list item", contains="All Inboxes"),
        row("Saturday hike?"),
        row("Q4 roadmap review"),
        row("Thesis chapter 3 feedback"),
        pause=0.5,
    )
    hold(0.6)

    mark("Gmail's categories, a click away", "Primary, Updates, Promotions and Social, from Gmail itself")
    for name in ("Primary", "Promotions", "Social", "All"):
        click("radio button", contains=name)
        hold(1.3)

    mark("Conversations, not piles of mail", "Older messages fold away, quotes and signatures dim")
    click(node=row("Saturday hike?"))
    hold(1.8)
    click("link", name="Show or hide this message")
    hold(2.2)

    mark("Right-click anything", "Every action on a message sits one click away")
    click(node=row("Q4 roadmap review"), button=BTN_RIGHT)
    menu = find("menu")
    items = [item for item in find_all(menu, role="menu item") if showing(item)]
    glide_over(*items[:7], pause=0.18)
    # Flag or Unflag, the seventh entry.
    click(node=items[6])
    hold(1.6)

    mark("Invitations, answered in place", "Accept, decline or propose a new time without leaving your mail")
    click(node=row("Updated invitation: Sprint planning"))
    hold(1.8)
    click("toggle button", name="Yes")
    hold(1.4)
    offer = find_all(app_node(), role="button", name="Not Now")
    if offer and showing(offer[0]):
        click(node=offer[0])
    hold(1.2)

    mark("Private by default", "Remote images stay blocked until you allow them")
    click(node=row("Your September statement is ready"))
    hold(2.4)
    load = find_all(app_node(), role="button", name="Load Images")
    if load and showing(load[0]):
        hover(node=load[0])
    hold(1.6)
    click(node=row("Five autumn loops"))
    hold(1.8)
    hover("button", name="Unsubscribe")
    hold(1.4)

    mark("Select many, act once, undo anything", "Ctrl+click, act on them all, and Ctrl+Z takes it back")
    click(node=row("Thesis chapter 3 feedback"))
    hold(0.9)
    tour("Key", GLib.Variant("(ub)", (KEYVALS["ctrl"], True)))
    for subject in ("Updated invitation: Sprint planning", "Invitation: Offline editor design review"):
        click(node=row(subject))
        hold(0.6)
    tour("Key", GLib.Variant("(ub)", (KEYVALS["ctrl"], False)))
    find(contains="3 Conversations Selected")
    hold(1.4)
    # The selection page's own Archive button, the wide one, rather than
    # the toolbar's icon.
    archive = max(
        (node for node in find_all(app_node(), role="button", name="Archive") if showing(node)),
        key=lambda node: extents(node).width,
    )
    click(node=archive)
    hold(1.6)
    click("button", name="Undo")
    hold(2)

    mark("Write, then send when it suits", "Undo Send, Send Later and a composer that shows its formatting")
    click(node=row("Saturday hike?"))
    hold(1.5)
    click("button", name="Reply")
    wait_for_window("Re: Saturday hike?")
    hold(1.5)
    type_text("Count me in! I'll bring the trail map and a flask of coffee.")
    hold(0.8)
    click("toggle button", name="Send Later")
    menu = find("menu", name="Send Later")
    items = [item for item in find_all(menu, role="menu item") if showing(item)]
    glide_over(*items[:3], pause=0.25)
    click(node=items[-2])
    hold(2)

    mark("An assistant that shows its work", "Local model or Claude, and it asks before it acts")
    key("ctrl+j")
    hold(1)
    type_text(scripted_model.QUESTION + "\n", pace=0.03)
    find(contains="You have not opened it yet", patience=90)
    hold(3.5)
    key("ctrl+j")
    hold(1)

    mark("Gmail search, with suggestions", "The full query syntax, across one account or all")
    click("toggle button", name="Search")
    hold(0.6)
    type_text("from:priya has:attachment", pace=0.06)
    hold(0.6)
    key("Return")
    hold(2.2)
    key("Escape")
    hold(1)

    mark("At home on GNOME", "It follows your desktop into dark mode and back")
    subprocess.run(["gsettings", "set", "org.gnome.desktop.interface", "color-scheme", "prefer-dark"], check=True)
    hold(3.2)

    mark("From widescreen to phone size", "The layout folds down as the window narrows")
    set_width(430)
    hold(2.4)
    set_width(WINDOW[2])
    hold(1.5)

    stop_recording()
    app.terminate()


if __name__ == "__main__":
    if PROBE:
        probe()
    else:
        run_tour()
