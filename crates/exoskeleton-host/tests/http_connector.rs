//! Integration tests for http.request connector via the Vessel.
//!
//! Proves: W-1 (http.request enabled), I1 (adapter boundary), I2 (idempotency),
//! I9 (Tool AQ execution), H-1 resolved (safe drop in async context).

mod common;

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

use exoskeleton_host::vessel::Vessel;

/// Spawn a minimal mock HTTP server that accepts one connection, reads the
/// request, writes a canned 200 OK, and returns the captured request lines.
fn mock_http_server() -> (String, std::thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://127.0.0.1:{}", addr.port());

    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let reader = BufReader::new(stream.try_clone().unwrap());

        let mut lines = Vec::new();
        for line in reader.lines() {
            let line = line.unwrap();
            if line.is_empty() {
                break;
            }
            lines.push(line);
        }

        let body = r#"{"result":"ok"}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).unwrap();
        stream.flush().unwrap();

        lines
    });

    (url, handle)
}

// ── E0-T5: http.request appears in vessel capability list ──

#[tokio::test]
async fn http_request_in_capability_list() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::test_config(dir.path());
    let vessel = Vessel::start_with_registry(config, common::test_registry())
        .await
        .unwrap();

    let caps = vessel.list_capabilities();
    let names: Vec<&str> = caps.iter().map(|d| d.name.as_str()).collect();
    assert!(
        names.contains(&"http.request"),
        "http.request not in capabilities: {names:?}"
    );

    vessel.shutdown().await.unwrap();
}

// ── E0-T4: HTTP GET via invoke_tool (Tool AQ path) ──

#[tokio::test]
async fn http_get_via_invoke_tool() {
    let (url, server_handle) = mock_http_server();

    let dir = tempfile::tempdir().unwrap();
    let config = common::test_config(dir.path());
    let vessel = Vessel::start_with_registry(config, common::test_registry())
        .await
        .unwrap();

    let result = vessel
        .invoke_tool(
            "http.request",
            serde_json::json!({"url": url, "method": "GET"}),
        )
        .await
        .unwrap();

    assert_eq!(result["status"], 200);
    assert_eq!(result["body"], r#"{"result":"ok"}"#);

    // Verify the server received the request with idempotency header
    let request_lines = server_handle.join().unwrap();
    let has_idempotency = request_lines
        .iter()
        .any(|l| l.to_lowercase().starts_with("x-idempotency-key:"));
    assert!(has_idempotency, "I2: X-Idempotency-Key header missing");

    vessel.shutdown().await.unwrap();
}

// ── E0-T6: Vessel boots and shuts down cleanly with http.request ──

#[tokio::test]
async fn vessel_shutdown_with_http_connector() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::test_config(dir.path());

    // Boot with full registry including http.request
    let vessel = Vessel::start_with_registry(config, common::test_registry())
        .await
        .unwrap();

    // Shutdown should complete without panic (H-1 resolved)
    vessel.shutdown().await.unwrap();
}

// ── E0-T7: HttpRequestConnector safe in #[tokio::test] context ──

#[tokio::test]
async fn http_connector_drop_in_tokio_test() {
    use worldinterface_connector::connectors::HttpRequestConnector;

    // Construct and drop inside async context — no panic
    let connector = HttpRequestConnector::new();
    let desc = worldinterface_connector::traits::Connector::describe(&connector);
    assert_eq!(desc.name, "http.request");
    drop(connector);
}
