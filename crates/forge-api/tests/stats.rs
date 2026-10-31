mod common;

use chrono::{DateTime, Duration, TimeZone, Utc};
use common::TestApp;
use serde_json::json;
use sqlx::PgPool;

async fn event(app: &TestApp, kind: &str, install: Option<&str>, launcher: &str, days_ago: i64) {
    sqlx::query(
        "INSERT INTO telemetry_events (day, kind, addon_id, version, launcher_version, os, install_id, created_at)
         SELECT ($1::timestamptz)::date, $2, id, 'v1', $3, 'windows', $4::uuid, $1 FROM addons WHERE slug = 'lucky'",
    )
    .bind(Utc::now() - Duration::days(days_ago))
    .bind(kind)
    .bind(launcher)
    .bind(install)
    .execute(&app.pool)
    .await
    .unwrap();
}

#[sqlx::test]
async fn empty_platform_returns_zeros(pool: PgPool) {
    let app = TestApp::new(pool);
    let stats = app.get("/v1/stats", None).await.json();
    assert_eq!(stats["launcher"], json!({ "downloads": 0, "releases": 0, "current_tag": null, "current_downloads": 0 }));
    assert_eq!(stats["addons"]["installs"], 0);
    assert_eq!(stats["addons"]["active_installations_30d"], 0);
    assert_eq!(stats["top_addons_30d"], json!([]));
    assert_eq!(stats["platform"]["addons"], 0);
    assert_eq!(stats["checks"]["releases"]["last"], json!(null), "never ran");
}

#[sqlx::test]
async fn counts_launcher_and_addon_numbers(pool: PgPool) {
    let app = TestApp::new(pool);
    app.publish(1, "alice", "lucky").await;
    let a = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";
    let b = "3f2504e0-4f89-41d3-9a0c-0305e82c3302";
    event(&app, "install", Some(a), "0.4.3", 1).await;
    event(&app, "install", Some(b), "0.4.2", 2).await;
    event(&app, "install", None, "0.4.3", 40).await;
    event(&app, "update", Some(a), "0.4.3", 0).await;
    event(&app, "install_failed", None, "0.4.3", 0).await;
    sqlx::query("INSERT INTO launcher_releases (tag, asset, published_at, prerelease, download_count, fetched_at) VALUES ('v0.4.3', 'a.exe', now(), false, 7, now())")
        .execute(&app.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO download_counts (day, source, target_id, count) VALUES (current_date, 'website', 1, 3), (current_date, 'pack', 1, 2)")
        .execute(&app.pool)
        .await
        .unwrap();

    let stats = app.get("/v1/stats", None).await.json();
    assert_eq!(stats["launcher"]["current_downloads"], 7);
    assert_eq!(
        stats["addons"],
        json!({ "installs": 3, "updates": 1, "installs_30d": 2, "updates_30d": 1, "active_installations_30d": 2, "on_current_launcher_30d": 1 })
    );
    assert_eq!(stats["top_addons_30d"], json!([{ "slug": "lucky", "name": "Addon lucky", "installs": 2 }]));
    assert_eq!((stats["platform"]["addons"].as_i64(), stats["platform"]["creators"].as_i64()), (Some(1), Some(1)));
    assert_eq!((stats["platform"]["website_downloads"].as_i64(), stats["platform"]["pack_downloads"].as_i64()), (Some(3), Some(2)));
}

#[sqlx::test]
async fn active_installs_only_with_id(pool: PgPool) {
    let app = TestApp::new(pool);
    app.publish(1, "alice", "lucky").await;
    event(&app, "install", None, "0.4.3", 1).await;
    event(&app, "install", None, "0.4.3", 2).await;
    assert_eq!(app.get("/v1/stats", None).await.json()["addons"]["active_installations_30d"], 0);
}

#[sqlx::test]
async fn checks_from_jobs_table(pool: PgPool) {
    let app = TestApp::new(pool);
    let at: DateTime<Utc> = Utc.with_ymd_and_hms(2026, 10, 31, 10, 7, 0).unwrap();
    *app.now.lock().unwrap() = at;
    sqlx::query("INSERT INTO jobs (kind, run_at, done_at) VALUES ('poll_releases', $1, $1)").bind(at - Duration::minutes(5)).execute(&app.pool).await.unwrap();
    let checks = app.get("/v1/stats", None).await.json()["checks"].clone();
    assert_eq!(checks["releases"]["last"], json!("2026-10-31T10:02:00Z"));
    assert_eq!(checks["releases"]["next"], json!("2026-10-31T10:15:00Z"), "the next 15-minute slot");
    assert_eq!(checks["sources"]["next"], json!("2026-10-31T12:00:00Z"), "the next 6-hour slot");

    sqlx::query("INSERT INTO jobs (kind, run_at) VALUES ('poll_releases', $1)").bind(at).execute(&app.pool).await.unwrap();
    assert_eq!(app.get("/v1/stats", None).await.json()["checks"]["releases"]["next"], json!("2026-10-31T10:07:00Z"), "running now");
}
