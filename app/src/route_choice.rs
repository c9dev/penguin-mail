//! Which ways of signing in to Gmail this computer offers, and in what
//! order. Nothing here touches GTK, so the rows, their order and the
//! footer are tested without a display.

use mailrs_domain::translate::gettext;
use mailrs_sync::goa::{self, GoaAccount, GoaError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Linux,
    MacOs,
}

impl Platform {
    /// `MacOs` stands for every system without Online Accounts.
    pub fn this() -> Platform {
        if cfg!(target_os = "linux") {
            Platform::Linux
        } else {
            Platform::MacOs
        }
    }
}

/// What the desktop's Online Accounts offers for the address being signed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Goa {
    /// Not on this computer.
    Absent,
    /// On this computer, without the address.
    Present,
    /// On this computer and already signed in as the address.
    Holds,
}

/// What decides the rows beyond the platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ready {
    pub goa: Goa,
    /// A project from an earlier setup is kept on this computer.
    pub saved_project: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Goa,
    AppPassword,
    OwnProject,
    /// The project kept from an earlier setup.
    SavedProject,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteRow {
    pub route: Route,
    pub title: String,
    pub subtitle: String,
    /// Whether the row carries the "Recommended" tag.
    pub recommended: bool,
}

/// Where Online Accounts stands for `email`, from the list it answered:
/// absent on any error, holding the address when it lists it, else present.
pub fn goa_state(list: &Result<Vec<GoaAccount>, GoaError>, email: Option<&str>) -> Goa {
    match list {
        Err(_) => Goa::Absent,
        Ok(list) => match email {
            Some(email) if goa::find(list, None, email).is_some() => Goa::Holds,
            _ => Goa::Present,
        },
    }
}

/// The rows every route choice starts from, with no project kept. The
/// caller says whether a project is kept.
pub fn ready_now(goa: Goa) -> Ready {
    Ready {
        goa,
        saved_project: false,
    }
}

/// The heading of the route page and of the sheet that leads to it.
pub fn title() -> String {
    // Translators: Gmail and Penguin Mail are names; keep them as they are.
    gettext("Choose how Penguin Mail signs in to Gmail")
}

/// The rows to show, in order: the own project first, which gives every
/// Gmail feature, then one other way, Online Accounts on Linux and an app
/// password elsewhere.
pub fn rows(platform: Platform, ready: Ready) -> Vec<RouteRow> {
    let mut rows = own_rows(ready);
    rows.push(match platform {
        Platform::Linux => goa_row(ready.goa),
        Platform::MacOs => password_row(false),
    });
    rows
}

/// The rows under "Other ways to sign in", which the Online Accounts page
/// leads to: the own project and the app password.
pub fn other_ways(ready: Ready) -> Vec<RouteRow> {
    let mut rows = own_rows(ready);
    rows.push(password_row(true));
    rows
}

/// The line under the list.
pub fn footer() -> String {
    // Translators: Gmail and Google are names; keep them as they are.
    gettext("Want every Gmail feature? Your own Google project gives you all of them, free.")
}

/// The project kept from an earlier setup, when there is one, and the
/// setup of a project.
fn own_rows(ready: Ready) -> Vec<RouteRow> {
    let mut rows = Vec::new();
    if ready.saved_project {
        rows.push(saved_project_row(true));
    }
    rows.push(own_project_row(!ready.saved_project, ready.saved_project));
    rows
}

fn goa_row(goa: Goa) -> RouteRow {
    RouteRow {
        route: Route::Goa,
        title: if goa == Goa::Holds {
            gettext("Use the Google account on this computer")
        } else {
            gettext("Add your Google account to this computer")
        },
        subtitle: if goa == Goa::Holds {
            gettext("Already signed in on this computer")
        } else {
            gettext("Quick. Mail, calendar and contacts.")
        },
        recommended: false,
    }
}

fn password_row(goa_listed: bool) -> RouteRow {
    RouteRow {
        route: Route::AppPassword,
        // Translators: Google is a name; keep it as it is.
        title: gettext("Use a password from Google"),
        subtitle: if goa_listed {
            gettext("Quick. Mail, calendar and contacts.")
        } else {
            gettext("Quickest. About 2 minutes")
        },
        recommended: false,
    }
}

fn saved_project_row(recommended: bool) -> RouteRow {
    RouteRow {
        route: Route::SavedProject,
        // Translators: Google is a name; keep it as it is.
        title: gettext("Use your Google project again"),
        // Translators: Gmail is a name; keep it as it is.
        subtitle: gettext("Every Gmail feature, with the project you set up before."),
        recommended,
    }
}

fn own_project_row(recommended: bool, saved: bool) -> RouteRow {
    RouteRow {
        route: Route::OwnProject,
        title: if saved {
            gettext("Set up a different project")
        } else {
            // Translators: Google is a name; keep it as it is.
            gettext("Set up your own Google project")
        },
        // Translators: Gmail is a name; keep it as it is.
        subtitle: gettext("Every Gmail feature, on any computer. About 10 minutes, once."),
        recommended,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready(goa: Goa) -> Ready {
        ready_now(goa)
    }

    fn routes(rows: &[RouteRow]) -> Vec<Route> {
        rows.iter().map(|r| r.route).collect()
    }

    #[test]
    fn a_saved_project_comes_first_and_the_setup_offers_a_different_one() {
        for (platform, goa) in [
            (Platform::MacOs, Goa::Absent),
            (Platform::Linux, Goa::Holds),
        ] {
            let saved = Ready {
                saved_project: true,
                ..ready(goa)
            };
            let rows = rows(platform, saved);
            assert_eq!(rows[0].route, Route::SavedProject);
            assert_eq!(rows[0].title, "Use your Google project again");
            let own = rows.iter().find(|r| r.route == Route::OwnProject).unwrap();
            assert_eq!(own.title, "Set up a different project");
            assert!(!own.recommended);
        }
    }

    #[test]
    fn linux_lists_the_own_project_then_online_accounts_and_recommends_the_first() {
        for goa in [Goa::Absent, Goa::Present, Goa::Holds] {
            let rows = rows(Platform::Linux, ready(goa));
            assert_eq!(routes(&rows), [Route::OwnProject, Route::Goa]);
            assert_eq!(
                rows[0].subtitle,
                "Every Gmail feature, on any computer. About 10 minutes, once."
            );
            assert_eq!(
                rows.iter().filter(|r| r.recommended).count(),
                1,
                "only the own project carries the tag"
            );
            assert!(rows[0].recommended);
        }
    }

    #[test]
    fn elsewhere_lists_the_own_project_then_the_app_password() {
        let rows = rows(Platform::MacOs, ready(Goa::Absent));
        assert_eq!(routes(&rows), [Route::OwnProject, Route::AppPassword]);
        assert!(rows[0].recommended);
        assert_eq!(rows[1].subtitle, "Quickest. About 2 minutes");
    }

    #[test]
    fn the_online_accounts_row_says_this_computer_and_never_gnome() {
        let holds = rows(Platform::Linux, ready(Goa::Holds));
        assert_eq!(holds[1].title, "Use the Google account on this computer");
        assert_eq!(holds[1].subtitle, "Already signed in on this computer");
        let present = rows(Platform::Linux, ready(Goa::Present));
        assert_eq!(present[1].title, "Add your Google account to this computer");
        assert_eq!(present[1].subtitle, "Quick. Mail, calendar and contacts.");
        for row in holds.iter().chain(&present) {
            assert!(!row.title.contains("GNOME"), "{}", row.title);
            assert!(!row.subtitle.contains("GNOME"), "{}", row.subtitle);
        }
    }

    #[test]
    fn other_ways_leave_out_online_accounts() {
        let rows = other_ways(ready(Goa::Holds));
        assert_eq!(routes(&rows), [Route::OwnProject, Route::AppPassword]);
        assert_eq!(rows[1].subtitle, "Quick. Mail, calendar and contacts.");
    }

    #[test]
    fn an_unreachable_online_accounts_counts_as_absent() {
        let down = Err(GoaError::Unreachable("x".into()));
        assert_eq!(goa_state(&down, Some("a@b.com")), Goa::Absent);
    }

    #[test]
    fn online_accounts_holds_the_address_or_only_exists() {
        let dana = GoaAccount {
            id: "account_1_0".into(),
            identity: "Dana@Example.com".into(),
            presentation: "Dana@Example.com".into(),
            mail_disabled: false,
            attention_needed: false,
        };
        let list = Ok(vec![dana]);
        assert_eq!(goa_state(&list, Some(" dana@example.com ")), Goa::Holds);
        assert_eq!(goa_state(&list, Some("other@example.com")), Goa::Present);
        assert_eq!(goa_state(&list, None), Goa::Present);
    }

    #[test]
    fn no_row_names_a_protocol_or_a_reason() {
        for platform in [Platform::Linux, Platform::MacOs] {
            for row in rows(platform, ready(Goa::Present))
                .iter()
                .chain(&other_ways(ready(Goa::Present)))
            {
                for word in ["IMAP", "OAuth", "scope", "verif", "CASA", "review", "cost"] {
                    assert!(!row.title.contains(word), "{}", row.title);
                    assert!(!row.subtitle.contains(word), "{}", row.subtitle);
                }
            }
        }
    }

    #[test]
    fn the_footer_and_the_title_name_the_own_project_and_the_choice() {
        assert_eq!(
            footer(),
            "Want every Gmail feature? Your own Google project gives you all of them, free."
        );
        assert_eq!(title(), "Choose how Penguin Mail signs in to Gmail");
    }
}
