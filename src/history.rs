//! History Entry persistence with Secret redaction (see ADR 0003).
//!
//! History Entries are stored newest-first in the gitignored
//! `<workspace root>/.rest-harness-history.yaml`. Before any entry reaches
//! disk:
//!
//! 1. Every non-empty Secret value (from the Collection's
//!    [`SecretStore::secret_values_for_env`]) is replaced with `***` in the
//!    URL, request header names/values, request body, and response
//!    header names/values and body (redaction is a plain substring
//!    replacement at the string level).
//! 2. Response bodies larger than [`MAX_RESPONSE_BODY_BYTES`] (1 MB) are
//!    truncated at a valid UTF-8 char boundary and `body_truncated` is set.
//! 3. The store is capped at the [`MAX_HISTORY_ENTRIES`] (100) most recent
//!    entries.

use crate::domain::{HistoryEntry, HttpHeader};
use crate::error::{io_error, Result};
use chrono::Utc;
use std::path::{Path, PathBuf};

/// Maximum number of History Entries kept on disk (most recent).
pub const MAX_HISTORY_ENTRIES: usize = 100;

/// Maximum stored response body size in bytes (1 MB).
pub const MAX_RESPONSE_BODY_BYTES: usize = 1_048_576;

/// Replacement marker written in place of Secret values.
pub const REDACTION: &str = "***";

/// Loads, saves and appends History Entries for a Workspace root.
#[derive(Debug, Clone)]
pub struct HistoryStore {
    root: PathBuf,
}

impl HistoryStore {
    /// A store rooted at `root` (reads/writes `<root>/.rest-harness-history.yaml`).
    pub fn new(root: &Path) -> Self {
        HistoryStore {
            root: root.to_path_buf(),
        }
    }

    /// The Workspace root this store is bound to.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Path of the backing `.rest-harness-history.yaml` file for `root`.
    pub fn history_path(root: &Path) -> PathBuf {
        root.join(crate::storage::HISTORY_FILE)
    }

    /// Load all History Entries bound to this store (newest first); a
    /// missing file yields an empty list.
    pub fn load(&self) -> Result<Vec<HistoryEntry>> {
        Self::load_at(&self.root)
    }

    /// Persist `entries` (must already be redacted) for this store.
    pub fn save(&self, entries: &[HistoryEntry]) -> Result<()> {
        Self::save_at(&self.root, entries)
    }

    /// Load, redact `entry`, prepend it, cap the list, and write back,
    /// for this store.
    pub fn append_entry(&self, entry: HistoryEntry, secret_values: &[String]) -> Result<()> {
        Self::append_entry_at(&self.root, entry, secret_values)
    }

    /// Load all History Entries from `<root>/.rest-harness-history.yaml`
    /// (newest first). A missing file yields an empty list.
    pub fn load_at(root: &Path) -> Result<Vec<HistoryEntry>> {
        let path = Self::history_path(root);
        if !path.is_file() {
            return Ok(Vec::new());
        }
        let text = std::fs::read_to_string(&path).map_err(|e| io_error(&path, e))?;
        let entries: Vec<HistoryEntry> = serde_yaml::from_str(&text)?;
        Ok(entries)
    }

    /// Persist `entries` (must already be redacted) to
    /// `<root>/.rest-harness-history.yaml`.
    pub fn save_at(root: &Path, entries: &[HistoryEntry]) -> Result<()> {
        let path = Self::history_path(root);
        let text = serde_yaml::to_string(&entries)?;
        std::fs::write(&path, text).map_err(|e| io_error(&path, e))?;
        Ok(())
    }

    /// Load, redact `entry` against `secret_values`, prepend it, cap the
    /// list at [`MAX_HISTORY_ENTRIES`], and write back to disk.
    pub fn append_entry_at(
        root: &Path,
        entry: HistoryEntry,
        secret_values: &[String],
    ) -> Result<()> {
        let mut entry = entry;
        redact_history_entry(&mut entry, secret_values);
        truncate_response_body(&mut entry);

        let mut entries = Self::load_at(root)?;
        entries.insert(0, entry);
        entries.truncate(MAX_HISTORY_ENTRIES);
        Self::save_at(root, &entries)
    }
}

/// Replace every occurrence of each non-empty Secret value with `***` in
/// the URL, request headers (names and values), request body, and response
/// headers (names and values) and body.
pub fn redact_history_entry(entry: &mut HistoryEntry, secret_values: &[String]) {
    for secret in secret_values.iter().filter(|s| !s.is_empty()) {
        entry.request.url = entry.request.url.replace(secret, REDACTION);
        entry.request.body = entry.request.body.replace(secret, REDACTION);
        redact_headers(&mut entry.request.headers, secret);

        entry.response.body = entry.response.body.replace(secret, REDACTION);
        redact_headers(&mut entry.response.headers, secret);
    }
}

fn redact_headers(headers: &mut [HttpHeader], secret: &str) {
    for header in headers.iter_mut() {
        header.name = header.name.replace(secret, REDACTION);
        header.value = header.value.replace(secret, REDACTION);
    }
}

/// Truncate the response body to [`MAX_RESPONSE_BODY_BYTES`] bytes at a
/// valid UTF-8 char boundary, setting `body_truncated` when truncation
/// happened.
pub fn truncate_response_body(entry: &mut HistoryEntry) {
    let body = &entry.response.body;
    if body.len() <= MAX_RESPONSE_BODY_BYTES {
        return;
    }
    let mut end = MAX_RESPONSE_BODY_BYTES;
    while end > 0 && !body.is_char_boundary(end) {
        end -= 1;
    }
    entry.response.body = body[..end].to_string();
    entry.response.body_truncated = true;
}

/// Convenience builder for a History Entry stamped with the current UTC
/// time; Secrets are NOT redacted here — redaction happens on write
/// ([`HistoryStore::append_entry`]).
pub fn build_entry(
    request: crate::domain::RedactedHttpRequest,
    status_code: u16,
    reason_phrase: String,
    elapsed_ms: u64,
    size_bytes: u64,
    response_headers: Vec<HttpHeader>,
    response_body: String,
) -> HistoryEntry {
    HistoryEntry {
        request,
        response: crate::domain::HistoryResponse {
            status_code,
            reason_phrase,
            elapsed_ms,
            size_bytes,
            headers: response_headers,
            body: response_body,
            body_truncated: false,
            timestamp: Utc::now(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{HistoryResponse, HttpMethod, RedactedHttpRequest};

    fn entry(url: &str, body: &str) -> HistoryEntry {
        HistoryEntry {
            request: RedactedHttpRequest {
                method: HttpMethod::Post,
                url: url.to_string(),
                headers: Vec::new(),
                body: String::new(),
            },
            response: HistoryResponse {
                status_code: 200,
                reason_phrase: "OK".into(),
                elapsed_ms: 1,
                size_bytes: body.len() as u64,
                headers: Vec::new(),
                body: body.to_string(),
                body_truncated: false,
                timestamp: Utc::now(),
            },
        }
    }

    #[test]
    fn truncates_only_over_limit_bodies_at_char_boundary() {
        let big = "x".repeat(MAX_RESPONSE_BODY_BYTES + 10);
        let mut e = entry("https://example.com", &big);
        truncate_response_body(&mut e);
        assert!(e.response.body_truncated);
        assert_eq!(e.response.body.len(), MAX_RESPONSE_BODY_BYTES);

        // Multi-byte character split across the limit must stay valid UTF-8.
        let multibyte = "é".repeat(MAX_RESPONSE_BODY_BYTES / 2 + 10); // 2 bytes each
        let mut e = entry("https://example.com", &multibyte);
        truncate_response_body(&mut e);
        assert!(e.response.body_truncated);
        assert!(e.response.body.len() <= MAX_RESPONSE_BODY_BYTES);
        std::str::from_utf8(e.response.body.as_bytes()).expect("still valid UTF-8");

        // Under-limit bodies are untouched.
        let mut e = entry("https://example.com", "small");
        truncate_response_body(&mut e);
        assert!(!e.response.body_truncated);
        assert_eq!(e.response.body, "small");
    }
}
