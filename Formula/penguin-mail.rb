class PenguinMail < Formula
  desc "Mail and calendar for Linux and macOS, written in Rust"
  homepage "https://penguin-mail.com"
  license "GPL-3.0-or-later"

  version "1.0.12"

  on_arm do
    url "https://github.com/c9dev/penguin-mail/releases/download/v1.0.12/penguin-mail-1.0.12-macos-arm64-unsigned.zip"
    sha256 "b2dcf2e17dda069434afa2b96b7109813d244df67a2ed070e1c6259c4948215f"
  end

  on_intel do
    url "https://github.com/c9dev/penguin-mail/releases/download/v1.0.12/penguin-mail-1.0.12-macos-x86_64-unsigned.zip"
    sha256 "1629b3450131c4fc3d9011f43b41074d8d15d30256d846c05abb34b5caa77ef6"
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
