mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, send};
use serde_json::{Value, json};
use sqlx::PgPool;

/// Publishes an addon and sets its name, summary, description and category directly.
async fn addon(app: &TestApp, github_id: i64, slug: &str, name: &str, summary: &str, description: &str, category: &str) {
    app.publish(github_id, &format!("dev{github_id}"), slug).await;
    sqlx::query("UPDATE addons SET name = $2, summary = $3, description_md = $4, category = $5 WHERE slug = $1")
        .bind(slug)
        .bind(name)
        .bind(summary)
        .bind(description)
        .bind(category)
        .execute(&app.pool)
        .await
        .unwrap();
}

async fn slugs(app: &TestApp, query: &str) -> Vec<String> {
    let reply = app.get(&format!("/v1/addons?{query}"), None).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    reply.json()["addons"].as_array().unwrap().iter().map(|a| a["slug"].as_str().unwrap().to_string()).collect()
}

async fn version(app: &TestApp, slug: &str, downloads: i64, scanned: bool) {
    sqlx::query(
        "INSERT INTO versions (addon_id, tag, published_at, asset_name, asset_url, download_count, scan_status, sha256, created_at)
         SELECT id, 'v1', now(), 'a.zip', 'https://github.com/a.zip', $2, $3, 'abc', now() FROM addons WHERE slug = $1",
    )
    .bind(slug)
    .bind(downloads)
    .bind(if scanned { "done" } else { "pending" })
    .execute(&app.pool)
    .await
    .unwrap();
    if scanned {
        sqlx::query(
            "INSERT INTO scans (version_id, sha256, scanner_version, started_at, finished_at, result)
             SELECT v.id, 'abc', 'x', now(), now(), 'clean' FROM versions v JOIN addons a ON a.id = v.addon_id WHERE a.slug = $1",
        )
        .bind(slug)
        .execute(&app.pool)
        .await
        .unwrap();
    }
}

async fn catalog(pool: PgPool) -> TestApp {
    let app = TestApp::new(pool);
    addon(&app, 1, "luckyodds", "LuckyOdds", "Odds for rare cards", "Shows your luck.", "Miscellaneous").await;
    addon(&app, 2, "builder", "ASBuilder", "Build planner", "Plans builds. Mentions luckyodds in passing.", "Warrior").await;
    addon(&app, 3, "plates", "Survivor Plates", "Nameplates", "Flat nameplates.", "Unit Frames").await;
    version(&app, "luckyodds", 50, true).await;
    version(&app, "builder", 500, false).await;
    version(&app, "plates", 5, true).await;
    app
}

#[sqlx::test]
async fn fulltext_and_typo(pool: PgPool) {
    let app = catalog(pool).await;
    assert!(slugs(&app, "q=lucky").await.contains(&"luckyodds".to_string()), "prefix match");
    assert!(slugs(&app, "q=lukcy").await.contains(&"luckyodds".to_string()), "typo in the name");
    assert_eq!(slugs(&app, "q=nameplates").await, ["plates"]);
    assert!(slugs(&app, "q=zzzzzz").await.is_empty());
}

#[sqlx::test]
async fn name_ranks_first(pool: PgPool) {
    let app = catalog(pool).await;
    assert_eq!(slugs(&app, "q=luckyodds").await, ["luckyodds", "builder"], "name hit before description hit");
}

#[sqlx::test]
async fn filters_combine(pool: PgPool) {
    let app = catalog(pool).await;
    assert_eq!(slugs(&app, "category=Unit%20Frames").await, ["plates"]);
    assert_eq!(slugs(&app, "category=Classes").await, ["builder"], "a category finds its subcategories");
    assert_eq!(slugs(&app, "category=warrior").await, ["builder"]);
    assert_eq!(slugs(&app, "trust=scanned&sort=name").await, ["luckyodds", "plates"]);
    assert_eq!(slugs(&app, "trust=scanned&category=Miscellaneous").await, ["luckyodds"]);
    assert_eq!(slugs(&app, "trust=unscanned").await, ["builder"]);

    sqlx::query("INSERT INTO game_versions (name, sort_key) VALUES ('0.69', 1), ('0.70', 2)").execute(&app.pool).await.unwrap();
    sqlx::query(
        "UPDATE versions SET game_min_id = (SELECT id FROM game_versions WHERE name = '0.70')
         WHERE addon_id = (SELECT id FROM addons WHERE slug = 'plates')",
    )
    .execute(&app.pool)
    .await
    .unwrap();
    let on_069 = slugs(&app, "game=0.69&sort=name").await;
    assert_eq!(on_069, ["builder", "luckyodds"], "plates needs 0.70; unknown counts as maybe");
}

#[sqlx::test]
async fn blocked_and_deleted_hidden(pool: PgPool) {
    let app = catalog(pool).await;
    sqlx::query("UPDATE addons SET blocked_at = now() WHERE slug = 'plates'").execute(&app.pool).await.unwrap();
    sqlx::query("UPDATE addons SET deleted_at = now() WHERE slug = 'builder'").execute(&app.pool).await.unwrap();
    assert_eq!(slugs(&app, "").await, ["luckyodds"]);
}

#[sqlx::test]
async fn paging_bounds(pool: PgPool) {
    let app = catalog(pool).await;
    let page = app.get("/v1/addons?per_page=2&page=2&sort=name", None).await.json();
    assert_eq!(page["total"], 3);
    assert_eq!(page["per_page"], 2);
    assert_eq!(page["addons"].as_array().unwrap().len(), 1);
    assert_eq!(app.get("/v1/addons?per_page=999", None).await.json()["per_page"], 50);
    assert_eq!(slugs(&app, "").await, ["builder", "luckyodds", "plates"], "default: most downloads first");
}

#[sqlx::test]
async fn addons_with_a_blocked_version_are_hidden(pool: PgPool) {
    // An admin blocked the version players would get: the addon leaves the list, any filter.
    let app = catalog(pool).await;
    sqlx::query("UPDATE versions SET blocked_at = now(), block_reason = 'malware' WHERE addon_id = (SELECT id FROM addons WHERE slug = 'plates')")
        .execute(&app.pool)
        .await
        .unwrap();
    assert_eq!(slugs(&app, "").await, ["builder", "luckyodds"]);
    assert_eq!(slugs(&app, "q=nameplates").await, Vec::<String>::new());
}

#[sqlx::test]
async fn tag_addons_have_no_download_count(pool: PgPool) {
    // GitHub counts downloads of release files only; a tag's zip has no counter.
    let app = catalog(pool).await;
    addon(&app, 4, "tagged", "Tagged", "From tags", "", "Miscellaneous").await;
    sqlx::query("UPDATE addons SET source_type = 'tag', asset_pattern = NULL WHERE slug = 'tagged'").execute(&app.pool).await.unwrap();
    version(&app, "tagged", 0, true).await;
    let item = &app.get("/v1/addons?q=tagged", None).await.json()["addons"][0];
    assert_eq!(item["downloads"], Value::Null, "{item}");
    for query in ["", "sort=downloads"] {
        assert_eq!(slugs(&app, query).await.last().map(String::as_str), Some("tagged"), "after every counted addon ({query})");
    }
}

#[sqlx::test]
async fn list_item_shape(pool: PgPool) {
    let app = catalog(pool).await;
    let page = app.get("/v1/addons?q=luckyodds", None).await.json();
    let item: &Value = &page["addons"][0];
    for key in ["slug", "name", "summary", "icon_url", "owner", "category", "trust", "verified_author", "downloads", "updated_at"] {
        assert!(item.get(key).is_some(), "missing {key} in {item}");
    }
    assert_eq!(item["downloads"], 50);
    assert_eq!(item["owner"]["login"], "dev1");
    assert_eq!(item["trust"], "scanned");
    assert_eq!(item["verified_author"], json!(false));
}

#[sqlx::test]
async fn etag_304(pool: PgPool) {
    let app = catalog(pool).await;
    let first = app.get("/v1/addons", None).await;
    let etag = first.headers["etag"].to_str().unwrap().to_string();
    let request = Request::get("/v1/addons").header("if-none-match", &etag).body(Body::empty()).unwrap();
    let second = send(&app.router, request).await;
    assert_eq!(second.status, StatusCode::NOT_MODIFIED);
    assert!(second.body.is_empty());
}

#[sqlx::test]
async fn author_filter_lists_one_authors_addons(pool: PgPool) {
    let app = common::TestApp::new(pool);
    app.publish(1, "alice", "lucky").await;
    app.publish(2, "bob", "odds").await;
    let slugs = |v: serde_json::Value| -> Vec<String> { v["addons"].as_array().unwrap().iter().map(|a| a["slug"].as_str().unwrap().to_string()).collect() };
    assert_eq!(slugs(app.get("/v1/addons?author=ALICE", None).await.json()), ["lucky"], "ignores case");
    assert_eq!(slugs(app.get("/v1/addons?author=nobody", None).await.json()), Vec::<String>::new());
}
