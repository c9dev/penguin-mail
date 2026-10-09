#!/usr/bin/env python3
"""Exercise missing signing settings and dependency resolution without Apple credentials."""

import importlib.util
import os
from pathlib import Path
import plistlib
import subprocess
import tempfile
import unittest
from unittest.mock import patch

scripts = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("bundle", scripts / "bundle-macos.py")
bundle = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bundle)


class Packaging(unittest.TestCase):
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
