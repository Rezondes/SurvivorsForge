mod common;

use axum::http::StatusCode;
use chrono::Duration;
use common::{RETURN_TO, TestApp, query_param, urlencode};
use serde_json::json;
use sqlx::PgPool;

#[sqlx::test]
async fn start_rejects_foreign_return_to(pool: PgPool) {
    let app = TestApp::new(pool);
    for bad in [
        "https://evil.test/auth/callback",
        "https://web.test.evil.test/auth/callback",
        "http://web.test/auth/callback",
        "https://web.test/auth/callback/extra",
        "https://web.test/auth/callback?x=1",
    ] {
        let reply = app.get(&format!("/v1/auth/github/start?return_to={}", urlencode(bad)), None).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{bad}");
        assert_eq!(reply.error(), "invalid_return_to");
    }
    let reply = app.get(&format!("/v1/auth/github/start?return_to={}", urlencode(RETURN_TO)), None).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    let location = reply.location();
    assert!(location.starts_with("http://github-oauth.test/login/oauth/authorize?"), "{location}");
    assert_eq!(query_param(&location, "code_challenge_method").as_deref(), Some("S256"));
    assert_eq!(query_param(&location, "client_id").as_deref(), Some("client-id"));
}

#[sqlx::test]
async fn callback_rejects_unknown_reused_or_expired_state(pool: PgPool) {
    let app = TestApp::new(pool);
    app.github.data().oauth_users.insert("c".into(), forge_api::github::types::User { id: 1, login: "a".into(), avatar_url: "".into() });

    let unknown = app.get("/v1/auth/github/callback?state=nope&code=c", None).await;
    assert_eq!(unknown.error(), "invalid_state");

    let state = app.start_login().await;
    let first = app.get(&format!("/v1/auth/github/callback?state={state}&code=c"), None).await;
    assert_eq!(first.status, StatusCode::SEE_OTHER);
    let reused = app.get(&format!("/v1/auth/github/callback?state={state}&code=c"), None).await;
    assert_eq!(reused.error(), "invalid_state", "a state works once");

    let state = app.start_login().await;
    app.advance(Duration::minutes(11));
    let expired = app.get(&format!("/v1/auth/github/callback?state={state}&code=c"), None).await;
    assert_eq!(expired.error(), "invalid_state");
}

#[sqlx::test]
async fn denied_login_returns_to_client_with_error(pool: PgPool) {
    let app = TestApp::new(pool);
    let state = app.start_login().await;
    let reply = app.get(&format!("/v1/auth/github/callback?state={state}&error=access_denied"), None).await;
    assert_eq!(query_param(&reply.location(), "error").as_deref(), Some("denied"));
}

#[sqlx::test]
async fn login_upserts_by_github_id(pool: PgPool) {
    let app = TestApp::new(pool);
    app.login(77, "old-name").await;
    app.login(77, "new-name").await;
    let rows: Vec<(String,)> = sqlx::query_as("SELECT login::text FROM users WHERE github_id = 77").fetch_all(&app.pool).await.unwrap();
    assert_eq!(rows, [("new-name".to_string(),)]);
}

#[sqlx::test]
async fn login_code_single_use_and_expires(pool: PgPool) {
    let app = TestApp::new(pool);
    app.github.data().oauth_users.insert("c".into(), forge_api::github::types::User { id: 5, login: "u".into(), avatar_url: "".into() });
    let login_code = |reply: &common::Reply| query_param(&reply.location(), "code").unwrap();

    let state = app.start_login().await;
    let code = login_code(&app.get(&format!("/v1/auth/github/callback?state={state}&code=c"), None).await);
    let wrong_return = app.post("/v1/auth/exchange", None, json!({ "code": code, "return_to": "https://evil.test/cb" })).await;
    assert_eq!(wrong_return.error(), "invalid_code", "bound to return_to");

    let state = app.start_login().await;
    let code = login_code(&app.get(&format!("/v1/auth/github/callback?state={state}&code=c"), None).await);
    let ok = app.post("/v1/auth/exchange", None, json!({ "code": code, "return_to": RETURN_TO })).await;
    assert_eq!(ok.status, StatusCode::OK);
    let again = app.post("/v1/auth/exchange", None, json!({ "code": code, "return_to": RETURN_TO })).await;
    assert_eq!(again.error(), "invalid_code", "single use");

    let state = app.start_login().await;
    let code = login_code(&app.get(&format!("/v1/auth/github/callback?state={state}&code=c"), None).await);
    app.advance(Duration::seconds(61));
    let late = app.post("/v1/auth/exchange", None, json!({ "code": code, "return_to": RETURN_TO })).await;
    assert_eq!(late.error(), "invalid_code", "expires after 60 s");
}

/// The challenge of `verifier` (RFC 7636, S256).
fn challenge(verifier: &str) -> String {
    use base64::Engine;
    use sha2::Digest;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier.as_bytes()))
}

/// A login started with `code_challenge`, through GitHub's callback; the one-time code.
async fn code_with_challenge(app: &TestApp, challenge: &str) -> String {
    app.github.data().oauth_users.insert("pk".into(), forge_api::github::types::User { id: 31, login: "pk".into(), avatar_url: "".into() });
    let reply = app.get(&format!("/v1/auth/github/start?return_to={}&code_challenge={challenge}", urlencode(RETURN_TO)), None).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    let state = query_param(&reply.location(), "state").unwrap();
    let reply = app.get(&format!("/v1/auth/github/callback?state={state}&code=pk"), None).await;
    query_param(&reply.location(), "code").unwrap()
}

#[sqlx::test]
async fn exchange_needs_the_verifier_of_its_challenge(pool: PgPool) {
    let app = TestApp::new(pool);
    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";

    let code = code_with_challenge(&app, &challenge(verifier)).await;
    let missing = app.post("/v1/auth/exchange", None, json!({ "code": code, "return_to": RETURN_TO })).await;
    assert_eq!(missing.error(), "invalid_code", "no verifier");
    let used = app.post("/v1/auth/exchange", None, json!({ "code": code, "return_to": RETURN_TO, "code_verifier": verifier })).await;
    assert_eq!(used.error(), "invalid_code", "a refused code is spent");

    let code = code_with_challenge(&app, &challenge(verifier)).await;
    let wrong = app.post("/v1/auth/exchange", None, json!({ "code": code, "return_to": RETURN_TO, "code_verifier": "another-verifier-of-enough-length-0123456789" })).await;
    assert_eq!(wrong.error(), "invalid_code", "wrong verifier");

    let code = code_with_challenge(&app, &challenge(verifier)).await;
    let ok = app.post("/v1/auth/exchange", None, json!({ "code": code, "return_to": RETURN_TO, "code_verifier": verifier })).await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.body);
    assert!(ok.json()["token"].is_string());
}

#[sqlx::test]
async fn login_without_challenge_ignores_a_verifier(pool: PgPool) {
    let app = TestApp::new(pool);
    app.github.data().oauth_users.insert("c".into(), forge_api::github::types::User { id: 32, login: "w".into(), avatar_url: "".into() });
    let state = app.start_login().await;
    let code = query_param(&app.get(&format!("/v1/auth/github/callback?state={state}&code=c"), None).await.location(), "code").unwrap();
    let ok = app.post("/v1/auth/exchange", None, json!({ "code": code, "return_to": RETURN_TO, "code_verifier": "anything" })).await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.body);
}

#[sqlx::test]
async fn start_rejects_a_malformed_challenge(pool: PgPool) {
    let app = TestApp::new(pool);
    let long_enough = "a".repeat(43);
    for bad in ["short", &"a".repeat(44), &format!("{}+", &long_enough[..42]), &format!("{}=", &long_enough[..42])] {
        let reply = app.get(&format!("/v1/auth/github/start?return_to={}&code_challenge={}", urlencode(RETURN_TO), urlencode(bad)), None).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{bad}");
        assert_eq!(reply.error(), "invalid_challenge");
    }
}

#[sqlx::test]
async fn me_requires_valid_token(pool: PgPool) {
    let app = TestApp::new(pool);
    assert_eq!(app.get("/v1/me", None).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(app.get("/v1/me", Some("garbage")).await.status, StatusCode::UNAUTHORIZED);

    let token = app.login(9, "nine").await;
    let me = app.get("/v1/me", Some(&token)).await;
    assert_eq!(me.status, StatusCode::OK);
    let me = me.json();
    assert_eq!(me["github_id"], 9);
    assert_eq!(me["login"], "nine");
    assert_eq!(me["role"], "user");
    assert_eq!(me["avatar_url"], "https://avatars.test/9");
    assert!(me["id"].is_i64());

    assert_eq!(app.post("/v1/auth/logout", Some(&token), json!({})).await.status, StatusCode::NO_CONTENT);
    assert_eq!(app.get("/v1/me", Some(&token)).await.status, StatusCode::UNAUTHORIZED, "logged out");

    let token = app.login(9, "nine").await;
    app.advance(Duration::days(31));
    assert_eq!(app.get("/v1/me", Some(&token)).await.status, StatusCode::UNAUTHORIZED, "expired");
}

#[sqlx::test]
async fn admin_role_follows_env(pool: PgPool) {
    let admin = TestApp::with_config(pool.clone(), |c| c.admin_github_ids = vec![42]);
    let token = admin.login(42, "boss").await;
    assert_eq!(admin.get("/v1/me", Some(&token)).await.json()["role"], "admin");

    let plain = TestApp::new(pool);
    let token = plain.login(42, "boss").await;
    assert_eq!(plain.get("/v1/me", Some(&token)).await.json()["role"], "user", "removed from ADMIN_GITHUB_IDS");
}

#[sqlx::test]
async fn banned_user_cannot_login_or_use_token(pool: PgPool) {
    let app = TestApp::new(pool);
    let token = app.login(13, "bad").await;
    sqlx::query("UPDATE users SET banned_at = now() WHERE github_id = 13").execute(&app.pool).await.unwrap();

    let me = app.get("/v1/me", Some(&token)).await;
    assert_eq!(me.status, StatusCode::FORBIDDEN);
    assert_eq!(me.error(), "banned");

    app.github.data().oauth_users.insert("c".into(), forge_api::github::types::User { id: 13, login: "bad".into(), avatar_url: "".into() });
    let state = app.start_login().await;
    let reply = app.get(&format!("/v1/auth/github/callback?state={state}&code=c"), None).await;
    assert_eq!(query_param(&reply.location(), "error").as_deref(), Some("banned"));
    assert_eq!(query_param(&reply.location(), "code"), None);
}

#[sqlx::test]
async fn no_plaintext_tokens_stored(pool: PgPool) {
    let app = TestApp::new(pool);
    let token = app.login(3, "three").await;
    let dump: String = sqlx::query_scalar(
        "SELECT coalesce(string_agg(t::text, ' '), '') FROM (
             SELECT row_to_json(s) AS t FROM sessions s
             UNION ALL SELECT row_to_json(l) FROM login_codes l
             UNION ALL SELECT row_to_json(o) FROM oauth_states o) rows",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert!(!dump.contains(&token));
    assert!(!dump.contains("gho_"), "no GitHub user token");
}
