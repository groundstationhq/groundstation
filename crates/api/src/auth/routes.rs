//! `/auth/*`: signing in and out. Outside `/v1` because it isn't part of the
//! data contract; the UI only links here.

use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Form, Router};
use chrono::Duration;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::oidc::OidcError;
use super::pages::{self, Login};
use super::{
    cookie, login_cookie, login_cookie_name, safe_return, session_cookie, session_cookie_name,
};
use crate::AppState;
use crate::accounts::{self, Identity, SignedIn};
use crate::sessions::{self, Flow};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/auth/login", get(login_page).post(password_login))
        .route("/auth/oidc", get(oidc_start))
        .route("/auth/callback", get(oidc_callback))
        .route("/auth/logout", get(logout).post(sign_out))
        .merge(super::cli::routes())
}

#[derive(Deserialize)]
struct LoginParams {
    return_to: Option<String>,
    error: Option<String>,
    signed_out: Option<String>,
}

async fn login_page(State(s): State<AppState>, Query(q): Query<LoginParams>) -> Response {
    let return_to = safe_return(q.return_to.as_deref());
    let password = s.config.sign_in.admin_password.is_some();
    // With a provider as the only way in, skip straight to it, unless the
    // person just signed out: the provider would sign them straight back in.
    if !password && q.error.is_none() && q.signed_out.is_none() && s.oidc.is_some() {
        return Redirect::to(&format!(
            "/auth/oidc?return_to={}",
            pages::encode(&return_to)
        ))
        .into_response();
    }
    let error = q.error.as_deref().map(|e| match e {
        "password" => "That password isn't right.",
        _ => "Signing in didn't work. Try again.",
    });
    pages::login(&Login {
        return_to: &return_to,
        error,
        notice: q.signed_out.is_some().then_some("You're signed out."),
        password,
        provider: s.oidc.as_ref().map(|o| o.name.as_str()),
    })
}

#[derive(Deserialize)]
struct PasswordForm {
    password: String,
    return_to: Option<String>,
}

async fn password_login(
    State(s): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<PasswordForm>,
) -> Response {
    let return_to = safe_return(form.return_to.as_deref());
    let Some(expected) = &s.config.sign_in.admin_password else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !s.password_attempts.take(std::time::Instant::now()) {
        tracing::warn!("admin password sign-in refused: too many wrong passwords");
        let mut response = pages::message(
            "Too many attempts",
            "Too many wrong passwords. Wait a minute, then try again.",
        );
        *response.status_mut() = StatusCode::TOO_MANY_REQUESTS;
        return response;
    }
    if !same_secret(&form.password, expected) {
        tracing::warn!("failed admin password sign-in");
        let to = format!(
            "/auth/login?error=password&return_to={}",
            pages::encode(&return_to)
        );
        return Redirect::to(&to).into_response();
    }
    s.password_attempts.give_back();
    signed_in(&s, &headers, &Identity::admin(), &return_to).await
}

/// Compares in time independent of where the inputs differ.
fn same_secret(given: &str, expected: &str) -> bool {
    let (a, b) = (Sha256::digest(given), Sha256::digest(expected));
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

#[derive(Deserialize)]
struct StartParams {
    return_to: Option<String>,
}

async fn oidc_start(State(s): State<AppState>, Query(q): Query<StartParams>) -> Response {
    let Some(oidc) = &s.oidc else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let start = match oidc.start().await {
        Ok(start) => start,
        Err(e) => return provider_failed(&e),
    };
    let flow = Flow {
        pkce_verifier: start.pkce_verifier,
        nonce: start.nonce,
        return_to: safe_return(q.return_to.as_deref()),
    };
    if let Err(e) = sessions::save_flow(&s.pg, &start.state, &flow).await {
        tracing::error!("saving a sign-in: {e}");
        return unavailable();
    }
    let mut response = Redirect::to(&start.url).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        login_cookie(Some(&start.state), s.config.https()),
    );
    response
}

#[derive(Deserialize)]
struct Callback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

async fn oidc_callback(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<Callback>,
) -> Response {
    let Some(oidc) = &s.oidc else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if q.error.is_some() {
        return Redirect::to("/auth/login?error=provider").into_response();
    }
    let (Some(code), Some(state)) = (q.code, q.state) else {
        return failed("The identity provider sent an incomplete answer.");
    };
    // Only the browser that started a sign-in may finish it. Otherwise a
    // link to someone else's finished sign-in would sign you in as them.
    if cookie(&headers, login_cookie_name(s.config.https())) != Some(state.as_str()) {
        return failed("This sign-in was started in another browser, or expired. Start again.");
    }
    let flow = match sessions::take_flow(&s.pg, &state).await {
        Ok(Some(flow)) => flow,
        Ok(None) => return failed("This sign-in expired or was already used. Start again."),
        Err(e) => {
            tracing::error!("reading a sign-in: {e}");
            return unavailable();
        }
    };
    match oidc.finish(code, &flow).await {
        Ok(identity) => {
            let mut response = signed_in(&s, &headers, &identity, &flow.return_to).await;
            response
                .headers_mut()
                .append(header::SET_COOKIE, login_cookie(None, s.config.https()));
            response
        }
        Err(e) => provider_failed(&e),
    }
}

async fn signed_in(
    s: &AppState,
    headers: &HeaderMap,
    identity: &Identity,
    return_to: &str,
) -> Response {
    let ttl = Duration::days(s.config.session_days);
    let session = async {
        // A session this browser had before ends here: one copied from it
        // earlier mustn't outlive this sign-in.
        if let Some(old) = cookie(headers, session_cookie_name(s.config.https())) {
            sessions::delete(&s.pg, old).await?;
        }
        match accounts::sign_in(&s.pg, identity, s.signup()).await? {
            SignedIn::Member(user, tenant) => {
                sessions::create(&s.pg, user, tenant, ttl).await.map(Some)
            }
            SignedIn::Suspended => Ok(None),
        }
    };
    let id = match session.await {
        Ok(Some(id)) => id,
        Ok(None) => {
            let mut response = pages::message(
                "Workspace suspended",
                "Your Ground Station workspace is suspended. Ask its administrator.",
            );
            *response.status_mut() = StatusCode::FORBIDDEN;
            return response;
        }
        Err(e) => {
            tracing::error!("signing in: {e}");
            return unavailable();
        }
    };
    let mut response = Redirect::to(return_to).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        session_cookie(Some(&id), ttl.num_seconds(), s.config.https()),
    );
    response
}

/// `GET /auth/logout`: the UI's Sign out link. A link on another site could
/// point here too, and browsers say where a navigation came from: one from
/// elsewhere gets a button to confirm, whose post must come from this origin.
async fn logout(State(s): State<AppState>, headers: HeaderMap) -> Response {
    // At once only when the browser says this site's own page, or nobody's
    // (an address typed in), sent it. A browser that doesn't say could be
    // following a link or an image on any site, so it gets the question.
    let site = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok());
    if !site.is_some_and(|site| matches!(site, "same-origin" | "none")) {
        return pages::sign_out();
    }
    sign_out(State(s), headers).await
}

/// `POST /auth/logout`, from the confirmation page.
async fn sign_out(State(s): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(id) = cookie(&headers, session_cookie_name(s.config.https()))
        && let Err(e) = sessions::delete(&s.pg, id).await
    {
        tracing::error!("ending a session: {e}");
    }
    let mut response = Redirect::to("/auth/login?signed_out=1").into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        session_cookie(None, 0, s.config.https()),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// A sign-in that can't finish as sent: start again.
fn failed(text: &str) -> Response {
    let mut response = pages::message("Couldn't sign in", text);
    *response.status_mut() = StatusCode::BAD_REQUEST;
    response
}

/// The provider couldn't be reached or refused the sign-in. Its own words go
/// to the log, not to whoever is signing in.
fn provider_failed(e: &OidcError) -> Response {
    tracing::warn!("sign-in with the identity provider failed: {e}");
    let (status, text) = match e {
        OidcError::Discovery(_) => (
            StatusCode::BAD_GATEWAY,
            "The identity provider can't be reached. Try again in a minute.",
        ),
        OidcError::NotAllowed => (
            StatusCode::FORBIDDEN,
            "This account isn't allowed to sign in here.",
        ),
        OidcError::Exchange(_) | OidcError::Verification(_) => (
            StatusCode::BAD_REQUEST,
            "The identity provider's answer didn't check out. Start again.",
        ),
    };
    let mut response = pages::message("Couldn't sign in", text);
    *response.status_mut() = status;
    response
}

fn unavailable() -> Response {
    let mut response = pages::message(
        "Couldn't sign in",
        "Ground Station's database is unavailable. Try again in a minute.",
    );
    *response.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_secrets_exactly() {
        assert!(same_secret(
            "correct horse battery",
            "correct horse battery"
        ));
        assert!(!same_secret(
            "correct horse batter",
            "correct horse battery"
        ));
        assert!(!same_secret("", "x"));
    }
}
