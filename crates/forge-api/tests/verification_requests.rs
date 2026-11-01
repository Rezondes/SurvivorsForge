mod common;

use axum::http::StatusCode;
use common::TestApp;
use serde_json::json;
use sqlx::PgPool;

const CHANNEL: &str = "https://discord.com/channels/1557821819032051852/1564402358094405744";

/// A scanned version of `lucky`; returns its id.
async fn scanned_version(app: &TestApp, tag: &str, flagged: bool) -> i64 {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO versions (addon_id, tag, published_at, asset_name, asset_url, sha256, scan_status, created_at)
         SELECT id, $1, now(), 'a.zip', 'https://github.com/a.zip', 'abc', 'done', now() FROM addons WHERE slug = 'lucky' RETURNING id",
    )
    .bind(tag)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    let scan: i64 = sqlx::query_scalar(
        "INSERT INTO scans (version_id, sha256, scanner_version, started_at, finished_at, result) VALUES ($1, 'abc', 'x', now(), now(), $2) RETURNING id",
    )
    .bind(id)
    .bind(if flagged { "flagged" } else { "clean" })
    .fetch_one(&app.pool)
    .await
    .unwrap();
    if flagged {
        sqlx::query("INSERT INTO findings (scan_id, rule, severity, file, message) VALUES ($1, 'lua.url', 'warn', 'a.lua', 'm')")
            .bind(scan)
            .execute(&app.pool)
            .await
            .unwrap();
    }
    id
}

async fn trust(app: &TestApp, id: i64) -> String {
    sqlx::query_scalar("SELECT trust FROM version_trust WHERE version_id = $1").bind(id).fetch_one(&app.pool).await.unwrap()
}

async fn setup(pool: PgPool) -> (TestApp, String, String) {
    let app = TestApp::with_config(pool, |c| c.admin_github_ids = vec![99]);
    let owner = app.publish(1, "alice", "lucky").await;
    let admin = app.login(99, "root").await;
    (app, owner, admin)
}

#[sqlx::test]
async fn owners_ask_with_a_link_to_the_discord_channel(pool: PgPool) {
    let (app, owner, admin) = setup(pool).await;
    scanned_version(&app, "v1", false).await;
    let v2 = scanned_version(&app, "v2", false).await;
    let link = format!("{CHANNEL}/1570000000000000000");

    let reply = app.post("/v1/addons/lucky/verification-request", Some(&owner), json!({ "discord_url": link })).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.error());
    assert_eq!(reply.json()["status"], "open");
    assert_eq!(reply.json()["tag"], "v2", "the newest checked version");

    let mine = app.get("/v1/addons/lucky/verification-request", Some(&owner)).await;
    assert_eq!((mine.status, mine.json()["status"].clone()), (StatusCode::OK, json!("open")));

    let queue = app.get("/v1/admin/verification-requests", Some(&admin)).await.json();
    let open = queue.as_array().unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!((open[0]["slug"].clone(), open[0]["tag"].clone(), open[0]["discord_url"].clone()), (json!("lucky"), json!("v2"), json!(link)));
    assert_eq!(open[0]["requested_by"], "alice");
    assert_eq!(open[0]["version_id"], v2);

    let again = app.post("/v1/addons/lucky/verification-request", Some(&owner), json!({ "discord_url": CHANNEL })).await;
    assert_eq!((again.status, again.error()), (StatusCode::CONFLICT, "request_open".into()));
}

#[sqlx::test]
async fn only_links_to_the_verified_addons_channel_count(pool: PgPool) {
    let (app, owner, _) = setup(pool).await;
    scanned_version(&app, "v1", false).await;
    for bad in ["https://example.com/x", "https://discord.com/channels/1557821819032051852/1", "http://discord.com/channels/1557821819032051852/1564402358094405744", &format!("{CHANNEL}/abc"), &format!("{CHANNEL}x"), ""] {
        let reply = app.post("/v1/addons/lucky/verification-request", Some(&owner), json!({ "discord_url": bad })).await;
        assert_eq!((reply.status, reply.error()), (StatusCode::BAD_REQUEST, "invalid_link".into()), "{bad}");
    }
    let ok = app.post("/v1/addons/lucky/verification-request", Some(&owner), json!({ "discord_url": CHANNEL })).await;
    assert_eq!(ok.status, StatusCode::CREATED, "the channel itself is fine: {}", ok.error());
}

#[sqlx::test]
async fn only_the_owner_asks_and_only_for_checked_unverified_addons(pool: PgPool) {
    let (app, owner, admin) = setup(pool).await;
    let stranger = app.login(2, "bob").await;
    let none = app.post("/v1/addons/lucky/verification-request", Some(&owner), json!({ "discord_url": CHANNEL })).await;
    assert_eq!((none.status, none.error()), (StatusCode::CONFLICT, "no_checked_version".into()));

    let v1 = scanned_version(&app, "v1", false).await;
    let reply = app.post("/v1/addons/lucky/verification-request", Some(&stranger), json!({ "discord_url": CHANNEL })).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert_eq!(app.get("/v1/admin/verification-requests", Some(&stranger)).await.status, StatusCode::FORBIDDEN);

    app.post(&format!("/v1/admin/versions/{v1}/verification"), Some(&admin), json!({})).await;
    let done = app.post("/v1/addons/lucky/verification-request", Some(&owner), json!({ "discord_url": CHANNEL })).await;
    assert_eq!((done.status, done.error()), (StatusCode::CONFLICT, "already_verified".into()));
}

#[sqlx::test]
async fn admins_approve_by_verifying_the_version(pool: PgPool) {
    let (app, owner, admin) = setup(pool).await;
    let v1 = scanned_version(&app, "v1", false).await;
    let id = app.post("/v1/addons/lucky/verification-request", Some(&owner), json!({ "discord_url": CHANNEL })).await.json()["id"].as_i64().unwrap();

    let reply = app.post(&format!("/v1/admin/verification-requests/{id}/approve"), Some(&admin), json!({})).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.error());
    assert_eq!(trust(&app, v1).await, "verified");
    assert_eq!(app.get("/v1/admin/verification-requests", Some(&admin)).await.json(), json!([]));
    assert_eq!(app.get("/v1/addons/lucky/verification-request", Some(&owner)).await.json()["status"], "approved");
    let audited: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = 'approve_verification_request'").fetch_one(&app.pool).await.unwrap();
    assert_eq!(audited, 1);

    let twice = app.post(&format!("/v1/admin/verification-requests/{id}/approve"), Some(&admin), json!({})).await;
    assert_eq!(twice.status, StatusCode::NOT_FOUND, "decided already");
}

#[sqlx::test]
async fn a_flagged_version_needs_a_note_to_be_approved(pool: PgPool) {
    let (app, owner, admin) = setup(pool).await;
    let v1 = scanned_version(&app, "v1", true).await;
    let id = app.post("/v1/addons/lucky/verification-request", Some(&owner), json!({ "discord_url": CHANNEL })).await.json()["id"].as_i64().unwrap();
    let reply = app.post(&format!("/v1/admin/verification-requests/{id}/approve"), Some(&admin), json!({})).await;
    assert_eq!((reply.status, reply.error()), (StatusCode::UNPROCESSABLE_ENTITY, "note_required".into()));
    let reply = app.post(&format!("/v1/admin/verification-requests/{id}/approve"), Some(&admin), json!({ "note": "read it" })).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    assert_eq!(trust(&app, v1).await, "verified");
}

#[sqlx::test]
async fn admins_reject_with_a_reason_and_owners_may_ask_again(pool: PgPool) {
    let (app, owner, admin) = setup(pool).await;
    let v1 = scanned_version(&app, "v1", false).await;
    let id = app.post("/v1/addons/lucky/verification-request", Some(&owner), json!({ "discord_url": CHANNEL })).await.json()["id"].as_i64().unwrap();

    let reply = app.post(&format!("/v1/admin/verification-requests/{id}/reject"), Some(&admin), json!({ "note": " " })).await;
    assert_eq!((reply.status, reply.error()), (StatusCode::UNPROCESSABLE_ENTITY, "note_required".into()));
    let reply = app.post(&format!("/v1/admin/verification-requests/{id}/reject"), Some(&admin), json!({ "note": "Post the whole changelog first" })).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    assert_eq!(trust(&app, v1).await, "scanned");
    let mine = app.get("/v1/addons/lucky/verification-request", Some(&owner)).await.json();
    assert_eq!((mine["status"].clone(), mine["note"].clone()), (json!("rejected"), json!("Post the whole changelog first")));

    let again = app.post("/v1/addons/lucky/verification-request", Some(&owner), json!({ "discord_url": CHANNEL })).await;
    assert_eq!(again.status, StatusCode::CREATED);
}

#[sqlx::test]
async fn without_a_request_the_owner_sees_none(pool: PgPool) {
    let (app, owner, _) = setup(pool).await;
    let reply = app.get("/v1/addons/lucky/verification-request", Some(&owner)).await;
    assert_eq!((reply.status, reply.json()), (StatusCode::OK, json!(null)));
}
