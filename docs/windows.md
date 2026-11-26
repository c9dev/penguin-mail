# Windows port

Penguin Mail on Windows: `x86_64-pc-windows-gnu` (MSYS2 UCRT64) for
release and development, Windows 10 or later, with feature parity as the
goal. WebView2 draws mail HTML, an Inno Setup installer carries it for
direct downloads, and PGP/S-MIME and skill scripts are in scope.

## Building

From an MSYS2 UCRT64 shell, one command covers the release:

```sh
scripts/build-windows.sh
```

It builds `target/x86_64-pc-windows-gnu/release/penguin-mail.exe`
against MSYS2's GTK and hands the exe to `scripts/package-windows.sh`,
which stages the install tree at `dist/penguin-mail/` and compiles
`dist/penguin-mail-<version>-setup.exe` with Inno Setup 6.

What a machine needs:

- **Rust** with the `x86_64-pc-windows-gnu` toolchain.
- **MSYS2 UCRT64** with the packages the CI's `installer` job installs
  (GTK 4, libadwaita, gettext, pkgconf, binutils, gnupg, poppler). The
  default prefix is `C:/msys64/ucrt64`; `--msys2` names another.
- **The patched GTK**, for a release:
  `scripts/build-gtk4-msys2.sh <MINGW-packages checkout>` rebuilds
  MSYS2's gtk4 package from its recipe with
  `packaging/windows/gtk4-zero-monitor-popup.patch` added and installs
  it. It stops when the recipe moves off GTK 4.24.2 until the patch is
  checked against the new version. A stock gtk4 builds too, without the
  patch.
- **Inno Setup 6**, for the installer step; `--stage-only` skips it.
- **BusyBox** for the skills sandbox: downloaded to the cache once and
  checked against a pinned SHA-256 (update the pin in `build-windows.sh`
  when upstream publishes a new build), or named with `--busybox`.
- The WebView2 evergreen bootstrapper is fetched at packaging time
  and embedded in the installer.

The release moved off MSVC and gvsbuild on 2026-11-26: gvsbuild 2026.10.0
builds GLib with `proxy-libintl` in place of gettext's `intl.dll`, so
gettext-rs no longer links (`textdomain`, `gettext` unresolved). The MSVC
notes below describe the earlier gvsbuild 2025.11.1 zip, which still
carried the real `intl.dll`.

For development the gnu target builds against MSYS2's own GTK:

```sh
pacman -S mingw-w64-ucrt-x86_64-{gtk4,libadwaita,librsvg,gdk-pixbuf2,glib2,gettext-tools,gettext-runtime,adwaita-icon-theme,fontconfig,libwebp}
PKG_CONFIG_PATH=/ucrt64/lib/pkgconfig cargo run --target x86_64-pc-windows-gnu -p mailrs -- --demo
```

`.cargo/config.toml` already sets `PKG_CONFIG_ALLOW_CROSS` so
pkg-config agrees to probe a target that differs from the MSVC host
triple.

`scripts/update-po.sh` wants both gettext toolchains on `PATH`: UCRT64's
`bin` first — its `xgettext`/`msgmerge`/`msgfmt` parse the metainfo XML
correctly — then the gvsbuild `bin`, which supplies the `intl.dll`
`xtr.exe` links against (it is an MSVC build; without it the process
dies with exit 127, `ERROR_PROC_NOT_FOUND`). The gvsbuild `xgettext`
treats the XML as C and silently drops every metainfo string, so its
dir must not win.

## MSVC toolchain (verified)

The release target builds: `cargo build --target
x86_64-pc-windows-msvc` produces a working `penguin-mail.exe` and the
`--demo` run opens the full app. GTK comes from the gvsbuild release
zip (GTK4_Gvsbuild_2025.11.1_x64.zip: GTK 4.20.3, libadwaita 1.8.2 —
new enough for the `v4_20`/`v1_8` features), unzipped to a prefix whose
`lib/pkgconfig/*.pc` `prefix=` lines point at itself (the zip bakes in
`C:/gtk-build/...`, so they get rewritten; the bundle ships its own
`pkg-config.exe` and `lib/intl.lib` + `include/libintl.h` + `bin/
intl.dll`). The build wants: vcvars (`vcvarsall.bat x64`), the
prefix's `bin` on `PATH`, `PKG_CONFIG_PATH` at its `lib/pkgconfig` and
`share/pkgconfig`, and `GETTEXT_DIR`/`GETTEXT_LIB_DIR`/
`GETTEXT_INCLUDE_DIR` for gettext-sys. `RUSTFLAGS=-Lnative=<prefix>/lib`
covers any stray link. WebView2Loader.dll comes from the
`webview2-com-sys` `OUT_DIR/x64` and sits beside the exe.

One code bug this exposed: `language::bind` called
`gettextrs::setlocale`, whose `LocaleCategory` enum numbers glibc's
constants (`LcAll` is 6); the MSVC CRT uses different numbers
(`LC_ALL` is 0) and aborts on an unknown category. The setlocale block
is now `cfg(not(windows))`; libintl reads `LANGUAGE` without it.

## What already works

Built and verified on `x86_64-pc-windows-gnu` (MSYS2 UCRT64): the whole
workspace compiles, all 142 test binaries pass, and the app runs.
WebView2 serves the reading and sealed web kinds, the app's `mailrs:`
and `mailrs-cid:` schemes, injected scripts, link previews, find, and
the hidden unsubscribe engine. The notification-area icon shows the
tray menu (`tray-icon`/`muda`), WinRT toasts deliver the action buttons
through an in-process `Activated` handler, the taskbar button carries
an unread-count overlay (`ITaskbarList3::SetOverlayIcon`), the Run-key
login item toggles from Preferences, and the exe carries both icons as
Win32 resources.

`cargo check` on a Windows host passes for `mailrs-pgp`, `mailrs-smime`,
`mailrs-discover` and `mailrs-pop3` unchanged. The `keyring` crate's
Windows backend is already in the lockfile, so IMAP passwords, OAuth
tokens and AI keys land in Credential Manager with no new code. `dirs`
maps the config, data and cache paths onto `%APPDATA%`/`%LOCALAPPDATA%`.
`notify-rust` carries a WinRT backend (`tauri-winrt-notification`) that
the update announcer can use. `sync::goa::session()` already returns the
`Absent` stand-in off Linux, so the sign-in routes on Windows match
macOS: own Google project or app password.

The platform seams the port needs all exist, built for macOS first:
`app/src/web/`, `app/src/app/notifications/`, `app/src/app/dock_badge/`,
the `Engine` trait in `app/src/unsubscribe_page/hidden.rs`, and the
`Packaging` enum. Windows is a third adapter in each, not a rewrite.

The GNU build needs `PKG_CONFIG_ALLOW_CROSS=1`,
`PKG_CONFIG_PATH=<ucrt64>/lib/pkgconfig` and UCRT64's bin on `PATH`,
because the rustc host triple is `x86_64-pc-windows-msvc` and
`system-deps` treats gnu builds as cross-compiles.

## Verified compile blockers (historical)

`cargo check` on Windows today fails at:

- `ai/src/bridge.rs`, `ai/src/detect.rs`, `ai/src/providers/claude_code.rs`:
  `std::os::unix` imports, `tokio::net::UnixListener`/`UnixStream`,
  `DirBuilder::mode`, `Permissions::from_mode` and `.mode()` calls.
- `gettext-sys`: without a system libintl its build script runs the
  bundled gettext's autoconf setup and fails under MSVC. The script
  honours `GETTEXT_DIR`, `GETTEXT_LIB_DIR` and `GETTEXT_INCLUDE_DIR`;
  point them at the GTK prefix's libintl rather than touching code.
  Twelve crates inherit this through `mailrs-domain`.

Further blockers visible in the source, which the check will reach once
gettext is fixed:

- `app/src/app.rs`: unconditional `std::os::unix::process::CommandExt`
  and unconditional `ksni` import and field.
- `sync/src/config.rs`: unconditional `OpenOptionsExt`, `.mode(0o600)`
  and `private_dir`'s `0o700`.
- `app/src/ui/window/previews.rs`: unconditional unix fs imports.
- `app/src/web/mod.rs` and `app/src/unsubscribe_page.rs`: no `windows`
  branch, so `WebView`, `PageBrowser` and friends do not exist.
- `app/src/app/notifications.rs` and `app/src/app/dock_badge.rs`:
  `cfg(not(macos))` selects the Linux backend on Windows; re-gate to
  explicit `linux`/`macos`/`windows`.
- Tests using `std::os::unix::fs::{PermissionsExt, symlink}` across
  `sync`, `pgp`, `smime`, `ai` and `app/tests` need `cfg(unix)` gates or
  rewrites.

## Functional work after it compiles

### Web view (done)

`app/src/web/windows.rs` adapts WebView2 (`webview2-com` 0.39,
`windows` 0.62): an environment and controller per page, the child HWND
overlaid on a GTK anchor widget via `frame::follow`, all requests routed
through `WebResourceRequested` so `mailrs:`/`mailrs-cid:` serve from the
app and remote loads hit the compiled filter. Injected scripts run
after each `NavigationCompleted` (WebView2's
`AddScriptToExecuteOnDocumentCreated` does not fire for
`NavigateToString`), the `window.webkit.messageHandlers` shim bridges
to `chrome.webview.postMessage`, script dialogs are answered in
`ScriptDialogOpening` rather than shown, and GTK focus moves to the
webview's HWND so `document.hasFocus()` and focus events work.
`reading` keeps `IsScriptEnabled(true)` (markup scripts stay blocked
by the CSP `render` emits); `sealed` turns it off.

`app/src/unsubscribe_page/windows.rs` reuses the same runtime over a
message-only HWND: no visible page, same request filtering and script
support, and the hidden `Engine` the unsubscribe flow drives.

### Tray (done)

`ksni` is D-Bus-only, so the menu model became platform-neutral:
`MailTray::menu_model` yields `MenuRow`s that the ksni `menu()` and the
Windows adapter both draw. `tray::Handle` aliases `ksni::Handle` on Unix
and the `tray/windows.rs` handle on Windows, which wraps `tray-icon` +
`muda`: the crate owns a hidden window whose messages GTK's main loop
pumps, `update` marks shared state dirty, and a glib loop redraws icon,
tooltip and menu. Icons come from the exe's Win32 resources 1 and 2
(windres on gnu, embed-resource on msvc).

### Notifications (done)

`notifications/windows.rs` posts WinRT toasts: `ToastGeneric` XML with
the click payload serialized into `launch`/`arguments` attributes, and
an in-process `Activated` handler feeding the same app actions GIO
uses. A Start-menu shortcut carrying `AppUserModel_ID` is created at
startup because unpackaged toasts need it; `withdraw` maps to `Hide` by
tag, and reminders use `scenario="reminder"`.

A toast clicked after the app quit starts the exe again. The installer
gives the Start Menu shortcut `AppUserModelToastActivatorCLSID`
`{7B4D8E4A-3C1F-4F6E-9A2B-5D1E0C9F8A61}` and writes
`HKCU\Software\Classes\CLSID\{...}\LocalServer32` with the exe's path;
Windows launches that with `-Embedding` and the app answers the
activation. Uninstall removes the key. Run from a development folder, the
app writes the same key itself.

`dock_badge` draws the unread count into an overlay icon itself
(`ITaskbarList3::SetOverlayIcon` on each top-level HWND).

### Autostart (done)

`autostart.rs` writes XDG desktop entries on Unix; on Windows the login
item is `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` value
`Penguin Mail`, set to `"<exe>" --background`. The public `path`,
`is_enabled` and `apply` keep their shape; `path` returns the value
name as a token.

### Single instance and handoff

`main.rs` hands `--compose`, `mailto:` and `.ics`/`.eml` opens to the
running copy through `gio::Application` registration, over the session
bus GLib autolaunches with `gdbus.exe`. `package-windows.sh` stages it
beside the exe; without it every launch is a full copy of its own.
Verified on Windows 11: a second launch with `--compose mailto:...`
opens a composer in the running copy and exits. The smoke job starts the
installed app and then `--compose`, and fails unless one
`penguin-mail.exe` is left after 10 seconds.

The bus outlives the app and holds files in `bin\`, which Restart
Manager cannot close, so the installer and uninstaller stop the
`gdbus.exe` that runs from this install's own `bin\` before they touch
files. A restart that hands over to a new copy (the idle restart, an
update) can find the old copy still holding the store's lock for a
moment, so the app waits up to five seconds for it.

The app also creates the named mutex `PenguinMailRunning` at start. The
installer's `AppMutex` names it, so an upgrade or uninstall waits for
the running copy instead of replacing files it holds.

### Updates

The Windows installer is an update channel like the Linux tarball.
`install.rs` `method_for` returns `WindowsInstaller` for an installed
copy. The app downloads `penguin-mail-<version>-setup.exe`, checks it
against the release's `SHA256SUMS`, and checks that file's signature with
the project key built into the app (`signature.rs`). The installer ships a
native `gpgv.exe` under `libexec\gnupg\` for that, so the check is the
one Linux runs. The installer itself is not signed: Windows shows
SmartScreen's prompt on a first download. An update the app downloads
gets no prompt, since the app starts it directly. Setup writes its log
to `setup.log` beside the download, the file to ask for when an update
fails.

The app starts the installer with `/VERYSILENT /SUPPRESSMSGBOXES
/NORESTART /RELAUNCH=background` (when it was running in the tray) or
`/RELAUNCH=window`, then quits so it releases the `PenguinMailRunning`
mutex. The installer waits up to 30 seconds for that mutex in
`InitializeSetup`, installs over the old copy, and starts the app again,
with `--background` for `background`. The smoke job runs this path: it
installs, kills the app while a second setup waits, and expects exactly one
running copy afterwards.

### PGP and S-MIME (done)

Full parity means Gpg4win, and it landed: `pgp/src/gnupg.rs` `status_fd`
marks the status pipe's write end inheritable with
`SetHandleInformation` and passes the HANDLE value to `--status-fd`;
`Command::spawn` inherits it because std's Windows process setup
defaults `bInheritHandles` to true. `uninherit` takes the flag back off
after the spawn so no grandchild can hold the pipe open. Discovery is
`search_path()`: the Gpg4win and GnuPG VS-Desktop install directories
first, then PATH, with `.exe` candidates, and `is_program` rejects a
binary sitting beside `msys-2.0.dll`/`cygwin1.dll` because a POSIX build
reads `--status-fd` as a Unix fd a HANDLE cannot be (Git for Windows
ships one on PATH). `run_within`'s kill maps to a Job Object the child
is assigned to after spawn — `TerminateJobObject` stops the tree the way
killing the process group does on Unix; without `KILL_ON_JOB_CLOSE` a
run that ends in time leaves the agent alone.

The runner tests drive a stand-in Rust binary (`pgp/tests/support/
stand_in.rs`, the `gnupg-stand-in` bin target) configured by a conf file
beside it, covering the status channel, stdin copy, output cap, timeout
and process-tree kill on both platforms. The real crypto suites were
verified live against a native mingw GnuPG build (ucrt64's
`mingw-w64-ucrt-x86_64-gnupg`, the same code Gpg4win ships): every PGP
and S/MIME test passes under `PENGUIN_MAIL_REQUIRE_CRYPTO=1`, including
the CRL-fetch paths through a real dirmngr. `gpgsm` resolves `gpgconf`
beside its own exe, and only the POSIX pinentry-script paths stay
`cfg(unix)` — Windows has no scripted pinentry, so those runs use the
usual skip.

Live testing surfaced two platform truths:

- `NamedTempFile` held open by the parent denies the child's open on
  Windows (`verify` returned `FAILURE verify` = access denied). Both
  `pgp` and `smime` `read::verify` now call `into_temp_path()` before
  spawning so gpg/gpgsm can open the signature file.
- Windows' dirmngr does not answer a refused CRL fetch at once the way
  Linux's does — it retries until the app's `REVOCATION_WAIT` ends the
  run. The verdict (`RevocationUnknown`) is unchanged; the revocation
  test's timing bound widens on Windows only.
- `openssl ca` used by the CRL tests reads `\` as an escape in its
  config file; the test writes `database`/`crlnumber` paths
  forward-slashed.

`signature.rs` looks for `gpgv.exe` in `<exe dir>\..\libexec\gnupg\`
before `PATH`. That copy comes from the MSYS2 gnupg package, with its own
DLLs in the same folder.

### Skill scripts

~~bwrap does not exist on Windows.~~ Done — `sandbox/windows.rs` runs
each command in its own AppContainer: its SID opens only the skill
folder (read-only) and the conversation's scratch folder (read-write)
through ACL grants, home/mail/keys are denied by default, and the
`internetClient` capability is added only for skills allowed the
network. The invocation revokes its grants and deletes its profile when
it finishes. The shell starts suspended and must join a Job Object before
it can run. `KILL_ON_JOB_CLOSE` takes the whole tree down on timeout or
when the shell exits, and `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` keeps the
child's inherited handles to its three stdio pipes. Cygwin and MSYS2
shells cannot start inside an AppContainer (their shared-memory object
needs the global namespace), so the shell is a native busybox build,
found beside the binary, on PATH, or via `PENGUIN_MAIL_BUSYBOX`; the
installer must ship it next to the exe. The `Jail`/`run` interface is
unchanged and `packaging::runs_skills` can still turn scripts off.

### Smaller items

- ~~`domain/src/calendar/local.rs`~~: done — `system_zone` reads the
  IANA name through `iana-time-zone` (a transitive dep chrono already
  carried) after `TZ`, reusing `from_system`.
- `exe.rs`/`main.rs`: ` (deleted)` handling and `pre_exec` imports get
  `cfg(unix)`; the restart path already has `spawn + quit` for Windows.
- `Config::save`/`private_dir`/`previews`/`bridge` dirs: on Windows the
  profile directories are already per-user; the helper keeps creating
  the directory and skips the mode bits.
- Icons: generate a `.ico` from the existing PNGs at package time; set
  the tray and window icons from it.
- `build.rs`: needs `glib-compile-resources` on PATH (gvsbuild provides
  it). No change.

## Toolchain

GTK4 and libadwaita come from MSYS2 UCRT64, whose prefix provides
`glib-compile-resources`, libintl, compiled schemas, gdk-pixbuf loaders,
Adwaita icons, fonts configuration and the `.mo` catalogues.
`scripts/package-windows.sh` collects the exe plus those DLLs and data
files the same way `scripts/package-macos.sh` collects the macOS
bundle, walking the import tables breadth-first so every transitive
DLL lands in `bin/`, and adds `busybox64u.exe` beside the exe for the
skills sandbox. The staged tree mirrors a GTK prefix — `bin/` beside
`lib/`, `share/` and `etc/` — so the libraries find their data by
their own location; `main.rs` points `FONTCONFIG_PATH` at the bundled
`etc/fonts` when it exists, and both font variables go through
`_wputenv_s` because pango and fontconfig read plain `getenv`, which
Rust's `set_var` never reaches. `packaging/windows/penguin-mail.iss`
turns the tree into a per-user Inno Setup installer: Start Menu entry
with the app's AppUserModelID, the mailto/`.eml`/`.ics` capabilities
record for Default Apps, and the WebView2 evergreen bootstrapper run
post-install when the runtime is missing. Inno rather than MSIX: a
full-trust GTK app sharing folders with an external gpg and chaining
dependency installs fits the classic installer without packaged-mode
detection or filesystem virtualization getting in the way. The build ships unsigned: Penguin Mail is free
software, so each download gets one SmartScreen "Run anyway" prompt
rather than an Authenticode certificate's upkeep. Updates are checked
through the signed `SHA256SUMS` instead (see Updates).

### What the install tree holds

Besides `bin/` and the GTK data, `package-windows.sh` stages:

- `libexec\gnupg\gpgv.exe` and `libexec\poppler\pdftotext.exe` from
  MSYS2, each with its own DLLs and nothing shared with `bin\`, so
  MSYS2's GLib never meets the GTK DLLs. `stage_tool` does the copying
  by walking the import table. Their licences are under `share\licenses`.
  `pdftotext` has no poppler-data, so a PDF with unembedded Chinese,
  Japanese or Korean fonts may not give text.
- `share\hunspell`: LibreOffice's dictionaries for 16 of the app's
  languages (Japanese and Chinese have none), from a pinned commit, each
  file checked against `packaging/windows/dictionaries.sha256`. The
  composer's spell checker also reads extra pairs a person drops in a
  folder under AppData.
- `share\iso-codes\json` and the translated `iso_639-2.mo` and
  `iso_3166-1.mo`, from the pinned iso-codes 4.20.1 tarball, for language
  names in Preferences.

`scripts/stage-windows-data.sh` downloads and checks the dictionaries and
iso-codes files. To update one, change the pinned commit or version
there, then rewrite the checksum list from the downloaded files.

### Log file

A release build started from the Start Menu has no console, so the app
also writes `%LOCALAPPDATA%\penguin-mail\logs\penguin-mail.log`. At each
start the previous run moves to `penguin-mail.1.log`. Ask for this file
when someone reports a problem. The file is written from the first
warning on, so a run with nothing to report leaves the last one in place.

GLib puts caches in Internet Explorer's `INetCache` on Windows, which Disk
Cleanup empties, so `main.rs` sets `XDG_CACHE_HOME` to
`%LOCALAPPDATA%\penguin-mail\cache` before anything asks for it. Updates,
content filters and invitation files live there.

## Order of work

1. ~~Toolchain: MSYS2 UCRT64 prefix, first `cargo check`.~~ Done; the
   MSVC release build works off the gvsbuild release zip (see "MSVC
   toolchain").
2. ~~Library crates compile: `ai` (named-pipe bridge), `sync` (config
   modes), `domain` (local zone), `pgp`/`smime` Windows status-fd.~~
   Done; the runner tests are portable via the `gnupg-stand-in` binary.
3. ~~App compiles.~~ Done; all tests pass on `windows-gnu`.
4. ~~WebView2 adapter, then the unsubscribe `Engine`.~~ Done, verified
   end-to-end including the own-scheme and link-preview paths.
5. ~~Desktop integration: tray, toasts with action routing, autostart,
   AUMID.~~ Done. The smoke job checks the single-instance handoff.
6. ~~Gpg4win status-fd~~, ~~skills sandbox~~, ~~updates~~ (bundled gpgv).
   Status-fd done and verified live against a native GnuPG build; a
   winget Gpg4win install on this machine left registry entries but no
   files, so the real run used ucrt64's mingw gnupg instead. The skills
   sandbox is done: an AppContainer plus ACL grants and a job object,
   covered by the two `cfg(windows)` tests in
   `assistant/sources/skills/tests.rs`.
7. ~~Packaging script and installer choice~~, ~~signing~~, CI on a
   windows runner. Done: `scripts/package-windows.sh` stages the
   GTK-prefix tree and builds the Inno Setup installer; install,
   launch and uninstall verified silent and clean. Signing is
   deliberately out: a free-software download takes the one-time
   SmartScreen prompt instead.

## Testing on Windows

`cargo test --workspace` runs natively once GTK is installed; suites
using unix-only fs calls get `cfg(unix)`. The one-GTK-test-per-binary
rule still applies; whether `gtk::init` works in a service CI session
is checked on the first runner job. Docker-based dovecot/mailpit tests
run as on Linux when Docker is present. New adapters get the same test
shape as the macOS ones: the platform-specific logic behind a seam,
with unit tests around parsing and routing and an engine-level test
only where a display exists.

One upstream GTK bug limits where popovers can open: in a disconnected
Remote Desktop session (a user remotes into the PC, closes the client,
and the session stays logged in with no interactive display) GDK's
Win32 device enumeration can produce an empty monitor list while Win32
still reports a monitor, and
`gdk_win32_display_get_primary_monitor` unrefs the NULL that
`g_list_model_get_item` hands back for index 0, so the first
`GtkPopover` to position itself crashes the process
(`G_IS_OBJECT` critical, then an access violation). Any GTK4 app hits
it — a bare `Window` plus `Popover.popup()` reproduces it — and it is
unfixed upstream as of GTK 4.24.1.

The app guards every popup through `app/src/ui/safe_popover.rs`:
`safe_popup`/`safe_popup_at` route `gtk::Popover` popups, and
`may_present` guards `MenuButton`/`SplitButton::popup()` calls, whose
internal popovers reach the same GDK path. Every popup call site goes
through them (calendar popovers and quick-add, autocomplete, search
suggestions, sidebar and thread-list menus, the composer editor and
protection menu, the conversation message menus and its tag shortcuts,
the calendar-list row menus). On a monitor-less display the popover
simply never opens instead of crashing — and a session with no
monitors has nobody to show it to anyway. The `richbuffer` GTK checks
skip on the same condition, the same way they skip with no display at
all.

The guard cannot cover popups GTK opens itself (tooltips, a
`MenuButton` the user clicks, combo-box lists), so production Windows
builds should ship a patched GTK.
`packaging/windows/gtk4-zero-monitor-popup.patch` fixes it downstream;
it was written against GTK 4.24.1 and applies unchanged to the 4.20.3
source the gvsbuild bundle carries: when both enumeration passes in
`_gdk_win32_display_get_monitor_list` produce zero monitors — under
disconnected RDP the session's adapter is not attached to the desktop,
so the existing madeup-monitor retry never fires — it fabricates one
covering the Win32 virtual screen (`GetSystemMetrics`), and it NULL-
checks the three dereference sites (`get_primary_monitor`'s unref,
`gdk_win32_surface_layout_popup`'s workarea read, and the
`gdk_monitor_set_scale_factor` call in `gdkevents-win32.c`). With it,
`monitors()` reports the madeup monitor and popovers actually present
in the session instead of being skipped. The release build applies it with
`scripts/build-gtk4-msys2.sh`, which rebuilds MSYS2's gtk4 package with
the patch and stops if the recipe moves GTK 4 off 4.24.2 until the patch
is checked against the new version. A console session keeps
the monitor list populated, and whether an interactive RDP session
does too is unchecked.

## Open items

- Signing: out of scope. The installer ships unsigned and the README
  explains SmartScreen.
- ARM64: out of scope.
- Gpg4win: the app finds an installed copy for PGP and S/MIME keys a
  person already has. The winget Gpg4win install here registered but wrote
  no files; worth a second look on a clean machine.
- The release job's gtk4 package cache is written only by runs on main.
  After a change to the patch or to `build-gtk4-msys2.sh`, merge to main
  before tagging, or the release builds the gtk4 package from scratch.
- Everything under "Checks on a Windows VM" below is run by hand before a
  release that touches Windows.

## What CI checks

`.github/workflows/windows.yml` runs on every pull request:

- `native`: builds the app test binary with MSYS2 UCRT64 and runs its
  whole suite on one thread, the AppContainer sandbox tests included,
  then clippy. BusyBox is built from a pinned upstream release commit.
  It also compiles a placeholder installer and checks what it installs
  and what uninstall removes.
- `installer`: rebuilds MSYS2's gtk4 package with the monitor patch
  (cached), builds the app with MSYS2 UCRT64, and uploads
  `penguin-mail-<version>-setup.exe`. The release workflow calls the same
  job.
- `smoke`: installs that file silently, checks the bundled helpers, data
  and the toast activator key, starts the app with `--demo`, takes
  screenshots, checks that a second launch hands over, updates over the
  running app, and uninstalls. It fails if a file, shortcut, Run value or
  registry key is left behind. The screenshots are an artifact.

## Checks on a Windows VM

CI cannot judge how the app looks or behaves under a real session, so a
Windows 11 VM runs these before a release: first start; light and dark;
100 % and 150 % scaling; tray; toasts with actions, and a toast clicked
after quitting; a second launch and a `mailto:` click; opening an `.eml`;
an IMAP account against a local Dovecot (sign in, sync, send, move); PGP
sign and verify with the bundled gpgv; spell checking; the assistant
reading a PDF; an attachment with a hostile name; an opened `.js`
attachment, which Windows must warn about; an update from one version to
the next through `PENGUIN_MAIL_RELEASES_URL` pointing at a local mirror;
and an uninstall that leaves nothing behind.

On 2026-11-26 these ran on Windows 11 25H2 (a KVM VM, 1280x800, 100 % and
150 %, the MSYS2 build), with Dovecot, Mailpit and Radicale in containers
on the host and the update signed by a throwaway key in a test build. All
passed except two that were not run in the app: the assistant reading a
PDF (only the bundled `pdftotext.exe` was run by hand) and PGP sign and
verify (only the crate tests, against MSYS2's GnuPG). Not run either:
Google and Microsoft sign-in, an MCP server started as `npx`, Claude Code
installed from npm, and a Gpg4win install.

That run found and fixed: a missing WebView2Loader.dll, the SVG loader
path, GLib's missing spawn helpers (no link opened), the window size on a
small screen, a firewall prompt from DNS lookups, the idle restart's lock
race, a tray that ignored clicks after a background start, double and lost
toast clicks, an update notice whose button did nothing, the exe's name,
the cache folder, a blank page in a newly opened mail window, timers
throttled in the hidden unsubscribe browser, and the session bus stopping
updates.

Since then, also checked live: the tray menu follows dark mode (popup
menus take uxtheme's preferred app mode, set once at start and flushed when
the theme changes), and a mail notification clicked after the app quit
opens that mail, through the `show-thread` action its click carries.
