# macOS releases

The release workflow builds separate Apple Silicon (`arm64`) and Intel
(`x86_64`) apps on GitHub's macOS 15 runners. Each zip contains
`Penguin Mail.app`, including GTK, libadwaita, image loaders, icons, fonts
configuration and translations. WebKit and the other Apple frameworks
come from macOS. Homebrew is needed on the build machine only.

The app's minimum macOS version is taken from the bundled binaries, so a
dependency built for a newer macOS cannot silently raise the requirement
above what Finder reports. GnuPG, Claude Code and other optional external
programs are not bundled. Updates are installed by downloading the next
macOS release and replacing the app; the Linux updater does not run inside
the bundle.

CI builds both architectures without secrets, extracts each archive into
another directory and starts its demo with Homebrew paths blocked. The
release workflow reuses that build with the repository's secrets. A manual
run of **Release** uploads artifacts only; a version tag publishes them
alongside the Linux packages after CI passes.

## Google and Microsoft

The macOS build uses the same repository secrets as Linux:

- `PENGUIN_MAIL_GOOGLE_CLIENT_ID`
- `PENGUIN_MAIL_GOOGLE_CLIENT_SECRET`
- `MICROSOFT_CLIENT_ID`

No new Google client is needed for macOS. These identify the desktop app,
not a person's account or tokens. A fork does not inherit upstream's
secrets. Without these settings the app still builds, but the corresponding
sign-in is unavailable, as described in [setup](setup.md#building-your-own-copy).

## Optional Apple signing

Until all five Apple secrets below exist, the job makes an ad hoc signed
`penguin-mail-VERSION-macos-ARCH-unsigned.zip`. It reports a warning and
continues. This archive is for testing: downloaded copies can be blocked
by Gatekeeper. Missing or partial signing settings do not stop the Linux
release. Once all five are set, invalid credentials, signing failures or a
notarization rejection fail the job instead of publishing an unsigned
replacement.

### Get the Developer ID Application certificate

1. Enroll in the [Apple Developer Program](https://developer.apple.com/programs/enroll/).
   Use the Account Holder account to create the certificate.
2. On a Mac, open **Keychain Access > Certificate Assistant > Request a
   Certificate From a Certificate Authority**. Save the certificate signing
   request to disk. Its private key stays in that Mac's keychain.
3. In [Certificates, Identifiers & Profiles](https://developer.apple.com/account/resources/certificates/list),
   click **+**, then **Developer ID Application**, and upload the request.
   This app uses a zip, so it needs no Developer ID Installer certificate.
4. Download the `.cer` file and open it on the Mac that made the request.
   In Keychain Access, under **My Certificates**, select the Developer ID
   Application identity and export it as a password-protected `.p12`.
   Include its private key; the `.cer` alone cannot sign an app.
5. Encode the exported file with `base64 -i DeveloperID.p12 | pbcopy`.
   Paste the result into the secret below, never into a tracked file.

Apple's [certificate instructions](https://developer.apple.com/help/account/certificates/create-developer-id-certificates/)
and [signing identity export instructions](https://developer.apple.com/documentation/xcode/sharing-your-teams-signing-certificates)
describe these steps.

### Add the GitHub secrets

In the **upstream repository**, open **Settings > Secrets and variables >
Actions > New repository secret** and add:

| Secret | Value |
| --- | --- |
| `MACOS_CERTIFICATE_P12` | The base64 text of the exported `.p12`, including the private key. |
| `MACOS_CERTIFICATE_PASSWORD` | The password chosen when exporting the `.p12`. |
| `APPLE_ID` | The Apple Account email used for notarization. |
| `APPLE_TEAM_ID` | The Team ID shown under Membership details at developer.apple.com/account. |
| `APPLE_APP_SPECIFIC_PASSWORD` | A password generated at account.apple.com, under Sign-In and Security > App-Specific Passwords. This is not the account's login password. |

Use a notarization account belonging to the same developer team as the
certificate. Apple requires two-factor authentication to generate an
[app-specific password](https://support.apple.com/en-us/102654).

The job imports the certificate into a temporary keychain, signs the app
and every bundled library with hardened runtime and a timestamp, submits
the zip with `notarytool`, checks for **Accepted**, then staples and
validates the ticket. It removes the temporary keychain and certificate on
exit. Signed and notarized archives have no `-unsigned` suffix.

Run **Actions > Release > Run workflow** first to check the credentials
without publishing a release. Download the artifact for each architecture
and test Google sign-in before publishing a tag. Apple's service may take
longer on its first submission; a timeout fails the job and can be retried.

## Building locally

```sh
brew install gtk4 libadwaita adwaita-icon-theme gettext librsvg pkgconf
scripts/package-macos.sh dist
python3 scripts/check-macos-bundle.py dist/*-macos-*.zip
```

Use the Rust version from `Cargo.toml`. Local builds can read the OAuth
values from the ignored `packaging/secrets.env`. Apple signing values are
environment variables; leave them unset for an unsigned test archive.
`scripts/install-macos.sh` remains the faster local installation that uses
the computer's Homebrew libraries.
