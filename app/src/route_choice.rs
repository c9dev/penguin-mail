//! Which ways of signing in to Gmail this computer offers, and in what
//! order. Nothing here touches GTK, so the rows, their order and the
//! footer are tested without a display.

use mailrs_domain::translate::gettext;
use mailrs_sync::Clients;
use mailrs_sync::goa::{self, GoaAccount, GoaError};

/// Whether the own-project setup flow exists in this build.
pub const OWN_PROJECT_SETUP: bool = true;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Linux,
    MacOs,
}

impl Platform {
    pub fn this() -> Platform {
        if cfg!(target_os = "macos") {
            Platform::MacOs
        } else {
            Platform::Linux
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

/// Which routes this build can run now. A route that is not built yet is
/// left out of the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ready {
    pub goa: Goa,
    pub app_password: bool,
    pub own_project: bool,
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

/// What this build can run now. The own-project and password routes are always ready; the caller says whether a project is kept.
pub fn ready_now(clients: &Clients, goa: Goa) -> Ready {
    Ready {
        goa,
        app_password: true,
        own_project: OWN_PROJECT_SETUP || matches!(clients.own_google, Some(Ok(_))),
        saved_project: false,
    }
}

/// The heading of the route page and of the sheet that leads to it.
pub fn title() -> String {
    // Translators: Gmail and Penguin Mail are names; keep them as they are.
    gettext("Choose how Penguin Mail signs in to Gmail")
}

/// The rows to show, in order.
pub fn rows(platform: Platform, ready: Ready) -> Vec<RouteRow> {
    let goa_listed = platform == Platform::Linux && ready.goa != Goa::Absent;
    let saved = ready.own_project && ready.saved_project;
    let mut rows = Vec::new();
    if saved {
        rows.push(saved_project_row(!goa_listed));
    }
    if goa_listed {
        rows.push(goa_row(ready.goa));
        if ready.app_password {
            rows.push(password_row(true));
        }
        if ready.own_project {
            rows.push(own_project_row(false, saved));
        }
    } else {
        if ready.own_project {
            rows.push(own_project_row(!saved, saved));
        }
        if ready.app_password {
            rows.push(password_row(false));
        }
    }
    rows
}

/// The rows under "Other ways to sign in": the same list without the
/// Online Accounts row, which the page before it already offered.
pub fn rows_without_goa(platform: Platform, ready: Ready) -> Vec<RouteRow> {
    rows(platform, ready)
        .into_iter()
        .filter(|row| row.route != Route::Goa)
        .collect()
}

/// Whether any row would show.
pub fn any_route(platform: Platform, ready: Ready) -> bool {
    !rows(platform, ready).is_empty()
}

/// The line under the list, whenever the own-project row is listed.
pub fn footer(platform: Platform, ready: Ready) -> Option<String> {
    let listed = rows(platform, ready)
        .iter()
        .any(|row| row.route == Route::OwnProject);
    listed.then(|| {
        // Translators: Gmail and Google are names; keep them as they are.
        gettext("Want every Gmail feature? Your own Google project gives you all of them, free.")
    })
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
            gettext("Mail, calendar and contacts")
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
        Ready {
            goa,
            app_password: true,
            own_project: true,
            saved_project: false,
        }
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
    fn a_saved_project_without_the_setup_lists_no_row_for_it() {
        let saved = Ready {
            own_project: false,
            saved_project: true,
            ..ready(Goa::Absent)
        };
        assert!(
            rows(Platform::MacOs, saved)
                .iter()
                .all(|r| r.route != Route::SavedProject)
        );
    }

    #[test]
    fn linux_with_goa_lists_three_rows_goa_first() {
        let rows = rows(Platform::Linux, ready(Goa::Holds));
        assert_eq!(
            rows.iter().map(|r| r.route).collect::<Vec<_>>(),
            [Route::Goa, Route::AppPassword, Route::OwnProject]
        );
        assert_eq!(rows[0].subtitle, "Already signed in on this computer");
        assert_eq!(rows[1].subtitle, "Quick. Mail, calendar and contacts.");
        assert!(rows.iter().all(|r| !r.recommended));
    }

    #[test]
    fn the_goa_row_says_this_computer_and_never_gnome() {
        let holds = rows(Platform::Linux, ready(Goa::Holds));
        assert_eq!(holds[0].title, "Use the Google account on this computer");
        let present = rows(Platform::Linux, ready(Goa::Present));
        assert_eq!(present[0].title, "Add your Google account to this computer");
        assert_eq!(present[0].subtitle, "Mail, calendar and contacts");
        for row in holds.iter().chain(&present) {
            assert!(!row.title.contains("GNOME"), "{}", row.title);
            assert!(!row.subtitle.contains("GNOME"), "{}", row.subtitle);
        }
    }

    #[test]
    fn other_ways_leave_out_the_goa_row() {
        let routes: Vec<Route> = rows_without_goa(Platform::Linux, ready(Goa::Holds))
            .iter()
            .map(|r| r.route)
            .collect();
        assert_eq!(routes, [Route::AppPassword, Route::OwnProject]);
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
    fn a_mac_puts_the_own_project_first_and_recommends_it() {
        let rows = rows(Platform::MacOs, ready(Goa::Absent));
        assert_eq!(rows[0].route, Route::OwnProject);
        assert!(rows[0].recommended);
        assert_eq!(
            rows[0].subtitle,
            "Every Gmail feature, on any computer. About 10 minutes, once."
        );
        assert_eq!(rows[1].subtitle, "Quickest. About 2 minutes");
    }

    #[test]
    fn linux_without_goa_lists_the_own_project_first() {
        let routes: Vec<Route> = rows(Platform::Linux, ready(Goa::Absent))
            .iter()
            .map(|r| r.route)
            .collect();
        assert_eq!(routes, [Route::OwnProject, Route::AppPassword]);
    }

    #[test]
    fn routes_not_built_yet_are_left_out() {
        let only_own = Ready {
            goa: Goa::Absent,
            app_password: false,
            own_project: true,
            saved_project: false,
        };
        assert_eq!(rows(Platform::Linux, only_own).len(), 1);
        let none = Ready {
            own_project: false,
            ..only_own
        };
        assert!(rows(Platform::Linux, none).is_empty());
        assert!(!any_route(Platform::Linux, none));
        assert!(any_route(Platform::Linux, only_own));
    }

    #[test]
    fn no_row_names_a_protocol() {
        for platform in [Platform::Linux, Platform::MacOs] {
            for row in rows(platform, ready(Goa::Present)) {
                for word in ["IMAP", "OAuth", "scope", "verif", "CASA"] {
                    assert!(!row.title.contains(word), "{}", row.title);
                    assert!(!row.subtitle.contains(word), "{}", row.subtitle);
                }
            }
        }
    }

    #[test]
    fn the_footer_shows_whenever_the_own_project_is_listed() {
        let wanted = Some(
            "Want every Gmail feature? Your own Google project gives you all of them, free."
                .to_string(),
        );
        assert_eq!(footer(Platform::MacOs, ready(Goa::Absent)), wanted);
        assert_eq!(footer(Platform::Linux, ready(Goa::Absent)), wanted);
        assert_eq!(footer(Platform::Linux, ready(Goa::Present)), wanted);
        let no_own = Ready {
            own_project: false,
            ..ready(Goa::Absent)
        };
        assert_eq!(footer(Platform::Linux, no_own), None);
    }

    #[test]
    fn this_build_offers_the_own_project_setup_without_any_client() {
        let none = mailrs_sync::Clients::default();
        assert!(ready_now(&none, Goa::Absent).own_project);
        assert!(!ready_now(&none, Goa::Absent).saved_project);
        let named = mailrs_sync::Clients {
            own_google: Some(Ok(mailrs_sync::sign_in::own_client::OwnClient::new(
                "id", "secret",
            ))),
            ..Default::default()
        };
        assert!(ready_now(&named, Goa::Absent).own_project);
        assert_eq!(title(), "Choose how Penguin Mail signs in to Gmail");
    }
}
