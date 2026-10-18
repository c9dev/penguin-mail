//! Which ways of signing in to Gmail this computer offers, and in what
//! order. Nothing here touches GTK, so the rows, their order and the
//! footer are tested without a display.

use mailrs_domain::translate::gettext;
use mailrs_sync::Clients;

/// Whether the own-project setup flow exists in this build. The plan that
/// builds it sets this; until then only an environment-named client counts.
pub const OWN_PROJECT_SETUP: bool = false;

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

/// What GNOME Online Accounts offers for the address being signed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Goa {
    /// Not on this computer.
    Absent,
    /// On this computer, without the address.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the GNOME Online Accounts plan reads it")
    )]
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Goa,
    AppPassword,
    OwnProject,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteRow {
    pub route: Route,
    pub title: String,
    pub subtitle: String,
    /// Whether the row carries the "Recommended" tag.
    pub recommended: bool,
}

/// What this build can run now. The GNOME Online Accounts and password
/// plans fill their fields.
pub fn ready_now(clients: &Clients) -> Ready {
    Ready {
        goa: Goa::Absent,
        app_password: false,
        own_project: OWN_PROJECT_SETUP || matches!(clients.own_google, Some(Ok(_))),
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
    let mut rows = Vec::new();
    if goa_listed {
        rows.push(goa_row(ready.goa));
        if ready.app_password {
            rows.push(password_row(true));
        }
        if ready.own_project {
            rows.push(own_project_row(false));
        }
    } else {
        if ready.own_project {
            rows.push(own_project_row(true));
        }
        if ready.app_password {
            rows.push(password_row(false));
        }
    }
    rows
}

/// Whether any row would show.
pub fn any_route(platform: Platform, ready: Ready) -> bool {
    !rows(platform, ready).is_empty()
}

/// The line under the list, only when the own-project row comes first.
pub fn footer(platform: Platform, ready: Ready) -> Option<String> {
    let first = rows(platform, ready).into_iter().next()?;
    (first.route == Route::OwnProject).then(|| {
        // Translators: Gmail and Google are names; keep them as they are.
        gettext("Want every Gmail feature? Your own Google project gives you all of them, free.")
    })
}

fn goa_row(goa: Goa) -> RouteRow {
    RouteRow {
        route: Route::Goa,
        // Translators: GNOME Online Accounts is a name; keep it as it is.
        title: gettext("Use GNOME Online Accounts"),
        subtitle: if goa == Goa::Holds {
            gettext("Already signed in on this computer")
        } else {
            gettext("Recommended. Every feature, signs in through your desktop.")
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

fn own_project_row(recommended: bool) -> RouteRow {
    RouteRow {
        route: Route::OwnProject,
        // Translators: Google is a name; keep it as it is.
        title: gettext("Set up your own Google project"),
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
        }
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
    fn goa_present_but_not_holding_the_address_recommends_itself_in_words() {
        let rows = rows(Platform::Linux, ready(Goa::Present));
        assert_eq!(
            rows[0].subtitle,
            "Recommended. Every feature, signs in through your desktop."
        );
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
    fn the_footer_shows_only_when_the_own_project_comes_first() {
        let wanted = Some(
            "Want every Gmail feature? Your own Google project gives you all of them, free."
                .to_string(),
        );
        assert_eq!(footer(Platform::MacOs, ready(Goa::Absent)), wanted);
        assert_eq!(footer(Platform::Linux, ready(Goa::Absent)), wanted);
        assert_eq!(footer(Platform::Linux, ready(Goa::Present)), None);
        let no_own = Ready {
            own_project: false,
            ..ready(Goa::Absent)
        };
        assert_eq!(footer(Platform::Linux, no_own), None);
    }

    #[test]
    fn this_build_offers_the_own_project_only_when_the_environment_names_a_client() {
        let none = mailrs_sync::Clients::default();
        assert!(!ready_now(&none).own_project);
        let named = mailrs_sync::Clients {
            own_google: Some(Ok(mailrs_sync::sign_in::own_client::OwnClient::new(
                "id", "secret",
            ))),
            ..Default::default()
        };
        assert!(ready_now(&named).own_project);
        assert_eq!(title(), "Choose how Penguin Mail signs in to Gmail");
    }
}
