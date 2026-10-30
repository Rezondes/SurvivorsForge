//! Download counts of SurvivorsForge itself, from the release files of `LAUNCHER_REPO` on GitHub.

use crate::app::AppState;
use serde::Serialize;
use sqlx::PgPool;

/// The hourly job that copies the counts from GitHub.
pub const POLL: &str = "poll_launcher_releases";

/// Replaces the stored counts with GitHub's. On a GitHub error the old ones stay.
pub async fn poll(state: &AppState) -> Result<(), String> {
    let repo = state.github.repo_by_name(None, &state.config.launcher_repo).await.map_err(|e| e.to_string())?;
    let releases = state.github.releases(None, repo.id).await.map_err(|e| e.to_string())?;
    let now = (state.clock)();
    let mut tx = state.pool.begin().await.map_err(|e| e.to_string())?;
    // Everything at once, so a release deleted on GitHub leaves the totals too.
    sqlx::query("DELETE FROM launcher_releases").execute(&mut *tx).await.map_err(|e| e.to_string())?;
    for release in releases.iter().filter(|r| !r.draft) {
        for asset in &release.assets {
            sqlx::query(
                "INSERT INTO launcher_releases (tag, asset, published_at, prerelease, download_count, fetched_at)
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(&release.tag_name)
            .bind(&asset.name)
            .bind(release.published_at)
            .bind(release.prerelease)
            .bind(asset.download_count)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        }
    }
    tx.commit().await.map_err(|e| e.to_string())
}

/// All downloads, and those of the newest release that is no pre-release.
#[derive(Debug, Default, Clone, PartialEq, Serialize, sqlx::FromRow)]
pub struct Totals {
    pub downloads: i64,
    pub releases: i64,
    pub current_tag: Option<String>,
    pub current_downloads: i64,
}

pub async fn totals(pool: &PgPool) -> sqlx::Result<Totals> {
    sqlx::query_as(
        "WITH current AS (
             SELECT tag FROM launcher_releases WHERE NOT prerelease ORDER BY published_at DESC NULLS LAST LIMIT 1
         )
         SELECT coalesce(sum(download_count), 0)::bigint AS downloads,
                count(DISTINCT tag) AS releases,
                (SELECT tag FROM current) AS current_tag,
                coalesce(sum(download_count) FILTER (WHERE tag = (SELECT tag FROM current)), 0)::bigint AS current_downloads
         FROM launcher_releases",
    )
    .fetch_one(pool)
    .await
}
