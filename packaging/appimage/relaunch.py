#!/usr/bin/env python3
"""Check that an AppImage restarts, starts at login and runs the Claude
Code bridge without a second mount or a second extraction.

    relaunch.py <AppImage> mounted     the runtime mounts it with FUSE
    relaunch.py <AppImage> extracted   it runs with --appimage-extract-and-run

Run it on a hidden display with a session bus and the accessibility bus,
with HOME pointing at a throwaway folder: it adds an account row to the
store there. It drives the window through AT-SPI actions, so it needs no
window manager and no pointer.
"""

import ctypes
import glob
import os
import shlex
import signal
import sqlite3
import subprocess
import sys
import time
from pathlib import Path

import gi

gi.require_version("Atspi", "2.0")
from gi.repository import Atspi  # noqa: E402

IMAGE = sys.argv[1]
MODE = sys.argv[2]
assert MODE in ("mounted", "extracted"), MODE
FLAGS = ["--appimage-extract-and-run"] if MODE == "extracted" else []
HOME = Path(os.environ["HOME"])
DATA = Path(os.environ.get("XDG_DATA_HOME", HOME / ".local/share"))
CONFIG = Path(os.environ.get("XDG_CONFIG_HOME", HOME / ".config"))
STORE = DATA / "penguin-mail" / "mailrs.db"
LOGIN_ITEM = CONFIG / "autostart" / "io.github.c9dev.PenguinMail.desktop"
TEMP = os.environ.get("TMPDIR", "/tmp")
LOGS = HOME / "relaunch-logs"
LOGS.mkdir(parents=True, exist_ok=True)
launches = 0


def say(text):
    print(f"[{MODE}] {text}", flush=True)


def fail(text):
    for log in sorted(LOGS.glob("*.log")):
        print(f"--- {log}", flush=True)
        print(log.read_text(errors="replace")[-4000:], flush=True)
    raise SystemExit(f"[{MODE}] FAIL: {text}")


def wait(test, what, timeout=60):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        value = test()
        if value:
            return value
        time.sleep(0.25)
    fail(f"timed out waiting for {what}")


def copies():
    """What this run of the image holds: its FUSE mounts, or its runtime
    processes, each of which unpacks the image before it starts the app."""
    if MODE == "mounted":
        with open("/proc/self/mounts") as mounts:
            return sum("/.mount_" in line.split()[1] for line in mounts)
    image = IMAGE.encode()
    return sum((argv(proc.name) or [b""])[0] == image for proc in Path("/proc").glob("[0-9]*"))


def unpacked():
    """Penguin Mail folders extraction left in the temporary folder."""
    return [path for path in glob.glob(f"{TEMP}/appimage_extracted_*")
            if Path(path, "usr/bin/penguin-mail").exists()]


def argv(pid):
    try:
        return Path(f"/proc/{pid}/cmdline").read_bytes().split(b"\0")[:-1]
    except OSError:
        return None


def environ(pid):
    raw = Path(f"/proc/{pid}/environ").read_bytes().split(b"\0")
    return dict(item.decode().split("=", 1) for item in raw if b"=" in item)


def app_pids():
    """The app's processes: the bundled loader running usr/bin/penguin-mail,
    not a bridge."""
    found = []
    for proc in Path("/proc").glob("[0-9]*"):
        args = argv(proc.name)
        if not args or b"--mcp-bridge" in args:
            continue
        if any(arg.endswith(b"/usr/bin/penguin-mail") for arg in args):
            found.append(int(proc.name))
    return found


def the_app():
    pids = app_pids()
    return pids[0] if len(pids) == 1 else None


def alive(pid):
    return argv(pid) is not None and Path(f"/proc/{pid}/stat").read_text().split()[2] != "Z"


def launch(args=(), extra=None):
    global launches
    launches += 1
    log = open(LOGS / f"launch-{launches}.log", "w")
    env = dict(os.environ, **(extra or {}))
    subprocess.Popen([IMAGE, *FLAGS, *args], env=env, stdout=log, stderr=subprocess.STDOUT)
    return wait(the_app, "the app to start")


def application():
    desktop = Atspi.get_desktop(0)
    for index in range(desktop.get_child_count()):
        app = desktop.get_child_at_index(index)
        if app and "PenguinMail" in (app.get_name() or ""):
            return app
    return None


def frame():
    app = application()
    for child in range(app.get_child_count() if app else 0):
        node = app.get_child_at_index(child)
        if node and node.get_role_name() == "frame":
            return node
    return None


def find(node, test, depth=0):
    if node is None or depth > 80:
        return None
    if test(node):
        return node
    for index in range(node.get_child_count()):
        hit = find(node.get_child_at_index(index), test, depth + 1)
        if hit:
            return hit
    return None


# Older AT-SPI libraries name some of GTK 4's roles differently: Ubuntu
# 22.04 says "push button", and 24.04 says "last defined" for a switch.
ROLES = {"button": {"button", "push button"}}


def named(role, name):
    roles = ROLES.get(role, {role})
    return lambda node: node.get_role_name() in roles and (node.get_name() or "") == name


def actions(node):
    iface = node.get_action_iface()
    return [iface.get_action_name(i) for i in range(iface.get_n_actions())] if iface else []


def act(node, name):
    names = actions(node)
    if name not in names:
        fail(f"{node.get_role_name()} {node.get_name()!r} has no action {name}, only {names}")
    node.get_action_iface().do_action(names.index(name))


def outline(node, depth=0, lines=None):
    lines = [] if lines is None else lines
    if node is not None and depth <= 80:
        lines.append(f"{'  ' * depth}{node.get_role_name()} {node.get_name()!r} {actions(node)}")
        for index in range(node.get_child_count()):
            outline(node.get_child_at_index(index), depth + 1, lines)
    return lines


class Pointer:
    """XTest clicks. With no window manager the window sits at the
    screen's origin, so window coordinates are screen coordinates."""

    def __init__(self):
        self.x11 = ctypes.CDLL("libX11.so.6")
        self.xtst = ctypes.CDLL("libXtst.so.6")
        self.x11.XOpenDisplay.restype = ctypes.c_void_p
        self.x11.XFlush.argtypes = [ctypes.c_void_p]
        self.x11.XKeysymToKeycode.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
        self.x11.XKeysymToKeycode.restype = ctypes.c_ubyte
        self.xtst.XTestFakeKeyEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_int,
                                               ctypes.c_ulong]
        self.xtst.XTestFakeMotionEvent.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_int,
                                                   ctypes.c_int, ctypes.c_ulong]
        self.xtst.XTestFakeButtonEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_int,
                                                   ctypes.c_ulong]
        self.dpy = self.x11.XOpenDisplay(None)
        if not self.dpy:
            raise SystemExit("cannot open the X display")

    def click(self, node):
        box = node.get_extents(Atspi.CoordType.WINDOW)
        x, y = box.x + box.width // 2, box.y + box.height // 2
        self.xtst.XTestFakeMotionEvent(self.dpy, -1, x, y, 0)
        self.x11.XFlush(self.dpy)
        time.sleep(0.15)
        for pressed in (1, 0):
            self.xtst.XTestFakeButtonEvent(self.dpy, 1, pressed, 0)
            self.x11.XFlush(self.dpy)
            time.sleep(0.05)
        time.sleep(0.5)

    def key(self, keysym):
        code = self.x11.XKeysymToKeycode(self.dpy, keysym)
        for pressed in (1, 0):
            self.xtst.XTestFakeKeyEvent(self.dpy, code, pressed, 0)
            self.x11.XFlush(self.dpy)
            time.sleep(0.05)


def element(test, what, timeout=30, advance=None):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        hit = find(application(), test)
        if hit:
            return hit
        if advance:
            advance()
        time.sleep(0.25)
    (LOGS / "tree.log").write_text("\n".join(outline(application())))
    fail(f"found no {what}")


def quit_app(pid):
    """Quits through the app's own action, as the tray's Quit does."""
    subprocess.run(["gdbus", "call", "--session", "--dest", "io.github.c9dev.PenguinMail",
                    "--object-path", "/io/github/c9dev/PenguinMail",
                    "--method", "org.gtk.Actions.Activate", "quit", "[]", "{}"],
                   check=True, capture_output=True, timeout=30)
    gone(pid)


def gone(pid):
    wait(lambda: not alive(pid), "the app to quit")
    wait(lambda: copies() == baseline, "the runtime to let go of its copy")
    if MODE == "extracted" and unpacked():
        fail(f"quitting left {unpacked()}")


baseline = copies()
pointer = Pointer()


# A first start makes the store; closing its window with no accounts quits.
pid = launch()
wait(frame, "the first window")
act(frame(), "window.close")
gone(pid)
with sqlite3.connect(STORE) as db:
    account = db.execute(
        "INSERT INTO accounts (email, added_at, provider, provider_name) "
        "VALUES ('me@example.test', ?, 'imap', 'Test')", (int(time.time() * 1000),)).lastrowid
    for role in ("imap", "smtp"):
        db.execute(
            "INSERT INTO account_servers (account_id, role, host, port, security, user_name) "
            "VALUES (?, ?, '127.0.0.1', 1, 'tls', 'me@example.test')", (account, role))
say("store seeded with one account")

pid = launch(extra={"MAILRS_SHED_AFTER": "3"})
wait(frame, "the window")
held = copies()
if held != baseline + 1:
    fail(f"expected one copy of the image, found {held - baseline}")
appdir = environ(pid)["APPDIR"]
say(f"running from {appdir}")

# Start in the Tray at Login.
act(frame(), "win.preferences")
pointer.click(element(named("list item", "Sync"), "the Sync page"))
login = element(lambda n: (n.get_name() or "") == "Start in the Tray at Login"
                and (n.get_role_name() == "switch" or "toggle" in actions(n)), "the login switch")
pointer.click(login)
wait(LOGIN_ITEM.exists, "the login item")
line = next(l for l in LOGIN_ITEM.read_text().splitlines() if l.startswith("Exec="))
command = shlex.split(line.removeprefix("Exec="))
if command != [IMAGE, *FLAGS, "--background"]:
    fail(f"the login item runs {command}")
say(f"login item: PASS ({line})")

# Language, then the toast's Restart: the same process starts again.
pointer.click(element(named("list item", "Appearance"), "the Appearance page"))
language = element(lambda n: n.get_role_name() in ("combo box", "list item", "button")
                   and (n.get_name() or "").startswith("Language"), "the Language row")
pointer.click(language)
# GTK creates only the rows near the visible part of this list. Moving
# down reveals languages below the popup's initial viewport.
choice = element(lambda n: n.get_role_name() == "list item"
                 and (n.get_name() or "").startswith("Português"), "Português in the list",
                 advance=lambda: pointer.key(0xFF54))  # Down
act(choice, "listitem.scroll-to")
time.sleep(0.25)
pointer.click(choice)
close = element(named("button", "Close"), "the dialog's Close button")
act(close, "click")
restart = element(named("button", "Restart"), "the Restart toast")
act(restart, "click")
# The old window leaves the accessibility bus as the process execs, and
# the new one joins once GTK has started again.
wait(lambda: frame() is None, "the window to go for the restart", 30)
wait(frame, "the window after the language restart")
if the_app() != pid:
    fail("the language restart did not replace the running process")
if copies() != held:
    fail(f"the language restart left {copies() - baseline} copies of the image")
say("language restart: PASS")

# Closing the window sheds memory by restarting into the tray.
act(frame(), "window.close")
# AppRun has --background before it execs the bundled loader. Wait for
# the app's executable too, so that launcher interval cannot finish the wait.
wait(lambda: the_app() == pid and b"--background" in (argv(pid) or []),
     "the background app after the idle restart", 30)
if the_app() != pid:
    fail("the idle restart did not replace the running process")
time.sleep(2)
if copies() != held:
    fail(f"the idle restart left {copies() - baseline} copies of the image")
say("idle restart: PASS")

# The command Claude Code starts as the tool bridge.
bridge = environ(pid).get("PENGUIN_MAIL_BRIDGE_COMMAND")
if bridge != f"{appdir}/AppRun":
    fail(f"the bridge command is {bridge}")
ran = subprocess.run([bridge, "--mcp-bridge", f"{HOME}/no-such-socket"], stdin=subprocess.DEVNULL,
                     capture_output=True, text=True, timeout=60)
if "mcp bridge" not in ran.stderr and ran.returncode != 0:
    fail(f"the bridge did not start: {ran.returncode} {ran.stderr}")
if copies() != held:
    fail(f"the bridge left {copies() - baseline} copies of the image")
say(f"bridge: PASS (exit {ran.returncode}, {ran.stderr.strip()[-160:]!r})")

# What the desktop runs at the next login, with this copy gone.
quit_app(pid)
log = open(LOGS / "login.log", "w")
subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
pid = wait(the_app, "the copy started at login")
time.sleep(5)
if not alive(pid) or copies() != baseline + 1:
    fail("the copy started at login did not keep running")
quit_app(pid)
say("start at login: PASS")
