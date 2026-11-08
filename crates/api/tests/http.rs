//! The api over HTTP, against the databases from `docker-compose.yml`:
//! `docker compose up -d --wait && cargo test -p groundstation-api -- --ignored`

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::Utc;
use common::{ORIGIN, PASSWORD, app, get, person, send};
use groundstation_api::config::NewUsers;
use groundstation_backend::ch::EventRow;
use groundstation_backend::pg::tenants;
use groundstation_backend::testing;
use groundstation_schema::{EventKind, SCHEMA};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn v1_needs_a_session() {
    let (app, _) = app().await;
    for path in [
        "/v1/health",
        "/v1/trajectories",
        "/v1/trajectories/x",
        "/v1/stats/tools",
    ] {
        let (status, _, body) = send(&app, get(path, None)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(body["error"], "sign in required");
        let (status, ..) = send(&app, get(path, Some("gss_forged"))).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
    }
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn password_sign_in_sets_a_session_and_returns() {
    let (app, _) = app().await;
    let login = |password: &str, origin: Option<&str>| {
        let mut req = Request::post("/auth/login")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
        if let Some(origin) = origin {
            req = req.header(header::ORIGIN, origin);
        }
        let body = format!(
            "password={}&return_to=%2F%23%2Ftrajectories",
            password.replace(' ', "+")
        );
        req.body(Body::from(body)).unwrap()
    };

    let (status, headers, _) = send(&app, login(PASSWORD, Some(ORIGIN))).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers[header::LOCATION], "/#/trajectories");
    let cookie = headers[header::SET_COOKIE].to_str().unwrap();
    assert!(cookie.contains("HttpOnly"));
    let session = cookie
        .split(';')
        .next()
        .unwrap()
        .trim_start_matches("gs_session=");

    let (status, _, health) = send(&app, get("/v1/health", Some(session))).await;
    assert_eq!(status, StatusCode::OK, "{health}");
    assert_eq!(health["deployment"], "hosted");
    assert!(health.get("pid").is_none() && health.get("upload").is_none());

    let (status, headers, _) = send(&app, login("wrong password!", Some(ORIGIN))).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(
        headers[header::LOCATION]
            .to_str()
            .unwrap()
            .contains("error=password")
    );
    assert!(headers.get(header::SET_COOKIE).is_none());

    // From another site, or with no origin at all, nothing happens.
    for origin in [Some("https://evil.example"), None] {
        let (status, ..) = send(&app, login(PASSWORD, origin)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    // The UI's own link, from this site.
    let mut sign_out = get("/auth/logout", Some(session));
    sign_out
        .headers_mut()
        .insert("sec-fetch-site", "same-origin".parse().unwrap());
    let (status, headers, _) = send(&app, sign_out).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(
        headers[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
    let (status, ..) = send(&app, get("/v1/health", Some(session))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn wrong_passwords_are_limited_and_right_ones_are_not() {
    let (app, _) = app().await;
    let login = |password: &str| {
        Request::post("/auth/login")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(header::ORIGIN, ORIGIN)
            .body(Body::from(format!(
                "password={}",
                password.replace(' ', "+")
            )))
            .unwrap()
    };
    // Signing in doesn't use the allowance up...
    for _ in 0..12 {
        let (status, headers, _) = send(&app, login(PASSWORD)).await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert!(headers.contains_key(header::SET_COOKIE));
    }
    // ...ten wrong passwords do, and then even the right one waits.
    for _ in 0..10 {
        let (status, ..) = send(&app, login("not the password")).await;
        assert_eq!(status, StatusCode::SEE_OTHER);
    }
    for password in ["not the password", PASSWORD] {
        let (status, headers, _) = send(&app, login(password)).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert!(!headers.contains_key(header::SET_COOKIE));
    }
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn each_tenant_sees_only_its_own_trajectories() {
    let (app, state) = app().await;
    let (mine, my_session) = person(&state, NewUsers::Personal).await;
    let (_, their_session) = person(&state, NewUsers::Personal).await;

    let tenant = tenants::get(&state.pg, mine).await.unwrap().unwrap();
    let event = testing::event("mine-1", EventKind::AgentStarted, Utc::now());
    let row = EventRow::new(&event, SCHEMA, testing::stamp(&tenant)).unwrap();
    testing::databases()
        .await
        .ingest
        .insert_events(&[row])
        .await
        .unwrap();

    let (_, _, list) = send(&app, get("/v1/trajectories", Some(&my_session))).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["id"], "mine-1");
    assert_eq!(list[0]["host"], Value::Null);
    let (status, _, detail) = send(&app, get("/v1/trajectories/mine", Some(&my_session))).await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["events"][0]["kind"], "agent.started");

    let (_, _, list) = send(&app, get("/v1/trajectories", Some(&their_session))).await;
    assert_eq!(list, Value::Array(vec![]));
    let (status, ..) = send(&app, get("/v1/trajectories/mine-1", Some(&their_session))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, _, health) = send(&app, get("/v1/health", Some(&their_session))).await;
    assert_eq!(
        (health["trajectories"].as_u64(), health["events"].as_u64()),
        (Some(0), Some(0))
    );
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn shared_signup_puts_everyone_in_one_tenant() {
    let (_, state) = app().await;
    let (a, b) = tokio::join!(
        person(&state, NewUsers::Shared),
        person(&state, NewUsers::Shared)
    );
    let (c, _) = person(&state, NewUsers::Personal).await;
    assert_eq!(a.0, b.0);
    assert_eq!(a.0, groundstation_api::accounts::SHARED_TENANT);
    assert_ne!(a.0, c);
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn answers_are_never_cached() {
    // The same URL answers for whoever's session comes with it, so a shared
    // cache must never keep one, nor a browser after someone signs out.
    let (app, state) = app().await;
    let (_, session) = person(&state, NewUsers::Personal).await;
    let mut requests = vec![
        get("/v1/health", None),
        get("/", None),
        common::json(
            "POST",
            "/v1/tokens",
            &session,
            serde_json::json!({ "label": "x" }),
        ),
    ];
    for path in [
        "/v1/health",
        "/v1/trajectories",
        "/v1/stats/tools",
        "/v1/insights",
        "/v1/tokens",
    ] {
        requests.push(get(path, Some(&session)));
    }
    for req in requests {
        let what = format!("{} {}", req.method(), req.uri());
        let (status, headers, _) = send(&app, req).await;
        assert!(status != StatusCode::NOT_FOUND, "{what}: {status}");
        assert_eq!(headers[header::CACHE_CONTROL], "no-store", "{what}");
    }
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn serves_the_ui_and_sign_in_page_without_a_session() {
    // The sign-in page posts a form to its own origin. From a `no-referrer`
    // page browsers send that with `Origin: null`, which the api refuses.
    let (page, _) = app().await;
    let (_, headers, _) = send(&page, get("/auth/login", None)).await;
    assert_eq!(headers[header::REFERRER_POLICY], "same-origin");

    let (app, _) = app().await;
    let res = app.clone().oneshot(get("/", None)).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(
        res.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/html")
    );
    let res = app
        .clone()
        .oneshot(get("/auth/login?return_to=//evil", None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let html =
        String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    assert!(html.contains(r#"name="return_to" value="/""#), "{html}");
    let (status, ..) = send(&app, get("/v1/nope", None)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn a_bad_query_string_gets_the_json_error_any_failure_gets() {
    let (app, state) = app().await;
    let (_, session) = person(&state, NewUsers::Personal).await;
    for uri in [
        "/v1/trajectories?limit=abc",
        "/v1/stats/tools?days=-1",
        "/v1/insights?limit=many",
    ] {
        let (status, headers, body) = send(&app, get(uri, Some(&session))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
        assert_eq!(headers[header::CONTENT_TYPE], "application/json", "{uri}");
        assert_eq!(
            body["error"],
            groundstation_wire::BAD_QUERY,
            "{uri}: {body}"
        );
    }
}
