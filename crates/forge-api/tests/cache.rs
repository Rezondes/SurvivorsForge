mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, send};
use serde_json::json;
use sqlx::PgPool;

async fn cached_app(pool: PgPool) -> TestApp {
    TestApp::with_config(pool, |c| c.cache_seconds = 60)
}

async fn rename_directly(app: &TestApp, name: &str) {
    sqlx::query("UPDATE addons SET name = $1 WHERE slug = 'lucky'").bind(name).execute(&app.pool).await.unwrap();
}

#[sqlx::test]
async fn anonymous_reads_come_from_the_cache_until_a_write(pool: PgPool) {
    let app = cached_app(pool).await;
    let token = app.publish(1, "alice", "lucky").await;
    let first = app.get("/v1/addons/lucky", None).await.json()["name"].clone();

    rename_directly(&app, "Changed").await;
    assert_eq!(app.get("/v1/addons/lucky", None).await.json()["name"], first, "served from the cache");
    assert_eq!(app.get("/v1/addons/lucky", Some(&token)).await.json()["name"], "Changed", "requests with a token skip the cache");

    let saved = app.patch("/v1/addons/lucky", Some(&token), json!({ "summary": "New summary" })).await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.error());
    let fresh = app.get("/v1/addons/lucky", None).await.json();
    assert_eq!((fresh["name"].as_str(), fresh["summary"].as_str()), (Some("Changed"), Some("New summary")), "a write empties the cache");
}

#[sqlx::test]
async fn a_cached_answer_still_honours_if_none_match(pool: PgPool) {
    let app = cached_app(pool).await;
    app.publish(1, "alice", "lucky").await;
    let first = app.get("/v1/registry", None).await;
    let etag = first.headers["etag"].to_str().unwrap().to_string();

    let request = Request::get("/v1/registry").header("if-none-match", &etag).body(Body::empty()).unwrap();
    let reply = send(&app.router, request).await;
    assert_eq!(reply.status, StatusCode::NOT_MODIFIED);
    assert_eq!(reply.headers["etag"].to_str().unwrap(), etag);
}

#[sqlx::test]
async fn private_routes_are_never_cached(pool: PgPool) {
    let app = cached_app(pool).await;
    let token = app.publish(1, "alice", "lucky").await;
    assert_eq!(app.get("/v1/me/addons", Some(&token)).await.json().as_array().unwrap().len(), 1);
    sqlx::query("UPDATE addons SET deleted_at = now() WHERE slug = 'lucky'").execute(&app.pool).await.unwrap();
    assert_eq!(app.get("/v1/me/addons", Some(&token)).await.json().as_array().unwrap().len(), 0);
}

#[sqlx::test]
async fn telemetry_keeps_the_cache(pool: PgPool) {
    let app = cached_app(pool).await;
    app.publish(1, "alice", "lucky").await;
    let first = app.get("/v1/addons/lucky", None).await.json()["name"].clone();
    rename_directly(&app, "Changed").await;
    let event = json!({ "kind": "install", "addon": "lucky", "version": "v1", "launcher_version": "0.4.3", "os": "windows" });
    assert_eq!(app.post("/v1/telemetry/events", None, json!({ "events": [event] })).await.status, StatusCode::ACCEPTED);
    assert_eq!(app.get("/v1/addons/lucky", None).await.json()["name"], first, "counting changes no cached answer");
}
