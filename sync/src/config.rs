//! `config.toml` and the directories mailrs uses. Both binaries read it; the
//! app's welcome page also writes it.

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::EngineConfig;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path} is not valid: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("could not write {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("no {0} directory; set {1}")]
    NoDirectory(&'static str, &'static str),
}

impl ConfigError {
    /// True when the file does not exist yet, as on a first run.
    pub fn is_missing(&self) -> bool {
        matches!(self, ConfigError::Read { source, .. } if source.kind() == std::io::ErrorKind::NotFound)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub oauth: OAuthConfig,
    #[serde(default, skip_serializing_if = "SyncConfig::is_default")]
    pub sync: SyncConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthConfig {
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub poll_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_cache_mb: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_days: Option<i64>,
}

impl SyncConfig {
    fn is_default(&self) -> bool {
        *self == SyncConfig::default()
    }
}

impl Config {
    pub fn new(client_id: impl Into<String>, client_secret: impl Into<String>) -> Self {
        Config {
            oauth: OAuthConfig {
                client_id: client_id.into(),
                client_secret: client_secret.into(),
            },
            sync: SyncConfig::default(),
        }
    }

    pub fn parse(text: &str) -> Result<Config, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }

    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&text).map_err(|message| ConfigError::Parse {
            path: path.to_path_buf(),
            message,
        })
    }

    /// Writes the file readable only by its owner, since it holds the client secret.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let write_error = |source| ConfigError::Write {
            path: path.to_path_buf(),
            source,
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(write_error)?;
        }
        let text =
            toml::to_string(self).map_err(|e| write_error(std::io::Error::other(e.to_string())))?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(write_error)?;
        file.write_all(text.as_bytes()).map_err(write_error)
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
pub fn config_path() -> Result<PathBuf, ConfigError> {
    if let Some(path) = std::env::var_os("MAILRS_CONFIG") {
        return Ok(PathBuf::from(path));
    }
    let dir = dirs::config_dir().ok_or(ConfigError::NoDirectory("config", "MAILRS_CONFIG"))?;
    Ok(dir.join("mailrs").join("config.toml"))
}

/// `$MAILRS_DATA_DIR`, else `~/.local/share/mailrs`.
pub fn data_dir() -> Result<PathBuf, ConfigError> {
    if let Some(path) = std::env::var_os("MAILRS_DATA_DIR") {
        return Ok(PathBuf::from(path));
    }
    let dir = dirs::data_dir().ok_or(ConfigError::NoDirectory("data", "MAILRS_DATA_DIR"))?;
    Ok(dir.join("mailrs"))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
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

    #[test]
    fn saved_configs_load_back_and_stay_private() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("config.toml");
        let config = Config::new("id.apps.googleusercontent.com", "GOCSPX-secret");
        config.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), config);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn a_missing_file_is_reported_as_missing() {
        let err = Config::load(std::path::Path::new("/nonexistent/config.toml")).unwrap_err();
        assert!(err.is_missing());
    }
}
