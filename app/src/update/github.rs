//! The one place that talks to GitHub: the latest release and its files.

use std::path::Path;
use std::time::Duration;

use anyhow::Context;
use serde::Deserialize;

use super::version::{Asset, Release, Version};

const LATEST: &str = "https://api.github.com/repos/c9dev/penguin-mail/releases/latest";

/// GitHub refuses API calls without a User-Agent.
pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(format!("penguin-mail/{}", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(120))
        .build()
        .expect("a reqwest client builds with a user agent and a timeout")
}

/// The newest published release, or `None` while the repository has none.
pub async fn latest(client: &reqwest::Client) -> anyhow::Result<Option<Release>> {
    // A test release server can stand in for GitHub, as MAILRS_SHED_AFTER
    // stands in for the minute before the memory restart.
    let url = std::env::var("PENGUIN_MAIL_RELEASES_URL").unwrap_or_else(|_| LATEST.into());
    let response = client
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let body = response.error_for_status()?.text().await?;
    parse(&body)
}

/// A small file, such as `SHA256SUMS`, as text.
pub async fn text(client: &reqwest::Client, url: &str) -> anyhow::Result<String> {
    Ok(client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?)
}

/// Writes a release file to `to`.
pub async fn download(client: &reqwest::Client, url: &str, to: &Path) -> anyhow::Result<()> {
    let bytes = client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    tokio::fs::write(to, &bytes)
        .await
        .with_context(|| format!("could not write {}", to.display()))
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
}

/// Drafts, prereleases, and tags that are not a plain version are not
/// releases this app offers.
fn parse(json: &str) -> anyhow::Result<Option<Release>> {
    let api: ApiRelease = serde_json::from_str(json)?;
    if api.draft || api.prerelease {
        return Ok(None);
    }
    let Some(version) = Version::parse(&api.tag_name) else {
        return Ok(None);
    };
    Ok(Some(Release {
        version,
        page: api.html_url,
        assets: api
            .assets
            .into_iter()
            .map(|a| Asset {
                name: a.name,
                url: a.browser_download_url,
            })
            .collect(),
    }))
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn a_release_reads_from_the_api_answer() {
        let json = r#"{"tag_name":"v0.2.0",
            "html_url":"https://github.com/c9dev/penguin-mail/releases/tag/v0.2.0",
            "draft":false,"prerelease":false,"body":"- Things",
            "assets":[{"name":"SHA256SUMS","size":290,
              "browser_download_url":"https://github.com/c9dev/penguin-mail/releases/download/v0.2.0/SHA256SUMS"}]}"#;
        let release = parse(json).unwrap().unwrap();
        assert_eq!(release.version.to_string(), "0.2.0");
        assert_eq!(release.assets[0].name, "SHA256SUMS");
        assert!(release.assets[0].url.ends_with("/v0.2.0/SHA256SUMS"));
        assert!(release.page.ends_with("/v0.2.0"));
    }

    #[test]
    fn a_tag_that_is_not_a_plain_version_is_ignored() {
        let json = r#"{"tag_name":"nightly","html_url":"x","assets":[]}"#;
        assert!(parse(json).unwrap().is_none());
    }

    #[test]
    fn a_prerelease_is_ignored() {
        let json = r#"{"tag_name":"v0.3.0","html_url":"x","prerelease":true,"assets":[]}"#;
        assert!(parse(json).unwrap().is_none());
    }
}
