#!/usr/bin/env python3
"""Exercise missing signing settings and dependency resolution without Apple credentials."""

import importlib.util
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch

scripts = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("bundle", scripts / "bundle-macos.py")
bundle = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bundle)


class Packaging(unittest.TestCase):
    @unittest.skipUnless(os.uname().sysname == "Darwin", "NSBundle is a macOS API")
    def test_local_install_keeps_the_executable_in_its_app_bundle(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder).resolve()
            (root / "scripts").mkdir()
            shutil.copy2(scripts / "install-macos.sh", root / "scripts")
            (root / "Cargo.toml").write_text('version = "1.2.3"\n')
            binary = root / "target/release/penguin-mail"
            binary.parent.mkdir(parents=True)
            source = root / "probe.m"
            source.write_text('''#import <Foundation/Foundation.h>
int main(void) {
    @autoreleasepool {
        NSBundle *bundle = NSBundle.mainBundle;
        if (![bundle.bundleIdentifier isEqualToString:@"io.github.c9dev.PenguinMail"])
            return 1;
        NSString *actual = [NSProcessInfo.processInfo.arguments[0] stringByResolvingSymlinksInPath];
        if (![bundle.executablePath.stringByResolvingSymlinksInPath isEqualToString:actual])
            return 2;
    }
    return 0;
}
''')
            subprocess.run(["clang", "-framework", "Foundation", str(source), "-o", str(binary)], check=True)
            commands = root / "bin"
            commands.mkdir()
            for name, body in {
                "cargo": "exit 0\n",
                "brew": 'if [ "$1" = --prefix ]; then echo "/test/Homebrew prefix"; fi\n',
                "rsvg-convert": 'while [ "$1" != -o ]; do shift; done; touch "$2"\n',
                "iconutil": 'while [ "$1" != -o ]; do shift; done; touch "$2"\n',
            }.items():
                command = commands / name
                command.write_text("#!/bin/sh\n" + body)
                command.chmod(0o755)
            env = os.environ | {"PATH": str(commands) + ":" + os.environ["PATH"], "APP_DIR": str(root)}
            subprocess.run([str(root / "scripts/install-macos.sh")], env=env, check=True, capture_output=True)
            app = root / "Penguin Mail.app"
            info = plistlib.loads((app / "Contents/Info.plist").read_bytes())
            self.assertEqual(info["LSEnvironment"]["XDG_DATA_DIRS"], "/test/Homebrew prefix/share:/usr/local/share:/usr/share")
            subprocess.run(["codesign", "--verify", "--deep", "--strict", str(app)], check=True)
            subprocess.run([str(app / "Contents/MacOS" / info["CFBundleExecutable"])], check=True)

    def test_dependency_paths_resolve_from_the_owner(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder).resolve()
            library = root / "lib example.dylib"
            library.touch()
            owner = root / "page"
            self.assertEqual(bundle.resolve("@loader_path/lib example.dylib", owner, owner), library)
            with patch.object(bundle, "rpaths", return_value=["@loader_path"]):
                self.assertEqual(bundle.resolve("@rpath/lib example.dylib", owner, owner), library)
            with self.assertRaises(RuntimeError):
                bundle.resolve("missing.dylib", owner, owner)

    @unittest.skipUnless(os.uname().sysname == "Darwin", "PlistBuddy is a macOS tool")
    def test_missing_or_partial_credentials_skip_apple_and_keep_signing_failures(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder).resolve()
            commands = root / "bin"
            commands.mkdir()
            app = root / "Penguin Mail.app"
            (app / "Contents/Frameworks").mkdir(parents=True)
            (app / "Contents/Frameworks/example.dylib").touch()
            (app / "Contents/Info.plist").write_bytes(plistlib.dumps({"CFBundleShortVersionString": "1.2.3"}))
            for name, body in {
                "codesign": '#!/bin/sh\nexit "${SIGN_EXIT:-0}"\n',
                "ditto": '#!/bin/bash\ntouch "${@: -1}"\n',
                "security": '#!/bin/sh\nexit 99\n',
                "xcrun": '#!/bin/sh\nexit 99\n',
            }.items():
                script = commands / name
                script.write_text(body)
                script.chmod(0o755)
            env = {k: v for k, v in os.environ.items() if not k.startswith(("APPLE_", "MACOS_CERTIFICATE"))}
            env["PATH"] = str(commands) + ":" + env["PATH"]
            for certificate in ("", "partial-settings"):
                env["MACOS_CERTIFICATE_P12"] = certificate
                result = subprocess.run([str(scripts / "sign-macos.sh"), str(app), str(root)],
                                        env=env, capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("unsigned", result.stdout)
            self.assertEqual(len(list(root.glob("*-unsigned.zip"))), 1, result.stdout + str(list(root.iterdir())))
            env["SIGN_EXIT"] = "42"
            result = subprocess.run([str(scripts / "sign-macos.sh"), str(app), str(root)],
                                    env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 42)

    @unittest.skipUnless(os.uname().sysname == "Darwin", "PlistBuddy is a macOS tool")
    def test_a_notary_rejection_never_produces_a_release_archive(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder).resolve()
            commands = root / "bin"
            commands.mkdir()
            app = root / "Penguin Mail.app"
            (app / "Contents/Frameworks").mkdir(parents=True)
            (app / "Contents/Info.plist").write_bytes(plistlib.dumps({"CFBundleShortVersionString": "1.2.3"}))
            for name, body in {
                "codesign": '#!/bin/sh\nexit 0\n',
                "ditto": '#!/bin/bash\ntouch "${@: -1}"\n',
                "security": '#!/bin/sh\nif [ "$1" = find-identity ]; then echo \'1) ABCDEF "Developer ID Application: Test"\'; fi\n',
                "xcrun": '#!/bin/sh\nif [ "$1" = notarytool ]; then printf \'{"status":"%s","id":"test"}\\n\' "$NOTARY_STATUS"; fi\n',
                "spctl": '#!/bin/sh\nexit 0\n',
            }.items():
                script = commands / name
                script.write_text(body)
                script.chmod(0o755)
            env = os.environ | {
                "PATH": str(commands) + ":" + os.environ["PATH"],
                "MACOS_CERTIFICATE_P12": "dGVzdA==", "MACOS_CERTIFICATE_PASSWORD": "test",
                "APPLE_ID": "test@example.invalid", "APPLE_TEAM_ID": "TEST",
                "APPLE_APP_SPECIFIC_PASSWORD": "test", "NOTARY_STATUS": "Invalid",
            }
            run = lambda: subprocess.run([str(scripts / "sign-macos.sh"), str(app), str(root)],
                                         env=env, capture_output=True, text=True)
            rejected = run()
            self.assertNotEqual(rejected.returncode, 0)
            self.assertIn("ended with Invalid", rejected.stderr)
            self.assertFalse(list(root.glob("*.zip")))
            env["NOTARY_STATUS"] = "Accepted"
            accepted = run()
            self.assertEqual(accepted.returncode, 0, accepted.stderr)
            archives = list(root.glob("*.zip"))
            self.assertEqual(len(archives), 1)
            self.assertNotIn("unsigned", archives[0].name)


if __name__ == "__main__":
    unittest.main()
