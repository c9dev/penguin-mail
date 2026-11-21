#!/bin/sh
# Regenerates Formula/penguin-mail.rb from a published release. The two
# macOS asset names and their sha256 digests come from the GitHub release,
# so the formula matches what the release actually ships, including the
# -unsigned suffix while the builds are unsigned.
#
#   scripts/brew-formula.sh [version]   default: the latest release
#
# The Release workflow runs it after the publish job and commits the
# rewritten formula to main.
set -eu

cd "$(dirname "$0")/.."

repo=${PENGUIN_MAIL_REPO:-c9dev/penguin-mail}
api=${GITHUB_API_URL:-https://api.github.com}

fetch() {
    if [ -n "${GITHUB_TOKEN:-}" ]; then
        curl -fsSL -H "Authorization: Bearer $GITHUB_TOKEN" "$api$1"
    else
        curl -fsSL "$api$1"
    fi
}

version=${1:-}
if [ -z "$version" ]; then
    version=$(fetch "/repos/$repo/releases/latest" |
        python3 -c 'import json, sys; print(json.load(sys.stdin)["tag_name"].lstrip("v"))')
fi
version=${version#v}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
fetch "/repos/$repo/releases/tags/v$version" > "$work/release.json"

python3 - "$work/release.json" "$version" <<'PY'
import json
import sys
from pathlib import Path

release_file, version = sys.argv[1:3]
release = json.loads(Path(release_file).read_text())


def asset(arch):
    prefix = f"penguin-mail-{version}-macos-{arch}"
    names = [a["name"] for a in release.get("assets", [])
             if a["name"].startswith(prefix) and a["name"].endswith(".zip")]
    if len(names) != 1:
        raise SystemExit(
            f"Release {release.get('tag_name')} has {len(names)} {arch} zips; one is needed: {names}")
    found = next(a for a in release["assets"] if a["name"] == names[0])
    digest = found.get("digest") or ""
    if not digest.startswith("sha256:"):
        raise SystemExit(f"{names[0]} has no sha256 digest")
    return found["browser_download_url"], digest.removeprefix("sha256:")


arm_url, arm_sha = asset("arm64")
intel_url, intel_sha = asset("x86_64")

formula = f'''class PenguinMail < Formula
  desc "Mail and calendar for Linux and macOS, written in Rust"
  homepage "https://penguin-mail.com"
  license "GPL-3.0-or-later"

  version "{version}"

  on_arm do
    url "{arm_url}"
    sha256 "{arm_sha}"
  end

  on_intel do
    url "{intel_url}"
    sha256 "{intel_sha}"
  end

  livecheck do
    url "https://github.com/c9dev/penguin-mail/releases.atom"
    strategy :github_releases
  end

  def install
    # The release zip is one self-contained app with its libraries. Homebrew
    # keeps the app under its own prefix and the launcher on the PATH, so
    # both are upgradeable and uninstallable by Homebrew.
    libexec.install "Penguin Mail.app"
    (bin/"penguin-mail").write <<~EOS
      #!/bin/sh
      exec "$(dirname "$0")/../libexec/Penguin Mail.app/Contents/MacOS/penguin-mail" "$@"
    EOS
  end

  def caveats
    <<~EOS
      The app lives at $(brew --prefix)/libexec/Penguin Mail.app.

      The build is not notarized, so macOS blocks the first launch.
      Clear its quarantine attribute once and it opens normally:
        xattr -cr "$(brew --prefix)/libexec/Penguin Mail.app"

      To put the app in Applications:
        cp -R "$(brew --prefix)/libexec/Penguin Mail.app" /Applications/

      brew upgrade penguin-mail brings each new version.
    EOS
  end

  test do
    system bin/"penguin-mail", "--version"
  end
end
'''

path = Path("Formula/penguin-mail.rb")
path.parent.mkdir(exist_ok=True)
if path.exists() and path.read_text() == formula:
    print(f"Formula/penguin-mail.rb already installs {version}.")
else:
    path.write_text(formula)
    print(f"Wrote Formula/penguin-mail.rb for {version}.")
PY
