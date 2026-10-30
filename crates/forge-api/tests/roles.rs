mod common;

use axum::http::StatusCode;
use common::TestApp;
use serde_json::{Value, json};
use sqlx::PgPool;

/// Root is admin by configuration, mia a moderator given on the website, alice owns an addon.
async fn setup(pool: PgPool) -> (TestApp, String, String, String) {
    setup_with(pool, |_| {}).await
}

async fn setup_with(pool: PgPool, change: impl FnOnce(&mut forge_api::config::Config)) -> (TestApp, String, String, String) {
    let app = TestApp::with_config(pool, |c| {
        c.admin_github_ids = vec![99];
        change(c);
    });
    let owner = app.publish(1, "alice", "lucky").await;
    let admin = app.login(99, "root").await;
    app.login(7, "mia").await;
    let reply = app.put("/v1/admin/users/7/role", Some(&admin), json!({ "role": "moderator", "note": "helps with reports" })).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);
    let moderator = app.login(7, "mia").await;
    (app, owner, admin, moderator)
}

fn names(list: &Value) -> Vec<&str> {
    list.as_array().unwrap().iter().map(|p| p.as_str().unwrap()).collect()
}

#[sqlx::test]
async fn moderator_uses_admin_routes(pool: PgPool) {
    let (app, owner, _, moderator) = setup(pool).await;
    for route in ["/v1/admin/review-queue", "/v1/admin/reports", "/v1/admin/audit", "/v1/admin/users/alice", "/v1/admin/admins"] {
        assert_eq!(app.get(route, Some(&moderator)).await.status, StatusCode::OK, "{route}");
        assert_eq!(app.get(route, Some(&owner)).await.status, StatusCode::FORBIDDEN, "{route}");
    }
    let reply = app.post("/v1/admin/addons/lucky/block", Some(&moderator), json!({ "note": "spam" })).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);
}

#[sqlx::test]
async fn moderator_cannot_change_roles(pool: PgPool) {
    let (app, _, _, moderator) = setup(pool).await;
    let reply = app.put("/v1/admin/users/1/role", Some(&moderator), json!({ "role": "moderator", "note": "me too" })).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    let reply = app.put("/v1/admin/users/7/role", Some(&moderator), json!({ "role": "admin", "note": "promote myself" })).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn moderator_cannot_ban_staff(pool: PgPool) {
    let (app, _, admin, moderator) = setup(pool).await;
    app.login(8, "max").await;
    app.put("/v1/admin/users/8/role", Some(&admin), json!({ "role": "moderator", "note": "second one" })).await;

    let reply = app.post("/v1/admin/users/8/ban", Some(&moderator), json!({ "note": "rival" })).await;
    assert_eq!((reply.status, reply.error().as_str()), (StatusCode::FORBIDDEN, "cannot_ban_staff"));
    let reply = app.post("/v1/admin/users/99/ban", Some(&moderator), json!({ "note": "coup" })).await;
    assert_eq!(reply.error(), "cannot_ban_admin");
    let reply = app.post("/v1/admin/users/1/ban", Some(&moderator), json!({ "note": "malware" })).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "a plain user");
    let reply = app.post("/v1/admin/users/8/ban", Some(&admin), json!({ "note": "abuse" })).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "an admin can");
}

#[sqlx::test]
async fn admin_sets_moderator_and_back(pool: PgPool) {
    let (app, _, admin, moderator) = setup(pool).await;
    assert_eq!(app.get("/v1/me", Some(&moderator)).await.json()["role"], "moderator");
    let staff: Vec<(String, String)> = app.get("/v1/admin/admins", Some(&admin)).await.json().as_array().unwrap().iter()
        .map(|a| (a["login"].as_str().unwrap().into(), a["role"].as_str().unwrap().into()))
        .collect();
    assert_eq!(staff, [("root".into(), "admin".into()), ("mia".into(), "moderator".into())]);

    let again = app.login(7, "mia").await;
    assert_eq!(app.get("/v1/me", Some(&again)).await.json()["role"], "moderator", "survives a new login");

    let reply = app.put("/v1/admin/users/7/role", Some(&admin), json!({ "role": "root", "note": "typo" })).await;
    assert_eq!(reply.error(), "invalid_role");
    assert_eq!(app.put("/v1/admin/users/7/role", Some(&admin), json!({ "role": "user", "note": "done" })).await.status, StatusCode::NO_CONTENT);
    assert_eq!(app.get("/v1/me", Some(&again)).await.json()["role"], "user");
    assert_eq!(app.get("/v1/admin/reports", Some(&again)).await.status, StatusCode::FORBIDDEN);
    let again = app.login(7, "mia").await;
    assert_eq!(app.get("/v1/me", Some(&again)).await.json()["role"], "user", "taken for good");

    let audit = app.get("/v1/admin/audit?action=user_role", Some(&admin)).await.json();
    let details: Vec<(String, String)> = audit["entries"].as_array().unwrap().iter()
        .map(|e| (e["details"]["from"].as_str().unwrap().into(), e["details"]["to"].as_str().unwrap().into()))
        .collect();
    assert_eq!(details, [("moderator".into(), "user".into()), ("user".into(), "moderator".into())], "{audit}");
}

#[sqlx::test]
async fn me_lists_permissions(pool: PgPool) {
    let (app, owner, admin, moderator) = setup(pool).await;
    assert_eq!(names(&app.get("/v1/me", Some(&owner)).await.json()["permissions"]), Vec::<&str>::new());
    assert_eq!(names(&app.get("/v1/me", Some(&moderator)).await.json()["permissions"]), ["moderate", "view_admin_stats"]);
    assert_eq!(names(&app.get("/v1/me", Some(&admin)).await.json()["permissions"]), ["moderate", "manage_roles", "view_admin_stats", "view_grafana"]);
}

#[sqlx::test]
async fn stats_roles_come_from_config(pool: PgPool) {
    let (app, _, _, moderator) = setup_with(pool, |c| c.stats_roles = vec![forge_core::roles::Role::Admin]).await;
    assert_eq!(names(&app.get("/v1/me", Some(&moderator)).await.json()["permissions"]), ["moderate"]);
}

#[sqlx::test]
async fn granted_admin_survives_login(pool: PgPool) {
    // What 0024 writes for an admin granted before it: `granted_role = 'admin'`.
    let app = TestApp::new(pool);
    app.login(5, "old").await;
    sqlx::query("UPDATE users SET granted_role = 'admin', role = 'admin' WHERE github_id = 5").execute(&app.pool).await.unwrap();
    let token = app.login(5, "old").await;
    assert_eq!(app.get("/v1/me", Some(&token)).await.json()["role"], "admin");
    let roles: Vec<String> = sqlx::query_scalar("SELECT role FROM users ORDER BY id").fetch_all(&app.pool).await.unwrap();
    assert_eq!(roles, ["admin"]);
}
