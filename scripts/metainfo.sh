#!/usr/bin/env bash
# Writes the <releases> list in the AppStream metainfo from CHANGELOG.md, so
# GNOME Software, Flathub and the Snap Store show the same release notes as
# GitHub. release.sh runs it after writing the new version's section.
#
#   scripts/metainfo.sh            rewrite the list
#   scripts/metainfo.sh --check    say whether the list matches the changelog
#
# Each "## X.Y.Z (date)" section becomes one <release>. Its "### New" style
# headings become paragraphs and its bullets a list. The notes carry
# translate="no": they change with every release, and a translator would
# chase them for nothing.
set -euo pipefail

cd "$(dirname "$0")/.."
file=app/data/io.github.c9dev.PenguinMail.metainfo.xml
check=
case ${1:-} in
--check) check=1 ;;
"") ;;
*) echo "usage: scripts/metainfo.sh [--check]" >&2; exit 2 ;;
esac

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

python3 - CHANGELOG.md "$file" "$work/metainfo.xml" <<'PY'
import re
import sys
from html import escape

changelog, metainfo, out = sys.argv[1:]
REPOSITORY = "https://github.com/c9dev/penguin-mail"


def inline(text):
    """Markdown's inline marks as AppStream allows them: code stays code,
    links keep their words, and bold drops its stars."""
    parts = re.split(r"(`[^`]*`)", text)
    done = []
    for part in parts:
        if part.startswith("`") and part.endswith("`") and len(part) > 1:
            done.append("<code>" + escape(part[1:-1], quote=False) + "</code>")
            continue
        part = re.sub(r"\[([^\]]+)\]\([^)]+\)", r"\1", part)
        part = part.replace("**", "")
        done.append(escape(part, quote=False))
    return "".join(done)


def sections(lines):
    """(version, date, body lines) for each released section, newest first."""
    found = []
    for line in lines:
        heading = re.match(r"^## (\d+\.\d+\.\d+) \((\d{4}-\d{2}-\d{2})\)\s*$", line)
        if heading:
            found.append((heading[1], heading[2], []))
        elif line.startswith("## "):
            found.append((None, None, []))
        elif found:
            found[-1][2].append(line)
    return [(v, d, body) for v, d, body in found if v]


def blocks(body):
    """Paragraphs and bullet lists, with each item's wrapped lines joined."""
    out = []
    for line in body:
        stripped = line.strip()
        if not stripped:
            out.append(("gap", ""))
        elif stripped.startswith("### "):
            out.append(("p", stripped[4:]))
        elif stripped.startswith("- "):
            out.append(("li", stripped[2:]))
        elif out and out[-1][0] in ("li", "p") and line.startswith(" "):
            kind, text = out[-1]
            out[-1] = (kind, text + " " + stripped)
        elif out and out[-1][0] == "p":
            out[-1] = ("p", out[-1][1] + " " + stripped)
        else:
            out.append(("p", stripped))
    return [b for b in out if b[0] != "gap"]


def release(version, date, body):
    lines = [f'    <release version="{version}" date="{date}">']
    lines.append(f"      <url type=\"details\">{REPOSITORY}/releases/tag/v{version}</url>")
    lines.append('      <description translate="no">')
    in_list = False
    for kind, text in blocks(body):
        if kind == "li" and not in_list:
            lines.append("        <ul>")
            in_list = True
        if kind != "li" and in_list:
            lines.append("        </ul>")
            in_list = False
        if kind == "li":
            lines.append(f"          <li>{inline(text)}</li>")
        else:
            lines.append(f"        <p>{inline(text)}</p>")
    if in_list:
        lines.append("        </ul>")
    lines.append("      </description>")
    lines.append("    </release>")
    return "\n".join(lines)


with open(changelog, encoding="utf-8") as f:
    released = sections(f.read().splitlines())
with open(metainfo, encoding="utf-8") as f:
    text = f.read()
listed = "\n".join(release(*s) for s in released)
start = text.index("<releases>") + len("<releases>")
end = text.index("</releases>")
indent = text[text.rindex("\n", 0, end) + 1 : end]
text = text[:start] + "\n" + listed + "\n" + indent + text[end:]
with open(out, "w", encoding="utf-8") as f:
    f.write(text)
PY

if [ -n "$check" ]; then
    if diff -q "$file" "$work/metainfo.xml" >/dev/null; then
        echo "$file matches CHANGELOG.md."
        exit 0
    fi
    echo "$file lists other releases than CHANGELOG.md; run scripts/metainfo.sh" >&2
    exit 1
fi
mv "$work/metainfo.xml" "$file"
echo "Wrote the releases in $file."
