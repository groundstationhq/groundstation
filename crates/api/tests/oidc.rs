//! Signing in through a real OpenID Connect provider: Dex from
//! `docker-compose.yml`, with its one user.
//!
//! `make deps && cargo test -p groundstation-api -- --ignored` (the dev
//! profile starts Dex; the databases alone don't).

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use clap::Parser;
use groundstation_api::{AppState, Config, migrate, router};
use http_body_util::BodyExt;
use tower::ServiceExt;

const DEX: &str = "http://127.0.0.1:5556";

async fn app(extra: &[&str]) -> Router {
    let mut args = vec![
        "api",
        "--oidc-issuer",
        "http://127.0.0.1:5556/dex",
        "--oidc-client-id",
        "groundstation",
        "--oidc-client-secret",
        "groundstation-dev-secret",
        "--oidc-name",
        "Dex",
    ];
    args.extend_from_slice(extra);
    let config = Config::parse_from(args);
    migrate(&config).await.unwrap();
    router(AppState::new(config).unwrap())
}

async fn location(app: &Router, uri: &str) -> (StatusCode, Option<String>, Option<String>) {
    location_with(app, uri, None).await
}

/// `cookie` is a `Set-Cookie` value from an earlier answer, sent back as a
/// browser would.
async fn location_with(
    app: &Router,
    uri: &str,
    cookie: Option<&str>,
) -> (StatusCode, Option<String>, Option<String>) {
    let mut req = Request::get(uri);
    if let Some(cookie) = cookie {
        req = req.header(header::COOKIE, cookie.split(';').next().unwrap());
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let header = |name| {
        res.headers()
            .get(name)
            .map(|v: &axum::http::HeaderValue| v.to_str().unwrap().to_string())
    };
    (
        res.status(),
        header(header::LOCATION),
        header(header::SET_COOKIE),
    )
}

/// Logs in at Dex like a browser would and returns where Dex sends it back.
async fn dex_login(authorize: &str, email: &str) -> String {
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let mut url = authorize.to_string();
    // authorize -> local connector -> login form
    loop {
        let res = http.get(&url).send().await.unwrap();
        match res.headers().get("location") {
            Some(next) => url = absolute(next.to_str().unwrap()),
            None => {
                let html = res.text().await.unwrap();
                let action = html
                    .split("action=\"")
                    .nth(1)
                    .and_then(|s| s.split('"').next())
                    .expect("a login form")
                    .replace("&amp;", "&");
                url = absolute(&action);
                break;
            }
        }
    }
    let res = http
        .post(&url)
        .form(&[("login", email), ("password", "password")])
        .send()
        .await
        .unwrap();
    let mut next = absolute(res.headers()["location"].to_str().unwrap());
    // approval (skipped) -> our callback
    while next.starts_with(DEX) {
        let res = http.get(&next).send().await.unwrap();
        next = absolute(res.headers()["location"].to_str().unwrap());
    }
    next
}

fn absolute(location: &str) -> String {
    if location.starts_with('/') {
        format!("{DEX}{location}")
    } else {
        location.to_string()
    }
}

#[tokio::test]
#[ignore = "needs `make deps`"]
async fn signs_in_through_the_provider() {
    let app = app(&[]).await;
    // With a provider as the only way in, the login page goes straight to it.
    let (status, to, _) = location(&app, "/auth/login?return_to=/%23/trajectories").await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let (_, authorize, started) = location(&app, &to.unwrap()).await;
    let authorize = authorize.unwrap();
    let started = started.expect("a cookie that ties the sign-in to this browser");
    assert!(started.starts_with("gs_login=") && started.contains("HttpOnly"));
    assert!(
        authorize.starts_with("http://127.0.0.1:5556/dex/auth?"),
        "{authorize}"
    );
    assert!(authorize.contains("code_challenge="));

    let callback = dex_login(&authorize, "dev@groundstation.test").await;
    let path = callback.trim_start_matches("http://127.0.0.1:4321");
    assert!(path.starts_with("/auth/callback?code="), "{callback}");

    // Another browser given the same link is not signed in, and doesn't use
    // the sign-in up.
    let (status, _, cookie) = location(&app, path).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(cookie.is_none());
    let (status, ..) = location_with(&app, path, Some("gs_login=someone-elses-state")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, to, cookie) = location_with(&app, path, Some(&started)).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(to.as_deref(), Some("/#/trajectories"));
    let session = cookie.unwrap();
    let session = session.split(';').next().unwrap();

    let res = app
        .clone()
        .oneshot(
            Request::get("/v1/health")
                .header(header::COOKIE, session)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // The state is single-use: replaying the callback fails.
    let (status, ..) = location_with(&app, path, Some(&started)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[ignore = "needs `make deps`"]
async fn refuses_accounts_outside_the_allowed_domains() {
    let app = app(&["--oidc-allowed-domains", "acme.com"]).await;
    let (_, authorize, started) = location(&app, "/auth/oidc").await;
    let callback = dex_login(&authorize.unwrap(), "dev@groundstation.test").await;
    let path = callback.trim_start_matches("http://127.0.0.1:4321");
    let started = started.unwrap();
    let req = Request::get(path).header(header::COOKIE, started.split(';').next().unwrap());
    let res = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    assert!(res.headers().get(header::SET_COOKIE).is_none());
    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert!(String::from_utf8_lossy(&body).contains("isn&#39;t allowed"));
}

#[tokio::test]
#[ignore = "needs `make deps`"]
async fn signing_out_doesnt_go_straight_back_to_the_provider() {
    // With a provider as the only way in, the sign-in page skips to it...
    let app = app(&[]).await;
    let (status, to, _) = location(&app, "/auth/login").await;
    assert_eq!(
        (status, to.as_deref()),
        (StatusCode::SEE_OTHER, Some("/auth/oidc?return_to=/"))
    );
    // ...but not right after signing out, or the provider's own session
    // would sign the person straight back in.
    let (status, to, _) = location(&app, "/auth/login?signed_out=1").await;
    assert_eq!((status, to), (StatusCode::OK, None));
    let res = app
        .clone()
        .oneshot(
            Request::get("/auth/login?signed_out=1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let page = String::from_utf8_lossy(&body);
    assert!(
        page.contains("signed out") && page.contains("Continue with Dex"),
        "{page}"
    );
}

#[tokio::test]
#[ignore = "needs `make deps`"]
async fn a_provider_that_cant_be_reached_is_a_bad_gateway() {
    let app = router(
        AppState::new(Config::parse_from([
            "api",
            "--oidc-issuer",
            "http://127.0.0.1:9/dex",
            "--oidc-client-id",
            "groundstation",
        ]))
        .unwrap(),
    );
    let res = app
        .clone()
        .oneshot(Request::get("/auth/oidc").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let page = String::from_utf8_lossy(&body);
    // The provider's address and the library's words stay in the log.
    assert!(
        page.contains("can&#39;t be reached") && !page.contains("127.0.0.1:9"),
        "{page}"
    );
}
