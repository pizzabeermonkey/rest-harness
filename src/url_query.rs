//! Bidirectional URL ↔ query-params table synchronization.
//!
//! The query component is everything after the FIRST `?` in the URL string,
//! up to a `#` fragment when present. Everything before that `?` (scheme,
//! host, path — which may contain `{{variable}}` placeholders) is passed
//! through VERBATIM by both directions: parsing is plain string splitting,
//! NOT the `url` crate, so placeholder tokens in the host/path survive
//! untouched and are never percent-encoded or re-serialized.
//!
//! Encoding rules:
//! - Decoding ([`sync_url_to_params`]) handles `%XX` escapes only. A literal
//!   `+` is kept as a plus sign — `+`-as-space is form-encoding behaviour,
//!   not query-component behaviour. Invalid or truncated escapes stay
//!   literal.
//! - Encoding ([`sync_params_to_url`]) percent-encodes every byte except
//!   unreserved ASCII (`A-Z a-z 0-9 - . _ ~`) and the `{`/`}` braces (so
//!   `{{variable}}` placeholder tokens survive), using uppercase `%XX`.
//! - An empty value serializes as a bare `key` (no `=`), matching how the
//!   parse side reads bare keys back into empty values.
//!
//! Product decisions encoded here:
//! - The ENABLED rows are exactly the URL's pairs, in URL order. Enabled rows
//!   are fully owned by the URL: if the URL's query drops them, they are
//!   cleared. When a URL-derived row's key matches an existing enabled row,
//!   that row's `description` is carried over; new keys start empty.
//! - Rows with `enabled: false` are always preserved (same key, value,
//!   description, still disabled) and appended AFTER the URL-derived rows —
//!   including when the URL has no query component at all.
//! - Rows with empty keys are UI noise and are dropped from the rebuilt URL.
//! - [`sync_params_to_url`] drops any `#fragment`: the params table owns the
//!   query, and v1 never reconstructs fragments.

use crate::domain::KeyValueRow;

/// Parse the query component of `url` into enabled [`KeyValueRow`] rows,
/// preserving disabled rows and descriptions from `existing`.
///
/// Enabled rows come from the URL's pairs in URL order (pairs without `=`
/// get an empty value); disabled rows from `existing` are appended after,
/// unchanged. Empty query pairs (`&&`, a trailing `?`) yield no rows.
pub fn sync_url_to_params(url: &str, existing: &[KeyValueRow]) -> Vec<KeyValueRow> {
    let mut rows = Vec::new();

    if let Some(query) = split_url(url).1 {
        for pair in query.split('&') {
            if pair.is_empty() {
                continue;
            }
            let (raw_key, raw_value) = match pair.split_once('=') {
                Some((key, value)) => (key, value),
                None => (pair, ""),
            };
            let key = percent_decode(raw_key);
            // Descriptions live in the params table, not the URL: carry one
            // over from a matching existing enabled row when possible.
            let description = existing
                .iter()
                .find(|row| row.enabled && row.key == key)
                .map(|row| row.description.clone())
                .unwrap_or_default();
            rows.push(KeyValueRow {
                key,
                value: percent_decode(raw_value),
                description,
                enabled: true,
            });
        }
    }

    rows.extend(existing.iter().filter(|row| !row.enabled).cloned());
    rows
}

/// Rebuild `url` with its query string replaced by the enabled rows.
///
/// The prefix (everything before the first `?`, or before a `#` when there
/// is no query) is kept verbatim; any fragment is dropped. The query is
/// rebuilt from the enabled rows in order, skipping rows with empty keys.
/// When no enabled row serializes, the prefix is returned unchanged (no
/// dangling `?`).
pub fn sync_params_to_url(url: &str, params: &[KeyValueRow]) -> String {
    let prefix = url_prefix(url);
    let mut parts = Vec::new();
    for row in params
        .iter()
        .filter(|row| row.enabled && !row.key.is_empty())
    {
        if row.value.is_empty() {
            parts.push(percent_encode(&row.key));
        } else {
            parts.push(format!(
                "{}={}",
                percent_encode(&row.key),
                percent_encode(&row.value)
            ));
        }
    }
    if parts.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}?{}", parts.join("&"))
    }
}

/// Split `url` into its verbatim prefix and its query component
/// (everything after the first `?`, up to a `#` fragment when present).
/// The query is `None` when the URL contains no `?`.
fn split_url(url: &str) -> (&str, Option<&str>) {
    match url.find('?') {
        None => (url, None),
        Some(question) => {
            let rest = &url[question + 1..];
            let query = match rest.find('#') {
                Some(fragment) => &rest[..fragment],
                None => rest,
            };
            (&url[..question], Some(query))
        }
    }
}

/// The part of `url` before the first `?` or `#` (whichever comes first).
fn url_prefix(url: &str) -> &str {
    let cut = url.find(['?', '#']).unwrap_or(url.len());
    &url[..cut]
}

/// Percent-decode `%XX` escapes; a literal `+` is NOT converted to a space
/// (query-component style, not form-encoding). Invalid hex or a truncated
/// `%` at the end of the string stays literal.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 3 <= bytes.len() {
            if let (Some(high), Some(low)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push(high * 16 + low);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Hex digit value of an ASCII byte.
fn hex_val(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Uppercase hex digits used by [`percent_encode`].
const HEX: [u8; 16] = *b"0123456789ABCDEF";

/// Percent-encode every byte except unreserved ASCII (`A-Z a-z 0-9 - . _ ~`)
/// and the `{`/`}` braces (so `{{variable}}` placeholder tokens survive),
/// using uppercase `%XX`.
fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for &byte in input.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'{' | b'}' => {
                out.push(byte as char);
            }
            _ => {
                out.push('%');
                out.push(HEX[(byte >> 4) as usize] as char);
                out.push(HEX[(byte & 0x0F) as usize] as char);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An enabled row.
    fn row(key: &str, value: &str) -> KeyValueRow {
        KeyValueRow::new(key, value)
    }

    /// A disabled row with a description.
    fn disabled(key: &str, value: &str, description: &str) -> KeyValueRow {
        KeyValueRow {
            key: key.into(),
            value: value.into(),
            description: description.into(),
            enabled: false,
        }
    }

    #[test]
    fn round_trips_simple_query_into_two_enabled_rows() {
        let url = "https://api.example.com/v1/users?id=1&limit=20";
        let rows = sync_url_to_params(url, &[]);
        assert_eq!(rows, vec![row("id", "1"), row("limit", "20")]);
        assert!(rows.iter().all(|r| r.enabled));
        assert_eq!(sync_params_to_url(url, &rows), url);
    }

    #[test]
    fn url_edit_preserves_disabled_row_key_value_description() {
        let url = "https://api.example.com/v1/users?limit=20";
        let existing = vec![
            disabled("verbose", "true", "debug output"),
            row("limit", "20"),
        ];
        let rows = sync_url_to_params(url, &existing);
        assert_eq!(
            rows,
            vec![
                row("limit", "20"),
                disabled("verbose", "true", "debug output")
            ]
        );
    }

    #[test]
    fn description_carried_over_on_key_match_new_keys_start_empty() {
        let url = "https://api.example.com/?id=2&new=1";
        let existing = vec![KeyValueRow {
            key: "id".into(),
            value: "1".into(),
            description: "user id".into(),
            enabled: true,
        }];
        let rows = sync_url_to_params(url, &existing);
        assert_eq!(rows[0].description, "user id");
        assert_eq!(rows[1].description, "");
        assert!(rows.iter().all(|r| r.enabled));
    }

    #[test]
    fn decodes_percent_escapes_but_keeps_plus_literal() {
        let rows = sync_url_to_params("https://x/?q=a%20b%26c%3Dd%25e", &[]);
        assert_eq!(rows[0].value, "a b&c=d%e");
        // `+` stays a literal plus (query-component style, documented above).
        let rows = sync_url_to_params("https://x/?q=a+b", &[]);
        assert_eq!(rows[0].value, "a+b");
        // Invalid escapes stay literal.
        let rows = sync_url_to_params("https://x/?q=%zz%2", &[]);
        assert_eq!(rows[0].value, "%zz%2");
    }

    #[test]
    fn encodes_specials_and_unicode_with_uppercase_hex() {
        let url = sync_params_to_url("https://x/", &[row("q", "a b&c=d%é")]);
        assert_eq!(url, "https://x/?q=a%20b%26c%3Dd%25%C3%A9");
        let rows = sync_url_to_params(&url, &[]);
        assert_eq!(rows[0].value, "a b&c=d%é");
    }

    #[test]
    fn empty_value_round_trips_as_bare_key() {
        let url = "https://x/?flag&other=1";
        let rows = sync_url_to_params(url, &[]);
        assert_eq!(rows, vec![row("flag", ""), row("other", "1")]);
        assert_eq!(sync_params_to_url(url, &rows), url);
    }

    #[test]
    fn braces_survive_both_directions() {
        let url = "{{base_url}}/path?x={{token}}";
        let rows = sync_url_to_params(url, &[]);
        assert_eq!(rows, vec![row("x", "{{token}}")]);
        assert_eq!(sync_params_to_url(url, &rows), url);
    }

    #[test]
    fn no_query_behaviour() {
        // No query + no enabled params → URL unchanged, no dangling `?`.
        assert_eq!(sync_params_to_url("https://x/path", &[]), "https://x/path");
        // No query + enabled params → `?` added.
        assert_eq!(
            sync_params_to_url("https://x/path", &[row("a", "1")]),
            "https://x/path?a=1"
        );
        // A URL with no query yields only the preserved disabled rows
        // (enabled rows are cleared — the user deleted them from the URL).
        let existing = vec![row("a", "1"), disabled("b", "2", "note")];
        assert_eq!(
            sync_url_to_params("https://x/path", &existing),
            vec![disabled("b", "2", "note")]
        );
    }

    #[test]
    fn duplicate_keys_preserved_in_order() {
        let url = "https://x/?tag=a&tag=b";
        let rows = sync_url_to_params(url, &[]);
        assert_eq!(rows, vec![row("tag", "a"), row("tag", "b")]);
        assert_eq!(sync_params_to_url(url, &rows), url);
    }

    #[test]
    fn empty_key_rows_are_dropped_from_the_url() {
        assert_eq!(
            sync_params_to_url("https://x/", &[row("", "v"), row("a", "1")]),
            "https://x/?a=1"
        );
    }

    #[test]
    fn fragment_is_dropped_by_sync_params_to_url() {
        assert_eq!(
            sync_params_to_url("https://x/path?a=1#frag", &[row("a", "2")]),
            "https://x/path?a=2"
        );
        // Dropped even when there was no query at all.
        assert_eq!(
            sync_params_to_url("https://x/path#frag", &[row("a", "1")]),
            "https://x/path?a=1"
        );
        // And the no-params case returns the fragmentless prefix.
        assert_eq!(
            sync_params_to_url("https://x/path?a=1#frag", &[]),
            "https://x/path"
        );
    }
}
