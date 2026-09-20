# Translations

`penguin-mail.pot` holds every word a person reads, pulled out of the
source. Each `<locale>.po` beside it is one language.

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

4. Build and install it:

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
