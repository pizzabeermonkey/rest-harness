//! Integration tests for REST Harness v1 persistence, driven entirely
//! through the public API against `tempfile::TempDir` workspaces
//! (no network, no real HTTP calls).

use rest_harness::config::{AppConfig, SAMPLE_COLLECTION_NAME, SAMPLE_REQUEST_NAME};
use rest_harness::domain::{
    ApiKeyLocation, AuthConfig, Environment, HistoryEntry, HistoryResponse, HttpMethod,
    KeyValueRow, RawContentType, RedactedHttpRequest, Request, RequestBody, SecretStore,
};
use rest_harness::history::{HistoryStore, MAX_HISTORY_ENTRIES, MAX_RESPONSE_BODY_BYTES};
use rest_harness::storage::{
    ensure_gitignore, sanitize_filename, FsWorkspaceStorage, WorkspaceStorage, ENVIRONMENTS_DIR,
    HISTORY_FILE, SECRETS_FILE,
};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

fn storage() -> FsWorkspaceStorage {
    FsWorkspaceStorage
}

fn request(name: &str, url: &str) -> Request {
    Request {
        name: name.to_string(),
        method: HttpMethod::Get,
        url: url.to_string(),
        params: vec![KeyValueRow::new("id", "1")],
        headers: vec![KeyValueRow::new("accept", "application/json")],
        auth: AuthConfig::None,
        body: RequestBody::None,
    }
}

fn json_request(name: &str, body: &str) -> Request {
    Request {
        name: name.to_string(),
        method: HttpMethod::Post,
        url: "https://postman-echo.com/post".to_string(),
        headers: vec![KeyValueRow::new("content-type", "application/json")],
        body: RequestBody::Raw {
            content_type: RawContentType::Json,
            content: body.to_string(),
        },
        ..request(name, "https://postman-echo.com/post")
    }
}

fn sample_workspace() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("collections");
    fs::create_dir_all(&root).unwrap();
    (dir, root)
}

fn secret_values(store: &SecretStore) -> Vec<String> {
    store.secret_values_for_env(None)
}

fn history_entry(method: HttpMethod, url: &str, req_body: &str, resp_body: &str) -> HistoryEntry {
    HistoryEntry {
        request: RedactedHttpRequest {
            method,
            url: url.to_string(),
            headers: vec![],
            body: req_body.to_string(),
        },
        response: HistoryResponse {
            status_code: 200,
            reason_phrase: "OK".into(),
            elapsed_ms: 10,
            size_bytes: resp_body.len() as u64,
            headers: vec![],
            body: resp_body.to_string(),
            body_truncated: false,
            timestamp: chrono::Utc::now(),
        },
    }
}

// ---------------------------------------------------------------------------
// Workspace resolution, loading, reload
// ---------------------------------------------------------------------------

#[test]
fn resolve_workspace_creates_and_seeds_default_workspace() {
    let cwd = tempfile::tempdir().unwrap();
    let config = AppConfig::default();

    let root = config.resolve_workspace(cwd.path()).unwrap();
    assert_eq!(root, cwd.path().join("collections"));
    assert!(root.is_dir());

    // Sample Collection + Request seeded.
    let collection = root.join(SAMPLE_COLLECTION_NAME);
    assert!(collection.is_dir());
    let sample = collection.join(format!("{SAMPLE_REQUEST_NAME}.yaml"));
    assert!(sample.is_file());

    // Sample request content: GET with id=1 query param (id also in URL).
    let text = fs::read_to_string(&sample).unwrap();
    let req: Request = serde_yaml::from_str(&text).unwrap();
    assert_eq!(req.method, HttpMethod::Get);
    assert!(req.url.contains("id=1"));
    assert!(req.params.iter().any(|p| p.key == "id" && p.value == "1"));

    // Gitignore entries created by seeding.
    let gitignore = fs::read_to_string(cwd.path().join(".gitignore")).unwrap();
    assert!(gitignore.contains(SECRETS_FILE));
    assert!(gitignore.contains(HISTORY_FILE));

    // Seeding is idempotent: second run does not duplicate or clobber.
    fs::write(&sample, "name: edited by user\nmethod: delete\nurl: x\n").unwrap();
    let config2 = AppConfig::default();
    config2.resolve_workspace(cwd.path()).unwrap();
    let text2 = fs::read_to_string(&sample).unwrap();
    assert!(
        text2.contains("edited by user"),
        "seeding must not overwrite"
    );
}

#[test]
fn resolve_workspace_uses_last_workspace_when_it_exists() {
    let cwd = tempfile::tempdir().unwrap();
    let config_dir = tempfile::tempdir().unwrap();
    let config_path = config_dir.path().join("config.yaml");

    let mut config = AppConfig::load_from(&config_path).unwrap();
    config.last_workspace_path = Some(PathBuf::from("/nonexistent/workspace"));
    config.save_to(&config_path).unwrap();

    // Stale path -> falls back to default and that choice is persisted.
    let resolved = AppConfig::load_from(&config_path)
        .unwrap()
        .resolve_workspace(cwd.path())
        .unwrap();
    assert_eq!(resolved, cwd.path().join("collections"));

    let mut config = AppConfig::load_from(&config_path).unwrap();
    config.last_workspace_path = Some(resolved.clone());
    config.save_to(&config_path).unwrap();
    let resolved_again = AppConfig::load_from(&config_path)
        .unwrap()
        .resolve_workspace(cwd.path())
        .unwrap();
    assert_eq!(resolved_again, resolved, "persisted path must win");
}

#[test]
fn load_workspace_reflects_external_edits_after_reload() {
    let (_dir, root) = sample_workspace();
    let s = storage();

    let workspace = s.load_workspace(&root).unwrap();
    assert_eq!(workspace.collections.len(), 0);

    // External edit: create a Collection + Request behind the app's back.
    let col = root.join("External");
    fs::create_dir_all(col.join(ENVIRONMENTS_DIR)).unwrap();
    fs::write(
        col.join("External request.yaml"),
        "name: External request\nmethod: get\nurl: https://example.com\n",
    )
    .unwrap();

    let reloaded = s.reload_workspace(&root).unwrap();
    assert_eq!(reloaded.collections.len(), 1);
    let collection = &reloaded.collections[0];
    assert_eq!(collection.name, "External");
    assert_eq!(collection.requests.len(), 1);
    assert_eq!(collection.requests[0].name, "External request");
    assert_eq!(
        collection.requests[0].path,
        col.join("External request.yaml")
    );
    assert_eq!(collection.total_request_count(), 1);
}

#[test]
fn load_workspace_walks_nested_folders_and_skips_foreign_yaml() {
    let (_dir, root) = sample_workspace();
    let s = storage();

    let col = s.create_collection(&root, "Api").unwrap();
    let users = s.create_folder(&col.path, "Users").unwrap();
    let admin = s.create_folder(&users.path, "Admin").unwrap();

    s.save_request(&col.path, &request("Top level", "https://a.example.com"))
        .unwrap();
    s.save_request(&users.path, &request("List users", "https://b.example.com"))
        .unwrap();
    s.save_request(
        &admin.path,
        &request("Delete admin", "https://c.example.com"),
    )
    .unwrap();

    // A non-request YAML file inside a Folder is skipped, not crashed on.
    fs::write(
        users.path.join("notes.yaml"),
        "this is: not a request\njust: some yaml\n",
    )
    .unwrap();

    let workspace = s.load_workspace(&root).unwrap();
    let collection = &workspace.collections[0];
    assert_eq!(collection.name, "Api");
    assert_eq!(collection.folders.len(), 1);

    let users_folder = &collection.folders[0];
    assert_eq!(users_folder.name, "Users");
    assert_eq!(users_folder.requests.len(), 1);
    assert_eq!(users_folder.folders.len(), 1);

    let admin_folder = &users_folder.folders[0];
    assert_eq!(admin_folder.name, "Admin");
    assert_eq!(admin_folder.requests.len(), 1);

    // Recursive counts include every request, but not the foreign YAML.
    assert_eq!(users_folder.total_request_count(), 2);
    assert_eq!(collection.total_request_count(), 3);
}

// ---------------------------------------------------------------------------
// CRUD: Collections, Folders, Requests
// ---------------------------------------------------------------------------

#[test]
fn collection_crud_round_trip() {
    let (_dir, root) = sample_workspace();
    let s = storage();

    let col = s.create_collection(&root, "My public collection").unwrap();
    assert_eq!(col.name, "My public collection");
    assert!(col.path.join(ENVIRONMENTS_DIR).is_dir());

    // Duplicate creation fails.
    assert!(s.create_collection(&root, "My public collection").is_err());

    // Rename preserves sanitization; new path returned.
    let renamed = s
        .rename_collection(&col.path, "Renamed collection")
        .unwrap();
    assert_eq!(renamed.file_name().unwrap(), "Renamed collection");
    assert!(renamed.is_dir());
    assert!(!col.path.exists());

    // Delete.
    s.delete_collection(&renamed).unwrap();
    assert!(!renamed.exists());
    assert!(s.reload_workspace(&root).unwrap().collections.is_empty());
}

#[test]
fn folder_crud_including_deep_duplicate() {
    let (_dir, root) = sample_workspace();
    let s = storage();

    let col = s.create_collection(&root, "Api").unwrap();
    let users = s.create_folder(&col.path, "Users").unwrap();
    let admin = s.create_folder(&users.path, "Admin").unwrap();

    s.save_request(
        &admin.path,
        &request("Delete admin", "https://c.example.com"),
    )
    .unwrap();
    s.save_request(&users.path, &request("List users", "https://b.example.com"))
        .unwrap();

    // Duplicate_folder deep-copies the whole subtree.
    let copy = s.duplicate_folder(&users.path).unwrap();
    assert_eq!(copy.name, "Users copy");
    let deep_copy = copy.path.join("Admin").join("Delete admin.yaml");
    assert!(deep_copy.is_file(), "folder duplicate must be deep");

    // Second duplicate gets a non-colliding name.
    let copy2 = s.duplicate_folder(&users.path).unwrap();
    assert_eq!(copy2.name, "Users copy 2");

    // Rename folder.
    let renamed = s.rename_folder(&copy2.path, "Renamed folder").unwrap();
    assert_eq!(renamed.file_name().unwrap(), "Renamed folder");

    // Delete folder recursively.
    s.delete_folder(&renamed).unwrap();
    assert!(!renamed.exists());

    let workspace = s.load_workspace(&root).unwrap();
    let collection = &workspace.collections[0];
    assert_eq!(collection.folders.len(), 2); // Users + Users copy
    assert_eq!(collection.total_request_count(), 4);
}

#[test]
fn request_crud_upsert_create_and_rename_rewrites_name_field() {
    let (_dir, root) = sample_workspace();
    let s = storage();

    let col = s.create_collection(&root, "Api").unwrap();

    // create_request errors when the file already exists.
    let req = request("Sample request", "https://postman-echo.com/get?id=1");
    let path = s.create_request(&col.path, &req).unwrap();
    assert!(path.is_file());
    assert!(s.create_request(&col.path, &req).is_err());

    // save_request is an upsert: overwrites the same file.
    let mut updated = req.clone();
    updated.method = HttpMethod::Post;
    let same_path = s.save_request(&col.path, &updated).unwrap();
    assert_eq!(same_path, path);
    let saved = serde_yaml::from_str::<Request>(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(saved.method, HttpMethod::Post);

    // rename_request renames the file AND rewrites the name field.
    let renamed = s.rename_request(&path, "Renamed request").unwrap();
    assert_eq!(renamed.file_name().unwrap(), "Renamed request.yaml");
    assert!(!path.exists());
    let renamed_req: Request =
        serde_yaml::from_str(&fs::read_to_string(&renamed).unwrap()).unwrap();
    assert_eq!(renamed_req.name, "Renamed request");

    // duplicate_request copies file and updates name.
    let (dup_path, dup) = s.duplicate_request(&renamed).unwrap();
    assert_eq!(dup.name, "Renamed request copy");
    assert_eq!(dup_path.file_name().unwrap(), "Renamed request copy.yaml");

    // Second duplicate does not collide.
    let (dup_path2, dup2) = s.duplicate_request(&renamed).unwrap();
    assert_eq!(dup2.name, "Renamed request copy 2");
    assert_eq!(
        dup_path2.file_name().unwrap(),
        "Renamed request copy 2.yaml"
    );

    // delete_request removes the file.
    s.delete_request(&dup_path).unwrap();
    assert!(!dup_path.exists());
}

#[test]
fn request_yaml_is_pretty_and_deterministic() {
    let (_dir, root) = sample_workspace();
    let s = storage();
    let col = s.create_collection(&root, "Api").unwrap();

    let req = json_request(
        "Create user",
        "{\n  \"name\": \"Ada\",\n  \"admin\": true\n}\n",
    );
    let path = s.save_request(&col.path, &req).unwrap();
    let text = fs::read_to_string(&path).unwrap();

    // Multi-line body is a readable YAML block scalar.
    assert!(text.contains("content: |"));
    // Pretty, human-readable layout.
    assert!(text.starts_with("name: Create user\n"));
    assert!(text.contains("method: post"));
    assert!(text.contains("auth: none"));

    // Deterministic: rewriting produces identical bytes.
    let text2 = {
        let path2 = s.save_request(&col.path, &req).unwrap();
        fs::read_to_string(&path2).unwrap()
    };
    assert_eq!(text, text2);
}

#[test]
fn auth_config_round_trips_in_request_yaml() {
    let (_dir, root) = sample_workspace();
    let s = storage();
    let col = s.create_collection(&root, "Api").unwrap();

    let req = Request {
        name: "Bearer auth".to_string(),
        auth: AuthConfig::ApiKey {
            key: "X-Api-Key".to_string(),
            value: "{{api_key}}".to_string(),
            in_location: ApiKeyLocation::Header,
        },
        ..request("Bearer auth", "https://example.com")
    };
    let path = s.save_request(&col.path, &req).unwrap();
    let loaded: Request = serde_yaml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(loaded.auth, req.auth);
}

// ---------------------------------------------------------------------------
// Filename sanitization
// ---------------------------------------------------------------------------

#[test]
fn filenames_sanitize_slashes_but_preserve_spaces_and_casing() {
    let (_dir, root) = sample_workspace();
    let s = storage();

    let col = s.create_collection(&root, "My public collection").unwrap();
    assert_eq!(col.path.file_name().unwrap(), "My public collection");

    let req = request("Delete / user?id=1", "https://example.com");
    let path = s.save_request(&col.path, &req).unwrap();
    assert_eq!(
        path.file_name().unwrap(),
        "Delete _ user_id=1.yaml",
        "forbidden chars become underscores; spaces/casing preserved"
    );
    assert!(path.is_file());

    let loaded: Request = serde_yaml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(loaded.name, "Delete / user?id=1");

    let folder = s.create_folder(&col.path, "Admin / Root").unwrap();
    assert_eq!(folder.path.file_name().unwrap(), "Admin _ Root");
}

#[test]
fn sanitization_rejects_invalid_names() {
    for bad in ["", "   ", ".", ".."] {
        assert!(sanitize_filename(bad).is_err(), "{bad:?} must be rejected");
    }
    // Collection creation with an empty name is rejected.
    let (_dir, root) = sample_workspace();
    assert!(storage().create_collection(&root, "").is_err());
}

// ---------------------------------------------------------------------------
// Environments + Secrets
// ---------------------------------------------------------------------------

#[test]
fn environments_load_as_flat_yaml_and_round_trip() {
    let (_dir, root) = sample_workspace();
    let s = storage();
    let col = s.create_collection(&root, "Api").unwrap();

    let mut variables = BTreeMap::new();
    variables.insert(
        "base_url".to_string(),
        "https://staging.example.com".to_string(),
    );
    variables.insert("api_key".to_string(), "{{api_key}}".to_string());
    let env = Environment {
        name: "Staging".to_string(),
        path: PathBuf::new(),
        variables,
    };
    let path = s.save_environment(&col.path, &env).unwrap();
    assert_eq!(path, col.path.join(ENVIRONMENTS_DIR).join("Staging.yaml"));

    // File on disk is a FLAT mapping: no name/path fields.
    let text = fs::read_to_string(&path).unwrap();
    // BTreeMap serializes keys in sorted order.
    assert_eq!(
        text,
        "api_key: '{{api_key}}'\nbase_url: https://staging.example.com\n"
    );

    let loaded = s.load_environments(&col.path).unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].name, "Staging");
    assert_eq!(
        loaded[0].variables["base_url"],
        "https://staging.example.com"
    );
    assert_eq!(loaded[0].path, path);

    // Missing environments dir -> empty, not an error.
    let other = s.create_collection(&root, "Empty").unwrap();
    assert!(s.load_environments(&other.path).unwrap().is_empty());
}

#[test]
fn secrets_load_save_and_never_leak_into_request_or_environment_files() {
    let (_dir, root) = sample_workspace();
    let s = storage();
    let col = s.create_collection(&root, "Api").unwrap();

    // Missing .secrets.yaml -> empty store, not an error.
    assert!(s.load_secrets(&col.path).unwrap().is_empty());

    let mut secrets = SecretStore::empty();
    secrets
        .shared
        .insert("api_key".to_string(), "super-secret-token".to_string());
    secrets.environments.insert(
        "staging".to_string(),
        [("api_key".to_string(), "staging-token".to_string())].into(),
    );
    let secrets_path = s.save_secrets(&col.path, &secrets).unwrap();
    assert_eq!(secrets_path, col.path.join(SECRETS_FILE));

    let loaded = s.load_secrets(&col.path).unwrap();
    assert_eq!(loaded, secrets);
    assert_eq!(
        secret_values(&loaded),
        vec!["super-secret-token".to_string()]
    );
    assert_eq!(
        loaded.secret_values_for_env(Some("staging")),
        vec![
            "super-secret-token".to_string(),
            "staging-token".to_string()
        ]
    );

    // Save a request and an environment, then prove secrets never appear.
    let req = Request {
        name: "Uses secret".to_string(),
        auth: AuthConfig::Bearer {
            token: "{{api_key}}".to_string(),
        },
        ..request("Uses secret", "https://example.com/{{api_key}}")
    };
    let req_path = s.save_request(&col.path, &req).unwrap();
    let mut env = Environment::new("staging", PathBuf::new());
    env.variables
        .insert("api_key".to_string(), "{{api_key}}".to_string());
    let env_path = s.save_environment(&col.path, &env).unwrap();

    let req_text = fs::read_to_string(&req_path).unwrap();
    let env_text = fs::read_to_string(&env_path).unwrap();
    for file_text in [&req_text, &env_text] {
        assert!(
            !file_text.contains("super-secret-token"),
            "secret leaked into file:\n{file_text}"
        );
        assert!(!file_text.contains("staging-token"));
    }
    // Secrets live only in .secrets.yaml.
    assert!(fs::read_to_string(col.path.join(SECRETS_FILE))
        .unwrap()
        .contains("super-secret-token"));
}

#[test]
fn resolve_variables_precedence_across_store_and_env() {
    let mut env = Environment::new("staging", PathBuf::new());
    env.variables
        .insert("base_url".into(), "https://env.example.com".into());
    env.variables.insert("api_key".into(), "from-env".into());

    let mut secrets = SecretStore::empty();
    secrets
        .shared
        .insert("api_key".into(), "shared-secret".into());
    secrets.environments.insert(
        "staging".into(),
        [("api_key".to_string(), "staging-secret".to_string())].into(),
    );

    let resolved = secrets.resolve_variables(Some(&env));
    assert_eq!(resolved["api_key"], "staging-secret");
    assert_eq!(resolved["base_url"], "https://env.example.com");
}

#[test]
fn load_workspace_skips_reserved_names_and_hidden_entries() {
    let (_dir, root) = sample_workspace();
    let s = storage();

    let col = s.create_collection(&root, "Api").unwrap();
    // Things that must NOT become Folders/Requests/Environments:
    fs::create_dir_all(col.path.join("Sneaky").join(ENVIRONMENTS_DIR)).unwrap();
    fs::create_dir(col.path.join(".hidden-dir")).unwrap();
    fs::write(
        col.path.join(".hidden.yaml"),
        "name: x\nmethod: get\nurl: y\n",
    )
    .unwrap();

    let workspace = s.load_workspace(&root).unwrap();
    let collection = &workspace.collections[0];
    // environments dir is reserved, hidden entries invisible.
    assert!(collection
        .folders
        .iter()
        .all(|f| f.name != ENVIRONMENTS_DIR));
    assert!(collection.folders.iter().all(|f| !f.name.starts_with('.')));
    assert!(collection.requests.is_empty());

    // At workspace root, hidden dot-files/dirs are never Collections.
    fs::create_dir(root.join(".rest-harness")).unwrap();
    fs::create_dir(root.join(".git")).unwrap();
    fs::write(root.join(".rest-harness-history.yaml"), "[]").unwrap();
    let workspace = s.load_workspace(&root).unwrap();
    assert_eq!(workspace.collections.len(), 1);
}

// ---------------------------------------------------------------------------
// History: redaction, truncation, cap
// ---------------------------------------------------------------------------

#[test]
fn history_redacts_secret_values_everywhere() {
    let (_dir, root) = sample_workspace();
    let token = "super-secret-token";
    let entry = HistoryEntry {
        request: RedactedHttpRequest {
            method: HttpMethod::Post,
            url: format!("https://example.com/login?token={token}"),
            headers: vec![rest_harness::domain::HttpHeader {
                name: format!("X-{token}"),
                value: format!("Bearer {token}"),
            }],
            body: format!("{{\"token\": \"{token}\"}}"),
        },
        response: HistoryResponse {
            status_code: 200,
            reason_phrase: "OK".into(),
            elapsed_ms: 5,
            size_bytes: 20,
            headers: vec![rest_harness::domain::HttpHeader {
                name: "x-token".into(),
                value: format!("echo {token}"),
            }],
            body: format!("response with {token} inside"),
            body_truncated: false,
            timestamp: chrono::Utc::now(),
        },
    };

    HistoryStore::append_entry_at(&root, entry, &[token.to_string()]).unwrap();

    let path = root.join(HISTORY_FILE);
    assert!(path.is_file());

    // Raw file must not contain the secret anywhere.
    let raw = fs::read_to_string(&path).unwrap();
    assert!(
        !raw.contains(token),
        "history file leaked the secret:\n{raw}"
    );
    assert!(raw.contains("***"));

    let entries = HistoryStore::load_at(&root).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].request.url.contains("***"));
    assert_eq!(entries[0].request.headers[0].value, "Bearer ***");
    assert!(entries[0].request.body.contains("***"));
    assert!(entries[0].response.body.contains("***"));
    assert_eq!(entries[0].response.headers[0].value, "echo ***");

    // Newest entries are prepended.
    HistoryStore::append_entry_at(
        &root,
        history_entry(HttpMethod::Get, "https://b.example.com", "", "second"),
        &[],
    )
    .unwrap();
    let entries = HistoryStore::load_at(&root).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].request.url, "https://b.example.com");
}

#[test]
fn history_truncates_large_response_bodies_and_sets_flag() {
    let (_dir, root) = sample_workspace();

    let big_body = "x".repeat(MAX_RESPONSE_BODY_BYTES + 100);
    HistoryStore::append_entry_at(
        &root,
        history_entry(HttpMethod::Get, "https://example.com/big", "", &big_body),
        &[],
    )
    .unwrap();

    let entries = HistoryStore::load_at(&root).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].response.body_truncated);
    assert_eq!(entries[0].response.body.len(), MAX_RESPONSE_BODY_BYTES);

    // Truncation respects UTF-8 char boundaries.
    let multibyte_body = "é".repeat(MAX_RESPONSE_BODY_BYTES / 2 + 10);
    HistoryStore::append_entry_at(
        &root,
        history_entry(
            HttpMethod::Get,
            "https://example.com/utf8",
            "",
            &multibyte_body,
        ),
        &[],
    )
    .unwrap();
    let entries = HistoryStore::load_at(&root).unwrap();
    let utf8_entry = entries
        .iter()
        .find(|e| e.request.url.ends_with("/utf8"))
        .unwrap();
    assert!(utf8_entry.response.body_truncated);
    assert!(utf8_entry.response.body.len() <= MAX_RESPONSE_BODY_BYTES);
    std::str::from_utf8(utf8_entry.response.body.as_bytes()).expect("valid UTF-8 after truncation");
}

#[test]
fn history_caps_at_100_most_recent_entries() {
    let (_dir, root) = sample_workspace();

    for i in 0..(MAX_HISTORY_ENTRIES + 25) {
        HistoryStore::append_entry_at(
            &root,
            history_entry(
                HttpMethod::Get,
                &format!("https://example.com/{i}"),
                "",
                "{}",
            ),
            &[],
        )
        .unwrap();
    }

    let entries = HistoryStore::load_at(&root).unwrap();
    assert_eq!(entries.len(), MAX_HISTORY_ENTRIES);
    // Newest first: entry 124 (last appended) is at the front, entry 25 is
    // the oldest kept.
    assert_eq!(entries[0].request.url, "https://example.com/124");
    assert_eq!(
        entries[MAX_HISTORY_ENTRIES - 1].request.url,
        "https://example.com/25"
    );
}

#[test]
fn workspace_includes_history_from_disk() {
    let (_dir, root) = sample_workspace();
    let s = storage();

    HistoryStore::append_entry_at(
        &root,
        history_entry(HttpMethod::Get, "https://example.com", "", "{}"),
        &["secret-value".to_string()],
    )
    .unwrap();

    let workspace = s.load_workspace(&root).unwrap();
    assert_eq!(workspace.history.len(), 1);
    assert_eq!(workspace.history[0].request.url, "https://example.com");
}

#[test]
fn gitignore_helper_appends_missing_entries_without_duplicates() {
    let dir = tempfile::tempdir().unwrap();

    // No .gitignore yet -> all entries appended.
    ensure_gitignore(dir.path()).unwrap();
    let text = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
    assert!(text.contains(SECRETS_FILE));
    assert!(text.contains(HISTORY_FILE));
    assert!(text.contains(".rest-harness/"));

    // Running again must not duplicate anything.
    ensure_gitignore(dir.path()).unwrap();
    let text2 = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
    assert_eq!(text2.matches(SECRETS_FILE).count(), 1);
    assert_eq!(text2.matches(HISTORY_FILE).count(), 1);

    // A `**/`-prefixed existing pattern also counts as covered.
    let dir2 = tempfile::tempdir().unwrap();
    fs::write(
        dir2.path().join(".gitignore"),
        format!("**/{SECRETS_FILE}\n**/{HISTORY_FILE}\n.rest-harness/\n"),
    )
    .unwrap();
    let before = fs::read_to_string(dir2.path().join(".gitignore")).unwrap();
    ensure_gitignore(dir2.path()).unwrap();
    let after = fs::read_to_string(dir2.path().join(".gitignore")).unwrap();
    assert_eq!(before, after, "fully covered .gitignore must be untouched");
}
