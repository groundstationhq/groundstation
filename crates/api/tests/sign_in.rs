//! Sessions as browsers hold them: signing out, signing in again, suspended
//! workspaces, and the cookies and headers behind https.
//!
//! `docker compose up -d --wait && cargo test -p groundstation-api -- --ignored`

mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use clap::Parser;
use common::{ORIGIN, PASSWORD, app, get, person, send};
use groundstation_api::config::NewUsers;
use groundstation_api::sessions::{self, Flow};
use groundstation_api::{AppState, Config, migrate, router};
use groundstation_backend::pg::tenants;
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

fn sign_out(
    method: &str,
    session: &str,
    site: Option<&str>,
    origin: Option<&str>,
) -> Request<Body> {
    let mut req = Request::builder()
        .method(method)
        .uri("/auth/logout")
        .header(header::COOKIE, format!("gs_session={session}"));
    if let Some(site) = site {
        req = req.header("sec-fetch-site", site);
    }
    if let Some(origin) = origin {
        req = req.header(header::ORIGIN, origin);
    }
    req.body(Body::empty()).unwrap()
}

async fn status_of(app: &Router, session: &str) -> StatusCode {
    send(app, get("/v1/health", Some(session))).await.0
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn a_sign_out_link_on_another_site_only_asks() {
    let (app, state) = app().await;
    let (_, session) = person(&state, NewUsers::Personal).await;

    // Followed from another site: a page to confirm, and the session lives.
    let (status, _, page) = send(&app, sign_out("GET", &session, Some("cross-site"), None)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        page.as_str()
            .unwrap()
            .contains(r#"method="post" action="/auth/logout""#)
    );
    assert_eq!(status_of(&app, &session).await, StatusCode::OK);
    // The confirmation must come from this origin, like any change.
    let (status, ..) = send(&app, sign_out("POST", &session, None, None)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(status_of(&app, &session).await, StatusCode::OK);
    let (status, headers, _) = send(&app, sign_out("POST", &session, None, Some(ORIGIN))).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers[header::LOCATION], "/auth/login?signed_out=1");
    assert_eq!(status_of(&app, &session).await, StatusCode::UNAUTHORIZED);

    // The UI's own link signs out at once.
    let (_, session) = person(&state, NewUsers::Personal).await;
    let (status, ..) = send(&app, sign_out("GET", &session, Some("same-origin"), None)).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(status_of(&app, &session).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn a_sign_out_link_from_a_browser_that_doesnt_say_where_from_only_asks() {
    let (app, state) = app().await;
    let (_, session) = person(&state, NewUsers::Personal).await;
    // Browsers without `Sec-Fetch-Site` (Safari before 16.4) follow an
    // <img src=".../auth/logout"> on any site the same way.
    let (status, _, page) = send(&app, sign_out("GET", &session, None, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        page.as_str()
            .unwrap()
            .contains(r#"method="post" action="/auth/logout""#)
    );
    assert_eq!(status_of(&app, &session).await, StatusCode::OK);
    // Typed into the address bar: no site sent it, so at once.
    let (status, ..) = send(&app, sign_out("GET", &session, Some("none"), None)).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(status_of(&app, &session).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn signing_in_again_ends_the_session_the_browser_had() {
    let (app, state) = app().await;
    let (_, old) = person(&state, NewUsers::Personal).await;
    let req = Request::post("/auth/login")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::ORIGIN, ORIGIN)
        .header(header::COOKIE, format!("gs_session={old}"))
        .body(Body::from(format!(
            "password={}",
            PASSWORD.replace(' ', "+")
        )))
        .unwrap();
    let (status, ..) = send(&app, req).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(status_of(&app, &old).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn a_suspended_workspace_says_so() {
    let (app, state) = app().await;
    let (tenant, session) = person(&state, NewUsers::Personal).await;
    tenants::set_suspended(&state.pg, tenant, true)
        .await
        .unwrap();
    // Not "signed out", which would send the UI to a sign-in that can't help.
    let (status, _, body) = send(&app, get("/v1/health", Some(&session))).await;
    assert_eq!(
        (status, &body),
        (
            StatusCode::FORBIDDEN,
            &json!({ "error": "this workspace is suspended" })
        )
    );
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn behind_https_cookies_are_host_only_and_browsers_stay_on_https() {
    let config = Config::parse_from([
        "api",
        "--admin-password",
        PASSWORD,
        "--public-url",
        "https://gs.example.com",
    ]);
    migrate(&config).await.unwrap();
    let app = router(AppState::new(config).unwrap());
    let req = Request::post("/auth/login")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::ORIGIN, "https://gs.example.com")
        .body(Body::from(format!(
            "password={}",
            PASSWORD.replace(' ', "+")
        )))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let headers: HeaderMap = res.headers().clone();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        headers[header::STRICT_TRANSPORT_SECURITY],
        "max-age=31536000"
    );
    let set = headers[header::SET_COOKIE].to_str().unwrap();
    assert!(
        set.starts_with("__Host-gs_session=")
            && set.contains("; Path=/;")
            && set.ends_with("Secure"),
        "{set}"
    );

    // Only the host-only cookie counts; a plain one planted beside it doesn't.
    let session = set.split(';').next().unwrap().to_string();
    let planted = format!("gs_session=gss_planted; {session}");
    for (cookie, expect) in [
        (session.as_str(), StatusCode::OK),
        ("gs_session=gss_planted", StatusCode::UNAUTHORIZED),
        (planted.as_str(), StatusCode::OK),
    ] {
        let req = Request::get("/v1/health")
            .header(header::COOKIE, cookie)
            .body(Body::empty())
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), expect, "{cookie}");
        assert_eq!(
            res.headers()[header::STRICT_TRANSPORT_SECURITY],
            "max-age=31536000"
        );
    }
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn expired_sign_ins_are_swept_as_new_ones_start() {
    let (_, state) = app().await;
    let flow = Flow {
        pkce_verifier: "v".into(),
        nonce: "n".into(),
        return_to: "/".into(),
    };
    let stale = format!("stale-{}", uuid::Uuid::now_v7());
    sessions::save_flow(&state.pg, &stale, &flow).await.unwrap();
    state
        .pg
        .get()
        .await
        .unwrap()
        .execute(
            "UPDATE login_flows SET expires_at = now() - interval '1 minute' WHERE state_hash = sha256(convert_to($1, 'UTF8'))",
            &[&stale],
        )
        .await
        .unwrap();
    sessions::save_flow(&state.pg, &format!("new-{}", uuid::Uuid::now_v7()), &flow)
        .await
        .unwrap();
    let left: i64 = state
        .pg
        .get()
        .await
        .unwrap()
        .query_one(
            "SELECT count(*) FROM login_flows WHERE expires_at < now()",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(left, 0);
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn bad_input_is_refused_without_being_repeated() {
    let (app, state) = app().await;
    let (_, session) = person(&state, NewUsers::Personal).await;
    let long = "x".repeat(10_000);
    for req in [
        get(&format!("/v1/insights?state={long}"), Some(&session)),
        common::json(
            "POST",
            &format!("/v1/insights/{}/state", uuid::Uuid::nil()),
            &session,
            json!({ "state": long }),
        ),
        common::json("POST", "/v1/tokens", &session, json!({ "label": 5 })),
        common::json(
            "DELETE",
            &format!("/v1/tokens/{long}"),
            &session,
            json!(null),
        ),
    ] {
        let what = format!(
            "{} {}",
            req.method(),
            req.uri().path().chars().take(30).collect::<String>()
        );
        let res = app.clone().oneshot(req).await.unwrap();
        assert!(res.status().is_client_error(), "{what}");
        let body = res.into_body().collect().await.unwrap().to_bytes();
        assert!(
            body.len() < 200 && !String::from_utf8_lossy(&body).contains("xxxx"),
            "{what}"
        );
        let error: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(error["error"].is_string(), "{what}");
    }
    let (status, ..) = send(
        &app,
        common::json(
            "POST",
            "/v1/tokens",
            &session,
            json!({ "label": "a\u{0}b" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}
