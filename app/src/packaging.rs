//! The kind of package this binary was built for, chosen at build time with
//! a cargo feature: `packaging-rpm`, `packaging-flatpak` or
//! `packaging-snap`, and none for the .deb, the tarball and a build from
//! source. It decides who installs new versions and whether skill
//! scripts can run.

use mailrs_domain::translate::gettext;

const CHOSEN: usize = cfg!(feature = "packaging-rpm") as usize
    + cfg!(feature = "packaging-flatpak") as usize
    + cfg!(feature = "packaging-snap") as usize;
const _: () = assert!(
    CHOSEN <= 1,
    "a build is for one kind of package; pick one packaging-* feature"
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Packaging {
    /// The .deb, the tarball, or `scripts/install.sh`.
    Native,
    Rpm,
    Flatpak,
    Snap,
}

/// Who installs new versions of a package that does not update itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdatedBy {
    Dnf,
    Flathub,
    SnapStore,
}

pub const BUILT_FOR: Packaging = if cfg!(feature = "packaging-rpm") {
    Packaging::Rpm
} else if cfg!(feature = "packaging-flatpak") {
    Packaging::Flatpak
} else if cfg!(feature = "packaging-snap") {
    Packaging::Snap
} else {
    Packaging::Native
};

/// What sets one package apart, in one table so a new package kind is
/// one row.
struct Traits {
    updated_by: Option<UpdatedBy>,
    /// The sandbox the whole app runs in, by its product name.
    sandbox: Option<&'static str>,
}

impl Packaging {
    const fn traits(self) -> Traits {
        let (updated_by, sandbox) = match self {
            Packaging::Native => (None, None),
            Packaging::Rpm => (Some(UpdatedBy::Dnf), None),
            Packaging::Flatpak => (Some(UpdatedBy::Flathub), Some("Flatpak")),
            Packaging::Snap => (Some(UpdatedBy::SnapStore), Some("Snap")),
        };
        Traits {
            updated_by,
            sandbox,
        }
    }

    /// Who installs new versions, when the app does not.
    pub fn updated_by(self) -> Option<UpdatedBy> {
        self.traits().updated_by
    }

    /// The sandbox Flatpak or a snap puts the whole app in.
    pub fn sandbox_name(self) -> Option<&'static str> {
        self.traits().sandbox
    }

    /// Skill scripts run under bubblewrap, which cannot start inside the
    /// sandbox Flatpak or a strict snap already puts the app in. Running
    /// them without one would hand a skill the person's mail and keys, so
    /// skills stay off there.
    pub fn runs_skills(self) -> bool {
        self.sandbox_name().is_none()
    }
}

impl UpdatedBy {
    /// The line Preferences and the About window show in place of the
    /// update controls.
    pub fn line(self) -> String {
        match self {
            UpdatedBy::Dnf => gettext("Updates come from dnf"),
            UpdatedBy::Flathub => gettext("Updates come from Flathub"),
            UpdatedBy::SnapStore => gettext("Updates come from the Snap Store"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rpm_and_the_store_packages_leave_updates_to_someone_else() {
        assert_eq!(Packaging::Rpm.updated_by(), Some(UpdatedBy::Dnf));
        assert_eq!(Packaging::Flatpak.updated_by(), Some(UpdatedBy::Flathub));
        assert_eq!(Packaging::Snap.updated_by(), Some(UpdatedBy::SnapStore));
        assert_eq!(Packaging::Native.updated_by(), None);
    }

    #[test]
    fn skills_run_only_outside_another_sandbox() {
        assert!(Packaging::Native.runs_skills());
        assert!(Packaging::Rpm.runs_skills());
        assert!(!Packaging::Flatpak.runs_skills());
        assert!(!Packaging::Snap.runs_skills());
    }

    #[test]
    fn a_plain_build_is_native() {
        if !cfg!(any(
            feature = "packaging-rpm",
            feature = "packaging-flatpak",
            feature = "packaging-snap"
        )) {
            assert_eq!(BUILT_FOR, Packaging::Native);
        }
    }

    #[test]
    fn each_updater_names_itself() {
        assert_eq!(UpdatedBy::Dnf.line(), "Updates come from dnf");
        assert_eq!(UpdatedBy::Flathub.line(), "Updates come from Flathub");
        assert_eq!(UpdatedBy::SnapStore.line(), "Updates come from the Snap Store");
    }
}
