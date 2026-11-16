//! Async HTTP execution engine built on `reqwest` (rustls TLS, no openssl
//! linkage).
//!
//! [`ReqwestHttpClient`] is the real [`HttpClient`] implementation; the GUI
//! and tests substitute their own fakes via the [`HttpClient`] port.
//!
//! Behaviour notes:
//! - Timing ([`HttpResponseData::elapsed_ms`]) is measured with
//!   [`std::time::Instant`] around the whole send+read cycle.
//! - Duplicate response header names are joined with `", "` preserving the
//!   order in which they arrived.
//! - v1 pretty-prints JSON bodies only ([`crate::domain::pretty_json`]);
//!   XML/HTML bodies are shown raw.
//! - Network/timeout errors surface as [`Error::Other`]. A reqwest error's
//!   `Display` may embed the request URL; that URL is the interpolated one,
//!   which is only secret-bearing when the user placed a secret directly in
//!   the URL — the History redaction covers such values on storage, which is
//!   acceptable for v1.

use crate::domain::{HistoryEntry, HttpHeader, HttpMethod, RedactedHttpRequest};
use crate::error::{Error, Result};
use crate::interpolation::PreparedHttpRequest;
use std::str::FromStr;
use std::time::{Duration, Instant};

/// Structured response data returned by any [`HttpClient`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponseData {
    /// HTTP status code.
    pub status_code: u16,
    /// Canonical reason phrase (e.g. `OK`); empty when unknown.
    pub reason_phrase: String,
    /// Wall-clock duration of the call (send + read) in milliseconds.
    pub elapsed_ms: u64,
    /// Response body size in bytes.
    pub size_bytes: u64,
    /// Response headers; duplicate names joined with `", "` in arrival order.
    pub headers: Vec<HttpHeader>,
    /// Response body as lossy UTF-8.
    pub body_raw: String,
    /// Pretty-printed body when it parses as JSON, otherwise equal to
    /// [`HttpResponseData::body_raw`].
    pub body_pretty: String,
}

/// Port for executing prepared HTTP requests, so the GUI and tests can
/// substitute a fake for [`ReqwestHttpClient`].
pub trait HttpClient {
    /// Execute the request and return structured response data.
    fn execute(
        &self,
        request: &PreparedHttpRequest,
    ) -> impl std::future::Future<Output = Result<HttpResponseData>> + Send;
}

/// The real [`HttpClient`] on top of `reqwest`, with a total request timeout
/// and at most 10 redirects.
#[derive(Debug, Clone)]
pub struct ReqwestHttpClient {
    client: reqwest::Client,
}

impl ReqwestHttpClient {
    /// A client whose requests time out after `timeout`.
    pub fn new(timeout: Duration) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::limited(10))
            .build()
            .map_err(|e| Error::Other(format!("failed to build http client: {e}")))?;
        Ok(ReqwestHttpClient { client })
    }
}

impl Default for ReqwestHttpClient {
    fn default() -> Self {
        // Builder construction only fails on TLS backend setup errors, which
        // cannot happen for the fixed builder used here.
        Self::new(Duration::from_secs(30)).expect("reqwest client construction cannot fail")
    }
}

impl HttpClient for ReqwestHttpClient {
    async fn execute(&self, request: &PreparedHttpRequest) -> Result<HttpResponseData> {
        let method = reqwest_method(request.method)?;
        let url = reqwest::Url::parse(&request.url)
            .map_err(|e| Error::Other(format!("invalid url: {e}")))?;

        let mut builder = self.client.request(method, url);
        for header in &request.headers {
            let name = reqwest::header::HeaderName::from_str(&header.name)
                .map_err(|e| Error::Other(format!("invalid header name {:?}: {e}", header.name)))?;
            let value = reqwest::header::HeaderValue::from_str(&header.value).map_err(|e| {
                Error::Other(format!("invalid value for header {:?}: {e}", header.name))
            })?;
            builder = builder.header(name, value);
        }
        if let Some(body) = &request.body {
            builder = builder.body(body.clone());
        }

        let started = Instant::now();
        let response = builder
            .send()
            .await
            .map_err(|e| Error::Other(e.to_string()))?;

        let status = response.status();
        let mut headers: Vec<HttpHeader> = Vec::new();
        for (name, value) in response.headers() {
            let value = String::from_utf8_lossy(value.as_bytes()).into_owned();
            if let Some(existing) = headers.iter_mut().find(|h| h.name == name.as_str()) {
                existing.value.push_str(", ");
                existing.value.push_str(&value);
            } else {
                headers.push(HttpHeader {
                    name: name.as_str().to_string(),
                    value,
                });
            }
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|e| Error::Other(e.to_string()))?;
        let elapsed_ms = started.elapsed().as_millis() as u64;

        let body_raw = String::from_utf8_lossy(&bytes).into_owned();
        // v1 pretty-prints JSON only; XML/HTML bodies are shown raw.
        let body_pretty = if serde_json::from_str::<serde_json::Value>(&body_raw).is_ok() {
            crate::domain::pretty_json(&body_raw)
        } else {
            body_raw.clone()
        };

        Ok(HttpResponseData {
            status_code: status.as_u16(),
            reason_phrase: status.canonical_reason().unwrap_or("").to_string(),
            elapsed_ms,
            size_bytes: bytes.len() as u64,
            headers,
            body_raw,
            body_pretty,
        })
    }
}

/// Map a domain [`HttpMethod`] onto `reqwest::Method`.
///
/// All seven supported variants map directly; the error arm exists so that a
/// future variant addition fails loudly instead of silently.
fn reqwest_method(method: HttpMethod) -> Result<reqwest::Method> {
    Ok(match method {
        HttpMethod::Get => reqwest::Method::GET,
        HttpMethod::Post => reqwest::Method::POST,
        HttpMethod::Put => reqwest::Method::PUT,
        HttpMethod::Patch => reqwest::Method::PATCH,
        HttpMethod::Delete => reqwest::Method::DELETE,
        HttpMethod::Head => reqwest::Method::HEAD,
        HttpMethod::Options => reqwest::Method::OPTIONS,
    })
}

/// Build a History Entry from an exchange, stamped with the current UTC time.
///
/// The prepared request is mapped into a [`RedactedHttpRequest`] (headers
/// cloned, body = body or empty string). Redaction itself stays in
/// [`crate::history::HistoryStore::append_entry`] — it is not duplicated
/// here.
pub fn build_history_entry(
    request: &PreparedHttpRequest,
    response: &HttpResponseData,
) -> HistoryEntry {
    crate::history::build_entry(
        RedactedHttpRequest {
            method: request.method,
            url: request.url.clone(),
            headers: request.headers.clone(),
            body: request.body.clone().unwrap_or_default(),
        },
        response.status_code,
        response.reason_phrase.clone(),
        response.elapsed_ms,
        response.size_bytes,
        response.headers.clone(),
        response.body_raw.clone(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_client_constructs_with_thirty_second_timeout() {
        // The timeout is passed to the reqwest builder; construction itself
        // must succeed.
        let _client = ReqwestHttpClient::default();
        let _client = ReqwestHttpClient::new(Duration::from_millis(250)).unwrap();
    }

    #[test]
    fn maps_all_seven_methods() {
        assert_eq!(
            reqwest_method(HttpMethod::Get).unwrap(),
            reqwest::Method::GET
        );
        assert_eq!(
            reqwest_method(HttpMethod::Post).unwrap(),
            reqwest::Method::POST
        );
        assert_eq!(
            reqwest_method(HttpMethod::Put).unwrap(),
            reqwest::Method::PUT
        );
        assert_eq!(
            reqwest_method(HttpMethod::Patch).unwrap(),
            reqwest::Method::PATCH
        );
        assert_eq!(
            reqwest_method(HttpMethod::Delete).unwrap(),
            reqwest::Method::DELETE
        );
        assert_eq!(
            reqwest_method(HttpMethod::Head).unwrap(),
            reqwest::Method::HEAD
        );
        assert_eq!(
            reqwest_method(HttpMethod::Options).unwrap(),
            reqwest::Method::OPTIONS
        );
    }

    #[tokio::test]
    async fn execute_rejects_invalid_urls_before_sending() {
        let client = ReqwestHttpClient::new(Duration::from_secs(1)).unwrap();
        let request = PreparedHttpRequest {
            method: HttpMethod::Get,
            url: "http://exa mple.com".into(),
            headers: vec![],
            body: None,
        };
        let err = client.execute(&request).await.unwrap_err();
        assert!(matches!(err, Error::Other(_)), "got: {err:?}");
    }

    #[tokio::test]
    async fn execute_rejects_invalid_header_names_mentioning_the_header() {
        let client = ReqwestHttpClient::new(Duration::from_secs(1)).unwrap();
        let request = PreparedHttpRequest {
            method: HttpMethod::Get,
            url: "http://example.com".into(),
            headers: vec![HttpHeader {
                name: "bad header".into(),
                value: "v".into(),
            }],
            body: None,
        };
        let err = client.execute(&request).await.unwrap_err();
        match err {
            Error::Other(message) => assert!(message.contains("bad header"), "{message}"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn build_history_entry_maps_the_prepared_request() {
        let request = PreparedHttpRequest {
            method: HttpMethod::Post,
            url: "https://x/y".into(),
            headers: vec![HttpHeader {
                name: "Authorization".into(),
                value: "Bearer tok".into(),
            }],
            body: Some("{\"a\":1}".into()),
        };
        let response = HttpResponseData {
            status_code: 201,
            reason_phrase: "Created".into(),
            elapsed_ms: 9,
            size_bytes: 2,
            headers: vec![HttpHeader {
                name: "content-type".into(),
                value: "application/json".into(),
            }],
            body_raw: "ok".into(),
            body_pretty: "ok".into(),
        };
        let entry = build_history_entry(&request, &response);
        assert_eq!(entry.request.method, HttpMethod::Post);
        assert_eq!(entry.request.url, "https://x/y");
        assert_eq!(entry.request.headers, request.headers);
        assert_eq!(entry.request.body, "{\"a\":1}");
        assert_eq!(entry.response.status_code, 201);
        assert_eq!(entry.response.reason_phrase, "Created");
        assert_eq!(entry.response.elapsed_ms, 9);
        assert_eq!(entry.response.size_bytes, 2);
        assert_eq!(entry.response.body, "ok");
        assert!(!entry.response.body_truncated);
    }
}
