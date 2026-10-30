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

/// A scanned version; `flagged` adds a warn finding.
async fn version(app: &TestApp, tag: &str, days_ago: i32, flagged: bool) -> i64 {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO versions (addon_id, tag, published_at, asset_name, asset_url, sha256, scan_status, created_at)
         SELECT id, $1, now() - make_interval(days => $2), 'a.zip', 'https://github.com/a.zip', 'abc', 'done', now() - make_interval(days => $2)
         FROM addons WHERE slug = 'lucky' RETURNING id",
    )
    .bind(tag)
    .bind(days_ago)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    let scan: i64 = sqlx::query_scalar(
        "INSERT INTO scans (version_id, sha256, scanner_version, started_at, finished_at, result) VALUES ($1, 'abc', 'x', now(), now(), 'clean') RETURNING id",
    )
    .bind(id)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    if flagged {
        sqlx::query("INSERT INTO findings (scan_id, rule, severity, file, line, message, snippet) VALUES ($1, 'lua.url', 'warn', 'a.lua', 3, 'URL', 'x')")
            .bind(scan)
            .execute(&app.pool)
            .await
            .unwrap();
    }
    id
}

#[sqlx::test]
async fn queue_contents_and_order(pool: PgPool) {
    let (app, owner, admin) = setup(pool).await;
    let v1 = version(&app, "v1", 3, false).await;
    assert_eq!(app.get("/v1/admin/review-queue", Some(&admin)).await.json(), json!([]), "a clean first version waits for nobody");

    app.post(&format!("/v1/admin/versions/{v1}/verification"), Some(&admin), json!({})).await;
    let v2 = version(&app, "v2", 2, true).await;
    let v3 = version(&app, "v3", 1, false).await;
    let queue = app.get("/v1/admin/review-queue", Some(&admin)).await.json();
    let ids: Vec<i64> = queue.as_array().unwrap().iter().map(|q| q["version_id"].as_i64().unwrap()).collect();
    assert_eq!(ids, [v2, v3], "flagged, then a new version of a verified author; oldest first");
    assert_eq!(queue[0]["findings"][0]["rule"], "lua.url");
    assert_eq!(queue[0]["compare_url"], "https://github.com/alice/lucky/compare/v1...v2");
    assert_eq!(app.get("/v1/admin/review-queue", Some(&owner)).await.status, StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn skip_takes_a_version_off_the_queue_until_its_trust_changes(pool: PgPool) {
    let (app, owner, admin) = setup(pool).await;
    let v1 = version(&app, "v1", 3, false).await;
    app.post(&format!("/v1/admin/versions/{v1}/verification"), Some(&admin), json!({})).await;
    let v2 = version(&app, "v2", 2, false).await;
    assert_eq!(app.post(&format!("/v1/admin/versions/{v2}/skip"), Some(&owner), json!({})).await.status, StatusCode::FORBIDDEN);
    assert_eq!(app.post("/v1/admin/versions/9999/skip", Some(&admin), json!({})).await.status, StatusCode::NOT_FOUND);

    assert_eq!(app.post(&format!("/v1/admin/versions/{v2}/skip"), Some(&admin), json!({ "note": "looked fine" })).await.status, StatusCode::NO_CONTENT);
    assert_eq!(app.get("/v1/admin/review-queue", Some(&admin)).await.json(), json!([]));
    let trust: String = sqlx::query_scalar("SELECT trust FROM version_trust WHERE version_id = $1").bind(v2).fetch_one(&app.pool).await.unwrap();
    assert_eq!(trust, "scanned", "a skip does not verify");
    let audit = app.get("/v1/admin/audit?action=skip_version", Some(&admin)).await.json();
    assert_eq!((audit["total"].as_i64(), audit["entries"][0]["note"].as_str()), (Some(1), Some("looked fine")));

    // A new finding makes it flagged: the skip no longer holds.
    sqlx::query("INSERT INTO findings (scan_id, rule, severity, file, line, message, snippet) SELECT id, 'lua.url', 'warn', 'a.lua', 1, 'URL', 'x' FROM scans WHERE version_id = $1")
        .bind(v2)
        .execute(&app.pool)
        .await
        .unwrap();
    let queue = app.get("/v1/admin/review-queue", Some(&admin)).await.json();
    assert_eq!(queue[0]["version_id"].as_i64(), Some(v2));
}

#[sqlx::test]
async fn audit_filters_and_paging(pool: PgPool) {
    let (app, _, admin) = setup(pool).await;
    for i in 0..55 {
        app.post("/v1/admin/categories", Some(&admin), json!({ "name": format!("Cat{i}"), "sort_key": i })).await;
    }
    app.post("/v1/admin/addons/lucky/block", Some(&admin), json!({ "note": "x" })).await;
    let first = app.get("/v1/admin/audit", Some(&admin)).await.json();
    assert_eq!((first["total"].as_i64(), first["entries"].as_array().unwrap().len()), (Some(56), 50));
    assert_eq!(first["entries"][0]["action"], "block_addon", "newest first");
    assert_eq!(app.get("/v1/admin/audit?page=2", Some(&admin)).await.json()["entries"].as_array().unwrap().len(), 6);
    let blocks = app.get("/v1/admin/audit?action=block_addon&admin=root", Some(&admin)).await.json();
    assert_eq!(blocks["total"], 1);
    assert_eq!(app.get("/v1/admin/audit?admin=nobody", Some(&admin)).await.json()["total"], 0);
}

#[sqlx::test]
async fn categories_crud(pool: PgPool) {
    let (app, owner, admin) = setup(pool).await;
    assert_eq!(app.post("/v1/admin/categories", Some(&owner), json!({ "name": "Maps" })).await.status, StatusCode::FORBIDDEN);
    assert_eq!(app.post("/v1/admin/categories", Some(&admin), json!({ "name": "Maps", "sort_key": 0 })).await.status, StatusCode::CREATED);
    assert_eq!(app.post("/v1/admin/categories", Some(&admin), json!({ "name": "maps" })).await.error(), "name_taken");
    assert_eq!(app.patch("/v1/admin/categories/Unit%20Frames", Some(&admin), json!({ "name": "Interface" })).await.status, StatusCode::NO_CONTENT);
    assert_eq!(app.get("/v1/addons/lucky", None).await.json()["category"], "Interface", "addons follow a rename");
    assert_eq!(app.delete("/v1/admin/categories/Interface", Some(&admin)).await.error(), "category_in_use");
    assert_eq!(app.delete("/v1/admin/categories/Maps", Some(&admin)).await.status, StatusCode::NO_CONTENT);
    assert_eq!(app.delete("/v1/admin/categories/Classes", Some(&admin)).await.error(), "category_has_subcategories");

    // One level of subcategories, and none under a category that holds addons.
    let sub = |name: &str, parent: &str| json!({ "name": name, "parent": parent });
    assert_eq!(app.post("/v1/admin/categories", Some(&admin), sub("Death Knight", "Classes")).await.status, StatusCode::CREATED);
    assert_eq!(app.post("/v1/admin/categories", Some(&admin), sub("Frost", "Death Knight")).await.error(), "invalid_parent");
    assert_eq!(app.post("/v1/admin/categories", Some(&admin), sub("Bars", "Interface")).await.error(), "category_in_use");
    assert_eq!(app.post("/v1/admin/categories", Some(&admin), sub("Bars", "Nope")).await.error(), "invalid_parent");
    assert_eq!(app.patch("/v1/admin/categories/Classes", Some(&admin), json!({ "parent": "Combat" })).await.error(), "invalid_parent");
    assert_eq!(app.patch("/v1/admin/categories/Death%20Knight", Some(&admin), json!({ "parent": "" })).await.status, StatusCode::NO_CONTENT);
    let list = app.get("/v1/categories", None).await.json();
    let dk = list.as_array().unwrap().iter().find(|c| c["name"] == "Death Knight").unwrap();
    assert_eq!(dk["parent"], serde_json::Value::Null, "moved to the top");
    assert!(list.as_array().unwrap().iter().any(|c| c["name"] == "Interface"));
}

#[sqlx::test]
async fn user_lookup(pool: PgPool) {
    let (app, owner, admin) = setup(pool).await;
    let user = app.get("/v1/admin/users/ALICE", Some(&admin)).await.json();
    assert_eq!((user["github_id"].as_i64(), user["addons"].as_i64(), user["banned"].as_bool()), (Some(1), Some(1), Some(false)));
    assert_eq!(app.get("/v1/admin/users/ghost", Some(&admin)).await.status, StatusCode::NOT_FOUND);
    assert_eq!(app.get("/v1/admin/users/alice", Some(&owner)).await.status, StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn admins_grant_and_revoke_admin_rights(pool: PgPool) {
    let (app, owner, admin) = setup(pool).await;
    let alice = app.get("/v1/me", Some(&owner)).await.json();
    let alice_id = alice["github_id"].as_i64().unwrap();
    let grant = |id: i64, admin_flag: bool| json!({ "role": if admin_flag { "admin" } else { "user" }, "note": format!("set {id}") });

    assert_eq!(app.put(&format!("/v1/admin/users/{alice_id}/role"), Some(&owner), grant(alice_id, true)).await.status, StatusCode::FORBIDDEN);
    assert_eq!(app.put(&format!("/v1/admin/users/{alice_id}/role"), Some(&admin), json!({ "role": "admin" })).await.error(), "note_required");
    assert_eq!(app.put(&format!("/v1/admin/users/{alice_id}/role"), Some(&admin), grant(alice_id, true)).await.status, StatusCode::NO_CONTENT);
    assert_eq!(app.get("/v1/me", Some(&owner)).await.json()["role"], "admin", "at once, without a new login");

    // The grant survives a new login, unlike a role that only came from ADMIN_GITHUB_IDS.
    let again = app.login(1, "alice").await;
    assert_eq!(app.get("/v1/me", Some(&again)).await.json()["role"], "admin");
    let admins: Vec<String> = app.get("/v1/admin/admins", Some(&admin)).await.json().as_array().unwrap().iter().map(|a| a["login"].as_str().unwrap().to_string()).collect();
    assert_eq!(admins, ["alice", "root"]);

    assert_eq!(app.put("/v1/admin/users/99/role", Some(&again), grant(99, false)).await.error(), "admin_by_config", "set in ADMIN_GITHUB_IDS");
    assert_eq!(app.put(&format!("/v1/admin/users/{alice_id}/role"), Some(&again), grant(alice_id, false)).await.error(), "cannot_demote_self");
    assert_eq!(app.put(&format!("/v1/admin/users/{alice_id}/role"), Some(&admin), grant(alice_id, false)).await.status, StatusCode::NO_CONTENT);
    assert_eq!(app.get("/v1/me", Some(&again)).await.json()["role"], "user");
    assert_eq!(app.put("/v1/admin/users/12345/role", Some(&admin), grant(12345, true)).await.status, StatusCode::NOT_FOUND);

    let audit = app.get("/v1/admin/audit?action=user_role", Some(&admin)).await.json();
    assert_eq!(audit["entries"].as_array().map(Vec::len), Some(2), "{audit}");
}
