"""Records the headless shell's monitor into a file until stopped.

    python3 record.py OUT.mkv

It asks Mutter's ScreenCast interface for a stream of the virtual monitor
with the pointer drawn in, and GStreamer encodes that stream as H.264 at a
quality high enough to cut and encode again. It prints "recording" once
frames flow, and stops cleanly on SIGINT or SIGTERM.
"""

import signal
import subprocess
import sys

import gi

gi.require_version("Gio", "2.0")
gi.require_version("GLibUnix", "2.0")
from gi.repository import Gio, GLib, GLibUnix  # noqa: E402

out = sys.argv[1]
bus = Gio.bus_get_sync(Gio.BusType.SESSION)


def call(path, interface, method, args, reply):
    return bus.call_sync(
        "org.gnome.Mutter.ScreenCast",
        path,
        interface,
        method,
        args,
        GLib.VariantType(reply),
        Gio.DBusCallFlags.NONE,
        -1,
        None,
    ).unpack()


session = call(
    "/org/gnome/Mutter/ScreenCast",
    "org.gnome.Mutter.ScreenCast",
    "CreateSession",
    GLib.Variant("(a{sv})", ({},)),
    "(o)",
)[0]
# Meta-0 is the virtual monitor gnome-shell --virtual-monitor adds, and
# cursor mode 1 draws the pointer into the frames.
stream = call(
    session,
    "org.gnome.Mutter.ScreenCast.Session",
    "RecordMonitor",
    GLib.Variant("(sa{sv})", ("Meta-0", {"cursor-mode": GLib.Variant("u", 1)})),
    "(o)",
)[0]

loop = GLib.MainLoop()
encoder = []


def added(connection, sender, path, interface, name, parameters):
    node = parameters.unpack()[0]
    encoder.append(
        subprocess.Popen(
            [
                "gst-launch-1.0", "-q", "-e",
                "pipewiresrc", "path=%d" % node, "do-timestamp=true", "keepalive-time=1000",
                "!", "videorate", "!", "video/x-raw,framerate=60/1",
                "!", "videoconvert", "!", "video/x-raw,format=I420",
                "!", "queue", "max-size-buffers=0", "max-size-time=0", "max-size-bytes=0",
                "!", "x264enc", "speed-preset=veryfast", "pass=quant", "quantizer=12",
                "tune=zerolatency", "key-int-max=60", "threads=8",
                "!", "matroskamux", "!", "filesink", "location=%s" % out,
            ]
        )
    )
    print("recording", flush=True)


bus.signal_subscribe(
    None,
    "org.gnome.Mutter.ScreenCast.Stream",
    "PipeWireStreamAdded",
    stream,
    None,
    Gio.DBusSignalFlags.NONE,
    added,
)
bus.call_sync(
    "org.gnome.Mutter.ScreenCast",
    session,
    "org.gnome.Mutter.ScreenCast.Session",
    "Start",
    None,
    None,
    Gio.DBusCallFlags.NONE,
    -1,
    None,
)


def stop(*_):
    for process in encoder:
        process.send_signal(signal.SIGINT)
        process.wait(timeout=60)
    try:
        bus.call_sync(
            "org.gnome.Mutter.ScreenCast",
            session,
            "org.gnome.Mutter.ScreenCast.Session",
            "Stop",
            None,
            None,
            Gio.DBusCallFlags.NONE,
            -1,
            None,
        )
    except GLib.Error:
        pass
    loop.quit()
    return GLib.SOURCE_REMOVE


GLibUnix.signal_add(GLib.PRIORITY_DEFAULT, signal.SIGINT, stop)
GLibUnix.signal_add(GLib.PRIORITY_DEFAULT, signal.SIGTERM, stop)
loop.run()
