//! The kind of package this binary was built for, chosen at build time with
//! a cargo feature: `packaging-flatpak`, `packaging-snap` or
//! `packaging-appimage`, and none for the .deb, the rpm, the tarball and a
//! build from source. It decides where updates come from and whether skill
//! scripts can run.

use mailrs_domain::translate::gettext;

#[cfg(any(
    all(feature = "packaging-flatpak", feature = "packaging-snap"),
    all(feature = "packaging-flatpak", feature = "packaging-appimage"),
    all(feature = "packaging-snap", feature = "packaging-appimage"),
))]
compile_error!("a build is for one kind of package; pick one packaging-* feature");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Packaging {
    /// The .deb, the rpm, the tarball, or `scripts/install.sh`.
    Native,
    Flatpak,
    Snap,
    AppImage,
}

/// A software store that installs new versions itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Store {
    Flathub,
    Snap,
}

pub const BUILT_FOR: Packaging = if cfg!(feature = "packaging-flatpak") {
    Packaging::Flatpak
} else if cfg!(feature = "packaging-snap") {
    Packaging::Snap
} else if cfg!(feature = "packaging-appimage") {
    Packaging::AppImage
} else {
    Packaging::Native
};

impl Packaging {
    /// The store that updates this copy, when one does.
    pub fn store(self) -> Option<Store> {
        match self {
            Packaging::Flatpak => Some(Store::Flathub),
            Packaging::Snap => Some(Store::Snap),
            Packaging::Native | Packaging::AppImage => None,
        }
    }

    /// Skill scripts run under bubblewrap, which cannot start inside the
    /// sandbox Flatpak or a strict snap already puts the app in. Running
    /// them without one would hand a skill the person's mail and keys, so
    /// skills stay off there.
    pub fn runs_skills(self) -> bool {
        self.sandbox_name().is_none()
    }

    /// The sandbox the whole app runs in, by its product name.
    pub fn sandbox_name(self) -> Option<&'static str> {
        match self {
            Packaging::Flatpak => Some("Flatpak"),
            Packaging::Snap => Some("Snap"),
            Packaging::Native | Packaging::AppImage => None,
        }
    }
}

impl Store {
    /// The line Preferences and the About window show in place of the
    /// update controls.
    pub fn updates_line(self) -> String {
        match self {
            Store::Flathub => gettext("Updates come from Flathub"),
            Store::Snap => gettext("Updates come from the Snap Store"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_store_packages_leave_updates_to_a_store() {
        assert_eq!(Packaging::Flatpak.store(), Some(Store::Flathub));
        assert_eq!(Packaging::Snap.store(), Some(Store::Snap));
        assert_eq!(Packaging::AppImage.store(), None);
        assert_eq!(Packaging::Native.store(), None);
    }

    #[test]
    fn skills_run_only_outside_another_sandbox() {
        assert!(Packaging::Native.runs_skills());
        assert!(Packaging::AppImage.runs_skills());
        assert!(!Packaging::Flatpak.runs_skills());
        assert!(!Packaging::Snap.runs_skills());
    }

    #[test]
    fn a_plain_build_is_native() {
        if !cfg!(any(
            feature = "packaging-flatpak",
            feature = "packaging-snap",
            feature = "packaging-appimage"
        )) {
            assert_eq!(BUILT_FOR, Packaging::Native);
        }
    }

    #[test]
    fn each_store_names_itself() {
        assert_eq!(Store::Flathub.updates_line(), "Updates come from Flathub");
        assert_eq!(Store::Snap.updates_line(), "Updates come from the Snap Store");
    }
}
