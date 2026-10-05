"""Drives the demo into each README screenshot and saves it.

Run through scripts/screenshots.sh, which provides the hidden display, the
session bus, and the sandboxed home directory this expects.

    python3 screenshots.py APP OUT_DIR SANDBOX [SHOT...]

Each shot is a function below that starts the app, reaches one state, and
saves one window. Controls are found by role and accessible name, and
worked through their accessibility actions; the keyboard is only for
typing and shortcuts. A shot that cannot find what it wants saves every
window as target/failed-N.png and stops, and dump() writes the whole
accessible tree out when a new shot needs names to look for.
"""

import os
import subprocess
import sys
import time

import gi

gi.require_version("Atspi", "2.0")
from gi.repository import Atspi  # noqa: E402
from Xlib import X, XK, display  # noqa: E402
from Xlib.ext import xtest  # noqa: E402

APP, OUT, SANDBOX = sys.argv[1:4]
WANTED = sys.argv[4:]
# Where a failed shot leaves its evidence, out of the tracked tree.
TARGET = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "target")

xdisplay = display.Display()
root = xdisplay.screen().root


# ---- Windows --------------------------------------------------------------


def top_levels():
    """Every mapped window on the display, as (id, title, width, height)."""
    found = []
    for window in root.query_tree().children:
        try:
            if window.get_attributes().map_state != X.IsViewable:
                continue
            geometry = window.get_geometry()
            title = window.get_wm_name() or ""
        except Exception:
            continue
        if isinstance(title, bytes):
            title = title.decode("utf-8", "replace")
        found.append((window, title, geometry.width, geometry.height))
    return found


def window_titled(words, patience=90):
    """Waits for a window whose title holds `words` and returns it."""
    deadline = time.time() + patience
    while time.time() < deadline:
        for window, title, _, _ in top_levels():
            if words in title:
                return window
        time.sleep(0.5)
    raise SystemExit("no window titled %r; open: %r" % (words, [t for _, t, _, _ in top_levels()]))


def resize(window, width, height):
    """Sets the window's size. With no window manager on the display, the
    request goes straight through."""
    window.configure(x=0, y=0, width=width, height=height)
    xdisplay.sync()


def focus(window):
    """Gives a window the keyboard. With no window manager, nothing else
    will."""
    window.set_input_focus(X.RevertToParent, X.CurrentTime)
    xdisplay.sync()
    time.sleep(0.3)


def capture(window, name, nudge=True, whole_screen=False):
    # GTK redraws only what changed, and on this display that sometimes
    # leaves a stray glyph behind where bold text turned regular. A width
    # nudge makes it draw the whole window again.
    geometry = window.get_geometry()
    if nudge:
        window.configure(width=geometry.width + 1)
        xdisplay.sync()
        time.sleep(0.8)
        window.configure(width=geometry.width)
        xdisplay.sync()
    time.sleep(1.5)
    path = os.path.join(OUT, name + ".png")
    # Without the time chunks, a shot whose pixels did not change writes
    # the same bytes as before.
    # A popover is a window of its own on X11, which a shot of the main
    # window leaves out. The screen holds it, cropped to the window's size.
    geometry = window.get_geometry()
    if whole_screen:
        target = ["-window", "root", "-crop", "%dx%d+0+0" % (geometry.width, geometry.height), "+repage"]
    else:
        target = ["-window", str(window.id)]
    subprocess.run(
        ["import", *target, "-strip", "-define", "png:exclude-chunks=date,time", path],
        check=True,
    )
    print("saved", path)


# ---- Input ----------------------------------------------------------------


def key(combo):
    """Presses a combination such as "ctrl+shift+l" or "Escape"."""
    names = combo.split("+")
    codes = []
    for name in names:
        name = {"ctrl": "Control_L", "shift": "Shift_L", "alt": "Alt_L"}.get(name, name)
        keysym = XK.string_to_keysym(name)
        if not keysym:
            raise ValueError("no key named %r" % name)
        codes.append(xdisplay.keysym_to_keycode(keysym))
    for code in codes:
        xtest.fake_input(xdisplay, X.KeyPress, code)
    for code in reversed(codes):
        xtest.fake_input(xdisplay, X.KeyRelease, code)
    xdisplay.sync()
    time.sleep(0.3)


def type_text(text):
    for char in text:
        keysym = XK.string_to_keysym(char) if len(char) > 1 else ord(char)
        if char == " ":
            keysym = XK.XK_space
        elif char == "\n":
            keysym = XK.XK_Return
        code = xdisplay.keysym_to_keycode(keysym)
        shift = char.isupper() or char in '~!@#$%^&*()_+{}|:"<>?'
        if shift:
            xtest.fake_input(xdisplay, X.KeyPress, xdisplay.keysym_to_keycode(XK.XK_Shift_L))
        xtest.fake_input(xdisplay, X.KeyPress, code)
        xtest.fake_input(xdisplay, X.KeyRelease, code)
        if shift:
            xtest.fake_input(xdisplay, X.KeyRelease, xdisplay.keysym_to_keycode(XK.XK_Shift_L))
        xdisplay.sync()
        time.sleep(0.02)
    time.sleep(0.3)


def scroll_down(x, y, clicks, button=5):
    """Turns the wheel down over a point, one notch a click. Button 4 turns
    it up."""
    xtest.fake_input(xdisplay, X.MotionNotify, x=x, y=y)
    xdisplay.sync()
    time.sleep(0.2)
    for _ in range(clicks):
        xtest.fake_input(xdisplay, X.ButtonPress, button)
        xtest.fake_input(xdisplay, X.ButtonRelease, button)
        xdisplay.sync()
        time.sleep(0.08)
    time.sleep(0.5)


def click(x, y):
    """Presses the left button at a point on the screen."""
    xtest.fake_input(xdisplay, X.MotionNotify, x=x, y=y)
    xdisplay.sync()
    time.sleep(0.2)
    xtest.fake_input(xdisplay, X.ButtonPress, 1)
    xtest.fake_input(xdisplay, X.ButtonRelease, 1)
    xdisplay.sync()
    time.sleep(0.5)


def scroll_to_morning():
    """Puts the time grid's top edge near 07:00. The grid opens at the
    current hour, so go to the top first."""
    scroll_down(900, 500, 20, button=4)
    scroll_down(900, 500, 5)


def park_pointer():
    """Moves the pointer off the window, so no hover shows in the shot."""
    xtest.fake_input(xdisplay, X.MotionNotify, x=1590, y=990)
    xdisplay.sync()


# ---- The accessibility tree -----------------------------------------------


def failed():
    """Saves every window as target/failed-N.png, to show where a shot
    went wrong."""
    for number, (window, _, _, _) in enumerate(top_levels()):
        path = os.path.join(TARGET, "failed-%d.png" % number)
        subprocess.run(["import", "-window", str(window.id), path], check=False)
        print("saw", path)


def app_node():
    desktop = Atspi.get_desktop(0)
    for index in range(desktop.get_child_count()):
        child = desktop.get_child_at_index(index)
        if child is not None and (child.get_name() or "").startswith("io.github.c9dev"):
            return child
    return None


def find_all(node, role=None, name=None, contains=None):
    """Every node under `node` with the role and name asked for."""
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

    walk(node)
    return found


def find(role=None, name=None, contains=None, patience=30):
    """Waits for one showing node with the role and name asked for."""
    deadline = time.time() + patience
    while time.time() < deadline:
        node = app_node()
        if node is not None:
            for hit in find_all(node, role, name, contains):
                if showing(hit):
                    return hit
        time.sleep(0.5)
    failed()
    raise SystemExit("nothing showing with role %r and name %r" % (role, name or contains))


def showing(node):
    try:
        return node.get_state_set().contains(Atspi.StateType.SHOWING)
    except Exception:
        return False


def act(role=None, name=None, contains=None, patience=30):
    """Runs the default action of a control found by role and name: a
    click for a button, a toggle for a switch. A row that wraps a switch
    can carry the same role and name with no action of its own, so this
    takes the first match that has one."""
    deadline = time.time() + patience
    while time.time() < deadline:
        node = app_node()
        for hit in find_all(node, role, name, contains) if node else []:
            if not showing(hit):
                continue
            try:
                if Atspi.Action.get_n_actions(hit) > 0:
                    Atspi.Action.do_action(hit, 0)
                    time.sleep(0.6)
                    return hit
            except Exception:
                continue
        time.sleep(0.5)
    failed()
    raise SystemExit("nothing to act on with role %r and name %r" % (role, name or contains))


def fill(role, name, text):
    """Puts text into a field found by role and name, through the field's
    editable text rather than the keyboard, so it needs no focus."""
    node = find(role, name)
    Atspi.EditableText.set_text_contents(node, text)
    time.sleep(0.4)


def select_rows(*positions):
    """Selects rows of the thread list by position, as Ctrl+click does."""
    rows = find("list", contains=None)
    for position in positions:
        Atspi.Selection.select_child(rows, position)
        time.sleep(0.3)


def choose(contains):
    """Selects the list row whose name holds `contains`, as a click on it
    does. The sidebar's rows take no action, only a selection."""
    row = find("list item", contains=contains)
    # A list view wraps each row in an unnamed item of its own, and the
    # selection counts those.
    while row.get_parent().get_role_name() != "list":
        row = row.get_parent()
    rows = row.get_parent()
    # The selection also counts rows that are hidden and so absent from
    # the tree, which puts the row at its place in the tree or after it.
    start = row.get_index_in_parent()
    for index in range(start, start + 40):
        Atspi.Selection.select_child(rows, index)
        time.sleep(0.3)
        chosen = Atspi.Selection.get_selected_child(rows, 0)
        if chosen is not None and contains in (chosen.get_name() or ""):
            time.sleep(0.6)
            return
    failed()
    raise SystemExit("could not select the row %r" % contains)


def dump(label):
    """Writes the accessible tree to target/tree-LABEL.txt. For working out
    what a new shot has to find."""
    path = os.path.join(TARGET, "tree-%s.txt" % label)
    with open(path, "w") as out:

        def walk(n, depth):
            try:
                out.write("%s%s %r\n" % ("  " * depth, n.get_role_name(), n.get_name()))
            except Exception:
                return
            for index in range(n.get_child_count()):
                child = n.get_child_at_index(index)
                if child is not None:
                    walk(child, depth + 1)

        walk(app_node(), 0)


# ---- The app --------------------------------------------------------------


def settings_file(extra):
    """A settings file for one run: the demo's defaults plus `extra`,
    which is TOML text."""
    path = os.path.join(SANDBOX, "settings.toml")
    with open(path, "w") as out:
        out.write(extra)
    return path


def launch(settings="", env=None, args=("--demo",)):
    # SCREENSHOTS_DARK=1 takes every shot in the dark style, for material
    # such as the promo video that wants both.
    if os.environ.get("SCREENSHOTS_DARK") and DARK not in settings:
        settings = DARK + settings
    environment = dict(os.environ)
    environment["MAILRS_DEMO_FULL_CONSENT"] = "1"
    environment["MAILRS_SETTINGS"] = settings_file(settings)
    environment.update(env or {})
    # Only the app gets the shifted clock, not the driver or the display.
    if os.environ.get("DEMO_CLOCK_PRELOAD"):
        environment["LD_PRELOAD"] = os.environ["DEMO_CLOCK_PRELOAD"]
    log = open(os.path.join(SANDBOX, "app.log"), "a")
    return subprocess.Popen([APP, *args], env=environment, stdout=log, stderr=log)


def settle(seconds=3):
    """Gives the window time to draw what the last step changed."""
    park_pointer()
    time.sleep(seconds)


def hide_nudge():
    """Closes the bar that offers to review unanswered sent mail, so the
    list starts at its first row. It returns on the next launch. A window
    that opens on another space has no such bar, so a miss is not an
    error."""
    deadline = time.time() + 6
    while time.time() < deadline:
        node = app_node()
        for hit in find_all(node, "button", "Hide Until Next Launch") if node else []:
            if showing(hit):
                Atspi.Action.do_action(hit, 0)
                time.sleep(0.6)
                return
        time.sleep(0.5)


def main_window():
    window = window_titled("Penguin Mail")
    resize(window, 1440, 900)
    # The window is on the bus a moment after it maps, and the mailboxes
    # fill a moment after that.
    find("list item", contains="Q4 roadmap review", patience=90)
    focus(window)
    hide_nudge()
    settle(2)
    return window


# ---- The shots ------------------------------------------------------------

DARK = 'color_scheme = "dark"\n'


def inbox():
    run = launch(env={"MAILRS_DEMO_OPEN": "t-roadmap"})
    window = main_window()
    settle(5)
    capture(window, "inbox")
    return run


def dark():
    run = launch(DARK, env={"MAILRS_DEMO_OPEN": "t-roadmap"})
    window = main_window()
    # WebKit draws the HTML a beat after the conversation opens.
    settle(5)
    capture(window, "dark")
    return run


def phone():
    run = launch()
    window = main_window()
    # Narrow first, so the shot shows the phone layout from the start.
    resize(window, 400, 800)
    settle(3)
    # The third row, the roadmap reply, whose quoted history is folded.
    click(200, 350)
    settle(6)
    capture(window, "phone")
    return run


def preferences():
    run = launch(env={"MAILRS_DEMO_ACTION": "preferences"})
    window = main_window()
    find("page tab", name="General")
    act("page tab", contains="Calendar")
    settle(2)
    capture(window, "preferences")
    return run


def welcome():
    # Without --demo and with no account, the app opens on the page that
    # adds the first one. The sandboxed home holds no account, and nothing
    # presses the button, so nothing reaches Google.
    run = launch(args=())
    window = window_titled("Penguin Mail")
    resize(window, 1440, 900)
    find(name="Google, Gmail, Workspace, signs in through your browser", patience=60)
    settle(3)
    capture(window, "welcome")
    return run


def composer():
    run = launch(env={"MAILRS_DEMO_OPEN": "t-roadmap", "MAILRS_DEMO_COMPOSE": "reply"})
    main_window()
    window = window_titled("Re: Q4 roadmap review")
    focus(window)
    time.sleep(2)
    type_text("Thanks, Priya. I checked Jonas's estimate: last-write-wins is fine for October, and I can support it.")
    settle(2)
    capture(window, "composer")
    return run


def selection():
    run = launch()
    window = main_window()
    select_rows(2, 3, 5)
    settle(2)
    capture(window, "selection")
    return run


def flags():
    run = launch(env={"MAILRS_DEMO_OPEN": "t-hike"})
    window = main_window()
    find("label", name="Saturday hike?")
    time.sleep(2)
    # Ctrl+Alt+5 is the fifth colour, blue.
    key("ctrl+alt+5")
    # The confirmation toast stays a few seconds; the shot waits it out.
    settle(10)
    capture(window, "flags")
    return run


def vips():
    run = launch(env={"MAILRS_DEMO_OPEN": "t-hike", "MAILRS_DEMO_ACTION": "toggle-vip"})
    window = main_window()
    find("label", name="VIPs")
    # The toast that confirms the change stays a few seconds.
    settle(10)
    capture(window, "vips")
    return run


def automatic_reply():
    run = launch(env={"MAILRS_DEMO_ACTION": "account-vacation(int64 1)"})
    window = main_window()
    find("dialog", name="Automatic Reply")
    act("switch", name="Send Automatic Replies")
    act("switch", name="Only Between These Dates")
    fill("text", "Subject", "Out of office")
    fill("text", "Message", "I'm walking in the hills until Monday, away from mail.")
    settle(2)
    capture(window, "automatic-reply")
    return run


def rules():
    run = launch(env={"MAILRS_DEMO_ACTION": "account-rules(int64 1)"})
    window = main_window()
    act("button", name="New Rule")
    fill("text", "From", "hello@trailnotes.example")
    act("switch", name="Skip the Inbox")
    act("switch", name="Mark as Read")
    act("button", name="Create")
    find(contains="hello@trailnotes.example", role="list item")
    # The "Rule added" toast stays a few seconds; the shot waits it out.
    settle(10)
    capture(window, "rules")
    return run


def hide_my_email():
    run = launch(env={"MAILRS_DEMO_ACTION": "account-hide-my-email(int64 1)"})
    window = main_window()
    fill("text", "Where Did You Use It?", "Bike shop")
    act("button", name="Create")
    act("button", name="Done")
    find(contains="Bike shop")
    settle(2)
    capture(window, "hide-my-email")
    return run


def send_later():
    run = launch(env={"MAILRS_DEMO_ACTION": "compose"})
    main_window()
    composer = window_titled("New Message")
    focus(composer)
    time.sleep(2)
    # A new message opens with the cursor in To, and Return makes the
    # address a chip.
    type_text("mara.okafor@example.org\n")
    fill("text", "Subject", "Trail maps")
    fill("text", "Message", "Here are the maps for Saturday.")
    act("toggle button", name="Send Later")
    # The menu's items have no accessible names. The presets come first
    # and end on next Monday at 08:00 (tomorrow's 08:00 on a Sunday, the
    # same moment); Choose a Time follows.
    menu = find("menu", name="Send Later")
    items = find_all(menu, role="menu item")
    Atspi.Action.do_action(items[-2], 0)
    window = main_window()
    choose("Send Later")
    find("list item", contains="Trail maps")
    settle(3)
    capture(window, "send-later")
    return run


# ---- A scripted model for the assistant ----------------------------------

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from scripted_model import MODEL, QUESTION, scripted_model  # noqa: E402


def assistant():
    settings = '[ai]\nprovider = "local"\nbase_url = "%s"\nlocal_model = "%s"\n' % (
        scripted_model(),
        MODEL,
    )
    run = launch(settings, env={"MAILRS_DEMO_OPEN": "t-roadmap", "MAILRS_DEMO_ACTION": "assistant"})
    window = main_window()
    time.sleep(1)
    # The assistant action puts the cursor in the pane's question field.
    type_text(QUESTION + "\n")
    find(contains="You have not opened it yet", patience=60)
    settle(2)
    capture(window, "assistant")
    return run


def categories():
    run = launch()
    window = main_window()
    act("radio button", contains="Promotions")
    time.sleep(2)
    select_rows(0)
    settle(3)
    capture(window, "categories")
    return run


def outlook():
    """The demo's Outlook account: its Inbox with the Focused and Other
    tabs, the Travel tag in the sidebar, and the booking open with its
    Tags button. The sidebar's rows have no names that tell the Outlook
    Inbox from the others, so this scrolls to the foot of the list, where
    the Outlook account is the last section, and clicks where its Inbox
    row lands."""
    run = launch()
    window = main_window()
    scroll_down(150, 500, 80)
    click(94, 415)
    settle(2)
    # The Focused tab holds the booking, first in the list.
    click(480, 150)
    settle(4)
    capture(window, "outlook")
    return run


# The demo's POP3 account is the sixth and last, so its id is 6 and its
# section is at the foot of the sidebar. Its Inbox row has no name that
# tells it from the other Inboxes, so the shots click where it lands once
# the list is scrolled to the end.
POP3_ID = 6
POP3_ADDRESS = "dana@reyes-home.example"
POP3_FAILURES = {"MAILRS_DEMO_ACTION": "account-not-downloading(int64 %d)" % POP3_ID}


def pop3_sidebar():
    """The POP3 account in the sidebar, its Inbox open on a message."""
    run = launch()
    window = main_window()
    scroll_down(150, 500, 80)
    click(94, 483)
    settle(2)
    click(480, 100)
    settle(5)
    capture(window, "pop3-demo-sidebar")
    return run


def pop3_menu():
    """The POP3 account's menu, with Messages That Will Not Download."""
    run = launch()
    window = main_window()
    scroll_down(150, 500, 80)
    act(name="Options for " + POP3_ADDRESS)
    settle(2)
    capture(window, "pop3-demo-menu", nudge=False, whole_screen=True)
    return run


def pop3_failures(name="pop3-demo-failures", size=None):
    run = launch(env=POP3_FAILURES)
    window = main_window()
    find(contains="would not hand these over")
    if size:
        resize(window, *size)
    settle(3)
    capture(window, name, nudge=False)
    return run


def pop3_failures_narrow():
    return pop3_failures("pop3-demo-failures-narrow", (360, 640))


def pop3_failures_short():
    """A window too short for the dialog's list, to see it scroll."""
    return pop3_failures("pop3-demo-failures-short", (420, 360))


def pop3_reply():
    """Preferences, where Not Available says why the POP3 account has no
    automatic reply."""
    run = launch(env={"MAILRS_DEMO_ACTION": "preferences"})
    window = main_window()
    find("page tab", name="General")
    scroll_down(720, 450, 40)
    find(contains="cannot send automatic replies over POP3")
    settle(2)
    capture(window, "pop3-demo-reply", nudge=False)
    return run


def pop3_about():
    run = launch(env={"MAILRS_DEMO_ACTION": "about"})
    window = main_window()
    find(contains="kept only on this computer")
    settle(2)
    capture(window, "pop3-demo-about", nudge=False)
    return run


def to_calendar(view="Week"):
    act("radio button", contains="Calendar, ")
    find("radio button", name=view)
    act("radio button", name=view)
    time.sleep(1.5)


def invitation():
    run = launch(env={"MAILRS_DEMO_OPEN": "t-design-review"})
    window = main_window()
    settle(5)
    dump("invitation")
    capture(window, "invitation")
    return run


def calendar_week():
    run = launch()
    window = main_window()
    to_calendar("Week")
    scroll_to_morning()
    settle(2)
    dump("week")
    capture(window, "calendar-week")
    return run


def calendar_month():
    run = launch()
    window = main_window()
    to_calendar("Month")
    settle(2)
    capture(window, "calendar-month")
    return run


def calendar_agenda():
    run = launch()
    window = main_window()
    resize(window, 1600, 1000)
    to_calendar("Agenda")
    # The list opens a little way down; the shot starts at its top.
    scroll_down(900, 500, 10, button=4)
    settle(2)
    capture(window, "calendar-agenda")
    return run


def event_popover():
    run = launch()
    window = main_window()
    to_calendar("Week")
    act("button", name="Next Week")
    time.sleep(1.5)
    scroll_to_morning()
    act("button", contains="Offline editor design review, Tuesday 6")
    settle(2)
    dump("popover")
    capture(window, "event-popover", nudge=False, whole_screen=True)
    return run


SHOTS = {
    "invitation": invitation,
    "calendar-week": calendar_week,
    "calendar-month": calendar_month,
    "calendar-agenda": calendar_agenda,
    "event-popover": event_popover,
    "inbox": inbox,
    "dark": dark,
    "composer": composer,
    "selection": selection,
    "flags": flags,
    "vips": vips,
    "categories": categories,
    "outlook": outlook,
    "pop3-sidebar": pop3_sidebar,
    "pop3-menu": pop3_menu,
    "pop3-failures": pop3_failures,
    "pop3-failures-narrow": pop3_failures_narrow,
    "pop3-failures-short": pop3_failures_short,
    "pop3-reply": pop3_reply,
    "pop3-about": pop3_about,
    "automatic-reply": automatic_reply,
    "rules": rules,
    "assistant": assistant,
    "send-later": send_later,
    "hide-my-email": hide_my_email,
    "phone": phone,
    "preferences": preferences,
    "welcome": welcome,
}


def main():
    names = WANTED or list(SHOTS)
    unknown = [n for n in names if n not in SHOTS]
    if unknown:
        raise SystemExit("no shot named %s; the shots are %s" % (", ".join(unknown), ", ".join(SHOTS)))
    for name in names:
        print("taking", name)
        run = SHOTS[name]()
        run.terminate()
        try:
            run.wait(10)
        except subprocess.TimeoutExpired:
            run.kill()
            run.wait()
        time.sleep(1)


main()
