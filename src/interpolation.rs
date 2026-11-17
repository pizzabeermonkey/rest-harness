//! `{{variable}}` interpolation and request preparation.
//!
//! Placeholder syntax: `{{name}}`. The identifier between the braces is
//! trimmed of surrounding whitespace; an empty identifier (`{{ }}`) is NOT
//! a placeholder and stays literal, and an unmatched `{{` without a closing
//! `}}` is literal text as well.
//!
//! Expansion is single-pass: a variable whose own value contains `{{...}}`
//! is NOT re-expanded.
//!
//! [`prepare`] turns a stored [`Request`] into a [`PreparedHttpRequest`] the
//! HTTP client can execute: variables resolved, scheme completed, headers
//! assembled (enabled rows + Content-Type default + auth-derived entries)
//! and the body resolved. See the individual items for the exact rules.

use crate::domain::{
    ApiKeyLocation, AuthConfig, HttpHeader, HttpMethod, RawContentType, Request, RequestBody,
};
use crate::error::{Error, Result};
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use std::collections::{BTreeMap, BTreeSet};

/// Fully resolved request ready for the HTTP client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedHttpRequest {
    /// HTTP method.
    pub method: HttpMethod,
    /// Resolved URL; `http://` is prepended when the resolved URL has no scheme.
    pub url: String,
    /// Enabled header rows (interpolated) + auth-derived + Content-Type defaults.
    pub headers: Vec<HttpHeader>,
    /// Resolved raw body; `None` for [`RequestBody::None`] and for
    /// GET/HEAD/OPTIONS (those methods never send a body).
    pub body: Option<String>,
}

/// Scan `text` for `{{` ... `}}` placeholder pairs and insert the trimmed
/// identifier of each into `out`.
///
/// An empty identifier after trimming (`{{ }}`) is NOT a placeholder — it is
/// literal text. An unmatched `{{` without a closing `}}` is literal text.
pub fn collect_placeholders(text: &str, out: &mut BTreeSet<String>) {
    let mut cursor = 0;
    while let Some(offset) = text[cursor..].find("{{") {
        let inner = cursor + offset + 2;
        match text[inner..].find("}}") {
            // Unmatched `{{` — the rest is literal text.
            None => break,
            Some(end_offset) => {
                let end = inner + end_offset;
                let ident = text[inner..end].trim();
                if !ident.is_empty() {
                    out.insert(ident.to_string());
                }
                cursor = end + 2;
            }
        }
    }
}

/// Union of every placeholder in `request` that has no value in `variables`,
/// sorted and deduplicated. Sources: the URL, enabled params (keys AND
/// values), enabled headers (keys AND values), raw body content, and the
/// auth fields. An empty vec means the request is ready to send.
pub fn missing_variables(request: &Request, variables: &BTreeMap<String, String>) -> Vec<String> {
    let mut found = BTreeSet::new();

    collect_placeholders(&request.url, &mut found);
    for row in request.params.iter().filter(|row| row.enabled) {
        collect_placeholders(&row.key, &mut found);
        collect_placeholders(&row.value, &mut found);
    }
    for row in request.headers.iter().filter(|row| row.enabled) {
        collect_placeholders(&row.key, &mut found);
        collect_placeholders(&row.value, &mut found);
    }
    if let RequestBody::Raw { content, .. } = &request.body {
        collect_placeholders(content, &mut found);
    }
    match &request.auth {
        AuthConfig::None => {}
        AuthConfig::Bearer { token } => collect_placeholders(token, &mut found),
        AuthConfig::Basic { username, password } => {
            collect_placeholders(username, &mut found);
            collect_placeholders(password, &mut found);
        }
        AuthConfig::ApiKey { key, value, .. } => {
            collect_placeholders(key, &mut found);
            collect_placeholders(value, &mut found);
        }
    }

    found.retain(|name| !variables.contains_key(name));
    found.into_iter().collect()
}

/// Resolve a [`Request`] into a [`PreparedHttpRequest`], or fail with
/// [`Error::MissingVariables`] listing every unresolved placeholder.
///
/// Rules (v1 product decisions):
/// - The resolved URL gets `http://` prepended when it does not start with
///   `http://` or `https://` (case-insensitive).
/// - Headers start from the enabled header rows (interpolated). For raw
///   bodies, `Content-Type` is appended (`application/json` for JSON,
///   `text/plain` for text) unless an enabled header already sets
///   `content-type` (case-insensitive).
/// - Auth is applied only when `AuthConfig` is not `None`; Bearer/Basic are
///   skipped when an `authorization` header is already present. API keys go
///   to the query (appended after an existing `?`), a header, or a cookie
///   (appended to an existing cookie with `"; "`).
/// - [`RequestBody::None`] sends no body. GET/HEAD/OPTIONS never send a body
///   even when raw content exists (reqwest would reject it).
pub fn prepare(
    request: &Request,
    variables: &BTreeMap<String, String>,
) -> Result<PreparedHttpRequest> {
    let missing = missing_variables(request, variables);
    if !missing.is_empty() {
        return Err(Error::MissingVariables { variables: missing });
    }

    let mut url = interpolate(&request.url, variables);
    if !has_scheme(&url) {
        // Schemeless URLs (e.g. `127.0.0.1:8080/ping`, `example.com/x`)
        // default to plain HTTP.
        url = format!("http://{url}");
    }

    let mut headers: Vec<HttpHeader> = request
        .headers
        .iter()
        .filter(|row| row.enabled)
        .map(|row| HttpHeader {
            name: interpolate(&row.key, variables),
            value: interpolate(&row.value, variables),
        })
        .collect();

    if let RequestBody::Raw { content_type, .. } = &request.body {
        if !has_header(&headers, "content-type") {
            let value = match content_type {
                RawContentType::Json => "application/json",
                RawContentType::Text => "text/plain",
            };
            headers.push(HttpHeader {
                name: "Content-Type".to_string(),
                value: value.to_string(),
            });
        }
    }

    match &request.auth {
        AuthConfig::None => {}
        AuthConfig::Bearer { token } => {
            if !has_header(&headers, "authorization") {
                headers.push(HttpHeader {
                    name: "Authorization".to_string(),
                    value: format!("Bearer {}", interpolate(token, variables)),
                });
            }
        }
        AuthConfig::Basic { username, password } => {
            if !has_header(&headers, "authorization") {
                let user = interpolate(username, variables);
                let pass = interpolate(password, variables);
                headers.push(HttpHeader {
                    name: "Authorization".to_string(),
                    value: format!("Basic {}", STANDARD.encode(format!("{user}:{pass}"))),
                });
            }
        }
        AuthConfig::ApiKey {
            key,
            value,
            in_location,
        } => {
            let key = interpolate(key, variables);
            let value = interpolate(value, variables);
            match in_location {
                ApiKeyLocation::Query => {
                    // Appended raw; reqwest's URL parser applies the final
                    // query percent-encoding at send time.
                    let separator = if url.contains('?') { '&' } else { '?' };
                    url = format!("{url}{separator}{key}={value}");
                }
                ApiKeyLocation::Header => {
                    headers.push(HttpHeader { name: key, value });
                }
                ApiKeyLocation::Cookie => {
                    if let Some(existing) = headers
                        .iter_mut()
                        .find(|h| h.name.eq_ignore_ascii_case("cookie"))
                    {
                        existing.value = format!("{}; {key}={value}", existing.value);
                    } else {
                        headers.push(HttpHeader {
                            name: "Cookie".to_string(),
                            value: format!("{key}={value}"),
                        });
                    }
                }
            }
        }
    }

    let raw_body = match &request.body {
        RequestBody::None => None,
        RequestBody::Raw { content, .. } => Some(interpolate(content, variables)),
    };
    // GET/HEAD/OPTIONS never send a body even when raw content exists —
    // reqwest rejects bodies on those methods.
    let body = match request.method {
        HttpMethod::Get | HttpMethod::Head | HttpMethod::Options => None,
        _ => raw_body,
    };

    Ok(PreparedHttpRequest {
        method: request.method,
        url,
        headers,
        body,
    })
}

/// Whether `url` already carries an `http` or `https` scheme
/// (case-insensitive).
fn has_scheme(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// Whether `headers` already contains a header with this name
/// (case-insensitive).
fn has_header(headers: &[HttpHeader], name: &str) -> bool {
    headers.iter().any(|h| h.name.eq_ignore_ascii_case(name))
}

/// Single-pass placeholder expansion of `text`.
///
/// Mirrors [`collect_placeholders`] scanning: `{{ }}` and unmatched `{{` are
/// literal text. Variable values are inserted verbatim and never
/// re-expanded.
fn interpolate(text: &str, variables: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    while let Some(offset) = text[cursor..].find("{{") {
        let start = cursor + offset;
        out.push_str(&text[cursor..start]);
        let inner = start + 2;
        match text[inner..].find("}}") {
            None => {
                out.push_str(&text[start..]);
                cursor = text.len();
            }
            Some(end_offset) => {
                let end = inner + end_offset;
                let ident = text[inner..end].trim();
                if ident.is_empty() {
                    out.push_str(&text[start..end + 2]);
                } else {
                    match variables.get(ident) {
                        Some(value) => out.push_str(value),
                        // Unreachable via `prepare` (which errors on missing
                        // variables first); keep the placeholder literal.
                        None => out.push_str(&text[start..end + 2]),
                    }
                }
                cursor = end + 2;
            }
        }
    }
    out.push_str(&text[cursor..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::KeyValueRow;

    /// Variable map from `(name, value)` pairs.
    fn vars(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// A disabled row.
    fn disabled(key: &str, value: &str) -> KeyValueRow {
        KeyValueRow {
            key: key.into(),
            value: value.into(),
            description: String::new(),
            enabled: false,
        }
    }

    #[test]
    fn collect_finds_placeholders_and_skips_empty_or_unmatched() {
        let mut out = BTreeSet::new();
        collect_placeholders("{{ a }} and {{b}} and {{ }} and {{unmatched", &mut out);
        assert_eq!(out.iter().cloned().collect::<Vec<_>>(), vec!["a", "b"]);
    }

    #[test]
    fn missing_variables_are_sorted_and_deduplicated() {
        let request = Request {
            url: "{{beta}}/{{alpha}}/{{beta}}".into(),
            params: vec![KeyValueRow::new("q", "{{delta}}")],
            headers: vec![KeyValueRow::new("X-{{echo}}", "h")],
            body: RequestBody::Raw {
                content_type: RawContentType::Text,
                content: "{{foxtrot}}".into(),
            },
            auth: AuthConfig::Bearer {
                token: "{{token}}".into(),
            },
            ..Request::default()
        };
        let missing = missing_variables(&request, &vars(&[("present", "1")]));
        assert_eq!(
            missing,
            vec!["alpha", "beta", "delta", "echo", "foxtrot", "token"]
        );
    }

    #[test]
    fn disabled_rows_and_none_auth_are_not_scanned() {
        let request = Request {
            url: "https://x".into(),
            params: vec![disabled("p", "{{gone}}")],
            headers: vec![disabled("h", "{{also-gone}}")],
            ..Request::default()
        };
        assert!(missing_variables(&request, &BTreeMap::new()).is_empty());
    }

    #[test]
    fn single_pass_expansion_does_not_reexpand_values() {
        let variables = vars(&[("a", "{{b}}"), ("b", "value")]);
        let request = Request {
            url: "{{a}}?q={{b}}".into(),
            ..Request::default()
        };
        let prepared = prepare(&request, &variables).unwrap();
        assert_eq!(prepared.url, "http://{{b}}?q=value");
    }

    #[test]
    fn scheme_is_prepended_when_missing() {
        let variables = BTreeMap::new();
        let prepared = prepare(
            &Request {
                url: "example.com/x".into(),
                ..Request::default()
            },
            &variables,
        )
        .unwrap();
        assert_eq!(prepared.url, "http://example.com/x");

        // Existing schemes (any case) are kept as-is.
        for url in ["https://example.com", "HTTPS://example.com", "http://x/y"] {
            let prepared = prepare(
                &Request {
                    url: url.into(),
                    ..Request::default()
                },
                &variables,
            )
            .unwrap();
            assert_eq!(prepared.url, url);
        }
    }

    #[test]
    fn content_type_added_once_for_raw_bodies() {
        let variables = BTreeMap::new();
        let request = Request {
            method: HttpMethod::Post,
            url: "https://x".into(),
            body: RequestBody::Raw {
                content_type: RawContentType::Json,
                content: "{}".into(),
            },
            ..Request::default()
        };
        let prepared = prepare(&request, &variables).unwrap();
        assert_eq!(
            prepared.headers,
            vec![HttpHeader {
                name: "Content-Type".into(),
                value: "application/json".into()
            }]
        );

        // An enabled content-type header suppresses the default.
        let with_header = Request {
            headers: vec![KeyValueRow::new("content-type", "text/csv")],
            ..request.clone()
        };
        let prepared = prepare(&with_header, &variables).unwrap();
        assert_eq!(prepared.headers.len(), 1);
        assert_eq!(prepared.headers[0].value, "text/csv");

        // A DISABLED content-type header does not suppress it.
        let with_disabled = Request {
            headers: vec![disabled("content-type", "text/csv")],
            ..request
        };
        let prepared = prepare(&with_disabled, &variables).unwrap();
        assert_eq!(prepared.headers[0].value, "application/json");
    }

    #[test]
    fn bearer_auth_added_unless_authorization_header_present() {
        let variables = vars(&[("token", "tok-1")]);
        let request = Request {
            url: "https://x".into(),
            auth: AuthConfig::Bearer {
                token: "{{token}}".into(),
            },
            ..Request::default()
        };
        let prepared = prepare(&request, &variables).unwrap();
        assert_eq!(
            prepared.headers,
            vec![HttpHeader {
                name: "Authorization".into(),
                value: "Bearer tok-1".into()
            }]
        );

        // An existing (case-insensitive) authorization header wins.
        let with_header = Request {
            headers: vec![KeyValueRow::new("AUTHORIZATION", "custom")],
            ..request
        };
        let prepared = prepare(&with_header, &variables).unwrap();
        assert_eq!(
            prepared.headers,
            vec![HttpHeader {
                name: "AUTHORIZATION".into(),
                value: "custom".into()
            }]
        );
    }

    #[test]
    fn basic_auth_is_base64_encoded() {
        let variables = vars(&[("user", "ada"), ("pass", "s3cr3t")]);
        let request = Request {
            url: "https://x".into(),
            auth: AuthConfig::Basic {
                username: "{{user}}".into(),
                password: "{{pass}}".into(),
            },
            ..Request::default()
        };
        let prepared = prepare(&request, &variables).unwrap();
        assert_eq!(
            prepared.headers,
            vec![HttpHeader {
                name: "Authorization".into(),
                value: format!("Basic {}", STANDARD.encode("ada:s3cr3t")),
            }]
        );
    }

    #[test]
    fn api_key_goes_to_header_query_and_cookie() {
        let base = |in_location| Request {
            url: "https://x/path?a=1".into(),
            auth: AuthConfig::ApiKey {
                key: "api_key".into(),
                value: "k 1".into(),
                in_location,
            },
            ..Request::default()
        };

        let prepared = prepare(&base(ApiKeyLocation::Header), &BTreeMap::new()).unwrap();
        assert_eq!(
            prepared.headers,
            vec![HttpHeader {
                name: "api_key".into(),
                value: "k 1".into()
            }]
        );

        // Query: appended after the existing `?`.
        let prepared = prepare(&base(ApiKeyLocation::Query), &BTreeMap::new()).unwrap();
        assert_eq!(prepared.url, "https://x/path?a=1&api_key=k 1");
        // Query without an existing `?`.
        let prepared = prepare(
            &Request {
                url: "https://x/path".into(),
                ..base(ApiKeyLocation::Query)
            },
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(prepared.url, "https://x/path?api_key=k 1");

        // Cookie: appended to an existing cookie with `"; "`.
        let with_cookie = Request {
            headers: vec![KeyValueRow::new("Cookie", "session=s")],
            ..base(ApiKeyLocation::Cookie)
        };
        let prepared = prepare(&with_cookie, &BTreeMap::new()).unwrap();
        assert_eq!(
            prepared.headers,
            vec![HttpHeader {
                name: "Cookie".into(),
                value: "session=s; api_key=k 1".into()
            }]
        );
        // Cookie without an existing header.
        let prepared = prepare(&base(ApiKeyLocation::Cookie), &BTreeMap::new()).unwrap();
        assert_eq!(
            prepared.headers,
            vec![HttpHeader {
                name: "Cookie".into(),
                value: "api_key=k 1".into()
            }]
        );
    }

    #[test]
    fn placeholders_inside_param_keys_headers_and_body() {
        let variables = vars(&[("name", "users"), ("token", "t1"), ("id", "42")]);
        let request = Request {
            url: "https://x".into(),
            params: vec![KeyValueRow::new("{{name}}_id", "1")],
            headers: vec![KeyValueRow::new("X-Token", "{{token}}")],
            body: RequestBody::Raw {
                content_type: RawContentType::Json,
                content: r#"{"id": "{{id}}"}"#.into(),
            },
            method: HttpMethod::Post,
            ..Request::default()
        };
        assert!(missing_variables(&request, &variables).is_empty());
        let prepared = prepare(&request, &variables).unwrap();
        assert!(prepared
            .headers
            .iter()
            .any(|h| h.name == "X-Token" && h.value == "t1"));
        assert_eq!(prepared.body, Some(r#"{"id": "42"}"#.to_string()));
    }

    #[test]
    fn get_head_options_never_send_a_body() {
        let variables = BTreeMap::new();
        for method in [HttpMethod::Get, HttpMethod::Head, HttpMethod::Options] {
            let request = Request {
                method,
                url: "https://x".into(),
                body: RequestBody::Raw {
                    content_type: RawContentType::Text,
                    content: "ignored".into(),
                },
                ..Request::default()
            };
            let prepared = prepare(&request, &variables).unwrap();
            assert_eq!(prepared.body, None, "{method:?} must not send a body");
        }
    }

    #[test]
    fn prepare_fails_with_sorted_missing_names() {
        let request = Request {
            url: "{{b}}/{{a}}/{{b}}".into(),
            ..Request::default()
        };
        let err = prepare(&request, &BTreeMap::new()).unwrap_err();
        match err {
            Error::MissingVariables { variables } => {
                assert_eq!(variables, vec!["a".to_string(), "b".to_string()]);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn literal_and_unmatched_braces_survive_interpolation() {
        let variables = vars(&[("a", "A")]);
        let request = Request {
            url: "https://x/{{ }}?q={{a}}{{open".into(),
            ..Request::default()
        };
        let prepared = prepare(&request, &variables).unwrap();
        assert_eq!(prepared.url, "https://x/{{ }}?q=A{{open");
    }
}
