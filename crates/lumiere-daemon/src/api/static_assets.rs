use axum::Router;

#[cfg(not(feature = "embed-ui"))]
mod implementation {
    use std::path::PathBuf;

    use axum::{
        http::{StatusCode, header},
        response::{IntoResponse, Response},
        routing::any,
    };
    use tower_http::services::ServeDir;

    use super::*;

    pub fn router() -> Router {
        // SPA deep links must serve index.html with a 200; not_found_service
        // would force the status to 404, while fallback preserves the status.
        Router::new().fallback_service(ServeDir::new(web_root()).fallback(any(spa_index)))
    }

    async fn spa_index() -> Response {
        match tokio::fs::read(web_root().join("index.html")).await {
            Ok(bytes) => (
                [
                    (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                    (header::CACHE_CONTROL, "no-cache"),
                ],
                bytes,
            )
                .into_response(),
            Err(_) => (StatusCode::NOT_FOUND, "UI bundle not built").into_response(),
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
        http::{HeaderValue, Uri, header},
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
        let path = if requested.is_empty() {
            "index.html"
        } else {
            requested
        };
        let (path, file) = WebAssets::get(path)
            .map(|file| (path, file))
            .or_else(|| WebAssets::get("index.html").map(|file| ("index.html", file)))
            .expect("embedded UI must contain index.html");
        let content_type = HeaderValue::from_str(file.metadata.mimetype())
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
        let cache_control = if path == "index.html" {
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
