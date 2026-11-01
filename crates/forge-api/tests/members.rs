mod common;

use axum::http::StatusCode;
use common::TestApp;
use forge_api::github::types::User;
use serde_json::json;
use sqlx::PgPool;

fn github_user(app: &TestApp, id: i64, login: &str) {
    app.github.data().users.insert(login.to_lowercase(), User { id, login: login.into(), avatar_url: format!("https://avatars.test/{id}") });
}

#[sqlx::test]
async fn only_owner_manages_members(pool: PgPool) {
    let app = TestApp::new(pool);
    let owner = app.publish(1, "alice", "lucky").await;
    github_user(&app, 2, "bob");
    github_user(&app, 3, "carol");
    let bob = app.login(2, "bob").await;
    let stranger = app.login(4, "dave").await;

    let reply = app.post("/v1/addons/lucky/members", Some(&owner), json!({ "login": "bob", "can_edit_description": true })).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);

    for token in [&bob, &stranger] {
        assert_eq!(app.post("/v1/addons/lucky/members", Some(token), json!({ "login": "carol" })).await.status, StatusCode::FORBIDDEN);
        assert_eq!(app.get("/v1/addons/lucky/members", Some(token)).await.status, StatusCode::FORBIDDEN);
        assert_eq!(app.patch("/v1/addons/lucky/members/2", Some(token), json!({ "can_edit_images": true })).await.status, StatusCode::FORBIDDEN);
        assert_eq!(app.delete("/v1/addons/lucky/members/2", Some(token)).await.status, StatusCode::FORBIDDEN);
    }

    let reply = app.patch("/v1/addons/lucky/members/2", Some(&owner), json!({ "can_edit_images": true })).await;
    assert_eq!(reply.json(), json!([{ "github_id": 2, "login": "bob", "avatar_url": "https://avatars.test/2",
                                      "can_edit_description": true, "can_edit_images": true }]));
    assert_eq!(app.delete("/v1/addons/lucky/members/2", Some(&owner)).await.status, StatusCode::NO_CONTENT);
    assert_eq!(app.get("/v1/addons/lucky/members", Some(&owner)).await.json(), json!([]));
}

#[sqlx::test]
async fn unknown_login_404_and_stub_is_adopted_on_login(pool: PgPool) {
    let app = TestApp::new(pool);
    let owner = app.publish(1, "alice", "lucky").await;
    let reply = app.post("/v1/addons/lucky/members", Some(&owner), json!({ "login": "ghost" })).await;
    assert_eq!((reply.status, reply.error().as_str()), (StatusCode::NOT_FOUND, "github_user_not_found"));

    github_user(&app, 7, "newbie");
    assert_eq!(app.post("/v1/addons/lucky/members", Some(&owner), json!({ "login": "newbie" })).await.status, StatusCode::CREATED);
    let (count, logged_in): (i64, bool) =
        sqlx::query_as("SELECT count(*), bool_or(last_login_at IS NOT NULL) FROM users WHERE github_id = 7").fetch_one(&app.pool).await.unwrap();
    assert_eq!((count, logged_in), (1, false), "a stub");

    app.login(7, "newbie").await;
    let (count, logged_in): (i64, bool) =
        sqlx::query_as("SELECT count(*), bool_or(last_login_at IS NOT NULL) FROM users WHERE github_id = 7").fetch_one(&app.pool).await.unwrap();
    assert_eq!((count, logged_in), (1, true), "the stub became the real user");
    let members: i64 = sqlx::query_scalar("SELECT count(*) FROM addon_members").fetch_one(&app.pool).await.unwrap();
    assert_eq!(members, 1);
}

#[sqlx::test]
async fn description_edit_needs_flag(pool: PgPool) {
    let app = TestApp::new(pool);
    let owner = app.publish(1, "alice", "lucky").await;
    github_user(&app, 2, "bob");
    github_user(&app, 3, "carol");
    app.post("/v1/addons/lucky/members", Some(&owner), json!({ "login": "bob", "can_edit_description": true })).await;
    app.post("/v1/addons/lucky/members", Some(&owner), json!({ "login": "carol", "can_edit_images": true })).await;
    let bob = app.login(2, "bob").await;
    let carol = app.login(3, "carol").await;

    let reply = app.patch("/v1/addons/lucky", Some(&bob), json!({ "summary": "Better", "description_md": "# Hi" })).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.json()["description_md"], "# Hi");
    assert_eq!(app.patch("/v1/addons/lucky", Some(&bob), json!({ "name": "Mine" })).await.status, StatusCode::FORBIDDEN, "no owner fields");
    assert_eq!(app.patch("/v1/addons/lucky", Some(&carol), json!({ "summary": "x" })).await.status, StatusCode::FORBIDDEN);
    assert_eq!(app.patch("/v1/addons/lucky", Some(&carol), json!({})).await.status, StatusCode::FORBIDDEN, "empty patch needs an edit right");
    let dave = app.login(4, "dave").await;
    assert_eq!(app.patch("/v1/addons/lucky", Some(&dave), json!({})).await.status, StatusCode::FORBIDDEN, "stranger");
    assert_eq!(app.delete("/v1/addons/lucky", Some(&bob)).await.status, StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn self_and_duplicate_rejected(pool: PgPool) {
    let app = TestApp::new(pool);
    let owner = app.publish(1, "alice", "lucky").await;
    github_user(&app, 1, "alice");
    github_user(&app, 2, "bob");
    assert_eq!(app.post("/v1/addons/lucky/members", Some(&owner), json!({ "login": "alice" })).await.error(), "owner_not_member");
    app.post("/v1/addons/lucky/members", Some(&owner), json!({ "login": "bob" })).await;
    let reply = app.post("/v1/addons/lucky/members", Some(&owner), json!({ "login": "BOB" })).await;
    assert_eq!((reply.status, reply.error().as_str()), (StatusCode::CONFLICT, "member_exists"));
}

#[sqlx::test]
async fn public_view_hides_flags(pool: PgPool) {
    let app = TestApp::new(pool);
    let owner = app.publish(1, "alice", "lucky").await;
    github_user(&app, 2, "bob");
    app.post("/v1/addons/lucky/members", Some(&owner), json!({ "login": "bob", "can_edit_images": true })).await;
    let view = app.get("/v1/addons/lucky", None).await.json();
    assert_eq!(view["contributors"], json!([{ "login": "bob", "avatar_url": "https://avatars.test/2" }]));
}

#[sqlx::test]
async fn admin_may_do_what_the_owner_may(pool: PgPool) {
    let app = TestApp::with_config(pool, |c| c.admin_github_ids = vec![99]);
    app.publish(1, "alice", "lucky").await;
    github_user(&app, 2, "bob");
    let admin = app.login(99, "root").await;
    assert_eq!(app.post("/v1/addons/lucky/members", Some(&admin), json!({ "login": "bob" })).await.status, StatusCode::CREATED);
    assert_eq!(app.patch("/v1/addons/lucky", Some(&admin), json!({ "name": "Renamed" })).await.status, StatusCode::OK);
}
