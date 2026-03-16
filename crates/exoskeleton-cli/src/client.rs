//! HTTP client for communicating with a running ExoDaemon.
//!
//! [`DaemonClient`] wraps `reqwest` to provide typed access to all daemon
//! endpoints. All responses are returned as `serde_json::Value` for simplicity --
//! the CLI formats and displays them, it does not need typed domain objects.

use std::time::Duration;

/// CLI-specific error type wrapping HTTP and display errors.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// Could not reach the daemon at the configured address.
    #[error("daemon connection failed: {0}")]
    Connection(String),
    /// Daemon returned a non-success HTTP status.
    #[error("daemon returned error {status}: {body}")]
    DaemonError { status: u16, body: String },
    /// Configuration error (invalid address, missing config file, etc.).
    #[error("configuration error: {0}")]
    Config(String),
    /// Catch-all for other errors.
    #[error("{0}")]
    Other(String),
}

/// HTTP client for communicating with a running ExoDaemon.
///
/// Each method corresponds to one daemon API endpoint. All methods return
/// `serde_json::Value` for simplicity -- the CLI handles formatting.
pub struct DaemonClient {
    base_url: String,
    client: reqwest::Client,
}

impl DaemonClient {
    /// Create a new client targeting the given daemon base URL.
    ///
    /// The client has a 5-second timeout on all requests.
    pub fn new(base_url: &str) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("failed to build reqwest client");

        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client,
        }
    }

    /// GET /api/v1/status -> StateSnapshot or None (204).
    pub async fn status(&self) -> Result<Option<serde_json::Value>, CliError> {
        let resp = self.get_raw("/api/v1/status").await?;
        if resp.status == 204 {
            return Ok(None);
        }
        let value = serde_json::from_str(&resp.body)
            .map_err(|e| CliError::Other(format!("failed to parse response: {e}")))?;
        Ok(Some(value))
    }

    /// GET /api/v1/ticks?limit=N — returns tick history.
    pub async fn ticks(&self, limit: usize) -> Result<Vec<serde_json::Value>, CliError> {
        let path = format!("/api/v1/ticks?limit={limit}");
        let resp = self.get_raw(&path).await?;
        let value = serde_json::from_str(&resp.body)
            .map_err(|e| CliError::Other(format!("failed to parse response: {e}")))?;
        Ok(value)
    }

    /// GET /api/v1/ticks/:id -> TickRecord or None (404).
    pub async fn tick(&self, tick_id: &str) -> Result<Option<serde_json::Value>, CliError> {
        let path = format!("/api/v1/ticks/{tick_id}");
        let resp = self.get_raw_allow_404(&path).await?;
        match resp {
            Some(r) => {
                let value = serde_json::from_str(&r.body)
                    .map_err(|e| CliError::Other(format!("failed to parse response: {e}")))?;
                Ok(Some(value))
            }
            None => Ok(None),
        }
    }

    /// GET /api/v1/threads — returns all registered threads with status.
    pub async fn threads(&self) -> Result<Vec<serde_json::Value>, CliError> {
        let resp = self.get_raw("/api/v1/threads").await?;
        let value = serde_json::from_str(&resp.body)
            .map_err(|e| CliError::Other(format!("failed to parse response: {e}")))?;
        Ok(value)
    }

    /// GET /api/v1/relationships -> RelationshipSnapshot.
    pub async fn relationships(&self) -> Result<serde_json::Value, CliError> {
        let resp = self.get_raw("/api/v1/relationships").await?;
        let value = serde_json::from_str(&resp.body)
            .map_err(|e| CliError::Other(format!("failed to parse response: {e}")))?;
        Ok(value)
    }

    /// GET /api/v1/relationships/:principal\_id?limit=N — returns relationship history.
    pub async fn relationship_history(
        &self,
        principal: &str,
        limit: usize,
    ) -> Result<Vec<serde_json::Value>, CliError> {
        let path = format!("/api/v1/relationships/{principal}?limit={limit}");
        let resp = self.get_raw(&path).await?;
        let value = serde_json::from_str(&resp.body)
            .map_err(|e| CliError::Other(format!("failed to parse response: {e}")))?;
        Ok(value)
    }

    /// GET /api/v1/budget -> InspectionBudgetStatus.
    pub async fn budget(&self) -> Result<serde_json::Value, CliError> {
        let resp = self.get_raw("/api/v1/budget").await?;
        let value = serde_json::from_str(&resp.body)
            .map_err(|e| CliError::Other(format!("failed to parse response: {e}")))?;
        Ok(value)
    }

    /// GET /api/v1/events?limit=N — returns recent events.
    pub async fn events(&self, limit: usize) -> Result<Vec<serde_json::Value>, CliError> {
        let path = format!("/api/v1/events?limit={limit}");
        let resp = self.get_raw(&path).await?;
        let value = serde_json::from_str(&resp.body)
            .map_err(|e| CliError::Other(format!("failed to parse response: {e}")))?;
        Ok(value)
    }

    /// GET /api/v1/engines -> EngineStatus.
    pub async fn engines(&self) -> Result<serde_json::Value, CliError> {
        let resp = self.get_raw("/api/v1/engines").await?;
        let value = serde_json::from_str(&resp.body)
            .map_err(|e| CliError::Other(format!("failed to parse response: {e}")))?;
        Ok(value)
    }

    /// POST /api/v1/inbox -> { envelope_id }.
    pub async fn send_message(
        &self,
        source: &str,
        content: &str,
    ) -> Result<serde_json::Value, CliError> {
        let url = format!("{}/api/v1/inbox", self.base_url);
        let body = serde_json::json!({
            "source": source,
            "content": content,
        });
        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| CliError::Connection(e.to_string()))?;

        let status = resp.status().as_u16();
        let text = resp
            .text()
            .await
            .unwrap_or_else(|_| String::from("<unreadable>"));

        if status >= 400 {
            return Err(CliError::DaemonError { status, body: text });
        }

        let value = serde_json::from_str(&text)
            .map_err(|e| CliError::Other(format!("failed to parse response: {e}")))?;
        Ok(value)
    }

    /// GET /api/v1/artifacts/:id -> Artifact or None (404).
    pub async fn artifact(&self, id: &str) -> Result<Option<serde_json::Value>, CliError> {
        let path = format!("/api/v1/artifacts/{id}");
        let resp = self.get_raw_allow_404(&path).await?;
        match resp {
            Some(r) => {
                let value = serde_json::from_str(&r.body)
                    .map_err(|e| CliError::Other(format!("failed to parse response: {e}")))?;
                Ok(Some(value))
            }
            None => Ok(None),
        }
    }

    /// GET /api/v1/memory?type=X&limit=N -> MemoryResponse.
    pub async fn memory(
        &self,
        memory_type: Option<&str>,
        limit: usize,
    ) -> Result<serde_json::Value, CliError> {
        let mut path = format!("/api/v1/memory?limit={limit}");
        if let Some(mt) = memory_type {
            path.push_str(&format!("&type={mt}"));
        }
        let resp = self.get_raw(&path).await?;
        let value = serde_json::from_str(&resp.body)
            .map_err(|e| CliError::Other(format!("failed to parse response: {e}")))?;
        Ok(value)
    }

    /// GET /api/v1/snapshots?limit=N -> Vec<StateSnapshot>.
    pub async fn snapshots(&self, limit: usize) -> Result<Vec<serde_json::Value>, CliError> {
        let path = format!("/api/v1/snapshots?limit={limit}");
        let resp = self.get_raw(&path).await?;
        let value = serde_json::from_str(&resp.body)
            .map_err(|e| CliError::Other(format!("failed to parse response: {e}")))?;
        Ok(value)
    }

    /// GET /api/v1/snapshots/at/:tick -> StateSnapshot or None (404).
    pub async fn snapshot_at(&self, tick: u64) -> Result<Option<serde_json::Value>, CliError> {
        let path = format!("/api/v1/snapshots/at/{tick}");
        let resp = self.get_raw_allow_404(&path).await?;
        match resp {
            Some(r) => {
                let value = serde_json::from_str(&r.body)
                    .map_err(|e| CliError::Other(format!("failed to parse response: {e}")))?;
                Ok(Some(value))
            }
            None => Ok(None),
        }
    }

    /// GET /api/v1/inbox/history?limit=N -> Vec<InboxHistoryEntry>.
    pub async fn inbox_history(&self, limit: usize) -> Result<Vec<serde_json::Value>, CliError> {
        let path = format!("/api/v1/inbox/history?limit={limit}");
        let resp = self.get_raw(&path).await?;
        let value = serde_json::from_str(&resp.body)
            .map_err(|e| CliError::Other(format!("failed to parse response: {e}")))?;
        Ok(value)
    }

    /// POST /api/v1/charters/reload -> reload result.
    pub async fn reload_charters(&self) -> Result<serde_json::Value, CliError> {
        let url = format!("{}/api/v1/charters/reload", self.base_url);
        let resp = self
            .client
            .post(&url)
            .send()
            .await
            .map_err(|e| CliError::Connection(e.to_string()))?;

        let status = resp.status().as_u16();
        let text = resp
            .text()
            .await
            .unwrap_or_else(|_| String::from("<unreadable>"));

        if status >= 400 {
            return Err(CliError::DaemonError { status, body: text });
        }

        let value = serde_json::from_str(&text)
            .map_err(|e| CliError::Other(format!("failed to parse response: {e}")))?;
        Ok(value)
    }

    /// GET /api/v1/config -> SanitizedConfig.
    pub async fn config(&self) -> Result<serde_json::Value, CliError> {
        let resp = self.get_raw("/api/v1/config").await?;
        let value = serde_json::from_str(&resp.body)
            .map_err(|e| CliError::Other(format!("failed to parse response: {e}")))?;
        Ok(value)
    }

    // ── Internal helpers ──

    /// Issue a GET and return status + body text. Errors on non-2xx.
    async fn get_raw(&self, path: &str) -> Result<RawResponse, CliError> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| CliError::Connection(e.to_string()))?;

        let status = resp.status().as_u16();
        let body = resp
            .text()
            .await
            .unwrap_or_else(|_| String::from("<unreadable>"));

        if status >= 400 {
            return Err(CliError::DaemonError { status, body });
        }

        Ok(RawResponse { status, body })
    }

    /// Issue a GET, returning `Ok(None)` on 404 instead of an error.
    async fn get_raw_allow_404(&self, path: &str) -> Result<Option<RawResponse>, CliError> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| CliError::Connection(e.to_string()))?;

        let status = resp.status().as_u16();
        let body = resp
            .text()
            .await
            .unwrap_or_else(|_| String::from("<unreadable>"));

        if status == 404 {
            return Ok(None);
        }
        if status >= 400 {
            return Err(CliError::DaemonError { status, body });
        }

        Ok(Some(RawResponse { status, body }))
    }
}

/// Internal type for a raw HTTP response (status + body text).
struct RawResponse {
    status: u16,
    body: String,
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncWriteExt;

    use super::*;

    // ── E0-T32: Connection refused returns CliError::Connection ──

    #[tokio::test]
    async fn connection_refused_returns_cli_error_connection() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener); // nothing listening

        let client = DaemonClient::new(&format!("http://127.0.0.1:{port}"));
        let result = client.status().await;

        assert!(
            matches!(result, Err(CliError::Connection(_))),
            "expected CliError::Connection, got: {result:?}"
        );
    }

    // ── E0-T33: Server returns 500 produces CliError::DaemonError ──

    #[tokio::test]
    async fn server_500_returns_daemon_error() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let response =
                "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 14\r\n\r\nserver failure";
            stream.write_all(response.as_bytes()).await.unwrap();
        });

        let client = DaemonClient::new(&format!("http://127.0.0.1:{port}"));
        let result = client.status().await;

        match result {
            Err(CliError::DaemonError { status, body }) => {
                assert_eq!(status, 500);
                assert!(body.contains("server failure"), "body was: {body}");
            }
            other => panic!("expected CliError::DaemonError, got: {other:?}"),
        }
    }

    // ── E0-T34: 404 on allow_404 endpoint returns Ok(None) ──

    #[tokio::test]
    async fn not_found_on_allow_404_endpoint_returns_none() {
        // Test 1: 404 returns Ok(None)
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let response = "HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\n\r\nnot found";
            stream.write_all(response.as_bytes()).await.unwrap();
        });

        let client = DaemonClient::new(&format!("http://127.0.0.1:{port}"));
        let result = client.tick("nonexistent-id").await;
        assert!(
            matches!(result, Ok(None)),
            "expected Ok(None) for 404, got: {result:?}"
        );

        // Test 2: 500 on same method still returns error (not swallowed)
        let listener2 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port2 = listener2.local_addr().unwrap().port();

        tokio::spawn(async move {
            let (mut stream, _) = listener2.accept().await.unwrap();
            let response = "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 5\r\n\r\nerror";
            stream.write_all(response.as_bytes()).await.unwrap();
        });

        let client2 = DaemonClient::new(&format!("http://127.0.0.1:{port2}"));
        let result2 = client2.tick("nonexistent-id").await;
        assert!(
            matches!(result2, Err(CliError::DaemonError { status: 500, .. })),
            "expected DaemonError for 500 on allow_404 path, got: {result2:?}"
        );
    }
}
