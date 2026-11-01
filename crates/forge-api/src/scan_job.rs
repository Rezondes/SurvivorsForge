//! The job `scan_version`: download the version's file from GitHub, hash it, compare with GitHub's
//! digest, scan it with forge-scan and store the result. The file only ever lives in memory.

use crate::app::AppState;
use crate::github::{DOWNLOAD_HOSTS, GitHubError, download_allowed};
use crate::jobs::{self, Job};
use crate::sync::SCAN_VERSION;
use chrono::{DateTime, Utc};
use forge_scan::{Finding, Report, SCANNER_VERSION, Severity};
use serde_json::json;
use sha2::{Digest, Sha256};

pub const MAX_DOWNLOAD: u64 = 50 * 1024 * 1024;

#[derive(sqlx::FromRow)]
struct Target {
    addon_id: i64,
    asset_url: String,
    github_digest: Option<String>,
}

fn finding(rule: &str, message: &str) -> Finding {
    Finding { rule: rule.into(), severity: Severity::Critical, file: String::new(), line: None, message: message.into(), snippet: None }
}

/// Runs the job. Network errors return `Err` so the queue retries; on the last attempt the
/// version is marked `failed` first.
pub async fn run(state: &AppState, job: &Job) -> Result<(), String> {
    let version_id = job.payload["version_id"].as_i64().ok_or("payload without version_id")?;
    match scan_version(state, version_id).await {
        Err(e) if job.attempts >= job.max_attempts => {
            sqlx::query("UPDATE versions SET scan_status = 'failed' WHERE id = $1").bind(version_id).execute(&state.pool).await.ok();
            Err(e)
        }
        other => other,
    }
}

pub async fn scan_version(state: &AppState, version_id: i64) -> Result<(), String> {
    let started = (state.clock)();
    let target: Option<Target> = sqlx::query_as("SELECT addon_id, asset_url, github_digest FROM versions WHERE id = $1")
        .bind(version_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(|e| e.to_string())?;
    let Some(target) = target else { return Ok(()) };
    sqlx::query("UPDATE versions SET scan_status = 'running' WHERE id = $1").bind(version_id).execute(&state.pool).await.map_err(|e| e.to_string())?;

    let hosts: Vec<String> = DOWNLOAD_HOSTS.iter().map(|h| h.to_string()).collect();
    if !download_allowed(&target.asset_url, &hosts) {
        let report = Report { findings: vec![finding("source.forbidden_host", "The file is not hosted on GitHub")] };
        return store(state, version_id, None, "error", &report, started, "failed").await;
    }
    let bytes = match state.github.download(&target.asset_url, MAX_DOWNLOAD).await {
        Ok(bytes) => bytes,
        Err(GitHubError::TooLarge) => {
            let report = Report { findings: vec![finding("source.too_large", "The file is larger than 50 MB")] };
            return store(state, version_id, None, "error", &report, started, "failed").await;
        }
        Err(GitHubError::ForbiddenHost(url)) => {
            let report = Report { findings: vec![finding("source.forbidden_host", &format!("The download was redirected to {url}"))] };
            return store(state, version_id, None, "error", &report, started, "failed").await;
        }
        Err(e) => {
            sqlx::query("UPDATE versions SET scan_status = 'pending' WHERE id = $1").bind(version_id).execute(&state.pool).await.ok();
            return Err(e.to_string());
        }
    };
    let sha256 = hex::encode(Sha256::digest(&bytes));
    if target.github_digest.as_ref().is_some_and(|d| *d != sha256) {
        let report = Report { findings: vec![finding("source.digest_mismatch", "The file differs from the checksum GitHub reports")] };
        return store(state, version_id, Some(&sha256), "error", &report, started, "done").await;
    }

    let (expected, foreign) = folders(state, target.addon_id).await.map_err(|e| e.to_string())?;
    // A crash of the scanner fails this version for good instead of taking the worker down.
    let scanned = tokio::task::spawn_blocking(move || crate::scan_run::run_scan(bytes, expected, foreign))
        .await
        .map_err(|e| e.to_string())?;
    let report = match scanned {
        Ok(report) => report,
        Err(e) => {
            let report = Report { findings: vec![finding("scan.crashed", &e)] };
            return store(state, version_id, Some(&sha256), "error", &report, started, "failed").await;
        }
    };
    let result = if report.flagged() { "flagged" } else { "clean" };
    store(state, version_id, Some(&sha256), result, &report, started, "done").await
}

/// The addon's own folders, and every folder of other active addons.
async fn folders(state: &AppState, addon_id: i64) -> sqlx::Result<(Vec<String>, Vec<String>)> {
    let rows: Vec<(String, bool)> = sqlx::query_as("SELECT folder::text, addon_id = $1 FROM addon_folders WHERE active")
        .bind(addon_id)
        .fetch_all(&state.pool)
        .await?;
    let (own, other): (Vec<_>, Vec<_>) = rows.into_iter().partition(|(_, own)| *own);
    Ok((own.into_iter().map(|r| r.0).collect(), other.into_iter().map(|r| r.0).collect()))
}

async fn store(
    state: &AppState,
    version_id: i64,
    sha256: Option<&str>,
    result: &str,
    report: &Report,
    started: DateTime<Utc>,
    scan_status: &str,
) -> Result<(), String> {
    let run = async {
        let mut tx = state.pool.begin().await?;
        let scan_id: i64 = sqlx::query_scalar(
            "INSERT INTO scans (version_id, sha256, scanner_version, started_at, finished_at, result)
             VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
        )
        .bind(version_id)
        .bind(sha256)
        .bind(SCANNER_VERSION)
        .bind(started)
        .bind((state.clock)())
        .bind(result)
        .fetch_one(&mut *tx)
        .await?;
        for f in &report.findings {
            let severity = match f.severity {
                Severity::Info => "info",
                Severity::Notice => "notice",
                Severity::Warn => "warn",
                Severity::Critical => "critical",
            };
            sqlx::query("INSERT INTO findings (scan_id, rule, severity, file, line, message, snippet) VALUES ($1, $2, $3, $4, $5, $6, $7)")
                .bind(scan_id)
                .bind(&f.rule)
                .bind(severity)
                .bind(&f.file)
                .bind(f.line.map(|l| l as i32))
                .bind(&f.message)
                .bind(&f.snippet)
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query("UPDATE versions SET sha256 = coalesce($2, sha256), scan_status = $3 WHERE id = $1")
            .bind(version_id)
            .bind(sha256)
            .bind(scan_status)
            .execute(&mut *tx)
            .await?;
        tx.commit().await
    };
    run.await.map_err(|e: sqlx::Error| e.to_string())
}

/// Queues a scan for every version not yet scanned with this scanner version. `forge-api rescan --all`.
pub async fn rescan_outdated(state: &AppState) -> sqlx::Result<usize> {
    let ids: Vec<i64> = sqlx::query_scalar(
        "SELECT v.id FROM versions v
         WHERE NOT EXISTS (SELECT 1 FROM scans s WHERE s.version_id = v.id AND s.scanner_version = $1)",
    )
    .bind(SCANNER_VERSION)
    .fetch_all(&state.pool)
    .await?;
    let now = (state.clock)();
    for id in &ids {
        jobs::enqueue(&state.pool, SCAN_VERSION, json!({ "version_id": id }), now, Some(&format!("scan:{id}"))).await?;
    }
    Ok(ids.len())
}

#[cfg(test)]
mod tests {
    #[test]
    fn no_fs_usage() {
        let source = include_str!("scan_job.rs");
        let code = source.split("#[cfg(test)]").next().unwrap();
        assert!(!code.contains("std::fs"), "the zip must stay in memory");
        assert!(!code.contains("tokio::fs"), "the zip must stay in memory");
    }
}
