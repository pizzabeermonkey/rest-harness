//! Filesystem persistence for REST Harness (ADR 0002).
//!
//! Layout on disk:
//!
//! ```text
//! <workspace root>/
//! ├── .gitignore                    # ignores secrets + local history
//! ├── .rest-harness-history.yaml    # gitignored History Entries
//! ├── <Collection name>/            # one directory per Collection
//! │   ├── .secrets.yaml             # gitignored Collection-local Secrets
//! │   ├── environments/             # one flat YAML file per Environment
//! │   │   └── <Environment name>.yaml
//! │   ├── <Folder name>/            # nested Folders, recursively
//! │   │   └── ...
//! │   └── <Request name>.yaml       # one YAML file per Request
//! └── ...
//! ```
//!
//! Conventions and decisions:
//! - Names are sanitized ONLY for OS-forbidden characters; interior spaces
//!   and exact display casing are preserved (`My public collection`).
//! - Reserved names: inside a Collection, the `environments` directory and
//!   `.secrets.yaml` are never treated as Folders/Requests; at the workspace
//!   root, hidden dot-files/dirs (`.rest-harness/`, `.git`, ...) are never
//!   treated as Collections.
//! - `save_request` is an upsert; `create_request` fails if the target file
//!   already exists.
//! - When walking the tree, `.yaml` files that do not parse as a [`Request`]
//!   are skipped gracefully (they may be editor scratch files or foreign
//!   YAML); they are never deleted.
//! - Secret values are only ever written to `.secrets.yaml` — no save path
//!   serializes Secrets into Request or Environment files (ADR 0003).

use crate::domain::{
    Collection, Environment, Folder, HistoryEntry, Request, RequestEntry, SecretStore,
};
use crate::error::{io_error, Error, Result};
use crate::history::HistoryStore;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

/// Port (interface) for Workspace persistence, implemented by
/// [`FsWorkspaceStorage`]. Kept as a trait so downstream tasks (GUI, tests)
/// can substitute implementations.
pub trait WorkspaceStorage {
    /// Walk the tree under `root` and build a [`Workspace`].
    fn load_workspace(&self, root: &Path) -> Result<crate::domain::Workspace>;

    /// Re-read the Workspace from disk, reflecting external edits.
    fn reload_workspace(&self, root: &Path) -> Result<crate::domain::Workspace>;

    /// Create a new Collection directory (with an empty `environments/`
    /// subdirectory) inside the workspace root.
    fn create_collection(&self, workspace_root: &Path, name: &str) -> Result<Collection>;

    /// Rename a Collection directory (name sanitized).
    fn rename_collection(&self, collection_path: &Path, new_name: &str) -> Result<PathBuf>;

    /// Delete a Collection directory recursively.
    fn delete_collection(&self, collection_path: &Path) -> Result<()>;

    /// Create a Folder inside `parent_dir` (a Collection or another Folder).
    fn create_folder(&self, parent_dir: &Path, name: &str) -> Result<Folder>;

    /// Rename a Folder directory (name sanitized).
    fn rename_folder(&self, folder_path: &Path, new_name: &str) -> Result<PathBuf>;

    /// Deep-copy a Folder into the same parent under a non-colliding name
    /// (`<Name> copy`, then `<Name> copy 2`, ...).
    fn duplicate_folder(&self, folder_path: &Path) -> Result<Folder>;

    /// Delete a Folder directory recursively.
    fn delete_folder(&self, folder_path: &Path) -> Result<()>;

    /// Write a Request as `<sanitized name>.yaml` inside `parent_dir`
    /// (upsert: overwrites an existing file with the same name).
    fn save_request(&self, parent_dir: &Path, request: &Request) -> Result<PathBuf>;

    /// Like [`WorkspaceStorage::save_request`] but fails when the target
    /// file already exists.
    fn create_request(&self, parent_dir: &Path, request: &Request) -> Result<PathBuf>;

    /// Rename a Request: renames the `.yaml` file AND rewrites the `name`
    /// field inside it.
    fn rename_request(&self, request_path: &Path, new_name: &str) -> Result<PathBuf>;

    /// Duplicate a Request file under a non-colliding name, updating the
    /// `name` field of the copy.
    fn duplicate_request(&self, request_path: &Path) -> Result<(PathBuf, Request)>;

    /// Delete a Request `.yaml` file.
    fn delete_request(&self, request_path: &Path) -> Result<()>;

    /// Write an Environment as a flat YAML mapping at
    /// `environments/<sanitized name>.yaml` inside the Collection.
    fn save_environment(&self, collection_dir: &Path, env: &Environment) -> Result<PathBuf>;

    /// Load all Environments of a Collection (empty when the `environments`
    /// directory does not exist).
    fn load_environments(&self, collection_dir: &Path) -> Result<Vec<Environment>>;

    /// Load a Collection's Secret store (empty store when `.secrets.yaml`
    /// does not exist).
    fn load_secrets(&self, collection_dir: &Path) -> Result<SecretStore>;

    /// Write the Collection's Secret store to `.secrets.yaml`.
    fn save_secrets(&self, collection_dir: &Path, secrets: &SecretStore) -> Result<PathBuf>;
}

/// The default filesystem-backed [`WorkspaceStorage`].
#[derive(Debug, Clone, Copy, Default)]
pub struct FsWorkspaceStorage;

/// Sanitize a display name for use as a file or directory name.
///
/// Only OS-forbidden characters are replaced with `_`: `/`, `\`, NUL, the
/// Windows-forbidden `< > : " | ? *`, and control characters. Interior
/// spaces and exact display casing are preserved. Empty and dot-only
/// names (`.` / `..`) are rejected.
pub fn sanitize_filename(name: &str) -> Result<String> {
    let forbidden = ['/', '\\', '<', '>', ':', '"', '|', '?', '*', '\0'];
    let sanitized: String = name
        .trim()
        .chars()
        .map(|c| {
            if forbidden.contains(&c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();

    if sanitized.is_empty() {
        return Err(Error::InvalidName {
            name: name.to_string(),
            reason: "name is empty".to_string(),
        });
    }
    if sanitized.chars().all(|c| c == '.') {
        return Err(Error::InvalidName {
            name: name.to_string(),
            reason: "dot-only names are reserved".to_string(),
        });
    }
    Ok(sanitized)
}

/// Directory name reserved for Environments inside a Collection.
pub const ENVIRONMENTS_DIR: &str = "environments";

/// File name reserved for a Collection's Secret store.
pub const SECRETS_FILE: &str = ".secrets.yaml";

/// File name of the Workspace's gitignored History Entries.
pub const HISTORY_FILE: &str = ".rest-harness-history.yaml";

/// File name of the Workspace's local state directory.
pub const LOCAL_STATE_DIR: &str = ".rest-harness";

/// Ensure the Workspace root `.gitignore` ignores Secrets, History Entries
/// and the local state directory. Missing entries are appended (with a
/// comment); already-covered entries (exact or `**/`-prefixed) are not
/// duplicated.
pub fn ensure_gitignore(workspace_root: &Path) -> Result<()> {
    let entries = [
        SECRETS_FILE.to_string(),
        HISTORY_FILE.to_string(),
        format!("{LOCAL_STATE_DIR}/"),
    ];
    let gitignore_path = workspace_root.join(".gitignore");
    let existing = match fs::read_to_string(&gitignore_path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(io_error(&gitignore_path, e)),
    };

    let covered = |entry: &str| {
        existing.lines().any(|line| {
            let line = line.trim();
            line == entry
                || line == format!("**/{entry}")
                || line == format!("**/{LOCAL_STATE_DIR}/")
        })
    };
    let missing: Vec<&String> = entries.iter().filter(|e| !covered(e)).collect();
    if missing.is_empty() {
        return Ok(());
    }

    let mut text = existing.clone();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str("\n# rest-harness local secrets and execution history\n");
    for entry in missing {
        text.push_str(entry);
        text.push('\n');
    }
    fs::write(&gitignore_path, text).map_err(|e| io_error(&gitignore_path, e))?;
    Ok(())
}

/// Request files must use this extension.
const REQUEST_EXT: &str = "yaml";

impl FsWorkspaceStorage {
    fn create_dir(&self, path: &Path) -> Result<()> {
        fs::create_dir_all(path).map_err(|e| io_error(path, e))
    }

    fn require_dir(&self, path: &Path) -> Result<()> {
        let meta = fs::metadata(path).map_err(|e| io_error(path, e))?;
        if !meta.is_dir() {
            return Err(Error::Other(format!(
                "{} is not a directory",
                path.display()
            )));
        }
        Ok(())
    }

    fn require_file(&self, path: &Path) -> Result<()> {
        let meta = fs::metadata(path).map_err(|e| io_error(path, e))?;
        if !meta.is_file() {
            return Err(Error::Other(format!("{} is not a file", path.display())));
        }
        Ok(())
    }

    /// Recursively walk a directory, collecting nested Folders and Requests.
    fn load_folder(&self, dir: &Path) -> Result<Folder> {
        let mut folders = Vec::new();
        let mut requests = Vec::new();
        for entry in sorted_entries(dir)? {
            let path = entry.path();
            if entry_type(&entry, dir)?.is_dir() {
                if is_hidden(&path) {
                    continue;
                }
                folders.push(self.load_folder(&path)?);
            } else if let Some(req_entry) = try_load_request_entry(&path)? {
                requests.push(req_entry);
            }
        }
        Ok(Folder {
            name: dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string(),
            path: dir.to_path_buf(),
            folders,
            requests,
        })
    }

    /// Walk a Collection directory for Folders/Requests, then load
    /// Environments and Secrets.
    fn load_collection(&self, dir: &Path) -> Result<Collection> {
        let mut folders = Vec::new();
        let mut requests = Vec::new();
        for entry in sorted_entries(dir)? {
            let path = entry.path();
            if entry_type(&entry, dir)?.is_dir() {
                if is_hidden(&path)
                    || path.file_name().and_then(|n| n.to_str()) == Some(ENVIRONMENTS_DIR)
                {
                    continue;
                }
                folders.push(self.load_folder(&path)?);
            } else if let Some(req_entry) = try_load_request_entry(&path)? {
                requests.push(req_entry);
            }
        }
        let environments = self.load_environments(dir)?;
        let secrets = self.load_secrets(dir)?;
        Ok(Collection {
            name: dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string(),
            path: dir.to_path_buf(),
            folders,
            requests,
            environments,
            secrets,
        })
    }

    /// Next non-colliding sibling name: `<name> copy`, `<name> copy 2`, ...
    fn unique_name(&self, dir: &Path, name: &str, suffix: &str) -> Result<(String, PathBuf)> {
        let mut candidate = format!("{name} copy");
        let mut index = 1u32;
        loop {
            let path = dir.join(format!("{candidate}{suffix}"));
            if !path.exists() {
                return Ok((candidate, path));
            }
            index += 1;
            candidate = format!("{name} copy {index}");
        }
    }
}

impl WorkspaceStorage for FsWorkspaceStorage {
    fn load_workspace(&self, root: &Path) -> Result<crate::domain::Workspace> {
        self.require_dir(root)?;
        let mut collections = Vec::new();
        for entry in sorted_entries(root)? {
            let path = entry.path();
            if entry_type(&entry, root)?.is_dir() && !is_hidden(&path) {
                collections.push(self.load_collection(&path)?);
            }
            // Hidden dot-files/dirs at the root (e.g. `.rest-harness/`,
            // `.rest-harness-history.yaml`, `.git`) are never Collections.
        }
        let history: Vec<HistoryEntry> = HistoryStore::load_at(root)?;
        Ok(crate::domain::Workspace {
            root: root.to_path_buf(),
            collections,
            history,
        })
    }

    fn reload_workspace(&self, root: &Path) -> Result<crate::domain::Workspace> {
        // Always re-reads from disk; identical to `load_workspace` by design
        // (documented so callers know external edits are reflected).
        self.load_workspace(root)
    }

    fn create_collection(&self, workspace_root: &Path, name: &str) -> Result<Collection> {
        self.require_dir(workspace_root)?;
        let sanitized = sanitize_filename(name)?;
        let path = workspace_root.join(&sanitized);
        if path.exists() {
            return Err(Error::AlreadyExists(path));
        }
        self.create_dir(&path)?;
        // Create the environments/ directory up front so the Collection
        // structure is discoverable; empty directories are not tracked by
        // git, which is harmless (see ADR 0002).
        self.create_dir(&path.join(ENVIRONMENTS_DIR))?;
        ensure_gitignore(workspace_root)?;
        Ok(Collection {
            name: sanitized,
            path,
            folders: Vec::new(),
            requests: Vec::new(),
            environments: Vec::new(),
            secrets: SecretStore::empty(),
        })
    }

    fn rename_collection(&self, collection_path: &Path, new_name: &str) -> Result<PathBuf> {
        self.require_dir(collection_path)?;
        let sanitized = sanitize_filename(new_name)?;
        let parent = collection_path
            .parent()
            .ok_or_else(|| Error::Other("collection path has no parent".into()))?;
        let target = parent.join(&sanitized);
        if target.exists() {
            return Err(Error::AlreadyExists(target));
        }
        fs::rename(collection_path, &target).map_err(|e| io_error(collection_path, e))?;
        Ok(target)
    }

    fn delete_collection(&self, collection_path: &Path) -> Result<()> {
        self.require_dir(collection_path)?;
        fs::remove_dir_all(collection_path).map_err(|e| io_error(collection_path, e))
    }

    fn create_folder(&self, parent_dir: &Path, name: &str) -> Result<Folder> {
        self.require_dir(parent_dir)?;
        let sanitized = sanitize_filename(name)?;
        let path = parent_dir.join(&sanitized);
        if path.exists() {
            return Err(Error::AlreadyExists(path));
        }
        self.create_dir(&path)?;
        Ok(Folder {
            name: sanitized,
            path,
            folders: Vec::new(),
            requests: Vec::new(),
        })
    }

    fn rename_folder(&self, folder_path: &Path, new_name: &str) -> Result<PathBuf> {
        self.require_dir(folder_path)?;
        let sanitized = sanitize_filename(new_name)?;
        let parent = folder_path
            .parent()
            .ok_or_else(|| Error::Other("folder path has no parent".into()))?;
        let target = parent.join(&sanitized);
        if target.exists() {
            return Err(Error::AlreadyExists(target));
        }
        fs::rename(folder_path, &target).map_err(|e| io_error(folder_path, e))?;
        Ok(target)
    }

    fn duplicate_folder(&self, folder_path: &Path) -> Result<Folder> {
        self.require_dir(folder_path)?;
        let name = folder_path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| Error::Other("folder path has no file name".into()))?
            .to_string();
        let parent = folder_path
            .parent()
            .ok_or_else(|| Error::Other("folder path has no parent".into()))?;
        let (_, target) = self.unique_name(parent, &name, "")?;
        copy_dir_all(folder_path, &target)?;
        Ok(Folder {
            name: target
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string(),
            path: target,
            folders: Vec::new(),
            requests: Vec::new(),
        })
    }

    fn delete_folder(&self, folder_path: &Path) -> Result<()> {
        self.require_dir(folder_path)?;
        fs::remove_dir_all(folder_path).map_err(|e| io_error(folder_path, e))
    }

    fn save_request(&self, parent_dir: &Path, request: &Request) -> Result<PathBuf> {
        self.require_dir(parent_dir)?;
        let sanitized = sanitize_filename(&request.name)?;
        let path = parent_dir.join(format!("{sanitized}.{REQUEST_EXT}"));
        write_yaml(&path, request)?;
        Ok(path)
    }

    fn create_request(&self, parent_dir: &Path, request: &Request) -> Result<PathBuf> {
        self.require_dir(parent_dir)?;
        let sanitized = sanitize_filename(&request.name)?;
        let path = parent_dir.join(format!("{sanitized}.{REQUEST_EXT}"));
        if path.exists() {
            return Err(Error::AlreadyExists(path));
        }
        write_yaml(&path, request)?;
        Ok(path)
    }

    fn rename_request(&self, request_path: &Path, new_name: &str) -> Result<PathBuf> {
        self.require_file(request_path)?;
        let mut request = load_request(request_path)?;
        request.name = new_name.to_string();
        let parent = request_path
            .parent()
            .ok_or_else(|| Error::Other("request path has no parent".into()))?;
        let sanitized = sanitize_filename(new_name)?;
        let target = parent.join(format!("{sanitized}.{REQUEST_EXT}"));
        if target != request_path && target.exists() {
            return Err(Error::AlreadyExists(target));
        }
        write_yaml(&target, &request)?;
        if target != request_path {
            fs::remove_file(request_path).map_err(|e| io_error(request_path, e))?;
        }
        Ok(target)
    }

    fn duplicate_request(&self, request_path: &Path) -> Result<(PathBuf, Request)> {
        self.require_file(request_path)?;
        let original = load_request(request_path)?;
        let parent = request_path
            .parent()
            .ok_or_else(|| Error::Other("request path has no parent".into()))?;
        let (copy_name, target) =
            self.unique_name(parent, &original.name, format!(".{REQUEST_EXT}").as_str())?;
        let mut copy = original;
        copy.name = copy_name;
        write_yaml(&target, &copy)?;
        Ok((target, copy))
    }

    fn delete_request(&self, request_path: &Path) -> Result<()> {
        self.require_file(request_path)?;
        fs::remove_file(request_path).map_err(|e| io_error(request_path, e))
    }

    fn save_environment(&self, collection_dir: &Path, env: &Environment) -> Result<PathBuf> {
        self.require_dir(collection_dir)?;
        let sanitized = sanitize_filename(&env.name)?;
        let env_dir = collection_dir.join(ENVIRONMENTS_DIR);
        self.create_dir(&env_dir)?;
        let path = env_dir.join(format!("{sanitized}.{REQUEST_EXT}"));
        // Serialize ONLY the flat variable mapping — the Environment's name
        // lives in the filename and its path in the directory, and Secret
        // values live exclusively in `.secrets.yaml` (ADR 0003).
        write_yaml(&path, &env.variables)?;
        Ok(path)
    }

    fn load_environments(&self, collection_dir: &Path) -> Result<Vec<Environment>> {
        let env_dir = collection_dir.join(ENVIRONMENTS_DIR);
        if !env_dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut environments = Vec::new();
        for entry in sorted_entries(&env_dir)? {
            let path = entry.path();
            if !entry_type(&entry, &env_dir)?.is_file() || is_hidden(&path) {
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some(REQUEST_EXT) {
                continue;
            }
            let text = fs::read_to_string(&path).map_err(|e| io_error(&path, e))?;
            let variables: std::collections::BTreeMap<String, String> =
                serde_yaml::from_str(&text)?;
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            environments.push(Environment {
                name,
                path,
                variables,
            });
        }
        environments.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(environments)
    }

    fn load_secrets(&self, collection_dir: &Path) -> Result<SecretStore> {
        let path = collection_dir.join(SECRETS_FILE);
        if !path.is_file() {
            return Ok(SecretStore::empty());
        }
        let text = fs::read_to_string(&path).map_err(|e| io_error(&path, e))?;
        serde_yaml::from_str(&text).map_err(Error::Yaml)
    }

    fn save_secrets(&self, collection_dir: &Path, secrets: &SecretStore) -> Result<PathBuf> {
        self.require_dir(collection_dir)?;
        let path = collection_dir.join(SECRETS_FILE);
        write_yaml(&path, secrets)?;
        Ok(path)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Whether a path's file name is hidden (starts with `.`).
fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with('.'))
}

/// Entry file type, with the directory attached for error reporting.
fn entry_type(entry: &fs::DirEntry, dir: &Path) -> Result<fs::FileType> {
    entry.file_type().map_err(|e| io_error(dir, e))
}

/// Deterministically sorted directory entries by file name.
fn sorted_entries(dir: &Path) -> Result<Vec<fs::DirEntry>> {
    let mut entries: Vec<fs::DirEntry> = fs::read_dir(dir)
        .map_err(|e| io_error(dir, e))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| io_error(dir, e))?;
    entries.sort_by_key(|e| e.file_name());
    Ok(entries)
}

/// Pretty, deterministic YAML serialization to a file.
fn write_yaml<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let text = serde_yaml::to_string(value)?;
    fs::write(path, text).map_err(|e| io_error(path, e))
}

/// Parse a `.yaml` file as a [`Request`], returning `Ok(None)` when the file
/// is not a valid Request (skipped gracefully, never deleted).
fn try_load_request_entry(path: &Path) -> Result<Option<RequestEntry>> {
    if is_hidden(path) || path.extension().and_then(|e| e.to_str()) != Some(REQUEST_EXT) {
        return Ok(None);
    }
    let text = fs::read_to_string(path).map_err(|e| io_error(path, e))?;
    let request: Request = match serde_yaml::from_str(&text) {
        Ok(request) => request,
        Err(_) => return Ok(None), // not a Request file; skip it
    };
    Ok(Some(RequestEntry {
        name: request.name,
        path: path.to_path_buf(),
    }))
}

/// Load and validate a full [`Request`] from a `.yaml` file.
fn load_request(path: &Path) -> Result<Request> {
    let text = fs::read_to_string(path).map_err(|e| io_error(path, e))?;
    let request: Request = serde_yaml::from_str(&text)?;
    Ok(request)
}

/// Recursive directory copy (std has no `copy_dir_all`).
fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst).map_err(|e| io_error(dst, e))?;
    for entry in sorted_entries(src)? {
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry_type(&entry, src)?.is_dir() {
            copy_dir_all(&from, &to)?;
        } else {
            fs::copy(&from, &to).map_err(|e| io_error(&from, e))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_replaces_os_forbidden_characters() {
        assert_eq!(
            sanitize_filename("a/b\\c:d<e>f\"g|h?i*j").unwrap(),
            "a_b_c_d_e_f_g_h_i_j"
        );
        assert_eq!(sanitize_filename("tab\tchar").unwrap(), "tab_char");
        assert_eq!(sanitize_filename("nul\0").unwrap(), "nul_");
    }

    #[test]
    fn sanitize_preserves_spaces_and_casing() {
        assert_eq!(
            sanitize_filename("My public collection").unwrap(),
            "My public collection"
        );
        assert_eq!(
            sanitize_filename("Sample request").unwrap(),
            "Sample request"
        );
    }

    #[test]
    fn sanitize_rejects_empty_and_dot_only_names() {
        assert!(sanitize_filename("").is_err());
        assert!(sanitize_filename("   ").is_err());
        assert!(sanitize_filename(".").is_err());
        assert!(sanitize_filename("..").is_err());
        assert!(sanitize_filename("...").is_err());
    }
}
