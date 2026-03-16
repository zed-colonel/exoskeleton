//! Embedded Observatory SPA serving.
//!
//! When the `embedded-observatory` feature is enabled, the daemon serves the
//! Observatory React SPA from compiled-in assets. API routes always take
//! precedence — the embedded handler is a fallback for non-API paths.

use axum::body::Body;
use axum::http::{header, Response, StatusCode, Uri};
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "../../observatory/dist/"]
struct ObservatoryAssets;

/// Serve an embedded static file, or fall back to index.html for SPA routing.
///
/// This is the raw handler — prefer [`serve_embedded_safe`] as the axum fallback
/// since it guards against intercepting API/operational paths.
pub async fn serve_embedded(uri: Uri) -> Response<Body> {
    let path = uri.path().trim_start_matches('/');

    // Try exact file match first
    if let Some(resp) = serve_file(path) {
        return resp;
    }

    // SPA fallback: serve index.html for all non-file paths
    // This enables client-side routing (/chat, /timeline, etc.)
    if let Some(resp) = serve_file("index.html") {
        return resp;
    }

    // Observatory not embedded or index.html missing
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Body::from("not found"))
        .unwrap()
}

/// Safe fallback handler for use with `Router::fallback()`.
///
/// In axum 0.7, handlers that return 404 on matched routes can still trigger
/// the fallback in certain configurations. This guard ensures API and
/// operational paths always return a proper 404 rather than the SPA page.
pub async fn serve_embedded_safe(uri: Uri) -> Response<Body> {
    let path = uri.path();
    if path.starts_with("/api/") || matches!(path, "/healthz" | "/ready" | "/metrics") {
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from("not found"))
            .unwrap();
    }
    serve_embedded(uri).await
}

fn serve_file(path: &str) -> Option<Response<Body>> {
    let asset = ObservatoryAssets::get(path)?;
    let mime = asset.metadata.mimetype();
    Some(
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, mime)
            .header(header::CACHE_CONTROL, cache_control(path))
            .body(Body::from(asset.data.into_owned()))
            .unwrap(),
    )
}

/// Cache policy: hash-named Vite assets get long-lived caching.
/// index.html and config.js are never cached (may change between deploys).
fn cache_control(path: &str) -> &'static str {
    if path == "index.html" || path == "config.js" {
        "no-cache, no-store, must-revalidate"
    } else if path.contains("/assets/") {
        "public, max-age=31536000, immutable" // 1 year for Vite hashed assets
    } else {
        "public, max-age=3600" // 1 hour for other files
    }
}

/// Generate a runtime config.js that points the Observatory to this daemon.
///
/// Unlike the Docker entrypoint script that writes config.js from an env var,
/// the embedded daemon knows its own address. When served from the same origin,
/// the Observatory connects to window.location.origin automatically (Tier 3
/// in AutoConnect). This endpoint returns an empty config so that Tier 3
/// triggers correctly.
pub async fn serve_config_js() -> Response<Body> {
    let body = "window.__OBSERVATORY_CONFIG__ = { defaultConnections: [] };";
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/javascript")
        .header(header::CACHE_CONTROL, "no-cache, no-store, must-revalidate")
        .body(Body::from(body))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    // E3-T1: serve_embedded returns index.html for /
    #[tokio::test]
    async fn serve_root_returns_index_html() {
        let resp = serve_embedded(Uri::from_static("/")).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp.headers().get(header::CONTENT_TYPE).unwrap();
        assert_eq!(ct, "text/html");
    }

    // E3-T2: serve_embedded returns correct MIME for .js file
    #[tokio::test]
    async fn serve_js_returns_correct_mime() {
        // Find any .js asset in the embedded files
        let js_file = ObservatoryAssets::iter().find(|f| f.ends_with(".js"));
        if let Some(path) = js_file {
            let uri_str = format!("/{path}");
            let uri: Uri = uri_str.parse().unwrap();
            let resp = serve_embedded(uri).await;
            assert_eq!(resp.status(), StatusCode::OK);
            let ct = resp
                .headers()
                .get(header::CONTENT_TYPE)
                .unwrap()
                .to_str()
                .unwrap();
            assert!(
                ct.contains("javascript"),
                "expected javascript MIME, got: {ct}"
            );
        } else {
            panic!("no .js files found in embedded assets — dist/ may be empty");
        }
    }

    // E3-T4: Unknown paths return index.html (SPA routing)
    #[tokio::test]
    async fn unknown_paths_return_index_html_for_spa() {
        for path in ["/chat", "/timeline", "/nonexistent/deep/path"] {
            let uri: Uri = path.parse().unwrap();
            let resp = serve_embedded(uri).await;
            assert_eq!(resp.status(), StatusCode::OK, "failed for path: {path}");
            let ct = resp.headers().get(header::CONTENT_TYPE).unwrap();
            assert_eq!(ct, "text/html", "failed for path: {path}");
        }
    }

    // E3-T5: serve_config_js returns runtime config
    #[tokio::test]
    async fn serve_config_js_returns_runtime_config() {
        let resp = serve_config_js().await;
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            ct.contains("javascript"),
            "expected javascript MIME, got: {ct}"
        );

        let cc = resp
            .headers()
            .get(header::CACHE_CONTROL)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(cc.contains("no-cache"), "config.js should not be cached");

        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(text.contains("__OBSERVATORY_CONFIG__"));
        assert!(text.contains("defaultConnections"));
    }
}
