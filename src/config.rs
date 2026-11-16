//! Application configuration and Workspace resolution.
//!
//! `AppConfig` is persisted as YAML (default:
//! `$XDG_CONFIG_HOME/rest-harness/config.yaml`, falling back to
//! `~/.config/rest-harness/config.yaml`; tests pass explicit paths).
//!
//! [`AppConfig::resolve_workspace`] picks the Workspace to open:
//! 1. `last_workspace_path` when set AND still an existing directory;
//! 2. otherwise the default Workspace `<cwd>/collections`, which is created
//!    and seeded with the sample Collection on first run.

use crate::domain::{HttpMethod, KeyValueRow, Request, RequestBody};
use crate::error::{io_error, Result};
use crate::storage::{FsWorkspaceStorage, WorkspaceStorage};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Name of the default Collection seeded into a brand-new Workspace.
pub const SAMPLE_COLLECTION_NAME: &str = "My public collection";

/// Name of the seeded sample Request.
pub const SAMPLE_REQUEST_NAME: &str = "Sample request";

/// Persisted application configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppConfig {
    /// Workspace root to reopen on startup, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_workspace_path: Option<PathBuf>,
}

impl AppConfig {
    /// Load config from an explicit path; a missing file yields the default
    /// config.
    pub fn load_from(path: &Path) -> Result<AppConfig> {
        if !path.is_file() {
            return Ok(AppConfig::default());
        }
        let text = std::fs::read_to_string(path).map_err(|e| io_error(path, e))?;
        Ok(serde_yaml::from_str(&text)?)
    }

    /// Save config to an explicit path, creating parent directories.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io_error(parent, e))?;
        }
        let text = serde_yaml::to_string(self)?;
        std::fs::write(path, text).map_err(|e| io_error(path, e))?;
        Ok(())
    }

    /// Default config file location on this OS (None when no home directory
    /// can be determined).
    pub fn default_config_path() -> Option<PathBuf> {
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            if !xdg.is_empty() {
                return Some(PathBuf::from(xdg).join("rest-harness").join("config.yaml"));
            }
        }
        let home = std::env::home_dir()?;
        Some(
            home.join(".config")
                .join("rest-harness")
                .join("config.yaml"),
        )
    }

    /// Load from [`AppConfig::default_config_path`]; default config when no
    /// location or file exists.
    pub fn load() -> Result<AppConfig> {
        match Self::default_config_path() {
            Some(path) => Self::load_from(&path),
            None => Ok(AppConfig::default()),
        }
    }

    /// Remember `workspace_root` as the last opened Workspace and persist
    /// the config to its default location (no-op when no home directory).
    pub fn store_last_workspace(&mut self, workspace_root: &Path) -> Result<()> {
        self.last_workspace_path = Some(workspace_root.to_path_buf());
        if let Some(path) = Self::default_config_path() {
            self.save_to(&path)?;
        }
        Ok(())
    }

    /// Resolve the Workspace root to open (see module docs), creating and
    /// seeding the default `<cwd>/collections` Workspace when it does not
    /// exist yet.
    pub fn resolve_workspace(&self, cwd: &Path) -> Result<PathBuf> {
        if let Some(path) = &self.last_workspace_path {
            if path.is_dir() {
                return Ok(path.clone());
            }
            // A stale path (moved/deleted) falls through to the default.
        }
        let default_root = cwd.join("collections");
        if !default_root.is_dir() {
            std::fs::create_dir_all(&default_root).map_err(|e| io_error(&default_root, e))?;
            seed_sample_collection(&default_root)?;
            crate::storage::ensure_gitignore(cwd)?;
        }
        Ok(default_root)
    }
}

/// Idempotently seed the sample Collection:
/// `My public collection/Sample request.yaml` — a GET to
/// `https://postman-echo.com/get?id=1` with the `id=1` query parameter.
/// Existing files are never overwritten.
pub fn seed_sample_collection(collections_root: &Path) -> Result<()> {
    let storage = FsWorkspaceStorage;
    let collection = storage.create_collection(collections_root, SAMPLE_COLLECTION_NAME)?;
    let request = Request {
        name: SAMPLE_REQUEST_NAME.to_string(),
        method: HttpMethod::Get,
        url: "https://postman-echo.com/get?id=1".to_string(),
        params: vec![KeyValueRow::new("id", "1")],
        headers: Vec::new(),
        auth: crate::domain::AuthConfig::None,
        body: RequestBody::None,
    };
    // Upsert semantics would clobber user edits on re-run, so only seed the
    // file when it does not exist yet.
    let target = collection.path.join(format!("{SAMPLE_REQUEST_NAME}.yaml"));
    if !target.is_file() {
        storage.save_request(&collection.path, &request)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trips_last_workspace_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("config.yaml");

        let mut config = AppConfig::default();
        assert_eq!(config.last_workspace_path, None);
        config.last_workspace_path = Some(PathBuf::from("/tmp/demo-workspace"));
        config.save_to(&path).unwrap();

        let loaded = AppConfig::load_from(&path).unwrap();
        assert_eq!(
            loaded.last_workspace_path,
            Some(PathBuf::from("/tmp/demo-workspace"))
        );
    }

    #[test]
    fn load_from_missing_file_yields_default() {
        let dir = tempfile::tempdir().unwrap();
        let config = AppConfig::load_from(&dir.path().join("nope.yaml")).unwrap();
        assert_eq!(config, AppConfig::default());
    }

    #[test]
    fn resolve_workspace_prefers_existing_last_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let last = dir.path().join("preferred");
        std::fs::create_dir_all(&last).unwrap();

        let config = AppConfig {
            last_workspace_path: Some(last.clone()),
        };
        let cwd = tempfile::tempdir().unwrap();
        let resolved = config.resolve_workspace(cwd.path()).unwrap();
        assert_eq!(resolved, last);
        // No default workspace was created.
        assert!(!cwd.path().join("collections").exists());
    }

    #[test]
    fn resolve_workspace_falls_back_when_last_workspace_missing() {
        let dir = tempfile::tempdir().unwrap();
        let config = AppConfig {
            last_workspace_path: Some(dir.path().join("deleted")),
        };
        let cwd = tempfile::tempdir().unwrap();
        let resolved = config.resolve_workspace(cwd.path()).unwrap();
        assert_eq!(resolved, cwd.path().join("collections"));
        assert!(resolved.is_dir());
    }
}
