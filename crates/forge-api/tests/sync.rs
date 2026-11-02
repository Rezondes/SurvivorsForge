mod common;

use common::TestApp;
use forge_api::github::types::{Asset, Commit, Release, Tag};
use forge_api::sync::sync_addon;
use serde_json::json;
use sqlx::PgPool;

pub fn release(id: i64, tag: &str, assets: &[(i64, &str)]) -> Release {
    Release {
        id,
        tag_name: tag.into(),
        name: Some(format!("Release {tag}")),
        draft: false,
        prerelease: false,
        published_at: Some(chrono::Utc::now()),
        assets: assets
            .iter()
            .map(|(id, name)| Asset {
                id: *id,
                name: name.to_string(),
                size: 1234,
                browser_download_url: format!("https://github.com/dl/{id}/{name}"),
                download_count: 5,
                digest: None,
            })
            .collect(),
    }
}

async fn addon_id(app: &TestApp, slug: &str) -> i64 {
    sqlx::query_scalar("SELECT id FROM addons WHERE slug = $1").bind(slug).fetch_one(&app.pool).await.unwrap()
}

async fn versions(app: &TestApp) -> Vec<(String, Option<String>, String, bool)> {
    sqlx::query_as("SELECT tag, sha256, scan_status, deleted_on_github_at IS NOT NULL FROM versions ORDER BY tag")
        .fetch_all(&app.pool)
        .await
        .unwrap()
}

async fn scan_jobs(app: &TestApp) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind = 'scan_version'").fetch_one(&app.pool).await.unwrap()
}

#[sqlx::test]
async fn release_without_matching_asset_is_ignored(pool: PgPool) {
    let app = TestApp::new(pool);
    app.publish(1, "alice", "lucky").await;
    app.github.data().releases.insert(
        100,
        vec![release(1, "v1", &[(11, "lucky-v1.zip")]), release(2, "v2", &[(21, "Installer.exe"), (22, "notes.txt")])],
    );
    sync_addon(&app.state(), addon_id(&app, "lucky").await).await.unwrap();
    let tags: Vec<String> = versions(&app).await.into_iter().map(|v| v.0).collect();
    assert_eq!(tags, ["v1"]);
    assert_eq!(scan_jobs(&app).await, 1);
}

#[sqlx::test]
async fn edited_asset_resets_hash_and_rescans(pool: PgPool) {
    let app = TestApp::new(pool);
    app.publish(1, "alice", "lucky").await;
    let id = addon_id(&app, "lucky").await;
    app.github.data().releases.insert(100, vec![release(1, "v1", &[(11, "lucky-v1.zip")])]);
    sync_addon(&app.state(), id).await.unwrap();
    sqlx::query("UPDATE versions SET sha256 = 'abc', scan_status = 'done'").execute(&app.pool).await.unwrap();
    sqlx::query("UPDATE jobs SET done_at = now()").execute(&app.pool).await.unwrap();

    sync_addon(&app.state(), id).await.unwrap();
    assert_eq!(versions(&app).await[0], ("v1".into(), Some("abc".into()), "done".into(), false), "same file: nothing changes");

    app.github.data().releases.insert(100, vec![release(1, "v1", &[(12, "lucky-v1.zip")])]);
    sync_addon(&app.state(), id).await.unwrap();
    assert_eq!(versions(&app).await[0], ("v1".into(), None, "pending".into(), false));
    assert_eq!(scan_jobs(&app).await, 2, "scanned again");
}

#[sqlx::test]
async fn deleted_release_kept_in_history(pool: PgPool) {
    let app = TestApp::new(pool);
    app.publish(1, "alice", "lucky").await;
    let id = addon_id(&app, "lucky").await;
    app.github.data().releases.insert(100, vec![release(1, "v1", &[(11, "lucky-v1.zip")]), release(2, "v2", &[(21, "lucky-v2.zip")])]);
    sync_addon(&app.state(), id).await.unwrap();
    app.github.data().releases.insert(100, vec![release(2, "v2", &[(21, "lucky-v2.zip")])]);
    sync_addon(&app.state(), id).await.unwrap();
    let v = versions(&app).await;
    assert_eq!((v[0].0.as_str(), v[0].3), ("v1", true));
    assert_eq!((v[1].0.as_str(), v[1].3), ("v2", false));

    let listed = app.get("/v1/addons/lucky/versions", None).await.json();
    assert_eq!(listed.as_array().unwrap().len(), 2);
    assert!(listed.as_array().unwrap().iter().any(|v| v["tag"] == "v1" && v["deleted_on_github"] == true));
}

#[sqlx::test]
async fn versions_newest_first_when_they_share_a_date(pool: PgPool) {
    // Tags have no date of their own: one sync gives them all the same. GitHub lists newest first.
    let app = TestApp::new(pool);
    app.publish(1, "alice", "lucky").await;
    let id = addon_id(&app, "lucky").await;
    let at = chrono::Utc::now();
    let dated = |id: i64, tag: &str| Release { published_at: Some(at), ..release(id, tag, &[(id * 10, &format!("lucky-{tag}.zip"))]) };
    app.github.data().releases.insert(100, vec![dated(3, "v1.10.0"), dated(2, "v1.2.0"), dated(1, "v1.0.0")]);
    sync_addon(&app.state(), id).await.unwrap();
    let listed = app.get("/v1/addons/lucky/versions", None).await.json();
    let tags: Vec<&str> = listed.as_array().unwrap().iter().map(|v| v["tag"].as_str().unwrap()).collect();
    assert_eq!(tags, ["v1.10.0", "v1.2.0", "v1.0.0"]);
}

#[sqlx::test]
async fn poll_finds_missed_release(pool: PgPool) {
    let app = TestApp::new(pool);
    app.publish(1, "alice", "lucky").await;
    app.github.data().releases.insert(100, vec![release(1, "v1", &[(11, "lucky-v1.zip")])]);
    forge_api::sync::poll_releases(&app.state()).await.unwrap();
    let worker = forge_api::worker::platform(app.state());
    while worker.run_once(&app.pool).await.unwrap() {
        if versions(&app).await.len() == 1 {
            break;
        }
    }
    assert_eq!(versions(&app).await.len(), 1);
}

#[sqlx::test]
async fn tag_addon_versions_from_tags(pool: PgPool) {
    let app = TestApp::new(pool);
    let token = app.publish(1, "alice", "lucky").await;
    app.patch("/v1/addons/lucky", Some(&token), json!({ "source_type": "tag" })).await;
    app.github.data().tags.insert(
        100,
        vec![Tag { name: "v1.0".into(), commit: Commit { sha: "aaa".into() } }, Tag { name: "v1.1".into(), commit: Commit { sha: "bbb".into() } }],
    );
    sync_addon(&app.state(), addon_id(&app, "lucky").await).await.unwrap();
    let urls: Vec<String> = sqlx::query_scalar("SELECT asset_url FROM versions ORDER BY tag").fetch_all(&app.pool).await.unwrap();
    assert_eq!(urls, [
        "https://codeload.github.com/alice/lucky/zip/refs/tags/v1.0",
        "https://codeload.github.com/alice/lucky/zip/refs/tags/v1.1"
    ]);
    assert_eq!(scan_jobs(&app).await, 2);
}

#[sqlx::test]
async fn locked_addon_is_not_synced(pool: PgPool) {
    let app = TestApp::new(pool);
    app.publish(1, "alice", "lucky").await;
    app.github.data().releases.insert(100, vec![release(1, "v1", &[(11, "lucky-v1.zip")])]);
    sqlx::query("UPDATE addons SET locked_at = now()").execute(&app.pool).await.unwrap();
    sync_addon(&app.state(), addon_id(&app, "lucky").await).await.unwrap();
    assert!(versions(&app).await.is_empty());
}
