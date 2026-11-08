//! Ingest tokens over HTTP, and the CLI's approval page.
//! `docker compose up -d --wait && cargo test -p groundstation-api -- --ignored`

mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{app, get, json, person, send};
use groundstation_api::config::NewUsers;
use groundstation_api::sessions;
use groundstation_backend::pg::tokens;
use serde_json::{Value, json};

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn members_create_list_and_revoke_tokens() {
    let (app, state) = app().await;
    let (tenant, session) = person(&state, NewUsers::Personal).await;

    let (status, _, created) = send(
        &app,
        json("POST", "/v1/tokens", &session, json!({"label": " laptop "})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["label"], "laptop");
    let secret = created["secret"].as_str().unwrap();
    let grant = tokens::authenticate(&state.pg, secret)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(grant.tenant_id, tenant);

    let (_, _, list) = send(&app, get("/v1/tokens", Some(&session))).await;
    assert_eq!(list[0]["id"], created["id"]);
    assert!(list[0].get("secret").is_none() && !list[0]["last_used_at"].is_null());

    let (status, ..) = send(
        &app,
        json("POST", "/v1/tokens", &session, json!({"label": ""})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let path = format!("/v1/tokens/{}", created["id"].as_str().unwrap());
    let (status, ..) = send(&app, json("DELETE", &path, &session, json!(null))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(
        tokens::authenticate(&state.pg, secret)
            .await
            .unwrap()
            .is_none()
    );
    let (_, _, list) = send(&app, get("/v1/tokens", Some(&session))).await;
    assert!(!list[0]["revoked_at"].is_null());
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn only_creators_and_admins_revoke() {
    let (app, state) = app().await;
    // Shared tenant: the first person in may be its admin; a newcomer isn't.
    let (_, first) = person(&state, NewUsers::Shared).await;
    let (_, second) = person(&state, NewUsers::Shared).await;
    let (_, _, created) = send(
        &app,
        json("POST", "/v1/tokens", &first, json!({"label": "ci"})),
    )
    .await;
    let path = format!("/v1/tokens/{}", created["id"].as_str().unwrap());
    let (status, ..) = send(&app, json("DELETE", &path, &second, json!(null))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, ..) = send(&app, json("DELETE", &path, &first, json!(null))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn an_admin_revokes_any_token_and_a_member_only_their_own() {
    let (app, state) = app().await;
    // A personal tenant: its first person is its admin. Add a member.
    let (tenant, admin) = person(&state, NewUsers::Personal).await;
    let (_, outsider) = person(&state, NewUsers::Personal).await;
    let member = state
        .pg
        .get()
        .await
        .unwrap()
        .query_one(
            "WITH m AS (SELECT user_id FROM sessions WHERE id_hash = sha256(convert_to($2, 'UTF8')))
             INSERT INTO memberships (tenant_id, user_id, role) SELECT $1, user_id, 'member' FROM m RETURNING user_id",
            &[&tenant, &outsider],
        )
        .await
        .unwrap()
        .get::<_, uuid::Uuid>(0);
    let member_session =
        groundstation_api::sessions::create(&state.pg, member, tenant, chrono::Duration::hours(1))
            .await
            .unwrap();
    let create = |session: &str| json("POST", "/v1/tokens", session, json!({"label": "ci"}));
    let (_, _, theirs) = send(&app, create(&member_session)).await;
    let (_, _, admins) = send(&app, create(&admin)).await;
    let path = |t: &serde_json::Value| format!("/v1/tokens/{}", t["id"].as_str().unwrap());
    let (status, ..) = send(
        &app,
        json("DELETE", &path(&admins), &member_session, json!(null)),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a member can't revoke the admin's token"
    );
    let (status, ..) = send(&app, json("DELETE", &path(&theirs), &admin, json!(null))).await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "an admin revokes a member's token"
    );
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn writes_must_come_from_the_ui_origin() {
    let (app, state) = app().await;
    let (_, session) = person(&state, NewUsers::Personal).await;
    let mut cross = json("POST", "/v1/tokens", &session, json!({"label": "x"}));
    cross
        .headers_mut()
        .insert(header::ORIGIN, "https://evil.example".parse().unwrap());
    assert_eq!(send(&app, cross).await.0, StatusCode::FORBIDDEN);
    let mut text = json("POST", "/v1/tokens", &session, json!({"label": "x"}));
    text.headers_mut()
        .insert(header::CONTENT_TYPE, "text/plain".parse().unwrap());
    assert_eq!(send(&app, text).await.0, StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

/// The verifier the CLI in these tests holds, and its challenge.
const VERIFIER: &str = "a-verifier-only-the-cli-knows-0123456789abcdef";

fn approval_query() -> String {
    format!(
        "port=53682&state=abcdefghijklmnop0123&label=work%20laptop&challenge={}",
        sessions::challenge(VERIFIER)
    )
}

/// Allows the computer, as the approval page's form does; returns the code
/// the browser takes back to the CLI.
async fn approve(app: &Router, session: &str) -> String {
    let mut approve = json("POST", "/auth/cli", session, json!(null));
    approve.headers_mut().insert(
        header::CONTENT_TYPE,
        "application/x-www-form-urlencoded".parse().unwrap(),
    );
    *approve.body_mut() = approval_query().into();
    let (status, headers, _) = send(app, approve).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let to = headers[header::LOCATION].to_str().unwrap();
    let prefix = "http://127.0.0.1:53682/callback?state=abcdefghijklmnop0123&code=";
    assert!(to.starts_with(prefix), "{to}");
    to[prefix.len()..].to_string()
}

/// The CLI collecting its token: a form, with no cookie and no `Origin`.
async fn collect(app: &Router, code: &str, verifier: &str) -> (StatusCode, Value) {
    let req = Request::post("/auth/cli/token")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(format!("code={code}&verifier={verifier}")))
        .unwrap();
    let (status, _, body) = send(app, req).await;
    (status, body)
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn the_cli_gets_a_token_after_approval() {
    let (app, state) = app().await;
    let (tenant, session) = person(&state, NewUsers::Personal).await;
    let page = format!("/auth/cli?{}", approval_query());

    // Signed out: sign in first, then come back here.
    let (status, headers, _) = send(&app, get(&page, None)).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let to = headers[header::LOCATION].to_str().unwrap();
    assert!(
        to.starts_with("/auth/login?return_to=/auth/cli%3Fport%3D53682"),
        "{to}"
    );
    assert!(to.contains("challenge%3D"), "{to}");

    let (status, headers, html) = send(&app, get(&page, Some(&session))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.as_str().unwrap().contains("work laptop"));
    let csp = headers[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
    assert!(csp.contains("form-action 'self' http://127.0.0.1:*"));

    // The browser brings the CLI a one-time code, never the token.
    let code = approve(&app, &session).await;
    assert!(code.starts_with("gsc_"), "{code}");
    assert_eq!(tokens::list(&state.pg, tenant).await.unwrap().len(), 0);

    // Someone who saw the code (it's in the browser's history) but doesn't
    // hold the verifier gets nothing, and leaves it for the CLI.
    let (status, _) = collect(&app, &code, "a-guess").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, body) = collect(&app, &code, VERIFIER).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // The CLI shows where it's about to upload, and who allowed it.
    assert_eq!(body["approved_by"], "ana@acme.test");
    assert!(body["workspace"].is_string(), "{body}");
    let token = body["token"].as_str().unwrap();
    let grant = tokens::authenticate(&state.pg, token)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(grant.tenant_id, tenant);
    let made = tokens::list(&state.pg, tenant).await.unwrap();
    assert_eq!(made.len(), 1);
    assert_eq!(made[0].label, "work laptop");

    // Once only.
    let (status, body) = collect(&app, &code, VERIFIER).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("cloud login again")
    );

    let challenge = sessions::challenge(VERIFIER);
    for bad in [
        format!("port=80&state=abcdefghijklmnop0123&label=x&challenge={challenge}"),
        format!("port=53682&state=short&label=x&challenge={challenge}"),
        "port=53682&state=abcdefghijklmnop0123&label=x".to_string(),
        "port=53682&state=abcdefghijklmnop0123&label=x&challenge=ABC".to_string(),
    ] {
        let (status, ..) = send(&app, get(&format!("/auth/cli?{bad}"), Some(&session))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn an_approval_the_cli_never_collected_expires() {
    let (app, state) = app().await;
    let (tenant, session) = person(&state, NewUsers::Personal).await;
    let code = approve(&app, &session).await;
    state
        .pg
        .get()
        .await
        .unwrap()
        .execute(
            "UPDATE cli_approvals SET expires_at = now() - interval '1 second' WHERE tenant_id = $1",
            &[&tenant],
        )
        .await
        .unwrap();
    let (status, _) = collect(&app, &code, VERIFIER).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(tokens::list(&state.pg, tenant).await.unwrap().is_empty());

    // Approving twice (a reload, a double click) leaves one token, not two.
    let (first, second) = (approve(&app, &session).await, approve(&app, &session).await);
    assert_ne!(first, second);
    assert_eq!(collect(&app, &second, VERIFIER).await.0, StatusCode::OK);
    assert_eq!(tokens::list(&state.pg, tenant).await.unwrap().len(), 1);

    let (status, body) = collect(&app, "", "").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn an_approval_outlives_a_failure_while_its_token_is_made() {
    let (app, state) = app().await;
    let (tenant, session) = person(&state, NewUsers::Personal).await;
    let code = approve(&app, &session).await;
    // Postgres fails this tenant's next token, as a dropped connection would.
    let name = format!("gs_test_fail_{}", tenant.simple());
    let db = state.pg.get().await.unwrap();
    db.batch_execute(&format!(
        "CREATE FUNCTION {name}() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           IF NEW.tenant_id = '{tenant}' THEN RAISE EXCEPTION 'injected'; END IF;
           RETURN NEW;
         END $$;
         CREATE TRIGGER {name} BEFORE INSERT ON ingest_tokens
           FOR EACH ROW EXECUTE FUNCTION {name}();"
    ))
    .await
    .unwrap();
    let (status, _) = collect(&app, &code, VERIFIER).await;
    db.batch_execute(&format!(
        "DROP TRIGGER {name} ON ingest_tokens; DROP FUNCTION {name}();"
    ))
    .await
    .unwrap();
    assert!(status.is_server_error(), "{status}");
    assert!(tokens::list(&state.pg, tenant).await.unwrap().is_empty());
    // The CLI asks again with the same code and gets its token.
    let (status, body) = collect(&app, &code, VERIFIER).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(tokens::list(&state.pg, tenant).await.unwrap().len(), 1);
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn a_workspace_suspended_since_the_approval_gets_no_token() {
    let (app, state) = app().await;
    let (tenant, session) = person(&state, NewUsers::Personal).await;
    let code = approve(&app, &session).await;
    state
        .pg
        .get()
        .await
        .unwrap()
        .execute(
            "UPDATE tenants SET suspended_at = now() WHERE id = $1",
            &[&tenant],
        )
        .await
        .unwrap();
    let (status, _) = collect(&app, &code, VERIFIER).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(tokens::list(&state.pg, tenant).await.unwrap().is_empty());
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn allowing_after_signing_out_goes_back_through_sign_in() {
    let (app, state) = app().await;
    let (_, session) = person(&state, NewUsers::Personal).await;
    sessions::delete(&state.pg, &session).await.unwrap();
    let mut approve = json("POST", "/auth/cli", &session, json!(null));
    approve.headers_mut().insert(
        header::CONTENT_TYPE,
        "application/x-www-form-urlencoded".parse().unwrap(),
    );
    *approve.body_mut() = approval_query().into();
    let (status, headers, _) = send(&app, approve).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let to = headers[header::LOCATION].to_str().unwrap();
    assert!(
        to.starts_with("/auth/login?return_to=/auth/cli%3Fport%3D53682"),
        "{to}"
    );
}
