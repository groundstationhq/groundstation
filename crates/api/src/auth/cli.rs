//! `/auth/cli`: how `groundstation cloud login` gets an ingest token.
//!
//! The CLI listens on a loopback port and opens this page with a `state` and
//! the `challenge` of a secret it keeps. The person signs in if needed, sees
//! which tenant the computer will upload to, and allows it; the api sends the
//! browser back to the CLI's port with a one-time code. The CLI checks
//! `state`, so a page can't feed it another login's code, and collects its
//! token with the code and the secret (`POST /auth/cli/token`). The token
//! never passes through the browser, so it isn't left in its history.
//!
//! Someone else on the same computer who sees the approval URL (in `ps`, say)
//! could approve it for their own workspace. So with the token the CLI gets
//! the workspace's name and who approved, and shows them before it saves.

use axum::extract::rejection::{FormRejection, QueryRejection};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use groundstation_backend::pg::tenants;
use serde::{Deserialize, Serialize};

use super::pages::{self, encode, escape};
use super::{cookie, session_cookie_name};
use crate::AppState;
use crate::error::ApiError;
use crate::sessions::{self, Approval, Session, Viewer};
use crate::tokens;

/// The approval form posts here and then redirects to the CLI's port.
const CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; form-action 'self' http://127.0.0.1:*; frame-ancestors 'none'; base-uri 'none'";

/// Where the CLI collects its token. It sends no cookie and no `Origin`, and
/// what it sends (a one-time code and a secret) is nothing a cross-site
/// request could know, so [`super::same_origin`] lets it through.
pub const TOKEN_PATH: &str = "/auth/cli/token";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/auth/cli", get(page).post(approve))
        .route(TOKEN_PATH, post(collect))
}

#[derive(Deserialize)]
struct Request {
    port: u16,
    state: String,
    label: String,
    /// SHA-256, in hex, of the secret the CLI collects its token with.
    challenge: String,
}

impl Request {
    fn valid(&self) -> bool {
        self.port >= 1024
            && (16..=128).contains(&self.state.len())
            && self
                .state
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            && self.challenge.len() == 64
            && self
                .challenge
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            && tokens::check_label(&self.label).is_ok()
    }

    fn query(&self) -> String {
        format!(
            "port={}&state={}&label={}&challenge={}",
            self.port,
            encode(&self.state),
            encode(&self.label),
            self.challenge
        )
    }

    fn callback(&self, outcome: &str) -> String {
        format!(
            "http://127.0.0.1:{}/callback?state={}&{outcome}",
            self.port,
            encode(&self.state)
        )
    }
}

/// The person approving, if signed in; a page to show instead otherwise.
async fn signed_in(s: &AppState, headers: &HeaderMap) -> Result<Option<Viewer>, Box<Response>> {
    let Some(id) = cookie(headers, session_cookie_name(s.config.https())) else {
        return Ok(None);
    };
    match sessions::viewer(&s.pg, id).await {
        Ok(Some(Session::Live(viewer))) => Ok(Some(viewer)),
        Ok(Some(Session::Suspended)) => {
            let mut page = pages::message(
                "Couldn't connect",
                "Your Ground Station workspace is suspended. Ask its administrator.",
            );
            *page.status_mut() = StatusCode::FORBIDDEN;
            Err(Box::new(page))
        }
        Ok(None) => Ok(None),
        Err(e) => {
            tracing::error!("reading a session: {e}");
            Err(Box::new(unavailable()))
        }
    }
}

fn unavailable() -> Response {
    let mut page = pages::message(
        "Couldn't connect",
        "Ground Station's database is unavailable. Try again in a minute.",
    );
    *page.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
    page
}

async fn page(
    State(s): State<AppState>,
    headers: HeaderMap,
    q: Result<Query<Request>, QueryRejection>,
) -> Response {
    let Some(Query(req)) = q.ok().filter(|q| q.valid()) else {
        return bad_request();
    };
    let viewer = match signed_in(&s, &headers).await {
        Ok(Some(viewer)) => viewer,
        Ok(None) => {
            let back = format!("/auth/cli?{}", req.query());
            return Redirect::to(&format!("/auth/login?return_to={}", encode(&back)))
                .into_response();
        }
        Err(page) => return *page,
    };
    let tenant = match tenants::get(&s.pg, viewer.tenant_id).await {
        Ok(Some(t)) => t.name,
        Ok(None) => return bad_request(),
        Err(e) => {
            tracing::error!("reading a tenant: {e}");
            return unavailable();
        }
    };
    let body = format!(
        r#"<h1>Connect a computer</h1>
<p>The <code>groundstation</code> CLI on this computer asks to upload its agents' events to <strong>{tenant}</strong>, as “{label}”. It can't read anything back.</p>
<form method="post" action="/auth/cli">
<input type="hidden" name="port" value="{port}"><input type="hidden" name="state" value="{state}">
<input type="hidden" name="label" value="{label}"><input type="hidden" name="challenge" value="{challenge}">
<button class="btn" type="submit">Allow</button> <a class="btn" href="{cancel}">Cancel</a></form>"#,
        tenant = escape(&tenant),
        label = escape(&req.label),
        port = req.port,
        state = escape(&req.state),
        challenge = req.challenge,
        cancel = escape(&req.callback("error=denied")),
    );
    let mut response = pages::page_response("Connect a computer · Ground Station", &body);
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    response
}

async fn approve(
    State(s): State<AppState>,
    headers: HeaderMap,
    Form(req): Form<Request>,
) -> Response {
    if !req.valid() {
        return bad_request();
    }
    let viewer = match signed_in(&s, &headers).await {
        Ok(Some(viewer)) => viewer,
        // Signed out in another tab since: sign in, then allow again.
        Ok(None) => {
            let back = format!("/auth/cli?{}", req.query());
            return Redirect::to(&format!("/auth/login?return_to={}", encode(&back)))
                .into_response();
        }
        Err(page) => return *page,
    };
    let approval = Approval {
        user_id: viewer.user_id,
        tenant_id: viewer.tenant_id,
        label: req.label.trim().to_string(),
    };
    match sessions::save_approval(&s.pg, &approval, &req.challenge).await {
        Ok(code) => Redirect::to(&req.callback(&format!("code={}", encode(&code)))).into_response(),
        Err(e) => {
            tracing::error!("saving an approval: {e}");
            unavailable()
        }
    }
}

#[derive(Deserialize)]
struct Collect {
    code: String,
    verifier: String,
}

/// What the CLI gets for its code.
#[derive(Serialize)]
struct Collected {
    token: String,
    /// The tenant's name.
    workspace: String,
    /// Who allowed it: their email, or name.
    approved_by: Option<String>,
}

/// Creates the token the person approved, once, for the CLI that holds the
/// secret behind the approval's challenge.
async fn collect(State(s): State<AppState>, req: Result<Form<Collect>, FormRejection>) -> Response {
    let Ok(Form(req)) = req else {
        return ApiError::new(StatusCode::BAD_REQUEST, "send code and verifier as a form")
            .into_response();
    };
    match take(&s, &req.code, &req.verifier).await {
        Ok(collected) => Json(collected).into_response(),
        Err(e) => e.into_response(),
    }
}

/// Takes the approval and makes its token in one transaction: a failure on
/// the way leaves the approval for the CLI to ask with again.
async fn take(s: &AppState, code: &str, verifier: &str) -> Result<Collected, ApiError> {
    use groundstation_backend::Error;
    let refused = || {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "this approval expired or was already used; run groundstation cloud login again",
        )
    };
    let mut conn = s.pg.get().await.map_err(Error::from)?;
    let tx = conn.transaction().await.map_err(Error::from)?;
    let Some(approval) = sessions::take_approval(&tx, code, verifier).await? else {
        return Err(refused());
    };
    // They may have left the workspace, or it been suspended, since. Either
    // way the approval is spent.
    let Some(approver) = sessions::approver(&tx, &approval).await? else {
        tx.commit().await.map_err(Error::from)?;
        return Err(refused());
    };
    let viewer = match approver.session {
        Session::Live(viewer) => viewer,
        Session::Suspended => {
            tx.commit().await.map_err(Error::from)?;
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "this workspace is suspended",
            ));
        }
    };
    let created = tokens::issue_with(&tx, &viewer, &approval.label).await?;
    tx.commit().await.map_err(Error::from)?;
    tracing::info!(tenant = %viewer.tenant_id, token = %created.token.id, "created ingest token");
    Ok(Collected {
        token: created.secret,
        workspace: approver.workspace,
        approved_by: approver.who,
    })
}

fn bad_request() -> Response {
    let mut response = pages::message(
        "Couldn't connect",
        "This link is incomplete. Run groundstation cloud login again.",
    );
    *response.status_mut() = StatusCode::BAD_REQUEST;
    response
}
