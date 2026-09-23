"""Cuts the recorded tour into the finished demo video.

    python3 compose.py OUT_DIR VERSION

Reads OUT_DIR/raw.mkv and OUT_DIR/marks.json, which tour.py wrote, and
writes OUT_DIR/penguin-mail.mp4 and OUT_DIR/penguin-mail.webm: an opening
card, the tour with a caption over each scene, and a closing card. The
cards and captions are SVG drawn in the app icon's colours, turned into
images with rsvg-convert, so they use the same fonts as the desktop.
"""

import json
import os
import subprocess
import sys
from xml.sax.saxutils import escape

OUT, VERSION = sys.argv[1:3]
HERE = os.path.dirname(os.path.abspath(__file__))
ICON = os.path.join(HERE, "..", "..", "app", "data", "icons", "scalable", "apps", "io.github.c9dev.PenguinMail.svg")
ART = os.path.join(OUT, "art")
FPS = 60
W, H = 1920, 1080

# How long each card stays, and how long a fade between two parts lasts.
OPENING, CLOSING, FADE = 4.2, 5.5, 0.8
# The window's bottom and right edges in the recording, which tour.py
# sets; captions sit below the window.
WINDOW_BOTTOM, WINDOW_RIGHT = 958, 1740


def render(name, svg, width=W, height=H):
    source = os.path.join(ART, name + ".svg")
    target = os.path.join(ART, name + ".png")
    with open(source, "w") as out:
        out.write(svg)
    subprocess.run(["rsvg-convert", "-w", str(width), "-h", str(height), source, "-o", target], check=True)
    return target


def icon(x, y, size):
    """The app icon, drawn from its own file."""
    with open(ICON) as source:
        body = source.read()
    body = body[body.index(">", body.index("<svg")) + 1 : body.rindex("</svg>")]
    return '<svg x="%d" y="%d" width="%d" height="%d" viewBox="0 0 256 256">%s</svg>' % (x, y, size, size, body)


BACKGROUND = """
  <defs>
    <linearGradient id="base" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="#32302f"/><stop offset="0.6" stop-color="#282828"/><stop offset="1" stop-color="#1d2021"/>
    </linearGradient>
    <filter id="soft" x="-50%" y="-50%" width="200%" height="200%"><feGaussianBlur stdDeviation="130"/></filter>
    <filter id="shadow" x="-30%" y="-30%" width="160%" height="160%">
      <feDropShadow dx="0" dy="18" stdDeviation="22" flood-color="#000" flood-opacity="0.45"/>
    </filter>
  </defs>
  <rect width="1920" height="1080" fill="url(#base)"/>
  <g filter="url(#soft)">
    <ellipse cx="1650" cy="180" rx="520" ry="360" fill="#fe8019" opacity="0.30"/>
    <ellipse cx="260" cy="930" rx="620" ry="380" fill="#d65d0e" opacity="0.20"/>
    <ellipse cx="1180" cy="1020" rx="560" ry="300" fill="#7c6f64" opacity="0.35"/>
  </g>
"""

FONT = "font-family=\"Ubuntu Sans, Cantarell, sans-serif\""


def opening():
    return render(
        "opening",
        """<svg xmlns="http://www.w3.org/2000/svg" width="1920" height="1080">%s
  <g filter="url(#shadow)">%s</g>
  <text x="960" y="700" text-anchor="middle" %s font-size="104" font-weight="700" fill="#fbf1c7">Penguin Mail</text>
  <text x="960" y="770" text-anchor="middle" %s font-size="38" fill="#d5c4a1">A fast, private Gmail client for GNOME</text>
</svg>"""
        % (BACKGROUND, icon(835, 290, 250), FONT, FONT),
    )


def closing():
    return render(
        "closing",
        """<svg xmlns="http://www.w3.org/2000/svg" width="1920" height="1080">%s
  <g filter="url(#shadow)">%s</g>
  <text x="960" y="620" text-anchor="middle" %s font-size="92" font-weight="700" fill="#fbf1c7">Penguin Mail %s</text>
  <text x="960" y="690" text-anchor="middle" %s font-size="36" fill="#d5c4a1">Free software, written in Rust with GTK 4 and libadwaita</text>
  <rect x="610" y="748" width="700" height="72" rx="36" fill="#fe8019"/>
  <text x="960" y="796" text-anchor="middle" %s font-size="34" font-weight="700" fill="#282828">github.com/c9dev/penguin-mail</text>
</svg>"""
        % (BACKGROUND, icon(855, 240, 210), FONT, escape(VERSION), FONT, FONT),
    )


def caption(index, title, detail):
    """A panel under the window: the scene's title and one line about it."""
    # Wide enough for the longer line, at about the width Ubuntu Sans
    # takes per character at these sizes.
    width = int(max(len(title) * 19.5, len(detail) * 12.2)) + 120
    # Flush with the window's right edge, under the conversation, where
    # nothing the list opens, such as a long menu, reaches.
    x = WINDOW_RIGHT - width
    y = WINDOW_BOTTOM + 18
    return render(
        "caption-%02d" % index,
        """<svg xmlns="http://www.w3.org/2000/svg" width="1920" height="1080">
  <defs><filter id="lift" x="-20%%" y="-40%%" width="140%%" height="180%%">
    <feDropShadow dx="0" dy="6" stdDeviation="10" flood-color="#000" flood-opacity="0.35"/></filter></defs>
  <g filter="url(#lift)"><rect x="%d" y="%d" width="%d" height="86" rx="22" fill="#1d2021" fill-opacity="0.92"/></g>
  <rect x="%d" y="%d" width="6" height="54" rx="3" fill="#fe8019"/>
  <text x="%d" y="%d" %s font-size="32" font-weight="700" fill="#fbf1c7">%s</text>
  <text x="%d" y="%d" %s font-size="21" fill="#d5c4a1">%s</text>
</svg>"""
        % (
            x, y, width,
            x + 30, y + 16,
            x + 56, y + 40, FONT, escape(title),
            x + 56, y + 70, FONT, escape(detail),
        ),
    )


def duration(path):
    return float(
        subprocess.run(
            ["ffprobe", "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", path],
            capture_output=True, text=True, check=True,
        ).stdout
    )


def main():
    os.makedirs(ART, exist_ok=True)
    raw = os.path.join(OUT, "raw.mkv")
    with open(os.path.join(OUT, "marks.json")) as source:
        marks = json.load(source)
    # The tour starts a moment before its first scene, in the overview.
    start = max(0.0, marks[0]["at"] - 0.4)
    length = duration(raw) - start

    inputs = ["-loop", "1", "-framerate", str(FPS), "-t", str(OPENING), "-i", opening()]
    inputs += ["-ss", "%.3f" % start, "-i", raw]
    inputs += ["-loop", "1", "-framerate", str(FPS), "-t", str(CLOSING), "-i", closing()]

    # The first scene is the opening card's own words, so it has no caption.
    scenes = marks[1:]
    filters = [
        "[0:v]format=yuv420p,fade=t=in:st=0:d=0.8[open]",
        "[1:v]fps=%d,format=yuv420p,setpts=PTS-STARTPTS[tour0]" % FPS,
    ]
    last = "tour0"
    for index, scene in enumerate(scenes):
        begin = scene["at"] - start + 0.35
        end = (scenes[index + 1]["at"] - start - 0.25) if index + 1 < len(scenes) else length - 0.6
        inputs += ["-loop", "1", "-framerate", str(FPS), "-t", "%.3f" % (end - begin), "-i", caption(index, scene["caption"], scene["detail"])]
        stream = 3 + index
        filters.append(
            "[%d:v]format=rgba,fade=t=in:st=0:d=0.35:alpha=1,fade=t=out:st=%.3f:d=0.35:alpha=1,setpts=PTS+%.3f/TB[cap%d]"
            % (stream, end - begin - 0.35, begin, index)
        )
        filters.append("[%s][cap%d]overlay=eof_action=pass[tour%d]" % (last, index, index + 1))
        last = "tour%d" % (index + 1)
    filters.append("[%s]trim=duration=%.3f,setpts=PTS-STARTPTS[tour]" % (last, length))
    filters.append("[open][tour]xfade=transition=fade:duration=%.2f:offset=%.3f[a]" % (FADE, OPENING - FADE))
    filters.append(
        "[a][2:v]xfade=transition=fade:duration=%.2f:offset=%.3f,format=yuv420p[v]"
        % (FADE, OPENING - FADE + length - FADE)
    )
    graph = ";".join(filters)

    mp4 = os.path.join(OUT, "penguin-mail.mp4")
    subprocess.run(
        ["ffmpeg", "-v", "error", "-y", *inputs, "-filter_complex", graph, "-map", "[v]",
         "-c:v", "libx264", "-preset", "slow", "-crf", "18", "-profile:v", "high",
         "-pix_fmt", "yuv420p", "-movflags", "+faststart", "-r", str(FPS), mp4],
        check=True,
    )
    webm = os.path.join(OUT, "penguin-mail.webm")
    subprocess.run(
        ["ffmpeg", "-v", "error", "-y", "-i", mp4, "-c:v", "libvpx-vp9", "-crf", "30", "-b:v", "0",
         "-row-mt", "1", "-deadline", "good", "-cpu-used", "2", webm],
        check=True,
    )
    print("wrote", mp4, "and", webm)


main()
