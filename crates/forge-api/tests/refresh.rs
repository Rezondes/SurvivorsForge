mod common;

use axum::http::StatusCode;
use common::TestApp;
use forge_api::github::types::User;
use serde_json::json;
use sqlx::PgPool;

async fn open_jobs(app: &TestApp, kind: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind = $1 AND done_at IS NULL").bind(kind).fetch_one(&app.pool).await.unwrap()
}

async fn finish_jobs(app: &TestApp) {
    sqlx::query("UPDATE jobs SET done_at = now()").execute(&app.pool).await.unwrap();
}

#[sqlx::test]
async fn admin_queues_a_check_of_every_addon(pool: PgPool) {
    let app = TestApp::with_config(pool, |c| c.admin_github_ids = vec![99]);
    let owner = app.publish(1, "alice", "lucky").await;
    let admin = app.login(99, "root").await;
    finish_jobs(&app).await;

    assert_eq!(app.post("/v1/admin/addons/sync", Some(&owner), json!({})).await.status, StatusCode::FORBIDDEN);
    assert_eq!(app.post("/v1/admin/addons/sync", Some(&admin), json!({})).await.status, StatusCode::ACCEPTED);
    assert_eq!(app.post("/v1/admin/addons/sync", Some(&admin), json!({})).await.status, StatusCode::ACCEPTED, "no limit for admins");
    assert_eq!(open_jobs(&app, "poll_releases").await, 1, "one open check at a time");
}

#[sqlx::test]
async fn creator_forces_a_check_once_every_five_minutes(pool: PgPool) {
    let app = TestApp::new(pool);
    let owner = app.publish(1, "alice", "lucky").await;
    app.github.add_repo(101, "alice/odds", "alice");
    let reply = app.post("/v1/addons", Some(&owner), common::addon_body("odds", "alice/odds", &["Odds"])).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    let stranger = app.login(4, "dave").await;
    finish_jobs(&app).await;

    assert_eq!(app.post("/v1/addons/lucky/sync", Some(&stranger), json!({})).await.status, StatusCode::FORBIDDEN);
    assert_eq!(app.post("/v1/addons/lucky/sync", Some(&owner), json!({})).await.status, StatusCode::ACCEPTED);
    assert_eq!(open_jobs(&app, "sync_addon").await, 1);

    app.advance(chrono::Duration::minutes(3));
    let reply = app.post("/v1/addons/odds/sync", Some(&owner), json!({})).await;
    assert_eq!((reply.status, reply.error().as_str()), (StatusCode::TOO_MANY_REQUESTS, "refresh_too_soon"), "per user, not per addon");
    assert!(reply.json()["message"].as_str().unwrap().contains("in 2 minutes"), "{}", reply.body);

    app.advance(chrono::Duration::minutes(2));
    assert_eq!(app.post("/v1/addons/odds/sync", Some(&owner), json!({})).await.status, StatusCode::ACCEPTED);
}

#[sqlx::test]
async fn contributors_force_a_check_too(pool: PgPool) {
    let app = TestApp::new(pool);
    let owner = app.publish(1, "alice", "lucky").await;
    app.github.data().users.insert("bob".into(), User { id: 2, login: "bob".into(), avatar_url: String::new() });
    assert_eq!(app.post("/v1/addons/lucky/members", Some(&owner), json!({ "login": "bob" })).await.status, StatusCode::CREATED);
    let bob = app.login(2, "bob").await;
    assert_eq!(app.post("/v1/addons/lucky/sync", Some(&bob), json!({})).await.status, StatusCode::ACCEPTED);
    assert_eq!(app.post("/v1/addons/lucky/sync", Some(&owner), json!({})).await.status, StatusCode::ACCEPTED, "the limit is the user's own");
}
