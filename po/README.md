# Translations

`penguin-mail.pot` holds every word a person reads, pulled out of the
source. Each `<locale>.po` beside it is one language.

## English

The strings in the source are American English. British English is
`en_GB.po`, which `scripts/en-gb.py` writes from the template on every run
of `scripts/update-po.sh`, changing the spellings that differ (colour,
favourite, organise, cancelled, grey, licence). Do not edit it by hand. A
word it gets wrong goes into the script's `KEEP` set, and a spelling it
misses into `STEMS` or `WORDS`; `update-po.sh --check` fails while the file
is behind the template.

## Adding a language

1. Start the file from the template, naming the locale you are
   translating into:

   ```
   msginit --locale=fr_FR --input=po/penguin-mail.pot --output=po/fr_FR.po
   ```

2. Add one line to its header, the language's name written in that
   language. Preferences reads it to fill the Language list, and a `.po`
   without it is left out:

   ```
   "X-Language-Name: Français\n"
   ```

3. Translate. `msgstr ""` means untranslated, and the app then shows the
   English behind it, so a half-finished file is safe to commit.

4. Build and install it. `scripts/update-po.sh` writes `LINGUAS`, which is
   how `msgfmt --desktop` finds your language for the launcher's own name
   and description:

   ```
   scripts/update-po.sh     # compiles into target/locale, for a build-tree run
   scripts/install.sh       # installs into <prefix>/share/locale
   ```

   Penguin Mail offers a language in Preferences only once its catalogue
   is installed, and reads the choice as it starts, so pick one and
   restart.

## Keeping a language up to date

`scripts/update-po.sh` rebuilds the template from the source and brings
every `.po` up to it. Run it after changing any word the app shows.
Strings it added arrive untranslated; ones whose English changed arrive
marked `#, fuzzy`, with the old translation under them to work from.

`scripts/update-po.sh --check` says whether the template has fallen
behind the source without writing anything, which is what a build machine
wants.

The script needs `xtr`, which reads Rust where `xgettext` only guesses:

```
cargo install xtr
```

## Where the words come from

`POTFILES.in` lists the files that hold them. The script walks the crates
from their roots rather than from that list, and says so when the two have
drifted apart.

Some strings are deliberately not translated: Gmail's label ids, its
search syntax such as `has:attachment`, MIME types, header names, CSS, and
anything else that goes to Gmail rather than to a reader.

Two more stay in English on purpose. The `---------- Forwarded message
----------` line is the marker every mail client writes, and Penguin Mail
looks for it again when it reopens a saved draft. The `From:`, `Date:`,
`Subject:`, `To:` and `Cc:` names in a forwarded block are read back the
same way.

A date pattern made only of `%` codes, such as `%-d %b`, is in the
template so a language can reorder it or add words, as Portuguese does
with `%-d de %B`. Leave the names to the codes: `%A`, `%a`, `%B` and `%b`
come out in the language of your catalogue, taken from its `Language`
header, whatever `LC_TIME` says. Check that header names your locale, such
as `pt_PT`; a bare `de` gets the names of `de_DE`.
