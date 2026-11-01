mod common;

use axum::http::StatusCode;
use common::TestApp;
use serde_json::{Value, json};
use sqlx::PgPool;

fn addon(slug: &str, repo: &str, folders: &[&str]) -> Value {
    json!({
        "slug": slug, "name": format!("Addon {slug}"), "repo": repo, "source_type": "release",
        "asset_pattern": format!("{slug}-v*.zip"), "category": "Unit Frames", "summary": "Short.", "folders": folders,
    })
}

/// A logged-in creator who is admin of `repo` with the given id.
async fn creator(app: &TestApp, github_id: i64, login: &str, repo_id: i64, repo: &str) -> String {
    app.github.add_repo(repo_id, repo, login);
    app.login(github_id, login).await
}

#[sqlx::test]
async fn repos_of_the_own_account_need_no_app(pool: PgPool) {
    // GitHub logged her in as this account, so its repos are hers; the owner's id counts, not a name.
    let app = TestApp::new(pool);
    let alice = app.login(1, "alice").await;
    app.github.add_own_repo(20, "alice/lucky", 1);
    let reply = app.post("/v1/addons", Some(&alice), addon("lucky", "alice/lucky", &["Lucky"])).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    assert_eq!(reply.json()["owner"]["login"], "alice");

    app.github.add_own_repo(21, "alice/renamed", 7);
    let reply = app.post("/v1/addons", Some(&alice), addon("renamed", "alice/renamed", &["Renamed"])).await;
    assert_eq!(reply.error(), "app_not_installed", "same name, other account");

    // An addon an admin added from her repo: she claims it without the App too.
    app.github.add_own_repo(22, "alice/orphan", 1);
    app.post("/v1/addons", Some(&alice), addon("orphan", "alice/orphan", &["Orphan"])).await;
    sqlx::query("UPDATE addons SET owner_id = NULL, installation_id = NULL WHERE slug = 'orphan'").execute(&app.pool).await.unwrap();
    let reply = app.post("/v1/addons/orphan/claim", Some(&alice), json!({})).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
}

#[sqlx::test]
async fn create_requires_installation_public_repo_and_admin(pool: PgPool) {
    let app = TestApp::new(pool);
    let token = app.login(1, "alice").await;

    let reply = app.post("/v1/addons", Some(&token), addon("lucky", "alice/lucky", &["Lucky"])).await;
    assert_eq!((reply.status, reply.error().as_str()), (StatusCode::UNPROCESSABLE_ENTITY, "repo_not_found"));

    // A repo of another account: the App and admin rights there prove it is hers.
    app.github.add_own_repo(11, "bob/other", 2);
    let reply = app.post("/v1/addons", Some(&token), addon("other", "bob/other", &["Other"])).await;
    assert_eq!((reply.status, reply.error().as_str()), (StatusCode::UNPROCESSABLE_ENTITY, "app_not_installed"));

    app.github.add_repo(10, "alice/lucky", "someone-else");
    let reply = app.post("/v1/addons", Some(&token), addon("lucky", "alice/lucky", &["Lucky"])).await;
    assert_eq!((reply.status, reply.error().as_str()), (StatusCode::FORBIDDEN, "not_repo_admin"));

    app.github.data().permissions.insert((10, "alice".into()), "admin".into());
    app.github.data().repos.get_mut(&10).unwrap().private = true;
    let reply = app.post("/v1/addons", Some(&token), addon("lucky", "alice/lucky", &["Lucky"])).await;
    assert_eq!((reply.status, reply.error().as_str()), (StatusCode::UNPROCESSABLE_ENTITY, "repo_private"));

    app.github.data().repos.get_mut(&10).unwrap().private = false;
    let reply = app.post("/v1/addons", Some(&token), addon("lucky", "alice/lucky", &["Lucky"])).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    let view = reply.json();
    assert_eq!(view["slug"], "lucky");
    assert_eq!(view["repo"], "alice/lucky");
    assert_eq!(view["folders"], json!(["Lucky"]));
    assert_eq!(view["owner"]["login"], "alice");

    let repo_id: i64 = sqlx::query_scalar("SELECT repo_id FROM addons WHERE slug = $1").bind("lucky").fetch_one(&app.pool).await.unwrap();
    assert_eq!(repo_id, 10, "stored by numeric repo id");
    assert_eq!(app.post("/v1/addons", None, addon("x", "a/b", &["X"])).await.status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn one_addon_per_repo(pool: PgPool) {
    let app = TestApp::new(pool);
    let token = creator(&app, 1, "alice", 10, "alice/lucky").await;
    assert_eq!(app.post("/v1/addons", Some(&token), addon("lucky", "alice/lucky", &["Lucky"])).await.status, StatusCode::CREATED);
    let reply = app.post("/v1/addons", Some(&token), addon("lucky2", "alice/lucky", &["Lucky2"])).await;
    assert_eq!((reply.status, reply.error().as_str()), (StatusCode::CONFLICT, "repo_taken"));
}

#[sqlx::test]
async fn slug_taken_is_a_conflict(pool: PgPool) {
    let app = TestApp::new(pool);
    let a = creator(&app, 1, "alice", 10, "alice/lucky").await;
    let b = creator(&app, 2, "bob", 20, "bob/lucky").await;
    app.post("/v1/addons", Some(&a), addon("lucky", "alice/lucky", &["Lucky"])).await;
    let reply = app.post("/v1/addons", Some(&b), addon("lucky", "bob/lucky", &["Other"])).await;
    assert_eq!((reply.status, reply.error().as_str()), (StatusCode::CONFLICT, "slug_taken"));
}

#[sqlx::test]
async fn folder_owned_by_other_addon_is_rejected_case_insensitive(pool: PgPool) {
    let app = TestApp::new(pool);
    let a = creator(&app, 1, "alice", 10, "alice/lucky").await;
    let b = creator(&app, 2, "bob", 20, "bob/copy").await;
    assert_eq!(app.post("/v1/addons", Some(&a), addon("lucky", "alice/lucky", &["LuckyOdds"])).await.status, StatusCode::CREATED);
    let reply = app.post("/v1/addons", Some(&b), addon("copy", "bob/copy", &["Mine", "luckyodds"])).await;
    assert_eq!((reply.status, reply.error().as_str()), (StatusCode::CONFLICT, "folder_taken"));
    assert!(reply.json()["message"].as_str().unwrap().to_lowercase().contains("luckyodds"));

    // Changing folders later runs into the same wall.
    assert_eq!(app.post("/v1/addons", Some(&b), addon("copy", "bob/copy", &["Mine"])).await.status, StatusCode::CREATED);
    let reply = app.patch("/v1/addons/copy", Some(&b), json!({ "folders": ["Mine", "LUCKYODDS"] })).await;
    assert_eq!(reply.error(), "folder_taken");
}

#[sqlx::test]
async fn folder_that_windows_reads_as_another_is_rejected(pool: PgPool) {
    let app = TestApp::new(pool);
    let token = creator(&app, 1, "alice", 10, "alice/lucky").await;
    // Windows drops the trailing dot: "Bagnon." would be the folder of the addon Bagnon.
    for folder in ["Bagnon.", "NUL", "com1.lua", "a:b"] {
        let reply = app.post("/v1/addons", Some(&token), addon("lucky", "alice/lucky", &[folder])).await;
        assert_eq!(reply.error(), "invalid_folder", "{folder}");
    }
    let reply = app.post("/v1/addons", Some(&token), addon("lucky", "alice/lucky", &["Bagnon", "bagnon"])).await;
    assert_eq!(reply.error(), "invalid_folder");
}

#[sqlx::test]
async fn reserved_and_invalid_input_is_rejected(pool: PgPool) {
    let app = TestApp::new(pool);
    let token = creator(&app, 1, "alice", 10, "alice/lucky").await;
    let reply = app.post("/v1/addons", Some(&token), addon("lucky", "alice/lucky", &["Blizzard_RaidUI"])).await;
    assert_eq!(reply.error(), "folder_reserved");
    let mut body = addon("lucky", "alice/lucky", &["Lucky"]);
    body["category"] = json!("Nope");
    assert_eq!(app.post("/v1/addons", Some(&token), body).await.error(), "unknown_category");
    let mut body = addon("lucky", "alice/lucky", &["Lucky"]);
    body["slug"] = json!("Bad Slug");
    assert_eq!(app.post("/v1/addons", Some(&token), body).await.error(), "invalid_slug");
    let mut body = addon("lucky", "alice/lucky", &["Lucky"]);
    body["asset_pattern"] = json!("Lucky.rar");
    assert_eq!(app.post("/v1/addons", Some(&token), body).await.error(), "invalid_asset_pattern");
}

#[sqlx::test]
async fn only_owner_changes_owner_fields(pool: PgPool) {
    let app = TestApp::new(pool);
    let owner = creator(&app, 1, "alice", 10, "alice/lucky").await;
    let other = app.login(2, "bob").await;
    app.post("/v1/addons", Some(&owner), addon("lucky", "alice/lucky", &["Lucky"])).await;

    for change in [json!({ "name": "Mine" }), json!({ "category": "Miscellaneous" }), json!({ "folders": ["X"] }), json!({ "asset_pattern": "x*.zip" })] {
        let reply = app.patch("/v1/addons/lucky", Some(&other), change.clone()).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{change}");
    }
    assert_eq!(app.delete("/v1/addons/lucky", Some(&other)).await.status, StatusCode::FORBIDDEN);

    let change = json!({ "name": "Lucky Odds", "category": "Miscellaneous", "folders": ["Lucky", "LuckyData"] });
    let reply = app.patch("/v1/addons/lucky", Some(&owner), change).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let view = reply.json();
    assert_eq!(view["name"], "Lucky Odds");
    assert_eq!(view["category"], "Miscellaneous");
    assert_eq!(view["folders"], json!(["Lucky", "LuckyData"]));
}

#[sqlx::test]
async fn soft_delete_frees_slug_and_folders(pool: PgPool) {
    let app = TestApp::new(pool);
    let a = creator(&app, 1, "alice", 10, "alice/lucky").await;
    let b = creator(&app, 2, "bob", 20, "bob/lucky").await;
    app.post("/v1/addons", Some(&a), addon("lucky", "alice/lucky", &["Lucky"])).await;
    assert_eq!(app.delete("/v1/addons/lucky", Some(&a)).await.status, StatusCode::NO_CONTENT);
    assert_eq!(app.get("/v1/addons/lucky", None).await.status, StatusCode::NOT_FOUND);
    let reply = app.post("/v1/addons", Some(&b), addon("lucky", "bob/lucky", &["Lucky"])).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    assert_eq!(app.get("/v1/addons/lucky", None).await.json()["owner"]["login"], "bob");
}

#[sqlx::test]
async fn description_is_served_as_sanitized_html_and_limited(pool: PgPool) {
    let app = TestApp::new(pool);
    let token = app.publish(1, "alice", "lucky").await;
    let md = "# Lucky\n\n<script>alert(1)</script>![x](https://web.test/img/a.webp)";
    let view = app.patch("/v1/addons/lucky", Some(&token), json!({ "description_md": md })).await.json();
    assert_eq!(view["description_md"], md);
    let html = view["description_html"].as_str().unwrap();
    assert!(html.contains("<h2>Lucky</h2>"), "{html}");
    assert!(html.contains("https://web.test/img/a.webp"), "{html}");
    assert!(!html.contains("<script"), "{html}");

    let reply = app.patch("/v1/addons/lucky", Some(&token), json!({ "description_md": "x".repeat(50_001) })).await;
    assert_eq!((reply.status, reply.error().as_str()), (StatusCode::UNPROCESSABLE_ENTITY, "description_too_long"));
    assert_eq!(app.patch("/v1/addons/lucky", Some(&token), json!({ "description_md": "x".repeat(50_000) })).await.status, StatusCode::OK);
}

#[sqlx::test]
async fn banned_user_and_blocked_addon_are_read_only(pool: PgPool) {
    let app = TestApp::new(pool);
    let token = creator(&app, 1, "alice", 10, "alice/lucky").await;
    app.post("/v1/addons", Some(&token), addon("lucky", "alice/lucky", &["Lucky"])).await;

    sqlx::query("UPDATE addons SET blocked_at = now()").execute(&app.pool).await.unwrap();
    let reply = app.patch("/v1/addons/lucky", Some(&token), json!({ "summary": "x" })).await;
    assert_eq!((reply.status, reply.error().as_str()), (StatusCode::FORBIDDEN, "addon_blocked"));

    sqlx::query("UPDATE addons SET blocked_at = NULL").execute(&app.pool).await.unwrap();
    sqlx::query("UPDATE users SET banned_at = now()").execute(&app.pool).await.unwrap();
    let reply = app.patch("/v1/addons/lucky", Some(&token), json!({ "summary": "x" })).await;
    assert_eq!((reply.status, reply.error().as_str()), (StatusCode::FORBIDDEN, "banned"));
}

#[sqlx::test]
async fn the_long_description_can_come_with_the_new_addon(pool: PgPool) {
    let app = TestApp::new(pool);
    app.github.add_repo(100, "alice/lucky", "alice");
    let token = app.login(1, "alice").await;
    let mut body = addon("lucky", "alice/lucky", &["Lucky"]);
    body["description_md"] = json!("## Features\n\n- Odds on the minimap");
    assert_eq!(app.post("/v1/addons", Some(&token), body.clone()).await.status, StatusCode::CREATED);
    let shown = app.get("/v1/addons/lucky", None).await.json();
    assert_eq!(shown["description_md"], "## Features\n\n- Odds on the minimap");
    assert!(shown["description_html"].as_str().unwrap().contains("<h3>Features</h3>"), "{shown}");

    app.github.add_repo(101, "alice/big", "alice");
    let mut big = addon("big", "alice/big", &["Big"]);
    big["description_md"] = json!("x".repeat(50_001));
    assert_eq!(app.post("/v1/addons", Some(&token), big).await.error(), "description_too_long");
}
