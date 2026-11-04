mod common;

use axum::http::StatusCode;
use common::TestApp;
use serde_json::json;
use sqlx::PgPool;

async fn setup(pool: PgPool) -> (TestApp, String, String) {
    let app = TestApp::with_config(pool, |c| c.admin_github_ids = vec![99]);
    let owner = app.publish(1, "alice", "lucky").await;
    let admin = app.login(99, "root").await;
    (app, owner, admin)
}

fn report(target_type: &str, target: &str) -> serde_json::Value {
    json!({ "target_type": target_type, "target": target, "reason": "malware", "text": "It deletes my settings" })
}

#[sqlx::test]
async fn report_rate_limit_and_duplicates(pool: PgPool) {
    let (app, _, _) = setup(pool).await;
    let user = app.login(5, "eve").await;
    assert_eq!(app.post("/v1/reports", None, report("addon", "lucky")).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(app.post("/v1/reports", Some(&user), report("addon", "lucky")).await.status, StatusCode::CREATED);
    let again = app.post("/v1/reports", Some(&user), report("addon", "lucky")).await;
    assert_eq!((again.status, again.error().as_str()), (StatusCode::CONFLICT, "already_reported"));
    assert_eq!(app.post("/v1/reports", Some(&user), report("addon", "ghost")).await.status, StatusCode::NOT_FOUND);
    let mut bad = report("addon", "lucky");
    bad["reason"] = json!("boring");
    assert_eq!(app.post("/v1/reports", Some(&user), bad).await.error(), "invalid_reason");

    for i in 0..9 {
        let slug = format!("other{i}");
        app.publish(10 + i, &format!("dev{i}"), &slug).await;
        assert_eq!(app.post("/v1/reports", Some(&user), report("addon", &slug)).await.status, StatusCode::CREATED);
    }
    app.publish(30, "last", "eleventh").await;
    let limited = app.post("/v1/reports", Some(&user), report("addon", "eleventh")).await;
    assert_eq!((limited.status, limited.error().as_str()), (StatusCode::TOO_MANY_REQUESTS, "report_limit"));
    app.advance(chrono::Duration::days(1) + chrono::Duration::seconds(1));
    assert_eq!(app.post("/v1/reports", Some(&user), report("addon", "eleventh")).await.status, StatusCode::CREATED);
}

#[sqlx::test]
async fn admin_report_queue(pool: PgPool) {
    let (app, owner, admin) = setup(pool).await;
    let eve = app.login(5, "eve").await;
    let mallory = app.login(6, "mallory").await;
    app.post("/v1/reports", Some(&eve), report("addon", "lucky")).await;
    app.advance(chrono::Duration::minutes(1));
    app.post("/v1/reports", Some(&mallory), json!({ "target_type": "addon", "target": "lucky", "reason": "spam" })).await;

    assert_eq!(app.get("/v1/admin/reports", Some(&owner)).await.status, StatusCode::FORBIDDEN);
    let queue = app.get("/v1/admin/reports", Some(&admin)).await.json();
    assert_eq!(queue.as_array().unwrap().len(), 2);
    assert_eq!((queue[0]["reporter"].as_str(), queue[0]["reason"].as_str(), queue[0]["addon"].as_str()), (Some("eve"), Some("malware"), Some("lucky")));
    assert_eq!(queue[0]["text"], "It deletes my settings");

    let id = queue[0]["id"].as_i64().unwrap();
    assert_eq!(app.post(&format!("/v1/admin/reports/{id}/resolve"), Some(&admin), json!({ "status": "dismissed" })).await.error(), "note_required");
    let done = app.post(&format!("/v1/admin/reports/{id}/resolve"), Some(&admin), json!({ "status": "dismissed", "note": "works as intended" })).await;
    assert_eq!(done.status, StatusCode::NO_CONTENT);
    assert_eq!(app.get("/v1/admin/reports", Some(&admin)).await.json().as_array().unwrap().len(), 1);
}

#[sqlx::test]
async fn version_and_image_reports_name_what_was_reported(pool: PgPool) {
    let (app, owner, admin) = setup(pool).await;
    let version: i64 = sqlx::query_scalar(
        "INSERT INTO versions (addon_id, tag, published_at, asset_name, asset_url, created_at)
         SELECT id, 'v1', now(), 'a.zip', 'https://github.com/a.zip', now() FROM addons RETURNING id",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    let image: i64 = sqlx::query_scalar(
        "INSERT INTO images (addon_id, kind, file, width, height, uploaded_by, uploaded_at)
         SELECT a.id, 'screenshot', 'abc.webp', 10, 10, u.id, now() FROM addons a, users u WHERE u.login = 'alice' RETURNING id",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    app.post("/v1/reports", Some(&owner), report("version", &version.to_string())).await;
    app.post("/v1/reports", Some(&owner), report("image", &image.to_string())).await;
    let queue = app.get("/v1/admin/reports", Some(&admin)).await.json();
    assert_eq!(queue[0]["version_tag"], "v1", "{queue}");
    assert!(queue[1]["image_url"].as_str().is_some_and(|u| u.ends_with("/abc.webp")), "{queue}");
    assert!(queue[0]["image_url"].is_null() && queue[1]["version_tag"].is_null());
}

#[sqlx::test]
async fn blocked_hidden_and_trust_blocked(pool: PgPool) {
    let (app, owner, admin) = setup(pool).await;
    sqlx::query(
        "INSERT INTO versions (addon_id, tag, published_at, asset_name, asset_url, created_at)
         SELECT id, 'v1', now(), 'a.zip', 'https://github.com/a.zip', now() FROM addons",
    )
    .execute(&app.pool)
    .await
    .unwrap();
    let version: i64 = sqlx::query_scalar("SELECT id FROM versions").fetch_one(&app.pool).await.unwrap();

    assert_eq!(app.post(&format!("/v1/admin/versions/{version}/block"), Some(&owner), json!({ "note": "x" })).await.status, StatusCode::FORBIDDEN);
    assert_eq!(app.post(&format!("/v1/admin/versions/{version}/block"), Some(&admin), json!({ "note": "steals" })).await.status, StatusCode::NO_CONTENT);
    let versions = app.get("/v1/addons/lucky/versions", None).await.json();
    assert_eq!(versions[0]["trust"], "blocked");

    assert_eq!(app.post("/v1/admin/addons/lucky/block", Some(&admin), json!({ "note": "malware confirmed" })).await.status, StatusCode::NO_CONTENT);
    let page = app.get("/v1/addons/lucky", None).await.json();
    assert_eq!(page, json!({ "slug": "lucky", "name": "Addon lucky", "blocked": true, "block_reason": "malware confirmed" }));

    let unblock = app.call("DELETE", "/v1/admin/addons/lucky/block", Some(&admin), Some(json!({ "note": "false alarm" }))).await;
    assert_eq!(unblock.status, StatusCode::NO_CONTENT);
    assert_eq!(app.get("/v1/addons/lucky", None).await.json()["blocked"], false);
}

#[sqlx::test]
async fn ban_cascades_and_unban_restores_auto_blocks_only(pool: PgPool) {
    let (app, owner, admin) = setup(pool).await;
    app.github.add_repo(101, "alice/second", "alice");
    let created = app.post("/v1/addons", Some(&owner), common::addon_body("second", "alice/second", &["Second"])).await;
    assert_eq!(created.status, StatusCode::CREATED);
    app.post("/v1/admin/addons/second/block", Some(&admin), json!({ "note": "separate problem" })).await;

    assert_eq!(app.post("/v1/admin/users/1/ban", Some(&admin), json!({ "note": "spreads malware" })).await.status, StatusCode::NO_CONTENT);
    assert_eq!(app.get("/v1/me", Some(&owner)).await.status, StatusCode::UNAUTHORIZED, "sessions are gone");
    let blocked: Vec<(String, bool)> =
        sqlx::query_as("SELECT slug::text, blocked_by_ban FROM addons WHERE blocked_at IS NOT NULL ORDER BY slug").fetch_all(&app.pool).await.unwrap();
    assert_eq!(blocked, [("lucky".to_string(), true), ("second".to_string(), false)]);

    let unban = app.call("DELETE", "/v1/admin/users/1/ban", Some(&admin), Some(json!({ "note": "appeal accepted" }))).await;
    assert_eq!(unban.status, StatusCode::NO_CONTENT);
    let blocked: Vec<String> = sqlx::query_scalar("SELECT slug::text FROM addons WHERE blocked_at IS NOT NULL").fetch_all(&app.pool).await.unwrap();
    assert_eq!(blocked, ["second"], "the manual block stays");
}

#[sqlx::test]
async fn actions_need_note_and_are_audited(pool: PgPool) {
    let (app, _, admin) = setup(pool).await;
    assert_eq!(app.post("/v1/admin/addons/lucky/block", Some(&admin), json!({})).await.error(), "note_required");
    assert_eq!(app.post("/v1/admin/users/1/ban", Some(&admin), json!({ "note": " " })).await.error(), "note_required");
    app.post("/v1/admin/addons/lucky/block", Some(&admin), json!({ "note": "a" })).await;
    app.call("DELETE", "/v1/admin/addons/lucky/block", Some(&admin), Some(json!({ "note": "b" }))).await;
    app.post("/v1/admin/users/1/ban", Some(&admin), json!({ "note": "c" })).await;
    app.call("DELETE", "/v1/admin/users/1/ban", Some(&admin), Some(json!({ "note": "d" }))).await;
    let actions: Vec<(String, String)> = sqlx::query_as("SELECT action, note FROM audit_log ORDER BY id").fetch_all(&app.pool).await.unwrap();
    let expected = [("block_addon", "a"), ("unblock_addon", "b"), ("ban_user", "c"), ("unban_user", "d")];
    assert_eq!(actions, expected.map(|(a, n)| (a.to_string(), n.to_string())));
}

#[sqlx::test]
async fn admin_cannot_be_banned(pool: PgPool) {
    let (app, _, admin) = setup(pool).await;
    let reply = app.post("/v1/admin/users/99/ban", Some(&admin), json!({ "note": "oops" })).await;
    assert_eq!((reply.status, reply.error().as_str()), (StatusCode::UNPROCESSABLE_ENTITY, "cannot_ban_admin"));
}
