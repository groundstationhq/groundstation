//! The few pages the api renders itself: signing in happens before the UI can
//! load, and the UI only talks to `/v1`.

use axum::http::{HeaderValue, header};
use axum::response::{Html, IntoResponse, Response};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

const CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'";

const STYLE: &str = "body{margin:0;min-height:100vh;display:grid;place-items:center;background:#0a0a0b;color:#a4a5ab;font:14px/1.6 system-ui,sans-serif}
main{width:100%;max-width:360px;padding:32px 16px;box-sizing:border-box}h1{color:#ededef;font-size:17px;margin:0 0 6px}
p{margin:0 0 18px}.err{color:#f87171}label{display:block;font-size:12px;margin:0 0 6px}
input{width:100%;box-sizing:border-box;padding:8px 10px;border-radius:6px;border:1px solid #2a2b30;background:#151619;color:#ededef;font:inherit}
.btn{display:inline-block;margin-top:12px;padding:7px 12px;border-radius:6px;border:1px solid #2a2b30;background:#151619;color:#ededef;font:inherit;text-decoration:none;cursor:pointer}
hr{border:0;border-top:1px solid #222;margin:22px 0}code{font-family:ui-monospace,Menlo,monospace;color:#ededef}";

/// Nothing about these pages reaches another site. Not `no-referrer`: from a
/// page with that policy browsers post forms with `Origin: null`, and the
/// same-origin check would refuse the page's own forms.
const REFERRER_POLICY: &str = "same-origin";

pub struct Login<'a> {
    pub return_to: &'a str,
    pub error: Option<&'a str>,
    pub notice: Option<&'a str>,
    pub password: bool,
    pub provider: Option<&'a str>,
}

pub fn login(page: &Login<'_>) -> Response {
    let mut body = String::from("<h1>Sign in to Ground Station</h1>");
    if let Some(error) = page.error {
        body += &format!(r#"<p class="err">{}</p>"#, escape(error));
    }
    if let Some(notice) = page.notice {
        body += &format!("<p>{}</p>", escape(notice));
    }
    let return_to = escape(page.return_to);
    if let Some(provider) = page.provider {
        body += &format!(
            r#"<a class="btn" href="/auth/oidc?return_to={}">Continue with {}</a>"#,
            escape(&encode(page.return_to)),
            escape(provider)
        );
    }
    if page.password {
        if page.provider.is_some() {
            body += "<hr>";
        }
        body += &format!(
            r#"<form method="post" action="/auth/login">
<input type="hidden" name="return_to" value="{return_to}">
<label for="password">Admin password</label>
<input id="password" name="password" type="password" autocomplete="current-password" required autofocus>
<button class="btn" type="submit">Sign in</button></form>"#
        );
    }
    page_response("Sign in · Ground Station", &body)
}

/// Asks before signing out, for a sign-out link followed from another site.
pub fn sign_out() -> Response {
    page_response(
        "Sign out · Ground Station",
        r#"<h1>Sign out of Ground Station?</h1>
<form method="post" action="/auth/logout"><button class="btn" type="submit">Sign out</button> <a class="btn" href="/">Stay signed in</a></form>"#,
    )
}

pub fn message(title: &str, text: &str) -> Response {
    let body = format!("<h1>{}</h1><p>{}</p>", escape(title), escape(text));
    page_response(title, &body)
}

/// A page whose body is already escaped HTML.
pub fn page_response(title: &str, body: &str) -> Response {
    let html = format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1"><meta name="color-scheme" content="dark">
<title>{}</title><style>{STYLE}</style></head><body><main>{body}</main></body></html>"#,
        escape(title)
    );
    let mut response = Html(html).into_response();
    let h = response.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static(REFERRER_POLICY),
    );
    response
}

pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Everything but letters, digits and `-_.~/`.
const ENCODED: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~')
    .remove(b'/');

/// Percent-encodes a query value. Slashes stay, so a path reads as one.
pub fn encode(value: &str) -> String {
    utf8_percent_encode(value, ENCODED).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_what_it_echoes() {
        assert_eq!(
            escape(r#"<a href="x">'&"#),
            "&lt;a href=&quot;x&quot;&gt;&#39;&amp;"
        );
        assert_eq!(encode("/t/a b?c=1"), "/t/a%20b%3Fc%3D1");
    }

    #[test]
    fn forms_on_these_pages_keep_their_origin() {
        let page = message("t", "b");
        assert_eq!(page.headers()[header::REFERRER_POLICY], "same-origin");
    }
}
