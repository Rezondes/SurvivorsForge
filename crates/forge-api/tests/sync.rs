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

#[sqlx::test]
async fn existing_release_refreshes_published_date_without_rescanning(pool: PgPool) {
    let app = TestApp::new(pool);
    app.publish(1, "alice", "lucky").await;
    let id = addon_id(&app, "lucky").await;
    let first = chrono::Utc::now() - chrono::Duration::days(10);
    let corrected = first + chrono::Duration::days(3);
    let mut original = release(1, "v1", &[(11, "lucky-v1.zip")]);
    original.published_at = Some(first);
    app.github.data().releases.insert(100, vec![original.clone()]);
    sync_addon(&app.state(), id).await.unwrap();
    sqlx::query("UPDATE versions SET sha256 = 'verified', scan_status = 'done'")
        .execute(&app.pool).await.unwrap();
    sqlx::query("UPDATE jobs SET done_at = now()").execute(&app.pool).await.unwrap();

    original.published_at = Some(corrected);
    app.github.data().releases.insert(100, vec![original]);
    sync_addon(&app.state(), id).await.unwrap();

    let (published_at, sha256, scan_status): (chrono::DateTime<chrono::Utc>, Option<String>, String) =
        sqlx::query_as("SELECT published_at, sha256, scan_status FROM versions WHERE addon_id = $1 AND tag = 'v1'")
            .bind(id).fetch_one(&app.pool).await.unwrap();
    assert_eq!(published_at.timestamp_micros(), corrected.timestamp_micros(), "the existing release must reflect the corrected GitHub date at PostgreSQL microsecond precision");
    assert_eq!(sha256.as_deref(), Some("verified"), "date-only updates must preserve scan results");
    assert_eq!(scan_status, "done");
    assert_eq!(scan_jobs(&app).await, 1, "date-only updates must not enqueue another scan");
}

#[sqlx::test]
async fn paged_versions_keep_order_and_handle_boundaries(pool: PgPool) {
    let app = TestApp::new(pool);
    app.publish(1, "alice", "lucky").await;
    let id = addon_id(&app, "lucky").await;
    let at = chrono::Utc::now();
    let releases: Vec<Release> = (1..=41).rev().map(|n| Release {
        published_at: Some(at),
        ..release(n, &format!("v1.{n}.0"), &[(n * 10, &format!("lucky-v1.{n}.0.zip"))])
    }).collect();
    app.github.data().releases.insert(100, releases);
    sync_addon(&app.state(), id).await.unwrap();

    let mut tags = Vec::new();
    for (page, expected_count, has_more) in [(1, 20, true), (2, 20, true), (3, 1, false), (4, 0, false)] {
        let response = app.get(&format!("/v1/addons/lucky/versions/page?page={page}&per_page=20"), None).await;
        assert_eq!(response.status, axum::http::StatusCode::OK);
        let data = response.json();
        assert_eq!(data["total"], 41);
        assert_eq!(data["page"], page);
        assert_eq!(data["per_page"], 20);
        assert_eq!(data["has_more"], has_more);
        let rows = data["versions"].as_array().unwrap();
        assert_eq!(rows.len(), expected_count, "page {page}");
        tags.extend(rows.iter().map(|v| v["tag"].as_str().unwrap().to_string()));
    }
    let expected: Vec<String> = (1..=41).rev().map(|n| format!("v1.{n}.0")).collect();
    assert_eq!(tags, expected, "no duplicates or missing versions across page boundaries");

    let first = app.get("/v1/addons/lucky/versions/page?page=-5&per_page=500", None).await.json();
    assert_eq!(first["page"], 1);
    assert_eq!(first["per_page"], 100);
    assert_eq!(first["versions"].as_array().unwrap().len(), 41);
}
