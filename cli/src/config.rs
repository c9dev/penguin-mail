//! `config.toml` and where mailrs keeps its files.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use mailrs_sync::EngineConfig;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub oauth: OAuthConfig,
    #[serde(default)]
    pub sync: SyncConfig,
}

#[derive(Debug, Deserialize)]
pub struct OAuthConfig {
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct SyncConfig {
    pub poll_seconds: Option<u64>,
    pub body_cache_mb: Option<i64>,
    pub window_days: Option<i64>,
}

impl Config {
    pub fn parse(text: &str) -> Result<Config> {
        toml::from_str(text).context("config.toml is not valid")
    }

    pub fn load(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path).with_context(|| {
            format!(
                "could not read {}; docs/setup.md explains how to create it",
                path.display()
            )
        })?;
        Self::parse(&text)
    }

    pub fn engine_config(&self) -> EngineConfig {
        let defaults = EngineConfig::default();
        EngineConfig {
            poll_interval: self
                .sync
                .poll_seconds
                .map(Duration::from_secs)
                .unwrap_or(defaults.poll_interval),
            body_cache_bytes: self
                .sync
                .body_cache_mb
                .map(|mb| mb * 1024 * 1024)
                .unwrap_or(defaults.body_cache_bytes),
            window_days: self.sync.window_days.unwrap_or(defaults.window_days),
            ..defaults
        }
    }
}

/// `$MAILRS_CONFIG`, else `~/.config/mailrs/config.toml`.
pub fn config_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("MAILRS_CONFIG") {
        return Ok(PathBuf::from(path));
    }
    Ok(dirs::config_dir()
        .context("no config directory; set MAILRS_CONFIG")?
        .join("mailrs")
        .join("config.toml"))
}

/// `$MAILRS_DATA_DIR`, else `~/.local/share/mailrs`.
pub fn data_dir() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("MAILRS_DATA_DIR") {
        return Ok(PathBuf::from(path));
    }
    Ok(dirs::data_dir()
        .context("no data directory; set MAILRS_DATA_DIR")?
        .join("mailrs"))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::Config;

    #[test]
    fn a_minimal_config_uses_the_engine_defaults() {
        let config =
            Config::parse("[oauth]\nclient_id = \"id\"\nclient_secret = \"secret\"\n").unwrap();
        assert_eq!(config.oauth.client_id, "id");
        let engine = config.engine_config();
        assert_eq!(engine.poll_interval, Duration::from_secs(30));
        assert_eq!(engine.window_days, 30);
    }

    #[test]
    fn sync_settings_override_the_defaults() {
        let config = Config::parse(
            "[oauth]\nclient_id = \"id\"\nclient_secret = \"s\"\n[sync]\npoll_seconds = 10\nbody_cache_mb = 5\nwindow_days = 7\n",
        )
        .unwrap();
        let engine = config.engine_config();
        assert_eq!(engine.poll_interval, Duration::from_secs(10));
        assert_eq!(engine.body_cache_bytes, 5 * 1024 * 1024);
        assert_eq!(engine.window_days, 7);
    }

    #[test]
    fn the_oauth_section_is_required() {
        assert!(Config::parse("[sync]\npoll_seconds = 10\n").is_err());
    }
}
