mod common;

use axum::http::StatusCode;
use common::TestApp;
use serde_json::json;
use sqlx::PgPool;

fn profile(n: i64) -> serde_json::Value {
    json!({ "data": { "addons": [{ "name": "Lucky", "enabled": true }] }, "updated_at": n })
}

#[sqlx::test]
async fn the_list_needs_a_login(pool: PgPool) {
    let app = TestApp::new(pool);
    assert_eq!(app.get("/v1/me/profiles", None).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(app.put("/v1/me/profiles/Raid", None, profile(1)).await.status, StatusCode::UNAUTHORIZED);
    let token = app.login(1, "alice").await;
    let list = app.get("/v1/me/profiles", Some(&token)).await;
    assert_eq!(list.status, StatusCode::OK);
    assert_eq!(list.json(), json!({ "profiles": [] }));
}

#[sqlx::test]
async fn put_creates_and_replaces(pool: PgPool) {
    let app = TestApp::new(pool);
    let token = app.login(1, "alice").await;
    assert_eq!(app.put("/v1/me/profiles/Raid", Some(&token), profile(5)).await.status, StatusCode::OK);
    let changed = json!({ "data": { "addons": [] }, "updated_at": 9 });
    assert_eq!(app.put("/v1/me/profiles/raid", Some(&token), changed).await.status, StatusCode::OK, "any case is the same profile");
    let list = app.get("/v1/me/profiles", Some(&token)).await.json();
    assert_eq!(list, json!({ "profiles": [{ "name": "raid", "data": { "addons": [] }, "updated_at": 9 }] }));
    assert_eq!(app.delete("/v1/me/profiles/RAID", Some(&token)).await.status, StatusCode::NO_CONTENT);
    assert_eq!(app.get("/v1/me/profiles", Some(&token)).await.json(), json!({ "profiles": [] }));
}

#[sqlx::test]
async fn the_21st_profile_is_refused(pool: PgPool) {
    let app = TestApp::new(pool);
    let token = app.login(1, "alice").await;
    for n in 0..20 {
        assert_eq!(app.put(&format!("/v1/me/profiles/P{n}"), Some(&token), profile(n)).await.status, StatusCode::OK);
    }
    let refused = app.put("/v1/me/profiles/P20", Some(&token), profile(1)).await;
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert_eq!(refused.json()["error"], "too_many_profiles");
    assert_eq!(app.put("/v1/me/profiles/P3", Some(&token), profile(30)).await.status, StatusCode::OK, "replacing one still works");
}

#[sqlx::test]
async fn a_write_over_a_newer_one_is_refused(pool: PgPool) {
    let app = TestApp::new(pool);
    let token = app.login(1, "alice").await;
    app.put("/v1/me/profiles/Raid", Some(&token), profile(10)).await;
    let stale = json!({ "data": {}, "updated_at": 12, "base_updated_at": 8 });
    let refused = app.put("/v1/me/profiles/Raid", Some(&token), stale).await;
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert_eq!(refused.json()["error"], "profile_changed");
    let current = json!({ "data": {}, "updated_at": 12, "base_updated_at": 10 });
    assert_eq!(app.put("/v1/me/profiles/Raid", Some(&token), current).await.status, StatusCode::OK);
}

#[sqlx::test]
async fn others_profiles_stay_hidden(pool: PgPool) {
    let app = TestApp::new(pool);
    let alice = app.login(1, "alice").await;
    let bob = app.login(2, "bob").await;
    app.put("/v1/me/profiles/Raid", Some(&alice), profile(1)).await;
    assert_eq!(app.get("/v1/me/profiles", Some(&bob)).await.json(), json!({ "profiles": [] }));
    app.delete("/v1/me/profiles/Raid", Some(&bob)).await;
    assert_eq!(app.get("/v1/me/profiles", Some(&alice)).await.json()["profiles"].as_array().unwrap().len(), 1, "bob cannot delete it");
}

#[sqlx::test]
async fn a_bad_name_or_a_big_profile_is_refused(pool: PgPool) {
    let app = TestApp::new(pool);
    let token = app.login(1, "alice").await;
    let long = "x".repeat(41);
    assert_eq!(app.put(&format!("/v1/me/profiles/{long}"), Some(&token), profile(1)).await.status, StatusCode::BAD_REQUEST);
    assert_eq!(app.put("/v1/me/profiles/%20", Some(&token), profile(1)).await.status, StatusCode::BAD_REQUEST);
    let big = json!({ "data": { "x": "y".repeat(70 * 1024) }, "updated_at": 1 });
    assert_eq!(app.put("/v1/me/profiles/Raid", Some(&token), big).await.status, StatusCode::PAYLOAD_TOO_LARGE);
}
