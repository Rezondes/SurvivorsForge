mod common;

use axum::http::StatusCode;
use common::{TestApp, capitalized};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::io::Write;

fn addon_zip(folder: &str, filler: usize) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file(format!("repo-v1/{folder}/{folder}.toc"), options).unwrap();
    zip.write_all(b"## Version: 1").unwrap();
    zip.start_file(format!("repo-v1/{folder}/data.lua"), options).unwrap();
    zip.write_all("x".repeat(filler).as_bytes()).unwrap();
    zip.finish().unwrap().into_inner()
}

/// A checked version of `slug` whose zip the fake GitHub serves. `trust` is `clean` or `flagged`.
async fn checked_version(app: &TestApp, slug: &str, result: &str) -> Vec<u8> {
    let bytes = addon_zip(&capitalized(slug), 100);
    let url = format!("https://github.com/{slug}/v1.zip");
    app.github.data().downloads.insert(url.clone(), bytes.clone());
    let version: i64 = sqlx::query_scalar(
        "INSERT INTO versions (addon_id, tag, published_at, asset_name, asset_url, asset_size, sha256, scan_status, created_at)
         SELECT id, 'v1', now(), 'a.zip', $2, $3, $4, 'done', now() FROM addons WHERE slug = $1 RETURNING id",
    )
    .bind(slug)
    .bind(&url)
    .bind(bytes.len() as i64)
    .bind(hex::encode(Sha256::digest(&bytes)))
    .fetch_one(&app.pool)
    .await
    .unwrap();
    let scan: i64 = sqlx::query_scalar(
        "INSERT INTO scans (version_id, sha256, scanner_version, started_at, finished_at, result) VALUES ($1, 'abc', 'x', now(), now(), $2) RETURNING id",
    )
    .bind(version)
    .bind(if result == "flagged" { "flagged" } else { "clean" })
    .fetch_one(&app.pool)
    .await
    .unwrap();
    if result == "flagged" {
        sqlx::query("INSERT INTO findings (scan_id, rule, severity, file, line, message) VALUES ($1, 'lua.os', 'critical', 'a.lua', 1, 'x')")
            .bind(scan)
            .execute(&app.pool)
            .await
            .unwrap();
    }
    bytes
}

async fn app_with_packs(pool: PgPool) -> (TestApp, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_string_lossy().into_owned();
    (TestApp::with_config(pool, |c| c.pack_dir = path), dir)
}

async fn build(app: &TestApp, id: &str) {
    let key: String = sqlx::query_scalar("SELECT content_key FROM packs WHERE public_id = $1").bind(id).fetch_one(&app.pool).await.unwrap();
    forge_api::pack_job::build(&app.state(), &json!({ "content_key": key })).await.unwrap();
}

#[sqlx::test]
async fn create_show_build_and_download(pool: PgPool) {
    let (app, dir) = app_with_packs(pool).await;
    let token = app.publish(1, "alice", "lucky").await;
    app.publish(2, "bob", "odds").await;
    checked_version(&app, "lucky", "clean").await;
    checked_version(&app, "odds", "clean").await;

    assert_eq!(app.post("/v1/packs", None, json!({ "addons": [{ "slug": "lucky" }] })).await.status, StatusCode::UNAUTHORIZED);
    let created = app.post("/v1/packs", Some(&token), json!({ "name": "My UI", "addons": [{ "slug": "odds" }, { "slug": "lucky", "tag": "v1" }] })).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let id = created.json()["id"].as_str().unwrap().to_string();
    assert_eq!(id.len(), 10);

    let pack = app.get(&format!("/v1/packs/{id}"), None).await.json();
    assert_eq!((pack["name"].as_str(), pack["created_by"].as_str(), pack["available"].as_bool()), (Some("My UI"), Some("alice"), Some(true)));
    let slugs: Vec<&str> = pack["addons"].as_array().unwrap().iter().map(|a| a["slug"].as_str().unwrap()).collect();
    assert_eq!(slugs, ["lucky", "odds"]);
    assert_eq!(pack["file"]["status"], "none");

    let first = app.get(&format!("/v1/packs/{id}/download"), None).await;
    assert_eq!((first.status, first.json()["status"].as_str()), (StatusCode::ACCEPTED, Some("building")));
    let jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind = 'build_pack'").fetch_one(&app.pool).await.unwrap();
    app.get(&format!("/v1/packs/{id}/download"), None).await;
    assert_eq!(jobs, 1);
    assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM jobs WHERE kind = 'build_pack'").fetch_one(&app.pool).await.unwrap(), 1, "one build at a time");

    build(&app, &id).await;
    let ready = app.get(&format!("/v1/packs/{id}/download"), None).await;
    assert_eq!(ready.status, StatusCode::SEE_OTHER);
    let key: String = sqlx::query_scalar("SELECT content_key FROM packs WHERE public_id = $1").bind(&id).fetch_one(&app.pool).await.unwrap();
    assert_eq!(ready.location(), format!("https://web.test/packs-files/{key}.zip"));
    let zip = std::fs::File::open(dir.path().join(format!("{key}.zip"))).unwrap();
    let found: Vec<String> = forge_core::archive::analyze_reader(zip).unwrap().into_iter().map(|a| format!("{}={}", a.name, a.prefix)).collect();
    assert_eq!(found, ["Lucky=Lucky", "Odds=Odds"], "addon folders at the root");

    // The same versions in another order share the file.
    let again = app.post("/v1/packs", Some(&token), json!({ "addons": [{ "slug": "lucky" }, { "slug": "odds" }] })).await.json();
    let other = app.get(&format!("/v1/packs/{}/download", again["id"].as_str().unwrap()), None).await;
    assert_eq!(other.location(), ready.location());
}

#[sqlx::test]
async fn pack_download_counted(pool: PgPool) {
    let (app, _dir) = app_with_packs(pool).await;
    let token = app.publish(1, "alice", "lucky").await;
    checked_version(&app, "lucky", "clean").await;
    let id = app.post("/v1/packs", Some(&token), json!({ "addons": [{ "slug": "lucky" }] })).await.json()["id"].as_str().unwrap().to_string();
    app.get(&format!("/v1/packs/{id}/download"), None).await;
    let counts = || sqlx::query_scalar::<_, i64>("SELECT coalesce(sum(count), 0)::bigint FROM download_counts WHERE source = 'pack'").fetch_one(&app.pool);
    assert_eq!(counts().await.unwrap(), 0, "a build is no download");
    build(&app, &id).await;
    app.get(&format!("/v1/packs/{id}/download"), None).await;
    app.get(&format!("/v1/packs/{id}/download"), None).await;
    assert_eq!(counts().await.unwrap(), 1, "once per visitor and day");
}

#[sqlx::test]
async fn only_checked_versions_and_within_limits(pool: PgPool) {
    let (app, _dir) = app_with_packs(pool).await;
    let app = TestApp::with_config(app.pool.clone(), |c| {
        c.pack_max_addons = 2;
        c.packs_per_hour = 2;
        c.pack_max_bytes = 2_000;
    });
    let token = app.publish(1, "alice", "lucky").await;
    app.publish(2, "bob", "bad").await;
    app.publish(3, "carol", "new").await;
    app.publish(4, "dave", "big").await;
    checked_version(&app, "lucky", "clean").await;
    checked_version(&app, "bad", "flagged").await;
    sqlx::query("UPDATE versions SET asset_size = 5000 WHERE asset_url LIKE '%lucky%'").execute(&app.pool).await.unwrap();

    let error = |body| {
        let app = &app;
        let token = token.clone();
        async move { app.post("/v1/packs", Some(&token), body).await.error() }
    };
    assert_eq!(error(json!({ "addons": [] })).await, "empty_pack");
    assert_eq!(error(json!({ "addons": [{ "slug": "a" }, { "slug": "b" }, { "slug": "c" }] })).await, "too_many_addons");
    assert_eq!(error(json!({ "addons": [{ "slug": "nope" }] })).await, "unknown_addon");
    assert_eq!(error(json!({ "addons": [{ "slug": "bad" }] })).await, "not_packable", "flagged");
    assert_eq!(error(json!({ "addons": [{ "slug": "new" }] })).await, "unknown_addon", "no version yet");
    assert_eq!(error(json!({ "addons": [{ "slug": "lucky" }] })).await, "too_large", "known release size over the limit");
    sqlx::query("UPDATE versions SET asset_size = 50 WHERE asset_url LIKE '%lucky%'").execute(&app.pool).await.unwrap();
    assert_eq!(error(json!({ "addons": [{ "slug": "lucky" }, { "slug": "LUCKY" }] })).await, "duplicate_addon");

    for _ in 0..2 {
        assert_eq!(app.post("/v1/packs", Some(&token), json!({ "addons": [{ "slug": "lucky" }] })).await.status, StatusCode::CREATED);
    }
    let limited = app.post("/v1/packs", Some(&token), json!({ "addons": [{ "slug": "lucky" }] })).await;
    assert_eq!((limited.status, limited.error()), (StatusCode::TOO_MANY_REQUESTS, "pack_limit".to_string()));
}

#[sqlx::test]
async fn failed_builds_and_blocked_addons(pool: PgPool) {
    let (app, dir) = app_with_packs(pool).await;
    let token = app.publish(1, "alice", "lucky").await;
    checked_version(&app, "lucky", "clean").await;
    let id = app.post("/v1/packs", Some(&token), json!({ "addons": [{ "slug": "lucky" }] })).await.json()["id"].as_str().unwrap().to_string();

    // The file on GitHub is no longer the checked one.
    app.github.data().downloads.insert("https://github.com/lucky/v1.zip".into(), addon_zip("Lucky", 5));
    app.get(&format!("/v1/packs/{id}/download"), None).await;
    build(&app, &id).await;
    let failed = app.get(&format!("/v1/packs/{id}/download"), None).await;
    assert_eq!((failed.status, failed.error()), (StatusCode::UNPROCESSABLE_ENTITY, "build_failed".to_string()));
    assert!(failed.json()["message"].as_str().unwrap().contains("changed on GitHub"), "{}", failed.body);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "no file left behind");

    sqlx::query("UPDATE addons SET blocked_at = now() WHERE slug = 'lucky'").execute(&app.pool).await.unwrap();
    assert_eq!(app.get(&format!("/v1/packs/{id}"), None).await.json()["available"], false);
    assert_eq!(app.get(&format!("/v1/packs/{id}/download"), None).await.error(), "pack_unavailable");
}

#[sqlx::test]
async fn prune_drops_stale_and_over_budget_zips(pool: PgPool) {
    let (app, dir) = app_with_packs(pool).await;
    let token = app.publish(1, "alice", "lucky").await;
    app.publish(2, "bob", "odds").await;
    checked_version(&app, "lucky", "clean").await;
    checked_version(&app, "odds", "clean").await;
    let mut ids = Vec::new();
    for slug in ["lucky", "odds"] {
        let id = app.post("/v1/packs", Some(&token), json!({ "addons": [{ "slug": slug }] })).await.json()["id"].as_str().unwrap().to_string();
        app.get(&format!("/v1/packs/{id}/download"), None).await;
        build(&app, &id).await;
        app.get(&format!("/v1/packs/{id}/download"), None).await;
        ids.push(id);
    }
    let files = || std::fs::read_dir(dir.path()).unwrap().count();
    assert_eq!(files(), 2);

    // Within the retention time and the budget: nothing goes.
    forge_api::pack_job::prune(&app.state()).await.unwrap();
    assert_eq!(files(), 2);

    // Not downloaded for 8 days: the lucky pack goes, and the next download builds it again.
    sqlx::query("UPDATE pack_files SET last_download_at = now() - interval '8 days' WHERE content_key = (SELECT content_key FROM packs WHERE public_id = $1)")
        .bind(&ids[0])
        .execute(&app.pool)
        .await
        .unwrap();
    forge_api::pack_job::prune(&app.state()).await.unwrap();
    assert_eq!(files(), 1);
    assert_eq!(app.get(&format!("/v1/packs/{}/download", ids[0]), None).await.status, StatusCode::ACCEPTED);

    // Over the storage budget: the least recently downloaded goes first.
    let tight = TestApp::with_config(app.pool.clone(), |c| {
        c.pack_dir = dir.path().to_string_lossy().into_owned();
        c.pack_storage_bytes = 1;
    });
    forge_api::pack_job::prune(&tight.state()).await.unwrap();
    assert_eq!(files(), 0);
}

#[sqlx::test]
async fn only_listed_and_available_packs_are_public(pool: PgPool) {
    let (app, _dir) = app_with_packs(pool).await;
    let token = app.publish(1, "alice", "lucky").await;
    app.publish(2, "bob", "odds").await;
    checked_version(&app, "lucky", "clean").await;
    checked_version(&app, "odds", "clean").await;
    let create = |body: serde_json::Value| {
        let (app, token) = (&app, token.clone());
        async move { app.post("/v1/packs", Some(&token), body).await.json()["id"].as_str().unwrap().to_string() }
    };
    create(json!({ "name": "Hidden", "addons": [{ "slug": "lucky" }] })).await;
    let listed = create(json!({ "name": "My UI", "listed": true, "addons": [{ "slug": "lucky" }, { "slug": "odds" }] })).await;
    create(json!({ "name": "Odds only", "listed": true, "addons": [{ "slug": "odds" }] })).await;
    assert_eq!(app.get(&format!("/v1/packs/{listed}"), None).await.json()["listed"], true);

    let list = app.get("/v1/packs", None).await.json();
    let names: Vec<&str> = list["packs"].as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Odds only", "My UI"], "newest first, the unlisted one stays hidden");
    assert_eq!(list["total"], 2);
    let mine = list["packs"].as_array().unwrap().iter().find(|p| p["id"] == listed.as_str()).unwrap();
    assert_eq!((mine["created_by"].as_str(), mine["addons"].clone()), (Some("alice"), json!(["Addon lucky", "Addon odds"])));

    // Search: by the pack's name or the name of an addon in it, any case.
    let found = |q: &str| {
        let app = &app;
        let q = q.to_string();
        async move {
            let list = app.get(&format!("/v1/packs?q={q}"), None).await.json();
            list["packs"].as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap().to_string()).collect::<Vec<_>>()
        }
    };
    assert_eq!(found("my%20ui").await, ["My UI"]);
    assert_eq!(found("LUCKY").await, ["My UI"], "by an addon of the pack");
    assert_eq!(found("odds").await, ["Odds only", "My UI"]);
    assert!(found("nothing").await.is_empty());

    sqlx::query("UPDATE addons SET blocked_at = now() WHERE slug = 'odds'").execute(&app.pool).await.unwrap();
    let names: Vec<String> = app.get("/v1/packs", None).await.json()["packs"].as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap().to_string()).collect();
    assert!(names.is_empty(), "packs that cannot be downloaded are not offered: {names:?}");
}

/// Another checked version `tag` of `slug`, published after the ones before.
async fn newer_version(app: &TestApp, slug: &str, tag: &str) {
    let version: i64 = sqlx::query_scalar(
        "INSERT INTO versions (addon_id, tag, published_at, asset_name, asset_url, asset_size, sha256, scan_status, created_at)
         SELECT id, $2, now() + interval '1 day', 'a.zip', 'https://github.com/x.zip', 100, 'feed', 'done', now() FROM addons WHERE slug = $1 RETURNING id",
    )
    .bind(slug)
    .bind(tag)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO scans (version_id, sha256, scanner_version, started_at, finished_at, result) VALUES ($1, 'feed', 'x', now(), now(), 'clean')")
        .bind(version)
        .execute(&app.pool)
        .await
        .unwrap();
}

fn tags(pack: &serde_json::Value) -> Vec<(String, String, bool)> {
    pack["addons"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| (a["slug"].as_str().unwrap().into(), a["tag"].as_str().unwrap().into(), a["latest"].as_bool().unwrap()))
        .collect()
}

#[sqlx::test]
async fn latest_addons_follow_new_versions(pool: PgPool) {
    let (app, _dir) = app_with_packs(pool).await;
    let token = app.publish(1, "alice", "lucky").await;
    app.publish(2, "bob", "odds").await;
    checked_version(&app, "lucky", "clean").await;
    checked_version(&app, "odds", "clean").await;
    let created = app.post("/v1/packs", Some(&token), json!({ "addons": [{ "slug": "lucky", "latest": true }, { "slug": "odds", "tag": "v1" }] })).await;
    let id = created.json()["id"].as_str().unwrap().to_string();
    let key = |app: &TestApp| {
        let pool = app.pool.clone();
        let id = id.clone();
        async move { sqlx::query_scalar::<_, String>("SELECT content_key FROM packs WHERE public_id = $1").bind(id).fetch_one(&pool).await.unwrap() }
    };
    let before = key(&app).await;

    newer_version(&app, "lucky", "v2").await;
    newer_version(&app, "odds", "v2").await;
    let pack = app.get(&format!("/v1/packs/{id}"), None).await.json();
    assert_eq!(tags(&pack), [("lucky".into(), "v2".into(), true), ("odds".into(), "v1".into(), false)], "only the following addon moves");
    assert_eq!(pack["addons"][1]["versions"], json!(["v2", "v1"]), "the choices, newest first");
    assert_ne!(key(&app).await, before, "a new zip for the new versions");
}

#[sqlx::test]
async fn creator_changes_name_addons_and_versions(pool: PgPool) {
    let (app, _dir) = app_with_packs(pool).await;
    let token = app.publish(1, "alice", "lucky").await;
    let bob = app.publish(2, "bob", "odds").await;
    checked_version(&app, "lucky", "clean").await;
    checked_version(&app, "odds", "clean").await;
    newer_version(&app, "lucky", "v2").await;
    let id = app.post("/v1/packs", Some(&token), json!({ "name": "Old", "addons": [{ "slug": "lucky", "tag": "v1" }] })).await.json()["id"].as_str().unwrap().to_string();
    let uri = format!("/v1/packs/{id}");

    assert_eq!(app.patch(&uri, Some(&bob), json!({ "name": "Mine" })).await.status, StatusCode::FORBIDDEN);
    assert_eq!(app.patch(&uri, None, json!({ "name": "Mine" })).await.status, StatusCode::UNAUTHORIZED);

    let renamed = app.patch(&uri, Some(&token), json!({ "name": "  My UI ", "listed": true })).await;
    assert_eq!(renamed.status, StatusCode::OK, "{}", renamed.body);
    assert_eq!((renamed.json()["name"].as_str(), renamed.json()["listed"].as_bool()), (Some("My UI"), Some(true)));
    assert_eq!(tags(&renamed.json()), [("lucky".into(), "v1".into(), false)], "addons stay without addons");

    let changed = app.patch(&uri, Some(&token), json!({ "addons": [{ "slug": "lucky", "latest": true }, { "slug": "odds" }] })).await.json();
    assert_eq!(tags(&changed), [("lucky".into(), "v2".into(), true), ("odds".into(), "v1".into(), false)]);
    assert_eq!(changed["id"], json!(id), "same link");

    let refused = app.patch(&uri, Some(&token), json!({ "addons": [] })).await;
    assert_eq!((refused.status, refused.error().as_str()), (StatusCode::UNPROCESSABLE_ENTITY, "empty_pack"));
    let twice = app.patch(&uri, Some(&token), json!({ "addons": [{ "slug": "lucky", "tag": "v1" }, { "slug": "lucky", "latest": true }] })).await;
    assert_eq!(twice.error(), "duplicate_addon");
}
