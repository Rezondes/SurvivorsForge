mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, send};
use forge_api::github::types::{Owner, Repo};
use forge_api::routes::registry::{Imported, export_verified, import_verified};
use forge_core::registry::{Registry, SourceSpec};
use serde_json::json;
use sqlx::PgPool;

const VERIFIED: &str = include_str!("../../../registry/verified.json");

/// Makes every repo of verified.json known to the fake GitHub, without the App installed.
fn known_repos(app: &TestApp) {
    let registry = Registry::parse(VERIFIED).unwrap();
    let mut data = app.github.data();
    for (i, addon) in registry.addons.iter().enumerate() {
        let owner = addon.repo.split('/').next().unwrap().to_string();
        data.repos.insert(5000 + i as i64, Repo { id: 5000 + i as i64, full_name: addon.repo.clone(), private: false, owner: Owner { id: 0, login: owner }, description: None });
    }
}

/// The schema-1 shape of launcher 0.3.2: no platform fields.
#[derive(serde::Deserialize)]
#[allow(dead_code)]
struct OldRegistry {
    schema: u32,
    addons: Vec<OldAddon>,
}

#[derive(serde::Deserialize)]
#[allow(dead_code)]
struct OldAddon {
    id: String,
    name: String,
    author: String,
    repo: String,
    #[serde(default)]
    description: String,
    folders: Vec<String>,
    source: SourceSpec,
}

async fn with_version(app: &TestApp, slug: &str) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO versions (addon_id, tag, published_at, asset_name, asset_url, asset_size, sha256, scan_status, created_at)
         SELECT id, 'v1', now(), 'a.zip', 'https://github.com/a.zip', 10, 'abc', 'done', now() FROM addons WHERE slug = $1 RETURNING id",
    )
    .bind(slug)
    .fetch_one(&app.pool)
    .await
    .unwrap()
}

#[sqlx::test]
async fn response_parses_with_new_and_old_schema(pool: PgPool) {
    let app = TestApp::new(pool);
    app.publish(1, "alice", "lucky").await;
    with_version(&app, "lucky").await;
    let reply = app.get("/v1/registry", None).await;
    assert_eq!(reply.status, StatusCode::OK);
    let registry = Registry::parse(&reply.body).expect("new launcher");
    let latest = registry.addons[0].latest.as_ref().unwrap();
    assert_eq!((latest.tag.as_str(), latest.sha256.as_deref()), ("v1", Some("abc")));
    let old: OldRegistry = serde_json::from_str(&reply.body).expect("launcher 0.3.2");
    assert_eq!(old.schema, 1);
    assert_eq!(old.addons[0].id, "lucky");
}

#[sqlx::test]
async fn carries_icon_release_date_and_downloads(pool: PgPool) {
    let app = TestApp::new(pool);
    app.publish(1, "alice", "lucky").await;
    app.publish(2, "bob", "odds").await;
    let v1 = with_version(&app, "lucky").await;
    sqlx::query("UPDATE versions SET published_at = '2026-10-01T12:00:00Z', download_count = 40 WHERE id = $1").bind(v1).execute(&app.pool).await.unwrap();
    sqlx::query(
        "INSERT INTO versions (addon_id, tag, published_at, asset_name, asset_url, sha256, scan_status, created_at, download_count)
         SELECT id, 'v0', '2026-09-01T12:00:00Z', 'a.zip', 'https://github.com/a0.zip', 'abc', 'done', now(), 2 FROM addons WHERE slug = 'lucky'",
    )
    .execute(&app.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO images (addon_id, kind, file, width, height, uploaded_by, uploaded_at)
         SELECT a.id, 'icon', 'f00d.webp', 256, 256, a.owner_id, now() FROM addons a WHERE slug = 'lucky'",
    )
    .execute(&app.pool)
    .await
    .unwrap();
    sqlx::query("UPDATE addons SET source_type = 'tag', asset_pattern = NULL WHERE slug = 'odds'").execute(&app.pool).await.unwrap();

    let body: serde_json::Value = app.get("/v1/registry", None).await.json();
    let lucky = &body["addons"][0];
    assert_eq!(lucky["icon_url"], "https://web.test/img/f00d.webp", "{body}");
    assert_eq!(lucky["downloads"], 42, "every version of the addon");
    assert!(lucky["latest"]["published_at"].as_str().unwrap().starts_with("2026-10-01T12:00:00"), "{body}");
    let odds = &body["addons"][1];
    assert_eq!((odds["icon_url"].clone(), odds["downloads"].clone()), (json!(null), json!(null)), "GitHub counts no tag zips: {body}");

    let registry = Registry::parse(&body.to_string()).expect("the launcher reads it");
    assert_eq!((registry.addons[0].downloads, registry.addons[0].icon_url.as_deref()), (Some(42), Some("https://web.test/img/f00d.webp")));
}

#[sqlx::test]
async fn excludes_blocked_and_handles_no_version(pool: PgPool) {
    let app = TestApp::new(pool);
    app.publish(1, "alice", "lucky").await;
    app.publish(2, "bob", "gone").await;
    app.publish(3, "carol", "bad").await;
    sqlx::query("UPDATE addons SET deleted_at = now() WHERE slug = 'gone'").execute(&app.pool).await.unwrap();
    sqlx::query("UPDATE addons SET blocked_at = now() WHERE slug = 'bad'").execute(&app.pool).await.unwrap();
    let registry = Registry::parse(&app.get("/v1/registry", None).await.body).unwrap();
    assert_eq!(registry.addons.len(), 1);
    assert_eq!(registry.addons[0].latest, None);
}

#[sqlx::test]
async fn etag_and_cache_headers(pool: PgPool) {
    let app = TestApp::new(pool);
    app.publish(1, "alice", "lucky").await;
    let first = app.get("/v1/registry", None).await;
    assert_eq!(first.headers["cache-control"], "public, max-age=300");
    let etag = first.headers["etag"].to_str().unwrap().to_string();
    let second = send(&app.router, Request::get("/v1/registry").header("if-none-match", etag).body(Body::empty()).unwrap()).await;
    assert_eq!(second.status, StatusCode::NOT_MODIFIED);
}

#[sqlx::test]
async fn import_is_idempotent(pool: PgPool) {
    let app = TestApp::new(pool);
    known_repos(&app);
    let first = import_verified(&app.state(), VERIFIED).await.unwrap();
    let created = first.iter().filter(|r| matches!(r, Imported::Created(_))).count();
    assert_eq!(created, 12, "{first:?}");
    let second = import_verified(&app.state(), VERIFIED).await.unwrap();
    assert!(second.iter().all(|r| matches!(r, Imported::Skipped(..))), "{second:?}");

    let (count, owned, installed): (i64, i64, i64) =
        sqlx::query_as("SELECT count(*), count(owner_id), count(installation_id) FROM addons").fetch_one(&app.pool).await.unwrap();
    assert_eq!((count, owned, installed), (12, 0, 0), "no owner, no App");
    let view = app.get("/v1/addons/luckyodds", None).await.json();
    assert_eq!((view["author"].as_str(), view["claimed"].as_bool()), (Some("cadoky"), Some(false)));
    assert_eq!(view["owner"], json!(null));
    let stats: String = sqlx::query_scalar("SELECT source_type FROM addons WHERE slug = 'azerothstats'").fetch_one(&app.pool).await.unwrap();
    assert_eq!(stats, "tag");
}

#[sqlx::test]
async fn export_roundtrip(pool: PgPool) {
    let app = TestApp::with_config(pool, |c| c.admin_github_ids = vec![99]);
    known_repos(&app);
    import_verified(&app.state(), VERIFIED).await.unwrap();
    assert!(Registry::parse(&export_verified(&app.state()).await.unwrap()).unwrap().addons.is_empty(), "nothing verified yet");

    let admin = app.login(99, "root").await;
    let input = Registry::parse(VERIFIED).unwrap();
    for addon in &input.addons {
        let id = with_version(&app, &addon.id).await;
        sqlx::query(
            "INSERT INTO scans (version_id, sha256, scanner_version, started_at, finished_at, result) VALUES ($1, 'abc', 'x', now(), now(), 'clean')",
        )
        .bind(id)
        .execute(&app.pool)
        .await
        .unwrap();
        assert_eq!(app.post(&format!("/v1/admin/versions/{id}/verification"), Some(&admin), json!({})).await.status, StatusCode::OK);
    }
    let exported = Registry::parse(&export_verified(&app.state()).await.unwrap()).unwrap();
    let key = |r: &Registry| {
        let mut v: Vec<_> = r.addons.iter().map(|a| (a.id.clone(), a.repo.clone(), sorted(&a.folders), a.source.clone(), a.author.clone())).collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    };
    assert_eq!(key(&exported), key(&input));
}

fn sorted(v: &[String]) -> Vec<String> {
    let mut v: Vec<String> = v.iter().map(|s| s.to_lowercase()).collect();
    v.sort();
    v
}

#[sqlx::test]
async fn admin_transfers_owner(pool: PgPool) {
    let app = TestApp::with_config(pool, |c| c.admin_github_ids = vec![99]);
    let old_owner = app.publish(1, "alice", "lucky").await;
    let admin = app.login(99, "root").await;
    app.login(2, "bob").await;
    let body = json!({ "login": "bob", "note": "real author" });
    assert_eq!(app.post("/v1/admin/addons/lucky/owner", Some(&old_owner), body.clone()).await.status, StatusCode::FORBIDDEN);
    assert_eq!(app.post("/v1/admin/addons/lucky/owner", Some(&admin), json!({ "login": "bob" })).await.error(), "note_required");
    assert_eq!(app.post("/v1/admin/addons/lucky/owner", Some(&admin), body).await.status, StatusCode::NO_CONTENT);
    assert_eq!(app.get("/v1/addons/lucky", None).await.json()["owner"]["login"], "bob");
    assert_eq!(app.patch("/v1/addons/lucky", Some(&old_owner), json!({ "name": "Mine" })).await.status, StatusCode::FORBIDDEN);
    let action: String = sqlx::query_scalar("SELECT action FROM audit_log").fetch_one(&app.pool).await.unwrap();
    assert_eq!(action, "transfer_owner");
}

#[sqlx::test]
async fn only_admins_add_addons_without_app_checks(pool: PgPool) {
    let app = TestApp::with_config(pool, |c| c.admin_github_ids = vec![99]);
    known_repos(&app);
    let user = app.login(5, "eve").await;
    let admin = app.login(99, "root").await;
    let body = json!({
        "slug": "luckyodds", "name": "LuckyOdds", "repo": "cadoky/LuckyOdds", "source_type": "release",
        "asset_pattern": "LuckyOdds-v*.zip", "category": "Miscellaneous", "summary": "Odds.", "folders": ["LuckyOdds"], "author_name": "cadoky"
    });
    assert_eq!(app.post("/v1/admin/addons", Some(&user), body.clone()).await.status, StatusCode::FORBIDDEN);
    assert_eq!(app.post("/v1/addons", Some(&user), body.clone()).await.error(), "app_not_installed", "the normal way needs the App");
    let reply = app.post("/v1/admin/addons", Some(&admin), body).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    assert_eq!((reply.json()["claimed"].as_bool(), reply.json()["author"].as_str()), (Some(false), Some("cadoky")));
    let action: String = sqlx::query_scalar("SELECT action FROM audit_log").fetch_one(&app.pool).await.unwrap();
    assert_eq!(action, "admin_create_addon");
}

#[sqlx::test]
async fn author_claims_imported_addon(pool: PgPool) {
    let app = TestApp::new(pool);
    known_repos(&app);
    import_verified(&app.state(), VERIFIED).await.unwrap();
    let cadoky = app.login(77, "cadoky").await;
    let stranger = app.login(78, "mallory").await;

    let reply = app.post("/v1/addons/luckyodds/claim", Some(&cadoky), json!({})).await;
    assert_eq!(reply.error(), "app_not_installed", "first install the App on the repo");

    let repo_id: i64 = sqlx::query_scalar("SELECT repo_id FROM addons WHERE slug = 'luckyodds'").fetch_one(&app.pool).await.unwrap();
    app.github.data().installations.insert("cadoky/luckyodds".into(), 42);
    app.github.data().permissions.insert((repo_id, "cadoky".into()), "admin".into());
    assert_eq!(app.post("/v1/addons/luckyodds/claim", Some(&stranger), json!({})).await.error(), "not_repo_admin");
    let reply = app.post("/v1/addons/luckyodds/claim", Some(&cadoky), json!({})).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.json()["owner"]["login"], "cadoky");
    let installation: Option<i64> = sqlx::query_scalar("SELECT installation_id FROM addons WHERE slug = 'luckyodds'").fetch_one(&app.pool).await.unwrap();
    assert_eq!(installation, Some(42));
    assert_eq!(app.post("/v1/addons/luckyodds/claim", Some(&stranger), json!({})).await.error(), "already_claimed");
}
