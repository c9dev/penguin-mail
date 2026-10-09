#!/usr/bin/env python3
"""Unpack the release elsewhere and start its demo with Homebrew unreadable."""

import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile

spec = importlib.util.spec_from_file_location("bundle", Path(__file__).with_name("bundle-macos.py"))
bundle = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bundle)


def check(archive):
    with tempfile.TemporaryDirectory(prefix="Penguin Mail relocation ") as directory:
        root = Path(directory).resolve()
        subprocess.run(["ditto", "-x", "-k", str(archive), str(root)], check=True)
        app = root / "Penguin Mail.app"
        binary = app / "Contents/MacOS/penguin-mail"
        subprocess.run(["codesign", "--verify", "--deep", "--strict", str(app)], check=True)
        for image in [binary, *sorted((app / "Contents/Frameworks").iterdir())]:
            assert not bundle.rpaths(image), f"Unresolved library search paths: {image}"
            for name in bundle.links(image):
                if bundle.system_library(name) or name == "@rpath/" + image.name:
                    continue
                assert name.startswith("@loader_path/"), f"External dependency: {name}"
                resolved = (image.parent / name.removeprefix("@loader_path/")).resolve()
                assert resolved.is_relative_to(app) and resolved.is_file(), name
        for path in app.rglob("*"):
            assert not path.is_symlink(), f"Unresolved resource link: {path}"
        home = root / "home"
        home.mkdir()
        (home / "gnupg").mkdir(mode=0o700)
        env = os.environ | {
            "HOME": str(home), "GNUPGHOME": str(home / "gnupg"),
            "XDG_CACHE_HOME": str(home / "cache"),
            "XDG_CONFIG_HOME": str(home / "config"),
            "XDG_DATA_HOME": str(home / "data"),
            "PATH": "/usr/bin:/bin:/usr/sbin:/sbin",
        }
        policy = '(version 1)(allow default)(deny file-read* (subpath "/opt/homebrew") (subpath "/usr/local"))'
        command = ["/usr/bin/sandbox-exec", "-p", policy, str(binary)]
        subprocess.run([*command, "--version"], env=env, cwd=root, check=True, timeout=30)
        with (root / "demo.log").open("w+") as log:
            demo = subprocess.Popen([*command, "--demo"], env=env, cwd=root, stdout=log, stderr=log)
            try:
                try:
                    demo.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    pass
                else:
                    log.seek(0)
                    raise RuntimeError(f"The demo exited with {demo.returncode}: {log.read()}")
            finally:
                if demo.poll() is None:
                    demo.terminate()
                    try:
                        demo.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        demo.kill()
                        demo.wait()
            log.seek(0)
            text = log.read()
            for failure in ("Library not loaded", "Unable to load image-loading module",
                            "Could not load a pixbuf", "could not prepare the application bundle"):
                assert failure not in text, text
        print(f"{archive.name}: relocated app starts with no Homebrew access.")


if __name__ == "__main__":
    for argument in sys.argv[1:]:
        check(Path(argument).resolve())
