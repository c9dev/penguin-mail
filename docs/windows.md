# Windows port

Penguin Mail on Windows: `x86_64-pc-windows-msvc` for release,
`x86_64-pc-windows-gnu` (MSYS2 UCRT64) for development, Windows 10 or
later, with feature parity as the goal. WebView2 draws mail HTML, an
Inno Setup installer carries it for direct downloads, and PGP/S-MIME
and skill scripts are in scope.

## Building

From an MSYS2 or Git Bash shell, one command covers the release:

```sh
scripts/build-windows.sh --prefix D:/gtk-msvc/prefix
```

It finds Visual Studio through vswhere — any 2022 edition with the C++
workload, so no edition-specific path is hard-coded — imports the MSVC
environment, builds `target/x86_64-pc-windows-msvc/release/
penguin-mail.exe`, and hands the exe to `scripts/package-windows.sh`,
which stages the install tree at `dist/penguin-mail/` and compiles
`dist/penguin-mail-<version>-setup.exe` with Inno Setup 6.

What a machine needs:

- **Rust** with the `x86_64-pc-windows-msvc` target installed.
- **Visual Studio 2022** with the C++ workload: Build Tools is enough.
- **A GTK prefix**: the `GTK4_Gvsbuild_*_x64.zip` from the gvsbuild
  releases, unzipped anywhere — its `bin`, `lib`, `share` and `etc`
  sit at its root. The script rewrites the `prefix=` lines of its
  `lib/pkgconfig/*.pc` when they still name the zip's build location.
  Building the stack from source with `gvsbuild build gtk4 libadwaita
  librsvg` works the same and is the way to ship
  `packaging/windows/gtk4-zero-monitor-popup.patch`.
- **MSYS2 or Git Bash**, for bash and cygpath.
- **Inno Setup 6**, for the installer step; `--stage-only` skips it.
- **BusyBox** for the skills sandbox: downloaded to the cache once,
  or named with `--busybox`.
- The WebView2 evergreen bootstrapper is fetched at packaging time
  and embedded in the installer.

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
running copy through `gio::Application` registration. Verify
`is_remote()`/`activate_action` work on Windows builds of GIO (it has a
win32 backend that should cover this). If it does not, a named mutex
plus a named pipe carries the same handoff.

### Updates

`install.rs` gains a method that downloads the release's installer and
launches it, then exits. `signature.rs` cannot lean on `gpgv`/`gpg` on
Windows; sign the installer with Authenticode and verify it with
`WinVerifyTrust` against the embedded certificate, which also covers
the tarball checksum's job for that channel. Until signing secrets
exist, the updater stays off and About shows the usual unmanaged line.

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

`signature.rs` keeps its gpgv path on Unix and learns Authenticode on
Windows.

### Skill scripts

~~bwrap does not exist on Windows.~~ Done — `sandbox/windows.rs` runs
each command in an AppContainer: the package SID opens only the skill
folder (read-only) and the conversation's scratch folder (read-write)
through ACL grants, home/mail/keys are denied by default, and the
`internetClient` capability is added only for skills allowed the
network. A Job Object with `KILL_ON_JOB_CLOSE` takes the whole tree
down on timeout, and `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` keeps the
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

GTK4 and libadwaita for MSVC come from gvsbuild (MSYS2 works too but its
gnu toolchain mixes badly with MSVC). The release zip's prefix provides
`glib-compile-resources`, libintl for `GETTEXT_DIR`, compiled schemas,
gdk-pixbuf loaders, Adwaita icons, fonts configuration and the `.mo`
catalogues. Building the same stack from source with `gvsbuild build
gtk4 libadwaita` remains the fallback when the zip drifts out of date.
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
rather than an Authenticode certificate's upkeep. With nothing signed
there is nothing for `signature.rs` to verify against, so the updater
stays off and About shows the usual unmanaged line; a new installer
download is the update path.

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
   AUMID.~~ Done. Single-instance handoff still needs a check that
   GIO's win32 backend answers `is_remote`/`activate_action`.
6. ~~Gpg4win status-fd~~, ~~skills sandbox~~, updates+Authenticode.
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
in the session instead of being skipped. Apply it inside the GTK
source tree (`patch -p1`) before `gvsbuild build gtk4`; the
prebuilt gvsbuild zip cannot take patches. A console session keeps
the monitor list populated, and whether an interactive RDP session
does too is unchecked.

## Open items

- Signing: decided out of scope — the installer ships unsigned, so
  the updater stays off (nothing to verify against).
- Packaging the MSVC build: the same `package-windows.sh` run against
  the gvsbuild prefix, plus the `msvc-env` recipe turned into a
  script. Verified so far only on the MSYS2-gnu build.
- Gpg4win: done (HANDLE status-fd, Job-Object kill, `search_path`) and
  verified against a native GnuPG build. The winget Gpg4win install here
  registered but wrote no files; worth a second look on a clean machine.
- Whether GIO's win32 backend handles `is_remote`/`activate_action` for
  the single-instance handoff.
- Whether the second-instance toast activation path needs the
  `--compose`-style CLI handoff when a click lands on a toast after the
  process exited.
- ARM64 support is out of scope for v1.
