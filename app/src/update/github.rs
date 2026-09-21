//! The one place that talks to GitHub: the latest release and its files.

use std::path::Path;
use std::time::Duration;

use anyhow::Context;
use serde::Deserialize;

use super::version::{Asset, Release, Version};

/// Tries per address before moving on to the next one.
const TRIES: u32 = 3;

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
pub async fn text(client: &reqwest::Client, asset: &Asset) -> anyhow::Result<String> {
    Ok(String::from_utf8(fetch(client, asset).await?)?)
}

/// Writes a release file to `to`.
pub async fn download(client: &reqwest::Client, asset: &Asset, to: &Path) -> anyhow::Result<()> {
    let bytes = fetch(client, asset).await?;
    tokio::fs::write(to, &bytes)
        .await
        .with_context(|| format!("could not write {}", to.display()))
}

/// The addresses to fetch a file from, best first. GitHub's download links
/// have failed with 504 at an edge while the same file came through the API.
fn routes(asset: &Asset) -> Vec<(&str, bool)> {
    let mut routes = Vec::new();
    if let Some(api) = &asset.api {
        // The API hands the file itself back only when asked for bytes.
        routes.push((api.as_str(), true));
    }
    routes.push((asset.url.as_str(), false));
    routes
}

/// A server error or a dropped connection can pass; anything else, such as
/// a missing file, will say the same thing again.
fn worth_retrying(status: Option<reqwest::StatusCode>) -> bool {
    status.is_none_or(|s| s.is_server_error() || s == reqwest::StatusCode::TOO_MANY_REQUESTS)
}

/// Fetches a release file, trying each address a few times with a growing
/// pause before giving up on it, and returns the last error when every one
/// fails.
async fn fetch(client: &reqwest::Client, asset: &Asset) -> anyhow::Result<Vec<u8>> {
    let mut last = None;
    for (url, as_bytes) in routes(asset) {
        for attempt in 0..TRIES {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_secs(2u64 << attempt)).await;
            }
            let mut request = client.get(url);
            if as_bytes {
                request = request.header("Accept", "application/octet-stream");
            }
            let outcome = async { request.send().await?.error_for_status()?.bytes().await }
                .await
                .map(|b| b.to_vec());
            match outcome {
                Ok(bytes) => return Ok(bytes),
                Err(err) => {
                    let again = worth_retrying(err.status());
                    tracing::info!(url, attempt, error = %err, "could not fetch a release file");
                    last = Some(err);
                    if !again {
                        break;
                    }
                }
            }
        }
    }
    Err(last.map_or_else(
        || anyhow::anyhow!("no address to fetch {} from", asset.name),
        Into::into,
    ))
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
    /// The asset's own API address, which serves the file itself.
    #[serde(default)]
    url: Option<String>,
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
                api: a.url,
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
    fn a_file_comes_through_the_api_first_and_the_download_link_after() {
        let json = r#"{"tag_name":"v0.2.0","html_url":"x","assets":[{"name":"SHA256SUMS",
            "url":"https://api.github.com/repos/c9dev/penguin-mail/releases/assets/7",
            "browser_download_url":"https://github.com/c9dev/penguin-mail/releases/download/v0.2.0/SHA256SUMS"}]}"#;
        let release = parse(json).unwrap().unwrap();
        let routes = super::routes(&release.assets[0]);
        assert_eq!(routes.len(), 2);
        assert!(routes[0].0.starts_with("https://api.github.com/") && routes[0].1);
        assert!(routes[1].0.starts_with("https://github.com/") && !routes[1].1);
    }

    #[test]
    fn only_a_server_error_or_a_dropped_connection_is_tried_again() {
        use reqwest::StatusCode;
        assert!(super::worth_retrying(Some(StatusCode::GATEWAY_TIMEOUT)));
        assert!(super::worth_retrying(Some(StatusCode::TOO_MANY_REQUESTS)));
        assert!(super::worth_retrying(None));
        assert!(!super::worth_retrying(Some(StatusCode::NOT_FOUND)));
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
