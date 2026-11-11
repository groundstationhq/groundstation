//! Helpers for the api's tests against the databases from `docker-compose.yml`.
#![allow(dead_code)]

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use chrono::Duration;
use clap::Parser;
use groundstation_api::accounts::{self, Identity, Signup};
use groundstation_api::config::NewUsers;
use groundstation_api::{AppState, Config, migrate, router, sessions};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;
use uuid::Uuid;

pub const PASSWORD: &str = "correct horse battery";
pub const ORIGIN: &str = "http://127.0.0.1:4321";

pub async fn app() -> (Router, AppState) {
    let config = Config::parse_from(["api", "--admin-password", PASSWORD]);
    migrate(&config).await.unwrap();
    let state = AppState::new(config).unwrap();
    (router(state.clone()), state)
}

pub async fn send(app: &Router, req: Request<Body>) -> (StatusCode, HeaderMap, Value) {
    let res = app.clone().oneshot(req).await.unwrap();
    let (status, headers) = (res.status(), res.headers().clone());
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    (status, headers, body)
}

pub fn get(path: &str, session: Option<&str>) -> Request<Body> {
    let mut req = Request::get(path);
    if let Some(id) = session {
        req = req.header(header::COOKIE, format!("gs_session={id}"));
    }
    req.body(Body::empty()).unwrap()
}

/// A same-origin request with a JSON body, as the UI sends it.
pub fn json(method: &str, path: &str, session: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header(header::COOKIE, format!("gs_session={session}"))
        .header(header::ORIGIN, ORIGIN)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// A new person, signed in. Returns their tenant and session id.
pub async fn person(state: &AppState, new_users: NewUsers) -> (Uuid, String) {
    let identity = Identity {
        issuer: "https://idp.test".into(),
        subject: Uuid::now_v7().to_string(),
        email: Some("ana@acme.test".into()),
        name: None,
    };
    let signup = Signup {
        new_users,
        retention_days: 30,
    };
    let accounts::SignedIn::Member(user, tenant) = accounts::sign_in(&state.pg, &identity, signup)
        .await
        .unwrap()
    else {
        panic!("a new person's tenant isn't suspended");
    };
    let session = sessions::create(&state.pg, user, tenant, Duration::hours(1))
        .await
        .unwrap();
    (tenant, session)
}
