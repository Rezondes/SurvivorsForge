//! What the platform spends of its GitHub limits: every request GitHub answers is counted by
//! origin (the API route or worker job that caused it), endpoint and quota, and the
//! `x-ratelimit-*` headers keep the state of each quota. Both go to Prometheus at once and to the
//! database (`github_request_stats`, `github_quota`) on `flush`.

use chrono::{DateTime, DurationRound, TimeDelta, TimeZone, Utc};
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

tokio::task_local! {
    /// What caused the GitHub requests of this task, e.g. `GET /v1/addons/{slug}` or `job sync_addon`.
    pub static ORIGIN: String;
}

/// The counts of this process; one GitHub client per process.
pub static USAGE: LazyLock<Usage> = LazyLock::new(Usage::default);

fn origin() -> String {
    ORIGIN.try_with(Clone::clone).unwrap_or_else(|_| "other".into())
}

/// A page of github.com itself (`quota` `web` or `download`): owner, repo and tag hidden, e.g.
/// `/{owner}/{repo}/releases/expanded_assets/{tag}`.
pub fn web_endpoint(path: &str) -> String {
    let segments: Vec<&str> = path.split('?').next().unwrap_or_default().split('/').filter(|s| !s.is_empty()).collect();
    let rest: Vec<&str> = segments.iter().skip(2).copied().collect();
    let rest = match rest.as_slice() {
        ["releases", "latest"] => "releases/latest".to_string(),
        ["releases", "expanded_assets", ..] => "releases/expanded_assets/{tag}".into(),
        ["releases", "download", ..] => "releases/download/{tag}/{file}".into(),
        ["info", "refs"] => "info/refs".into(),
        [first, ..] => format!("{first}/..."),
        [] => String::new(),
    };
    format!("/{{owner}}/{{repo}}/{rest}").trim_end_matches('/').to_string()
}

/// The path with its names and numbers replaced: `/repositories/{id}/releases`.
pub fn endpoint(path: &str) -> String {
    let segments: Vec<&str> = path.split('?').next().unwrap_or_default().split('/').filter(|s| !s.is_empty()).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < segments.len() {
        let segment = segments[i];
        match segment {
            "repos" if i == 0 => {
                out.extend(["repos", "{owner}", "{repo}"]);
                i += 3;
            }
            "users" | "collaborators" => {
                out.extend([segment, "{login}"]);
                i += 2;
            }
            s if s.bytes().all(|b| b.is_ascii_digit()) => {
                out.push("{id}");
                i += 1;
            }
            s => {
                out.push(s);
                i += 1;
            }
        }
    }
    format!("/{}", out.join("/"))
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    hour: DateTime<Utc>,
    origin: String,
    endpoint: String,
    quota: String,
    status: u16,
}

/// One quota as GitHub's headers last told it.
#[derive(Debug, Clone, PartialEq)]
pub struct Quota {
    pub limit: i64,
    pub used: i64,
    pub remaining: i64,
    pub reset_at: Option<DateTime<Utc>>,
    pub seen_at: DateTime<Utc>,
}

#[derive(Default)]
pub struct Usage {
    requests: Mutex<HashMap<Key, i64>>,
    /// (quota, resource) -> state.
    quotas: Mutex<HashMap<(String, String), Quota>>,
}

impl Usage {
    /// Counts one answer of GitHub that came with `quota`, e.g. `read_token` or `installation:42`.
    pub fn record(&self, quota: &str, response: &reqwest::Response, now: DateTime<Utc>) {
        let header = |name: &str| response.headers().get(name).and_then(|v| v.to_str().ok());
        let path = response.url().path();
        let endpoint = if matches!(quota, "web" | "download") { web_endpoint(path) } else { endpoint(path) };
        let (origin, status) = (origin(), response.status().as_u16());
        metrics::counter!("forge_github_requests_total", "origin" => origin.clone(), "endpoint" => endpoint.clone(), "quota" => quota.to_string(), "status" => status.to_string())
            .increment(1);
        let hour = now.duration_trunc(TimeDelta::hours(1)).unwrap_or(now);
        *self.requests.lock().unwrap_or_else(|e| e.into_inner()).entry(Key { hour, origin, endpoint, quota: quota.into(), status }).or_default() += 1;

        let number = |name: &str| header(name).and_then(|v| v.parse::<i64>().ok());
        let (Some(limit), Some(remaining)) = (number("x-ratelimit-limit"), number("x-ratelimit-remaining")) else { return };
        let resource = header("x-ratelimit-resource").unwrap_or("core").to_string();
        let used = number("x-ratelimit-used").unwrap_or(limit - remaining);
        let reset_at = number("x-ratelimit-reset").and_then(|s| Utc.timestamp_opt(s, 0).single());
        let labels = [("quota", quota.to_string()), ("resource", resource.clone())];
        metrics::gauge!("forge_github_rate_limit", &labels).set(limit as f64);
        metrics::gauge!("forge_github_rate_used", &labels).set(used as f64);
        metrics::gauge!("forge_github_rate_remaining", &labels).set(remaining as f64);
        if let Some(reset) = reset_at {
            metrics::gauge!("forge_github_rate_reset_seconds", &labels).set(reset.timestamp() as f64);
        }
        self.quotas.lock().unwrap_or_else(|e| e.into_inner()).insert((quota.into(), resource), Quota { limit, used, remaining, reset_at, seen_at: now });
    }

    /// Writes the counts and the newest quota states; on an error they wait for the next try.
    pub async fn flush(&self, pool: &PgPool) -> sqlx::Result<()> {
        let requests = std::mem::take(&mut *self.requests.lock().unwrap_or_else(|e| e.into_inner()));
        let quotas = std::mem::take(&mut *self.quotas.lock().unwrap_or_else(|e| e.into_inner()));
        let result = write(pool, &requests, &quotas).await;
        if result.is_err() {
            let mut pending = self.requests.lock().unwrap_or_else(|e| e.into_inner());
            for (key, n) in requests {
                *pending.entry(key).or_default() += n;
            }
            let mut newer = self.quotas.lock().unwrap_or_else(|e| e.into_inner());
            for (key, quota) in quotas {
                newer.entry(key).or_insert(quota);
            }
        }
        result
    }
}

async fn write(pool: &PgPool, requests: &HashMap<Key, i64>, quotas: &HashMap<(String, String), Quota>) -> sqlx::Result<()> {
    if requests.is_empty() && quotas.is_empty() {
        return Ok(());
    }
    let mut tx = pool.begin().await?;
    for (k, n) in requests {
        sqlx::query(
            "INSERT INTO github_request_stats (hour, origin, endpoint, quota, status, count) VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (hour, origin, endpoint, quota, status) DO UPDATE SET count = github_request_stats.count + EXCLUDED.count",
        )
        .bind(k.hour)
        .bind(&k.origin)
        .bind(&k.endpoint)
        .bind(&k.quota)
        .bind(i16::try_from(k.status).unwrap_or(i16::MAX))
        .bind(n)
        .execute(&mut *tx)
        .await?;
    }
    for ((quota, resource), q) in quotas {
        // Worker and API both write; the newer answer wins.
        sqlx::query(
            "INSERT INTO github_quota (quota, resource, \"limit\", used, remaining, reset_at, seen_at) VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT (quota, resource) DO UPDATE SET \"limit\" = EXCLUDED.\"limit\", used = EXCLUDED.used, remaining = EXCLUDED.remaining,
                 reset_at = EXCLUDED.reset_at, seen_at = EXCLUDED.seen_at
             WHERE github_quota.seen_at <= EXCLUDED.seen_at",
        )
        .bind(quota)
        .bind(resource)
        .bind(q.limit)
        .bind(q.used)
        .bind(q.remaining)
        .bind(q.reset_at)
        .bind(q.seen_at)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_hide_names_and_numbers() {
        assert_eq!(endpoint("/repositories/123/releases"), "/repositories/{id}/releases");
        assert_eq!(endpoint("/repositories/123/releases/456"), "/repositories/{id}/releases/{id}");
        assert_eq!(endpoint("/repos/alice/lucky/installation"), "/repos/{owner}/{repo}/installation");
        assert_eq!(endpoint("/repositories/1/collaborators/bob/permission"), "/repositories/{id}/collaborators/{login}/permission");
        assert_eq!(endpoint("/users/bob/repos"), "/users/{login}/repos");
        assert_eq!(endpoint("/app/installations/77/access_tokens"), "/app/installations/{id}/access_tokens");
        assert_eq!(endpoint("/rate_limit"), "/rate_limit");
    }

    #[test]
    fn web_pages_hide_owner_repo_and_tag() {
        assert_eq!(web_endpoint("/Rezondes/SurvivorsForge/releases/latest"), "/{owner}/{repo}/releases/latest");
        assert_eq!(web_endpoint("/a/b/releases/expanded_assets/v1.2"), "/{owner}/{repo}/releases/expanded_assets/{tag}");
        assert_eq!(web_endpoint("/a/b.git/info/refs"), "/{owner}/{repo}/info/refs");
        assert_eq!(web_endpoint("/a/b/releases/download/v1/x.zip"), "/{owner}/{repo}/releases/download/{tag}/{file}");
    }

    #[tokio::test]
    async fn origin_comes_from_the_task() {
        assert_eq!(origin(), "other");
        assert_eq!(ORIGIN.scope("job sync_addon".into(), async { origin() }).await, "job sync_addon");
    }
}
