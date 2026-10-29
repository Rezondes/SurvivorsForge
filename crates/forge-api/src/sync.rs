//! Keeps an addon's versions in step with its GitHub releases or tags.
//!
//! Runs as the job `sync_addon`, queued by webhooks, by the `poll_releases` every 15 minutes and by hand.

use crate::app::AppState;
use crate::github::GitHubError;
use crate::jobs;
use chrono::{DateTime, Utc};
use forge_core::registry::glob_match;
use serde_json::json;

pub const SYNC_ADDON: &str = "sync_addon";
pub const POLL_RELEASES: &str = "poll_releases";
pub const SCAN_VERSION: &str = "scan_version";

/// One version as GitHub shows it right now.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteVersion {
    pub release_id: Option<i64>,
    pub tag: String,
    pub name: String,
    pub prerelease: bool,
    pub published_at: DateTime<Utc>,
    pub asset_id: Option<i64>,
    pub asset_name: String,
    pub asset_url: String,
    pub asset_size: Option<i64>,
    pub github_digest: Option<String>,
    pub download_count: i64,
}

#[derive(sqlx::FromRow)]
struct SyncTarget {
    id: i64,
    repo_id: i64,
    repo_full_name: String,
    installation_id: Option<i64>,
    source_type: String,
    asset_pattern: Option<String>,
}

pub fn codeload_url(repo_full_name: &str, tag: &str) -> String {
    format!("https://codeload.github.com/{repo_full_name}/zip/refs/tags/{tag}")
}

/// What GitHub offers for the addon: releases with an asset matching the pattern, or tags.
async fn remote_versions(
    state: &AppState,
    addon: &SyncTarget,
    now: DateTime<Utc>,
) -> Result<Vec<RemoteVersion>, GitHubError> {
    if addon.source_type == "tag" {
        let tags = state
            .github
            .tags(addon.installation_id, addon.repo_id)
            .await?;
        return Ok(tags
            .into_iter()
            .map(|t| RemoteVersion {
                release_id: None,
                name: t.name.clone(),
                asset_name: format!("{}.zip", t.name),
                asset_url: codeload_url(&addon.repo_full_name, &t.name),
                tag: t.name,
                prerelease: false,
                published_at: now,
                asset_id: None,
                asset_size: None,
                github_digest: None,
                download_count: 0,
            })
            .collect());
    }
    let pattern = addon.asset_pattern.as_deref().unwrap_or_default();
    let releases = state
        .github
        .releases(addon.installation_id, addon.repo_id)
        .await?;
    Ok(releases
        .into_iter()
        .filter(|r| !r.draft)
        .filter_map(|r| {
            let asset = r
                .assets
                .iter()
                .find(|a| glob_match(pattern, &a.name))?
                .clone();
            Some(RemoteVersion {
                release_id: Some(r.id),
                name: r.name.clone().unwrap_or_default(),
                tag: r.tag_name,
                prerelease: r.prerelease,
                published_at: r.published_at.unwrap_or(now),
                asset_id: Some(asset.id),
                asset_name: asset.name,
                asset_url: asset.browser_download_url,
                asset_size: Some(asset.size),
                github_digest: asset.digest,
                download_count: asset.download_count,
            })
        })
        .collect())
}

async fn enqueue_scan(state: &AppState, version_id: i64, now: DateTime<Utc>) -> sqlx::Result<()> {
    jobs::enqueue(
        &state.pool,
        SCAN_VERSION,
        json!({ "version_id": version_id }),
        now,
        Some(&format!("scan:{version_id}")),
    )
    .await?;
    Ok(())
}

/// Brings the versions of `addon_id` in line with GitHub. Locked and deleted addons are skipped.
pub async fn sync_addon(state: &AppState, addon_id: i64) -> Result<(), String> {
    let now = (state.clock)();
    let addon: Option<SyncTarget> = sqlx::query_as(
        "SELECT id, repo_id, repo_full_name, installation_id, source_type, asset_pattern FROM addons
         WHERE id = $1 AND deleted_at IS NULL AND locked_at IS NULL",
    )
    .bind(addon_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| e.to_string())?;
    let Some(addon) = addon else { return Ok(()) };
    let remote = remote_versions(state, &addon, now)
        .await
        .map_err(|e| e.to_string())?;
    store(state, &addon, &remote, now)
        .await
        .map_err(|e| e.to_string())
}

async fn store(
    state: &AppState,
    addon: &SyncTarget,
    remote: &[RemoteVersion],
    now: DateTime<Utc>,
) -> sqlx::Result<()> {
    for v in remote {
        let existing: Option<(i64, Option<i64>, Option<String>)> = sqlx::query_as(
            "SELECT id, asset_id, github_digest FROM versions WHERE addon_id = $1 AND tag = $2",
        )
        .bind(addon.id)
        .bind(&v.tag)
        .fetch_optional(&state.pool)
        .await?;
        match existing {
            None => {
                let id: i64 = sqlx::query_scalar(
                    "INSERT INTO versions (addon_id, github_release_id, tag, name, prerelease, published_at, asset_id, asset_name,
                                           asset_url, asset_size, github_digest, download_count, created_at, game_min_id, game_max_id)
                     SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, prev.game_min_id, prev.game_max_id
                     FROM (SELECT NULL) one
                     LEFT JOIN LATERAL (SELECT game_min_id, game_max_id FROM versions WHERE addon_id = $1
                                        ORDER BY published_at DESC, id DESC LIMIT 1) prev ON true
                     RETURNING id",
                )
                .bind(addon.id)
                .bind(v.release_id)
                .bind(&v.tag)
                .bind(&v.name)
                .bind(v.prerelease)
                .bind(v.published_at)
                .bind(v.asset_id)
                .bind(&v.asset_name)
                .bind(&v.asset_url)
                .bind(v.asset_size)
                .bind(&v.github_digest)
                .bind(v.download_count)
                .bind(now)
                .fetch_one(&state.pool)
                .await?;
                enqueue_scan(state, id, now).await?;
            }
            Some((id, asset_id, digest)) => {
                // A different file behind the same tag: the old hash and scan no longer say anything.
                let replaced = asset_id != v.asset_id
                    || (v.github_digest.is_some() && digest != v.github_digest);
                sqlx::query(
                    "UPDATE versions SET github_release_id = $2, name = $3, prerelease = $4, asset_id = $5, asset_name = $6,
                            asset_url = $7, asset_size = $8, github_digest = $9, download_count = $10, deleted_on_github_at = NULL,
                            sha256 = CASE WHEN $11 THEN NULL ELSE sha256 END,
                            scan_status = CASE WHEN $11 THEN 'pending' ELSE scan_status END
                     WHERE id = $1",
                )
                .bind(id)
                .bind(v.release_id)
                .bind(&v.name)
                .bind(v.prerelease)
                .bind(v.asset_id)
                .bind(&v.asset_name)
                .bind(&v.asset_url)
                .bind(v.asset_size)
                .bind(&v.github_digest)
                .bind(v.download_count)
                .bind(replaced)
                .execute(&state.pool)
                .await?;
                if replaced {
                    enqueue_scan(state, id, now).await?;
                }
            }
        }
    }
    // Gone from GitHub: kept in the history, marked.
    let tags: Vec<&str> = remote.iter().map(|v| v.tag.as_str()).collect();
    sqlx::query(
        "UPDATE versions SET deleted_on_github_at = $3
         WHERE addon_id = $1 AND deleted_on_github_at IS NULL AND NOT (tag = ANY($2))",
    )
    .bind(addon.id)
    .bind(&tags)
    .bind(now)
    .execute(&state.pool)
    .await?;
    Ok(())
}

/// Queues a sync of every followable addon. The job `poll_releases`.
pub async fn poll_releases(state: &AppState) -> Result<(), String> {
    let now = (state.clock)();
    let ids: Vec<i64> =
        sqlx::query_scalar("SELECT id FROM addons WHERE deleted_at IS NULL AND locked_at IS NULL")
            .fetch_all(&state.pool)
            .await
            .map_err(|e| e.to_string())?;
    for id in ids {
        enqueue_sync(state, id, now)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub async fn enqueue_sync(state: &AppState, addon_id: i64, now: DateTime<Utc>) -> sqlx::Result<()> {
    jobs::enqueue(
        &state.pool,
        SYNC_ADDON,
        json!({ "addon_id": addon_id }),
        now,
        Some(&format!("sync:{addon_id}")),
    )
    .await?;
    Ok(())
}
