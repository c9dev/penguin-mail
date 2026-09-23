#!/usr/bin/env python3
"""Writes po/en_GB.po, British English, from the template.

    scripts/en-gb.py TEMPLATE OUT

The source strings are American English. msginit copies every one into an
English catalogue, and this changes the spellings that differ: colour,
favourite, organise, cancelled, centre, grey, licence and so on. A
placeholder such as {color}, a URL and a keyboard shortcut stay as they are,
since the code reads them. scripts/update-po.sh runs this, so a new string
gets its British spelling without anyone writing it by hand.

A word the rules get wrong goes in KEEP, spelt as the source has it.
"""

import re
import subprocess
import sys
import tempfile

# American stem to British stem. Each is matched as the start of a word, so
# "color" covers colors, colored, coloring and colorful.
STEMS = {
    "color": "colour",
    "favorite": "favourite",
    "behavior": "behaviour",
    "honor": "honour",
    "neighbor": "neighbour",
    "center": "centre",
    "catalog": "catalogue",
    "gray": "grey",
    "canceled": "cancelled",
    "canceling": "cancelling",
    "labeled": "labelled",
    "labeling": "labelling",
    "traveled": "travelled",
    "traveling": "travelling",
    "modeled": "modelled",
    "judgment": "judgement",
    "acknowledgment": "acknowledgement",
    "fulfill": "fulfil",
    "analyz": "analys",
}

# Verbs in -ize whose British form is -ise, as the Oxford house style does
# not use but most British readers expect.
IZE = [
    "organiz", "categoriz", "summariz", "customiz", "authoriz", "recogniz",
    "prioritiz", "synchroniz", "minimiz", "maximiz", "realiz", "initializ",
    "apologiz", "optimiz", "personaliz", "emphasiz", "utiliz", "standardiz",
    "visualiz", "finaliz", "normaliz", "memoriz", "capitaliz", "characteriz",
    "specializ", "localiz", "serializ", "sanitiz", "stabiliz", "criticiz",
]
for stem in IZE:
    STEMS[stem] = stem[:-1] + "s"

# Whole words that change only as nouns: "License" is the About page's
# heading, while "licensed" stays.
WORDS = {"license": "licence", "licenses": "licences"}

# Words the rules would change and must not.
KEEP = {"size", "sizes", "seize", "prize", "capsize"}

# What the code reads rather than a person: placeholders, URLs, shortcuts,
# addresses, markup.
PROTECTED = re.compile(r"\{[^}]*\}|https?://\S+|<[^>]+>|\S+@\S+|\b[A-Za-z]+\+[A-Za-z0-9]+\b")
WORD = re.compile(r"[A-Za-z]+")


def british_word(word):
    lower = word.lower()
    if lower in KEEP:
        return word
    if lower in WORDS:
        new = WORDS[lower]
    else:
        new = None
        for stem, replacement in STEMS.items():
            if lower.startswith(stem):
                new = replacement + lower[len(stem):]
                break
        if new is None:
            return word
    if word.isupper():
        return new.upper()
    if word[0].isupper():
        return new[0].upper() + new[1:]
    return new


def british(text):
    """`text` with British spellings, leaving what the code reads alone."""
    out, at = [], 0
    for kept in PROTECTED.finditer(text):
        out.append(WORD.sub(lambda m: british_word(m.group()), text[at:kept.start()]))
        out.append(kept.group())
        at = kept.end()
    out.append(WORD.sub(lambda m: british_word(m.group()), text[at:]))
    return "".join(out)


def check_rules():
    cases = {
        "Could not change the color: {reason}": "Could not change the colour: {reason}",
        "Color the label “{label}” in {account} {color}?": "Colour the label “{label}” in {account} {color}?",
        "Favorites": "Favourites",
        "Summarize this conversation": "Summarise this conversation",
        "Canceling a reminder": "Cancelling a reminder",
        "Organization: {organisation}": "Organisation: {organisation}",
        "License": "Licence",
        "Text size": "Text size",
        "See https://example.com/color": "See https://example.com/color",
        "GRAY": "GREY",
    }
    for american, expected in cases.items():
        got = british(american)
        assert got == expected, "%r became %r, not %r" % (american, got, expected)


def main():
    check_rules()
    template, out = sys.argv[1:3]
    with tempfile.TemporaryDirectory() as work:
        made = "%s/en_GB.po" % work
        subprocess.run(
            ["msginit", "--no-translator", "--no-wrap", "--locale=en_GB.UTF-8",
             "-i", template, "-o", made],
            check=True, capture_output=True,
        )
        with open(made, encoding="utf-8") as f:
            lines = f.read().split("\n")
    result = []
    in_header, in_msgstr = True, False
    for line in lines:
        if line.startswith("msgid") or line.startswith("msgctxt"):
            in_msgstr = False
        elif line.startswith("msgstr"):
            in_msgstr = True
        if in_header and line == "":
            in_header = False
        if in_header:
            if line.startswith('"Last-Translator:'):
                line = '"Last-Translator: scripts/en-gb.py\\n"'
            elif line.startswith('"Language-Team:'):
                line = '"Language-Team: English (United Kingdom)\\n"'
            elif line.startswith('"Language: '):
                line = line + '\n"X-Language-Name: English (United Kingdom)\\n"'
            elif line.startswith("# Penguin Mail, a Gmail client"):
                line = "# British English for Penguin Mail, written by scripts/en-gb.py from the\n# template. Do not edit it by hand: change the rules in the script."
            result.append(line)
            continue
        if in_msgstr and '"' in line:
            start = line.index('"')
            body = line[start + 1:-1]
            line = line[:start + 1] + british(body) + '"'
        result.append(line)
    with open(out, "w", encoding="utf-8") as f:
        f.write("\n".join(result))


main()
