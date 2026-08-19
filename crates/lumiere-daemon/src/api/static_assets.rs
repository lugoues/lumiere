use axum::Router;

/// True when the request path names a file rather than a client route.
///
/// Missing assets must produce a real 404: serving index.html in their place
/// hands the browser HTML under a script or wasm MIME type, which it rejects
/// with an opaque error far from the actual cause (a stale or missing bundle).
fn is_asset_path(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .is_some_and(|name| name.contains('.'))
}

#[cfg(not(feature = "embed-ui"))]
mod implementation {
    use std::path::PathBuf;

    use axum::{
        body::Body,
        extract::Request,
        http::{StatusCode, header},
        response::{IntoResponse, Response},
        routing::any,
    };
    use tower_http::services::ServeDir;

    use super::*;

    pub fn router() -> Router {
        let root = web_root();
        match std::fs::metadata(root.join("index.html")).and_then(|meta| meta.modified()) {
            Ok(modified) => {
                let age = modified.elapsed().map(|age| age.as_secs()).unwrap_or(0);
                println!(
                    "Web UI: {} (built {}m {}s ago; rebuild with: mise run ui)",
                    root.display(),
                    age / 60,
                    age % 60,
                );
            }
            Err(_) => println!(
                "Web UI: {} (no bundle; build with: mise run ui)",
                root.display()
            ),
        }
        // SPA deep links must serve index.html with a 200; not_found_service
        // would force the status to 404, while fallback preserves the status.
        Router::new().fallback_service(ServeDir::new(root).fallback(any(spa_fallback)))
    }

    async fn spa_fallback(request: Request<Body>) -> Response {
        if is_asset_path(request.uri().path()) {
            return (StatusCode::NOT_FOUND, "no such asset").into_response();
        }
        match tokio::fs::read(web_root().join("index.html")).await {
            Ok(bytes) => (
                [
                    (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                    (header::CACHE_CONTROL, "no-cache"),
                ],
                bytes,
            )
                .into_response(),
            Err(_) => (
                StatusCode::NOT_FOUND,
                "UI bundle not built; run: cargo xtask ui",
            )
                .into_response(),
        }
    }

    fn web_root() -> PathBuf {
        std::env::var_os("LUMIERE_WEB_ROOT").map_or_else(
            || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../dist/web"),
            PathBuf::from,
        )
    }
}

#[cfg(feature = "embed-ui")]
mod implementation {
    use axum::{
        body::Bytes,
        http::{HeaderValue, StatusCode, Uri, header},
        response::{IntoResponse, Response},
        routing::get,
    };
    use rust_embed::RustEmbed;

    use super::*;

    #[derive(RustEmbed)]
    #[folder = "$CARGO_MANIFEST_DIR/../../dist/web/"]
    struct WebAssets;

    pub fn router() -> Router {
        Router::new().fallback(get(asset))
    }

    async fn asset(uri: Uri) -> Response {
        let requested = uri.path().trim_start_matches('/');
        if let Some(file) = WebAssets::get(if requested.is_empty() {
            "index.html"
        } else {
            requested
        }) {
            return serve(requested, file);
        }
        if is_asset_path(requested) {
            return (StatusCode::NOT_FOUND, "no such asset").into_response();
        }
        match WebAssets::get("index.html") {
            Some(file) => serve("index.html", file),
            None => (
                StatusCode::NOT_FOUND,
                "UI bundle was not embedded; build with: cargo xtask dist",
            )
                .into_response(),
        }
    }

    fn serve(path: &str, file: rust_embed::EmbeddedFile) -> Response {
        let content_type = HeaderValue::from_str(file.metadata.mimetype())
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
        let cache_control = if path == "index.html" || path.is_empty() {
            HeaderValue::from_static("no-cache")
        } else {
            HeaderValue::from_static("public, max-age=31536000, immutable")
        };
        (
            [
                (header::CONTENT_TYPE, content_type),
                (header::CACHE_CONTROL, cache_control),
            ],
            Bytes::from(file.data.into_owned()),
        )
            .into_response()
    }
}

pub use implementation::router;

#[cfg(test)]
mod tests {
    use super::is_asset_path;

    #[test]
    fn asset_paths_are_distinguished_from_routes() {
        assert!(is_asset_path("/wasm/lumiere-ui.js"));
        assert!(is_asset_path("/assets/main-abc123.css"));
        assert!(!is_asset_path("/"));
        assert!(!is_asset_path("/some/deep/route"));
    }
}
