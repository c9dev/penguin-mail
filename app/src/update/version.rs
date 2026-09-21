//! Release versions and the file each kind of install fetches.

use std::fmt;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    /// Reads `0.2.0` or a tag such as `v0.2.0`. Anything with a suffix is not
    /// a release this app offers.
    pub fn parse(text: &str) -> Option<Version> {
        let text = text.strip_prefix('v').unwrap_or(text);
        let mut parts = text.split('.').map(|p| p.parse::<u64>().ok());
        let version = Version {
            major: parts.next()??,
            minor: parts.next()??,
            patch: parts.next()??,
        };
        parts.next().is_none().then_some(version)
    }

    /// The version this binary was built as.
    pub fn running() -> Version {
        Version::parse(env!("CARGO_PKG_VERSION")).expect("Cargo.toml holds a plain version")
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// One file attached to a release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
}

/// A published version: its number, its page on GitHub, and its files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    pub page: String,
    pub assets: Vec<Asset>,
}

/// How this copy was installed, which decides what an update fetches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Method {
    /// From the .deb, under `/usr`. apt replaces it, behind a password.
    Deb,
    /// From a tarball or `scripts/install.sh`, into a prefix the person owns.
    Local { prefix: PathBuf },
}

/// The two files an update downloads.
#[derive(Debug)]
pub struct Download<'a> {
    pub package: &'a Asset,
    pub sums: &'a Asset,
}

/// The package this install method needs from a release, and the sums to
/// check it by. A release missing either offers nothing.
pub fn pick<'a>(release: &'a Release, method: &Method) -> Option<Download<'a>> {
    let v = release.version;
    let wanted = match method {
        Method::Deb => format!("penguin-mail_{v}_amd64.deb"),
        Method::Local { .. } => format!("penguin-mail-{v}-x86_64.tar.gz"),
    };
    let find = |name: &str| release.assets.iter().find(|a| a.name == name);
    Some(Download {
        package: find(&wanted)?,
        sums: find("SHA256SUMS")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(version: &str, names: &[&str]) -> Release {
        Release {
            version: Version::parse(version).unwrap(),
            page: String::new(),
            assets: names
                .iter()
                .map(|n| Asset {
                    name: n.to_string(),
                    url: format!("https://x/{n}"),
                })
                .collect(),
        }
    }

    #[test]
    fn a_version_reads_with_or_without_its_v() {
        assert_eq!(Version::parse("v0.1.10"), Version::parse("0.1.10"));
        assert_eq!(Version::parse("0.1.10").unwrap().to_string(), "0.1.10");
        assert_eq!(Version::parse("0.1"), None);
        assert_eq!(Version::parse("v0.1.x"), None);
        assert_eq!(Version::parse("0.1.0-rc1"), None);
        assert_eq!(Version::parse("0.1.0.4"), None);
    }

    #[test]
    fn versions_order_by_number_not_by_text() {
        assert!(Version::parse("0.1.10") > Version::parse("0.1.9"));
        assert!(Version::parse("1.0.0") > Version::parse("0.99.99"));
    }

    #[test]
    fn the_running_version_is_the_one_cargo_built() {
        assert_eq!(Version::running().to_string(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn each_install_method_fetches_its_own_file_and_the_sums() {
        let r = release(
            "0.2.0",
            &[
                "penguin-mail_0.2.0_amd64.deb",
                "penguin-mail-0.2.0-x86_64.tar.gz",
                "penguin-mail-0.2.0-x86_64.zip",
                "SHA256SUMS",
            ],
        );
        let deb = pick(&r, &Method::Deb).unwrap();
        assert_eq!(deb.package.name, "penguin-mail_0.2.0_amd64.deb");
        assert_eq!(deb.sums.name, "SHA256SUMS");
        let local = pick(
            &r,
            &Method::Local {
                prefix: "/home/a/.local".into(),
            },
        )
        .unwrap();
        assert_eq!(local.package.name, "penguin-mail-0.2.0-x86_64.tar.gz");
    }

    #[test]
    fn a_release_missing_a_file_offers_nothing() {
        let r = release("0.2.0", &["penguin-mail_0.2.0_amd64.deb"]);
        assert!(pick(&r, &Method::Deb).is_none());
    }
}
