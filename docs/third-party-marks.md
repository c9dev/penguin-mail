# Third-party marks

The Add Account tiles show these marks. They are their owners' trademarks and
Penguin Mail uses them only to identify the service. Files are in
`app/data/logos/` and ship in the app's resources.

| Mark | Owner | Source | File |
|---|---|---|---|
| Google "G" | Google LLC | Sign in with Google assets, <https://developers.google.com/static/identity/images/signin-assets.zip> (branding guidelines: <https://developers.google.com/identity/branding-guidelines>). The G is cut, unchanged, from the light square no-text button at 4x, and sits on a white tile. | `google.png` |
| Microsoft four squares | Microsoft Corporation | <https://learn.microsoft.com/en-us/entra/identity-platform/media/howto-add-branding-in-apps/ms-symbollockup_mssymbol_19.svg>, from Microsoft's Sign in with Microsoft branding guidelines, unaltered. Not shown yet: the Microsoft tile arrives with Microsoft sign-in. | `microsoft.svg` |
| Cloud for iCloud Mail | Penguin Mail | Drawn for this app. It is a plain cloud, not Apple's logo, which Apple's trademark guidelines do not let third parties use. | `icloud.svg` |

## Marks not used

- **Yahoo.** Yahoo's brand guidelines
  (<https://legal.yahoo.com/us/en/yahoo/permissions/branduseguidelines/index.html>)
  require Yahoo Brand Marketing to approve each use in advance and publish no
  logo download. The tile keeps its initial until Yahoo approves.
- **Fastmail.** The brand guidelines (<https://www.fastmail.com/policies/brand-guidelines/>)
  grant a licence only for approved purposes and say not to distribute the
  logos. Bundling them in an open-source app would break that, so the tile
  keeps its initial. Ask press@fastmail.com to change this.

## Rules kept

- Google's G stays on a white tile in light and dark mode and is never recolored.
- No mark is redrawn or recolored, and Google's is only cut out of its button. Each is scaled inside its 38 px
  tile at its own aspect ratio.
- Marks are decorative in the app: the tile's button carries the accessible name.
