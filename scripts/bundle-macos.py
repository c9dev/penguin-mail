#!/usr/bin/env python3
"""Make install-macos.sh's app independent of the build machine's Homebrew."""

import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import sys


def output(*args):
    return subprocess.check_output(args, text=True)


def system_library(name):
    return name.startswith(("/System/Library/", "/usr/lib/"))


def links(path):
    return [line.strip().split(" (compatibility", 1)[0]
            for line in output("otool", "-L", str(path)).splitlines()[1:]]


def rpaths(path):
    return re.findall(r"cmd LC_RPATH\n\s+cmdsize \d+\n\s+path (.*?) \(offset",
                      output("otool", "-l", str(path)))


def resolve(name, source, executable):
    def expand(value):
        return value.replace("@loader_path", str(source.parent)).replace(
            "@executable_path", str(executable.parent))
    if name.startswith("@rpath/"):
        for folder in rpaths(source) + rpaths(executable):
            candidate = Path(expand(folder)) / name.removeprefix("@rpath/")
            if candidate.is_file():
                return candidate.resolve()
        raise RuntimeError(f"Cannot resolve {name} from {source}")
    candidate = Path(expand(name))
    if not candidate.is_absolute() or not candidate.is_file():
        raise RuntimeError(f"Cannot resolve {name} from {source}")
    return candidate.resolve()


def relocate(contents, executable, modules):
    frameworks = contents / "Frameworks"
    frameworks.mkdir()
    copied = {}
    sources = {}

    def copy(source):
        source = source.resolve()
        if source in copied:
            return copied[source]
        target = frameworks / source.name
        if target.name in sources and sources[target.name] != source:
            raise RuntimeError(f"Two libraries have the same name: {target.name}")
        sources[target.name] = source
        shutil.copy2(source, target)
        target.chmod(0o755)
        copied[source] = target
        rewrite(source, target)
        return target

    minimum = [14, 0]

    def rewrite(source, target):
        nonlocal minimum
        commands = output("otool", "-l", str(source))
        versions = re.findall(r"cmd LC_BUILD_VERSION\n\s+cmdsize \d+\n\s+platform \d+\n\s+minos ([\d.]+)", commands)
        versions += re.findall(r"cmd LC_VERSION_MIN_MACOSX\n\s+cmdsize \d+\n\s+version ([\d.]+)", commands)
        for version in versions:
            minimum = max(minimum, [int(part) for part in version.split(".")])
        changes = []
        for name in links(source):
            if system_library(name):
                continue
            dependency = resolve(name, source, executable)
            if dependency == source:
                continue
            bundled = copy(dependency)
            relative = os.path.relpath(bundled, target.parent)
            changes += ["-change", name, "@loader_path/" + relative]
        if target.parent == frameworks:
            changes += ["-id", "@rpath/" + target.name]
        for folder in rpaths(source):
            changes += ["-delete_rpath", folder]
        if changes:
            subprocess.run(["install_name_tool", *changes, str(target)], check=True)

    rewrite(executable, executable)
    for module in modules:
        copy(module)
    return copied, ".".join(map(str, minimum))


def build(app):
    root = Path(__file__).resolve().parent.parent
    brew = Path(output("brew", "--prefix").strip())
    contents = app / "Contents"
    resources = contents / "Resources"
    executable = contents / "MacOS/penguin-mail"
    shutil.move(resources / "penguin-mail", executable)
    (contents / "MacOS/penguin-mail-launcher").unlink()
    shutil.rmtree(contents / "_CodeSignature", ignore_errors=True)

    # Query every installed loader, including librsvg's module.
    module_dir = Path(output("pkg-config", "--variable=gdk_pixbuf_moduledir",
                             "gdk-pixbuf-2.0").strip())
    modules = sorted(module_dir.glob("*.so"))
    if not any("svg" in p.name for p in modules):
        raise RuntimeError("librsvg's GdkPixbuf loader is missing")
    cache = output(str(brew / "bin/gdk-pixbuf-query-loaders"), *map(str, modules))
    copied, minimum = relocate(contents, executable, modules)
    for module in modules:
        cache = cache.replace(str(module), "@CONTENTS@/Frameworks/" + copied[module.resolve()].name)
    cache = "\n".join(line for line in cache.splitlines() if not line.startswith("#")) + "\n"
    (resources / "loaders.cache.in").write_text(cache)

    share = resources / "share"
    for theme in ("Adwaita", "hicolor"):
        shutil.copytree(brew / "share/icons" / theme, share / "icons" / theme,
                        symlinks=False, dirs_exist_ok=True)
    shutil.copytree(root / "app/data/icons", share / "icons/hicolor", dirs_exist_ok=True)
    schemas = share / "glib-2.0/schemas"
    schemas.mkdir(parents=True)
    # Only GTK's schemas are needed; unrelated installed apps stay outside.
    for schema in (brew / "opt/gtk4/share/glib-2.0/schemas").glob("*.xml"):
        shutil.copy2(schema, schemas)
    subprocess.run([str(brew / "bin/glib-compile-schemas"), str(schemas)], check=True)
    (resources / "gio/modules").mkdir(parents=True)
    fonts = resources / "etc/fonts"
    shutil.copytree(brew / "etc/fonts", fonts, symlinks=False)
    config = fonts / "fonts.conf"
    config.write_text(re.sub(r"\s*<cachedir>/[^<]+</cachedir>", "", config.read_text()))
    locale = share / "locale"
    for formula in ("glib", "gtk4", "libadwaita", "gdk-pixbuf"):
        source = brew / "opt" / formula / "share/locale"
        if source.is_dir():
            shutil.copytree(source, locale, dirs_exist_ok=True)
    for po in (root / "po").glob("*.po"):
        dest = locale / po.stem / "LC_MESSAGES/penguin-mail.mo"
        dest.parent.mkdir(parents=True, exist_ok=True)
        subprocess.run([str(brew / "opt/gettext/bin/msgfmt"), str(po), "-o", str(dest)], check=True)
    shutil.copy2(root / "LICENSE", resources / "LICENSE")
    # Keep the exact source packages and their license metadata with the binary.
    formulae = set()
    for source in copied:
        if "Cellar" in source.parts:
            formulae.add(source.parts[source.parts.index("Cellar") + 1])
    for formula in sorted(formulae | {"adwaita-icon-theme", "hicolor-icon-theme"}):
        keg = (brew / "opt" / formula).resolve()
        notices = resources / "licenses" / formula
        notices.mkdir(parents=True)
        for notice in keg.iterdir():
            if notice.is_file() and notice.name.startswith(("COPYING", "LICENSE", "LICENCE", "LGPL", "GPL", "AUTHORS")):
                shutil.copy2(notice, notices)
    (resources / "homebrew-packages.json").write_text(
        output("brew", "info", "--json=v2", *sorted(formulae)))
    info_file = contents / "Info.plist"
    info = plistlib.loads(info_file.read_bytes())
    info["CFBundleExecutable"] = "penguin-mail"
    info["LSMinimumSystemVersion"] = minimum
    info_file.write_bytes(plistlib.dumps(info))
    print(f"Bundled {len(copied)} libraries and loaders; requires macOS {minimum} or later.")


if __name__ == "__main__":
    build(Path(sys.argv[1]).resolve())
