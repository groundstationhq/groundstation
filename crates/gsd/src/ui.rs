//! Serves the embedded UI on the same address as the API.
//!
//! The UI is a static bundle that only ever talks to `/v1/*` on the origin
//! that served it, so serving it from gsd needs no proxy and no CORS, and the
//! same bundle can later be served by a hosted backend unchanged. Everything
//! outside `/v1` that is a GET falls through to here.

use axum::body::Body;
use axum::http::{HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "$OUT_DIR/ui"]
struct Assets;

/// Whether this binary carries the real UI (`ui/dist` at build time) or the placeholder.
pub const EMBEDDED: bool = matches!(env!("GSD_UI_EMBEDDED").as_bytes(), b"1");

const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'";

pub async fn serve(method: Method, uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path.starts_with("v1/") || path == "v1" {
        return (StatusCode::NOT_FOUND, "no such endpoint").into_response();
    }
    if method != Method::GET && method != Method::HEAD {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let (name, spa) = if path.is_empty() {
        ("index.html", true)
    } else if let Some(file) = Assets::get(path) {
        drop(file);
        (path, false)
    } else if !path.contains('.') {
        // Hash-routed app: any extension-less path is the app shell.
        ("index.html", true)
    } else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(file) = Assets::get(name) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mime = file.metadata.mimetype();
    let cache = if !spa && name.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    let body = if method == Method::HEAD {
        Body::empty()
    } else {
        Body::from(file.data.into_owned())
    };
    let mut resp = Response::new(body);
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(mime).unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn root_serves_the_app_shell() {
        let r = serve(Method::GET, "/".parse().unwrap()).await;
        assert_eq!(r.status(), StatusCode::OK);
        assert!(
            r.headers()[header::CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with("text/html")
        );
        assert_eq!(r.headers()[header::X_FRAME_OPTIONS], "DENY");
    }

    #[tokio::test]
    async fn extensionless_paths_fall_back_to_the_shell() {
        let r = serve(Method::GET, "/trajectories".parse().unwrap()).await;
        assert_eq!(r.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn unknown_files_and_api_paths_are_404() {
        assert_eq!(
            serve(Method::GET, "/nope.js".parse().unwrap())
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            serve(Method::GET, "/v1/nothing".parse().unwrap())
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            serve(Method::POST, "/".parse().unwrap()).await.status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
    }
}
