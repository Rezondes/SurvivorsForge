mod common;

use chrono::{Duration, Utc};
use common::TestApp;
use forge_api::github::types::{Asset, Release};
use forge_api::launcher_stats::{Totals, poll, totals};
use sqlx::PgPool;

const REPO_ID: i64 = 777;

fn release(id: i64, tag: &str, days_ago: i64, prerelease: bool, counts: &[(&str, i64)]) -> Release {
    Release {
        id,
        tag_name: tag.into(),
        name: None,
        draft: false,
        prerelease,
        published_at: Some(Utc::now() - Duration::days(days_ago)),
        assets: counts
            .iter()
            .enumerate()
            .map(|(i, (name, count))| Asset {
                id: id * 10 + i as i64,
                name: name.to_string(),
                size: 1,
                browser_download_url: format!("https://github.com/dl/{name}"),
                download_count: *count,
                digest: None,
            })
            .collect(),
    }
}

fn app_with_releases(pool: PgPool) -> TestApp {
    let app = TestApp::new(pool);
    app.github.add_repo(REPO_ID, "Rezondes/SurvivorsForge", "rezondes");
    app.github.data().releases.insert(
        REPO_ID,
        vec![
            release(3, "v0.5.0-rc.1", 1, true, &[("SurvivorsForge.exe", 4)]),
            release(2, "v0.4.3", 3, false, &[("SurvivorsForge.exe", 30), ("SurvivorsForge.AppImage", 10)]),
            release(1, "v0.4.2", 20, false, &[("SurvivorsForge.exe", 100)]),
            Release { draft: true, ..release(4, "v0.6.0", 0, false, &[("SurvivorsForge.exe", 1)]) },
        ],
    );
    app
}

#[sqlx::test]
async fn stores_asset_counts(pool: PgPool) {
    let app = app_with_releases(pool);
    poll(&app.state()).await.unwrap();
    let rows: Vec<(String, String, i64)> = sqlx::query_as("SELECT tag, asset, download_count FROM launcher_releases ORDER BY tag, asset").fetch_all(&app.pool).await.unwrap();
    assert_eq!(rows.len(), 4, "drafts are left out: {rows:?}");

    app.github.data().releases.get_mut(&REPO_ID).unwrap()[2].assets[0].download_count = 120;
    app.advance(Duration::hours(1));
    poll(&app.state()).await.unwrap();
    assert_eq!(totals(&app.pool).await.unwrap().downloads, 4 + 40 + 120, "the next poll overwrites");
    let history: Vec<Option<i64>> = sqlx::query_scalar("SELECT downloads FROM launcher_counts ORDER BY at").fetch_all(&app.pool).await.unwrap();
    assert_eq!(history, [Some(144), Some(164)], "each poll keeps its total for the curve");
}

#[sqlx::test]
async fn github_error_keeps_old_numbers(pool: PgPool) {
    let app = app_with_releases(pool);
    poll(&app.state()).await.unwrap();
    app.advance(Duration::hours(1));
    app.github.data().repos.clear();
    assert!(poll(&app.state()).await.is_err(), "a failed job, visible in the queue");
    assert_eq!(totals(&app.pool).await.unwrap().downloads, 144);
    let downloads: Vec<Option<i64>> = sqlx::query_scalar("SELECT downloads FROM launcher_counts ORDER BY at").fetch_all(&app.pool).await.unwrap();
    assert_eq!(downloads, [Some(144), None], "the failed poll still keeps the active launchers, without downloads");
}

#[sqlx::test]
async fn totals_skip_prereleases_for_current(pool: PgPool) {
    let app = app_with_releases(pool);
    assert_eq!(totals(&app.pool).await.unwrap(), Totals::default(), "nothing polled yet");
    poll(&app.state()).await.unwrap();
    assert_eq!(
        totals(&app.pool).await.unwrap(),
        Totals { downloads: 144, releases: 3, current_tag: Some("v0.4.3".into()), current_downloads: 40 }
    );
}

#[sqlx::test]
async fn keeps_the_launchers_active_in_the_last_30_days(pool: PgPool) {
    let app = app_with_releases(pool);
    app.publish(1, "alice", "lucky").await;
    // Two launchers in the window (one of them twice), one too old, one without an ID.
    for (install, days_ago) in [(Some("3f2504e0-4f89-41d3-9a0c-0305e82c3301"), 1), (Some("3f2504e0-4f89-41d3-9a0c-0305e82c3301"), 2), (Some("3f2504e0-4f89-41d3-9a0c-0305e82c3302"), 29), (Some("3f2504e0-4f89-41d3-9a0c-0305e82c3303"), 31), (None, 1)] {
        sqlx::query(
            "INSERT INTO telemetry_events (day, kind, addon_id, version, launcher_version, os, install_id, created_at)
             SELECT ($1::timestamptz)::date, 'install', id, 'v1', '0.5.3', 'windows', $2::uuid, $1 FROM addons WHERE slug = 'lucky'",
        )
        .bind(Utc::now() - Duration::days(days_ago))
        .bind(install)
        .execute(&app.pool)
        .await
        .unwrap();
    }
    poll(&app.state()).await.unwrap();
    let active: i64 = sqlx::query_scalar("SELECT active_30d FROM launcher_counts").fetch_one(&app.pool).await.unwrap();
    let on_stats = app.get("/v1/stats", None).await.json()["addons"]["active_installations_30d"].as_i64().unwrap();
    assert_eq!((active, on_stats), (2, 2), "the same number as /stats shows");
}
