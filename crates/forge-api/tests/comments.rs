mod common;

use axum::http::StatusCode;
use common::TestApp;
use serde_json::{Value, json};
use forge_api::github::types::User;
use sqlx::PgPool;

/// An addon by alice with comments on, a moderator, and eve as a visitor.
async fn setup(pool: PgPool) -> (TestApp, String, String, String) {
    let app = TestApp::with_config(pool, |c| c.admin_github_ids = vec![99]);
    let owner = app.publish(1, "alice", "lucky").await;
    let on = app.patch("/v1/addons/lucky", Some(&owner), json!({ "comments_enabled": true })).await;
    assert_eq!(on.status, StatusCode::OK, "{}", on.body);
    let admin = app.login(99, "root").await;
    let eve = app.login(5, "eve").await;
    (app, owner, admin, eve)
}

async fn comment(app: &TestApp, token: &str, body: Value) -> i64 {
    let reply = app.post("/v1/addons/lucky/comments", Some(token), body).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.error());
    reply.json()["id"].as_i64().unwrap()
}

async fn list(app: &TestApp, query: &str) -> Vec<Value> {
    app.get(&format!("/v1/addons/lucky/comments{query}"), None).await.json().as_array().unwrap().clone()
}

#[sqlx::test]
async fn anyone_reads_logged_in_users_write_and_reply(pool: PgPool) {
    let (app, owner, _, eve) = setup(pool).await;
    assert_eq!(app.post("/v1/addons/lucky/comments", None, json!({ "body": "hi" })).await.status, StatusCode::UNAUTHORIZED);
    let first = comment(&app, &eve, json!({ "body": "  Works great with the Mage.  " })).await;
    let reply = comment(&app, &owner, json!({ "body": "Thanks!", "parent_id": first })).await;

    let all = list(&app, "").await;
    assert_eq!(all.iter().map(|c| c["id"].as_i64().unwrap()).collect::<Vec<_>>(), vec![reply, first], "newest first");
    assert_eq!(all[1]["body"], "Works great with the Mage.", "stored trimmed");
    assert_eq!(all[1]["author"], "eve");
    assert_eq!(all[0]["parent_id"].as_i64(), Some(first));

    for body in [json!({ "body": "   " }), json!({ "body": "x".repeat(5_001) })] {
        assert_eq!(app.post("/v1/addons/lucky/comments", Some(&eve), body).await.error(), "invalid_comment");
    }
    let elsewhere = app.publish(2, "bob", "other").await;
    app.patch("/v1/addons/other", Some(&elsewhere), json!({ "comments_enabled": true })).await;
    let foreign = app.post("/v1/addons/other/comments", Some(&elsewhere), json!({ "body": "mine" })).await.json()["id"].as_i64().unwrap();
    let wrong = app.post("/v1/addons/lucky/comments", Some(&eve), json!({ "body": "re", "parent_id": foreign })).await;
    assert_eq!(wrong.status, StatusCode::NOT_FOUND, "a reply stays in its addon");
}

#[sqlx::test]
async fn pages_of_thirty_go_back_by_id(pool: PgPool) {
    let (app, _, _, eve) = setup(pool).await;
    for n in 0..32 {
        if n == 20 {
            app.advance(chrono::Duration::hours(1) + chrono::Duration::seconds(1));
        }
        comment(&app, &eve, json!({ "body": format!("comment {n}") })).await;
    }
    let first = list(&app, "").await;
    assert_eq!(first.len(), 31, "one more than a page shows that older comments exist");
    let before = first[29]["id"].as_i64().unwrap();
    let older = list(&app, &format!("?before={before}")).await;
    assert_eq!(older.iter().map(|c| c["body"].as_str().unwrap()).collect::<Vec<_>>(), vec!["comment 1", "comment 0"]);
}

#[sqlx::test]
async fn only_the_author_edits_the_team_and_moderators_delete(pool: PgPool) {
    let (app, owner, admin, eve) = setup(pool).await;
    let id = comment(&app, &eve, json!({ "body": "first try" })).await;
    let path = format!("/v1/addons/lucky/comments/{id}");

    for other in [&owner, &admin] {
        assert_eq!(app.patch(&path, Some(other), json!({ "body": "rewritten" })).await.status, StatusCode::FORBIDDEN, "nobody puts words in her mouth");
    }
    assert_eq!(app.patch(&path, Some(&eve), json!({ "body": "second try" })).await.status, StatusCode::NO_CONTENT);
    let edited = &list(&app, "").await[0];
    assert_eq!(edited["body"], "second try");
    assert!(edited["edited_at"].is_string());

    let mallory = app.login(6, "mallory").await;
    assert_eq!(app.delete(&path, Some(&mallory)).await.status, StatusCode::FORBIDDEN);
    assert_eq!(app.delete(&path, Some(&owner)).await.status, StatusCode::NO_CONTENT);
    let gone = &list(&app, "").await[0];
    assert_eq!((gone["deleted"].as_bool(), gone["body"].as_str()), (Some(true), Some("")), "the text is hidden, the place stays");
    let (body, by): (String, i64) = sqlx::query_as("SELECT body, deleted_by FROM addon_comments WHERE id = $1").bind(id).fetch_one(&app.pool).await.unwrap();
    assert_eq!((body.as_str(), by), ("second try", app.user_id(1).await), "kept for the moderators");
    let audit: (String, String, serde_json::Value) = sqlx::query_as("SELECT action, target_id, details FROM audit_log WHERE target_type = 'comment'")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!((audit.0.as_str(), audit.1), ("delete_comment", id.to_string()));
    assert_eq!((audit.2["addon"].as_str(), audit.2["body"].as_str()), (Some("lucky"), Some("second try")), "the owner deleting someone else's comment is on record");
    assert_eq!(app.patch(&path, Some(&eve), json!({ "body": "back" })).await.status, StatusCode::NOT_FOUND);
    assert_eq!(app.delete(&path, Some(&eve)).await.status, StatusCode::NOT_FOUND);

    // A reply may still go under the deleted comment; deleting one's own leaves no audit entry.
    let own = comment(&app, &eve, json!({ "body": "still here", "parent_id": id })).await;
    assert_eq!(app.delete(&format!("/v1/addons/lucky/comments/{own}"), Some(&eve)).await.status, StatusCode::NO_CONTENT);
    let entries: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE target_type = 'comment'").fetch_one(&app.pool).await.unwrap();
    assert_eq!(entries, 1);
}

#[sqlx::test]
async fn an_editor_of_the_addon_deletes_comments(pool: PgPool) {
    let (app, owner, _, eve) = setup(pool).await;
    app.github.data().users.insert("bob".into(), User { id: 2, login: "bob".into(), avatar_url: String::new() });
    let bob = app.login(2, "bob").await;
    let added = app.post("/v1/addons/lucky/members", Some(&owner), json!({ "login": "bob", "can_edit_description": true })).await;
    assert_eq!(added.status, StatusCode::CREATED, "{}", added.body);
    let id = comment(&app, &eve, json!({ "body": "spam spam" })).await;
    assert_eq!(app.patch(&format!("/v1/addons/lucky/comments/{id}"), Some(&bob), json!({ "body": "x" })).await.status, StatusCode::FORBIDDEN);
    assert_eq!(app.delete(&format!("/v1/addons/lucky/comments/{id}"), Some(&bob)).await.status, StatusCode::NO_CONTENT);
}

#[sqlx::test]
async fn twenty_comments_an_hour(pool: PgPool) {
    let (app, _, _, eve) = setup(pool).await;
    for n in 0..20 {
        comment(&app, &eve, json!({ "body": format!("{n}") })).await;
    }
    let limited = app.post("/v1/addons/lucky/comments", Some(&eve), json!({ "body": "one more" })).await;
    assert_eq!((limited.status, limited.error().as_str()), (StatusCode::TOO_MANY_REQUESTS, "comment_limit"));
    app.advance(chrono::Duration::hours(1) + chrono::Duration::seconds(1));
    comment(&app, &eve, json!({ "body": "an hour later" })).await;
}

#[sqlx::test]
async fn comments_start_off_and_the_owner_switches_them(pool: PgPool) {
    let fresh = TestApp::new(pool.clone());
    fresh.publish(3, "carol", "fresh").await;
    assert_eq!(fresh.get("/v1/addons/fresh/comments", None).await.error(), "comments_off", "off until turned on");
}

#[sqlx::test]
async fn the_owner_turns_comments_off_and_on(pool: PgPool) {
    let (app, owner, _, eve) = setup(pool).await;
    comment(&app, &eve, json!({ "body": "before" })).await;
    let bob = app.login(2, "bob").await;
    assert_eq!(app.patch("/v1/addons/lucky", Some(&bob), json!({ "comments_enabled": false })).await.status, StatusCode::FORBIDDEN);
    let off = app.patch("/v1/addons/lucky", Some(&owner), json!({ "comments_enabled": false })).await;
    assert_eq!((off.status, off.json()["comments_enabled"].as_bool()), (StatusCode::OK, Some(false)));
    for reply in [app.get("/v1/addons/lucky/comments", None).await, app.post("/v1/addons/lucky/comments", Some(&eve), json!({ "body": "after" })).await] {
        assert_eq!((reply.status, reply.error().as_str()), (StatusCode::FORBIDDEN, "comments_off"));
    }
    app.patch("/v1/addons/lucky", Some(&owner), json!({ "comments_enabled": true })).await;
    assert_eq!(list(&app, "").await.len(), 1, "the comments come back");
}

#[sqlx::test]
async fn a_blocked_addon_has_no_discussion(pool: PgPool) {
    let (app, _, admin, eve) = setup(pool).await;
    comment(&app, &eve, json!({ "body": "before" })).await;
    app.post("/v1/admin/addons/lucky/block", Some(&admin), json!({ "note": "malware confirmed" })).await;
    assert_ne!(app.get("/v1/addons/lucky/comments", None).await.status, StatusCode::OK);
    assert_ne!(app.post("/v1/addons/lucky/comments", Some(&eve), json!({ "body": "after" })).await.status, StatusCode::CREATED);
}

#[sqlx::test]
async fn comments_are_reported_into_the_admin_queue(pool: PgPool) {
    let (app, _, admin, eve) = setup(pool).await;
    let id = comment(&app, &eve, json!({ "body": "Buy gold at example.test" })).await;
    let mallory = app.login(6, "mallory").await;
    let report = json!({ "target_type": "comment", "target": id.to_string(), "reason": "spam" });
    assert_eq!(app.post("/v1/reports", Some(&mallory), report.clone()).await.status, StatusCode::CREATED);
    assert_eq!(app.post("/v1/reports", Some(&mallory), report).await.error(), "already_reported");

    let queue = app.get("/v1/admin/reports", Some(&admin)).await.json();
    let entry = &queue.as_array().unwrap()[0];
    assert_eq!((entry["target_type"].as_str(), entry["addon"].as_str()), (Some("comment"), Some("lucky")));
    assert_eq!(entry["comment"], "Buy gold at example.test");

    app.delete(&format!("/v1/addons/lucky/comments/{id}"), Some(&admin)).await;
    assert_eq!(app.get("/v1/admin/reports", Some(&admin)).await.json()[0]["comment"], "", "deleted since");
    let late = app.post("/v1/reports", Some(&eve), json!({ "target_type": "comment", "target": id.to_string(), "reason": "spam" })).await;
    assert_eq!(late.status, StatusCode::NOT_FOUND, "a deleted comment is not reported again");
}
