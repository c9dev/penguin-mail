//! `config.toml` and the directories Penguin Mail uses. Both binaries read
//! it; the app's welcome page also writes it.

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

/// The directory name under the config, data, and cache directories.
pub const DIR_NAME: &str = "penguin-mail";

/// The name those directories had before the app was renamed.
const OLD_DIR_NAME: &str = "mailrs";

/// `$MAILRS_CONFIG`, else `~/.config/penguin-mail/config.toml`.
pub fn config_path() -> Result<PathBuf, ConfigError> {
    if let Some(path) = std::env::var_os("MAILRS_CONFIG") {
        return Ok(PathBuf::from(path));
    }
    let dir = dirs::config_dir().ok_or(ConfigError::NoDirectory("config", "MAILRS_CONFIG"))?;
    Ok(dir.join(DIR_NAME).join("config.toml"))
}

/// `$MAILRS_DATA_DIR`, else `~/.local/share/penguin-mail`.
pub fn data_dir() -> Result<PathBuf, ConfigError> {
    if let Some(path) = std::env::var_os("MAILRS_DATA_DIR") {
        return Ok(PathBuf::from(path));
    }
    let dir = dirs::data_dir().ok_or(ConfigError::NoDirectory("data", "MAILRS_DATA_DIR"))?;
    Ok(dir.join(DIR_NAME))
}

/// Renames `~/.config/mailrs`, `~/.local/share/mailrs`, and `~/.cache/mailrs`
/// to `penguin-mail`, so an upgrade keeps the config, settings, and mail
/// store. Call it at startup, before reading any of them.
pub fn migrate_old_dirs() {
    let bases = [dirs::config_dir(), dirs::data_dir(), dirs::cache_dir()];
    for base in bases.into_iter().flatten() {
        let (old, new) = (base.join(OLD_DIR_NAME), base.join(DIR_NAME));
        match move_old_dir(&old, &new) {
            Ok(true) => tracing::info!("moved {} to {}", old.display(), new.display()),
            Ok(false) => {}
            Err(err) => tracing::warn!(
                "could not move {} to {}: {err}",
                old.display(),
                new.display()
            ),
        }
    }
}

/// Renames `old` to `new` when `old` exists and `new` does not. Returns
/// whether it moved anything.
fn move_old_dir(old: &Path, new: &Path) -> std::io::Result<bool> {
    if new.exists() || !old.is_dir() {
        return Ok(false);
    }
    std::fs::rename(old, new)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    use super::{Config, move_old_dir};

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
    fn the_old_directory_moves_once() {
        let base = tempfile::tempdir().unwrap();
        let (old, new) = (base.path().join("mailrs"), base.path().join("penguin-mail"));
        std::fs::create_dir(&old).unwrap();
        std::fs::write(old.join("mailrs.db"), "mail").unwrap();
        assert!(move_old_dir(&old, &new).unwrap());
        assert!(!old.exists());
        assert_eq!(
            std::fs::read_to_string(new.join("mailrs.db")).unwrap(),
            "mail"
        );

        // A later start, or an old copy that recreated the directory, leaves
        // both alone.
        std::fs::create_dir(&old).unwrap();
        assert!(!move_old_dir(&old, &new).unwrap());
        assert!(old.exists() && new.join("mailrs.db").exists());
    }

    #[test]
    fn nothing_moves_on_a_fresh_install() {
        let base = tempfile::tempdir().unwrap();
        let new = base.path().join("penguin-mail");
        assert!(!move_old_dir(&base.path().join("mailrs"), &new).unwrap());
        assert!(!new.exists());
    }

    #[test]
    fn a_missing_file_is_reported_as_missing() {
        let err = Config::load(std::path::Path::new("/nonexistent/config.toml")).unwrap_err();
        assert!(err.is_missing());
    }
}
