//! Who is making a request.
//!
//! Browsers carry an `HttpOnly` session cookie set at sign-in. [`Viewer`] is
//! the extractor every `/v1` handler takes: no live session, no data (401,
//! which the UI turns into its sign-in screen). Requests that change anything
//! must come from the api's own origin (see [`same_origin`]).

mod cli;
pub mod oidc;
mod pages;
mod routes;
mod throttle;

use axum::extract::{FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
pub use pages::{escape, message, page_response};
pub use routes::routes;
pub use throttle::Throttle;

use crate::AppState;
use crate::error::ApiError;
pub use crate::sessions::Viewer;

/// The session cookie's name. Behind https it carries the `__Host-` prefix:
/// browsers then take it only from this host, over https, for the whole
/// site, so no other subdomain or path can plant one that shadows it.
pub fn session_cookie_name(https: bool) -> &'static str {
    if https {
        "__Host-gs_session"
    } else {
        "gs_session"
    }
}

/// Holds the `state` of a sign-in with the provider while the browser is
/// there, so only the browser that started it can finish it.
pub fn login_cookie_name(https: bool) -> &'static str {
    if https { "__Host-gs_login" } else { "gs_login" }
}

impl FromRequestParts<AppState> for Viewer {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, s: &AppState) -> Result<Self, ApiError> {
        let unauthorized = || ApiError::new(StatusCode::UNAUTHORIZED, "sign in required");
        let id = cookie(&parts.headers, session_cookie_name(s.config.https()))
            .ok_or_else(unauthorized)?;
        match crate::sessions::viewer(&s.pg, id).await? {
            Some(crate::sessions::Session::Live(viewer)) => Ok(viewer),
            Some(crate::sessions::Session::Suspended) => Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "this workspace is suspended",
            )),
            None => Err(unauthorized()),
        }
    }
}

pub fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}

/// `Set-Cookie` for a session; `None` clears it.
pub fn session_cookie(id: Option<&str>, max_age_secs: i64, https: bool) -> HeaderValue {
    let (value, max_age) = match id {
        Some(id) => (id, max_age_secs),
        None => ("", 0),
    };
    let name = session_cookie_name(https);
    let secure = if https { "; Secure" } else { "" };
    let cookie =
        format!("{name}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}");
    HeaderValue::from_str(&cookie).expect("session ids are URL-safe")
}

/// `Set-Cookie` for a sign-in in progress; `None` clears it. `Lax`, because
/// the provider sends the browser back with a cross-site navigation. Only
/// `/auth` needs it, but a `__Host-` cookie must be for the whole site.
pub fn login_cookie(state: Option<&str>, https: bool) -> HeaderValue {
    let (value, max_age) = match state {
        Some(state) => (state, 600),
        None => ("", 0),
    };
    let name = login_cookie_name(https);
    let attributes = if https {
        "Path=/; Secure"
    } else {
        "Path=/auth"
    };
    let cookie = format!("{name}={value}; {attributes}; HttpOnly; SameSite=Lax; Max-Age={max_age}");
    HeaderValue::from_str(&cookie).expect("states are URL-safe")
}

/// Behind https, tells browsers to use nothing else for this host for a
/// year, so no one on the network can answer a plain-http request for it
/// (and set cookies from that answer).
pub async fn https_only(State(s): State<AppState>, req: Request, next: Next) -> Response {
    let mut response = next.run(req).await;
    if s.config.https() {
        response.headers_mut().insert(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static("max-age=31536000"),
        );
    }
    response
}

/// Refuses requests that change state unless the browser says they come from
/// this origin. With the session cookie `SameSite=Lax` and JSON-only `/v1`
/// writes (which need a CORS preflight the api never grants), this closes
/// cross-site request forgery. The CLI collecting its token is no browser
/// and acts on no cookie (see [`cli::TOKEN_PATH`]).
pub async fn same_origin(State(s): State<AppState>, req: Request, next: Next) -> Response {
    let safe = matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS)
        || req.uri().path() == cli::TOKEN_PATH;
    let origin = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok());
    if !safe && origin != Some(s.config.origin()) {
        return ApiError::new(
            StatusCode::FORBIDDEN,
            groundstation_wire::CROSS_ORIGIN_REFUSED,
        )
        .into_response();
    }
    next.run(req).await
}

/// A path on this site to go back to after signing in, never another site.
/// Browsers drop tabs and newlines from a URL and read `\` as `/`, so either
/// could turn a path into `//another.site`.
pub fn safe_return(to: Option<&str>) -> String {
    match to {
        Some(p)
            if p.starts_with('/')
                && !p.starts_with("//")
                && !p.contains('\\')
                && !p.chars().any(char::is_control) =>
        {
            p.to_string()
        }
        _ => "/".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_session_cookie_among_others() {
        let mut h = HeaderMap::new();
        h.append(header::COOKIE, "a=1; gs_session=gss_x".parse().unwrap());
        h.append(header::COOKIE, "b=2".parse().unwrap());
        assert_eq!(cookie(&h, session_cookie_name(false)), Some("gss_x"));
        assert_eq!(cookie(&h, session_cookie_name(true)), None);
        assert_eq!(cookie(&h, "nope"), None);
    }

    #[test]
    fn session_cookies_are_http_only_and_host_only_on_https() {
        let set = session_cookie(Some("gss_x"), 60, true);
        let set = set.to_str().unwrap();
        // What browsers require of a __Host- cookie: Secure, Path=/, no Domain.
        assert!(set.starts_with("__Host-gs_session=gss_x; Path=/;"), "{set}");
        assert!(
            set.contains("HttpOnly") && set.contains("SameSite=Lax") && set.ends_with("Secure")
        );
        assert!(!set.contains("Domain"));
        let login = login_cookie(Some("st"), true);
        let login = login.to_str().unwrap();
        assert!(
            login.starts_with("__Host-gs_login=st; Path=/; Secure;"),
            "{login}"
        );
        assert!(
            session_cookie(None, 60, false)
                .to_str()
                .unwrap()
                .starts_with("gs_session=; Path=/;")
        );
    }

    #[test]
    fn only_returns_to_this_site() {
        assert_eq!(safe_return(Some("/#/t/abc")), "/#/t/abc");
        for bad in [
            "//evil.example",
            "https://evil.example",
            "/\\evil",
            "/\t/evil.example",
            "/\n/evil.example",
            "/a\rb",
            "",
        ] {
            assert_eq!(safe_return(Some(bad)), "/", "{bad}");
        }
        assert_eq!(safe_return(None), "/");
    }
}
