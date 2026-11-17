//! Integration tests for the async HTTP engine, run against a local
//! one-shot TCP server (no external network).

use rest_harness::domain::{HttpMethod, KeyValueRow, RawContentType, Request, RequestBody};
use rest_harness::http::{HttpClient, ReqwestHttpClient};
use rest_harness::interpolation::prepare;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// First index of `needle` inside `haystack`.
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Spawn a one-shot HTTP/1.1 server on `127.0.0.1:0` that reads a single
/// request (headers plus a content-length body when one is declared), writes
/// `response` verbatim, and resolves to the raw request text it received.
async fn serve_once(response: String) -> (SocketAddr, JoinHandle<String>) {
    serve_once_bytes(response.into_bytes()).await
}

/// Byte-oriented variant of [`serve_once`] for responses whose body is not
/// valid UTF-8.
async fn serve_once_bytes(response: Vec<u8>) -> (SocketAddr, JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();

        let mut received: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 1024];
        let header_end = loop {
            let read = socket.read(&mut chunk).await.unwrap();
            assert!(read > 0, "connection closed before headers completed");
            received.extend_from_slice(&chunk[..read]);
            if let Some(end) = find_subsequence(&received, b"\r\n\r\n") {
                break end + 4;
            }
        };

        // Body length, when the request declares one.
        let content_length = String::from_utf8_lossy(&received)
            .to_ascii_lowercase()
            .lines()
            .find_map(|line| {
                line.strip_prefix("content-length:")
                    .map(|v| v.trim().parse::<usize>().unwrap())
            })
            .unwrap_or(0);
        while received.len() < header_end + content_length {
            let read = socket.read(&mut chunk).await.unwrap();
            assert!(read > 0, "connection closed before body completed");
            received.extend_from_slice(&chunk[..read]);
        }

        let request_text = String::from_utf8_lossy(&received).into_owned();
        socket.write_all(&response).await.unwrap();
        socket.shutdown().await.unwrap();
        request_text
    });
    (addr, handle)
}

fn prepared_get(
    url: String,
    headers: Vec<KeyValueRow>,
) -> rest_harness::interpolation::PreparedHttpRequest {
    let request = Request {
        url,
        headers,
        ..Request::default()
    };
    prepare(&request, &BTreeMap::new()).unwrap()
}

#[tokio::test]
async fn executes_against_local_server_and_captures_the_response() {
    let body = r#"{"ok":true,"count":2}"#;
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nx-custom: hello\r\nx-dup: one\r\nx-dup: two\r\ncontent-length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    let (addr, server) = serve_once(response).await;

    let prepared = prepared_get(
        format!("http://{addr}/users?id=1"),
        vec![KeyValueRow::new("X-Marker", "harness")],
    );
    let client = ReqwestHttpClient::new(Duration::from_secs(5)).unwrap();
    let data = client.execute(&prepared).await.unwrap();
    let request_text = server.await.unwrap();

    assert_eq!(data.status_code, 200);
    assert_eq!(data.reason_phrase, "OK");

    let header = |name: &str| {
        data.headers
            .iter()
            .find(|h| h.name == name)
            .map(|h| h.value.clone())
    };
    assert_eq!(header("content-type").as_deref(), Some("application/json"));
    assert_eq!(header("x-custom").as_deref(), Some("hello"));
    // Duplicate names joined with ", " preserving order.
    assert_eq!(header("x-dup").as_deref(), Some("one, two"));

    assert_eq!(data.size_bytes, body.len() as u64);
    assert_eq!(data.body_raw, body);
    assert_eq!(data.body_pretty, "{\n  \"count\": 2,\n  \"ok\": true\n}");
    // Elapsed timing is captured (u64 milliseconds).
    let _ = data.elapsed_ms;

    // The engine sent the method, path and custom header we asked for.
    assert!(request_text.starts_with("GET /users?id=1 HTTP/1.1\r\n"));
    assert!(request_text
        .to_ascii_lowercase()
        .contains("x-marker: harness"));
}

#[tokio::test]
async fn sends_raw_json_body_with_the_default_content_type() {
    let body = r#"{"name":"ada"}"#;
    let response = "HTTP/1.1 201 Created\r\ncontent-length: 2\r\n\r\nok".to_string();
    let (addr, server) = serve_once(response).await;

    let request = Request {
        method: HttpMethod::Post,
        url: format!("http://{addr}/create"),
        body: RequestBody::Raw {
            content_type: RawContentType::Json,
            content: body.to_string(),
        },
        ..Request::default()
    };
    let prepared = prepare(&request, &BTreeMap::new()).unwrap();

    let client = ReqwestHttpClient::new(Duration::from_secs(5)).unwrap();
    let data = client.execute(&prepared).await.unwrap();
    let request_text = server.await.unwrap();

    assert_eq!(data.status_code, 201);
    assert_eq!(data.reason_phrase, "Created");
    assert_eq!(data.size_bytes, 2);
    // "ok" is not JSON, so pretty == raw.
    assert_eq!(data.body_raw, "ok");
    assert_eq!(data.body_pretty, "ok");

    assert!(request_text.starts_with("POST /create HTTP/1.1\r\n"));
    let lowered = request_text.to_ascii_lowercase();
    assert!(lowered.contains("content-type: application/json"));
    assert!(lowered.contains(&format!("content-length: {}", body.len())));
    assert!(request_text.contains(body));
}

#[tokio::test]
async fn schemeless_prepared_urls_work_end_to_end() {
    let response = "HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n".to_string();
    let (addr, server) = serve_once(response).await;

    // Schemeless `host:port/path` — `prepare` must prepend `http://`.
    let prepared = prepared_get(format!("{addr}/ping"), Vec::new());
    assert!(
        prepared.url.starts_with("http://"),
        "prepare must prepend the scheme: {}",
        prepared.url
    );

    let client = ReqwestHttpClient::default();
    let data = client.execute(&prepared).await.unwrap();
    let request_text = server.await.unwrap();

    assert_eq!(data.status_code, 200);
    assert_eq!(data.reason_phrase, "OK");
    assert_eq!(data.size_bytes, 0);
    assert_eq!(data.body_raw, "");
    assert!(request_text.starts_with("GET /ping HTTP/1.1\r\n"));
}

#[tokio::test]
async fn non_utf8_bodies_become_a_readable_binary_summary() {
    // Repeated `[0xFF, 0xFE, 0x00, 0x01]` — invalid UTF-8, contains NULs.
    let body: Vec<u8> = [0xFF, 0xFE, 0x00, 0x01]
        .into_iter()
        .cycle()
        .take(32)
        .collect();
    let mut response =
        format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n", body.len()).into_bytes();
    response.extend_from_slice(&body);
    let (addr, server) = serve_once_bytes(response).await;

    let prepared = prepared_get(format!("http://{addr}/binary"), Vec::new());
    let client = ReqwestHttpClient::new(Duration::from_secs(5)).unwrap();
    let data = client.execute(&prepared).await.unwrap();
    server.await.unwrap();

    assert_eq!(data.size_bytes, 32);
    assert!(
        data.body_raw.starts_with("[Binary response: 32 bytes]\n"),
        "binary summary expected: {:?}",
        data.body_raw
    );
    // The hex dump contains the payload bytes.
    assert!(data.body_raw.contains("FF FE 00 01"), "{:?}", data.body_raw);
    // Not JSON, so pretty == raw.
    assert_eq!(data.body_pretty, data.body_raw);
}

#[tokio::test]
async fn duplicate_and_multi_value_headers_reach_history_entry() {
    let response =
        "HTTP/1.1 200 OK\r\nset-cookie: a=1\r\nset-cookie: b=2\r\ncontent-length: 0\r\n\r\n"
            .to_string();
    let (addr, server) = serve_once(response).await;

    let prepared = prepared_get(format!("http://{addr}/"), Vec::new());
    let client = ReqwestHttpClient::new(Duration::from_secs(5)).unwrap();
    let data = client.execute(&prepared).await.unwrap();
    server.await.unwrap();

    let entry = rest_harness::http::build_history_entry(&prepared, &data);
    assert_eq!(
        entry
            .response
            .headers
            .iter()
            .find(|h| h.name == "set-cookie")
            .map(|h| h.value.clone()),
        Some("a=1, b=2".to_string())
    );
    assert_eq!(entry.request.url, prepared.url);
}
