//! The Add Account band: the tuxedo envelope at the top of the dialog and
//! of the first-run window. Which pose each step of a flow shows, the
//! stamp a provider gets on the envelope, the provider tiles, and the
//! advice the built-in list gives for an address before anything leaves
//! the computer. `ui::post_band` draws what these say.

use mailrs_discover::{PasswordKind, ProviderInfo, Unreachable, Verdict};
use mailrs_domain::translate::{fill, gettext};

use super::Link;

/// One of the band's poses. Each is one SVG in the resource bundle, and
/// a change of pose is a crossfade between two of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Band {
    /// Beside the wordmark: the first page and the first run.
    Idle,
    /// A provider is chosen or matched, and its stamp is on the corner.
    Stamped,
    /// The flap lifts and the penguin watches a browser window.
    Browser,
    /// The penguin watches the mail servers while the lookup asks.
    Lookup,
    /// The flap closes and a badge sits on the corner.
    Error,
    /// As `Error`, with the line to the server broken halfway.
    Unreachable,
    /// The flap opens on a checked letter.
    Success,
}

impl Band {
    pub const ALL: [Band; 7] = [
        Band::Idle,
        Band::Stamped,
        Band::Browser,
        Band::Lookup,
        Band::Error,
        Band::Unreachable,
        Band::Success,
    ];

    /// The pose's name, as its SVG and the stack child are called.
    pub fn name(self) -> &'static str {
        match self {
            Band::Idle => "idle",
            Band::Stamped => "stamped",
            Band::Browser => "browser",
            Band::Lookup => "lookup",
            Band::Error => "error",
            Band::Unreachable => "unreachable",
            Band::Success => "success",
        }
    }

    /// Whether the warning badge sits on the envelope in this pose.
    pub fn warns(self) -> bool {
        matches!(self, Band::Error | Band::Unreachable)
    }

    /// What a screen reader hears for the band, which is otherwise a
    /// picture. `provider` is the stamp's provider, when there is one.
    pub fn described(self, provider: Option<&str>) -> String {
        let state = match self {
            // A brand, so it is not translated.
            Band::Idle => "Penguin Mail".to_string(),
            Band::Stamped => gettext("Ready to sign in"),
            Band::Browser => gettext("Waiting for your browser"),
            Band::Lookup => gettext("Looking for your mail servers"),
            Band::Error => gettext("Sign-in stopped"),
            Band::Unreachable => gettext("The server does not answer"),
            Band::Success => gettext("Your account is added"),
        };
        match provider {
            Some(provider) if self != Band::Idle => fill(
                &gettext("{state}, {provider}"),
                &[("state", &state), ("provider", provider)],
            ),
            _ => state,
        }
    }
}

/// Where a flow stands, as far as the band cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// The provider tiles.
    Pick,
    /// The address page, before Continue.
    Address,
    /// The lookup running.
    Lookup,
    /// Waiting for the browser to come back from a provider's sign-in.
    Browser,
    /// The password page, with the servers found.
    Found,
    /// The servers turned the password down, or said something else.
    Refused,
    /// The server did not answer.
    Unreachable,
    /// The provider lets no other mail app in.
    Closed,
    /// The account is added and its first sync runs.
    Added,
    /// Server Settings, which has no band.
    Servers,
}

/// The pose `step` shows, or `None` for a page without the band.
pub fn band_for(step: Step) -> Option<Band> {
    Some(match step {
        Step::Pick => Band::Idle,
        Step::Address | Step::Found => Band::Stamped,
        Step::Lookup => Band::Lookup,
        Step::Browser => Band::Browser,
        Step::Refused | Step::Closed => Band::Error,
        Step::Unreachable => Band::Unreachable,
        Step::Added => Band::Success,
        Step::Servers => return None,
    })
}

/// A provider's mark: its logo where the owner's terms let an app show
/// it, and its initial on a tile of its colour where they do not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    pub letter: char,
    /// The tile's colour, as `#rrggbb`.
    pub colour: &'static str,
    /// The file stem under `logos/` in the resources, when the tile shows
    /// artwork instead of the letter.
    pub logo: Option<&'static str>,
}

/// The stamp for an address the list does not name.
pub const ANY_SERVER: Stamp = Stamp {
    letter: '@',
    colour: "#504945",
    logo: None,
};

/// The stamp for `provider`, a name from the built-in list. A provider
/// without a tile of its own takes its initial on the neutral colour.
/// Yahoo and Fastmail keep the initial: Yahoo approves each use of its
/// logo in advance, and Fastmail's guidelines forbid redistributing it.
/// The iCloud cloud is Penguin Mail's own drawing, not Apple's logo.
pub fn stamp_for(provider: &str) -> Stamp {
    let colour = match provider {
        "Google" => "#076678",
        "Microsoft" => "#427b58",
        "iCloud Mail" => "#8f3f71",
        "Fastmail" => "#af3a03",
        "Yahoo Mail" => "#79740e",
        "Tuta" => "#9d0006",
        _ => ANY_SERVER.colour,
    };
    let initial = provider.chars().find(|c| c.is_alphanumeric());
    let letter = match (provider, initial) {
        // Apple writes it in lower case, and so does the tile.
        ("iCloud Mail", _) => 'i',
        (_, Some(initial)) => initial.to_uppercase().next().unwrap_or(initial),
        (_, None) => return ANY_SERVER,
    };
    let logo = match provider {
        "Google" => Some("google"),
        "Microsoft" => Some("microsoft"),
        "iCloud Mail" => Some("icloud"),
        _ => None,
    };
    Stamp {
        letter,
        colour,
        logo,
    }
}

/// One tile on the first page. Microsoft joins them once Penguin Mail
/// can sign in to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tile {
    Google,
    Icloud,
    Fastmail,
    Yahoo,
    Other,
}

impl Tile {
    pub const ALL: [Tile; 5] = [
        Tile::Google,
        Tile::Icloud,
        Tile::Fastmail,
        Tile::Yahoo,
        Tile::Other,
    ];

    pub fn title(self) -> String {
        match self {
            // Brands, so they are not translated.
            Tile::Google => "Google".to_string(),
            Tile::Icloud => "iCloud".to_string(),
            Tile::Fastmail => "Fastmail".to_string(),
            Tile::Yahoo => "Yahoo".to_string(),
            Tile::Other => gettext("Other"),
        }
    }

    pub fn subtitle(self) -> String {
        match self {
            Tile::Google => gettext("Gmail, Workspace"),
            Tile::Icloud => "iCloud Mail".to_string(),
            Tile::Fastmail => "Fastmail".to_string(),
            Tile::Yahoo => "Yahoo Mail".to_string(),
            Tile::Other => gettext("Any server"),
        }
    }

    /// The built-in list's name for the provider behind the tile.
    pub fn provider(self) -> Option<&'static str> {
        match self {
            Tile::Google => Some("Google"),
            Tile::Icloud => Some("iCloud Mail"),
            Tile::Fastmail => Some("Fastmail"),
            Tile::Yahoo => Some("Yahoo Mail"),
            Tile::Other => None,
        }
    }

    pub fn stamp(self) -> Stamp {
        self.provider().map_or(ANY_SERVER, stamp_for)
    }

    /// Whether this tile signs in through the browser rather than with a
    /// password.
    pub fn in_browser(self) -> bool {
        self == Tile::Google
    }

    /// What a screen reader hears for the tile: its name and how it signs
    /// in.
    pub fn described(self) -> String {
        let named = fill(
            &gettext("{title}, {subtitle}"),
            &[("title", &self.title()), ("subtitle", &self.subtitle())],
        );
        if self.in_browser() {
            fill(
                &gettext("{tile}, signs in through your browser"),
                &[("tile", &named)],
            )
        } else {
            named
        }
    }
}

/// How many tiles go on each row, three to a row, the last row holding
/// what is left. The window centres each row, so five tiles sit three over
/// two with nothing missing.
pub fn tile_rows(tiles: usize) -> Vec<usize> {
    (0..tiles)
        .step_by(3)
        .map(|start| (tiles - start).min(3))
        .collect()
}

/// The name a person knows a provider by: the tile's title for a provider
/// with a tile ("iCloud" for "iCloud Mail"), the list's name otherwise.
pub fn short_name(provider: &str) -> String {
    Tile::ALL
        .into_iter()
        .find(|tile| tile.provider() == Some(provider))
        .map_or_else(|| provider.to_string(), Tile::title)
}

/// Where Apple makes app passwords: the Apple Account page.
const APPLE_ACCOUNT: &str = "https://account.apple.com";

/// What kind of advice the list has for an address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdviceKind {
    /// The provider wants an app password.
    AppPassword,
    /// The provider signs in through the browser.
    Browser,
    /// No other mail app can reach the provider.
    Closed,
}

/// The card the address page shows before Continue, from the built-in
/// list alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Advice {
    pub kind: AdviceKind,
    /// The provider, as a person knows it.
    pub provider: String,
    pub stamp: Stamp,
    pub title: String,
    pub body: String,
    pub link: Option<Link>,
}

/// The line under every piece of advice: what it came from.
pub fn advice_source() -> String {
    gettext("Penguin Mail's own list knows this address. Nothing has left this computer.")
}

/// The list's advice for an address at `domain`, or `None` when the list
/// does not know the domain or has nothing to add. Nothing leaves the
/// computer for this.
pub fn advice(domain: &str) -> Option<Advice> {
    let found = mailrs_discover::table_only(domain);
    match found.verdict {
        Verdict::Servers => {
            let info = found.candidates.into_iter().next()?.provider?;
            password_advice(&info)
        }
        Verdict::Google => Some(browser_advice("Google")),
        Verdict::Microsoft => Some(closed(
            "Microsoft",
            gettext("Microsoft is not here yet"),
            gettext("Microsoft accounts come in a later version of Penguin Mail."),
        )),
        Verdict::Unreachable { provider, reason } => {
            let named = [("provider", provider.as_str())];
            let (title, body) = match reason {
                Unreachable::NoImap => (
                    gettext("{provider} has no IMAP"),
                    gettext(
                        "{provider} keeps its mail in its own apps, so other mail apps cannot reach it. Use another address instead.",
                    ),
                ),
                Unreachable::NotYet => (
                    gettext("Penguin Mail cannot reach {provider} yet"),
                    gettext(
                        "{provider} lets other mail apps in only through an app of its own, which Penguin Mail does not support yet. Use another address instead.",
                    ),
                ),
            };
            Some(closed(&provider, fill(&title, &named), fill(&body, &named)))
        }
        Verdict::NothingFound => None,
    }
}

/// The advice for a tile picked before any address is typed.
pub fn tile_advice(tile: Tile) -> Option<Advice> {
    let provider = tile.provider()?;
    if tile.in_browser() {
        return Some(browser_advice(provider));
    }
    password_advice(&mailrs_discover::provider_named(provider)?)
}

/// What a provider that wants an app password asks for, or `None` for one
/// that takes the account's own password.
fn password_advice(info: &ProviderInfo) -> Option<Advice> {
    let provider = short_name(&info.name);
    let named = [("provider", provider.as_str())];
    let title = match info.password {
        PasswordKind::AppPassword => gettext("{provider} needs an app password"),
        PasswordKind::AppPasswordWithTwoStep => {
            gettext("With two-step sign-in on, {provider} needs an app password")
        }
        PasswordKind::AccountPassword => return None,
    };
    // Apple makes app passwords on the Apple Account page, and a person
    // knows the password it stands in for as their Apple password.
    let (body, link) = if info.name == "iCloud Mail" {
        (
            gettext(
                "On the next page, paste an app-specific password from your Apple Account page, not your Apple password.",
            ),
            Some(Link {
                label: gettext("Open Apple Account Page"),
                url: APPLE_ACCOUNT.to_string(),
            }),
        )
    } else {
        (
            fill(
                &gettext(
                    "On the next page, paste an app password from {provider}'s settings, not your {provider} password.",
                ),
                &named,
            ),
            info.app_password_url.clone().map(|url| Link {
                label: fill(&gettext("Open {provider}'s Settings"), &named),
                url,
            }),
        )
    };
    Some(Advice {
        kind: AdviceKind::AppPassword,
        stamp: stamp_for(&info.name),
        title: fill(&title, &named),
        body,
        link,
        provider,
    })
}

fn browser_advice(provider: &str) -> Advice {
    let named = [("provider", provider)];
    Advice {
        kind: AdviceKind::Browser,
        provider: provider.to_string(),
        stamp: stamp_for(provider),
        title: fill(&gettext("{provider} signs in through your browser"), &named),
        body: fill(
            &gettext("Continue opens {provider}'s sign-in page in your browser."),
            &named,
        ),
        link: None,
    }
}

fn closed(provider: &str, title: String, body: String) -> Advice {
    Advice {
        kind: AdviceKind::Closed,
        provider: provider.to_string(),
        stamp: stamp_for(provider),
        title,
        body,
        link: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_step_shows_the_pose_the_mockup_gives_it() {
        assert_eq!(band_for(Step::Pick), Some(Band::Idle));
        assert_eq!(band_for(Step::Address), Some(Band::Stamped));
        assert_eq!(band_for(Step::Lookup), Some(Band::Lookup));
        assert_eq!(band_for(Step::Browser), Some(Band::Browser));
        assert_eq!(band_for(Step::Found), Some(Band::Stamped));
        assert_eq!(band_for(Step::Refused), Some(Band::Error));
        assert_eq!(band_for(Step::Unreachable), Some(Band::Unreachable));
        assert_eq!(band_for(Step::Closed), Some(Band::Error));
        assert_eq!(band_for(Step::Added), Some(Band::Success));
    }

    #[test]
    fn server_settings_has_no_band() {
        assert_eq!(band_for(Step::Servers), None);
    }

    #[test]
    fn only_the_two_error_poses_carry_the_badge() {
        let warning: Vec<Band> = Band::ALL.into_iter().filter(|b| b.warns()).collect();
        assert_eq!(warning, [Band::Error, Band::Unreachable]);
    }

    #[test]
    fn each_pose_is_named_after_its_svg() {
        let names: Vec<&str> = Band::ALL.into_iter().map(Band::name).collect();
        assert_eq!(
            names,
            ["idle", "stamped", "browser", "lookup", "error", "unreachable", "success"]
        );
    }

    #[test]
    fn a_screen_reader_hears_what_the_band_shows() {
        assert_eq!(Band::Idle.described(None), "Penguin Mail");
        assert_eq!(
            Band::Lookup.described(None),
            "Looking for your mail servers"
        );
        assert_eq!(
            Band::Browser.described(Some("Google")),
            "Waiting for your browser, Google"
        );
    }

    #[test]
    fn the_picker_offers_five_tiles_and_no_microsoft() {
        let titles: Vec<String> = Tile::ALL.into_iter().map(Tile::title).collect();
        assert_eq!(titles, ["Google", "iCloud", "Fastmail", "Yahoo", "Other"]);
    }

    #[test]
    fn only_google_signs_in_through_the_browser() {
        let browser: Vec<Tile> = Tile::ALL.into_iter().filter(|t| t.in_browser()).collect();
        assert_eq!(browser, [Tile::Google]);
        assert_eq!(
            Tile::Google.described(),
            "Google, Gmail, Workspace, signs in through your browser"
        );
        assert_eq!(Tile::Fastmail.described(), "Fastmail, Fastmail");
    }

    #[test]
    fn each_tile_names_a_provider_the_list_knows() {
        for tile in Tile::ALL {
            if let Some(name) = tile.provider() {
                assert!(
                    mailrs_discover::provider_named(name).is_some() || name == "Google",
                    "{name}"
                );
            }
        }
        assert_eq!(Tile::Other.provider(), None);
    }

    #[test]
    fn a_tile_and_its_provider_share_one_stamp() {
        assert_eq!(Tile::Icloud.stamp(), stamp_for("iCloud Mail"));
        assert_eq!(Tile::Icloud.stamp().letter, 'i');
        assert_eq!(Tile::Fastmail.stamp().colour, "#af3a03");
        assert_eq!(Tile::Other.stamp(), ANY_SERVER);
    }

    #[test]
    fn a_provider_without_a_tile_gets_its_initial_on_the_neutral_colour() {
        assert_eq!(
            stamp_for("GMX"),
            Stamp {
                letter: 'G',
                colour: ANY_SERVER.colour,
                logo: None,
            }
        );
        assert_eq!(stamp_for("Tuta").colour, "#9d0006");
        assert_eq!(stamp_for("mailbox.org").letter, 'M');
        assert_eq!(stamp_for(""), ANY_SERVER);
    }

    #[test]
    fn only_providers_whose_terms_allow_it_get_a_logo() {
        let logos: Vec<(&str, Option<&str>)> = [
            "Google",
            "Microsoft",
            "iCloud Mail",
            "Fastmail",
            "Yahoo Mail",
            "GMX",
        ]
        .into_iter()
        .map(|name| (name, stamp_for(name).logo))
        .collect();
        assert_eq!(
            logos,
            [
                ("Google", Some("google")),
                ("Microsoft", Some("microsoft")),
                ("iCloud Mail", Some("icloud")),
                ("Fastmail", None),
                ("Yahoo Mail", None),
                ("GMX", None),
            ]
        );
        assert_eq!(ANY_SERVER.logo, None);
    }

    #[test]
    fn every_logo_a_stamp_names_ships_in_the_resources() {
        let manifest = include_str!("../../data/penguin-mail.gresource.xml");
        for name in ["Google", "Microsoft", "iCloud Mail"] {
            let logo = stamp_for(name).logo.expect(name);
            let listed = manifest
                .lines()
                .any(|line| line.contains(&format!("logos/{logo}.")));
            assert!(listed, "{logo} is not in the gresource manifest");
        }
    }

    #[test]
    fn a_provider_with_a_tile_goes_by_the_tiles_name() {
        assert_eq!(short_name("iCloud Mail"), "iCloud");
        assert_eq!(short_name("Yahoo Mail"), "Yahoo");
        assert_eq!(short_name("GMX"), "GMX");
    }

    #[test]
    fn icloud_advice_asks_for_an_app_password_before_continue() {
        let said = advice("icloud.com").expect("the list knows iCloud");
        assert_eq!(said.kind, AdviceKind::AppPassword);
        assert_eq!(said.title, "iCloud needs an app password");
        assert_eq!(
            said.body,
            "On the next page, paste an app-specific password from your Apple Account page, not your Apple password."
        );
        assert_eq!(said.stamp.letter, 'i');
        assert_eq!(
            said.link,
            Some(Link {
                label: "Open Apple Account Page".to_string(),
                url: "https://account.apple.com".to_string(),
            })
        );
    }

    #[test]
    fn fastmail_advice_keeps_the_general_words() {
        let said = advice("fastmail.com").expect("the list knows Fastmail");
        assert_eq!(
            said.link.map(|link| link.label),
            Some("Open Fastmail's Settings".to_string())
        );
    }

    #[test]
    fn five_tiles_sit_three_over_two() {
        assert_eq!(tile_rows(5), [3, 2]);
    }

    #[test]
    fn six_tiles_sit_three_over_three() {
        assert_eq!(tile_rows(6), [3, 3]);
    }

    #[test]
    fn a_short_list_fills_one_row() {
        assert_eq!(tile_rows(2), [2]);
        assert_eq!(tile_rows(0), Vec::<usize>::new());
    }

    #[test]
    fn two_step_providers_say_when_they_want_an_app_password() {
        let said = advice("gmx.de").expect("the list knows GMX");
        assert_eq!(
            said.title,
            "With two-step sign-in on, GMX needs an app password"
        );
    }

    #[test]
    fn a_gmail_address_signs_in_through_the_browser() {
        let said = advice("gmail.com").expect("the list knows Gmail");
        assert_eq!(said.kind, AdviceKind::Browser);
        assert_eq!(said.title, "Google signs in through your browser");
        assert_eq!(said.stamp, Tile::Google.stamp());
    }

    #[test]
    fn tuta_says_it_has_no_imap_before_anything_is_asked() {
        let said = advice("tuta.com").expect("the list knows Tuta");
        assert_eq!(said.kind, AdviceKind::Closed);
        assert_eq!(said.title, "Tuta has no IMAP");
        assert_eq!(
            said.body,
            "Tuta keeps its mail in its own apps, so other mail apps cannot reach it. Use another address instead."
        );
        assert_eq!(said.stamp.letter, 'T');
    }

    #[test]
    fn a_domain_the_list_does_not_know_gets_no_advice() {
        assert_eq!(advice("reyes.studio"), None);
    }

    #[test]
    fn every_password_provider_in_the_list_asks_for_an_app_password() {
        assert_eq!(advice("posteo.de").map(|a| a.kind), Some(AdviceKind::AppPassword));
        assert_eq!(advice("web.de").map(|a| a.kind), Some(AdviceKind::AppPassword));
    }

    #[test]
    fn a_tile_picked_before_an_address_already_has_its_advice() {
        assert_eq!(
            tile_advice(Tile::Fastmail).map(|a| a.title),
            Some("Fastmail needs an app password".to_string())
        );
        assert_eq!(tile_advice(Tile::Other), None);
    }
}
