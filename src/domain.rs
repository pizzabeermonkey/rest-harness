//! Domain models for REST Harness v1.
//!
//! Naming follows `GLOSSARY.md`: a [`Workspace`] contains [`Collection`]s,
//! a Collection contains [`Folder`]s, [`RequestEntry`]s and [`Environment`]s,
//! and Secret values live in a Collection-local [`SecretStore`] (never inside
//! Request or Environment files — see ADR 0003).
//!
//! Serialization notes:
//! - Requests are stored one-per-`.yaml`-file in a human-friendly layout with
//!   multi-line bodies rendered as YAML block scalars (`|`), which
//!   `serde_yaml` produces naturally (ADR 0002).
//! - Environment files are *flat* YAML mappings (`variable: value`); the
//!   Environment's `name` comes from the filename and its `path` from the
//!   directory, so neither field is serialized into the file itself.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Default value function for [`KeyValueRow::enabled`].
fn default_enabled() -> bool {
    true
}

/// HTTP methods supported by REST Harness.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HttpMethod {
    /// GET request.
    #[default]
    Get,
    /// POST request.
    Post,
    /// PUT request.
    Put,
    /// PATCH request.
    Patch,
    /// DELETE request.
    Delete,
    /// HEAD request.
    Head,
    /// OPTIONS request.
    Options,
}

impl HttpMethod {
    /// Uppercase label of the method (e.g. `GET`), used by badges and
    /// pick lists.
    pub fn label(self) -> &'static str {
        match self {
            HttpMethod::Get => "GET",
            HttpMethod::Post => "POST",
            HttpMethod::Put => "PUT",
            HttpMethod::Patch => "PATCH",
            HttpMethod::Delete => "DELETE",
            HttpMethod::Head => "HEAD",
            HttpMethod::Options => "OPTIONS",
        }
    }
}

impl std::fmt::Display for HttpMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// One editable key/value row (query parameter, header, ...).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct KeyValueRow {
    /// Key name (e.g. header name or query parameter name).
    #[serde(default)]
    pub key: String,
    /// Value.
    #[serde(default)]
    pub value: String,
    /// Optional human description shown in the UI.
    #[serde(default)]
    pub description: String,
    /// Whether the row is active; defaults to `true` when omitted.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

impl KeyValueRow {
    /// Convenience constructor for enabled rows.
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        KeyValueRow {
            key: key.into(),
            value: value.into(),
            description: String::new(),
            enabled: true,
        }
    }
}

/// Where an API-key auth value is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApiKeyLocation {
    /// Sent as a URL query parameter.
    Query,
    /// Sent as a request header.
    Header,
    /// Sent as a cookie.
    Cookie,
}

/// Authentication configuration of a Request.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthConfig {
    /// No authentication.
    #[default]
    None,
    /// Bearer token, sent as `Authorization: Bearer <token>`.
    Bearer {
        /// The bearer token.
        token: String,
    },
    /// HTTP Basic auth credentials.
    Basic {
        /// Username.
        username: String,
        /// Password.
        password: String,
    },
    /// Static API key sent in a configurable location.
    ApiKey {
        /// The key *name* (header name, query param name, cookie name).
        key: String,
        /// The key *value* (may contain `{{variable}}` placeholders).
        value: String,
        /// Where the key/value pair is sent.
        in_location: ApiKeyLocation,
    },
}

/// Content type of a raw request body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RawContentType {
    /// `application/json`.
    Json,
    /// `text/plain`.
    Text,
}

/// The body of a Request.
///
/// Multi-line `content` strings serialize as readable YAML block scalars
/// (`|`) because that is what `serde_yaml` emits for multi-line strings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RequestBody {
    /// No body (GET/HEAD/...).
    #[default]
    None,
    /// A raw text body with an explicit content type.
    Raw {
        /// Declared content type.
        content_type: RawContentType,
        /// The body text itself.
        content: String,
    },
}

/// A saved Request — the full model, deserialized from a request `.yaml` file.
///
/// Most fields have defaults so slightly malformed files still load, but
/// `url` is required: a file without a URL is not a Request. This lets the
/// storage layer skip foreign `.yaml` files gracefully (they fail to parse
/// and are ignored, never deleted).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Request {
    /// Display name; also determines the filename via sanitization.
    #[serde(default)]
    pub name: String,
    /// HTTP method.
    #[serde(default)]
    pub method: HttpMethod,
    /// Target URL (may contain `{{variable}}` placeholders). Required: a
    /// YAML file without a `url` is not treated as a Request.
    pub url: String,
    /// Query parameters.
    #[serde(default)]
    pub params: Vec<KeyValueRow>,
    /// Request headers.
    #[serde(default)]
    pub headers: Vec<KeyValueRow>,
    /// Authentication configuration; omitted (`none`) for simple requests.
    #[serde(default)]
    pub auth: AuthConfig,
    /// Request body.
    #[serde(default)]
    pub body: RequestBody,
}

/// A named set of Collection-scoped variables (see GLOSSARY: Environment).
///
/// On disk this is a FLAT YAML mapping `variable_name: value`; `name` lives
/// in the filename and `path` in the directory, so neither is serialized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Environment {
    /// Display name (also the file name of `environments/<name>.yaml`).
    pub name: String,
    /// Absolute path of the `.yaml` file backing this Environment.
    pub path: PathBuf,
    /// Ordered variable map.
    pub variables: BTreeMap<String, String>,
}

impl Environment {
    /// Create an in-memory Environment with no variables.
    pub fn new(name: impl Into<String>, path: PathBuf) -> Self {
        Environment {
            name: name.into(),
            path,
            variables: BTreeMap::new(),
        }
    }
}

impl Serialize for Environment {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.variables.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Environment {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let variables = BTreeMap::<String, String>::deserialize(deserializer)?;
        Ok(Environment {
            name: String::new(),
            path: PathBuf::new(),
            variables,
        })
    }
}

/// Secret storage for one Collection, loaded from `<Collection>/.secrets.yaml`.
///
/// The file supports two shapes for shared Collection Secrets plus
/// per-Environment overrides:
///
/// ```yaml
/// api_key: abc123            # top-level flat keys = shared collection secrets
/// secrets:                   # equivalent shape for shared secrets
///   another_key: hunter2
/// environments:              # per-Environment overrides
///   staging:
///     api_key: stage-only-token
/// ```
///
/// Secret values live only here — they are never serialized into Request or
/// Environment YAML files (ADR 0003).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SecretStore {
    /// Shared Collection secrets, resolved for every Environment.
    pub shared: BTreeMap<String, String>,
    /// Per-Environment overrides keyed by Environment name.
    pub environments: BTreeMap<String, BTreeMap<String, String>>,
}

/// Keys with reserved meaning at the top level of a `.secrets.yaml` file.
const SECRET_RESERVED_KEYS: [&str; 2] = ["secrets", "environments"];

impl SecretStore {
    /// An empty store.
    pub fn empty() -> Self {
        SecretStore::default()
    }

    /// Whether the store holds no secrets at all.
    pub fn is_empty(&self) -> bool {
        self.shared.is_empty() && self.environments.is_empty()
    }

    /// Effective variables for an Environment: Environment variables,
    /// overridden by shared Secrets, overridden by Environment-specific
    /// Secret overrides.
    pub fn resolve_variables(&self, env: Option<&Environment>) -> BTreeMap<String, String> {
        let mut resolved = match env {
            Some(env) => env.variables.clone(),
            None => BTreeMap::new(),
        };
        for (key, value) in &self.shared {
            resolved.insert(key.clone(), value.clone());
        }
        if let Some(env) = env {
            if let Some(overrides) = self.environments.get(&env.name) {
                for (key, value) in overrides {
                    resolved.insert(key.clone(), value.clone());
                }
            }
        }
        resolved
    }

    /// Non-empty Secret values used for redaction: shared values plus, when
    /// an Environment name is given, that Environment's overrides.
    pub fn secret_values_for_env(&self, env_name: Option<&str>) -> Vec<String> {
        let mut values: Vec<String> = self
            .shared
            .values()
            .filter(|v| !v.is_empty())
            .cloned()
            .collect();
        if let Some(env_name) = env_name {
            if let Some(overrides) = self.environments.get(env_name) {
                values.extend(overrides.values().filter(|v| !v.is_empty()).cloned());
            }
        }
        values
    }
}

impl<'de> Deserialize<'de> for SecretStore {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct SecretStoreVisitor;

        impl<'de> serde::de::Visitor<'de> for SecretStoreVisitor {
            type Value = SecretStore;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a mapping of secret names to values")
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(SecretStore::empty())
            }

            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut map: M,
            ) -> Result<Self::Value, M::Error> {
                let mut store = SecretStore::empty();
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "secrets" => {
                            let entries: BTreeMap<String, String> = map.next_value()?;
                            store.shared.extend(entries);
                        }
                        "environments" => {
                            let entries: BTreeMap<String, BTreeMap<String, String>> =
                                map.next_value()?;
                            store.environments = entries;
                        }
                        _ => {
                            let value: String = map.next_value()?;
                            store.shared.insert(key, value);
                        }
                    }
                }
                Ok(store)
            }
        }

        deserializer.deserialize_map(SecretStoreVisitor)
    }
}

impl Serialize for SecretStore {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;

        let mut map = serializer.serialize_map(None)?;
        // Shared secrets are written as top-level flat keys; any shared key
        // that would collide with a reserved key is parked under `secrets:`
        // so the file round-trips without clobbering its own structure.
        let mut reserved = BTreeMap::new();
        for (key, value) in &self.shared {
            if SECRET_RESERVED_KEYS.contains(&key.as_str()) {
                reserved.insert(key.clone(), value.clone());
            } else {
                map.serialize_entry(key, value)?;
            }
        }
        if !reserved.is_empty() {
            map.serialize_entry("secrets", &reserved)?;
        }
        if !self.environments.is_empty() {
            map.serialize_entry("environments", &self.environments)?;
        }
        map.end()
    }
}

/// One request/response header in a History Entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpHeader {
    /// Header name.
    pub name: String,
    /// Header value (already redacted in stored entries).
    pub value: String,
}

/// The request side of a History Entry, with Secret values already redacted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedactedHttpRequest {
    /// HTTP method used.
    pub method: HttpMethod,
    /// Target URL (Secret values replaced with `***`).
    pub url: String,
    /// Headers sent (Secret values replaced with `***`).
    pub headers: Vec<HttpHeader>,
    /// Body sent (Secret values replaced with `***`).
    pub body: String,
}

/// The response side of a History Entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryResponse {
    /// HTTP status code.
    pub status_code: u16,
    /// HTTP reason phrase (e.g. `OK`).
    pub reason_phrase: String,
    /// Wall-clock duration of the call in milliseconds.
    pub elapsed_ms: u64,
    /// Response size in bytes.
    pub size_bytes: u64,
    /// Response headers.
    pub headers: Vec<HttpHeader>,
    /// Response body (possibly truncated, Secret values replaced with `***`).
    pub body: String,
    /// Whether `body` was truncated to fit the 1 MB History limit.
    pub body_truncated: bool,
    /// When the response was received.
    pub timestamp: DateTime<Utc>,
}

/// An immutable record of one executed request/response exchange
/// (see GLOSSARY: History Entry).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// The executed request, with Secrets redacted.
    pub request: RedactedHttpRequest,
    /// The received response.
    pub response: HistoryResponse,
}

/// A Request as listed in a Collection/Folder tree: its display name and the
/// `.yaml` path it is loaded from on demand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestEntry {
    /// Display name from the request file's `name` field.
    pub name: String,
    /// Path of the `.yaml` file backing this Request.
    pub path: PathBuf,
}

/// A named subdirectory grouping related Requests hierarchically.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Folder {
    /// Display name (directory name, exact casing preserved).
    pub name: String,
    /// Absolute path of the Folder directory.
    pub path: PathBuf,
    /// Nested Folders.
    pub folders: Vec<Folder>,
    /// Requests directly inside this Folder.
    pub requests: Vec<RequestEntry>,
}

impl Folder {
    /// Number of Requests in this Folder and all nested Folders.
    pub fn total_request_count(&self) -> usize {
        self.requests.len()
            + self
                .folders
                .iter()
                .map(Folder::total_request_count)
                .sum::<usize>()
    }
}

/// A top-level group of saved Requests, Folders and Environments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collection {
    /// Display name (directory name, exact casing preserved).
    pub name: String,
    /// Absolute path of the Collection directory.
    pub path: PathBuf,
    /// Top-level Folders inside this Collection.
    pub folders: Vec<Folder>,
    /// Requests directly inside this Collection.
    pub requests: Vec<RequestEntry>,
    /// Environments scoped to this Collection.
    pub environments: Vec<Environment>,
    /// Collection-local Secret store (`.secrets.yaml`).
    pub secrets: SecretStore,
}

impl Collection {
    /// Number of Requests in this Collection: direct Requests plus every
    /// Request inside any nested Folder.
    pub fn total_request_count(&self) -> usize {
        self.requests.len()
            + self
                .folders
                .iter()
                .map(Folder::total_request_count)
                .sum::<usize>()
    }
}

/// The Workspace: a root directory containing Collections plus the local
/// (gitignored) History Entries.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Workspace {
    /// Absolute path of the Workspace root directory.
    pub root: PathBuf,
    /// Collections found in the Workspace root.
    pub collections: Vec<Collection>,
    /// Most recent History Entries (newest first).
    pub history: Vec<HistoryEntry>,
}

/// Pretty-print a JSON string, returning the input unchanged when it is not
/// valid JSON. Provided so GUI/engine tasks can pretty-print request and
/// response bodies without depending on `serde_json` directly.
///
/// Object keys are always emitted in sorted order: the `gpui` dependency
/// tree enables serde_json's `preserve_order` feature (insertion-ordered
/// maps), so the output is canonicalized explicitly to keep it
/// deterministic and independent of the dependency graph.
pub fn pretty_json(input: &str) -> String {
    serde_json::from_str::<serde_json::Value>(input)
        .map(canonicalize_json)
        .and_then(|value| serde_json::to_string_pretty(&value))
        .unwrap_or_else(|_| input.to_string())
}

/// Rebuild `value` with object keys sorted (recursively); arrays keep
/// their order. See [`pretty_json`].
fn canonicalize_json(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut entries: Vec<(String, serde_json::Value)> = map.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            let mut sorted = serde_json::Map::new();
            for (key, value) in entries {
                sorted.insert(key, canonicalize_json(value));
            }
            serde_json::Value::Object(sorted)
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(canonicalize_json).collect())
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_value_row_defaults_enabled_to_true() {
        let row: KeyValueRow =
            serde_yaml::from_str("key: id\nvalue: '1'").expect("parse without enabled flag");
        assert!(row.enabled);
        assert_eq!(row.key, "id");

        let row: KeyValueRow =
            serde_yaml::from_str("key: id\nvalue: '1'\nenabled: false").expect("parse");
        assert!(!row.enabled);
    }

    #[test]
    fn method_serializes_lowercase() {
        assert_eq!(
            serde_yaml::to_string(&HttpMethod::Patch).unwrap().trim(),
            "patch"
        );
        assert_eq!(
            serde_yaml::to_string(&AuthConfig::None).unwrap().trim(),
            "none"
        );
    }

    #[test]
    fn multi_line_body_uses_block_scalar() {
        let request = Request {
            name: "post json".into(),
            method: HttpMethod::Post,
            url: "https://example.com".into(),
            body: RequestBody::Raw {
                content_type: RawContentType::Json,
                content: "{\n  \"a\": 1\n}\n".into(),
            },
            ..Request::default()
        };
        let yaml = serde_yaml::to_string(&request).unwrap();
        assert!(
            yaml.contains("content: |"),
            "expected block scalar in:\n{yaml}"
        );
        let back: Request = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(back, request);
    }

    #[test]
    fn request_yaml_round_trip_is_clean_for_simple_requests() {
        let request = Request {
            name: "Sample request".into(),
            method: HttpMethod::Get,
            url: "https://postman-echo.com/get?id=1".into(),
            params: vec![KeyValueRow::new("id", "1")],
            ..Request::default()
        };
        let yaml = serde_yaml::to_string(&request).unwrap();
        let back: Request = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(back, request);
    }

    #[test]
    fn environment_serializes_as_flat_mapping() {
        let mut env = Environment::new("Staging", PathBuf::from("/tmp/Staging.yaml"));
        env.variables
            .insert("base_url".into(), "https://staging.example.com".into());
        let yaml = serde_yaml::to_string(&env).unwrap();
        assert!(
            !yaml.contains("name:") && !yaml.contains("path:"),
            "name/path must not leak into the flat mapping:\n{yaml}"
        );
        let back: Environment = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(back.variables, env.variables);
    }

    #[test]
    fn secret_store_supports_both_shared_shapes_and_env_overrides() {
        let yaml = "api_key: abc123\nsecrets:\n  db_password: hunter2\nenvironments:\n  staging:\n    api_key: stage-token\n";
        let store: SecretStore = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(store.shared.get("api_key").unwrap(), "abc123");
        assert_eq!(store.shared.get("db_password").unwrap(), "hunter2");
        assert_eq!(store.environments["staging"]["api_key"], "stage-token");

        let round: SecretStore =
            serde_yaml::from_str(&serde_yaml::to_string(&store).unwrap()).unwrap();
        assert_eq!(round, store);
    }

    #[test]
    fn secret_store_round_trips_reserved_shared_key_names() {
        let mut store = SecretStore::empty();
        store.shared.insert("secrets".into(), "tricky".into());
        let yaml = serde_yaml::to_string(&store).unwrap();
        let back: SecretStore = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(back.shared.get("secrets").unwrap(), "tricky");
    }

    #[test]
    fn resolve_variables_precedence() {
        let mut env = Environment::new("staging", PathBuf::new());
        env.variables.insert("api_key".into(), "from-env".into());
        env.variables.insert("only_env".into(), "env-value".into());

        let mut store = SecretStore::empty();
        store.shared.insert("api_key".into(), "shared".into());
        store.environments.insert(
            "staging".into(),
            [("api_key".to_string(), "override".to_string())].into(),
        );

        let resolved = store.resolve_variables(Some(&env));
        assert_eq!(resolved["api_key"], "override");
        assert_eq!(resolved["only_env"], "env-value");

        let resolved_no_env = store.resolve_variables(None);
        assert_eq!(resolved_no_env["api_key"], "shared");
    }

    #[test]
    fn secret_values_for_env_filters_empty_and_scopes_overrides() {
        let mut store = SecretStore::empty();
        store.shared.insert("a".into(), "v1".into());
        store.shared.insert("empty".into(), String::new());
        store.environments.insert(
            "staging".into(),
            [("b".to_string(), "v2".to_string())].into(),
        );
        store
            .environments
            .insert("prod".into(), [("c".to_string(), "v3".to_string())].into());

        let staging = store.secret_values_for_env(Some("staging"));
        assert_eq!(staging.len(), 2);
        assert!(staging.contains(&"v1".to_string()));
        assert!(staging.contains(&"v2".to_string()));

        let shared_only = store.secret_values_for_env(None);
        assert_eq!(shared_only, vec!["v1".to_string()]);
    }

    #[test]
    fn request_counts_are_recursive() {
        let deep = Folder {
            name: "deep".into(),
            path: PathBuf::new(),
            folders: vec![],
            requests: vec![
                RequestEntry {
                    name: "a".into(),
                    path: PathBuf::new(),
                },
                RequestEntry {
                    name: "b".into(),
                    path: PathBuf::new(),
                },
            ],
        };
        let mid = Folder {
            name: "mid".into(),
            path: PathBuf::new(),
            folders: vec![deep],
            requests: vec![RequestEntry {
                name: "c".into(),
                path: PathBuf::new(),
            }],
        };
        let collection = Collection {
            name: "col".into(),
            path: PathBuf::new(),
            folders: vec![mid.clone()],
            requests: vec![RequestEntry {
                name: "root".into(),
                path: PathBuf::new(),
            }],
            environments: vec![],
            secrets: SecretStore::empty(),
        };
        assert_eq!(mid.total_request_count(), 3);
        assert_eq!(collection.total_request_count(), 4);
    }

    #[test]
    fn pretty_json_formats_valid_json_and_passes_through_other_text() {
        assert_eq!(pretty_json("{\"a\":1}"), "{\n  \"a\": 1\n}");
        assert_eq!(pretty_json("not json"), "not json");
    }

    #[test]
    fn history_entry_round_trips_with_timestamp() {
        let entry = HistoryEntry {
            request: RedactedHttpRequest {
                method: HttpMethod::Get,
                url: "https://example.com".into(),
                headers: vec![HttpHeader {
                    name: "authorization".into(),
                    value: "***".into(),
                }],
                body: String::new(),
            },
            response: HistoryResponse {
                status_code: 200,
                reason_phrase: "OK".into(),
                elapsed_ms: 42,
                size_bytes: 128,
                headers: vec![],
                body: "{}".into(),
                body_truncated: false,
                timestamp: DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap(),
            },
        };
        let yaml = serde_yaml::to_string(&entry).unwrap();
        let back: HistoryEntry = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(back, entry);
    }
}
