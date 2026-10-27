# AppImage

The x86_64 AppImage targets Ubuntu 22.04 and newer, and Debian 13. It
carries the current app, GTK, libadwaita, WebKitGTK and their runtime
libraries. The dependency requirements for the app and the other packages
are unchanged. This addresses the older desktop libraries in
[issue #7](https://github.com/c9dev/penguin-mail/issues/7).

Download the `.AppImage` from a release, make it executable, and run it:

```sh
chmod +x penguin-mail-*-x86_64.AppImage
./penguin-mail-1.0.9-x86_64.AppImage
```

Keep the file where you want it before enabling Start at Login. The login
item names that file, including when its name contains spaces. If you move
or rename it later, turn Start at Login off and on again. A restart reuses
the running copy's files, so it needs neither a second mount nor a second
extraction.
Download a new AppImage to update. This package does not install `.deb`
updates or overwrite another installation.

The desktop supplies its display server, D-Bus services, keyring,
bubblewrap and xdg-dbus-proxy. Install missing desktop services with your distribution's
package manager. To sign in to Gmail through the Google account on your
desktop, install `gnome-online-accounts`, and on a desktop other than GNOME
also `gnome-online-accounts-gtk`. GnuPG and S/MIME use the system's `gpg` and `gpgsm`;
install `gnupg` and `gpgsm` if you need them. Claude Code and command-based
MCP servers also use programs installed on your computer.

WebKit and image loaders retain their bubblewrap sandboxes. The AppImage
uses `/usr/bin/bwrap`, including its distribution-provided AppArmor profile.
Ubuntu 22.04, Ubuntu 25.04 and newer, and Debian 13 need no changes.

Ubuntu 24.04 lets a program make the namespaces bubblewrap needs only when
an AppArmor profile names it, and it has none for bubblewrap. Penguin Mail
checks at startup, and where bubblewrap is stopped it shows the command
that allows this one file, and starts once you choose Check Again. The
command writes this profile, with your file's path, to
`/etc/apparmor.d/penguin-mail-appimage` and loads it:

```text
abi <abi/4.0>,
include <tunables/global>

profile penguin-mail-appimage "/home/you/Apps/penguin-mail-1.0.9-x86_64.AppImage" flags=(unconfined) {
  userns,
}
```

Download a new version to the same path, or run the command again for the
new file. To undo it, run
`sudo apparmor_parser -R /etc/apparmor.d/penguin-mail-appimage`, then
delete that file. The Flatpak and the snap
need no setup on Ubuntu 24.04. If bubblewrap cannot run for another
reason, report its error rather than disabling the sandbox.

If FUSE is unavailable, use extraction mode:

```sh
./penguin-mail-1.0.9-x86_64.AppImage --appimage-extract-and-run
```

Start at Login then runs the file with the same flag. This mode unpacks
about 1.1 GB into the temporary folder, which many systems keep in memory,
and keeps it there while Penguin Mail runs. Quitting frees it, and older
versions' folders too. After a logout the folder stays until the next
start, which reuses it.

If you run without FUSE every day, extract once instead, with
`--appimage-extract`. Move `squashfs-root` to a permanent location on disk
and run its `AppRun`. Keep the whole directory.

## Building

Build on x86_64 Ubuntu 26.04 with the normal development dependencies,
Python 3, curl, the GLib command-line tools, Mesa EGL/DRI, GLES, glycin
loaders, glib-networking, Adwaita icons and DejaVu fonts. The AppImage
workflow lists the exact package names and pins the build image and
packaging tools. Its package manifest records the runtime package and
source versions. It uses the same OAuth build inputs as other release
jobs. No signing key is available during compilation or packaging.

Use a separate target directory and build the AppImage feature:

```sh
export CARGO_TARGET_DIR=target/appimage
cargo build --locked --release -p mailrs -p mailrs-cli --features mailrs/packaging-appimage
scripts/stage.sh /tmp/penguin-appimage-tree
scripts/appimage.sh /tmp/penguin-appimage-tree 1.0.9 dist
```

Do not use this build tree for `.deb`, rpm or tarball packaging. Their
jobs build without `packaging-appimage`.

The launcher invokes the bundled dynamic loader with a private library
path. A process-local adapter supplies resource paths and relocates the
known WebKit helper commands. It preserves WebKit's sandbox arguments,
seccomp descriptors and IPC descriptors. A static image-loader adapter
adds the bundle read-only to glycin's sandbox and selects the same loader.
Neither adapter replaces the host's bubblewrap. Runtime search paths are
not exported to host child programs.

The bundle includes both app executables, translations, icons, MIME data,
schemas, image loaders, TLS support, CA certificates, licenses and a package
manifest under `usr/share/doc/penguin-mail`.

## Validation and limits

`scripts/test-appimage.sh <artifact> <pinned-base-image>` builds a disposable
desktop test container and checks version output, extraction, a name with
spaces, HTML, an inline image, sandboxed SVG decoding, HTTPS certificate
verification, host GnuPG, the host environment, and a demo window.
`packaging/appimage/relaunch.py` then drives the app through AT-SPI with
and without FUSE. It checks the language restart, the idle restart, the
Claude Code bridge command and the login item, and that none of them
mounts or unpacks the image a second time. Quitting must free the
unpacked files.
The AppImage workflow runs it on Ubuntu 22.04, Ubuntu 24.04 and Debian 13.
CI calls that workflow when the app, its packaging or Cargo changes, and
every week; a release builds the published file with it and waits for CI.

The outer test container allows user namespaces and unmasked system paths
so WebKit and glycin can create their own sandboxes. A second container
also gets `/dev/fuse` and the right to mount, for the mounted run.
These Docker options apply only to the test harness; they are not
installation instructions.
Container tests share the Docker host's kernel. They do not establish
compatibility with each distribution's desktop AppArmor policy, physical
GPU drivers, Wayland compositor, keyring dialogs, account sign-in, or
notification and tray implementations. Those paths need desktop testing
before declaring the release supported on every such configuration.
