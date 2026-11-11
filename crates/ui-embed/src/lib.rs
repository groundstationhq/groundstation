//! Serves the embedded UI on the same address as the API.
//!
//! The UI is a static bundle that only ever talks to `/v1/*` on the origin
//! that served it, so serving it from gsd or the hosted api needs no proxy and
//! no CORS, and both serve the same bundle. Every GET the host doesn't route
//! falls through to [`serve`], and [`no_store`] keeps everything but the
//! bundle's files out of caches.

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderValue, Method, StatusCode, Uri, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "$OUT_DIR/ui"]
struct Assets;

/// Whether this binary carries the real UI (`ui/dist` at build time) or the placeholder.
pub const EMBEDDED: bool = matches!(env!("GS_UI_EMBEDDED").as_bytes(), b"1");

const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'";

/// For the bundle's content-hashed files: a new build gets new names.
const IMMUTABLE: &str = "public, max-age=31536000, immutable";

pub async fn serve(method: Method, uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path.starts_with("v1/") || path == "v1" {
        return (StatusCode::NOT_FOUND, "no such endpoint").into_response();
    }
    if method != Method::GET && method != Method::HEAD {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            [(header::ALLOW, "GET, HEAD")],
        )
            .into_response();
    }
    let found = Assets::get(path).map(|file| (path, file)).or_else(|| {
        // Hash-routed app: the root and any extension-less path is the app shell.
        (!path.contains('.'))
            .then(|| Assets::get("index.html"))
            .flatten()
            .map(|file| ("index.html", file))
    });
    let Some((name, file)) = found else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mime = file.metadata.mimetype();
    // The shell is fetched again on every load anyway; storing it would only
    // let a browser show it from its caches after someone signs out.
    let cache = if name.starts_with("assets/") {
        IMMUTABLE
    } else {
        "no-store"
    };
    // For HEAD, axum sets Content-Length from this body, then drops it.
    let mut resp = Response::new(Body::from(file.data.into_owned()));
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

/// Middleware for a host that serves the UI: every answer except the bundle's
/// own files (which say how to cache themselves) is `no-store`, so neither a
/// shared cache nor the browser keeps what one session read for the next. On
/// the hosted api that is a tenant's trajectories, prompts included.
pub async fn no_store(req: Request, next: Next) -> Response {
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    h.entry(header::CACHE_CONTROL)
        .or_insert(HeaderValue::from_static("no-store"));
    h.entry(header::X_CONTENT_TYPE_OPTIONS)
        .or_insert(HeaderValue::from_static("nosniff"));
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::http::HeaderMap;
    use axum::middleware;
    use axum::routing::get;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    /// The UI as a host mounts it, behind a route of its own.
    fn app() -> Router {
        Router::new()
            .route("/v1/health", get(|| async { "{}" }))
            .fallback(serve)
            .layer(middleware::from_fn(no_store))
    }

    async fn send(method: Method, uri: &str) -> (StatusCode, HeaderMap, Vec<u8>) {
        let req = axum::http::Request::builder()
            .method(method)
            .uri(uri)
            .body(Body::empty())
            .unwrap();
        let resp = app().oneshot(req).await.unwrap();
        let (status, headers) = (resp.status(), resp.headers().clone());
        let body = resp
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec();
        (status, headers, body)
    }

    fn index() -> String {
        String::from_utf8(Assets::get("index.html").unwrap().data.into_owned()).unwrap()
    }

    #[test]
    fn the_ui_is_compiled_in() {
        // In debug builds too: a file read from disk at request time would be
        // whatever the last build of any crate left there.
        assert!(matches!(
            Assets::get("index.html").unwrap().data,
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[tokio::test]
    async fn root_serves_the_app_shell() {
        let (status, h, body) = send(Method::GET, "/").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            h[header::CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with("text/html")
        );
        assert_eq!(h[header::X_FRAME_OPTIONS], "DENY");
        assert_eq!(h[header::CACHE_CONTROL], "no-store");
        assert_eq!(body, index().as_bytes());
    }

    #[tokio::test]
    async fn extensionless_paths_fall_back_to_the_shell() {
        for path in ["/trajectories", "/auth/nope", "/assets"] {
            let (status, _, body) = send(Method::GET, path).await;
            assert_eq!(status, StatusCode::OK, "{path}");
            assert_eq!(body, index().as_bytes(), "{path}");
        }
    }

    #[tokio::test]
    async fn head_answers_like_get_without_a_body() {
        let (_, get_headers, get_body) = send(Method::GET, "/").await;
        let (status, h, body) = send(Method::HEAD, "/").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.is_empty());
        assert_eq!(h[header::CONTENT_LENGTH], get_body.len().to_string());
        assert_eq!(h[header::CONTENT_TYPE], get_headers[header::CONTENT_TYPE]);
    }

    #[tokio::test]
    async fn unknown_files_and_api_paths_are_404() {
        for path in ["/nope.js", "/v1/nothing", "/assets/../index.html"] {
            assert_eq!(
                send(Method::GET, path).await.0,
                StatusCode::NOT_FOUND,
                "{path}"
            );
        }
    }

    #[tokio::test]
    async fn other_methods_are_refused() {
        let (status, h, _) = send(Method::POST, "/").await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(h[header::ALLOW], "GET, HEAD");
    }

    #[tokio::test]
    async fn only_the_bundle_is_cached() {
        let (_, h, _) = send(Method::GET, "/v1/health").await;
        assert_eq!(h[header::CACHE_CONTROL], "no-store");
        assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        let (_, h, _) = send(Method::GET, "/v1/nothing").await;
        assert_eq!(h[header::CACHE_CONTROL], "no-store");
    }

    #[tokio::test]
    async fn the_shell_links_only_files_that_are_there() {
        let index = index();
        let linked: Vec<&str> = index
            .split('"')
            .filter(|s| s.starts_with("/assets/"))
            .collect();
        if !EMBEDDED {
            assert!(linked.is_empty());
            assert!(index.contains("npm run build"), "{index}");
            return;
        }
        assert!(!linked.is_empty(), "{index}");
        for path in linked {
            let (status, h, body) = send(Method::GET, path).await;
            assert_eq!(status, StatusCode::OK, "{path}");
            assert_eq!(h[header::CACHE_CONTROL], IMMUTABLE, "{path}");
            assert!(
                !h[header::CONTENT_TYPE]
                    .to_str()
                    .unwrap()
                    .starts_with("text/html"),
                "{path}"
            );
            assert!(!body.is_empty(), "{path}");
        }
    }
}
